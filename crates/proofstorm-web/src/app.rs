use crate::{
    client,
    model::{archived, closed, health, lab_name, lab_phase, label, position},
};
use leptos::{prelude::*, task::spawn_local};
use proofstorm_view::{ComponentView, EnvironmentLab, EnvironmentView, ObserverStatus};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::{JsCast, closure::Closure};

#[component]
#[allow(
    clippy::too_many_lines,
    reason = "top-level view owns its subscription and refresh lifecycle"
)]
pub fn App() -> impl IntoView {
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
    let show_archived = RwSignal::new(false);
    let busy = Rc::new(Cell::new(false));
    let dirty = Rc::new(Cell::new(false));
    let refresher = move || refresh.update(|n| *n = n.wrapping_add(1));

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
                        if !view.labs.items.iter().any(|lab| lab.id == id) {
                            id = view
                                .labs
                                .items
                                .first()
                                .map(|lab| lab.id.clone())
                                .unwrap_or_default();
                            selected.set(id.clone());
                            component.set(String::new());
                        }
                        environment.set(Some(view));
                        if id.is_empty() {
                            detail.set(None);
                            error.set(None);
                        } else {
                            match client::lab(&id, history_pages.get_untracked()).await {
                                Ok(lab) if selected.get_untracked() == id => {
                                    detail.set(Some(lab));
                                    error.set(None);
                                }
                                Err(message) => error.set(Some(message)),
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
    // Native EventSource reconnects automatically. Every connection gets an invalidation.
    match web_sys::EventSource::new("/v1/events") {
        Ok(source) => {
            let on_event = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| {
                connected.set(true);
                refresher();
            });
            let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| connected.set(false));
            let _ = source
                .add_event_listener_with_callback("environment", on_event.as_ref().unchecked_ref());
            source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
            let connection = StoredValue::new_local((source, on_event, on_error));
            on_cleanup(move || connection.with_value(|(source, _, _)| source.close()));
        }
        Err(_) => error.set(Some(
            "This browser could not open the live event stream.".into(),
        )),
    }
    // Retry failed snapshots even when an otherwise healthy stream is quiet.
    let retry = gloo_timers::callback::Interval::new(5000, move || {
        if error.get_untracked().is_some() {
            refresher();
        }
    });
    let _retry = StoredValue::new_local(retry);
    view! {
        <header class="app-header">
            <a class="brand" href="/" aria-label="Proofstorm home"><span class="brand-mark">"✳"</span>"proofstorm"</a>
            <span class="header-divider"></span><span class="header-context">"Environment"</span>
            <div class="header-right"><span class=move || if connected.get() && error.get().is_none() { "live-state" } else { "live-state offline" }><i></i>{move || if error.get().is_some() { "Snapshot stale" } else if connected.get() { "Live · SSE" } else { "Reconnecting" }}</span><span class="read-only">"READ ONLY"</span></div>
        </header>
        <div class="workspace-shell">
            <aside class="sidebar">
                <div class="sidebar-heading"><span class="eyebrow">"WORKSPACE"</span><strong>{move || environment.get().map_or_else(|| "Connecting…".into(), |v| v.workspace_id)}</strong></div>
                <div class="section-label"><span>"LABS"</span><span>{move || environment.get().map_or(0, |v| v.labs.items.len())}</span></div>
                <input class="search" aria-label="Find a lab" placeholder="Find a lab…" on:input=move |ev| search.set(event_target_value(&ev)) />
                <label class="archive-toggle"><input type="checkbox" on:change=move |ev| show_archived.set(event_target_checked(&ev)) />"Include closed / missing"</label>
                <nav class="lab-list" aria-label="Labs">{move || {
                    let query = search.get().to_lowercase();
                    environment.get().map(|v| v.labs.items.into_iter().filter(|lab| (show_archived.get() || !archived(lab) || selected.get() == lab.id) && lab_name(lab).to_lowercase().contains(&query)).map(|lab| {
                        let id = lab.id.clone(); let active_id = id.clone();
                        let name = lab_name(&lab); let status = lab_phase(&lab);
                        view! { <button class=move || if selected.get() == active_id { "lab-item selected" } else { "lab-item" } on:click=move |_| { selected.set(id.clone()); detail.set(None); component.set(String::new()); history_pages.set(1); }><span class="lab-icon">"⬡"</span><span><strong>{name}</strong><small>{status}</small></span><span class="chevron">"›"</span></button> }
                    }).collect_view())
                }}</nav>
                <div class="sidebar-footer"><span class="tiny-dot"></span>"Local environment"<p>"Agents build. You observe."</p></div>
            </aside>
            <main>
                <Show when=move || !connected.get() && loaded.get()><div class="notice warning">"Live connection interrupted. Showing the last snapshot while the stream reconnects."</div></Show>
                {move || error.get().map(|message| view! { <div class="notice warning" role="status">{message}<button on:click=move |_| refresher()>"Retry"</button></div> })}
                {move || observer.get().filter(|o| o.error.is_some()).map(|o| view! { <div class="notice warning">{o.error.unwrap_or_default()}</div> })}
                {move || detail.get().map(|lab| view! { <LabPanel lab selected_component=component history_pages zoom pan /> })}
                <Show when=move || detail.get().is_none()>
                    <div class="empty-state"><span class="empty-mark">"✳"</span><span class="eyebrow">"YOUR LAB, IN VIEW"</span><h1>{move || if loaded.get() && selected.get().is_empty() { "Waiting for your first lab" } else { "Connecting to your environment" }}</h1><p>"Ask an agent to build a lab. Components, connections and activity appear here as it takes shape."</p><code>"proofstorm up examples/developer-lab.json --name demo"</code><small>"Use the same database and workspace as your agent."</small></div>
                </Show>
                <footer class="main-footer"><span>"Topology & runtime status · Desired resources"</span><span>"Protocol traffic and resource usage are not collected"</span></footer>
            </main>
        </div>
    }
}

#[component]
fn LabPanel(
    lab: EnvironmentLab,
    selected_component: RwSignal<String>,
    history_pages: RwSignal<usize>,
    zoom: RwSignal<f64>,
    pan: RwSignal<(f64, f64)>,
) -> impl IntoView {
    let name = lab_name(&lab);
    let state = label(&lab.runtime.state);
    let phase = lab_phase(&lab);
    let is_closed = closed(&lab);
    let warning_state = state.clone();
    let observation_state = if is_closed {
        "not running".into()
    } else {
        state.clone()
    };
    let ready = lab
        .components
        .items
        .iter()
        .filter(|c| c.ready == Some(true))
        .count();
    let count = lab.components.items.len();
    let links = lab.links.items.len();
    let lab_for_detail = lab.clone();
    let graph_lab = lab.clone();
    view! {
        <div class="page-heading"><div><div class="breadcrumb">"ENVIRONMENT / LAB"</div><h1>{name}<span class="phase-badge">{phase}</span></h1><p class="instance-id">{lab.id.clone()}</p></div><div class="heading-note">"Updated "{time(lab.runtime.fetched_at_unix)}<small>"from your local cluster"</small></div></div>
        <Show when=move || state != "available" && !is_closed><div class="notice warning">"Runtime observation: "{warning_state.clone()}". Component readiness is unknown until a current observation is available."</div></Show>
        <div class="metrics"><div><span>"COMPONENTS"</span><strong>{count}</strong><small>{format!("{ready} ready")}</small></div><div><span>"CONNECTIONS"</span><strong>{links}</strong><small>"declared topology"</small></div><div><span>"SESSIONS"</span><strong>{format!("{}{}",lab.sessions.items.len(),if lab.sessions.next_cursor.is_some(){"+"}else{""})}</strong><small>"activity tracking"</small></div><div><span>"OBSERVATION"</span><strong class="metric-text">{observation_state}</strong><small>"cluster status"</small></div></div>
        <section class="topology-section"><div class="panel-title"><h2>"Lab topology"</h2><span>"Select a component to inspect"</span><div class="legend"><i class="ready"></i>"Ready"<i class="pending"></i>"Pending"<i class="unknown"></i>"Unknown"</div></div>
            <div class="topology-body"><Graph lab=graph_lab selected=selected_component zoom pan /><aside class="inspector">{move || {
                lab_for_detail.components.items.iter().find(|c| c.id == selected_component.get()).cloned().map_or_else(|| view! { <div class="inspector-empty"><span>"⌖"</span><h3>"Explore your lab"</h3><p>"Choose a node to see its endpoints, health and resource requests."</p><small>"Connections describe configuration. They do not represent observed traffic."</small></div> }.into_any(), |c| view! { <ComponentPanel component=c lab=lab_for_detail.clone() /> }.into_any())
            }}</aside></div>
        </section>
        <div class="history-grid"><section class="history-panel"><div class="panel-title"><h2>"Activity"</h2><span>"Latest first"</span></div><div class="activity-list">
            {lab.activity.items.iter().map(|a| { let phase = label(&a.phase); view! { <div class="activity-row"><span class=format!("activity-dot {phase}")></span><div><strong>{label(&a.kind)}</strong><small>{format!("{} · {}", a.principal_id, a.components.join(", "))}</small><code>{a.id.clone()}</code></div><div class="activity-outcome"><span>{phase}</span><small>{a.native_exit_code.map_or_else(|| time(a.accepted_at_unix), |code| format!("exit {code}"))}</small></div></div> } }).collect_view()}
            {lab.activity.items.is_empty().then(|| view! { <p class="quiet-empty">"Agent actions will appear here. Receipts are collected automatically."</p> })}
        </div></section><section class="history-panel"><div class="panel-title"><h2>"Sessions"</h2><span>"Informational"</span></div><div class="session-list">{lab.sessions.items.iter().map(|s| view! { <div class="session-row"><span class="avatar">"A"</span><div><strong>{s.session.principal_id.clone()}</strong><small>{format!("{} · {} overlaps",label(&s.session.phase),s.overlapping_session_count)}</small><code>{s.session.id.clone()}</code></div></div> }).collect_view()}{lab.sessions.items.is_empty().then(|| view! { <p class="quiet-empty">"No sessions recorded for this lab."</p> })}</div><p class="session-note">"Sessions track activity. They never lock or reserve the lab."</p></section></div>
        {(lab.activity.next_cursor.is_some() || lab.sessions.next_cursor.is_some()).then(|| view! { <button class="load-more" on:click=move |_| history_pages.update(|pages| *pages += 1)>"Load more history"</button> })}
    }
}

#[component]
fn Graph(
    lab: EnvironmentLab,
    selected: RwSignal<String>,
    zoom: RwSignal<f64>,
    pan: RwSignal<(f64, f64)>,
) -> impl IntoView {
    let drag = RwSignal::new(None::<(i32, i32)>);
    let height = lab
        .components
        .items
        .iter()
        .map(|c| position(&lab.components.items, &c.id).1 + 140)
        .max()
        .unwrap_or(320)
        .max(350);
    let width = lab
        .components
        .items
        .iter()
        .map(|c| position(&lab.components.items, &c.id).0 + 260)
        .max()
        .unwrap_or(1100)
        .max(800);
    let nodes = lab.components.items.clone();
    view! {
        <div class="graph"><svg viewBox=format!("0 0 {width} {height}") aria-label="Lab component topology" role="group"
            on:pointerdown=move |ev| { if ev.button() == 0 { drag.set(Some((ev.client_x(),ev.client_y()))); } }
            on:pointerup=move |_| drag.set(None) on:pointerleave=move |_| drag.set(None)
            on:pointermove=move |ev| { if let Some((x,y))=drag.get_untracked() { let scale = ev.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()).map_or(1.0, |e| f64::from(width)/f64::from(e.client_width().max(1))); pan.update(|p| { p.0+=f64::from(ev.client_x()-x)*scale; p.1+=f64::from(ev.client_y()-y)*scale; }); drag.set(Some((ev.client_x(),ev.client_y()))); } }>
            <defs><pattern id="dots" width="20" height="20" patternUnits="userSpaceOnUse"><circle cx="1" cy="1" r="1" fill="#dce0d9" /></pattern><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="#9aa59b" /></marker></defs>
            <rect width="100%" height="100%" fill="url(#dots)" />
            <g transform=move || format!("translate({} {}) scale({})",pan.get().0,pan.get().1,zoom.get())>
                {lab.links.items.iter().filter(|l| nodes.iter().any(|c| c.id==l.from) && nodes.iter().any(|c| c.id==l.to)).map(|link| { let (x1,y1)=position(&nodes,&link.from); let (x2,y2)=position(&nodes,&link.to); let (start,end) = match x2.cmp(&x1) { std::cmp::Ordering::Greater => ((x1+205,y1+42),(x2,y2+42)), std::cmp::Ordering::Less => ((x1,y1+42),(x2+205,y2+42)), std::cmp::Ordering::Equal => ((x1+102,y1+84),(x2+102,y2)) }; let mid=i32::midpoint(start.0,end.0); view! { <g><path class="connection" d=format!("M {} {} C {mid} {}, {mid} {}, {} {}",start.0,start.1,start.1,end.1,end.0,end.1) marker-end="url(#arrow)"/><title>{format!("{} → {} · {}", link.from,link.to,label(&link.kind))}</title></g> } }).collect_view()}
                {lab.components.items.into_iter().map(|c| { let (x,y)=position(&nodes,&c.id); let id=c.id.clone();let active=id.clone();let key_id=id.clone();let status=health(&c);view! { <g class=move || format!("graph-node {} {}",status,if selected.get()==active{"active"}else{""}) transform=format!("translate({x} {y})") tabindex="0" role="button" aria-label=format!("Inspect {}",c.id) on:pointerdown=move |ev| ev.stop_propagation() on:click=move |_| selected.set(id.clone()) on:keydown=move |ev| {if matches!(ev.key().as_str(),"Enter"|" ") {ev.prevent_default();selected.set(key_id.clone());}}><rect width="205" height="84" rx="10"/><circle cx="184" cy="19" r="4"/><text class="node-kind" x="15" y="23">{label(&c.kind).to_uppercase()}</text><text class="node-name" x="15" y="46">{short(&c.id,24)}</text><text class="node-impl" x="15" y="65">{short(&c.implementation,28)}</text><title>{format!("{} · {} · {status}",c.id,c.implementation)}</title></g> } }).collect_view()}
            </g>
        </svg><div class="graph-toolbar"><button aria-label="Zoom out" on:click=move |_| zoom.update(|z| *z=(*z/1.2).max(0.3))>"−"</button><span>{move || format!("{:.0}%",zoom.get()*100.0)}</span><button aria-label="Zoom in" on:click=move |_| zoom.update(|z| *z=(*z*1.2).min(3.0))>"+"</button><button on:click=move |_| { zoom.set(1.0);pan.set((0.0,0.0)); }>"Reset"</button></div><span class="graph-caption">"Drag to pan · Select to inspect"</span></div>
    }
}
#[component]
fn ComponentPanel(component: ComponentView, lab: EnvironmentLab) -> impl IntoView {
    let id = component.id.clone();
    let connection = component
        .endpoints
        .iter()
        .find(|e| e.local_connection_supported)
        .map(|e| {
            format!(
                "proofstorm connect {} {} {} --config connection.json",
                lab.handle
                    .as_ref()
                    .map_or(lab.id.as_str(), |h| h.name.as_str()),
                component.id,
                e.name
            )
        });
    view! { <div class="component-panel"><span class="eyebrow">{label(&component.kind)}</span><h3>{component.id.clone()}</h3><p>{component.implementation.clone()}" · "{health(&component)}</p>
        <h4>"Connections"</h4>{component.endpoints.is_empty().then(|| view!{<p>"No service endpoints."</p>})}
        {component.endpoints.into_iter().map(|e| view!{<div class="endpoint"><strong>{e.name}</strong><code>{format!("{}:{}",e.cluster_host,e.port)}</code><small>{format!("{} · {}",e.transport,if e.local_connection_supported {"local connection available"} else {"cluster access"})}</small></div>}).collect_view()}
        <h4>"Conditions"</h4>{component.conditions.is_empty().then(|| view!{<p>"No conditions observed yet."</p>})}{component.conditions.into_iter().map(|c|view!{<div class="condition"><strong>{label(&c.condition_type)}" · "{label(&c.state)}</strong><small>{label(&c.reason)}</small></div>}).collect_view()}
        <h4>"Desired resources"</h4>{lab.resource_error.map(|_|view!{<p>"Resource demands unavailable."</p>})}
        {lab.resources.map(|r|view!{<div>{r.workloads.into_iter().filter(|w|w.component.as_deref()==Some(&id)).map(|w|view!{<div class="demand"><strong>{w.name}" × "{w.replicas}</strong>{w.containers.into_iter().map(|c|view!{<small>{c.name}{format!(" · requests {} · limits {}",quantities(&c.requests),quantities(&c.limits))}</small>}).collect_view()}</div>}).collect_view()}{r.storage.into_iter().filter(|s|s.component.as_deref()==Some(&id)).map(|s|view!{<div class="demand"><strong>"Storage · "{s.name}</strong><small>{quantities(&s.requests)}</small></div>}).collect_view()}</div>})}
        {connection.map(|command| view! {<small class="inspector-note">"Connect from your machine:"</small><code class="connect-command">{command}</code>})}
    </div> }
}
fn quantities(values: &std::collections::BTreeMap<String, String>) -> String {
    if values.is_empty() {
        "unspecified".into()
    } else {
        values
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}
fn short(value: &str, max: usize) -> String {
    if value.chars().count() > max {
        format!("{}…", value.chars().take(max - 1).collect::<String>())
    } else {
        value.into()
    }
}
fn time(unix: i64) -> String {
    let seconds = unix.rem_euclid(86400);
    format!(
        "{:02}:{:02}:{:02} UTC",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}
