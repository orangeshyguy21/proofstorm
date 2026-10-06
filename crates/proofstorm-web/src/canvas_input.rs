use crate::canvas_navigation::{Pinch, wheel_pixels, wheel_zoom, zoom_at};
use leptos::{attr::Attribute, ev, prelude::*};
use wasm_bindgen::JsCast;

fn non_passive<E: wasm_bindgen::convert::FromWasmAbi>(name: &'static str) -> ev::Custom<E> {
    let mut event = ev::Custom::new(name);
    event.options_mut().set_passive(false);
    event
}

fn inverse(event: &web_sys::Event) -> Option<web_sys::SvgMatrix> {
    event
        .current_target()?
        .dyn_into::<web_sys::SvgGraphicsElement>()
        .ok()?
        .get_screen_ctm()?
        .inverse()
        .ok()
}

fn vector(matrix: &web_sys::SvgMatrix, x: f64, y: f64) -> (f64, f64) {
    (
        f64::from(matrix.a()) * x + f64::from(matrix.c()) * y,
        f64::from(matrix.b()) * x + f64::from(matrix.d()) * y,
    )
}

fn number(event: &web_sys::Event, key: &str) -> Option<f64> {
    js_sys::Reflect::get(event, &key.into())
        .ok()?
        .as_f64()
        .filter(|value| value.is_finite())
}

/// Direct, non-passive SVG listeners are removed by Leptos when the graph unmounts.
/// No document-wide gesture handling: page zoom and other panels retain their inputs.
#[allow(
    clippy::too_many_lines,
    reason = "gesture listeners share one canvas interaction state"
)]
pub fn handlers(
    zoom: RwSignal<f64>,
    pan: RwSignal<(f64, f64)>,
    camera: RwSignal<(f64, f64, f64, f64)>,
    interacted: RwSignal<bool>,
    dragging: Memo<bool>,
) -> impl Attribute {
    let pinch = StoredValue::new(Pinch::default());
    let apply_zoom = move |event: &web_sys::Event, x: f64, y: f64, requested: f64| {
        let Some(matrix) = inverse(event) else { return };
        let point = vector(&matrix, x, y);
        let pointer = (
            point.0 + f64::from(matrix.e()),
            point.1 + f64::from(matrix.f()),
        );
        let (left, top, width, height) = camera.get_untracked();
        let center = (left + width / 2.0, top + height / 2.0);
        if let Some((next_zoom, next_pan)) = zoom_at(
            zoom.get_untracked(),
            pan.get_untracked(),
            center,
            pointer,
            requested,
        ) {
            interacted.set(true);
            batch(move || {
                zoom.set(next_zoom);
                pan.set(next_pan);
            });
        }
    };
    (
        ev::on(non_passive("wheel"), move |event: web_sys::WheelEvent| {
            // Never apply our transform in addition to uncancelable browser zoom.
            if !event.cancelable() {
                return;
            }
            event.prevent_default();
            if dragging.get_untracked() || pinch.with_value(Pinch::active) {
                return;
            }
            let Some(svg) = event
                .current_target()
                .and_then(|el| el.dyn_into::<web_sys::Element>().ok())
            else {
                return;
            };
            let dx = wheel_pixels(
                event.delta_x(),
                event.delta_mode(),
                f64::from(svg.client_width()),
            );
            let dy = wheel_pixels(
                event.delta_y(),
                event.delta_mode(),
                f64::from(svg.client_height()),
            );
            if !dx.is_finite() || !dy.is_finite() {
                return;
            }
            if event.ctrl_key() {
                apply_zoom(
                    &event,
                    f64::from(event.client_x()),
                    f64::from(event.client_y()),
                    wheel_zoom(zoom.get_untracked(), dy),
                );
            } else if let Some(matrix) = inverse(&event) {
                let delta = vector(&matrix, dx, dy);
                interacted.set(true);
                pan.update(|p| {
                    p.0 -= delta.0;
                    p.1 -= delta.1;
                });
            }
        }),
        ev::on(non_passive("gesturestart"), move |event: web_sys::Event| {
            pinch.update_value(Pinch::end);
            if !event.cancelable() || dragging.get_untracked() {
                return;
            }
            let Some(scale) = number(&event, "scale") else {
                return;
            };
            if pinch.try_update_value(|p| p.start(scale)).unwrap_or(false) {
                event.prevent_default();
                interacted.set(true);
            }
        }),
        ev::on(
            non_passive("gesturechange"),
            move |event: web_sys::Event| {
                if !event.cancelable() || !pinch.with_value(Pinch::active) {
                    return;
                }
                event.prevent_default();
                if dragging.get_untracked() {
                    pinch.update_value(Pinch::end);
                    return;
                }
                let Some((scale, (x, y))) = number(&event, "scale")
                    .zip(number(&event, "clientX").zip(number(&event, "clientY")))
                else {
                    return;
                };
                if let Some(ratio) = pinch.try_update_value(|p| p.change(scale)).flatten() {
                    apply_zoom(&event, x, y, zoom.get_untracked() * ratio);
                }
            },
        ),
        ev::on(non_passive("gestureend"), move |event: web_sys::Event| {
            if pinch.with_value(Pinch::active) {
                event.prevent_default();
            }
            pinch.update_value(Pinch::end);
        }),
        // A gesture that leaves the canvas must not suppress subsequent wheel input.
        ev::on(ev::pointerleave, move |_| pinch.update_value(Pinch::end)),
    )
}
