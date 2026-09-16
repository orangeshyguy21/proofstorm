//! Read-only projection of the pinned CDK d3dec24c LDK dashboard.
//! This version has HTML, not a JSON node API. Parse only exact public fields;
//! template changes fail closed instead of publishing empty/guessed channels.
use super::channels::{checked, pubkey};
use proofstorm_view::{LightningObservation, ObservedChannel};
use regex::Regex;

pub(super) fn project(dashboard: &str, balance: &str) -> Option<LightningObservation> {
    let key = capture(
        dashboard,
        r#"<p class="node-address">Node ID: ([0-9a-fA-F]{66})</p>"#,
    )?;
    let count: usize = capture(
        balance,
        r#"<div class="metric-value">([0-9]+)</div><div class="metric-label">Total Channels</div>"#,
    )?
    .parse()
    .ok()?;
    let boxes = balance
        .split("<div class=\"channel-box\">")
        .skip(1)
        .collect::<Vec<_>>();
    if boxes.len() != count || (count == 0 && !balance.contains("No channels found.")) {
        return None;
    }
    let channels = boxes.into_iter().map(|html| {
        let id = capture(html, r#"<span class="detail-label">Channel ID</span><span class="detail-value">([0-9a-fA-F]{64})</span>"#)?.to_ascii_lowercase();
        let peer = capture(html, r#"<span class="detail-label">Node ID</span><span class="detail-value">([0-9a-fA-F]{66})</span>"#)?;
        let state = capture(html, r#"<span class="status-badge status-(?:active|inactive)">(Active|Inactive)</span>"#)?;
        checked(ObservedChannel {
            channel_id: Some(id), funding_outpoint: String::new(), capacity_only: true,
            peer_pubkey: pubkey(&peer)?, active: state == "Active",
            capacity_msat: capacity(html, "Total")?,
            local_msat: capacity(html, "Outbound")?, remote_msat: capacity(html, "Inbound")?,
        })
    }).collect::<Option<Vec<_>>>()?;
    if channels
        .iter()
        .map(ObservedChannel::id)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != channels.len()
    {
        return None;
    }
    Some(LightningObservation {
        observed_at_unix: super::now(),
        error: None,
        node_pubkey: Some(pubkey(&key)?),
        channels,
    })
}
fn capture(html: &str, pattern: &str) -> Option<String> {
    let re = Regex::new(pattern).ok()?;
    let mut matches = re.captures_iter(html);
    let value = matches.next()?.get(1)?.as_str().to_owned();
    matches.next().is_none().then_some(value)
}
fn capacity(html: &str, label: &str) -> Option<u64> {
    let value = capture(
        html,
        &format!(
            r#"<div class="balance-amount">₿([0-9]+(?:,[0-9]{{3}})*)</div><div class="balance-label">{label}</div>"#
        ),
    )?;
    value
        .replace(',', "")
        .parse::<u64>()
        .ok()?
        .checked_mul(1000)
}

struct ForwardGuard(kube::api::Portforwarder);
impl Drop for ForwardGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub(super) async fn page(
    pods: &kube::Api<k8s_openapi::api::core::v1::Pod>,
    pod: &str,
    path: &'static str,
) -> Option<String> {
    use http_body_util::{BodyExt, Empty};
    use hyper::body::Bytes;
    use hyper_util::rt::TokioIo;
    // The existing loopback dashboard stays private; no service or host port.
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        let mut forward = ForwardGuard(pods.portforward(pod, &[8091]).await.ok()?);
        let stream = forward.0.take_stream(8091)?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .ok()?;
        let request = hyper::Request::builder()
            .uri(path)
            .header("Host", "127.0.0.1:8091")
            .body(Empty::<Bytes>::new())
            .ok()?;
        let read = async {
            let response = sender.send_request(request).await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            let mut body = response.into_body();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.ok()?.into_data() {
                    if bytes.len() + data.len() > 1_048_576 {
                        return None;
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            String::from_utf8(bytes).ok()
        };
        // Dropping this scope also drops the HTTP driver on timeout or completion.
        tokio::select! { biased; result = read => result, _ = connection => None }
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dashboard() -> String {
        format!(
            r#"<p class="node-address">Node ID: 02{}</p>"#,
            "a".repeat(64)
        )
    }
    #[test]
    fn pinned_html_projects_inactive_channels_and_capacity_units() {
        let html = format!(
            r#"<div class="metric-value">1</div><div class="metric-label">Total Channels</div><div class="channel-box"><span class="detail-label">Channel ID</span><span class="detail-value">{}</span><span class="detail-label">Node ID</span><span class="detail-value">03{}</span><span class="status-badge status-inactive">Inactive</span><div class="balance-amount">₿10,000</div><div class="balance-label">Total</div><div class="balance-amount">₿6,000</div><div class="balance-label">Outbound</div><div class="balance-amount">₿3,500</div><div class="balance-label">Inbound</div>"#,
            "b".repeat(64),
            "c".repeat(64)
        );
        let result = project(&dashboard(), &html).unwrap();
        assert_eq!(result.channels[0].local_msat, 6_000_000);
        assert!(result.channels[0].capacity_only);
        assert!(!result.channels[0].active);
        assert!(project(&dashboard(), &html.replace("₿6,000", "₿60,000")).is_none());
        assert!(project(&dashboard(), &html.replace("Total Channels", "Channels")).is_none());
        assert!(project(&dashboard(), "<html>error</html>").is_none());
        let empty = r#"<div class="metric-value">0</div><div class="metric-label">Total Channels</div>No channels found."#;
        assert!(project(&dashboard(), empty).unwrap().channels.is_empty());
    }
}
