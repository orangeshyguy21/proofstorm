use leptos::prelude::*;

#[derive(Clone, Copy)]
struct ObservationClock(RwSignal<i64>);

// JavaScript dates fit comfortably in i64 seconds; discard subsecond precision.
#[allow(clippy::cast_possible_truncation)]
fn seconds(milliseconds: f64) -> i64 {
    (milliseconds / 1000.0).floor() as i64
}
pub fn parse_timestamp(value: &str) -> Option<i64> {
    let milliseconds = js_sys::Date::parse(value);
    milliseconds.is_finite().then(|| seconds(milliseconds))
}
pub fn provide_clock() {
    let now = RwSignal::new(seconds(js_sys::Date::now()));
    provide_context(ObservationClock(now));
    let timer = gloo_timers::callback::Interval::new(1000, move || {
        now.set(seconds(js_sys::Date::now()));
    });
    let _timer = StoredValue::new_local(timer);
}
#[component]
pub fn UpdatedAgo(unix: i64, #[prop(default = "Updated")] label: &'static str) -> impl IntoView {
    let clock = expect_context::<ObservationClock>();
    view! {<span>{move || if unix > 0 {
        format!("{label} {}", crate::model::elapsed_time(unix, clock.0.get()))
    } else { "Update time unavailable".into() }}</span>}
}
