use crate::{freshness::FreshnessStatus, model::sat, relationships::msat};
use leptos::prelude::*;
use proofstorm_view::{EnvironmentCell, SystemView};
#[component]
pub fn RelationshipPanel(
    telemetry: RwSignal<Option<SystemView>>,
    cell: RwSignal<Option<EnvironmentCell>>,
    selected: RwSignal<String>,
) -> impl IntoView {
    let observation = Memo::new(move |_| {
        let cell = cell.get()?;
        let system = telemetry.get()?;
        let usage = system.cells.into_iter().find(|u| {
            u.id == cell.id && cell.layout_id.as_deref() == Some(u.incarnation.as_str())
        })?;
        usage
            .balances
            .into_iter()
            .find(|b| b.component == selected.get())
    });
    let holdings = Memo::new(move |_| observation.get().and_then(|b| b.holdings));
    let channels = Memo::new(move |_| observation.get().and_then(|b| b.lightning));
    let holdings_stale = move || {
        holdings.get().is_some_and(|o| {
            crate::freshness::observation_status(
                o.observed_at_unix,
                o.error.is_some(),
                crate::model::OBSERVATION_MAX_AGE,
            ) != crate::model::Freshness::Live
        })
    };
    let channels_stale = move || {
        channels.get().is_some_and(|o| {
            crate::freshness::observation_status(
                o.observed_at_unix,
                o.error.is_some(),
                crate::model::OBSERVATION_MAX_AGE,
            ) != crate::model::Freshness::Live
        })
    };
    view! {
        <Show when=move ||holdings.get().is_some()>
            <div class="holdings-panel"><h4>"Holdings by mint"</h4>
                <Show when=holdings_stale><p class="observation-warning">{move ||if holdings.get().is_some_and(|o|o.observed_at_unix>0) {"Showing last observed holdings"}else{"Holdings unavailable"}}</p></Show>
                <Show when=move ||!holdings_stale() && holdings.get().is_some_and(|o|o.mints.is_empty())><p>"No holdings"</p></Show>
                <For each=move ||holdings.get().map(|o|o.mints).unwrap_or_default() key=|h|h.id.clone() children=move |holding| {
                    let id=holding.id;let data=Memo::new(move |_|holdings.get().and_then(|o|o.mints.into_iter().find(|h|h.id==id)));
                    view!{<div class="mint-holding"><strong>{move ||data.get().and_then(|h|h.mint).unwrap_or_else(||"Unmapped mint".into())}</strong>
                        {move ||data.get().map(|h|h.amounts.into_iter().map(|a|view!{<div class="balance-row"><span>{a.label}</span><strong>{sat(a.sat)}<small>"sat"</small></strong></div>}).collect_view())}
                    </div>}
                } />
                <small>{move ||holdings.get().map(|o|view!{<FreshnessStatus unix=o.observed_at_unix failed=o.error.is_some() />})}</small>
                <p class="inspector-note">"Connections show spendable and reserved holdings. Pending amounts are listed separately."</p>
            </div>
        </Show>
        <Show when=move ||channels.get().is_some()>
            <div class="channels-panel"><h4>"Open channels"</h4>
                <Show when=channels_stale><p class="observation-warning">{move ||if channels.get().is_some_and(|o|o.observed_at_unix>0) {"Showing last observed channels"}else{"Channels unavailable"}}</p></Show>
                <Show when=move ||!channels_stale() && channels.get().is_some_and(|o|o.channels.is_empty())><p>"No open channels"</p></Show>
                <For each=move ||channels.get().map(|o|o.channels).unwrap_or_default() key=|c|c.funding_outpoint.clone() children=move |channel| {
                    let id=channel.funding_outpoint;let data=Memo::new(move |_|channels.get().and_then(|o|o.channels.into_iter().find(|c|c.funding_outpoint==id)));
                    let peer=move ||{
                        let key=data.get().map(|c|c.peer_pubkey).unwrap_or_default();
                        let names=telemetry.get().and_then(|s|s.cells.into_iter().find(|u|cell.get().is_some_and(|l|l.id==u.id))).map(|u|u.balances.into_iter().filter(|b|b.lightning.as_ref().and_then(|o|o.node_pubkey.as_deref())==Some(key.as_str())).map(|b|b.component).collect::<Vec<_>>()).unwrap_or_default();
                        if names.len()==1 {names[0].clone()}else{"Outside this cell".into()}
                    };
                    view!{<div class="channel-detail"><strong>{peer}</strong><small>{move ||if data.get().is_some_and(|c|c.active){"Active"}else{"Inactive"}}</small>
                        <div class="balance-row"><span>"Capacity"</span><strong>{move ||data.get().map(|c|msat(c.capacity_msat))}<small>"sat"</small></strong></div>
                        <div class="balance-row"><span>"Local"</span><strong>{move ||data.get().map(|c|msat(c.local_msat))}<small>"sat"</small></strong></div>
                        <div class="balance-row"><span>"Remote"</span><strong>{move ||data.get().map(|c|msat(c.remote_msat))}<small>"sat"</small></strong></div>
                        <details class="build-details"><summary>"Channel ID"</summary><code>{move ||data.get().map(|c|c.funding_outpoint)}</code></details>
                    </div>}
                } />
                <small>{move ||channels.get().map(|o|view!{<FreshnessStatus unix=o.observed_at_unix failed=o.error.is_some() />})}</small>
                <p class="inspector-note">"Balances include reserves. Fees and in-flight amounts can leave part of the capacity unassigned."</p>
            </div>
        </Show>
    }
}
