use std::{fs, path::Path, process::Command};

#[test]
fn strict_release_build_rejects_missing_and_empty_frontend_payloads() {
    let root = tempfile::tempdir().unwrap();
    let build_script = root.path().join("build-script");
    let compiled = Command::new("rustc")
        .args(["--edition=2024"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs"))
        .arg("-o")
        .arg(&build_script)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let assets = root.path().join("assets");
    fs::create_dir(&assets).unwrap();
    let run = || {
        Command::new(&build_script)
            .env("PROOFSTORM_REQUIRE_WEB_ASSETS", "1")
            .env("PROOFSTORM_WEB_DIST", &assets)
            .env("OUT_DIR", root.path())
            .env("TARGET", "aarch64-apple-darwin")
            .env("PROFILE", "release")
            .output()
            .unwrap()
    };
    assert!(!run().status.success());
    for name in ["index.html", "app.js", "app.wasm", "style.css"] {
        fs::write(assets.join(name), b"nonempty fixture").unwrap();
    }
    assert!(run().status.success());
    fs::write(assets.join("app.wasm"), b"").unwrap();
    assert!(!run().status.success());
    fs::remove_file(assets.join("app.wasm")).unwrap();
    assert!(!run().status.success());
}
