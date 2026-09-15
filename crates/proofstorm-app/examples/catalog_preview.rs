//! Disposable, in-memory catalog UI fixture; no real cluster or user database.
//! Build with `PROOFSTORM_WEB_DIST` pointing to an isolated web build.
use proofstorm_app::{Runtime, cell::Cells};
use proofstorm_core::{CandidateBuildPhase, CandidateInput};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = proofstorm_store::Store::memory()?;
    proofstorm_app::developer::configure(&store, "catalog-preview", "preview")?;
    for (id, implementation, succeeded) in [
        ("preview-cdk", "cdk", true),
        ("preview-coco", "cocod-wallet", false),
    ] {
        let request = proofstorm_app::candidate::CandidateBuildRequest {
            candidate_id: id.into(),
            implementation: implementation.into(),
            idempotency_key: id.into(),
            source: Some(CandidateInput::Commit {
                sha: Some("a".repeat(40)),
                url: None,
            }),
            pull_request_url: String::new(),
            base_version: None,
            build_profile: None,
        };
        let mut candidate =
            proofstorm_app::candidate::admit(&store, "catalog-preview", "preview", &request)
                .await?;
        candidate.phase = if succeeded {
            CandidateBuildPhase::Succeeded
        } else {
            CandidateBuildPhase::Failed
        };
        candidate.completed_at_unix = Some(candidate.accepted_at_unix + 1);
        if succeeded {
            candidate.image = Some(format!(
                "proofstorm-registry.localhost:5000/candidates/cdk@sha256:{}",
                "b".repeat(64)
            ));
        } else {
            candidate.error_message =
                Some("Synthetic failure for catalog preview; no source build was executed.".into());
        }
        store.update_candidate_build("catalog-preview", &candidate)?;
    }
    let runtime = Runtime {
        client: kube::Client::new(
            tower::service_fn(|_: http::Request<kube::client::Body>| async {
                Ok::<_, std::io::Error>(http::Response::new(kube::client::Body::from(
                    serde_json::json!({"apiVersion":"v1","kind":"List","metadata":{},"items":[]})
                        .to_string()
                        .into_bytes(),
                )))
            }),
            "default",
        ),
        control_namespace: "default".into(),
        cluster_source: "catalog-preview".into(),
    };
    let cells = Cells::new(store, runtime, "catalog-preview".into(), "preview".into());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8799").await?;
    eprintln!("Catalog preview at http://127.0.0.1:8799 (synthetic fixtures)");
    proofstorm_app::http::serve_listener(cells, listener).await?;
    Ok(())
}
