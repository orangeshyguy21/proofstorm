use std::{env, fmt::Write, fs, path::PathBuf};
fn main() {
    let assets = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("../proofstorm-web/dist");
    println!("cargo:rerun-if-changed={}", assets.display());
    let mut files = fs::read_dir(&assets)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    files.sort();
    let mut code = String::from("pub static WEB_ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for path in files {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let mime = match path.extension().and_then(|e| e.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript",
            Some("wasm") => "application/wasm",
            Some("css") => "text/css",
            _ => continue,
        };
        let path = path.to_str().expect("UTF-8 asset path");
        writeln!(code, "({name:?}, {mime:?}, include_bytes!({path:?})),").expect("asset entry");
    }
    code.push_str("];\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR").expect("output directory")).join("web_assets.rs"),
        code,
    )
    .expect("write asset manifest");
}
