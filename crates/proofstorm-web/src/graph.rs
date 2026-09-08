use crate::{
    canvas_model::{self, CanvasNode, Layout, Positions},
    model::health,
    system::NodeBalance,
};
use leptos::prelude::*;
use proofstorm_view::{EnvironmentLab, SystemView};
use wasm_bindgen::JsCast;

#[derive(Clone)]
struct Drag {
    node: Option<String>,
    last: (i32, i32),
    start: (i32, i32),
    moved: bool,
}

#[component]
#[allow(
    clippy::too_many_lines,
    reason = "canvas owns pointer gestures and persistence"
)]
pub fn Graph(
    lab: RwSignal<Option<EnvironmentLab>>,
    selected: RwSignal<String>,
    zoom: RwSignal<f64>,
    pan: RwSignal<(f64, f64)>,
    telemetry: RwSignal<Option<SystemView>>,
) -> impl IntoView {
    let nodes = Memo::new(move |_| {
        lab.get()
            .map(|l| canvas_model::nodes(&l))
            .unwrap_or_default()
    });
    let positions = RwSignal::new(Positions::new());
    let camera = RwSignal::new((0.0, 0.0, 900.0, 500.0));
    let storage_key = RwSignal::new(String::new());
    let save_error = RwSignal::new(false);
    let drag = RwSignal::new(None::<Drag>);
    let suppress_click = RwSignal::new(false);
    let fit = move || {
        camera.set(canvas_model::bounds(
            &nodes.get_untracked(),
            &positions.get_untracked(),
        ));
        zoom.set(1.0);
        pan.set((0.0, 0.0));
    };
    let save = move || {
        let key = storage_key.get_untracked();
        let result = (|| {
            let storage = web_sys::window()?.local_storage().ok()??;
            let encoded = serde_json::to_string(&Layout {
                positions: positions.get_untracked(),
            })
            .ok()?;
            storage.set_item(&key, &encoded).ok()
        })();
        save_error.set(result.is_none());
    };
    Effect::new(move |_| {
        let Some(lab) = lab.get() else {
            return;
        };
        let current = nodes.get();
        let key = format!(
            "proofstorm.canvas.v1:{}",
            lab.layout_id.as_deref().unwrap_or(&lab.id)
        );
        let changed = key != storage_key.get_untracked();
        let mut next = if changed {
            web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
                .and_then(|s| s.get_item(&key).ok().flatten())
                .and_then(|raw| serde_json::from_str::<Layout>(&raw).ok())
                .unwrap_or_default()
                .positions
        } else {
            positions.get_untracked()
        };
        canvas_model::ensure_positions(&current, &mut next);
        if next != positions.get_untracked() {
            positions.set(next);
        }
        if changed {
            storage_key.set(key);
            fit();
        }
    });
    let finish = move |event: web_sys::PointerEvent| {
        if let Some(state) = drag.get_untracked() {
            suppress_click.set(state.moved);
            if state.moved {
                save();
            } else if let Some(id) = state.node {
                selected.set(id);
            }
        }
        drag.set(None);
        if let Some(svg) = event
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        {
            let _ = svg.release_pointer_capture(event.pointer_id());
        }
    };
    view! {
        <div class="graph" class:dragging=move ||drag.get().is_some()>
            <svg viewBox=move ||{let (x,y,w,h)=camera.get();format!("{x} {y} {w} {h}")} aria-label="Lab component topology" role="group"
                on:pointerdown=move |event| {
                    if event.button()!=0{return;}
                    let node=event.target().and_then(|t|t.dyn_into::<web_sys::Element>().ok()).and_then(|e|e.closest("[data-node-id]").ok().flatten()).and_then(|e|e.get_attribute("data-node-id"));
                    let point=(event.client_x(),event.client_y());
                    suppress_click.set(false);drag.set(Some(Drag{node,last:point,start:point,moved:false}));
                    if let Some(svg)=event.current_target().and_then(|t|t.dyn_into::<web_sys::Element>().ok()){let _=svg.set_pointer_capture(event.pointer_id());}
                }
                on:pointermove=move |event| {
                    let Some(mut state)=drag.get_untracked() else{return;};
                    let point=(event.client_x(),event.client_y());
                    if !state.moved && (point.0-state.start.0).abs()+(point.1-state.start.1).abs()<4{return;}
                    state.moved=true;
                    let (_,_,w,h)=camera.get_untracked();
                    let scale=event.current_target().and_then(|t|t.dyn_into::<web_sys::Element>().ok()).map_or(1.0,|e|(w/f64::from(e.client_width().max(1))).max(h/f64::from(e.client_height().max(1))));
                    let delta=(f64::from(point.0-state.last.0)*scale,f64::from(point.1-state.last.1)*scale);
                    if let Some(id)=&state.node {
                        let items=nodes.get_untracked();
                        if let Some(node)=items.iter().find(|n|&n.id==id) {let z=zoom.get_untracked();positions.update(|p|canvas_model::move_node(node,&items,p,(delta.0/z,delta.1/z)));}
                    }else{pan.update(|p|{p.0+=delta.0;p.1+=delta.1;});}
                    state.last=point;drag.set(Some(state));
                }
                on:pointerup=finish on:pointercancel=move |_|{drag.set(None);save();}>
                <defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path class="arrow-head" d="M 0 0 L 10 5 L 0 10 z" /></marker></defs>
                <g transform=move ||{let (left,top,width,height)=camera.get();let scale=zoom.get();let offset=pan.get();format!("translate({} {}) scale({scale})",offset.0+(left+width/2.0)*(1.0-scale),offset.1+(top+height/2.0)*(1.0-scale))}>
                    <For each=move ||lab.get().map(|l|l.links.items).unwrap_or_default() key=|link|link.id.clone() children=move |link| {
                        let id=link.id;
                        view!{<path class="connection" marker-end="url(#arrow)" d=move ||{
                            let items=nodes.get();let p=positions.get();
                            lab.get().and_then(|l|l.links.items.into_iter().find(|l|l.id==id)).and_then(|link|{
                                let from=items.iter().find(|n|n.id==link.from)?;let to=items.iter().find(|n|n.id==link.to)?;
                                let a=canvas_model::world_position(from,&p);let b=canvas_model::world_position(to,&p);
                                let (sx,ex)=if b.0>=a.0 {(a.0+260.0,b.0)}else{(a.0,b.0+260.0)};let mid=f64::midpoint(sx,ex);
                                Some(format!("M {sx} {} C {mid} {}, {mid} {}, {ex} {}",a.1+72.0,a.1+72.0,b.1+72.0,b.1+72.0))
                            }).unwrap_or_default()
                        } />}
                    } />
                    <For each=move ||nodes.get() key=|node|node.id.clone() children=move |node| {
                        let id=node.id;
                        let data=Memo::new(move |_|nodes.get().into_iter().find(|n|n.id==id));
                        view!{<CanvasTile data lab selected positions nodes telemetry suppress_click on_save=save />}
                    } />
                </g>
            </svg>
            <div class="graph-toolbar"><button aria-label="Zoom out" on:click=move |_|zoom.update(|z|*z=(*z/1.2).max(0.15))>"−"</button><span>{move ||format!("{:.0}%",zoom.get()*100.0)}</span><button aria-label="Zoom in" on:click=move |_|zoom.update(|z|*z=(*z*1.2).min(6.0))>"+"</button><button on:click=move |_|fit()>"Fit to lab"</button><button on:click=move |_|{let mut next=Positions::new();canvas_model::ensure_positions(&nodes.get_untracked(),&mut next);positions.set(next);fit();save();}>"Reset layout"</button></div>
            <Show when=move ||save_error.get()><div class="layout-notice" role="status">"Layout could not be saved in this browser."</div></Show>
            <div class="graph-legend"><span>"Drag to arrange · arrow keys to move selected items"</span></div>
        </div>
    }
}
#[component]
fn CanvasTile(
    data: Memo<Option<CanvasNode>>,
    lab: RwSignal<Option<EnvironmentLab>>,
    selected: RwSignal<String>,
    positions: RwSignal<Positions>,
    nodes: Memo<Vec<CanvasNode>>,
    telemetry: RwSignal<Option<SystemView>>,
    suppress_click: RwSignal<bool>,
    on_save: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let status = move || {
        data.get()
            .and_then(|node| {
                lab.get()
                    .and_then(|l| l.components.items.into_iter().find(|c| c.id == node.owner))
            })
            .map_or("unknown", |c| health(&c))
    };
    view! {
        <g data-node-id=move ||data.get().map(|n|n.id) class=move ||data.get().map(|n|format!("graph-node type-{} {} {}",canvas_model::appearance(n.kind).0,if selected.get()==n.id {"active"}else{""},if n.parent.is_some(){"embedded-node"}else{""})).unwrap_or_default()
            transform=move ||data.get().map(|n|{let p=canvas_model::world_position(&n,&positions.get());format!("translate({} {})",p.0,p.1)})
            role="button" tabindex="0" aria-label=move ||data.get().map(|n|format!("Inspect {}{}",n.name,n.parent.map(|p|format!(" in {p}")).unwrap_or_default()))
            on:click=move |_|{if !suppress_click.get_untracked(){if let Some(n)=data.get_untracked(){selected.set(n.id);}}}
            on:keydown=move |event| {
                let Some(node)=data.get_untracked() else{return;};
                if matches!(event.key().as_str(),"Enter"|" "){event.prevent_default();selected.set(node.id);return;}
                let step=if event.shift_key(){40.0}else{10.0};
                let delta=match event.key().as_str(){"ArrowLeft"=>(-step,0.0),"ArrowRight"=>(step,0.0),"ArrowUp"=>(0.0,-step),"ArrowDown"=>(0.0,step),_=>return};
                event.prevent_default();selected.set(node.id.clone());positions.update(|p|canvas_model::move_node(&node,&nodes.get_untracked(),p,delta));on_save();
            }>
            {move ||data.get().filter(|n|n.embedded_count>0).map(|n|view!{<rect class="component-group" x="-12" y="-12" width="284" height={canvas_model::group_height(&n)+24.0} rx="16" />})}
            <rect class="node-body" width=move ||data.get().map_or(260,|n|if n.parent.is_some(){232}else{260}) height=move ||data.get().map_or(144,|n|if n.parent.is_some(){88}else{144}) rx="10" />
            <path class="type-icon" transform="translate(16 12) scale(.65)" d=move ||data.get().map(|n|canvas_model::appearance(n.kind).2) />
            <text class="node-kind" x="39" y="25">{move ||data.get().map(|n|canvas_model::appearance(n.kind).1)}</text>
            <text class="node-name" x="17" y="52">{move ||data.get().map(|n|short(&n.name,26))}</text>
            <text class="node-impl" x="17" y="73">{move ||data.get().map(|n|if n.parent.is_some(){"Embedded · shares parent process".into()}else{short(&n.implementation,30)})}</text>
            <Show when=move ||data.get().is_some_and(|n|n.parent.is_none())>
                {move ||data.get().zip(lab.get()).map(|(n,l)|view!{<NodeBalance telemetry lab_id=l.id component=n.owner kind=n.kind />})}
                <text class="node-health" x="17" y="132">{status}</text><circle class=move ||format!("status-dot {}",status()) cx="241" cy="127" r="4" />
            </Show>
        </g>
    }
}
fn short(value: &str, max: usize) -> String {
    if value.chars().count() > max {
        format!("{}…", value.chars().take(max - 1).collect::<String>())
    } else {
        value.into()
    }
}
