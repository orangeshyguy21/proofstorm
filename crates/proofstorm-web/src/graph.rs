use crate::{
    model::{health, label, position},
    system::NodeBalance,
};
use leptos::prelude::*;
use proofstorm_view::{EnvironmentLab, SystemView};
use wasm_bindgen::JsCast;

#[component]
pub fn Graph(
    lab: RwSignal<Option<EnvironmentLab>>,
    selected: RwSignal<String>,
    zoom: RwSignal<f64>,
    pan: RwSignal<(f64, f64)>,
    telemetry: RwSignal<Option<SystemView>>,
) -> impl IntoView {
    let drag = RwSignal::new(None::<(i32, i32)>);
    let size = Memo::new(move |_| {
        lab.get().map_or((900, 500), |lab| {
            let nodes = &lab.components.items;
            (
                nodes
                    .iter()
                    .map(|c| position(nodes, &c.id).0 + 260)
                    .max()
                    .unwrap_or(900),
                nodes
                    .iter()
                    .map(|c| position(nodes, &c.id).1 + 150)
                    .max()
                    .unwrap_or(500)
                    .max(400),
            )
        })
    });
    view! {
        <div class="graph"><svg viewBox=move || format!("0 0 {} {}",size.get().0,size.get().1) aria-label="Lab component topology" role="group"
            on:pointerdown=move |event| { if event.button() == 0 { drag.set(Some((event.client_x(),event.client_y()))); } }
            on:pointerup=move |_| drag.set(None) on:pointerleave=move |_| drag.set(None)
            on:pointermove=move |event| { if let Some((x,y))=drag.get_untracked() {
                let (width,height) = size.get_untracked();
                let scale = event.current_target().and_then(|t|t.dyn_into::<web_sys::Element>().ok()).map_or(1.0, |e| (f64::from(width)/f64::from(e.client_width().max(1))).max(f64::from(height)/f64::from(e.client_height().max(1))));
                pan.update(|p| { p.0+=f64::from(event.client_x()-x)*scale; p.1+=f64::from(event.client_y()-y)*scale; });
                drag.set(Some((event.client_x(),event.client_y())));
            } }>
            <defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path class="arrow-head" d="M 0 0 L 10 5 L 0 10 z" /></marker></defs>
            <g transform=move || {let (w,h)=size.get();let z=zoom.get();let p=pan.get();format!("translate({} {}) scale({z})",p.0+f64::from(w)*0.5*(1.0-z),p.1+f64::from(h)*0.5*(1.0-z))}>
                {move || lab.get().map(|lab| {
                    let nodes=&lab.components.items;
                    lab.links.items.iter().filter(|l|nodes.iter().any(|c|c.id==l.from)&&nodes.iter().any(|c|c.id==l.to)).map(|link| {
                        let (x1,y1)=position(nodes,&link.from);let (x2,y2)=position(nodes,&link.to);
                        let (start,end)=match x2.cmp(&x1) {
                            std::cmp::Ordering::Greater=>((x1+232,y1+64),(x2,y2+64)),
                            std::cmp::Ordering::Less=>((x1,y1+64),(x2+232,y2+64)),
                            std::cmp::Ordering::Equal=>if y1<y2 {((x1+116,y1+128),(x2+116,y2))}else{((x1+116,y1),(x2+116,y2+128))},
                        };
                        let mid=i32::midpoint(start.0,end.0);
                        view!{<g><path class="connection" d=format!("M {} {} C {mid} {}, {mid} {}, {} {}",start.0,start.1,start.1,end.1,end.0,end.1) marker-end="url(#arrow)"/><title>{format!("{} → {} · {}",link.from,link.to,label(&link.kind))}</title></g>}
                    }).collect_view()
                })}
                {move || lab.get().map(|lab| {
                    let nodes=&lab.components.items;
                    nodes.iter().map(|c| {
                        let (x,y)=position(nodes,&c.id);let id=c.id.clone();let active=id.clone();let key=id.clone();let status=health(c);
                        view!{<g class=move || format!("graph-node {status} {}",if selected.get()==active{"active"}else{""}) transform=format!("translate({x} {y})") tabindex="0" role="button" aria-label=format!("Inspect {}",c.id) on:pointerdown=move |event|event.stop_propagation() on:click=move |_|selected.set(id.clone()) on:keydown=move |event|{if matches!(event.key().as_str(),"Enter"|" "){event.prevent_default();selected.set(key.clone());}}>
                            <rect width="232" height="128" rx="10"/><circle cx="212" cy="22" r="4"/>
                            <text class="node-kind" x="17" y="26">{label(&c.kind)}</text><text class="node-name" x="17" y="52">{short(&c.id,26)}</text><text class="node-impl" x="17" y="73">{short(&c.implementation,30)}</text>
                            <NodeBalance telemetry lab_id=lab.id.clone() component=c.id.clone() kind=c.kind />
                            <title>{format!("{} · {} · {status}",c.id,c.implementation)}</title>
                        </g>}
                    }).collect_view()
                })}
            </g>
        </svg>
        <div class="graph-toolbar"><button aria-label="Zoom out" on:click=move |_|zoom.update(|z|*z=(*z/1.2).max(0.3))>"−"</button><span>{move ||format!("{:.0}%",zoom.get()*100.0)}</span><button aria-label="Zoom in" on:click=move |_|zoom.update(|z|*z=(*z*1.2).min(4.0))>"+"</button><button on:click=move |_|{zoom.set(1.0);pan.set((0.0,0.0));}>"Fit to lab"</button></div>
        <div class="graph-legend"><span><i class="ready"></i>"Ready"</span><span><i class="pending"></i>"Pending"</span><span><i class="blocked"></i>"Blocked"</span><span><i class="unknown"></i>"Unknown"</span></div>
        </div>
    }
}
fn short(value: &str, max: usize) -> String {
    if value.chars().count() > max {
        format!("{}…", value.chars().take(max - 1).collect::<String>())
    } else {
        value.into()
    }
}
