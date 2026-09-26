use leptos::{prelude::*, task::spawn_local};
use proofstorm_view::{CatalogListRequest, CatalogPage};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[component]
pub fn CatalogHome(builds: RwSignal<bool>) -> impl IntoView {
    view! {
        <nav class="catalog-tabs" aria-label="Catalog pages"><button aria-pressed=move || !builds.get() on:click=move |_| builds.set(false)>"Images"</button><button aria-pressed=move || builds.get() on:click=move |_| builds.set(true)>"Builds"</button></nav>
        <Show when=move || !builds.get()><CatalogPanel /></Show>
        <Show when=move || builds.get()><BuildPanel /></Show>
    }
}

#[component]
pub fn CatalogPanel() -> impl IntoView {
    let expanded = RwSignal::new(BTreeSet::<String>::new());
    let search = RwSignal::new(String::new());
    let kind = RwSignal::new(String::new());
    let origin = RwSignal::new(String::new());
    let cursor = RwSignal::new(None::<String>);
    let page = RwSignal::new(None::<CatalogPage>);
    let error = RwSignal::new(None::<String>);
    let loading = RwSignal::new(false);
    let revision = RwSignal::new(0_u64);
    let refresh = RwSignal::new(0_u64);
    let interval = gloo_timers::callback::Interval::new(5000, move || {
        if cursor.get_untracked().is_none() {
            refresh.update(|n| *n = n.wrapping_add(1));
        }
    });
    let _interval = StoredValue::new_local(interval);
    Effect::new(move |_| {
        let search = search.get();
        let kind = kind.get();
        let origin = origin.get();
        let cursor = cursor.get();
        refresh.get();
        let mut query = json!({"query": search,"cursor":cursor,"limit":25});
        if !kind.is_empty() {
            query["kinds"] = json!([kind]);
        }
        if !origin.is_empty() {
            query["origins"] = json!([origin]);
        }
        let query: CatalogListRequest =
            serde_json::from_value(query).expect("fixed catalog selectors");
        let generation = revision.get_untracked().wrapping_add(1);
        revision.set(generation);
        loading.set(true);
        spawn_local(async move {
            let result = crate::client::catalog(&query).await;
            if revision.get_untracked() != generation {
                return;
            }
            loading.set(false);
            match result {
                Ok(value) => {
                    if page.with_untracked(|old| old.as_ref() != Some(&value)) {
                        page.set(Some(value));
                    }
                    error.set(None);
                }
                Err(message) => error.set(Some(message)),
            }
        });
    });
    view! {
        <section class="catalog-panel" aria-label="Component catalog">
            <h1>"Component catalog"</h1>
            <p>"Built-in images ship with Proofstorm. Candidates are experimental builds agents can create"</p>
            <div class="catalog-filters">
                <input class="search" aria-label="Search catalog" placeholder="Search components, versions or images…" prop:value=move || search.get() on:input=move |ev| { cursor.set(None); search.set(event_target_value(&ev)); } />
                <select aria-label="Component type" on:change=move |ev| { cursor.set(None); kind.set(event_target_value(&ev)); }>
                    <option value="">"All component types"</option><option value="mint">"Mints"</option><option value="wallet">"Wallets"</option><option value="bitcoin">"Bitcoin"</option><option value="lightning">"Lightning"</option><option value="payment_processor">"Payment processors"</option><option value="database">"Databases"</option><option value="identity_provider">"Identity providers"</option><option value="workspace">"Workspaces"</option><option value="proxy">"Proxies"</option><option value="oracle">"Oracles"</option>
                </select>
                <select aria-label="Image origin" on:change=move |ev| { cursor.set(None); origin.set(event_target_value(&ev)); }>
                    <option value="">"All origins"</option><option value="built_in">"Built-in"</option><option value="candidate">"Candidate"</option>
                </select>
                <button on:click=move |_| { cursor.set(None); refresh.update(|n| *n = n.wrapping_add(1)); }>"Refresh"</button>
            </div>
            <Show when=move || loading.get() && page.with(Option::is_none)><p role="status">"Loading catalog…"</p></Show>
            {move || error.get().map(|message| view! { <p class="notice warning" role="alert">{message}<button on:click=move |_| { cursor.set(None); refresh.update(|n| *n = n.wrapping_add(1)); }>"Retry search"</button></p> })}
            {move || page.get().map(|result| {
                let count = result.matched_count;
                let next = result.next_cursor;
                view! {
                    <p role="status">{format!("{count} matching catalog {}", if count == 1 { "entry" } else { "entries" })}</p>
                    <div class="catalog-table-scroll">
                        <table class="catalog-table" aria-label="Catalog images">
                            <thead><tr><th scope="col">"Image"</th><th scope="col">"Version"</th><th scope="col">"Type"</th><th scope="col">"Origin"</th><th scope="col">"Platform"</th></tr></thead>
                            {result.items.into_iter().map(|entry| view! { <CatalogRow entry expanded /> }).collect_view()}
                        </table>
                    </div>
                    <Show when=move || count == 0><p>"No images match these filters."</p></Show>
                    {next.map(|next| view! { <button disabled=move || loading.get() on:click=move |_| cursor.set(Some(next.clone()))>"Next page"</button> })}
                    <Show when=move || cursor.get().is_some()><button on:click=move |_| cursor.set(None)>"First page"</button></Show>
                }
            })}
        </section>
    }
}

#[component]
fn CatalogRow(entry: Value, expanded: RwSignal<BTreeSet<String>>) -> impl IntoView {
    let field = |key: &str| entry[key].as_str().unwrap_or("Unknown").to_owned();
    let id = field("id");
    let version = field("version");
    let kind = match entry["kind"].as_str().unwrap_or_default() {
        "bitcoin" => "Bitcoin",
        "lightning" => "Lightning",
        "payment_processor" => "Payment processor",
        "mint" => "Mint",
        "wallet" => "Wallet",
        "database" => "Database",
        "identity_provider" => "Identity provider",
        "workspace" => "Workspace",
        "proxy" => "Proxy",
        "oracle" => "Oracle",
        _ => "Unknown",
    };
    let lifecycle = field("support_lifecycle");
    let image = field("image");
    let platform = field("platform");
    let description = field("description");
    let candidate = entry["origin"] == "candidate";
    let shared = entry["shared_image_implementations"]
        .as_array()
        .filter(|values| values.len() > 1)
        .map(|values| {
            format!(
                "Shared image · presets: {}",
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
    let candidate_id = entry["candidate_id"].as_str().map(str::to_owned);
    let selector = json!({"implementation":id,"version":version}).to_string();
    let key = StoredValue::new(json!([id, version, image, candidate_id]).to_string());
    let is_open = move || expanded.with(|rows| rows.contains(&key.get_value()));
    let toggle = move || {
        expanded.update(|rows| {
            let key = key.get_value();
            if !rows.remove(&key) {
                rows.insert(key);
            }
        })
    };
    view! {
        <tbody class="catalog-entry">
            <tr class="catalog-row" class:expanded=is_open on:click=move |_| toggle()>
                <td><button class="catalog-row-toggle" aria-expanded=move || is_open().to_string() on:click=move |ev| { ev.stop_propagation(); toggle(); }>
                    <span class="catalog-chevron" aria-hidden="true">"›"</span><strong>{id}</strong>
                </button></td>
                <td class="catalog-version">{version}</td><td>{kind}</td>
                <td><span class=if candidate { "catalog-origin candidate" } else { "catalog-origin" }>{if candidate { "Candidate" } else { "Built-in" }}</span></td>
                <td class="catalog-platform">{platform}</td>
            </tr>
            <Show when=is_open>
                <tr class="catalog-detail-row"><td colspan="5">
                    <div class="catalog-row-details">
                        <p>{description.clone()}</p>
                        <p class="catalog-lifecycle">{format!("Support: {lifecycle}")}</p>
                        {shared.clone().map(|text| view! { <p>{text}</p> })}
                        <div class="catalog-detail-fields">
                            <div><h3>"Image reference"</h3><pre>{image.clone()}</pre><CopyText text=image.clone() label="Copy image" /></div>
                            <div><h3>"Component selector"</h3><pre>{selector.clone()}</pre><CopyText text=selector.clone() label="Copy selector" /></div>
                        </div>
                        {candidate_id.clone().map(|id| view! { <CandidateDetails id /> })}
                    </div>
                </td></tr>
            </Show>
        </tbody>
    }
}

#[component]
fn CopyText(text: String, label: &'static str) -> impl IntoView {
    let text = StoredValue::new(text);
    let status = RwSignal::new(String::new());
    view! {
        <button on:click=move |_| spawn_local(async move {
            let Some(window) = web_sys::window() else { return; };
            let result = wasm_bindgen_futures::JsFuture::from(window.navigator().clipboard().write_text(&text.get_value())).await;
            status.set(if result.is_ok() { "Copied" } else { "Copy unavailable; select the text above" }.into());
        })>{label}</button><span role="status">{move || status.get()}</span>
    }
}

#[component]
fn BuildPanel() -> impl IntoView {
    let search = RwSignal::new(String::new());
    let phase = RwSignal::new(String::new());
    let cursor = RwSignal::new(None::<String>);
    let page = RwSignal::new(None::<Value>);
    let error = RwSignal::new(None::<String>);
    let loading = RwSignal::new(false);
    let generation = RwSignal::new(0_u64);
    let refresh = RwSignal::new(0_u64);
    Effect::new(move |_| {
        let phase = phase.get();
        let query = proofstorm_view::DirectoryQuery {
            query: search.get(),
            case_insensitive: true,
            phase: (!phase.is_empty()).then_some(phase),
            cursor: cursor.get(),
            ..Default::default()
        };
        refresh.get();
        let revision = generation.get_untracked().wrapping_add(1);
        generation.set(revision);
        loading.set(true);
        spawn_local(async move {
            let result = crate::client::builds(&query).await;
            if generation.get_untracked() != revision {
                return;
            }
            loading.set(false);
            match result {
                Ok(value) => {
                    if page.with_untracked(|old| old.as_ref() != Some(&value)) {
                        page.set(Some(value));
                    }
                    error.set(None);
                }
                Err(message) => error.set(Some(message)),
            }
        });
    });
    let interval = gloo_timers::callback::Interval::new(5000, move || {
        if cursor.get_untracked().is_none() {
            refresh.update(|n| *n = n.wrapping_add(1));
        }
    });
    let _interval = StoredValue::new_local(interval);
    view! {
        <section class="catalog-panel" aria-label="Candidate builds"><h1>"Candidate builds"</h1><p>"Source builds and their recorded results. Only successful builds appear in Images. Runtime readiness and experiment results are separate evidence."</p>
            <div class="catalog-filters"><input class="search" aria-label="Search builds" placeholder="Search candidate, component or source…" on:input=move |ev| { cursor.set(None); search.set(event_target_value(&ev)); } />
                <select aria-label="Build phase" on:change=move |ev| { cursor.set(None); phase.set(event_target_value(&ev)); }><option value="">"All phases"</option><option value="pending">"Pending"</option><option value="resolving">"Resolving"</option><option value="building">"Building"</option><option value="pushing">"Pushing"</option><option value="succeeded">"Succeeded"</option><option value="failed">"Failed"</option><option value="cancelled">"Cancelled"</option></select>
                <button on:click=move |_| { cursor.set(None); refresh.update(|n| *n = n.wrapping_add(1)); }>"Refresh"</button>
            </div>
            <Show when=move || loading.get() && page.with(Option::is_none)><p role="status">"Loading builds…"</p></Show>
            {move || error.get().map(|message| view! { <p class="notice warning" role="alert">{message}<button on:click=move |_| { cursor.set(None); refresh.update(|n| *n = n.wrapping_add(1)); }>"Restart search"</button></p> })}
            {move || page.get().map(|page| {
                let items = page["items"].as_array().cloned().unwrap_or_default();
                let next = page["next_cursor"].as_str().map(str::to_owned);
                let empty = items.is_empty();
                view! {
                    <div class="catalog-grid">{items.into_iter().map(|record| {
                        let id = record["id"].as_str().unwrap_or_default().to_owned();
                        let title = id.clone();
                        let status = record["phase"].as_str().unwrap_or("Unknown").to_owned();
                        let implementation = record["implementation"].as_str().unwrap_or_default().to_owned();
                        let sha = record["commit_sha"].as_str().unwrap_or_default().to_owned();
                        let message = record["message"].as_str().unwrap_or_default().to_owned();
                        view! { <article class="catalog-card"><span class="catalog-origin candidate">{status}</span><h2>{title}</h2><p>{implementation}</p><p class="catalog-image">{sha}</p><p>{message}</p><CandidateDetails id /></article> }
                    }).collect_view()}</div>
                    <Show when=move || empty><p>"No matching builds in this page. Follow the next page if available."</p></Show>
                    {next.map(|next| view! { <button on:click=move |_| cursor.set(Some(next.clone()))>"Next page"</button> })}
                    <Show when=move || cursor.get().is_some()><button on:click=move |_| cursor.set(None)>"First page"</button></Show>
                }
            })}
        </section>
    }
}

#[component]
fn CandidateDetails(id: String) -> impl IntoView {
    let id = StoredValue::new(id);
    let open = RwSignal::new(false);
    let path = RwSignal::new("/provenance".to_owned());
    let offset = RwSignal::new(0_usize);
    let digest = RwSignal::new(None::<String>);
    let page = RwSignal::new(None::<Value>);
    let error = RwSignal::new(None::<String>);
    let generation = RwSignal::new(0_u64);
    let refresh = RwSignal::new(0_u64);
    Effect::new(move |_| {
        if !open.get() {
            return;
        }
        let path = path.get();
        let offset = offset.get();
        refresh.get();
        let expected_digest = digest.get_untracked();
        let revision = generation.get_untracked().wrapping_add(1);
        generation.set(revision);
        spawn_local(async move {
            let result =
                crate::client::candidate(&id.get_value(), &path, offset, expected_digest).await;
            if generation.get_untracked() != revision {
                return;
            }
            match result {
                Ok(value) => {
                    digest.set(value["digest"].as_str().map(str::to_owned));
                    page.set(Some(value));
                    error.set(None);
                }
                Err(message) => error.set(Some(message)),
            }
        });
    });
    view! {
        <button aria-expanded=move || open.get() on:click=move |_| open.update(|v| *v = !*v)>"Build details"</button>
        <Show when=move || open.get()><select aria-label="Build evidence" on:change=move |ev| { digest.set(None); offset.set(0); page.set(None); path.set(event_target_value(&ev)); }><option value="/provenance">"Source and recipe"</option><option value="/diagnostics">"Retained build diagnostics"</option><option value="">"Complete build record"</option></select>
            <button on:click=move |_| { digest.set(None); offset.set(0); refresh.update(|n| *n = n.wrapping_add(1)); }>"Refresh evidence"</button>
            {move || error.get().map(|message| view! { <p class="notice warning" role="alert">{message}</p> })}
            {move || page.get().map(|value| {
                let text = value["text"].as_str().unwrap_or_default().to_owned();
                let next = value["next_offset"].as_u64();
                view! { <pre>{text}</pre>{next.map(|next| view! { <button on:click=move |_| offset.set(usize::try_from(next).unwrap_or(0))>"Continue reading"</button> })} }
            })}
        </Show>
    }
}
