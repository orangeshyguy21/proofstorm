use leptos::prelude::*;

#[component]
pub fn ThemePicker() -> impl IntoView {
    let theme = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item("proofstorm.theme").ok().flatten())
        .filter(|theme| matches!(theme.as_str(), "dark" | "light"))
        .unwrap_or_else(|| "system".into());
    view! {
        <select class="theme-picker" aria-label="Color theme" prop:value=theme on:change=move |event| {
            let theme = event_target_value(&event);
            if let Some(window) = web_sys::window() {
                if let Some(root) = window.document().and_then(|d| d.document_element()) {
                    if theme == "system" { let _ = root.remove_attribute("data-theme"); }
                    else { let _ = root.set_attribute("data-theme", &theme); }
                }
                if let Ok(Some(storage)) = window.local_storage() { let _ = storage.set_item("proofstorm.theme", &theme); }
            }
        }><option value="system">"System theme"</option><option value="dark">"Dark"</option><option value="light">"Light"</option></select>
    }
}
