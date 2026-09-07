use crate::{
    inspector::time,
    model::{block_height, cpu, memory, process_group, sat},
};
use leptos::prelude::*;
use proofstorm_core::ComponentKind;
use proofstorm_view::{ComponentBalance, EnvironmentLab, ProcessUsage, SystemView, UsageTotals};
use std::collections::{BTreeMap, BTreeSet};

#[component]
pub fn SystemSummary(
    telemetry: RwSignal<Option<SystemView>>,
    open: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <button class=move || if open.get() { "system-summary selected" } else { "system-summary" } on:click=move |_| open.set(true) aria-label="Open system usage">
            <span class="system-summary-title"><span>"System"</span><span>"↗"</span></span>
            <span class="system-summary-values"><span><small>"CPU"</small><strong>{move || cpu(telemetry.get().filter(|s|s.error.is_none()).and_then(|s|s.totals.cpu_millicores))}</strong></span><span><small>"Memory"</small><strong>{move || memory(telemetry.get().filter(|s|s.error.is_none()).and_then(|s|s.totals.memory_bytes))}</strong></span></span>
            <small class="system-summary-count">{move || telemetry.get().map_or_else(||"Loading…".into(), |s| if s.error.is_some(){"Unavailable".into()}else if s.labs.iter().any(|lab|lab.error.is_some()){"Partial inventory".into()}else if s.sampled_at_unix==0{"Sampling…".into()}else if s.totals.sampled<s.totals.running {format!("{} running · partial metrics",s.totals.running)}else{format!("{} running · {} lab{}",s.totals.running,s.labs.len(),if s.labs.len()==1{""}else{"s"})})}</small>
        </button>
    }
}

#[component]
pub fn SystemPanel(
    telemetry: RwSignal<Option<SystemView>>,
    selected_lab: RwSignal<String>,
    selected_component: RwSignal<String>,
    open: RwSignal<bool>,
) -> impl IntoView {
    let filter = RwSignal::new(String::new());
    let search = RwSignal::new(String::new());
    let include_stopped = RwSignal::new(false);
    let expanded_labs = RwSignal::new(BTreeSet::<String>::new());
    let expanded_groups = RwSignal::new(BTreeSet::<String>::new());
    view! {
        <section class="system-page">
            <div class="page-heading"><div><h1>"System"</h1><p class="page-description">"Containers in this workspace’s labs"</p></div><span class="heading-note">{move ||telemetry.get().filter(|s|s.sampled_at_unix>0).map(|s|format!("Updated {}",time(s.sampled_at_unix)))}</span></div>
            {move ||telemetry.get().and_then(|s|s.error).map(|message|view!{<div class="notice warning">{message}</div>})}
            {move ||telemetry.get().map(|s|{
                let incomplete=s.error.is_some()||s.labs.iter().any(|lab|lab.error.is_some());
                let t=s.totals;
                view!{
                    <div class="metrics"><div><span>"CPU usage"</span><strong>{cpu(t.cpu_millicores)}</strong></div><div><span>"Memory"</span><strong>{memory(t.memory_bytes)}</strong></div><div><span>"Running"</span><strong>{if incomplete{"—".into()}else{t.running.to_string()}}</strong><small>{if incomplete{"Inventory unavailable".into()}else{format!("{} ready",t.ready)}}</small></div><div><span>"Restarts"</span><strong>{if incomplete{"—".into()}else{t.restarts.to_string()}}</strong></div></div>
                    {incomplete.then(||view!{<p class="measurement-note">"Totals unavailable · some lab inventories could not be read"</p>})}
                    {(t.sampled<t.running).then(||view!{<p class="measurement-note">{format!("Partial usage · {} of {} running containers sampled",t.sampled,t.running)}</p>})}
                }
            })}
            <section class="resource-panel">
                <div class="panel-title"><h2>"Resources & processes"</h2><span>"Expand a lab or component"</span></div>
                <div class="process-filters"><select aria-label="Filter by lab" prop:value=move ||filter.get() on:change=move |event|filter.set(event_target_value(&event))><option value="">"All labs"</option>{move ||telemetry.get().map(|s|s.labs.into_iter().map(|lab|view!{<option value=lab.id>{lab.name}</option>}).collect_view())}</select><input class="search" placeholder="Find a component or process…" aria-label="Find a component or process" on:input=move |event|search.set(event_target_value(&event)) /><label><input type="checkbox" on:change=move |event|include_stopped.set(event_target_checked(&event)) />"Include stopped"</label></div>
                <div class="table-scroll"><table><thead><tr><th>"Lab / component / container"</th><th>"State"</th><th>"CPU"</th><th>"Memory"</th><th>"Restarts"</th><th>"Sampled"</th></tr></thead><tbody>{move ||{
                    let query=search.get().to_lowercase();let lab_filter=filter.get();let mut rows=Vec::new();
                    for lab in telemetry.get().into_iter().flat_map(|s|s.labs).filter(|lab|lab_filter.is_empty()||lab.id==lab_filter) {
                        let expanded=expanded_labs.get().contains(&lab.id)||!query.is_empty();
                        let mut groups=BTreeMap::<String,Vec<ProcessUsage>>::new();
                        for process in lab.processes { groups.entry(process_group(&process)).or_default().push(process); }
                        let id=lab.id.clone();let nav_id=id.clone();let totals=lab.totals;let unavailable=lab.error.is_some();
                        rows.push(view!{<tr class="lab-resource-row"><td><button class="tree-toggle" aria-expanded=expanded on:click=move |_|toggle(expanded_labs,&id)><span>{if expanded{"⌄"}else{"›"}}</span>{lab.name}</button><button class="lab-open" aria-label="Open lab topology" on:click=move |_|{selected_component.set(String::new());selected_lab.set(nav_id.clone());open.set(false);}>"↗"</button>{lab.error.or(lab.metrics_error).map(|message|view!{<small class="table-warning">{message}</small>})}</td><TotalsCells totals unavailable /></tr>}.into_any());
                        if !expanded {continue;}
                        for (name,processes) in groups {
                            let filtered=processes.iter().filter(|p| (include_stopped.get()||!p.terminated) && (query.is_empty()||format!("{name} {} {}",p.pod,p.container).to_lowercase().contains(&query))).cloned().collect::<Vec<_>>();
                            if filtered.is_empty(){continue;}
                            let key=format!("{}:{name}",lab.id);let group_open=expanded_groups.get().contains(&key)||!query.is_empty();
                            let totals=UsageTotals::from_processes(processes.iter());let nav_id=lab.id.clone();let component=processes.iter().find_map(|p|p.component.clone());
                            rows.push(view!{<tr class="component-resource-row"><td><button class="tree-toggle" aria-expanded=group_open on:click=move |_|toggle(expanded_groups,&key)><span>{if group_open{"⌄"}else{"›"}}</span>{name}</button>{component.map(|component|view!{<button class="lab-open" aria-label="Inspect component on topology" on:click=move |_|{selected_lab.set(nav_id.clone());selected_component.set(component.clone());open.set(false);}>"↗"</button>})}</td><TotalsCells totals /></tr>}.into_any());
                            if group_open { for p in filtered { rows.push(view!{<ProcessRow process=p />}.into_any()); } }
                        }
                    }
                    if rows.is_empty(){rows.push(view!{<tr><td colspan="6" class="quiet-empty">"No labs"</td></tr>}.into_any());}
                    rows.collect_view()
                }}</tbody></table></div>
            </section>
        </section>
    }
}
fn toggle(signal: RwSignal<BTreeSet<String>>, key: &str) {
    signal.update(|values| {
        if !values.remove(key) {
            values.insert(key.into());
        }
    });
}
#[component]
fn TotalsCells(totals: UsageTotals, #[prop(default = false)] unavailable: bool) -> impl IntoView {
    if unavailable {
        return view! {<td>"Unavailable"</td><td>"—"</td><td>"—"</td><td>"—"</td><td>"—"</td>}
            .into_any();
    }
    view! {<td>{format!("{} running",totals.running)}<small>{format!("{} ready",totals.ready)}</small></td><td>{cpu(totals.cpu_millicores)}</td><td>{memory(totals.memory_bytes)}</td><td>{totals.restarts}</td><td>{format!("{} / {}",totals.sampled,totals.running)}</td>}.into_any()
}
#[component]
fn ProcessRow(process: ProcessUsage) -> impl IntoView {
    let p = process;
    let measured = p.cpu_millicores.is_some() && p.memory_bytes.is_some();
    view! {<tr class="process-row"><td><strong>{p.container}</strong><small>{p.pod}</small></td><td><span class=if p.ready{"process-state ready"}else{"process-state"}>{p.state}</span></td><td>{cpu(p.cpu_millicores)}<small>{format!("Req {} · limit {}",cpu(p.cpu_request_millicores),cpu(p.cpu_limit_millicores))}</small></td><td>{memory(p.memory_bytes)}<small>{format!("Req {} · limit {}",memory(p.memory_request_bytes),memory(p.memory_limit_bytes))}</small></td><td>{p.restarts}</td><td title=p.metrics_timestamp.unwrap_or_default()>{if measured{"Yes"}else{"—"}}</td></tr>}
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
pub fn BlockHeight(
    telemetry: RwSignal<Option<SystemView>>,
    lab: RwSignal<Option<EnvironmentLab>>,
) -> impl IntoView {
    view! {<span class="block-height" title="Highest current block height observed across this lab’s Bitcoin nodes">{move ||{
        let lab=lab.get();let id=lab.as_ref().map(|l|l.id.as_str()).unwrap_or_default();
        let expected=lab.as_ref().map_or(0,|l|l.components.items.iter().filter(|c|c.kind==ComponentKind::Bitcoin).count());
        let usage=telemetry.get().and_then(|s|s.labs.into_iter().find(|l|l.id==id));
        let value=usage.as_ref().and_then(block_height);
        let sampled=usage.as_ref().map_or(0,|l|l.balances.iter().filter(|b|b.block_height.is_some()&&b.error.is_none()).count());
        format!("Block {}{}",value.map_or_else(||"—".into(),sat),if sampled>0&&sampled<expected{" · partial"}else{""})
    }}</span>}
}
#[component]
pub fn NodeBalance(
    telemetry: RwSignal<Option<SystemView>>,
    lab_id: String,
    component: String,
    kind: ComponentKind,
) -> impl IntoView {
    view! {<g class="node-balance">{move ||{
        let observation=balance(telemetry,&lab_id,&component);
        let (value,label)=if kind==ComponentKind::Bitcoin {
            (observation.as_ref().and_then(|b|b.block_height).map_or_else(||"—".into(),sat),"Block height".into())
        }else if matches!(kind,ComponentKind::Wallet|ComponentKind::Lightning){
            let amount=observation.as_ref().and_then(|b|b.amounts.iter().find(|a|a.label=="Spendable"||a.label=="Local").or_else(||b.amounts.first()));
            (amount.map_or_else(||"—".into(),|a|format!("{} sat",sat(a.sat))),amount.map_or_else(||"Balance".into(),|a|a.label.clone()))
        }else{
            let memory=telemetry.get().and_then(|s|s.labs.into_iter().find(|l|l.id==lab_id)).and_then(|l|{let processes=l.processes.iter().filter(|p|p.component.as_deref()==Some(component.as_str())).collect::<Vec<_>>();(!processes.is_empty()).then(||UsageTotals::from_processes(processes.into_iter()))}).and_then(|t|t.memory_bytes);
            (crate::model::memory(memory),"Memory".into())
        };
        view!{<line x1="17" y1="87" x2="215" y2="87"/><text class="node-amount" x="17" y="111">{value}</text><text class="node-balance-label" x="215" y="111" text-anchor="end">{label}</text>}
    }}</g>}
}
#[component]
pub fn BalancePanel(
    telemetry: RwSignal<Option<SystemView>>,
    lab_id: String,
    component: String,
) -> impl IntoView {
    view! {{move ||balance(telemetry,&lab_id,&component).map(|b|view!{
        <div class="balance-panel"><h4>"Latest observation"</h4>{b.error.map(|message|view!{<p>{message}</p>})}{b.block_height.map(|height|view!{<div class="balance-row"><span>"Block height"</span><strong>{sat(height)}</strong></div>})}{b.amounts.into_iter().map(|a|view!{<div class="balance-row"><span>{a.label}</span><strong>{sat(a.sat)}<small>"sat"</small></strong></div>}).collect_view()}<small>{format!("Observed {}",time(b.observed_at_unix))}</small></div>
    })}}
}
