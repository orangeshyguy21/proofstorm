//! Run the real CI packaging script with build/Docker stubs, then verify the
//! restored checkout through the acceptance runner's real artifact validator.
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use proofstorm_app::artifacts::TestArtifacts;
use serde_json::json;
use sha2::{Digest, Sha256};

fn write(path: &Path, contents: impl AsRef<[u8]>, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn hash(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn registered_checkout(root: &Path) -> proofstorm_app::installation::Installation {
    let development = root.join(".proofstorm-dev");
    let home = development.join("state");
    let resources = development.join("resources");
    let web = development.join("web");
    let cli = development.join("target/debug/proofstorm");
    let mcp = development.join("target/debug/proofstorm-mcp");
    let metadata = proofstorm_app::release::describe();
    for path in [&cli, &mcp] {
        write(
            path,
            format!("#!/bin/sh\ncat <<'METADATA'\n{metadata}\nMETADATA\n"),
            0o700,
        );
    }
    let mut files = BTreeMap::new();
    for (name, contents) in [
        ("release-info.json", metadata.to_string()),
        (
            "controller-source.json",
            json!({"sha256":"a".repeat(64)}).to_string(),
        ),
        (
            "controller-source/Dockerfile.proofstormd",
            "FROM fixture\n".into(),
        ),
    ] {
        let path = resources.join(name);
        write(&path, contents, 0o600);
        files.insert(name, hash(&path));
    }
    write(&web.join("index.html"), "fixture", 0o600);
    write(
        &development.join("owner.json"),
        json!({"source":root}).to_string(),
        0o600,
    );
    write(
        &development.join("build.json"),
        json!({"target":development.join("target")}).to_string(),
        0o600,
    );
    let installation = proofstorm_app::installation::Installation {
        format_version: 2,
        id: "a".repeat(32),
        home: home.clone(),
        api_port: 12345,
        registry_port: 12346,
    };
    write(
        &home.join("installation.json"),
        serde_json::to_vec(&installation).unwrap(),
        0o600,
    );
    write(
        &home.join("checkout-artifacts.json"),
        json!({
            "format_version":1,"installation_id":installation.id,"source":root,
            "resources":resources,"web_dist":web,"cli":cli,"mcp":mcp,
            "cli_sha256":hash(&cli),"mcp_sha256":hash(&mcp),"files":files,"metadata":metadata
        })
        .to_string(),
        0o600,
    );
    // These private runtime files must never be transported with build artifacts.
    for name in [
        "kubeconfig",
        "runtime-owner.json",
        "proofstorm.sqlite3",
        "credentials.json",
    ] {
        write(&home.join(name), "private fixture state", 0o600);
    }
    for name in [
        "proofstorm-acceptance",
        "proofstorm-qualification",
        "proofstorm-xtask",
    ] {
        write(
            &root.join("target/check/debug").join(name),
            "#!/bin/sh\nexit 0\n",
            0o700,
        );
    }
    TestArtifacts::checkout(&home).unwrap();
    installation
}

fn package(root: &Path, base: &Path) {
    let script = root.join("scripts/qualification-build.sh");
    write(
        &script,
        include_str!("../../../scripts/qualification-build.sh"),
        0o700,
    );
    let stubs = base.join("stubs");
    for name in ["just", "cargo"] {
        write(&stubs.join(name), "#!/bin/sh\nexit 0\n", 0o700);
    }
    write(
        &stubs.join("uname"),
        "#!/bin/sh\ncase \"$1\" in -s) echo Linux ;; -m) echo x86_64 ;; *) exit 1 ;; esac\n",
        0o700,
    );
    write(
        &stubs.join("docker"),
        r#"#!/bin/sh
case "$1 $2" in
  'buildx build') exit 0 ;;
  'image save') test "$4" = --output && printf 'fixture controller\n' > "$5" ;;
  *) exit 1 ;;
esac
"#,
        0o700,
    );
    let path = std::env::join_paths(
        std::iter::once(stubs).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output_directory = base.join("artifacts");
    let output = Command::new("bash")
        .arg(&script)
        .arg("linux/amd64")
        .arg(&output_directory)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn restored_host_archive_is_self_contained_without_runtime_state() {
    let scratch = tempfile::tempdir().unwrap();
    let base = scratch.path().canonicalize().unwrap();
    let root = base.join("checkout with spaces");
    let installation = registered_checkout(&root);
    let development = root.join(".proofstorm-dev");
    let home = installation.home.clone();
    package(&root, &base);
    // Remove every original build artifact so omitted archive members cannot be
    // supplied by the build job's filesystem during this restore check.
    fs::rename(&development, base.join("original-development")).unwrap();
    fs::rename(root.join("target"), base.join("original-target")).unwrap();
    let output = Command::new("tar")
        .arg("-xf")
        .arg(base.join("artifacts/host.tar"))
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    TestArtifacts::checkout(&home).unwrap();
    assert_eq!(
        proofstorm_app::installation::Installation::load(&home).unwrap(),
        installation
    );
    for name in ["installation.json", "checkout-artifacts.json"] {
        assert_eq!(
            fs::metadata(home.join(name)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let names: std::collections::BTreeSet<_> = fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        ["installation.json".into(), "checkout-artifacts.json".into()].into()
    );
    for path in [
        development.join("target/debug/proofstorm"),
        development.join("target/debug/proofstorm-mcp"),
    ] {
        assert_ne!(fs::metadata(path).unwrap().permissions().mode() & 0o111, 0);
    }
}
