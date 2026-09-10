use std::{env, fmt::Write, fs, path::PathBuf};
fn main() {
    for key in [
        "PROOFSTORM_WEB_DIST",
        "PROOFSTORM_REQUIRE_WEB_ASSETS",
        "PROOFSTORM_BUILD_REVISION",
        "PROOFSTORM_BUILD_SOURCE_SHA256",
        "PROOFSTORM_CONTROLLER_RECEIPT",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let controller = env::var_os("PROOFSTORM_CONTROLLER_RECEIPT").map_or_else(
        || b"null".to_vec(),
        |path| {
            let path = PathBuf::from(path);
            println!("cargo:rerun-if-changed={}", path.display());
            fs::read(path).expect("read explicit controller build receipt")
        },
    );
    fs::write(
        PathBuf::from(env::var("OUT_DIR").expect("output directory"))
            .join("controller_receipt.json"),
        controller,
    )
    .expect("write embedded controller receipt");
    for key in [
        "PROOFSTORM_BUILD_REVISION",
        "PROOFSTORM_BUILD_SOURCE_SHA256",
    ] {
        println!(
            "cargo:rustc-env={key}={}",
            env::var(key).unwrap_or_else(|_| "unknown".into())
        );
    }
    println!(
        "cargo:rustc-env=PROOFSTORM_BUILD_TARGET={}",
        env::var("TARGET").expect("build target")
    );
    println!(
        "cargo:rustc-env=PROOFSTORM_BUILD_PROFILE={}",
        env::var("PROFILE").expect("build profile")
    );
    let assets = env::var_os("PROOFSTORM_WEB_DIST").map_or_else(
        || {
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest directory"))
                .join("../proofstorm-web/dist")
        },
        PathBuf::from,
    );
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
    if env::var("PROOFSTORM_REQUIRE_WEB_ASSETS").as_deref() == Ok("1") {
        for extension in ["html", "js", "wasm", "css"] {
            assert!(
                files.iter().any(
                    |p| p.extension().and_then(|e| e.to_str()) == Some(extension)
                        && fs::metadata(p).is_ok_and(|m| m.len() > 0)
                ),
                "release requires nonempty {extension} web assets"
            );
        }
        assert!(
            assets.join("index.html").is_file(),
            "release requires index.html"
        );
    }
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
