use gloo_net::http::Request;
use leptos::{prelude::*, task::spawn_local};
use proofstorm_view::{LocalConnectionState as State, LocalConnectionView as View};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, closure::Closure};

#[derive(Clone, Copy)]
struct Connections {
    views: RwSignal<Vec<View>>,
    token: RwSignal<String>,
    online: RwSignal<bool>,
    available: RwSignal<bool>,
    denied: RwSignal<bool>,
}

pub fn provide_connections() {
    let state = Connections {
        views: RwSignal::new(Vec::new()),
        token: RwSignal::new(String::new()),
        online: RwSignal::new(false),
        available: RwSignal::new(false),
        denied: RwSignal::new(false),
    };
    provide_context(state);
    let stream = StoredValue::new_local(
        None::<(
            web_sys::EventSource,
            Closure<dyn FnMut(web_sys::MessageEvent)>,
            Closure<dyn FnMut(web_sys::Event)>,
        )>,
    );
    on_cleanup(move || {
        stream.with_value(|stream| {
            if let Some((source, _, _)) = stream {
                source.close();
            }
        })
    });
    spawn_local(async move {
        let Ok(response) = Request::get("/v1/gui/context").send().await else {
            return;
        };
        if !response.ok() {
            return;
        }
        let Ok(context) = response.json::<Value>().await else {
            return;
        };
        if context["managed"] != true {
            return;
        }
        state.available.set(true);
        state
            .token
            .set(context["csrf"].as_str().unwrap_or_default().into());
        if let Ok(response) = Request::get("/v1/gui/connections").send().await {
            if response.status() == 403 {
                state.denied.set(true);
                return;
            }
        }
        let Ok(source) = web_sys::EventSource::new("/v1/gui/connections/events") else {
            return;
        };
        let message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |event: web_sys::MessageEvent| {
                if let Some(data) = event
                    .data()
                    .as_string()
                    .and_then(|s| serde_json::from_str::<Vec<View>>(&s).ok())
                {
                    state.views.set(data);
                    state.online.set(true);
                }
            },
        );
        let error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| state.online.set(false));
        let _ = source
            .add_event_listener_with_callback("connections", message.as_ref().unchecked_ref());
        source.set_onerror(Some(error.as_ref().unchecked_ref()));
        stream.set_value(Some((source, message, error)));
    });
}

fn current(state: Connections, cell: &str, incarnation: &str, component: &str) -> Option<View> {
    state.views.with(|views| {
        views
            .iter()
            .find(|v| v.cell == cell && v.incarnation == incarnation && v.component == component)
            .cloned()
    })
}

pub fn summary(
    cell: String,
    incarnation: String,
    component: String,
    endpoints: Signal<usize>,
) -> Signal<String> {
    let state = expect_context::<Connections>();
    Signal::derive(move || {
        let endpoints = endpoints.get();
        let view = current(state, &cell, &incarnation, &component);
        if view.is_some() && !state.online.get() {
            return "Local connection status unavailable".into();
        }
        match view {
            Some(v) => match v.state {
                State::Connected => format!(
                    "Local connection active · port {}",
                    v.url
                        .as_deref()
                        .and_then(|s| s.rsplit(':').next())
                        .unwrap_or("—")
                ),
                State::Connecting => "Connecting locally…".into(),
                State::Disconnecting => "Disconnecting…".into(),
                State::Failed => "Local connection failed".into(),
                State::Disconnected => format!("{endpoints} available · local connection closed"),
            },
            None => format!("{endpoints} available"),
        }
    })
}

async fn action(path: &str, token: &str, body: Value) -> Result<(), String> {
    let controller = web_sys::AbortController::new().map_err(|_| "Browser request unavailable")?;
    let abort = controller.clone();
    let _timeout = gloo_timers::callback::Timeout::new(20_000, move || abort.abort());
    let response = Request::post(path)
        .header("X-Proofstorm-Session", token)
        .abort_signal(Some(&controller.signal()))
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|_| {
            "Could not confirm the request. Connection status will refresh when the GUI reconnects."
                .to_owned()
        })?;
    if response.ok() {
        return Ok(());
    }
    let body = response
        .json::<Value>()
        .await
        .map_err(|_| "Invalid server response")?;
    Err(body["error"]["message"]
        .as_str()
        .unwrap_or("Connection request failed")
        .into())
}

#[component]
pub fn MintConnection(cell: String, incarnation: String, component: String) -> impl IntoView {
    let state = expect_context::<Connections>();
    let request =
        StoredValue::new(json!({"cell":cell,"incarnation":incarnation,"component":component}));
    let view = Memo::new(move |_| current(state, &cell, &incarnation, &component));
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let copied = RwSignal::new(false);
    let copying = RwSignal::new(false);
    let displayed_url = RwSignal::new(String::new());
    let connected = move || {
        state.online.get()
            && view.get().is_some_and(|v| {
                matches!(v.state, State::Connected | State::Disconnecting) && v.url.is_some()
            })
    };
    // Keep the last URL mounted while its container animates closed.
    Effect::new(move |_| {
        if let Some(url) = view.get().and_then(|v| v.url) {
            displayed_url.set(url);
        }
        if !connected() {
            copied.set(false);
        }
    });
    let active = move || {
        view.get().is_some_and(|v| {
            matches!(
                v.state,
                State::Connected | State::Connecting | State::Disconnecting
            )
        })
    };
    view! {
        <div class="mint-local-connection">
            <Show when=move ||state.available.get() fallback=||view!{<p>"Open this cell with storm gui to connect locally."</p>}>
                <Show when=move ||!state.online.get()><p role="status">{move ||if state.denied.get(){"Local connection access is not enabled for this developer."}else{"Connection status unavailable. Reconnecting…"}}</p></Show>
                <div class="connection-url-reveal" class:visible=connected aria-hidden=move ||(!connected()).to_string()>
                    <div class="connection-url-clip">
                        <div class="connection-url-control">
                            <code>{move ||displayed_url.get()}</code>
                            <button class="connection-copy" class:copied=move ||copied.get()
                                aria-label=move ||if copied.get(){"URL copied"}else{"Copy URL"}
                                disabled=move ||!connected() || copying.get() || copied.get()
                                tabindex=move ||if connected(){0}else{-1}
                                on:click=move |_| {
                                    if let Some(url) = view.get().and_then(|v|v.url) {
                                        copying.set(true);
                                        error.set(None);
                                        spawn_local(async move {
                                            let success = if let Some(window) = web_sys::window() {
                                                wasm_bindgen_futures::JsFuture::from(window.navigator().clipboard().write_text(&url)).await.is_ok()
                                            } else { false };
                                            let _ = copying.try_set(false);
                                            if success {
                                                let _ = copied.try_set(true);
                                                gloo_timers::future::TimeoutFuture::new(1800).await;
                                                let _ = copied.try_set(false);
                                            } else {
                                                let _ = error.try_set(Some("Copy unavailable; select the URL.".into()));
                                            }
                                        });
                                    }
                                }>
                                <svg class="copy-glyph" viewBox="0 0 24 24" aria-hidden="true"><rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V4H4v12h4"/></svg>
                                <svg class="copy-check" viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg>
                            </button>
                        </div>
                    </div>
                </div>
                <span class="connection-copy-status" role="status">{move ||if copied.get(){"URL copied"}else{""}}</span>
                <button class="connection-action" class:working=move ||busy.get() || view.get().is_some_and(|v|matches!(v.state,State::Connecting|State::Disconnecting))
                    disabled=move ||busy.get() || !state.online.get() || view.get().is_some_and(|v|v.state == State::Disconnecting)
                    on:click=move |_| {
                        let closing = active();
                        let body = if closing { json!({"id":view.get().map(|v|v.id)}) } else {request.get_value()};
                        busy.set(true); error.set(None); copied.set(false);
                        spawn_local(async move {
                            let result = action(if closing {"/v1/gui/connections/close"} else {"/v1/gui/connections/open"}, &state.token.get_untracked(), body).await;
                            let _ = error.try_set(result.err()); let _ = busy.try_set(false);
                        });
                    }><span class="connection-spinner" aria-hidden="true"></span><span>{move ||
                        match view.get().map(|v|v.state) {Some(State::Connecting)=>"Cancel connection",Some(State::Disconnecting)=>"Disconnecting…",Some(State::Connected)=>"Disconnect",_=>"Connect locally"}
                    }</span></button>
                {move ||view.get().filter(|v|matches!(v.state,State::Failed|State::Disconnected)).and_then(|v|v.message).map(|message|view!{<p role="status">{message}</p>})}
                {move ||error.get().map(|message|view!{<p class="observation-warning" role="alert">{message}</p>})}
            </Show>
        </div>
    }
}
