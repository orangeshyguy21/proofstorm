//! Local, read-only HTTP transport. Uses the same workspace/principal as the CLI.
use crate::{Error, environment::EnvironmentQuery, lab::Labs};
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use tokio::sync::{Semaphore, watch};
type Body = BoxBody<Bytes, Infallible>;
use proofstorm_view::ObserverStatus;
use std::sync::{Arc, RwLock};
include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{convert::Infallible, net::Ipv4Addr, time::Duration};
use tokio::{net::TcpListener, task::JoinSet};

pub async fn serve(labs: Labs, port: u16) -> Result<(), Error> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|e| Error::failure(e.to_string(), None))?;
    eprintln!(
        "Proofstorm: http://{}/ (live environment; local access only)",
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
    let observer = crate::observer::Observer::start(labs.clone());
    let events = crate::events::Events::start(labs.clone(), observer.status.clone());
    let streams = Arc::new(Semaphore::new(8));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>return Ok(()),
            accepted=listener.accept(),if tasks.len()<16=> {
                let (socket,_)=accepted.map_err(|e|Error::failure(e.to_string(),None))?;
                let labs=labs.clone();
                let status=observer.status.clone();
                let events=events.receiver.clone();
                let streams=streams.clone();
                tasks.spawn(async move {
                    let service=service_fn(move |request|handle(labs.clone(),status.clone(),events.clone(),streams.clone(),request));
                    let _=http1::Builder::new().timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(10)).keep_alive(false).max_buf_size(8192).serve_connection(TokioIo::new(socket),service).await;
                });
            },
            _=tasks.join_next(),if !tasks.is_empty()=>{}
        }
    }
}
async fn handle(
    labs: Labs,
    observer: Arc<RwLock<ObserverStatus>>,
    events: watch::Receiver<u64>,
    streams: Arc<Semaphore>,
    request: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let host = request
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.parse::<hyper::http::uri::Authority>().ok());
    let local_host = host
        .as_ref()
        .is_some_and(|h| matches!(h.host(), "127.0.0.1" | "localhost" | "[::1]"));
    let same_origin = request.headers().get("origin").is_none_or(|origin| {
        origin.to_str().ok().is_some_and(|origin| {
            host.as_ref()
                .is_some_and(|host| origin == format!("http://{host}"))
        })
    });
    if !same_origin || !local_host {
        return Ok(error(StatusCode::FORBIDDEN, "local_clients_only"));
    }
    if request.method() != hyper::Method::GET {
        return Ok(error(StatusCode::METHOD_NOT_ALLOWED, "read_only"));
    }
    match request.uri().path() {
        "/v1/events" => Ok(event_stream(labs, events, streams)),
        "/v1/environment" => {
            let Ok(query) = serde_urlencoded::from_str::<EnvironmentQuery>(
                request.uri().query().unwrap_or_default(),
            ) else {
                return Ok(error(StatusCode::BAD_REQUEST, "invalid_query"));
            };
            match labs.environment(&query).await {
                Ok(view) => Ok(json(StatusCode::OK, &view)),
                Err(e) => {
                    eprintln!("environment read failed: {e}");
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
        "/v1/observer" => {
            if labs
                .store
                .authorize(
                    &labs.workspace,
                    &labs.principal,
                    proofstorm_core::Capability::ExperimentRead,
                )
                .is_err()
            {
                return Ok(error(StatusCode::FORBIDDEN, "access_denied"));
            }
            Ok(observer.read().map_or_else(
                |_| error(StatusCode::SERVICE_UNAVAILABLE, "observer_unavailable"),
                |s| json(StatusCode::OK, &*s),
            ))
        }
        "/v1/environment/schema" => {
            // Schema contains no workspace data. It is the exact shared read-model contract.
            Ok(json(
                StatusCode::OK,
                &schemars::schema_for!(crate::environment::EnvironmentView),
            ))
        }
        path => Ok(asset(path)),
    }
}
fn error(status: StatusCode, code: &str) -> Response<Body> {
    json(status, &serde_json::json!({"error":{"code":code}}))
}
fn json(status: StatusCode, value: &impl serde::Serialize) -> Response<Body> {
    let bytes = serde_json::to_vec(value)
        .unwrap_or_else(|_| b"{\"error\":{\"code\":\"serialization_failed\"}}".to_vec());
    let mut response = Response::new(Full::new(Bytes::from(bytes)).boxed());
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

fn asset(path: &str) -> Response<Body> {
    let name = if path == "/" {
        "index.html"
    } else {
        path.trim_start_matches('/')
    };
    if let Some((_, mime, data)) = WEB_ASSETS.iter().find(|(file, _, _)| *file == name) {
        let mut response = Response::new(Full::new(Bytes::from_static(data)).boxed());
        response.headers_mut().insert(
            "content-type",
            hyper::header::HeaderValue::from_static(mime),
        );
        response.headers_mut().insert(
            "cache-control",
            hyper::header::HeaderValue::from_static("no-cache"),
        );
        response.headers_mut().insert(
            "x-content-type-options",
            hyper::header::HeaderValue::from_static("nosniff"),
        );
        response
    } else if path == "/" {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "web_assets_missing_run_make_web_then_rebuild",
        )
    } else {
        error(StatusCode::NOT_FOUND, "not_found")
    }
}

fn can_observe(labs: &Labs) -> bool {
    [
        proofstorm_core::Capability::LabRead,
        proofstorm_core::Capability::LabStatus,
        proofstorm_core::Capability::ExperimentRead,
    ]
    .into_iter()
    .all(|cap| {
        labs.store
            .authorize(&labs.workspace, &labs.principal, cap)
            .is_ok()
    })
}
fn event_stream(
    labs: Labs,
    events: watch::Receiver<u64>,
    streams: Arc<Semaphore>,
) -> Response<Body> {
    if !can_observe(&labs) {
        return error(StatusCode::FORBIDDEN, "access_denied");
    }
    let Ok(permit) = streams.try_acquire_owned() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "stream_limit");
    };
    // Notifications are invalidations, not a durable event log. Always refresh on connect,
    // including reconnects carrying Last-Event-ID. No history buffer or replay required.
    let stream = futures::stream::unfold(
        (events, true, labs, permit),
        |(mut events, first, labs, permit)| async move {
            let changed = if first {
                true
            } else {
                tokio::select! {
                    result = events.changed() => { if result.is_err() { return None; } true },
                    () = tokio::time::sleep(Duration::from_secs(2)) => false,
                }
            };
            if !can_observe(&labs) {
                return None;
            }
            let bytes = if changed {
                let version = *events.borrow_and_update();
                format!("event: environment\nid: {version}\ndata: {{\"refresh\":true}}\n\n")
            } else {
                ": keepalive\n\n".into()
            };
            Some((
                Ok::<_, Infallible>(Frame::data(Bytes::from(bytes))),
                (events, false, labs, permit),
            ))
        },
    );
    let mut response = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    for (name, value) in [
        ("content-type", "text/event-stream"),
        ("cache-control", "no-store"),
        ("x-accel-buffering", "no"),
        ("x-content-type-options", "nosniff"),
    ] {
        response
            .headers_mut()
            .insert(name, hyper::header::HeaderValue::from_static(value));
    }
    response
}
