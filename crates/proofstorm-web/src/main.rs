#[cfg(target_arch = "wasm32")]
mod app;
mod canvas_model;
#[cfg(target_arch = "wasm32")]
mod client;
#[cfg(target_arch = "wasm32")]
mod edges;
#[cfg(target_arch = "wasm32")]
mod freshness;
#[cfg(target_arch = "wasm32")]
mod graph;
#[cfg(target_arch = "wasm32")]
mod gui;
#[cfg(target_arch = "wasm32")]
mod inspector;
#[cfg(target_arch = "wasm32")]
mod lab_view;
mod model;
#[cfg(target_arch = "wasm32")]
mod motion;
#[cfg(target_arch = "wasm32")]
mod relationship_panel;
mod relationships;
#[cfg(target_arch = "wasm32")]
mod system;
#[cfg(target_arch = "wasm32")]
mod theme;

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    // Authentication runs before mounting. Leptos initializes its own executor
    // during mount, so this first future must use the browser executor directly.
    wasm_bindgen_futures::spawn_local(async {
        let seed = gui::bootstrap().await;
        leptos::mount::mount_to_body(move || {
            leptos::prelude::provide_context(seed.clone());
            app::App()
        });
    });
}
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("Build the browser app with `just web`, then run `proofstorm serve`.");
}
