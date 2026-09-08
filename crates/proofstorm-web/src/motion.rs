use leptos::prelude::*;
/// Alternate animation names only when displayed values change, never on refresh alone.
pub fn pulse<T: Clone + PartialEq + Send + Sync + 'static>(
    value: Memo<T>,
) -> RwSignal<&'static str> {
    let previous = StoredValue::new(None::<T>);
    let alternate = StoredValue::new(false);
    let class = RwSignal::new("");
    Effect::new(move |_| {
        let next = value.get();
        if previous
            .get_value()
            .as_ref()
            .is_some_and(|old| old != &next)
        {
            alternate.update_value(|v| *v = !*v);
            class.set(if alternate.get_value() {
                "value-change-a"
            } else {
                "value-change-b"
            });
        }
        previous.set_value(Some(next));
    });
    class
}
