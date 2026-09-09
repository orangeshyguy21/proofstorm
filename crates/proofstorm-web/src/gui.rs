use gloo_net::http::Request;
use leptos::{prelude::*, task::spawn_local};
use serde_json::{Value, json};

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
        return Err(value["error"]["message"]
            .as_str()
            .unwrap_or("The action could not be completed")
            .into());
    }
    Ok(value)
}

#[component]
#[allow(
    clippy::too_many_lines,
    reason = "the project dialog keeps its validation and explicit confirmation state together"
)]
pub fn GuiControls() -> impl IntoView {
    let seed = use_context::<Seed>().unwrap_or_default();
    let managed = RwSignal::new(seed.managed);
    let context = RwSignal::new(None::<Value>);
    let project = RwSignal::new(seed.project);
    let harness = RwSignal::new("codex".to_owned());
    let modal = RwSignal::new(false);
    let checking = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let preview = RwSignal::new(None::<Value>);
    let preview_input = RwSignal::new(String::new());
    let error = RwSignal::new(seed.error);
    let result = RwSignal::new(None::<Value>);
    let generation = RwSignal::new(0_u64);
    let request_id = RwSignal::new(0_u64);
    let dialog = NodeRef::<leptos::html::Dialog>::new();
    let trigger = NodeRef::<leptos::html::Button>::new();

    spawn_local(async move {
        if let Ok(response) = Request::get("/v1/gui/context").send().await {
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
                            if modal.get_untracked() && project.get_untracked() != path {
                                polling.set(false);
                                return;
                            }
                            if !modal.get_untracked() {
                                project.set(path.into());
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

    Effect::new(move |_| {
        let input = project.get();
        let agent = harness.get();
        let open = modal.get();
        let ctx = context.get();
        let version = request_id.get_untracked().wrapping_add(1);
        request_id.set(version);
        preview.set(None);
        preview_input.set(String::new());
        if !open || busy.get_untracked() {
            return;
        }
        result.set(None);
        let Some(ctx) = ctx else {
            return;
        };
        error.set(None);
        if input.trim().is_empty() {
            checking.set(false);
            return;
        }
        checking.set(true);
        let token = ctx["csrf"].as_str().unwrap_or_default().to_owned();
        let timer = gloo_timers::callback::Timeout::new(400, move || {
            spawn_local(async move {
                let response = post(
                    "/v1/gui/plan",
                    &token,
                    json!({"project":input,"harness":agent}),
                )
                .await;
                if request_id.get_untracked() != version {
                    return;
                }
                checking.set(false);
                match response {
                    Ok(value) => {
                        preview_input.set(input);
                        preview.set(Some(value));
                    }
                    Err(message) => error.set(Some(message)),
                }
            });
        });
        let timer = StoredValue::new_local(timer);
        on_cleanup(move || timer.dispose());
    });

    let open_agent = move |_| {
        if busy.get_untracked() || preview_input.get_untracked() != project.get_untracked() {
            return;
        }
        let (Some(plan), Some(ctx)) = (preview.get_untracked(), context.get_untracked()) else {
            return;
        };
        if plan["harness"].as_str() != Some(&harness.get_untracked()) {
            return;
        }
        let token = ctx["csrf"].as_str().unwrap_or_default().to_owned();
        busy.set(true);
        error.set(None);
        result.set(None);
        spawn_local(async move {
            match post(
                "/v1/gui/open",
                &token,
                json!({"project":plan["project"],"harness":plan["harness"]}),
            )
            .await
            {
                Ok(value) => result.set(Some(value)),
                Err(message) => error.set(Some(message)),
            }
            busy.set(false);
        });
    };
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
            <button class="gui-trigger" node_ref=trigger on:click=move |_| {
                modal.set(true);
                if let Some(dialog)=dialog.get() { let _=dialog.show_modal(); }
            }>"Connect coding agent…"</button>
            <dialog class="gui-dialog" node_ref=dialog aria-labelledby="gui-title"
                on:cancel=move |event: web_sys::Event| { if busy.get_untracked() {event.prevent_default();} else {modal.set(false);} }>
                <div class="gui-dialog-heading"><div><p class="gui-eyebrow">"PROJECT CONNECTION"</p><h2 id="gui-title">"Connect your coding agent"</h2></div>
                    <button class="icon-button" aria-label="Close project connection" disabled=move ||busy.get() on:click=close>"×"</button></div>
                <p class="gui-description">"Make Proofstorm’s tools available to the agent and project you choose. Nothing is enabled globally."</p>
                <label class="gui-label" for="gui-agent">"Coding agent"</label>
                <select id="gui-agent" class="gui-path" prop:value=move ||harness.get() disabled=move ||busy.get()
                    on:change=move |event| {preview.set(None); result.set(None); harness.set(event_target_value(&event));}>
                    <option value="codex">"Codex · native app"</option>
                    <option value="opencode">"OpenCode · terminal"</option>
                    <option value="claude">"Claude Code · terminal"</option>
                </select>
                <label class="gui-label" for="gui-project">"Project folder"</label>
                <input id="gui-project" class="gui-path" type="text" autocomplete="off" spellcheck="false"
                    placeholder="/Users/you/code/my-app" prop:value=move ||project.get() disabled=move ||busy.get()
                    on:input=move |event|project.set(event_target_value(&event)) />
                <div class="gui-recents">{move ||context.get().map(|ctx|ctx["recent_projects"].as_array().cloned().unwrap_or_default()
                    .into_iter().filter_map(|p|p.as_str().map(str::to_owned)).map(|path| {
                        let selected=path.clone();
                        view! { <button class="gui-recent" disabled=move ||busy.get() on:click=move |_|project.set(selected.clone())>{path}</button> }
                    }).collect_view())}</div>
                <div class="gui-check" aria-live="polite">
                    <Show when=move ||checking.get()><p>"Checking folder and agent…"</p></Show>
                    {move ||preview.get().map(|plan|view! {
                        <p>"Tools will be enabled for:"</p><code>{plan["project"].as_str().unwrap_or_default().to_owned()}</code>
                        <p class="gui-muted">"Configuration: "{plan["config"].as_str().unwrap_or_default().to_owned()}". Existing settings are preserved."</p>
                        <p class="gui-muted">{plan["guidance"].as_str().unwrap_or_default().to_owned()}</p>
                        <Show when=move ||harness.get()!="codex"><p class="gui-muted">"Connect here, then run the provided command in your terminal. This button does not open a terminal or native agent window."</p></Show>
                    })}
                    {move ||error.get().map(|message|view!{<p class="gui-error" role="alert">{message}</p>})}
                    {move ||result.get().map(|value| {
                        let opened=value["app_opened"]==true;
                        let terminal=value["terminal_required"]==true;
                        view!{
                            <p class="gui-success">{if opened {"Configured · MCP server verified · Codex opened"} else if terminal {"Configured · MCP server verified · Ready to open in your terminal"} else {"Configured · MCP server verified · App could not open"}}</p>
                            {value["terminal_command"].as_str().map(|command|view!{<label class="gui-label" for="gui-command">"Run in your terminal"</label><textarea id="gui-command" class="gui-starter" readonly rows="3">{command.to_owned()}</textarea>})}
                            <p>{value["launch_error"].as_str().unwrap_or_default().to_owned()}</p>
                            <p class="gui-muted">{value["guidance"].as_str().unwrap_or_default().to_owned()}</p>
                            <label class="gui-label" for="gui-starter">"Try this in a new agent session"</label>
                            <textarea id="gui-starter" class="gui-starter" readonly rows="3">{value["starter_request"].as_str().unwrap_or_default().to_owned()}</textarea>
                        }
                    })}
                </div>
                <div class="gui-dialog-actions"><button class="gui-secondary" disabled=move ||busy.get() on:click=close>"Close"</button>
                    <button class="gui-primary" disabled=move ||busy.get() || checking.get() || preview.get().is_none()
                        || preview_input.get()!=project.get() || context.get().is_none_or(|ctx|ctx["runtime_ready"]!=true)
                        on:click=open_agent>{move ||if busy.get() {"Connecting…"} else if harness.get()=="codex" {"Open in Codex"} else {"Connect project"}}</button></div>
                {move ||context.get().filter(|ctx|ctx["runtime_ready"]!=true).map(|ctx|view!{<p class="gui-error">"Runtime not ready. Run proofstorm setup, then reopen the GUI. "{ctx["runtime_error"].as_str().unwrap_or_default().to_owned()}</p>})}
            </dialog>
        </Show>
    }
}
