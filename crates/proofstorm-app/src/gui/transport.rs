//! Session authentication and narrow onboarding commands; no shell or generic file APIs.
use super::Session;
use crate::http::{Body, json};
use http_body_util::BodyExt;
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};

fn header<'a>(request: &'a Request<Incoming>, name: &str) -> &'a str {
    request
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

fn constant_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

pub(super) fn authorized(
    headers: &hyper::HeaderMap,
    token: &str,
    cookie_name: &str,
) -> (bool, bool, bool) {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    let bearer = get("authorization")
        .strip_prefix("Bearer ")
        .is_some_and(|v| constant_eq(v, token));
    let cookie = get("cookie")
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .any(|(name, value)| name == cookie_name && constant_eq(value, token));
    let csrf = constant_eq(get("x-proofstorm-session"), token);
    (bearer, cookie, csrf)
}

fn fail(status: StatusCode, message: &str) -> Response<Body> {
    json(status, &json!({"error":{"message":message}}))
}

async fn body<T: serde::de::DeserializeOwned>(
    request: &mut Request<Incoming>,
) -> anyhow::Result<T> {
    anyhow::ensure!(
        header(request, "content-type").split(';').next() == Some("application/json"),
        "JSON content type required"
    );
    let bytes = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        while let Some(frame) = request.body_mut().frame().await {
            if let Ok(data) = frame?.into_data() {
                anyhow::ensure!(bytes.len() + data.len() <= 8192, "request is too large");
                bytes.extend_from_slice(&data);
            }
        }
        Ok::<_, anyhow::Error>(bytes)
    })
    .await??;
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRequest {
    project: PathBuf,
    #[serde(default)]
    harness: crate::harness::Harness,
    #[serde(default)]
    replace_connection: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Focus {
    generation: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FolderRequest {
    project: Option<PathBuf>,
}

/// Return None only for authenticated reads or public static assets.
#[allow(
    clippy::too_many_lines,
    reason = "keep authentication gates next to the complete typed action allowlist"
)]
pub(crate) async fn route(
    request: &mut Request<Incoming>,
    session: Arc<Session>,
) -> Option<Response<Body>> {
    let path = request.uri().path().to_owned();
    let origin = session.record.url();
    let expected_host = format!("127.0.0.1:{}", session.record.port);
    let (bearer, cookie, csrf) = authorized(
        request.headers(),
        &session.record.token,
        &session.cookie_name(),
    );
    let same_origin = header(request, "origin") == origin;
    let read = request.method() == hyper::Method::GET;
    if header(request, "host") != expected_host
        || (!header(request, "origin").is_empty() && !same_origin)
        || matches!(
            header(request, "sec-fetch-site"),
            "cross-site" | "same-site"
        )
    {
        return Some(fail(
            StatusCode::FORBIDDEN,
            "This GUI accepts only its own local origin.",
        ));
    }
    if !read && (request.method() != hyper::Method::POST || !(bearer || same_origin && csrf)) {
        return Some(fail(
            StatusCode::FORBIDDEN,
            "Authenticated same-origin action required.",
        ));
    }
    if path == "/v1/gui/session" && !read && csrf {
        let mut response = json(StatusCode::OK, &json!({"authenticated":true}));
        response.headers_mut().insert(
            "set-cookie",
            format!(
                "{}={}; Path=/; HttpOnly; SameSite=Strict",
                session.cookie_name(),
                session.record.token
            )
            .parse()
            .unwrap(),
        );
        return Some(response);
    }
    if path.starts_with("/v1/") && !(bearer || cookie) {
        return Some(fail(
            StatusCode::UNAUTHORIZED,
            "Run proofstorm gui to open an authenticated browser session.",
        ));
    }
    let response = match (request.method().as_str(), path.as_str()) {
        ("GET", "/v1/gui/health") if bearer => json(StatusCode::OK, &session.record.health()),
        ("GET", "/v1/gui/context") => {
            let s = session.clone();
            match tokio::task::spawn_blocking(move || s.context()).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(_) => fail(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Could not inspect this installation.",
                ),
            }
        }
        ("GET", "/v1/gui/activation") => {
            let activation = session.activation.lock().unwrap();
            json(
                StatusCode::OK,
                &json!({"generation":activation.generation,
                "project":(activation.created.elapsed()<Duration::from_secs(3)).then_some(&activation.project)}),
            )
        }
        ("POST", "/v1/gui/focus-ack") => match body::<Focus>(request).await {
            Ok(focus) => {
                let mut activation = session.activation.lock().unwrap();
                if activation.generation == focus.generation {
                    activation.focused = true;
                }
                json(StatusCode::OK, &json!({"acknowledged":true}))
            }
            Err(_) => fail(StatusCode::BAD_REQUEST, "Invalid focus acknowledgement."),
        },
        ("POST", "/v1/gui/activate") if bearer => match body::<ProjectRequest>(request).await {
            Ok(project) if project.project.is_absolute() => json(
                StatusCode::OK,
                &session.activate(&project.project.to_string_lossy()).await,
            ),
            _ => fail(
                StatusCode::BAD_REQUEST,
                "An absolute project folder is required.",
            ),
        },
        ("POST", "/v1/gui/stop") if bearer => {
            if let Ok(_permit) = session.actions.clone().try_acquire_owned() {
                session
                    .stopping
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                session.shutdown.notify_one();
                json(
                    StatusCode::OK,
                    &json!({"stopping":true,"cells_stopped":false}),
                )
            } else {
                fail(
                    StatusCode::CONFLICT,
                    "An attachment is still running. Retry afterward.",
                )
            }
        }
        ("POST", "/v1/gui/plan" | "/v1/gui/open" | "/v1/gui/pick-folder") => {
            let picking = path.ends_with("/pick-folder");
            let project = if picking {
                body::<FolderRequest>(request)
                    .await
                    .map(|folder| ProjectRequest {
                        project: folder.project.unwrap_or_default(),
                        harness: crate::harness::Harness::Codex,
                        replace_connection: None,
                    })
            } else {
                body::<ProjectRequest>(request).await
            };
            let Ok(project) = project else {
                return Some(fail(
                    StatusCode::BAD_REQUEST,
                    "Choose an absolute project folder; no other options are accepted.",
                ));
            };
            let Ok(permit) = session.actions.clone().try_acquire_owned() else {
                return Some(fail(
                    StatusCode::CONFLICT,
                    "Another attachment is being checked or applied. Retry shortly.",
                ));
            };
            if session.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                return Some(fail(
                    StatusCode::CONFLICT,
                    "GUI is stopping. Reopen it with proofstorm gui.",
                ));
            }
            let preview = path.ends_with("/plan");
            // Keep the operation alive if a browser disconnects after confirming it.
            let job = tokio::spawn(async move {
                let _permit = permit;
                if picking {
                    return session
                        .pick_folder(
                            (!project.project.as_os_str().is_empty()).then_some(project.project),
                        )
                        .await;
                }
                session
                    .open_project(
                        project.harness,
                        project.project,
                        preview,
                        project.replace_connection,
                    )
                    .await
            });
            match job.await {
                Ok(Ok(value)) => json(StatusCode::OK, &value),
                Ok(Err(error)) => {
                    if let Some(conflict) =
                        error.downcast_ref::<crate::harness::ConnectionConflict>()
                    {
                        json(
                            StatusCode::CONFLICT,
                            &json!({"error":{"message":error.to_string()},"connection_conflict":conflict}),
                        )
                    } else {
                        fail(StatusCode::CONFLICT, &format!("{error:#}"))
                    }
                }
                Err(_) => fail(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "The operation was interrupted. Recheck the project before retrying.",
                ),
            }
        }
        _ if read && !path.starts_with("/v1/gui/") => return None,
        _ => fail(StatusCode::NOT_FOUND, "Unknown GUI action."),
    };
    Some(response)
}

pub(crate) fn secure(response: &mut Response<Body>) {
    for (name, value) in [
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'; form-action 'self'",
        ),
        ("x-content-type-options", "nosniff"),
    ] {
        response
            .headers_mut()
            .insert(name, hyper::header::HeaderValue::from_static(value));
    }
}
