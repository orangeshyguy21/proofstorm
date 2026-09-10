use super::*;
use crate::release::bundle::tests::Bundle;

struct Inputs {
    root: tempfile::TempDir,
    info: Value,
    provenance: Value,
}

impl Inputs {
    fn new() -> Self {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let bundle = Bundle::new("x86_64-unknown-linux-gnu", "alpha");
        let source = root.path().join("source");
        directory(&source.join("charts")).unwrap();
        copy_tree(
            &bundle.root().join("chart").canonicalize().unwrap(),
            &source.join("charts/proofstorm"),
        )
        .unwrap();
        fs::copy(bundle.root().join("LICENSE"), source.join("LICENSE")).unwrap();
        directory(&source.join("tools")).unwrap();
        fs::copy(
            bundle.root().join("tools/versions.env"),
            source.join("tools/versions.env"),
        )
        .unwrap();
        fs::write(
            source.join("charts/proofstorm/Chart.yaml"),
            "version: 0.1.0-alpha.1\nappVersion: 0.1.0-alpha.1\n",
        )
        .unwrap();
        let info = bundle.info.clone();
        let provenance = json!({"revision": info["source_revision"], "sha256": info["source_sha256"], "dirty": true});
        let result = Self {
            root,
            info,
            provenance,
        };
        result.binaries();
        result
    }

    fn binaries(&self) {
        directory(&self.root.path().join("binaries")).unwrap();
        for (name, flag) in [
            ("proofstorm", "release-info"),
            ("proofstorm-mcp", "--release-info"),
        ] {
            let path = self.root.path().join("binaries").join(name);
            let payload = self.info.to_string().replace('\'', "'\\''");
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\n[ \"$1\" = '{flag}' ] || exit 97\nprintf '%s\\n' '{payload}'\n"
                ),
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn package(&self, name: &str, development: bool) -> Result<Value> {
        package(
            &self.root.path().join("source"),
            &self.root.path().join("binaries"),
            &self.provenance,
            &self.root.path().join(name),
            development,
        )
    }
}

#[test]
fn alpha_and_development_bundles_use_existing_names_and_roundtrip() {
    let inputs = Inputs::new();
    for development in [false, true] {
        let label = if development { "dev" } else { "alpha" };
        let first = inputs.package(label, development).unwrap();
        let second = inputs
            .package(&format!("{label}-again"), development)
            .unwrap();
        assert_eq!(first["sha256"], second["sha256"]);
        assert_eq!(first["release_ready"], false);
        let archive = Path::new(first["archive"].as_str().unwrap());
        let suffix = if development {
            "-dev-debug-bbbbbbbbbbbb"
        } else {
            ""
        };
        assert_eq!(
            archive.file_name().unwrap(),
            format!("proofstorm-0.1.0-alpha.1{suffix}-x86_64-unknown-linux-gnu.tar.gz").as_str()
        );
        let receipt = archive::extract(
            archive,
            &inputs.root.path().join(format!("{label}-unpacked")),
        )
        .unwrap();
        assert_eq!(
            receipt["channel"],
            if development { "development" } else { "alpha" }
        );
    }
}

#[test]
fn binary_mismatch_source_mismatch_and_unready_release_fail_without_archives() {
    let mut inputs = Inputs::new();
    let mcp = inputs.root.path().join("binaries/proofstorm-mcp");
    fs::write(&mcp, "#!/bin/sh\nprintf '%s' '{}'\n").unwrap();
    assert!(
        inputs
            .package("mismatch", false)
            .unwrap_err()
            .to_string()
            .contains("same release inputs")
    );
    inputs.binaries();
    inputs.provenance["sha256"] = json!("f".repeat(64));
    assert!(
        inputs
            .package("source-mismatch", false)
            .unwrap_err()
            .to_string()
            .contains("provenance mismatch")
    );
    inputs.provenance["sha256"] = inputs.info["source_sha256"].clone();
    inputs.info["version"] = json!("0.1.0");
    inputs.info["controller"]["metadata"]["version"] = json!("0.1.0");
    inputs.binaries();
    assert!(
        inputs
            .package("unready", false)
            .unwrap_err()
            .to_string()
            .contains("release blocked")
    );
    for name in ["mismatch", "source-mismatch", "unready"] {
        assert_eq!(
            fs::read_dir(inputs.root.path().join(name)).unwrap().count(),
            0
        );
    }
}

#[test]
fn missing_assets_chart_versions_and_tool_pins_are_checked_before_publication() {
    for name in [
        "charts/proofstorm/crds/proofstorm.dev_proofstormlabs.yaml",
        "charts/proofstorm/Chart.yaml",
        "tools/versions.env",
    ] {
        let inputs = Inputs::new();
        let path = inputs.root.path().join("source").join(name);
        if name.ends_with("proofstormlabs.yaml") {
            fs::remove_file(path).unwrap();
        } else {
            fs::write(path, "wrong\n").unwrap();
        }
        assert!(inputs.package("output", false).is_err());
        assert_eq!(
            fs::read_dir(inputs.root.path().join("output"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn linked_inputs_and_output_inside_inputs_are_refused() {
    let inputs = Inputs::new();
    let source = inputs.root.path().join("source");
    let binaries = inputs.root.path().join("binaries");
    assert!(
        package(
            &source,
            &binaries,
            &inputs.provenance,
            &source.join("output"),
            false
        )
        .is_err()
    );
    assert!(!source.join("output").exists());
    fs::remove_file(binaries.join("proofstorm")).unwrap();
    std::os::unix::fs::symlink(binaries.join("proofstorm-mcp"), binaries.join("proofstorm"))
        .unwrap();
    assert!(inputs.package("output", false).is_err());
    assert_eq!(
        fs::read_dir(inputs.root.path().join("output"))
            .unwrap()
            .count(),
        0
    );
}
