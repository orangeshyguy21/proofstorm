use crate::{
    graph::Graph,
    inspector::{ComponentDiagnostics, ComponentPanel, time},
    model::{cell_name, cell_phase, label},
    system::BlockHeight,
};
use leptos::prelude::*;
use proofstorm_view::{EnvironmentCell, SystemView};

#[component]
pub fn CellPanel(
    cell: RwSignal<Option<EnvironmentCell>>,
    selected_component: RwSignal<String>,
    history_pages: RwSignal<usize>,
    zoom: RwSignal<f64>,
    pan: RwSignal<(f64, f64)>,
    telemetry: RwSignal<Option<SystemView>>,
    drawer: RwSignal<&'static str>,
) -> impl IntoView {
    view! {
        <Show when=move || cell.get().is_some()>
            <section class="cell-workspace">
                <div class="canvas-heading">
                    {move || cell.get().map(|cell| view! {<h1 title=cell.id.clone()>{cell_name(&cell)}</h1><span class="phase-badge">{cell_phase(&cell)}</span><span class="cell-count">{format!("{} components · {} embedded",cell.components.items.len(),cell.components.items.iter().filter_map(|c|c.details.as_ref()).map(|d|d.embedded.len()).sum::<usize>())}</span>})}
                    <BlockHeight telemetry cell />
                </div>
                {move || cell.get().and_then(|cell| cell.read_error).map(|_|view!{<div class="notice warning">"This cell’s history uses an incompatible format."</div>})}
                {move || cell.get().filter(|cell| cell_phase(cell) == "blocked").and_then(|cell| cell.runtime.message).map(|message|view!{<div class="notice warning">{message}</div>})}
                <div class="canvas-layout">
                    <Graph cell selected=selected_component zoom pan telemetry />
                    <Show when=move || !selected_component.get().is_empty()>
                        <aside class="inspector" aria-label="Component details">
                            <div class="inspector-heading"><span>"Component"</span><button class="icon-button" aria-label="Close component details" on:click=move |_| selected_component.set(String::new())>"×"</button></div>
                            {move || cell.get().and_then(|cell| crate::canvas_model::selected_owner(&cell, &selected_component.get()).cloned().map(|component| (cell,component))).map(|(cell,component)| view!{<ComponentPanel component cell telemetry selection=selected_component.get() />})}
                            <crate::relationship_panel::RelationshipPanel telemetry cell selected=selected_component />
                            {move || cell.get().and_then(|cell|cell.components.items.iter().find(|c|c.id==selected_component.get()).cloned().map(|c|(cell,c))).map(|(cell,component)|view!{<ComponentDiagnostics component cell />})}
                        </aside>
                    </Show>
                </div>
                <section class=move || if drawer.get().is_empty() { "history-drawer" } else { "history-drawer open" } aria-label="Cell history">
                    <div class="drawer-tabs" role="tablist" aria-label="History">
                        <button role="tab" aria-selected=move || drawer.get() == "activity" aria-expanded=move || drawer.get() == "activity" on:click=move |_| drawer.update(|tab| *tab=if *tab == "activity" { "" } else { "activity" })>"Activity "<span>{move || cell.get().map(|cell| cell.activity.items.len())}</span></button>
                        <button role="tab" aria-selected=move || drawer.get() == "sessions" aria-expanded=move || drawer.get() == "sessions" on:click=move |_| drawer.update(|tab| *tab=if *tab == "sessions" { "" } else { "sessions" })>"Sessions "<span>{move || cell.get().map(|cell| cell.sessions.items.len())}</span></button>
                        <Show when=move || cell.get().and_then(|l| l.resources).is_some_and(|r| !r.retained_storage.is_empty())><button role="tab" aria-selected=move || drawer.get() == "storage" on:click=move |_| drawer.update(|tab| *tab=if *tab=="storage" {""} else {"storage"})>"Retained storage"</button></Show>
                        <span class="drawer-spacer"></span>
                        <Show when=move || !drawer.get().is_empty()><button class="icon-button" aria-label="Close history" on:click=move |_| drawer.set("")>"⌄"</button></Show>
                    </div>
                    <Show when=move || !drawer.get().is_empty()><div class="drawer-content" role="tabpanel">
                        <Show when=move || drawer.get() == "activity">{move || cell.get().map(|cell| view! {
                            <div class="activity-list">{cell.activity.items.iter().map(|activity| {
                                let phase = label(&activity.phase);
                                view! {<div class="activity-row"><span class=format!("activity-dot {phase}")></span><div><strong>{proofstorm_view::action_title(activity.kind)}</strong><small>{format!("{} · {}",activity.principal_id,activity.components.join(", "))}</small><details class="action-details"><summary>"Details"</summary><code>{format!("{} · {}",label(&activity.kind),activity.id)}</code></details></div><div class="activity-outcome"><span>{proofstorm_view::outcome_title(activity.phase)}</span><small>{activity.native_exit_code.map_or_else(||time(activity.accepted_at_unix),|code|format!("Exit {code}"))}</small></div></div>}
                            }).collect_view()}{cell.activity.items.is_empty().then(|| view!{<p class="quiet-empty">"No activity"</p>})}</div>
                        })}</Show>
                        <Show when=move || drawer.get() == "sessions">{move || cell.get().map(|cell| view! {
                            <div class="session-list">{cell.sessions.items.iter().map(|session|view!{<div class="session-row"><span class="avatar">"A"</span><div><strong>{session.session.principal_id.clone()}</strong><small>{label(&session.session.phase)}</small><code>{session.session.id.clone()}</code></div></div>}).collect_view()}{cell.sessions.items.is_empty().then(||view!{<p class="quiet-empty">"No sessions"</p>})}</div>
                        })}</Show>
                        <Show when=move || drawer.get() == "storage"><div class="retained-storage">{move || cell.get().and_then(|l|l.resources).map(|r|r.retained_storage.into_iter().map(|(name,size)|view!{<div class="balance-row"><span>{name}</span><strong>{size}</strong></div>}).collect_view())}</div></Show>
                        <Show when=move || drawer.get() != "storage" && cell.get().is_some_and(|cell| if drawer.get() == "activity" { cell.activity.next_cursor.is_some() } else { cell.sessions.next_cursor.is_some() })><button class="load-more" on:click=move |_| history_pages.update(|pages| *pages += 1)>"Load more"</button></Show>
                    </div></Show>
                </section>
            </section>
        </Show>
    }
}
