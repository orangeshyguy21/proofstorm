use crate::{
    client,
    lab_view::LabPanel,
    model::{lab_name, lab_phase},
    system::{SystemPanel, SystemSummary},
    theme::ThemePicker,
};
use leptos::{prelude::*, task::spawn_local};
use proofstorm_view::{EnvironmentLab, EnvironmentView, ObserverStatus};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::{JsCast, closure::Closure};

#[component]
#[allow(
    clippy::too_many_lines,
    reason = "top-level view owns its subscription and refresh lifecycle"
)]
pub fn App() -> impl IntoView {
    crate::freshness::provide_clock();
    let system_open = RwSignal::new(false);
    let navigation = RwSignal::new(
        web_sys::window()
            .and_then(|w| w.inner_width().ok())
            .and_then(|v| v.as_f64())
            .is_none_or(|width| width > 760.0),
    );
    let drawer = RwSignal::new("");
    let telemetry = RwSignal::new(None::<proofstorm_view::SystemView>);
    let telemetry_error = RwSignal::new(false);
    let telemetry_refresh = RwSignal::new(0_u64);
    let zoom = RwSignal::new(1.0_f64);
    let pan = RwSignal::new((0.0_f64, 0.0_f64));
    let environment = RwSignal::new(None::<EnvironmentView>);
    let selected = RwSignal::new(String::new());
    let detail = RwSignal::new(None::<EnvironmentLab>);
    let component = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let connected = RwSignal::new(false);
    let loaded = RwSignal::new(false);
    let observer = RwSignal::new(None::<ObserverStatus>);
    let history_pages = RwSignal::new(1_usize);
    let refresh = RwSignal::new(0_u64);
    let search = RwSignal::new(String::new());
    let busy = Rc::new(Cell::new(false));
    let dirty = Rc::new(Cell::new(false));
    let refresher = move || refresh.update(|n| *n = n.wrapping_add(1));

    let previous_lab = StoredValue::new(String::new());
    Effect::new(move |_| {
        let id = selected.get();
        if previous_lab.get_value() != id {
            previous_lab.set_value(id);
            zoom.set(1.0);
            pan.set((0.0, 0.0));
            history_pages.set(1);
        }
    });

    // Coalesce invalidations while fetching. Selection changes also request a snapshot.
    Effect::new(move |_| {
        refresh.get();
        selected.get();
        history_pages.get();
        dirty.set(true);
        if busy.replace(true) {
            return;
        }
        let busy = busy.clone();
        let dirty = dirty.clone();
        spawn_local(async move {
            while dirty.replace(false) {
                let result = client::environment().await;
                match result {
                    Ok(view) => {
                        let mut id = selected.get_untracked();
                        // Compare successful inventories so normal refreshes and reconnects
                        // preserve selection, while a newly observed lab opens its canvas.
                        let new_lab = environment.with_untracked(|previous| {
                            previous.as_ref().and_then(|previous| {
                                view.labs
                                    .items
                                    .iter()
                                    .find(|lab| {
                                        !previous.labs.items.iter().any(|old| old.id == lab.id)
                                    })
                                    .map(|lab| lab.id.clone())
                            })
                        });
                        if let Some(new_lab) = new_lab {
                            id = new_lab;
                            system_open.set(false);
                            search.set(String::new());
                        } else if !view.labs.items.iter().any(|lab| lab.id == id) {
                            id = view
                                .labs
                                .items
                                .first()
                                .map(|lab| lab.id.clone())
                                .unwrap_or_default();
                        }
                        if selected.get_untracked() != id {
                            selected.set(id.clone());
                            detail.set(None);
                            component.set(String::new());
                            zoom.set(1.0);
                            pan.set((0.0, 0.0));
                            history_pages.set(1);
                        }
                        environment.set(Some(view));
                        if id.is_empty() {
                            detail.set(None);
                            error.set(None);
                        } else {
                            match client::lab(&id, history_pages.get_untracked()).await {
                                Ok(lab) if selected.get_untracked() == id => {
                                    if crate::canvas_model::selected_owner(
                                        &lab,
                                        &component.get_untracked(),
                                    )
                                    .is_none()
                                    {
                                        component.set(String::new());
                                    }
                                    detail.set(Some(lab));
                                    error.set(None);
                                }
                                Err(message) if selected.get_untracked() == id => {
                                    error.set(Some(message));
                                }
                                _ => {}
                            }
                        }
                        match client::observer().await {
                            Ok(status) => observer.set(Some(status)),
                            Err(_) => observer.set(None),
                        }
                    }
                    Err(message) => error.set(Some(message)),
                }
                loaded.set(true);
            }
            busy.set(false);
        });
    });
    let telemetry_busy = Rc::new(Cell::new(false));
    let telemetry_dirty = Rc::new(Cell::new(false));
    Effect::new(move |_| {
        telemetry_refresh.get();
        telemetry_dirty.set(true);
        if telemetry_busy.replace(true) {
            return;
        }
        let busy = telemetry_busy.clone();
        let dirty = telemetry_dirty.clone();
        spawn_local(async move {
            while dirty.replace(false) {
                match client::system().await {
                    Ok(view) => {
                        telemetry.set(Some(view));
                        telemetry_error.set(false);
                    }
                    Err(_) => telemetry_error.set(true),
                }
            }
            busy.set(false);
        });
    });
    // Native EventSource reconnects automatically. Every connection gets an invalidation.
    match web_sys::EventSource::new("/v1/events") {
        Ok(source) => {
            let on_event = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| {
                connected.set(true);
                refresher();
                telemetry_refresh.update(|n| *n = n.wrapping_add(1));
            });
            let on_telemetry = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| {
                connected.set(true);
                telemetry_refresh.update(|n| *n = n.wrapping_add(1));
            });
            let _ = source.add_event_listener_with_callback(
                "telemetry",
                on_telemetry.as_ref().unchecked_ref(),
            );
            let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| connected.set(false));
            let _ = source
                .add_event_listener_with_callback("environment", on_event.as_ref().unchecked_ref());
            source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
            let connection = StoredValue::new_local((source, on_event, on_error, on_telemetry));
            on_cleanup(move || connection.with_value(|(source, _, _, _)| source.close()));
        }
        Err(_) => error.set(Some(
            "This browser could not open the live event stream.".into(),
        )),
    }
    // Retry failed snapshots even when an otherwise healthy stream is quiet.
    let retry = gloo_timers::callback::Interval::new(5000, move || {
        if telemetry_error.get_untracked() {
            telemetry_refresh.update(|n| *n = n.wrapping_add(1));
        }
        if error.get_untracked().is_some() {
            refresher();
        }
    });
    let _retry = StoredValue::new_local(retry);
    view! {
        <header class="app-header">
            <button class="icon-button" aria-label="Toggle lab navigation" aria-expanded=move || navigation.get() on:click=move |_| navigation.update(|open| *open = !*open)>"☰"</button>
            <a class="brand" href="/" aria-label="Proofstorm home"><span class="brand-mark">"✳"</span>"proofstorm"</a>
            <span class="header-context">{move || environment.get().map(|v| v.workspace_id)}</span>
            <div class="header-right"><span class=move || if connected.get() && error.get().is_none() { "live-state" } else { "live-state offline" }><i></i>{move || if error.get().is_some() { "Update failed" } else if connected.get() { "Live" } else { "Reconnecting" }}</span><ThemePicker /></div>
        </header>
        <div class=move || if navigation.get() { "workspace-shell" } else { "workspace-shell nav-collapsed" }>
            <aside class="sidebar" aria-label="Workspace navigation">
                <SystemSummary telemetry open=system_open />
                <div class="section-label"><span>"Labs"</span><span>{move || environment.get().map_or(0, |v| v.labs.items.len())}</span></div>
                <input class="search" aria-label="Find a lab" placeholder="Find a lab…" prop:value=move || search.get() on:input=move |ev| search.set(event_target_value(&ev)) />
                <nav class="lab-list" aria-label="Labs">{move || {
                    let query = search.get().to_lowercase();
                    environment.get().map(|v| v.labs.items.into_iter().filter(|lab| lab_name(lab).to_lowercase().contains(&query)).map(|lab| {
                        let id = lab.id.clone(); let active_id = id.clone();
                        let name = lab_name(&lab); let status = lab_phase(&lab);
                        view! { <button class=move || if !system_open.get() && selected.get() == active_id { "lab-item selected" } else { "lab-item" } on:click=move |_| {
                            system_open.set(false);
                            if selected.get_untracked() != id { selected.set(id.clone()); detail.set(None); zoom.set(1.0); pan.set((0.0,0.0)); component.set(String::new()); history_pages.set(1); }
                        }><span class="lab-icon">"⬡"</span><span><strong>{name}</strong><small>{status}</small></span></button> }
                    }).collect_view())
                }}</nav>
            </aside>
            <main class=move || if !connected.get() || telemetry_error.get() { "measurements-stale" } else { "" }>
                <div class="notifications">
                    <Show when=move || !connected.get() && loaded.get()><div class="notice warning" role="status">"Connection lost. Values may be stale. Reconnecting…"</div></Show>
                    {move || error.get().map(|message| view! { <div class="notice warning" role="status">{message}<button on:click=move |_| refresher()>"Retry"</button></div> })}
                    {move || observer.get().and_then(|o| o.error).map(|message| view! { <div class="notice warning">{message}</div> })}
                    <Show when=move || telemetry_error.get()><div class="notice warning">"Measurements could not refresh. Showing last observed values."</div></Show>
                </div>
                <Show when=move || system_open.get()><SystemPanel telemetry selected_lab=selected selected_component=component open=system_open /></Show>
                <Show when=move || !system_open.get()>
                    <LabPanel lab=detail selected_component=component history_pages zoom pan telemetry drawer />
                    <Show when=move || detail.get().is_none()><div class="empty-state"><span class="empty-mark">"✳"</span><h1>{move || if loaded.get() && selected.get().is_empty() { "No labs" } else { "Loading lab…" }}</h1><Show when=move || loaded.get() && selected.get().is_empty()><code>"proofstorm up examples/developer-lab.json --name demo"</code></Show></div></Show>
                </Show>
            </main>
        </div>
    }
}
