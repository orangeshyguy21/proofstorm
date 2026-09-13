use proofstorm_prober::{PORT, PROTOCOL_VERSION, worker::Worker};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments == ["--self-check"] {
        println!(
            "{}",
            serde_json::json!({"protocol_version": PROTOCOL_VERSION})
        );
        return Ok(());
    }
    let [instance_key, namespace] = arguments.as_slice() else {
        return Err("expected instance key and namespace".into());
    };
    let worker = Worker::from_system_config(instance_key, namespace)?;
    // Only the authenticated Kubernetes port-forward transport can reach this listener.
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, PORT)).await?;
    worker
        .serve(listener, async {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("install termination handler");
                tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
