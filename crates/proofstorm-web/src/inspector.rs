use crate::{
    model::{cpu_quantity, health},
    system::BalancePanel,
};
use leptos::prelude::*;
use proofstorm_core::{ComponentConditionState, ComponentConditionType};
use proofstorm_view::{ComponentView, EnvironmentCell};
#[derive(Clone, Copy)]
pub(crate) struct InspectorExpansion {
    pub selected: RwSignal<String>,
    pub cell: RwSignal<Option<EnvironmentCell>>,
    pub open: RwSignal<std::collections::BTreeSet<(String, String)>>,
}

#[component]
pub(crate) fn InspectorSection(
    title: &'static str,
    summary: Signal<String>,
    children: Children,
) -> impl IntoView {
    let state = expect_context::<InspectorExpansion>();
    let key = move || {
        let cell = state.cell.get().map(|cell| (cell.id, cell.layout_id));
        (
            serde_json::json!([cell, state.selected.get()]).to_string(),
            title.to_owned(),
        )
    };
    let expanded = move || state.open.with(|open| open.contains(&key()));
    view! {
        <section class="inspector-section">
            <button class="inspector-section-toggle" aria-expanded=move || expanded().to_string()
                on:click=move |_| state.open.update(|open| { let key = key(); if !open.remove(&key) { open.insert(key); } })>
                <span class="inspector-section-heading">{title}<span class="inspector-section-chevron" aria-hidden="true">"›"</span></span>
                <span class="inspector-section-summary">{summary}</span>
            </button>
            <div class="inspector-section-body" hidden=move || !expanded()>{children()}</div>
        </section>
    }
}

#[component]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Leptos props own values across reactive view lifetimes"
)]
pub(crate) fn ComponentPanel(
    component: ComponentView,
    cell: EnvironmentCell,
    telemetry: RwSignal<Option<proofstorm_view::SystemView>>,
    selection: String,
) -> impl IntoView {
    if let Some(embedded) = component
        .details
        .as_ref()
        .and_then(|d| {
            d.embedded
                .iter()
                .find(|e| crate::canvas_model::embedded_id(&component.id, &e.id) == selection)
        })
        .cloned()
    {
        let version = embedded.version.unwrap_or_else(|| "Not reported".into());
        return view! {
            <div class=format!("component-panel type-{}",crate::canvas_model::appearance(embedded.kind).0)>
                <span class="eyebrow type-label">{crate::canvas_model::appearance(embedded.kind).1}</span><h3>{embedded.name}</h3><p>"Embedded in "{component.id.clone()}</p>
                <InspectorSection title="Embedded resource" summary=Signal::stored(version.clone())>
                    <div class="version-details"><span>"Version"</span><strong>{version}</strong></div>
                    <p>"Shares its parent’s process and resource usage."</p>
                    {(embedded.kind != proofstorm_core::ComponentKind::Lightning).then(||view!{<p>"Separate runtime health and balances are not reported for this embedded resource."</p>})}
                </InspectorSection>
                <VersionDetails component=component.clone() title="Parent version & build" />
            </div>
        }.into_any();
    }
    view! { <div class=format!("component-panel type-{}",crate::canvas_model::appearance(component.kind).0)><span class="eyebrow type-label">{crate::canvas_model::appearance(component.kind).1}</span><h3>{component.id.clone()}</h3><p>{crate::canvas_model::implementation_label(&component.implementation).to_owned()}" · "{health(&component)}</p>
        <VersionDetails component=component.clone() />
        <BalancePanel telemetry cell_id=cell.id.clone() component=component.id.clone() />
    </div> }.into_any()
}
#[component]
pub(crate) fn ComponentDiagnostics(
    component: ComponentView,
    cell: EnvironmentCell,
) -> impl IntoView {
    let id = component.id.clone();
    let connection = component
        .endpoints
        .iter()
        .find(|e| e.local_connection_supported)
        .map(|e| {
            format!(
                "proofstorm connect {} {} {} --config connection.json",
                cell.handle
                    .as_ref()
                    .map_or(cell.id.as_str(), |h| h.name.as_str()),
                component.id,
                e.name
            )
        });
    let endpoint_summary = format!("{} available", component.endpoints.len());
    let ready = component
        .conditions
        .iter()
        .filter(|c| c.state == ComponentConditionState::True)
        .count();
    let health_summary = if component.conditions.is_empty() {
        "No observations".into()
    } else {
        format!("{ready} of {} ready", component.conditions.len())
    };
    let resource_summary = if cell.resource_error.is_some() {
        "Unavailable".into()
    } else {
        cell.resources.as_ref().map_or_else(
            || "Not reported".into(),
            |r| {
                let workloads = r
                    .workloads
                    .iter()
                    .filter(|w| w.component.as_deref() == Some(&id))
                    .count();
                let storage = r
                    .storage
                    .iter()
                    .filter(|s| s.component.as_deref() == Some(&id))
                    .count();
                format!(
                    "{workloads} workload{} · {storage} storage volume{}",
                    if workloads == 1 { "" } else { "s" },
                    if storage == 1 { "" } else { "s" }
                )
            },
        )
    };
    view! {<div class="component-diagnostics">
        <InspectorSection title="Endpoints" summary=Signal::stored(endpoint_summary)>{component.endpoints.is_empty().then(|| view!{<p>"No endpoints"</p>})}
        {component.endpoints.into_iter().map(|e| view!{<div class="endpoint"><strong>{e.name}</strong><code>{format!("{}:{}",e.cluster_host,e.port)}</code><small>{format!("{} · {}",e.transport,if e.local_connection_supported {"local connection available"} else {"cluster access"})}</small></div>}).collect_view()}
        {connection.map(|command| view! {<small class="inspector-note">"Connect locally"</small><code class="connect-command">{command}</code>})}
        </InspectorSection>
        <InspectorSection title="Health checks" summary=Signal::stored(health_summary)>{component.conditions.is_empty().then(|| view!{<p>"No observations"</p>})}{component.conditions.into_iter().map(|c|view!{<div class="condition"><strong>{condition_title(c.condition_type)}</strong><span class="check-state">{match c.state { ComponentConditionState::True => "Ready", ComponentConditionState::False => "Not ready", ComponentConditionState::Unknown => "Unknown" }}</span>{(c.state != ComponentConditionState::True).then(||view!{<p>{c.message}</p>})}</div>}).collect_view()}
        </InspectorSection>
        <InspectorSection title="Resources & limits" summary=Signal::stored(resource_summary)>{cell.resource_error.map(|_|view!{<p>"Resource requests unavailable."</p>})}
        {cell.resources.map(|r|view!{<div>{r.workloads.into_iter().filter(|w|w.component.as_deref()==Some(&id)).map(|w|view!{<div class="demand"><strong>{w.name}" × "{w.replicas}</strong>{w.containers.into_iter().map(|c|view!{<small>{c.name}{format!(" · Reserved: {} · Maximum: {}",quantities(&c.requests),quantities(&c.limits))}</small>}).collect_view()}</div>}).collect_view()}{r.storage.into_iter().filter(|s|s.component.as_deref()==Some(&id)).map(|s|view!{<div class="demand"><strong>"Storage · "{s.name}</strong><small>{quantities(&s.requests)}</small></div>}).collect_view()}</div>})}
        </InspectorSection>
    </div>}
}

pub(crate) fn observation_summary(summary: String, unix: i64, failed: bool) -> String {
    let freshness =
        crate::freshness::observation_status(unix, failed, crate::model::OBSERVATION_MAX_AGE);
    if unix <= 0 {
        freshness.label().into()
    } else if freshness == crate::model::Freshness::Live {
        summary
    } else {
        format!("{summary} · {}", freshness.label())
    }
}

fn quantities(values: &std::collections::BTreeMap<String, String>) -> String {
    if values.is_empty() {
        "unspecified".into()
    } else {
        values
            .iter()
            .map(|(k, v)| {
                if k == "cpu" {
                    format!("CPU {}", cpu_quantity(v))
                } else {
                    format!("{k} {v}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}
pub(crate) fn time(unix: i64) -> String {
    let seconds = unix.rem_euclid(86400);
    format!(
        "{:02}:{:02}:{:02} UTC",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

fn condition_title(kind: ComponentConditionType) -> &'static str {
    match kind {
        ComponentConditionType::WorkloadReady => "Workload",
        ComponentConditionType::StorageReady => "Storage",
        ComponentConditionType::CredentialsReady => "Credentials",
        ComponentConditionType::ServiceReady => "Service",
        ComponentConditionType::ProtocolReady => "Protocol",
        ComponentConditionType::DependenciesReady => "Dependencies",
        ComponentConditionType::ComponentReady => "Component",
        ComponentConditionType::ExperimentControllable => "Controls",
    }
}

#[component]
fn VersionDetails(
    component: ComponentView,
    #[prop(default = "Version & build")] title: &'static str,
) -> impl IntoView {
    let requested = component
        .version
        .clone()
        .unwrap_or_else(|| "Catalog default".into());
    let details = component.details;
    let summary = details.as_ref().map_or_else(
        || requested.clone(),
        |d| match d.observed_version.as_ref() {
            Some(version) if version == &d.resolved_version => format!("{version} · confirmed"),
            Some(version) => format!("{version} running · {} resolved", d.resolved_version),
            None => format!("{} · unconfirmed", d.resolved_version),
        },
    );
    view! {
        <InspectorSection title summary=Signal::stored(summary)>
        <div class="version-details"><span>"Resolved version"</span><strong>{details.as_ref().map_or_else(||requested.clone(),|d|d.resolved_version.clone())}</strong><span>"Confirmed running"</span><strong>{details.as_ref().and_then(|d|d.observed_version.clone()).unwrap_or_else(||"Not confirmed".into())}</strong></div>
        <div class="build-details"><p>{format!("Requested: {requested}")}</p>
            {details.map(|d|view!{
                <p>{format!("Adapter: {}",d.adapter_version)}</p><span>"Image"</span><code>{d.image}</code>
                {d.source_commit.map(|commit|view!{<span>"Source commit"</span><code>{commit}</code>})}
            })}
        </div>
        </InspectorSection>
    }
}
