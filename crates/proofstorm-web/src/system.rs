use crate::model::{cpu, memory, sat};
use leptos::prelude::*;
use proofstorm_view::{ComponentBalance, SystemView};

pub fn toggle_fullscreen(fullscreen: RwSignal<bool>) {
    let document = web_sys::window().and_then(|window| window.document());
    if fullscreen.get_untracked() {
        fullscreen.set(false);
        if let Some(document) = document {
            document.exit_fullscreen();
        }
    } else {
        // The document stays mounted when environment snapshots replace the lab panel.
        // The fixed panel also works in browsers without the Fullscreen API.
        fullscreen.set(true);
        if let Some(element) = document.and_then(|document| document.document_element()) {
            let _ = element.request_fullscreen();
        }
    }
}

#[component]
pub fn SystemSummary(
    telemetry: RwSignal<Option<SystemView>>,
    open: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <button class=move || if open.get() { "system-summary selected" } else { "system-summary" } on:click=move |_| open.set(true) aria-label="Open system usage">
            <span class="system-summary-title"><span>"▤  System"</span><span>"↗"</span></span>
            <span class="system-summary-values"><span><small>"CPU"</small><strong>{move || cpu(telemetry.get().filter(|s| s.error.is_none()).and_then(|s| s.totals.cpu_millicores))}</strong></span><span><small>"Memory"</small><strong>{move || memory(telemetry.get().filter(|s| s.error.is_none()).and_then(|s| s.totals.memory_bytes))}</strong></span></span>
            <small class="system-summary-count">{move || telemetry.get().map_or_else(|| "Loading…".into(), |s| if s.error.is_some() { "Unavailable".into() } else if s.sampled_at_unix == 0 { "Sampling…".into() } else { format!("{} running · {} labs",s.totals.running,s.labs.len()) })}</small>
        </button>
    }
}

#[component]
pub fn SystemPanel(
    telemetry: RwSignal<Option<SystemView>>,
    selected_lab: RwSignal<String>,
    open: RwSignal<bool>,
) -> impl IntoView {
    let filter = RwSignal::new(String::new());
    let search = RwSignal::new(String::new());
    let include_stopped = RwSignal::new(false);
    view! {
        <div class="page-heading"><div><div class="breadcrumb">"WORKSPACE"</div><h1>"System"</h1><p class="page-description">"Lab containers"</p></div><span class="heading-note">{move || telemetry.get().filter(|s| s.sampled_at_unix > 0).map(|s| format!("Updated {}",crate::app::time(s.sampled_at_unix)))}</span></div>
        {move || telemetry.get().and_then(|s| s.error).map(|message| view!{<div class="notice warning">{message}</div>})}
        {move || telemetry.get().map(|s| {
            let totals = s.totals;
            view! {
                <div class="metrics system-metrics"><div><span>"CPU USAGE"</span><strong>{cpu(totals.cpu_millicores)}</strong></div><div><span>"MEMORY"</span><strong>{memory(totals.memory_bytes)}</strong></div><div><span>"RUNNING"</span><strong>{totals.running}</strong><small>"containers"</small></div><div><span>"RESTARTS"</span><strong>{totals.restarts}</strong></div></div>
                {(totals.sampled < totals.running).then(||view!{<p class="measurement-note">{format!("Partial usage · {} of {} running containers sampled",totals.sampled,totals.running)}</p>})}
                <section class="history-panel lab-usage-panel"><div class="panel-title"><h2>"By lab"</h2></div><div class="table-scroll"><table><thead><tr><th>"Lab"</th><th>"Running"</th><th>"CPU"</th><th>"Memory"</th><th>"Restarts"</th><th>"Samples"</th></tr></thead><tbody>{s.labs.into_iter().map(|lab| {
                    let id = lab.id.clone();
                    let row_filter = id.clone();
                    let row_active = id.clone();
                    let unavailable = lab.error.is_some();
                    view! { <tr class=move || if filter.get() == row_active { "row-selected" } else { "" }><td><button class="table-link" on:click=move |_| filter.set(row_filter.clone())>{lab.name}</button><button class="lab-open" aria-label="Open lab topology" on:click=move |_| { selected_lab.set(id.clone()); open.set(false); }>"↗"</button>{lab.error.or(lab.metrics_error).map(|message| view!{<small class="table-warning">{message}</small>})}</td><td>{if unavailable { "—".into() } else { lab.totals.running.to_string() }}</td><td>{cpu(lab.totals.cpu_millicores)}</td><td>{memory(lab.totals.memory_bytes)}</td><td>{lab.totals.restarts}</td><td>{format!("{}/{}",lab.totals.sampled,lab.totals.running)}</td></tr> }
                }).collect_view()}</tbody></table></div></section>
            }
        })}
        <section class="history-panel process-panel"><div class="panel-title"><h2>"Processes"</h2><span>"Containers"</span></div>
            <div class="process-filters"><select aria-label="Filter processes by lab" prop:value=move || filter.get() on:change=move |event| filter.set(event_target_value(&event))><option value="">"All labs"</option>{move || telemetry.get().map(|s| s.labs.into_iter().map(|lab|view!{<option value=lab.id>{lab.name}</option>}).collect_view())}</select><input class="search" placeholder="Filter processes…" aria-label="Filter processes" on:input=move |event| search.set(event_target_value(&event)) /><label><input type="checkbox" prop:checked=move || include_stopped.get() on:change=move |event| include_stopped.set(event_target_checked(&event)) />"Include stopped"</label></div>
            <div class="table-scroll"><table class="process-table"><thead><tr><th>"Process / pod"</th><th>"Lab"</th><th>"State"</th><th>"CPU"</th><th>"Memory"</th><th>"Restarts"</th></tr></thead><tbody>{move || {
                let query = search.get().to_lowercase();
                let lab_filter = filter.get();
                let rows = telemetry.get().into_iter().flat_map(|s| s.labs).filter(|lab| lab_filter.is_empty() || lab.id == lab_filter).flat_map(|lab| lab.processes.into_iter().map(move |process| (lab.name.clone(), process))).filter(|(_,p)| (include_stopped.get() || !matches!(p.state.as_str(),"Completed"|"Error"|"Stopped"|"OOMKilled")) && format!("{} {} {}",p.pod,p.container,p.component.as_deref().unwrap_or_default()).to_lowercase().contains(&query)).collect::<Vec<_>>();
                if rows.is_empty() { return view!{<tr><td colspan="6" class="quiet-empty">"No processes"</td></tr>}.into_any(); }
                rows.into_iter().map(|(lab,p)| {
                    let title = p.metrics_timestamp.as_ref().map_or_else(|| "No measurement".into(), |time| format!("Measured {time}"));
                    view! { <tr><td><strong>{p.component.unwrap_or_else(||p.container.clone())}</strong><small>{format!("{} / {}",p.pod,p.container)}</small></td><td>{lab}</td><td><span class=if p.running && p.ready { "process-state ready" } else { "process-state" }>{p.state}</span></td><td title=title.clone()>{cpu(p.cpu_millicores)}<small>{format!("{} requested",cpu(p.cpu_request_millicores))}</small></td><td title=title>{memory(p.memory_bytes)}<small>{format!("{} requested",memory(p.memory_request_bytes))}</small></td><td>{p.restarts}</td></tr> }
                }).collect_view().into_any()
            }}</tbody></table></div>
        </section>
    }
}

fn balance(
    telemetry: RwSignal<Option<SystemView>>,
    lab: &str,
    component: &str,
) -> Option<ComponentBalance> {
    telemetry
        .get()?
        .labs
        .into_iter()
        .find(|l| l.id == lab)?
        .balances
        .into_iter()
        .find(|b| b.component == component)
}

#[component]
pub fn NodeBalance(
    telemetry: RwSignal<Option<SystemView>>,
    lab_id: String,
    component: String,
) -> impl IntoView {
    view! { <g class="node-balance">{move || balance(telemetry, &lab_id, &component).map(|balance| {
        let amount = balance.amounts.iter().find(|a| a.label == "Spendable" || a.label == "Local").or_else(|| balance.amounts.first());
        let value = amount.map_or_else(|| "—".into(), |amount| format!("{} sat",sat(amount.sat)));
        let label = if balance.error.is_some() { "Unavailable".into() } else { amount.map_or_else(String::new,|a|a.label.clone()) };
        view! { <line x1="15" y1="77" x2="190" y2="77"/><text class="node-amount" x="15" y="99">{value}</text><text class="node-balance-label" x="190" y="99" text-anchor="end">{label}</text> }
    })}</g> }
}

#[component]
pub fn BalancePanel(
    telemetry: RwSignal<Option<SystemView>>,
    lab_id: String,
    component: String,
) -> impl IntoView {
    view! { {move || balance(telemetry, &lab_id, &component).map(|balance| view! {
        <div class="balance-panel"><h4>"Balance"</h4>{balance.error.map(|message| view!{<p>{message}</p>})}{balance.amounts.into_iter().map(|a|view!{<div class="balance-row"><span>{a.label}</span><strong>{sat(a.sat)}<small>"sat"</small></strong></div>}).collect_view()}<small>{format!("Observed {}",crate::app::time(balance.observed_at_unix))}</small></div>
    })} }
}
