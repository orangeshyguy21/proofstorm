//! Bounded subprocesses. Captured output stays in private temporary files.
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;
/// Cluster-wide list reads grow with retained runtime objects (cell actions).
const LIST_OUTPUT_LIMIT: u64 = 128 * 1024 * 1024;

pub(super) fn run(home: &Path, program: &Path, args: &[&str], seconds: u64) -> Result<String> {
    run_inner(home, program, args, seconds, None, OUTPUT_LIMIT)
}

pub(super) fn run_list(home: &Path, program: &Path, args: &[&str], seconds: u64) -> Result<String> {
    run_inner(home, program, args, seconds, None, LIST_OUTPUT_LIMIT)
}

pub(super) fn controller_build(home: &Path, args: &[&str]) -> Result<String> {
    run_inner(
        home,
        Path::new("docker"),
        args,
        3600,
        Some(&home.join("controller-build.log")),
        OUTPUT_LIMIT,
    )
}

pub(super) fn image_preparation(
    home: &Path,
    command: &mut Command,
    seconds: u64,
) -> Result<String> {
    run_command(
        home,
        command,
        seconds,
        Some(&home.join("image-preparation.log")),
        OUTPUT_LIMIT,
    )
}

pub(super) fn image_command(args: &[&str], inherited_debug: Option<std::ffi::OsString>) -> Command {
    let mut command = Command::new("docker");
    command.args(args);
    if args.starts_with(&["buildx", "imagetools", "create"]) {
        // Registry copies encountered peer HTTP/2 PROTOCOL_ERROR failures.
        // Go's documented client switch is scoped to this CLI/plugin process;
        // HTTPS, credentials, digest checks, and the daemon stay unchanged.
        // GODEBUG uses the last occurrence of a setting.
        let mut debug = inherited_debug.unwrap_or_default();
        if !debug.is_empty() {
            debug.push(",");
        }
        debug.push("http2client=0");
        command.env("GODEBUG", debug);
    }
    command
}

fn build_log(log: Option<&Path>, out: &fs::File, err: &fs::File) -> Result<()> {
    if let Some(path) = log {
        let mut bytes = Vec::new();
        for file in [out, err] {
            let mut file = file.try_clone()?;
            file.seek(SeekFrom::Start(0))?;
            file.take(OUTPUT_LIMIT).read_to_end(&mut bytes)?;
        }
        save(path, &bytes)?;
    }
    Ok(())
}

fn run_inner(
    home: &Path,
    program: &Path,
    args: &[&str],
    seconds: u64,
    log: Option<&Path>,
    limit: u64,
) -> Result<String> {
    run_command(home, Command::new(program).args(args), seconds, log, limit)
}

/// Apply the usual bounds and environment policy to a command with scoped overrides.
pub(super) fn configured(home: &Path, command: &mut Command, seconds: u64) -> Result<String> {
    run_command(home, command, seconds, None, OUTPUT_LIMIT)
}

fn run_command(
    home: &Path,
    command: &mut Command,
    seconds: u64,
    log: Option<&Path>,
    limit: u64,
) -> Result<String> {
    let program = command.get_program().to_owned();
    let program = Path::new(&program);
    let out = tempfile::tempfile()?;
    let err = tempfile::tempfile()?;
    command
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(err.try_clone()?);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("K3D_") || name.starts_with("HELM_") || name.starts_with("PROOFSTORM_")
        {
            command.env_remove(key);
        }
    }
    command.env("KUBECONFIG", home.join("kubeconfig"));
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot run {}; check it is installed", program.display()))?;
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= Duration::from_secs(seconds)
            || out.metadata()?.len() > limit
            || err.metadata()?.len() > OUTPUT_LIMIT
        {
            let _ = child.kill();
            let _ = child.wait();
            build_log(log, &out, &err)?;
            anyhow::bail!(
                "{} exceeded its time/output limit; retry the current setup stage",
                program.display()
            );
        }
        thread::sleep(Duration::from_millis(100));
    };
    // Errors deliberately omit potentially sensitive subprocess output (kubeconfig/tokens).
    build_log(log, &out, &err)?;
    if !status.success() {
        if let Some(path) = log {
            let operation = if path
                .file_name()
                .is_some_and(|name| name == "image-preparation.log")
            {
                "image preparation"
            } else {
                "controller build"
            };
            anyhow::bail!(
                "{operation} failed; inspect private log {} and retry setup",
                path.display()
            );
        }
    }
    ensure!(
        status.success(),
        "{} failed ({}); check Docker/network access and retry",
        program.display(),
        status
    );
    // A fast exit can outrun the poll above; never hand back truncated output.
    ensure!(
        out.metadata()?.len() <= limit,
        "{} exceeded its output limit; retry the current setup stage",
        program.display()
    );
    let mut out = out;
    out.seek(SeekFrom::Start(0))?;
    let mut result = String::new();
    out.read_to_string(&mut result)?;
    Ok(result)
}

pub(super) fn save(path: &Path, value: &[u8]) -> Result<()> {
    use std::io::Write;
    ensure!(
        !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()),
        "refusing symlink state file"
    );
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("state parent missing")?)?;
    file.write_all(value)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn http1_is_scoped_to_registry_copy_and_preserves_other_debug_settings() {
        for inherited in [None, Some(""), Some("x509usefallbackroots=1,http2client=1")] {
            let args = [
                "buildx",
                "imagetools",
                "create",
                "--prefer-index=false",
                "--tag",
                "local/image",
                "remote/image@sha256:pin",
            ];
            let command = image_command(&args, inherited.map(Into::into));
            assert_eq!(command.get_args().collect::<Vec<_>>(), args);
            let env = command.get_envs().collect::<Vec<_>>();
            assert_eq!(env.len(), 1);
            assert_eq!(env[0].0, "GODEBUG");
            let expected = inherited.filter(|v| !v.is_empty()).map_or_else(
                || "http2client=0".to_string(),
                |value| format!("{value},http2client=0"),
            );
            assert_eq!(env[0].1.unwrap(), expected.as_str());
        }
        for args in [
            vec!["buildx", "imagetools", "inspect", "image"],
            vec!["exec", "node", "crictl", "pull", "image"],
        ] {
            assert_eq!(
                image_command(&args, Some("other=1".into()))
                    .get_envs()
                    .count(),
                0
            );
        }
    }
    #[test]
    fn oversized_output_is_an_error_not_truncated_json() {
        let root = tempfile::tempdir().unwrap();
        let args = ["-c", "head -c 4096 /dev/zero"];
        let error = run_inner(root.path(), Path::new("/bin/sh"), &args, 5, None, 1024);
        assert!(error.unwrap_err().to_string().contains("output limit"));
        let output = run_inner(root.path(), Path::new("/bin/sh"), &args, 5, None, 4096).unwrap();
        assert_eq!(output.len(), 4096);
    }
    #[test]
    fn failed_build_and_image_diagnostics_stay_private_and_out_of_error_text() {
        let root = tempfile::tempdir().unwrap();
        for (name, label) in [
            ("controller-build.log", "controller build"),
            ("image-preparation.log", "image preparation"),
        ] {
            let log = root.path().join(name);
            let error = run_inner(
                root.path(),
                Path::new("/bin/sh"),
                &["-c", "printf 'private-build-output' >&2; exit 1"],
                5,
                Some(&log),
                OUTPUT_LIMIT,
            )
            .unwrap_err();
            assert!(error.to_string().contains(label));
            assert!(!error.to_string().contains("private-build-output"));
            assert_eq!(fs::read_to_string(&log).unwrap(), "private-build-output");
            assert_eq!(
                fs::metadata(log).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
