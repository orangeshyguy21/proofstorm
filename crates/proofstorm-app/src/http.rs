//! Local, read-only HTTP transport. Uses the same workspace/principal as the CLI.
use crate::{Error, environment::EnvironmentQuery, lab::Labs};
use http_body_util::Full;
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use std::{convert::Infallible, net::Ipv4Addr, time::Duration};
use tokio::{net::TcpListener, task::JoinSet};

pub async fn serve(labs: Labs, port: u16) -> Result<(), Error> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|e| Error::failure(e.to_string(), None))?;
    eprintln!(
        "environment API: http://{}/v1/environment (read-only, local processes only)",
        listener
            .local_addr()
            .map_err(|e| Error::failure(e.to_string(), None))?
    );
    serve_listener(labs, listener).await
}
/// Serve an already-bound loopback listener, also used by transport contract tests.
pub async fn serve_listener(labs: Labs, listener: TcpListener) -> Result<(), Error> {
    let address = listener
        .local_addr()
        .map_err(|e| Error::failure(e.to_string(), None))?;
    if !address.ip().is_loopback() {
        return Err(Error::problem(
            "loopback_required",
            "the environment API only serves local loopback addresses",
        ));
    }
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>return Ok(()),
            accepted=listener.accept(),if tasks.len()<16=> {
                let (socket,_)=accepted.map_err(|e|Error::failure(e.to_string(),None))?;
                let labs=labs.clone();
                tasks.spawn(async move {
                    let service=service_fn(move |request|handle(labs.clone(),request));
                    let _=tokio::time::timeout(Duration::from_secs(30),http1::Builder::new().keep_alive(false).max_buf_size(8192).serve_connection(TokioIo::new(socket),service)).await;
                });
            },
            _=tasks.join_next(),if !tasks.is_empty()=>{}
        }
    }
}
async fn handle(
    labs: Labs,
    request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let host = request
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.parse::<hyper::http::uri::Authority>().ok());
    if request.headers().contains_key("origin")
        || !host.is_some_and(|h| matches!(h.host(), "127.0.0.1" | "localhost" | "[::1]"))
    {
        return Ok(error(StatusCode::FORBIDDEN, "local_clients_only"));
    }
    if request.method() != hyper::Method::GET {
        return Ok(error(StatusCode::METHOD_NOT_ALLOWED, "read_only"));
    }
    match request.uri().path() {
        "/v1/environment" => {
            let Ok(query) = serde_urlencoded::from_str::<EnvironmentQuery>(
                request.uri().query().unwrap_or_default(),
            ) else {
                return Ok(error(StatusCode::BAD_REQUEST, "invalid_query"));
            };
            match labs.environment(&query).await {
                Ok(view) => Ok(json(StatusCode::OK, &view)),
                Err(e) => {
                    let code = e
                        .details
                        .as_ref()
                        .and_then(|v| v["code"].as_str())
                        .unwrap_or("environment_unavailable");
                    let status = if code == "access_denied" {
                        StatusCode::FORBIDDEN
                    } else {
                        match e.kind {
                            crate::ErrorKind::Invalid => StatusCode::BAD_REQUEST,
                            crate::ErrorKind::Missing => StatusCode::NOT_FOUND,
                            crate::ErrorKind::Failure => StatusCode::SERVICE_UNAVAILABLE,
                        }
                    };
                    Ok(error(status, code))
                }
            }
        }
        "/v1/environment/schema" => {
            // Schema contains no workspace data. It is the exact shared read-model contract.
            Ok(json(
                StatusCode::OK,
                &schemars::schema_for!(crate::environment::EnvironmentView),
            ))
        }
        _ => Ok(error(StatusCode::NOT_FOUND, "not_found")),
    }
}
fn error(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    json(status, &serde_json::json!({"error":{"code":code}}))
}
fn json(status: StatusCode, value: &impl serde::Serialize) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(value)
        .unwrap_or_else(|_| b"{\"error\":{\"code\":\"serialization_failed\"}}".to_vec());
    let mut response = Response::new(Full::new(Bytes::from(bytes)));
    *response.status_mut() = status;
    response.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        "cache-control",
        hyper::header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        hyper::header::HeaderValue::from_static("nosniff"),
    );
    response
}
