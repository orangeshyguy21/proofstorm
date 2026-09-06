#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod client;
mod model;

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("Build the browser app with `make web`, then run `proofstorm serve`.");
}
