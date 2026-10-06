//! Canvas coordinates are independent of screen pixels and saved node positions.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

pub const MIN_ZOOM: f64 = 0.15;
pub const MAX_ZOOM: f64 = 6.0;

/// Keep the same world point beneath the pointer, even after panning or fitting.
pub fn zoom_at(
    current: f64,
    pan: (f64, f64),
    center: (f64, f64),
    pointer: (f64, f64),
    requested: f64,
) -> Option<(f64, (f64, f64))> {
    if ![
        current, pan.0, pan.1, center.0, center.1, pointer.0, pointer.1, requested,
    ]
    .into_iter()
    .all(f64::is_finite)
        || current <= 0.0
        || requested <= 0.0
    {
        return None;
    }
    let zoom = requested.clamp(MIN_ZOOM, MAX_ZOOM);
    let ratio = zoom / current;
    let anchor = (pointer.0 - center.0, pointer.1 - center.1);
    Some((
        zoom,
        (
            anchor.0 - (anchor.0 - pan.0) * ratio,
            anchor.1 - (anchor.1 - pan.1) * ratio,
        ),
    ))
}

/// Browsers can report pixels, lines, or pages, including fractional deltas.
pub fn wheel_pixels(delta: f64, mode: u32, page_pixels: f64) -> f64 {
    delta
        * match mode {
            1 => 16.0,
            2 => page_pixels,
            _ => 1.0,
        }
}

pub fn wheel_zoom(current: f64, pixels: f64) -> f64 {
    current * (-pixels * 0.01).clamp(-2.0, 2.0).exp()
}

/// `WebKit` scale is cumulative from gesture start, not an incremental multiplier.
#[derive(Default)]
pub struct Pinch {
    scale: Option<f64>,
}

impl Pinch {
    pub fn start(&mut self, scale: f64) -> bool {
        self.scale = (scale.is_finite() && scale > 0.0).then_some(scale);
        self.active()
    }

    pub fn active(&self) -> bool {
        self.scale.is_some()
    }

    pub fn change(&mut self, scale: f64) -> Option<f64> {
        let previous = self.scale?;
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        self.scale = Some(scale);
        Some(scale / previous)
    }

    pub fn end(&mut self) {
        self.scale = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn zoom_keeps_pointer_over_the_same_world_point_after_pan_and_fit() {
        let center = (750.0, -200.0);
        let pan = (120.0, -45.0);
        let pointer = (430.0, 185.0);
        let world = (
            center.0 + (pointer.0 - center.0 - pan.0) / 1.7,
            center.1 + (pointer.1 - center.1 - pan.1) / 1.7,
        );
        for requested in [0.001, 0.7, 2.3, 100.0] {
            let (zoom, next) = zoom_at(1.7, pan, center, pointer, requested).unwrap();
            close(center.0 + next.0 + (world.0 - center.0) * zoom, pointer.0);
            close(center.1 + next.1 + (world.1 - center.1) * zoom, pointer.1);
            assert!((MIN_ZOOM..=MAX_ZOOM).contains(&zoom));
        }
    }

    #[test]
    fn zoom_at_a_limit_does_not_drift_and_reverses_immediately() {
        let pan = (40.0, -20.0);
        let (zoom, next) = zoom_at(MAX_ZOOM, pan, (0.0, 0.0), (100.0, 80.0), 12.0).unwrap();
        close(zoom, MAX_ZOOM);
        close(next.0, pan.0);
        close(next.1, pan.1);
        assert!(wheel_zoom(zoom, 2.0) < zoom);
    }

    #[test]
    fn wheel_units_and_fractional_movements_are_preserved() {
        close(wheel_pixels(0.25, 0, 800.0), 0.25);
        close(wheel_pixels(-2.0, 1, 800.0), -32.0);
        close(wheel_pixels(0.5, 2, 800.0), 400.0);
        close(wheel_zoom(wheel_zoom(1.0, -5.0), 5.0), 1.0);
    }

    #[test]
    fn invalid_zoom_input_cannot_poison_the_camera() {
        for value in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            assert!(zoom_at(1.0, (0.0, 0.0), (0.0, 0.0), (0.0, 0.0), value).is_none());
        }
    }

    #[test]
    fn webkit_scale_is_incremental_and_resets_between_gestures() {
        let mut pinch = Pinch::default();
        assert!(pinch.change(1.2).is_none());
        assert!(pinch.start(1.0));
        close(pinch.change(1.2).unwrap(), 1.2);
        close(pinch.change(1.5).unwrap(), 1.25);
        assert!(pinch.change(f64::NAN).is_none());
        close(pinch.change(1.2).unwrap(), 0.8);
        pinch.end();
        assert!(!pinch.active());
        assert!(pinch.change(1.6).is_none());
        assert!(pinch.start(1.0));
        close(pinch.change(1.1).unwrap(), 1.1);
    }
}
