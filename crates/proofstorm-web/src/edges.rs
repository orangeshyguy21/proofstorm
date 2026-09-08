use crate::{
    canvas_model::{self, CanvasNode, Positions},
    relationships::{self, Edge, EdgeKind},
};
use leptos::prelude::*;
#[component]
pub fn ObservedEdge(
    data: Memo<Option<Edge>>,
    nodes: Memo<Vec<CanvasNode>>,
    positions: RwSignal<Positions>,
    selected: RwSignal<String>,
) -> impl IntoView {
    let geometry = Memo::new(move |_| {
        data.get()
            .and_then(|edge| {
                let items = nodes.get();
                let p = positions.get();
                let from = items.iter().find(|n| n.id == edge.from)?;
                let to = items.iter().find(|n| n.id == edge.to)?;
                Some(relationships::geometry(
                    canvas_model::world_position(from, &p),
                    canvas_model::world_position(to, &p),
                    edge.lane,
                ))
            })
            .unwrap_or_default()
    });
    let amounts = Memo::new(move |_| data.get().map(|e| e.kind));
    let pulse = crate::motion::pulse(amounts);
    let channel = Memo::new(move |_| match amounts.get() {
        Some(EdgeKind::Channel {
            capacity,
            local,
            remote,
            ..
        }) => (capacity, local, remote),
        _ => (1, 0, 0),
    });
    let inspect = move || {
        if let Some(edge) = data.get_untracked() {
            selected.set(edge.from);
        }
    };
    view! {
        <g class=move ||format!("observed-edge {} {} {}",if data.get().is_some_and(|e|e.stale){"stale-edge"}else{""},if data.get().is_some_and(|e|matches!(e.kind,EdgeKind::Channel{active:false,..})){"inactive-edge"}else{""},pulse.get())>
            <Show when=move ||matches!(amounts.get(),Some(EdgeKind::Channel{..}))>
                <path class="channel-track" pathLength="100" d=move ||geometry.get().path />
                <path class="channel-local" pathLength="100" d=move ||geometry.get().path stroke-dasharray=move ||{let c=channel.get();format!("{} 100",percent(c.1,c.0))} />
                <path class="channel-remote" pathLength="100" d=move ||geometry.get().path stroke-dasharray=move ||{let c=channel.get();let remote=percent(c.2,c.0);format!("0 {} {} 100",100.0-remote,remote)} />
            </Show>
            <Show when=move ||matches!(amounts.get(),Some(EdgeKind::Holding{..}))>
                <path class="holding-path" d=move ||geometry.get().path />
            </Show>
            <path class="edge-hit" d=move ||geometry.get().path role="button" tabindex="0"
                aria-label=move ||data.get().map(|e|format!("Inspect {} to {} {}",e.from,e.to,if matches!(e.kind,EdgeKind::Channel{..}){format!("channel {}",e.lane+1)}else{"holdings".into()}))
                on:pointerdown=|event|event.stop_propagation() on:click=move |_|inspect()
                on:keydown=move |event|{if matches!(event.key().as_str(),"Enter"|" "){event.prevent_default();inspect();}} />
        </g>
    }
}
fn percent(value: u64, capacity: u64) -> f64 {
    f64::from(
        u32::try_from(u128::from(value) * 10000 / u128::from(capacity.max(1)))
            .unwrap_or(10000)
            .min(10000),
    ) / 100.0
}
