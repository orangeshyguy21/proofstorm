//! Anonymous access to the installation's loopback registry, on the selected engine.
use super::{Installation, docker, process};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

pub(super) struct Registry {
    config: tempfile::TempDir,
    host: String,
}

impl Registry {
    pub(super) fn new(installation: &Installation) -> Result<Self> {
        // Resolve before hiding DOCKER_CONFIG. Docker applies DOCKER_CONTEXT,
        // DOCKER_HOST and currentContext precedence, including the default context.
        let endpoint: Value = serde_json::from_str(&docker(
            &installation.home,
            &[
                "context",
                "inspect",
                "--format",
                "{{json .Endpoints.docker}}",
            ],
            15,
        )?)?;
        let registry =
            Self::from_endpoint(&installation.home, &installation.host_registry(), &endpoint)?;
        // Desktop may install Buildx only under the user's config directory.
        // Preserve the selected executable, never the surrounding configuration.
        let plugins: Value = serde_json::from_str(&docker(
            &installation.home,
            &["info", "--format", "{{json .ClientInfo.Plugins}}"],
            15,
        )?)?;
        let buildx = plugins
            .as_array()
            .and_then(|plugins| plugins.iter().find(|plugin| plugin["Name"] == "buildx"))
            .and_then(|plugin| plugin["Path"].as_str())
            .context("Docker Buildx plugin missing; install Buildx before checkout setup")?;
        let buildx = Path::new(buildx);
        ensure!(
            buildx.is_absolute() && buildx.is_file(),
            "invalid Docker Buildx plugin path"
        );
        let directory = registry.config.path().join("cli-plugins");
        fs::create_dir(&directory)?;
        std::os::unix::fs::symlink(buildx.canonicalize()?, directory.join("docker-buildx"))?;
        Ok(registry)
    }

    fn from_endpoint(home: &Path, registry: &str, endpoint: &Value) -> Result<Self> {
        let host = endpoint["Host"]
            .as_str()
            .context("Docker endpoint missing")?;
        // This path publishes to a host-loopback registry. Do not silently switch
        // engines or discard remote-context TLS/SSH authentication to make it work.
        ensure!(
            host.strip_prefix("unix://")
                .is_some_and(|path| Path::new(path).is_absolute()),
            "checkout controller publication requires a local Docker Unix socket; select a local Docker context"
        );
        let config = tempfile::Builder::new()
            .prefix("controller-registry-")
            .tempdir_in(home)?;
        fs::set_permissions(config.path(), fs::Permissions::from_mode(0o700))?;
        // A literal {} lets Docker auto-discover a platform credential helper.
        // One empty registry entry suppresses discovery without supplying credentials.
        process::save(
            &config.path().join("config.json"),
            &serde_json::to_vec(&json!({"auths": {registry: {}}}))?,
        )?;
        Ok(Self {
            config,
            host: host.into(),
        })
    }

    fn isolate(&self, command: &mut Command) {
        command
            .env("DOCKER_CONFIG", self.config.path())
            .env("DOCKER_HOST", &self.host);
        for key in [
            "DOCKER_CONTEXT",
            "DOCKER_AUTH_CONFIG",
            "DOCKER_TLS",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
            "DOCKER_CUSTOM_HEADERS",
            "BUILDX_CONFIG",
            "BUILDX_BUILDER",
        ] {
            command.env_remove(key);
        }
    }

    pub(super) fn run(&self, home: &Path, args: &[&str], seconds: u64) -> Result<String> {
        let mut command = Command::new("docker");
        command.args(args);
        self.isolate(&mut command);
        process::configured(home, &mut command, seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_anonymous_config_is_scoped_and_removed_on_drop() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::from_endpoint(
            home.path(),
            "127.0.0.1:42102",
            &json!({"Host":"unix:///custom/docker.sock"}),
        )
        .unwrap();
        let path = registry.config.path().to_owned();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = path.join("config.json");
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(file).unwrap()).unwrap(),
            json!({"auths":{"127.0.0.1:42102":{}}})
        );
        drop(registry);
        assert!(!path.exists());
    }

    #[test]
    fn subprocess_keeps_resolved_engine_without_ambient_auth_or_context() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::from_endpoint(
            home.path(),
            "127.0.0.1:42102",
            &json!({"Host":"unix:///custom/docker.sock"}),
        )
        .unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", r#"
            test "$DOCKER_HOST" = unix:///custom/docker.sock &&
            test -f "$DOCKER_CONFIG/config.json" &&
            test -z "${DOCKER_CONTEXT+x}${DOCKER_AUTH_CONFIG+x}${DOCKER_TLS+x}${DOCKER_TLS_VERIFY+x}${DOCKER_CERT_PATH+x}${DOCKER_CUSTOM_HEADERS+x}${BUILDX_CONFIG+x}${BUILDX_BUILDER+x}" &&
            printf isolated
        "#]);
        for key in [
            "DOCKER_CONFIG",
            "DOCKER_HOST",
            "DOCKER_CONTEXT",
            "DOCKER_AUTH_CONFIG",
            "DOCKER_TLS",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
            "DOCKER_CUSTOM_HEADERS",
            "BUILDX_CONFIG",
            "BUILDX_BUILDER",
        ] {
            command.env(key, "ambient-value");
        }
        registry.isolate(&mut command);
        assert_eq!(
            process::configured(home.path(), &mut command, 5).unwrap(),
            "isolated"
        );
    }

    #[test]
    fn malformed_or_remote_endpoints_never_fall_back_to_another_engine() {
        let home = tempfile::tempdir().unwrap();
        for endpoint in [
            json!({}),
            json!({"Host":""}),
            json!({"Host":"unix://relative"}),
            json!({"Host":"tcp://remote:2376"}),
            json!({"Host":"ssh://remote"}),
        ] {
            assert!(Registry::from_endpoint(home.path(), "127.0.0.1:42102", &endpoint).is_err());
        }
        assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    }
}
