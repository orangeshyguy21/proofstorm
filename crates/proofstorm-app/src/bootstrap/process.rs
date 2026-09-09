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
            anyhow::bail!(
                "{} exceeded its time/output limit; retry the current setup stage",
                program.display()
            );
        }
        thread::sleep(Duration::from_millis(100));
    };
    // Errors deliberately omit potentially sensitive subprocess output (kubeconfig/tokens).
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
