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

pub(super) fn run(home: &Path, program: &Path, args: &[&str], seconds: u64) -> Result<String> {
    run_inner(home, program, args, seconds, None)
}

pub(super) fn controller_build(home: &Path, args: &[&str]) -> Result<String> {
    run_inner(
        home,
        Path::new("docker"),
        args,
        3600,
        Some(&home.join("controller-build.log")),
    )
}

fn build_log(log: Option<&Path>, out: &fs::File, err: &fs::File) -> Result<()> {
    if let Some(path) = log {
        let mut bytes = Vec::new();
        for file in [out, err] {
            let mut file = file.try_clone()?;
            file.seek(SeekFrom::Start(0))?;
            file.take(8 * 1024 * 1024).read_to_end(&mut bytes)?;
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
) -> Result<String> {
    let out = tempfile::tempfile()?;
    let err = tempfile::tempfile()?;
    let mut command = Command::new(program);
    command
        .args(args)
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
            || out.metadata()?.len() > 8 * 1024 * 1024
            || err.metadata()?.len() > 8 * 1024 * 1024
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
            anyhow::bail!(
                "controller build failed; inspect private log {} and retry setup",
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
    let mut out = out;
    out.seek(SeekFrom::Start(0))?;
    let mut result = String::new();
    out.take(8 * 1024 * 1024).read_to_string(&mut result)?;
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
    fn failed_build_diagnostics_stay_private_and_out_of_error_text() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("controller-build.log");
        let error = run_inner(
            root.path(),
            Path::new("/bin/sh"),
            &["-c", "printf 'private-build-output' >&2; exit 1"],
            5,
            Some(&log),
        )
        .unwrap_err();
        assert!(!error.to_string().contains("private-build-output"));
        assert_eq!(fs::read_to_string(&log).unwrap(), "private-build-output");
        assert_eq!(
            fs::metadata(log).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
