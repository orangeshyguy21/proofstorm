//! Explicit Docker/registry check; never runs in ordinary unit tests or CI.
use super::*;

#[test]
#[ignore = "requires Docker Desktop/Buildx and a public registry connection"]
fn public_manifest_reads_do_not_invoke_ambient_credential_helpers() -> Result<()> {
    const TEST: &str = "bootstrap::registry::live_tests::public_manifest_reads_do_not_invoke_ambient_credential_helpers";
    if let Some(home) = std::env::var_os("REGISTRY_TEST_CHILD_HOME") {
        let home = Path::new(&home);
        let installation = Installation::initialize(home, None, None)?;
        let registry = Registry::new(&installation)?;
        let image = std::env::var("REGISTRY_TEST_IMAGE")?;
        let marker = std::env::var_os("REGISTRY_HELPER_MARKER").context("helper marker")?;
        let marker = Path::new(&marker);
        let args = ["buildx", "imagetools", "inspect", "--raw", &image];
        // Prove the fixture is sensitive to the original bug. This helper is a
        // local tripwire, never Docker Desktop or the system credential store.
        assert!(docker(home, &args, 60).is_err());
        assert!(
            marker.is_file(),
            "ambient credential helper was not reached"
        );
        fs::remove_file(marker)?;
        for manifest in [
            registry.run(home, &args, 60)?,
            registry.image_preparation(home, &args, 60)?,
        ] {
            assert_eq!(
                serde_json::from_str::<Value>(&manifest)?["schemaVersion"],
                2
            );
            assert!(!marker.exists(), "isolated command invoked ambient helper");
        }
        return Ok(());
    }

    let root = tempfile::tempdir()?;
    let home = root.path().join("state");
    fs::create_dir(&home)?;
    let endpoint: Value = serde_json::from_str(&docker(
        &home,
        &[
            "context",
            "inspect",
            "--format",
            "{{json .Endpoints.docker}}",
        ],
        15,
    )?)?;
    let plugins: Value = serde_json::from_str(&docker(
        &home,
        &["info", "--format", "{{json .ClientInfo.Plugins}}"],
        15,
    )?)?;
    let buildx = plugins
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["Name"] == "buildx"))
        .and_then(|entry| entry["Path"].as_str())
        .context("Docker Buildx plugin")?;
    let ambient = root.path().join("ambient");
    fs::create_dir_all(ambient.join("cli-plugins"))?;
    std::os::unix::fs::symlink(
        Path::new(buildx).canonicalize()?,
        ambient.join("cli-plugins/docker-buildx"),
    )?;
    fs::write(
        ambient.join("config.json"),
        br#"{"credsStore":"proofstorm-test-tripwire"}"#,
    )?;
    let helper = root
        .path()
        .join("docker-credential-proofstorm-test-tripwire");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf invoked >> \"$REGISTRY_HELPER_MARKER\"\nexit 1\n",
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
    let mut paths = vec![root.path().to_owned()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    // A separate test process avoids changing environment variables under other
    // tests, and makes ambient helper discovery part of the regression check.
    let output = Command::new(std::env::current_exe()?)
        .args([TEST, "--exact", "--ignored", "--nocapture"])
        .env("REGISTRY_TEST_CHILD_HOME", &home)
        .env("REGISTRY_HELPER_MARKER", root.path().join("helper-invoked"))
        .env("REGISTRY_TEST_IMAGE", std::env::var("REGISTRY_TEST_IMAGE")?)
        .env("PATH", std::env::join_paths(paths)?)
        .env("DOCKER_CONFIG", &ambient)
        .env(
            "DOCKER_HOST",
            endpoint["Host"].as_str().context("Docker host")?,
        )
        .env_remove("DOCKER_CONTEXT")
        .env_remove("DOCKER_AUTH_CONFIG")
        .env_remove("BUILDX_CONFIG")
        .env_remove("BUILDX_BUILDER")
        .output()?;
    ensure!(
        output.status.success(),
        "registry isolation check failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
