use leptos::prelude::*;
use proofstorm_view::AppRoute;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

/// Keep the existing view selections in sync with browser history.
#[derive(Clone, Copy)]
pub struct Navigation {
    pub system: RwSignal<bool>,
    pub catalog: RwSignal<bool>,
    pub builds: RwSignal<bool>,
    pub cell: RwSignal<String>,
    pub component: RwSignal<String>,
    replace: StoredValue<bool>,
}

impl Navigation {
    pub fn new() -> Self {
        let navigation = Self {
            system: RwSignal::new(false),
            catalog: RwSignal::new(false),
            builds: RwSignal::new(false),
            cell: RwSignal::new(String::new()),
            component: RwSignal::new(String::new()),
            replace: StoredValue::new(true),
        };
        let window = web_sys::window().expect("browser window");
        navigation.restore(&window.location().pathname().unwrap_or_default());
        let on_popstate = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            if let Some(window) = web_sys::window() {
                navigation.restore(&window.location().pathname().unwrap_or_default());
            }
        });
        let _ = window
            .add_event_listener_with_callback("popstate", on_popstate.as_ref().unchecked_ref());
        let listener = StoredValue::new_local((window, on_popstate));
        on_cleanup(move || {
            listener.with_value(|(window, callback)| {
                let _ = window.remove_event_listener_with_callback(
                    "popstate",
                    callback.as_ref().unchecked_ref(),
                );
            });
        });
        Effect::new(move |_| {
            let path = navigation.route().path();
            let replace = navigation.replace.get_value();
            navigation.replace.set_value(false);
            if let Some(window) = web_sys::window() {
                if window.location().pathname().ok().as_deref() == Some(&path) {
                    return;
                }
                if let Ok(history) = window.history() {
                    let url = format!("{path}{}", window.location().search().unwrap_or_default());
                    if replace {
                        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url));
                    } else {
                        let _ = history.push_state_with_url(&JsValue::NULL, "", Some(&url));
                    }
                }
            }
        });
        navigation
    }

    /// Automatic selection or removal repairs the current entry, not Back history.
    pub fn replace_next(&self) {
        self.replace.set_value(true);
    }

    fn restore(self, path: &str) {
        self.replace_next();
        let route = AppRoute::parse(path).unwrap_or_default();
        self.system.set(route == AppRoute::System);
        self.catalog
            .set(matches!(route, AppRoute::Catalog | AppRoute::Builds));
        self.builds.set(route == AppRoute::Builds);
        match route {
            AppRoute::Cell { id, component } => {
                self.cell.set(id);
                self.component.set(component);
            }
            AppRoute::Home => {
                self.cell.set(String::new());
                self.component.set(String::new());
            }
            _ => {}
        }
    }

    fn route(self) -> AppRoute {
        // Track hidden selections too, so automatic background repairs consume
        // their replace flag before the user's next navigation.
        let system = self.system.get();
        let catalog = self.catalog.get();
        let builds = self.builds.get();
        let id = self.cell.get();
        let component = self.component.get();
        if system {
            AppRoute::System
        } else if catalog {
            if builds {
                AppRoute::Builds
            } else {
                AppRoute::Catalog
            }
        } else if id.is_empty() {
            AppRoute::Home
        } else {
            AppRoute::Cell { id, component }
        }
    }
}
