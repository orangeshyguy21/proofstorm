use crate::model::merge_resources;
use gloo_net::http::Request;
use proofstorm_view::{EnvironmentLab, EnvironmentView, ObserverStatus};
use std::collections::BTreeSet;

async fn get<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    let controller = web_sys::AbortController::new().map_err(|_| "Browser request unavailable")?;
    let abort = controller.clone();
    let _timeout = gloo_timers::callback::Timeout::new(15_000, move || abort.abort());
    let response = Request::get(url)
        .abort_signal(Some(&controller.signal()))
        .send()
        .await
        .map_err(|_| "The server could not be reached. Keeping the last snapshot.".to_owned())?;
    if !response.ok() {
        let status = response.status();
        let body = response.json::<serde_json::Value>().await.ok();
        let code = body
            .as_ref()
            .and_then(|body| body["error"]["code"].as_str());
        return Err(match (status, code) {
            (403, _) => "Access denied. Check the server's workspace permissions.".into(),
            (_, Some("store_failure")) => {
                "The server could not read the workspace database. Check the server terminal."
                    .into()
            }
            (_, Some("stored_record_incompatible")) => {
                "A stored workspace record is incompatible with this version of Proofstorm.".into()
            }
            (_, Some("runtime_failure")) => "The current cluster could not be read. Check the cluster connection; keeping the last snapshot.".into(),
            (_, Some("lab_not_in_cluster")) => "This lab has been removed from the cluster. Refreshing the lab list.".into(),
            _ => format!("Server returned HTTP {status}. Check the server terminal."),
        });
    }
    response
        .json()
        .await
        .map_err(|_| "The server returned an incompatible view.".into())
}
pub async fn environment() -> Result<EnvironmentView, String> {
    let mut view: EnvironmentView = get("/v1/environment?limit=50").await?;
    let mut seen = BTreeSet::new();
    while let Some(cursor) = view.labs.next_cursor.take() {
        if !seen.insert(cursor.clone()) {
            return Err("Lab pagination did not advance.".into());
        }
        let page: EnvironmentView = get(&format!(
            "/v1/environment?limit=50&cursor={}",
            encode(&cursor)
        ))
        .await?;
        view.labs.items.extend(page.labs.items);
        view.labs.next_cursor = page.labs.next_cursor;
        view.observation_finished_at_unix = page.observation_finished_at_unix;
    }
    view.labs
        .items
        .sort_by_key(|lab| (crate::model::lab_name(lab), lab.id.clone()));
    view.labs.items.dedup_by(|a, b| a.id == b.id);
    Ok(view)
}
pub async fn observer() -> Result<ObserverStatus, String> {
    get("/v1/observer").await
}
pub async fn lab(id: &str, history_pages: usize) -> Result<EnvironmentLab, String> {
    let base = format!("/v1/environment?limit=20&instance_id={}", encode(id));
    let view: EnvironmentView = get(&base).await?;
    let mut lab = view
        .labs
        .items
        .into_iter()
        .next()
        .ok_or("Lab is no longer available")?;
    for section in ["component", "link", "session", "activity"] {
        let mut seen = BTreeSet::new();
        let mut pages = 1;
        loop {
            let cursor = match section {
                "component" => lab.components.next_cursor.take(),
                "link" => lab.links.next_cursor.take(),
                "session" => lab.sessions.next_cursor.clone(),
                _ => lab.activity.next_cursor.clone(),
            };
            if pages >= history_pages && matches!(section, "session" | "activity") {
                break;
            }
            let Some(cursor) = cursor else {
                break;
            };
            if !seen.insert(cursor.clone()) {
                return Err("Section pagination did not advance.".into());
            }
            let page: EnvironmentView =
                get(&format!("{base}&{section}_cursor={}", encode(&cursor))).await?;
            let page = page
                .labs
                .items
                .into_iter()
                .next()
                .ok_or("Lab changed during refresh")?;
            if page.revision_digest != lab.revision_digest {
                return Err("Lab changed during refresh; refreshing again.".into());
            }
            match section {
                "component" => {
                    lab.components.items.extend(page.components.items);
                    lab.components.next_cursor = page.components.next_cursor;
                    merge_resources(&mut lab.resources, page.resources);
                    if page.resource_error.is_some() {
                        lab.resource_error = page.resource_error;
                    }
                }
                "link" => {
                    lab.links.items.extend(page.links.items);
                    lab.links.next_cursor = page.links.next_cursor;
                }
                "session" => {
                    lab.sessions.items.extend(page.sessions.items);
                    lab.sessions.next_cursor = page.sessions.next_cursor;
                }
                _ => {
                    lab.activity.items.extend(page.activity.items);
                    lab.activity.next_cursor = page.activity.next_cursor;
                }
            }
            pages += 1;
        }
    }
    lab.components.items.sort_by(|a, b| a.id.cmp(&b.id));
    lab.components.items.dedup_by(|a, b| a.id == b.id);
    lab.links.items.sort_by(|a, b| a.id.cmp(&b.id));
    lab.links.items.dedup_by(|a, b| a.id == b.id);
    Ok(lab)
}
fn encode(value: &str) -> String {
    value.bytes().fold(String::new(), |mut out, b| {
        use std::fmt::Write;
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
        out
    })
}
