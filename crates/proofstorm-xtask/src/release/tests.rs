use super::*;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../tests/fixtures/release-info.json")).unwrap()
}

fn rejects(pointer: &str, value: Value, alpha: bool, message: &str) {
    let mut info = fixture();
    *info.pointer_mut(pointer).unwrap() = value;
    let error = validate(&info, alpha).unwrap_err().to_string();
    assert!(error.contains(message), "{pointer}: {error}");
}

#[test]
fn both_platforms_pass_without_claiming_release_readiness() {
    for (target, platform) in [
        ("x86_64-unknown-linux-gnu", "linux/amd64"),
        ("aarch64-apple-darwin", "linux/arm64"),
    ] {
        let mut info = fixture();
        info["target"] = json!(target);
        info["bootstrap_tools"]["target"] = json!(target);
        info["controller"]["platform"] = json!(platform);
        for alpha in [false, true] {
            let receipt = validate(&info, alpha).unwrap();
            assert_eq!(receipt["platform"], platform);
            assert_eq!(receipt["metadata_valid"], true);
            assert_eq!(receipt["alpha_requirements_checked"], alpha);
            assert_eq!(receipt["release_ready"], false);
            assert_eq!(receipt["unverified"].as_array().unwrap().len(), 3);
        }
    }
}

#[test]
fn publication_mapping_preserves_digests_and_unverified_status() {
    let images = image_inventory(&fixture()).unwrap();
    assert_eq!(
        images[0]["published_source"],
        format!(
            "ghcr.io/orangeshyguy21/proofstorm/custom@sha256:{}",
            "d".repeat(64)
        )
    );
    assert_eq!(
        images[1]["published_source"],
        format!("docker.io/library/busybox@sha256:{}", "e".repeat(64))
    );
    assert_eq!(images[2]["published_source"], images[2]["image"]);
    for image in images {
        assert_eq!(image["availability_verified"], false);
        assert_eq!(image["verified_platforms"], json!([]));
    }
}

#[test]
fn development_can_lack_publication_but_alpha_cannot() {
    let mut info = fixture();
    info["image_publication"] = json!("{}");
    let receipt = validate(&info, false).unwrap();
    assert!(receipt["workload_images"][0]["published_source"].is_null());
    assert!(
        validate(&info, true)
            .unwrap_err()
            .to_string()
            .contains("published workload")
    );
    for key in ["controller", "bootstrap_tools"] {
        let mut info = fixture();
        info.as_object_mut().unwrap().remove(key);
        assert!(validate(&info, false).is_ok());
        assert!(validate(&info, true).is_err());
    }
}

#[test]
fn invalid_targets_versions_profiles_and_formats_fail_closed() {
    rejects("/format_version", json!(2), false, "metadata format");
    rejects("/format_version", json!(true), false, "metadata format");
    rejects(
        "/target",
        json!("x86_64-apple-darwin"),
        false,
        "unsupported bundle target",
    );
    rejects("/build_profile", json!("custom"), false, "build profile");
    for version in ["", "../escape", "a b", "v1\n", "α.1"] {
        rejects("/version", json!(version), false, "unsafe version");
    }
    for version in ["0.1.0", "0.1.0-alpha.", "0.1.0-alpha.1-dev", "0.1-alpha.1"] {
        assert!(!alpha_version(version));
    }
    assert!(alpha_version("0.1.0-alpha.1"));
    let mut info = fixture();
    info["version"] = json!("0.1.0");
    info["controller"]["metadata"]["version"] = info["version"].clone();
    assert!(
        validate(&info, true)
            .unwrap_err()
            .to_string()
            .contains("alpha version")
    );
}

#[test]
fn all_embedded_asset_types_and_receipts_are_required() {
    for (index, name) in ["index.html", ".js", ".wasm", ".css"].iter().enumerate() {
        let mut info = fixture();
        info["web_assets"].as_array_mut().unwrap().remove(index);
        assert!(
            validate(&info, false)
                .unwrap_err()
                .to_string()
                .contains(name)
        );
    }
    for size in [json!(0), json!(-1), json!(true), json!("1"), json!(1.5)] {
        rejects("/web_assets/0/size", size, false, "asset receipt");
    }
    for digest in ["c".repeat(63), "C".repeat(64), "z".repeat(64)] {
        rejects(
            "/web_assets/0/sha256",
            json!(digest),
            false,
            "asset receipt",
        );
    }
}

#[test]
fn mutable_and_malformed_images_are_rejected() {
    for image in [
        "example/app:latest".into(),
        format!("@sha256:{}", "d".repeat(64)),
        format!("example/app@sha256:{}", "D".repeat(64)),
        format!("example/app other@sha256:{}", "d".repeat(64)),
        format!("example/app@other@sha256:{}", "d".repeat(64)),
    ] {
        rejects("/workload_images/0", json!(image), false, "not pinned");
    }
    rejects("/workload_images/0", json!(42), false, "workload image");
}

#[test]
fn publication_configuration_is_validated() {
    for namespace in [
        "docker.io/owner/repo",
        "ghcr.io/UPPER/repo",
        "ghcr.io/owner",
        "ghcr.io/owner/repo other",
        "ghcr.io/owner/repo@tag",
    ] {
        rejects(
            "/image_publication",
            json!(json!({"namespace": namespace}).to_string()),
            false,
            "publication namespace",
        );
    }
    rejects("/image_publication", json!("["), false, "publication JSON");
    rejects("/image_publication", json!("[]"), false, "object");
    rejects("/image_publication", json!({}), false, "JSON string");
}

#[test]
fn controller_contract_and_tools_cannot_be_mismatched_even_in_development() {
    rejects(
        "/controller/platform",
        json!("linux/arm64"),
        false,
        "controller platform",
    );
    rejects(
        "/controller/metadata/version",
        json!("0.2.0"),
        false,
        "runtime contract",
    );
    rejects(
        "/controller/metadata/runtime_contract_sha256",
        json!("2".repeat(64)),
        false,
        "runtime contract",
    );
    rejects(
        "/runtime_contract_sha256",
        json!(""),
        false,
        "runtime contract",
    );
    rejects(
        "/bootstrap_tools/target",
        json!("foreign"),
        false,
        "tool target mismatch",
    );
    rejects(
        "/bootstrap_tools/tools",
        json!([]),
        true,
        "pinned bootstrap tools",
    );
    rejects(
        "/bootstrap_tools/tools",
        json!("not a list"),
        true,
        "pinned bootstrap tools",
    );
    for image in [
        "ghcr.io/owner/controller:latest".into(),
        format!("docker.io/owner/controller@sha256:{}", "2".repeat(64)),
    ] {
        rejects(
            "/controller/image",
            json!(image),
            true,
            "digest-pinned controller",
        );
    }
}

#[test]
fn missing_fields_and_wrong_json_shapes_are_errors_not_panics() {
    for info in [
        Value::Null,
        json!([]),
        json!(true),
        json!("metadata"),
        json!(1),
    ] {
        assert!(validate(&info, false).is_err());
    }
    for key in [
        "format_version",
        "target",
        "version",
        "build_profile",
        "web_assets",
        "workload_images",
    ] {
        let mut info = fixture();
        info.as_object_mut().unwrap().remove(key);
        assert!(validate(&info, false).is_err(), "missing {key}");
        info[key] = json!(true);
        assert!(validate(&info, false).is_err(), "boolean {key}");
    }
}
