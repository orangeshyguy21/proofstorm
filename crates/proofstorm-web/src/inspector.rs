use crate::{
    model::{health, label},
    system::BalancePanel,
};
use leptos::prelude::*;
use proofstorm_core::{ComponentConditionState, ComponentConditionType};
use proofstorm_view::{ComponentView, EnvironmentLab};
#[component]
pub(crate) fn ComponentPanel(
    component: ComponentView,
    lab: EnvironmentLab,
    telemetry: RwSignal<Option<proofstorm_view::SystemView>>,
) -> impl IntoView {
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
        <BalancePanel telemetry lab_id=lab.id.clone() component=component.id.clone() />
        <h4>"Endpoints"</h4>{component.endpoints.is_empty().then(|| view!{<p>"No endpoints"</p>})}
        {component.endpoints.into_iter().map(|e| view!{<div class="endpoint"><strong>{e.name}</strong><code>{format!("{}:{}",e.cluster_host,e.port)}</code><small>{format!("{} · {}",e.transport,if e.local_connection_supported {"local connection available"} else {"cluster access"})}</small></div>}).collect_view()}
        <h4>"Health checks"</h4>{component.conditions.is_empty().then(|| view!{<p>"No observations"</p>})}{component.conditions.into_iter().map(|c|view!{<div class="condition"><strong>{condition_title(c.condition_type)}</strong><span class="check-state">{match c.state { ComponentConditionState::True => "Ready", ComponentConditionState::False => "Not ready", ComponentConditionState::Unknown => "Unknown" }}</span>{(c.state != ComponentConditionState::True).then(||view!{<p>{c.message}</p>})}</div>}).collect_view()}
        <h4>"Desired resources"</h4>{lab.resource_error.map(|_|view!{<p>"Resource requests unavailable."</p>})}
        {lab.resources.map(|r|view!{<div>{r.workloads.into_iter().filter(|w|w.component.as_deref()==Some(&id)).map(|w|view!{<div class="demand"><strong>{w.name}" × "{w.replicas}</strong>{w.containers.into_iter().map(|c|view!{<small>{c.name}{format!(" · requests {} · limits {}",quantities(&c.requests),quantities(&c.limits))}</small>}).collect_view()}</div>}).collect_view()}{r.storage.into_iter().filter(|s|s.component.as_deref()==Some(&id)).map(|s|view!{<div class="demand"><strong>"Storage · "{s.name}</strong><small>{quantities(&s.requests)}</small></div>}).collect_view()}</div>})}
        {connection.map(|command| view! {<small class="inspector-note">"Connect locally"</small><code class="connect-command">{command}</code>})}
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
