use crate::{
    freshness::FreshnessStatus, inspector::InspectorSection, model::sat, relationships::msat,
};
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
        let selection = selected.get();
        let owner = crate::canvas_model::selected_owner(&cell, &selection)?;
        if selection != owner.id
            && !crate::canvas_model::nodes(&cell)
                .iter()
                .any(|n| n.id == selection && n.kind == proofstorm_core::ComponentKind::Lightning)
        {
            return None;
        }
        usage.balances.into_iter().find(|b| b.component == owner.id)
    });
    let bitcoin = Memo::new(move |_| observation.get().and_then(|b| b.bitcoin));
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
        <Show when=move ||bitcoin.get().is_some()>
            <InspectorSection title="Bitcoin peers" summary=Signal::derive(move || bitcoin.get().map_or_else(|| "Not reported".into(), |o| crate::inspector::observation_summary(format!("{} connected in this cell", o.peers.len()), o.observed_at_unix, o.error.is_some())))>
            <div class="channels-panel">
                {move ||bitcoin.get().map(|o|view!{<FreshnessStatus unix=o.observed_at_unix failed=o.error.is_some() />})}
                <Show when=move ||bitcoin.get().is_some_and(|o|o.error.is_none() && o.peers.is_empty())><p>"No connected peers in this cell"</p></Show>
                {move ||bitcoin.get().map(|o|o.peers.into_iter().map(|peer|view!{<div class="channel-detail"><strong>{peer}</strong></div>}).collect_view())}
            </div>
            </InspectorSection>
        </Show>
        <Show when=move ||holdings.get().is_some()>
            <InspectorSection title="Holdings by mint" summary=Signal::derive(move || holdings.get().map_or_else(|| "Not reported".into(), |o| crate::inspector::observation_summary(format!("{} mint{}", o.mints.len(), if o.mints.len() == 1 { "" } else { "s" }), o.observed_at_unix, o.error.is_some())))>
            <div class="holdings-panel">
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
            </InspectorSection>
        </Show>
        <Show when=move ||channels.get().is_some()>
            <InspectorSection title="Open channels" summary=Signal::derive(move || channels.get().map_or_else(|| "Not reported".into(), |o| crate::inspector::observation_summary(format!("{} open · {} active", o.channels.len(), o.channels.iter().filter(|c| c.active).count()), o.observed_at_unix, o.error.is_some())))>
            <div class="channels-panel">
                <Show when=channels_stale><p class="observation-warning">{move ||if channels.get().is_some_and(|o|o.observed_at_unix>0) {"Showing last observed channels"}else{"Channels unavailable"}}</p></Show>
                <Show when=move ||!channels_stale() && channels.get().is_some_and(|o|o.channels.is_empty())><p>"No open channels"</p></Show>
                <For each=move ||channels.get().map(|o|o.channels).unwrap_or_default() key=|c|c.id().to_owned() children=move |channel| {
                    let id=channel.id().to_owned();let data=Memo::new(move |_|channels.get().and_then(|o|o.channels.into_iter().find(|c|c.id()==id)));
                    let peer=move ||{
                        let key=data.get().map(|c|c.peer_pubkey).unwrap_or_default();
                        let names=telemetry.get().and_then(|s|s.cells.into_iter().find(|u|cell.get().is_some_and(|l|l.id==u.id))).map(|u|u.balances.into_iter().filter(|b|b.lightning.as_ref().and_then(|o|o.node_pubkey.as_deref())==Some(key.as_str())).map(|b|b.component).collect::<Vec<_>>()).unwrap_or_default();
                        if names.len()==1 {names[0].clone()}else{"Outside this cell".into()}
                    };
                    view!{<div class="channel-detail"><strong>{peer}</strong><small>{move ||if data.get().is_some_and(|c|c.active){"Active"}else{"Inactive"}}</small>
                        <div class="balance-row"><span>"Capacity"</span><strong>{move ||data.get().map(|c|msat(c.capacity_msat))}<small>"sat"</small></strong></div>
                        <div class="balance-row"><span>{move ||if data.get().is_some_and(|c|c.capacity_only){"Outbound"}else{"Local"}}</span><strong>{move ||data.get().map(|c|msat(c.local_msat))}<small>"sat"</small></strong></div>
                        <div class="balance-row"><span>{move ||if data.get().is_some_and(|c|c.capacity_only){"Inbound"}else{"Remote"}}</span><strong>{move ||data.get().map(|c|msat(c.remote_msat))}<small>"sat"</small></strong></div>
                        <details class="build-details"><summary>"Channel ID"</summary><code>{move ||data.get().map(|c|if c.funding_outpoint.is_empty(){c.id().to_owned()}else{c.funding_outpoint})}</code></details>
                    </div>}
                } />
                <small>{move ||channels.get().map(|o|view!{<FreshnessStatus unix=o.observed_at_unix failed=o.error.is_some() />})}</small>
                <p class="inspector-note">{move ||if channels.get().is_some_and(|o|o.channels.iter().any(|c|c.capacity_only)){"LDK reports outbound and inbound capacity, rounded down to whole sats. Reserves and fees can leave capacity unassigned."}else{"Balances include reserves. Fees and in-flight amounts can leave part of the capacity unassigned."}}</p>
            </div>
            </InspectorSection>
        </Show>
    }
}
