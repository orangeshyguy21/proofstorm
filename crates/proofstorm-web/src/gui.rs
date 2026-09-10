use gloo_net::http::Request;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{Value, json};

/// Vendor paths from OpenCode's UI; see assets/AGENT-MARKS.md.
#[component]
fn AgentMark(agent: &'static str) -> impl IntoView {
    let path = match agent {
        "opencode" => "M12 4H4V16H12V4ZM16 20H0V0H16V20Z",
        "claude" => {
            "M26.9568 9.88184H22.1265L30.7753 31.7848H35.4917L26.9568 9.88184ZM13.028 9.88184L4.4917 31.7848H9.32203L11.2305 27.1793H20.2166L22.0126 31.6724H26.8444L18.0832 9.88184H13.028ZM12.5783 23.1361L15.4987 15.3853L18.5315 23.1361H12.5783Z"
        }
        _ => {
            "M32.8377 17.282C33.2127 16.25 33.3072 15.218 33.2127 14.1875C33.1197 13.1571 32.7447 12.1251 32.2752 11.1876C31.4322 9.78209 30.2127 8.6571 28.8072 8.0001C27.3072 7.34461 25.7127 7.15711 24.1197 7.53211C23.3698 6.78212 22.5253 6.12512 21.5878 5.65713C20.6503 5.18913 19.5253 5.00013 18.4948 5.00013C16.8851 4.99074 15.3125 5.48246 13.9948 6.40712C12.6824 7.34311 11.7449 8.6571 11.2754 10.1571C10.1504 10.4376 9.21289 10.9071 8.27539 11.4696C7.4324 12.1251 6.77541 12.9696 6.21291 13.8126C5.36992 15.2195 5.08792 16.8125 5.27542 18.407C5.46399 19.9968 6.11605 21.496 7.1504 22.718C6.79608 23.7086 6.66795 24.7659 6.77541 25.8124C6.86991 26.8444 7.2449 27.8749 7.7129 28.8124C8.55739 30.2194 9.77538 31.3444 11.1824 31.9999C12.6824 32.6569 14.2753 32.8444 15.8698 32.4694C16.6198 33.2194 17.4628 33.8749 18.4003 34.3444C19.3378 34.8139 20.4628 34.9999 21.4948 34.9999C23.1043 35.0097 24.6769 34.5185 25.9947 33.5944C27.3072 32.6569 28.2447 31.3444 28.7127 29.8444C29.7719 29.6432 30.7682 29.1934 31.6197 28.5319C32.4627 27.8749 33.2127 27.1249 33.6822 26.1874C34.5251 24.7819 34.8071 23.1875 34.6196 21.5945C34.4322 20 33.8697 18.5015 32.8377 17.282ZM21.5878 33.0304C20.0878 33.0304 18.9628 32.5609 17.9323 31.7179C17.9323 31.7179 18.0253 31.6234 18.1198 31.6234L24.1197 28.1554C24.2862 28.0803 24.4196 27.9469 24.4947 27.7804C24.5698 27.636 24.6021 27.4731 24.5877 27.3109V18.875L27.1197 20.375V27.3124C27.1455 28.0547 27.0215 28.7945 26.755 29.4878C26.4885 30.181 26.085 30.8134 25.5687 31.3473C25.0523 31.8811 24.4337 32.3054 23.7497 32.5949C23.0658 32.8843 22.3305 33.0314 21.5878 33.0304ZM9.49488 27.8749C8.83789 26.7499 8.55739 25.4374 8.83789 24.125C8.83789 24.125 8.93239 24.2195 9.02539 24.2195L15.0253 27.6874C15.1693 27.7638 15.3325 27.7966 15.4948 27.7819C15.6823 27.7819 15.8698 27.7819 15.9628 27.6874L23.2753 23.4695V26.3749L17.1823 29.9374C16.5506 30.3042 15.8527 30.5427 15.1287 30.6393C14.4046 30.7358 13.6686 30.6884 12.9629 30.4999C11.4629 30.1249 10.2449 29.1874 9.49488 27.8749ZM7.9004 14.8445C8.56239 13.7234 9.58826 12.8627 10.8074 12.4056V19.532C10.8074 19.718 10.8074 19.907 10.9004 20C10.9755 20.1665 11.1089 20.2998 11.2754 20.375L18.5878 24.5944L16.0573 26.0944L10.0574 22.625C9.41842 22.2639 8.85742 21.7797 8.40684 21.2004C7.95627 20.6211 7.62506 19.9582 7.4324 19.25C7.05741 17.8445 7.1504 16.157 7.9004 14.8445ZM28.6197 19.625L21.3073 15.407L23.8377 13.9071L29.8377 17.375C30.7752 17.9375 31.5252 18.6875 31.9947 19.625C32.4642 20.5625 32.7447 21.5945 32.6502 22.7195C32.5603 23.7755 32.1699 24.7837 31.5252 25.6249C30.8697 26.4694 30.0252 27.1249 28.9947 27.4999V20.375C28.9947 20.1875 28.9947 20 28.9002 19.907C28.9002 19.907 28.8072 19.718 28.6197 19.625ZM31.1502 15.875C31.1502 15.875 31.0572 15.782 30.9627 15.782L24.9627 12.3126C24.7752 12.2196 24.6822 12.2196 24.4947 12.2196C24.3072 12.2196 24.1197 12.2196 24.0252 12.3126L16.7128 16.532V13.6251L22.8073 10.0626C23.7448 9.50009 24.7752 9.31259 25.9002 9.31259C26.9322 9.31259 27.9627 9.68759 28.9002 10.3446C29.7447 11.0001 30.4947 11.8446 30.8697 12.7821C31.2447 13.7196 31.3377 14.8445 31.1502 15.875ZM15.4003 21.125L12.8699 19.625V12.5946C12.8699 11.5626 13.1503 10.4376 13.7128 9.59459C14.2753 8.6571 15.1198 8.0001 16.0573 7.53211C17.0127 7.05249 18.0956 6.88812 19.1503 7.06261C20.1823 7.15711 21.2128 7.62511 22.0573 8.2821C22.0573 8.2821 21.9628 8.3751 21.8698 8.3751L15.8698 11.8446C15.7033 11.9197 15.57 12.0531 15.4948 12.2196C15.4003 12.4071 15.4003 12.5001 15.4003 12.6876V21.125ZM16.7128 18.125L19.9948 16.25L23.2753 18.125V21.875L19.9948 23.75L16.7128 21.875V18.125Z"
        }
    };
    view! { <svg class="gui-agent-mark" viewBox=if agent=="opencode" {"0 0 16 20"} else {"0 0 40 40"} fill="currentColor" aria-hidden="true"><path d=path /></svg> }
}

#[derive(Clone, Default)]
pub struct Seed {
    project: String,
    error: Option<String>,
    managed: bool,
}

pub async fn bootstrap() -> Seed {
    let Some(window) = web_sys::window() else {
        return Seed::default();
    };
    let storage = window.session_storage().ok().flatten();
    let mut seed = Seed {
        project: storage
            .as_ref()
            .and_then(|s| s.get_item("proofstorm.gui.project").ok().flatten())
            .unwrap_or_default(),
        ..Seed::default()
    };
    let hash = window.location().hash().unwrap_or_default();
    let Ok(params) = web_sys::UrlSearchParams::new_with_str(hash.trim_start_matches('#')) else {
        return seed;
    };
    let Some(token) = params.get("session") else {
        return seed;
    };
    seed.managed = true;
    seed.project = params.get("project").unwrap_or_default();
    if let Some(storage) = storage {
        let _ = storage.set_item("proofstorm.gui.project", &seed.project);
    }
    // Remove the bearer fragment from the current history entry before any API work.
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some("/"));
    }
    if let Err(error) = post("/v1/gui/session", &token, json!({})).await {
        seed.error = Some(error);
    }
    seed
}

async fn post(path: &str, token: &str, value: Value) -> Result<Value, String> {
    let controller = web_sys::AbortController::new().map_err(|_| "Browser request unavailable")?;
    let abort = controller.clone();
    let _timer = gloo_timers::callback::Timeout::new(
        if path.ends_with("/session") {
            10_000
        } else {
            240_000
        },
        move || abort.abort(),
    );
    let response=Request::post(path).header("X-Proofstorm-Session",token)
        .abort_signal(Some(&controller.signal())).json(&value).map_err(|_|"Invalid request")?
        .send().await.map_err(|_|"The GUI could not be reached. Reopen it with proofstorm gui; an accepted attachment may still finish.".to_owned())?;
    let ok = response.ok();
    let value: Value = response
        .json()
        .await
        .map_err(|_| "The GUI returned an invalid response")?;
    if !ok {
        if path.ends_with("/open") && value["connection_conflict"].is_object() {
            return Ok(value);
        }
        return Err(value["error"]["message"]
            .as_str()
            .unwrap_or("The action could not be completed")
            .into());
    }
    Ok(value)
}

#[derive(Clone, Copy)]
struct Launcher {
    managed: RwSignal<bool>,
    context: RwSignal<Option<Value>>,
    project: RwSignal<String>,
    harness: RwSignal<String>,
    modal: RwSignal<bool>,
    busy: RwSignal<bool>,
    picking: RwSignal<bool>,
    conflict: RwSignal<Option<Value>>,
    error: RwSignal<Option<String>>,
    result: RwSignal<Option<Value>>,
    open_agent: Callback<(&'static str, Option<String>)>,
    pick_folder: Callback<()>,
}

/// One session and action state for the header dialog and empty-state launcher.
#[allow(
    clippy::too_many_lines,
    reason = "the shared session owns loading, activation and serialized launch state"
)]
pub fn provide_launcher() {
    let seed = use_context::<Seed>().unwrap_or_default();
    let managed = RwSignal::new(seed.managed);
    let context = RwSignal::new(None::<Value>);
    let project = RwSignal::new(seed.project);
    let harness = RwSignal::new("codex".to_owned());
    let modal = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let picking = RwSignal::new(false);
    let conflict = RwSignal::new(None::<Value>);
    let error = RwSignal::new(seed.error);
    let result = RwSignal::new(None::<Value>);
    let generation = RwSignal::new(0_u64);

    spawn_local(async move {
        let controller = web_sys::AbortController::new().ok();
        let abort = controller.clone();
        let _timeout = gloo_timers::callback::Timeout::new(30_000, move || {
            if let Some(abort) = abort {
                abort.abort();
            }
        });
        if let Ok(response) = Request::get("/v1/gui/context")
            .abort_signal(
                controller
                    .as_ref()
                    .map(web_sys::AbortController::signal)
                    .as_ref(),
            )
            .send()
            .await
        {
            if response.ok() {
                if let Ok(value) = response.json::<Value>().await {
                    managed.set(true);
                    generation.set(value["activation_generation"].as_u64().unwrap_or(0));
                    context.set(Some(value));
                }
            } else if response.status() == 401 {
                managed.set(true);
                error.set(Some(
                    "Run proofstorm gui again to unlock this browser session.".into(),
                ));
            }
        }
        if context.get_untracked().is_none()
            && managed.get_untracked()
            && error.get_untracked().is_none()
        {
            error.set(Some(
                "Could not check installed apps. Reload this page or run proofstorm gui again."
                    .into(),
            ));
        }
    });

    // Existing tabs may acknowledge a focus request. If the browser refuses focus,
    // the launcher falls back to its normal default-browser URL open operation.
    let polling = RwSignal::new(false);
    let interval = gloo_timers::callback::Interval::new(1000, move || {
        if polling.get_untracked() {
            return;
        }
        let Some(ctx) = context.get_untracked() else {
            return;
        };
        let token = ctx["csrf"].as_str().unwrap_or_default().to_owned();
        polling.set(true);
        spawn_local(async move {
            if let Ok(response) = Request::get("/v1/gui/activation").send().await {
                if let Ok(value) = response.json::<Value>().await {
                    let next = value["generation"].as_u64().unwrap_or(0);
                    if next > generation.get_untracked() {
                        generation.set(next);
                        if let Some(path) = value["project"].as_str() {
                            // Do not acknowledge a different project while a user
                            // is reviewing this dialog. The CLI can open a new tab.
                            if (modal.get_untracked()
                                || busy.get_untracked()
                                || conflict.get_untracked().is_some())
                                && project.get_untracked() != path
                            {
                                polling.set(false);
                                return;
                            }
                            if !modal.get_untracked()
                                && !busy.get_untracked()
                                && conflict.get_untracked().is_none()
                            {
                                project.set(path.into());
                                result.set(None);
                                error.set(None);
                                if let Some(storage) = web_sys::window()
                                    .and_then(|w| w.session_storage().ok().flatten())
                                {
                                    let _ = storage.set_item("proofstorm.gui.project", path);
                                }
                            }
                            if let Some(window) = web_sys::window() {
                                let _ = window.focus();
                                if window
                                    .document()
                                    .is_some_and(|d| d.has_focus().unwrap_or(false))
                                {
                                    let _ = post(
                                        "/v1/gui/focus-ack",
                                        &token,
                                        json!({"generation":next}),
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                }
            }
            polling.set(false);
        });
    });
    let interval = StoredValue::new_local(interval);
    on_cleanup(move || interval.dispose());

    let open_agent = Callback::new(
        move |(agent, confirmation): (&'static str, Option<String>)| {
            if busy.get_untracked() || project.get_untracked().trim().is_empty() {
                return;
            }
            let Some(ctx) = context.get_untracked() else {
                return;
            };
            if ctx["runtime_ready"] != true
                || !ctx["agents"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|a| a == agent))
            {
                return;
            }
            let folder = project.get_untracked();
            let token = ctx["csrf"].as_str().unwrap_or_default().to_owned();
            harness.set(agent.into());
            busy.set(true);
            conflict.set(None);
            error.set(None);
            result.set(None);
            spawn_local(async move {
                match post(
                    "/v1/gui/open",
                    &token,
                    json!({"project":folder,"harness":agent,"replace_connection":confirmation}),
                )
                .await
                {
                    Ok(value) if value["connection_conflict"].is_object() => {
                        conflict.set(Some(value["connection_conflict"].clone()));
                    }
                    Ok(value) => result.set(Some(value)),
                    Err(message) => error.set(Some(message)),
                }
                busy.set(false);
            });
        },
    );

    let pick_folder = Callback::new(move |()| {
        if busy.get_untracked() {
            return;
        }
        let Some(ctx) = context.get_untracked() else {
            return;
        };
        let token = ctx["csrf"].as_str().unwrap_or_default().to_owned();
        let current = project.get_untracked();
        busy.set(true);
        picking.set(true);
        error.set(None);
        spawn_local(async move {
            match post(
                "/v1/gui/pick-folder",
                &token,
                json!({"project":(!current.is_empty()).then_some(current)}),
            )
            .await
            {
                Ok(value) => {
                    if let Some(path) = value["project"].as_str() {
                        project.set(path.into());
                        conflict.set(None);
                        result.set(None);
                        if let Some(storage) =
                            web_sys::window().and_then(|w| w.session_storage().ok().flatten())
                        {
                            let _ = storage.set_item("proofstorm.gui.project", path);
                        }
                    }
                }
                Err(message) => error.set(Some(message)),
            }
            picking.set(false);
            busy.set(false);
        });
    });

    provide_context(Launcher {
        managed,
        context,
        project,
        harness,
        modal,
        busy,
        picking,
        conflict,
        error,
        result,
        open_agent,
        pick_folder,
    });
}

#[component]
pub fn GuiControls() -> impl IntoView {
    let Launcher {
        managed,
        modal,
        busy,
        conflict,
        result,
        ..
    } = expect_context::<Launcher>();
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let trigger = NodeRef::<leptos::html::Button>::new();
    let close = move |_| {
        if !busy.get_untracked() {
            if let Some(dialog) = dialog.get() {
                dialog.close();
            }
            modal.set(false);
            if let Some(button) = trigger.get() {
                let _ = button.focus();
            }
        }
    };
    view! {
        <Show when=move ||managed.get()>
            <button class="gui-trigger" node_ref=trigger disabled=move ||busy.get() on:click=move |_| {
                result.set(None);
                conflict.set(None);
                modal.set(true);
                if let Some(dialog)=dialog.get() { let _=dialog.show_modal(); }
            }>"Launch Agent"</button>
            <dialog class="gui-dialog" node_ref=dialog aria-labelledby="gui-title"
                on:cancel=move |event: web_sys::Event| { if busy.get_untracked() {event.prevent_default();} else {modal.set(false);} }>
                <div class="gui-dialog-heading"><h2 id="gui-title">"Open in your agent"</h2>
                    <button class="icon-button" aria-label="Close project connection" disabled=move ||busy.get() on:click=close>"×"</button></div>
                <AgentLauncher />
            </dialog>
        </Show>
    }
}

#[component]
fn AgentLauncher() -> impl IntoView {
    let Launcher {
        context,
        project,
        harness,
        busy,
        picking,
        conflict,
        error,
        result,
        open_agent,
        pick_folder,
        ..
    } = expect_context::<Launcher>();
    view! {
        <div class="gui-launcher">
                <div class="gui-agents">
                    {[("codex", "Codex"), ("opencode", "OpenCode"), ("claude", "Claude Code")].into_iter().map(|(agent, label)| view! {
                        <Show when=move ||context.get().is_some_and(|ctx|ctx["agents"].as_array().is_some_and(|a|a.iter().any(|v|v==agent)))>
                            <button class=format!("gui-agent gui-agent-{agent}") aria-label=format!("Open in {label}")
                                disabled=move ||busy.get() || project.get().trim().is_empty() || context.get().is_none_or(|ctx|ctx["runtime_ready"]!=true)
                                on:click=move |_|open_agent.run((agent, None))>
                                <AgentMark agent=agent />
                                <span>{label}</span><span class="gui-agent-arrow" aria-hidden="true">"↗"</span>
                            </button>
                        </Show>
                    }).collect_view()}
                </div>
                <button class="gui-project gui-folder-picker" aria-label="Choose project folder" aria-haspopup="dialog"
                    title=move ||project.get() disabled=move ||busy.get() || context.get().is_none()
                    on:click=move |_|pick_folder.run(())>
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v10H3V7Z" /></svg>
                    <span>{move ||if project.get().is_empty() {"Choose a project folder".into()} else {project.get()}}</span>
                    <span class="gui-folder-chevron" aria-hidden="true">"⌄"</span>
                </button>
                <div class="gui-check" aria-live="polite">
                    <Show when=move ||context.get().is_none() && error.get().is_none()><p class="gui-muted">"Looking for installed apps…"</p></Show>
                    <Show when=move ||context.get().is_some_and(|ctx|ctx["agents"].as_array().is_some_and(Vec::is_empty))><p class="gui-muted">"No supported native apps found. Install Codex, OpenCode or Claude in Applications, then reopen this page."</p></Show>
                    <Show when=move ||busy.get()><p class="gui-muted"><span class="gui-connecting" aria-hidden="true"></span>{move ||if picking.get() {"Choose a folder in the macOS dialog…"} else {"Connecting proofstorm and opening your project…"}}</p></Show>
                    {move ||conflict.get().map(|existing|view! {
                        <div class="gui-conflict">
                            <p>"Replace the old “"{existing["name"].as_str().unwrap_or_default().to_owned()}"” connection with proofstorm?"</p>
                            <p class="gui-muted">"A backup will be saved. Other connections and settings stay unchanged."</p>
                            <code>{existing["config"].as_str().unwrap_or_default().to_owned()}</code>
                            <button class="gui-primary" disabled=move ||busy.get() on:click=move |_| {
                                let agent = match harness.get_untracked().as_str() {"opencode"=>"opencode", "claude"=>"claude", _=>"codex"};
                                open_agent.run((agent, existing["confirmation"].as_str().map(str::to_owned)));
                            }>"Replace and open"</button>
                            <button class="gui-secondary" disabled=move ||busy.get() on:click=move |_|conflict.set(None)>"Keep existing"</button>
                        </div>
                    })}
                    {move ||error.get().map(|message|view!{<p class="gui-error" role="alert">{message}</p>})}
                    {move ||result.get().map(|value| {
                        let opened=value["app_opened"]==true;
                        let agent = match value["harness"].as_str() {
                            Some("codex") => "Codex",
                            Some("opencode") => "OpenCode",
                            Some("claude" | "claude-code") => "Claude Code",
                            _ => "your agent",
                        };
                        let message = if opened {
                            format!("Opened in {agent}.")
                        } else {
                            format!("Couldn’t open {agent}. Your Proofstorm setup is saved.")
                        };
                        view!{
                            <p class=if opened {"gui-success"} else {"gui-error"}>{message}</p>
                            {value["launch_error"].as_str().map(|message|view!{<p class="gui-error">{message.to_owned()}</p>})}
                            <p class="gui-muted">"Approve any folder or MCP prompts in the app. Existing sessions may need a restart."</p>
                        }
                    })}
                </div>
                {move ||context.get().filter(|ctx|ctx["runtime_ready"]!=true).map(|ctx|view!{<p class="gui-error">"Runtime not ready. Run proofstorm setup, then reopen the GUI. "{ctx["runtime_error"].as_str().unwrap_or_default().to_owned()}</p>})}

        </div>
    }
}

/// The exact same buttons, folder and conflict handling used by the dialog.
#[component]
pub fn EmptyAgentLauncher() -> impl IntoView {
    let Launcher { managed, .. } = expect_context::<Launcher>();
    view! {
        <Show when=move ||managed.get()>
            <div class="empty-agent-launcher">
                <p class="empty-agent-intro">"Open a coding agent to create your first lab."</p>
                <AgentLauncher />
            </div>
        </Show>
    }
}
