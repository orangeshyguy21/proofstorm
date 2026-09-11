//! Native Mac installer checks with source, network, compiler and write restrictions.
use super::{archive::output_path, bundle, linux_install::input_digests_for, text};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fmt::Write as _,
    fs,
    io::Write,
    net::{TcpListener, TcpStream},
    path::Path,
    process::Command,
};

const TARGET: &str = "aarch64-apple-darwin";

fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn prepare(
    source: &Path,
    snapshot: &Path,
    archive: &Path,
    installer: &Path,
    work: &Path,
) -> Result<Vec<String>> {
    let work = output_path(work)?;
    ensure!(!work.exists(), "Mac installer work directory must be new");
    let sources = [source.canonicalize()?, snapshot.canonicalize()?];
    for source in &sources {
        ensure!(
            !work.starts_with(source),
            "installer work must be outside both source trees"
        );
        crate::development::regular(&source.join("Cargo.toml"))?;
    }
    let digests = input_digests_for(archive, installer, TARGET)?;
    let name = archive
        .file_name()
        .and_then(|v| v.to_str())
        .context("invalid archive")?;
    fs::create_dir(&work)?;
    fs::create_dir(work.join("input"))?;
    fs::create_dir(work.join("home"))?;
    fs::create_dir(work.join("tmp"))?;
    fs::copy(archive, work.join("input").join(name))?;
    fs::copy(installer, work.join("input/install.sh"))?;
    fs::write(
        work.join("input").join(format!("{name}.sha256")),
        format!("{}  {name}\n", digests.0),
    )?;
    ensure!(
        input_digests_for(
            &work.join("input").join(name),
            &work.join("input/install.sh"),
            TARGET
        )? == digests,
        "Mac installer inputs changed during staging"
    );
    let mut policy = format!(
        "(version 1) (allow default) (deny network*) (deny file-write*) (allow file-write* (subpath {}) (literal \"/dev/null\"))",
        serde_json::to_string(&work)?
    );
    for source in &sources {
        write!(
            policy,
            " (deny file-read* (subpath {}))",
            serde_json::to_string(source)?
        )?;
    }
    policy.push_str(r#" (deny process-exec (regex #".*/(cargo|rustc|rustup|trunk|clang|cc|gcc|make|cmake|ninja)$"))"#);
    fs::write(work.join("isolation.sb"), &policy)?;
    save(
        &work.join("run.json"),
        &json!({"target":TARGET,"archive":name,
        "archive_sha256":digests.0,"installer_sha256":digests.1,"denied_sources":sources,
        "policy_sha256":bundle::checksum(&work.join("isolation.sb"), policy.len() as u64)?}),
    )?;
    Ok(vec![
        work.to_str().context("invalid work path")?.into(),
        name.into(),
    ])
}

fn run_policy(work: &Path, command: &Path) -> Command {
    let mut process = Command::new("/usr/bin/sandbox-exec");
    process
        .current_dir(work)
        .arg("-f")
        .arg(work.join("isolation.sb"))
        .arg(command);
    process
}

fn verify_isolation(work: &Path) -> Result<()> {
    let work = work.canonicalize()?;
    crate::development::regular(Path::new("/usr/bin/clang"))?;
    let guard = work.join("isolation-probe");
    fs::copy(std::env::current_exe()?, &guard)?;
    let outside = tempfile::tempdir()?;
    let sentinel = outside.path().join("keep");
    fs::write(&sentinel, "unchanged")?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let status = run_policy(&work, &guard)
        .arg("macos-install")
        .arg("probe")
        .arg(&work)
        .arg(&sentinel)
        .arg(listener.local_addr()?.to_string())
        .status()?;
    ensure!(
        status.success() && fs::read_to_string(&sentinel)? == "unchanged",
        "Mac isolation probe failed; do not run installer"
    );
    save(
        &work.join("isolation.json"),
        &json!({"source_read_denied":true,"network_denied":true,"compiler_denied":true,"outside_write_denied":true}),
    )
}

fn probe(work: &Path, sentinel: &Path, address: &str) -> Result<()> {
    let run = bundle::read_json(&work.join("run.json"))?;
    for source in run["denied_sources"]
        .as_array()
        .context("missing denied sources")?
    {
        let path = Path::new(source.as_str().context("invalid source")?).join("Cargo.toml");
        ensure!(fs::read(path).is_err(), "source remains readable");
    }
    ensure!(
        TcpStream::connect(address).is_err(),
        "network remains available"
    );
    ensure!(
        fs::write(sentinel, "changed").is_err(),
        "outside writes remain allowed"
    );
    ensure!(
        Command::new("/usr/bin/clang")
            .arg("--version")
            .output()
            .is_err(),
        "compiler execution remains allowed"
    );
    Ok(())
}

fn finish(work: &Path, status: &str) -> Result<()> {
    ensure!(status == "0", "Mac installer worker failed (exit {status})");
    let run = bundle::read_json(&work.join("run.json"))?;
    let isolation = bundle::read_json(&work.join("isolation.json"))?;
    for key in [
        "source_read_denied",
        "network_denied",
        "compiler_denied",
        "outside_write_denied",
    ] {
        ensure!(
            isolation[key] == true,
            "missing Mac isolation evidence: {key}"
        );
    }
    let policy = work.join("isolation.sb");
    ensure!(
        run["policy_sha256"] == bundle::checksum(&policy, fs::metadata(&policy)?.len())?,
        "Mac sandbox policy changed"
    );
    let name = text(&run, "archive")?;
    ensure!(
        bundle::safe_name(name) && !name.contains('/'),
        "invalid staged archive name"
    );
    let (archive, installer) = input_digests_for(
        &work.join("input").join(name),
        &work.join("input/install.sh"),
        TARGET,
    )?;
    ensure!(
        run["archive_sha256"] == archive && run["installer_sha256"] == installer,
        "Mac installer inputs changed"
    );
    save(
        &work.join("install-smoke-report.json"),
        &json!({
            "target":TARGET,"isolation":"macos-sandbox","local_install":true,"reinstall":true,"cli_mcp_metadata_match":true,
            "source_checkout_present":true,"source_read_access_denied":true,"build_tools_present":true,"compiler_execution_denied":true,
            "outside_writes_denied":true,"network_enabled":false,"runtime_tested":false,"github_download_tested":false,
            "development_override":false,"archive_sha256":archive,"installer_sha256":installer
        }),
    )
}

pub(super) fn cli(args: impl Iterator<Item = OsString>) -> Result<()> {
    ensure!(
        super::build::host_target()? == TARGET,
        "Mac installer checks require native Apple Silicon macOS"
    );
    let args: Vec<_> = args.collect();
    let args: Vec<_> = args
        .iter()
        .map(|s| s.to_str().context("UTF-8 arguments required"))
        .collect::<Result<_>>()?;
    match args.as_slice() {
        ["prepare", source, snapshot, archive, installer, work] => {
            for field in prepare(
                Path::new(source),
                Path::new(snapshot),
                Path::new(archive),
                Path::new(installer),
                Path::new(work),
            )? {
                std::io::stdout().write_all(field.as_bytes())?;
                std::io::stdout().write_all(&[0])?;
            }
            Ok(())
        }
        ["isolation", work] => verify_isolation(Path::new(work)),
        ["probe", work, sentinel, address] => probe(Path::new(work), Path::new(sentinel), address),
        ["finish", work, status] => finish(Path::new(work), status),
        _ => bail!("invalid Mac installer check arguments"),
    }
}
