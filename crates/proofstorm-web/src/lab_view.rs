use crate::{
    graph::Graph,
    inspector::{ComponentPanel, time},
    model::{lab_name, lab_phase, label},
    system::BlockHeight,
};
use leptos::prelude::*;
use proofstorm_view::{EnvironmentLab, SystemView};

#[component]
pub fn LabPanel(
    lab: RwSignal<Option<EnvironmentLab>>,
    selected_component: RwSignal<String>,
    history_pages: RwSignal<usize>,
    zoom: RwSignal<f64>,
    pan: RwSignal<(f64, f64)>,
    telemetry: RwSignal<Option<SystemView>>,
    drawer: RwSignal<&'static str>,
) -> impl IntoView {
    view! {
        <Show when=move || lab.get().is_some()>
            <section class="lab-workspace">
                <div class="canvas-heading">
                    {move || lab.get().map(|lab| view! {<h1 title=lab.id.clone()>{lab_name(&lab)}</h1><span class="phase-badge">{lab_phase(&lab)}</span><span class="lab-count">{format!("{} components · {} links",lab.components.items.len(),lab.links.items.len())}</span>})}
                    <BlockHeight telemetry lab />
                </div>
                {move || lab.get().and_then(|lab| lab.read_error).map(|_|view!{<div class="notice warning">"This lab’s history uses an incompatible format."</div>})}
                {move || lab.get().filter(|lab| lab_phase(lab) == "blocked").and_then(|lab| lab.runtime.message).map(|message|view!{<div class="notice warning">{message}</div>})}
                <div class="canvas-layout">
                    <Graph lab selected=selected_component zoom pan telemetry />
                    <Show when=move || !selected_component.get().is_empty()>
                        <aside class="inspector" aria-label="Component details">
                            <div class="inspector-heading"><span>"Component"</span><button class="icon-button" aria-label="Close component details" on:click=move |_| selected_component.set(String::new())>"×"</button></div>
                            {move || lab.get().and_then(|lab| lab.components.items.iter().find(|component| component.id == selected_component.get()).cloned().map(|component| (lab,component))).map(|(lab,component)| view!{<ComponentPanel component lab telemetry />})}
                        </aside>
                    </Show>
                </div>
                <section class=move || if drawer.get().is_empty() { "history-drawer" } else { "history-drawer open" } aria-label="Lab history">
                    <div class="drawer-tabs" role="tablist" aria-label="History">
                        <button role="tab" aria-selected=move || drawer.get() == "activity" aria-expanded=move || drawer.get() == "activity" on:click=move |_| drawer.update(|tab| *tab=if *tab == "activity" { "" } else { "activity" })>"Activity "<span>{move || lab.get().map(|lab| lab.activity.items.len())}</span></button>
                        <button role="tab" aria-selected=move || drawer.get() == "sessions" aria-expanded=move || drawer.get() == "sessions" on:click=move |_| drawer.update(|tab| *tab=if *tab == "sessions" { "" } else { "sessions" })>"Sessions "<span>{move || lab.get().map(|lab| lab.sessions.items.len())}</span></button>
                        <Show when=move || lab.get().and_then(|l| l.resources).is_some_and(|r| !r.retained_storage.is_empty())><button role="tab" aria-selected=move || drawer.get() == "storage" on:click=move |_| drawer.update(|tab| *tab=if *tab=="storage" {""} else {"storage"})>"Retained storage"</button></Show>
                        <span class="drawer-spacer"></span>
                        <Show when=move || !drawer.get().is_empty()><button class="icon-button" aria-label="Close history" on:click=move |_| drawer.set("")>"⌄"</button></Show>
                    </div>
                    <Show when=move || !drawer.get().is_empty()><div class="drawer-content" role="tabpanel">
                        <Show when=move || drawer.get() == "activity">{move || lab.get().map(|lab| view! {
                            <div class="activity-list">{lab.activity.items.iter().map(|activity| {
                                let phase = label(&activity.phase);
                                view! {<div class="activity-row"><span class=format!("activity-dot {phase}")></span><div><strong>{proofstorm_view::action_title(activity.kind)}</strong><small>{format!("{} · {}",activity.principal_id,activity.components.join(", "))}</small><details class="action-details"><summary>"Details"</summary><code>{format!("{} · {}",label(&activity.kind),activity.id)}</code></details></div><div class="activity-outcome"><span>{proofstorm_view::outcome_title(activity.phase)}</span><small>{activity.native_exit_code.map_or_else(||time(activity.accepted_at_unix),|code|format!("Exit {code}"))}</small></div></div>}
                            }).collect_view()}{lab.activity.items.is_empty().then(|| view!{<p class="quiet-empty">"No activity"</p>})}</div>
                        })}</Show>
                        <Show when=move || drawer.get() == "sessions">{move || lab.get().map(|lab| view! {
                            <div class="session-list">{lab.sessions.items.iter().map(|session|view!{<div class="session-row"><span class="avatar">"A"</span><div><strong>{session.session.principal_id.clone()}</strong><small>{label(&session.session.phase)}</small><code>{session.session.id.clone()}</code></div></div>}).collect_view()}{lab.sessions.items.is_empty().then(||view!{<p class="quiet-empty">"No sessions"</p>})}</div>
                        })}</Show>
                        <Show when=move || drawer.get() == "storage"><div class="retained-storage">{move || lab.get().and_then(|l|l.resources).map(|r|r.retained_storage.into_iter().map(|(name,size)|view!{<div class="balance-row"><span>{name}</span><strong>{size}</strong></div>}).collect_view())}</div></Show>
                        <Show when=move || drawer.get() != "storage" && lab.get().is_some_and(|lab| if drawer.get() == "activity" { lab.activity.next_cursor.is_some() } else { lab.sessions.next_cursor.is_some() })><button class="load-more" on:click=move |_| history_pages.update(|pages| *pages += 1)>"Load more"</button></Show>
                    </div></Show>
                </section>
            </section>
        </Show>
    }
}
