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
#[derive(Clone, Copy)]
struct ObservationConnection {
    connected: RwSignal<bool>,
    failed: RwSignal<bool>,
}
pub fn provide_connection(connected: RwSignal<bool>, failed: RwSignal<bool>) {
    provide_context(ObservationConnection { connected, failed });
}
pub fn observation_status(unix: i64, failed: bool, max_age: i64) -> crate::model::Freshness {
    let connection = expect_context::<ObservationConnection>();
    crate::model::observation_freshness(
        unix,
        now(),
        failed || connection.failed.get(),
        connection.connected.get(),
        max_age,
    )
}
#[component]
pub fn FreshnessStatus(
    #[prop(into)] unix: Signal<i64>,
    #[prop(into, default = false.into())] failed: Signal<bool>,
    #[prop(default = crate::model::OBSERVATION_MAX_AGE)] max_age: i64,
) -> impl IntoView {
    // The clock still detects stalled updates, but the DOM only changes when
    // the status changes, never once per second as a relative timestamp did.
    let status = Memo::new(move |_| observation_status(unix.get(), failed.get(), max_age));
    view! {<span class=move ||format!("freshness-status {}",status.get().class()) role="status"><i aria-hidden="true"></i>{move ||status.get().label()}</span>}
}
pub fn now() -> i64 {
    expect_context::<ObservationClock>().0.get()
}
