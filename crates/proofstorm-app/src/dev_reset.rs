//! Explicit runtime retirement and fresh state for a checkout or an installed release.
//! Build caches and installed release files are never deleted.
use crate::installation::Installation;
use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

const JOURNAL: &str = "reset-pending.json";

fn checkout(home: &Path) -> Option<&Path> {
    (home.file_name()? == "state" && home.parent()?.file_name()? == ".proofstorm-dev")
        .then(|| home.parent().unwrap())
}

/// An installed release keeps its default home beside `install.json`.
fn installed(home: &Path) -> Option<&Path> {
    let root = home.parent()?;
    (home.file_name()? == "state"
        && checkout(home).is_none()
        && root.join("install.json").is_file())
    .then_some(root)
}

/// Where reset keeps its journal, lock and archives, outside the home it replaces.
fn reset_root(home: &Path) -> Option<&Path> {
    checkout(home).or_else(|| installed(home))
}

fn resume_command(home: &Path) -> &'static str {
    if checkout(home).is_some() {
        "storm dev reset --yes"
    } else {
        "storm setup --recreate --yes"
    }
}

fn regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "refusing linked or non-file state: {}",
        path.display()
    );
    Ok(())
}

fn directory(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.is_dir(),
        "refusing linked or non-directory state: {}",
        path.display()
    );
    Ok(())
}

/// The checkout source for a checkout home; `None` for an installed release.
fn target(home: &Path) -> Result<Option<PathBuf>> {
    ensure!(home.is_absolute(), "reset requires an absolute home");
    let work = reset_root(home).context(
        "reset only supports a checkout's .proofstorm-dev/state or an installed release's default home",
    )?;
    directory(work)?;
    ensure!(
        work.canonicalize()? == work,
        "reset refuses linked installation paths"
    );
    let source = if checkout(home).is_some() {
        let source = work.parent().context("checkout source missing")?;
        let owner = work.join("owner.json");
        regular(&owner)?;
        let owner: Value = serde_json::from_slice(&fs::read(owner)?)?;
        ensure!(
            owner["source"] == json!(source)
                && source.join("crates/proofstorm-app/Cargo.toml").is_file(),
            "checkout ownership does not match reset target"
        );
        Some(source.to_owned())
    } else {
        let owner = work.join("install.json");
        regular(&owner)?;
        let owner: Value = serde_json::from_slice(&fs::read(owner)?)?;
        let prefix = owner["prefix"].as_str().map(Path::new);
        ensure!(
            owner["format_version"] == 1
                && prefix.is_some_and(|prefix| prefix.join("lib/proofstorm") == work),
            "installed release ownership does not match reset target"
        );
        None
    };
    if home.try_exists()? {
        directory(home)?;
    }
    Ok(source)
}

pub(crate) fn check_pending(home: &Path) -> Result<()> {
    if let Some(work) = reset_root(home) {
        ensure!(
            !work.join(JOURNAL).try_exists()?,
            "reset is unfinished; rerun {} before setup or rebuilding",
            resume_command(home)
        );
    }
    Ok(())
}

// Outside `state`: the lock remains stable while that directory is archived.
// Setup, registration and GUI startup use the same guard as reset.
pub(crate) fn checkout_guard(home: &Path, resetting: bool) -> Result<Option<Connection>> {
    let Some(work) = checkout(home).filter(|work| work.join("owner.json").exists()) else {
        return Ok(None);
    };
    target(home)?;
    let db = operation_lock(work)?;
    if !resetting {
        check_pending(home)?;
    }
    Ok(Some(db))
}

fn operation_lock(work: &Path) -> Result<Connection> {
    let path = work.join("operation-lock.sqlite3");
    if !path.try_exists()? {
        use std::os::unix::fs::OpenOptionsExt;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
    }
    regular(&path)?;
    let db = Connection::open(&path)?;
    db.busy_timeout(Duration::ZERO)?;
    db.execute_batch("BEGIN IMMEDIATE")
        .context("another installation operation is running; retry after it finishes")?;
    Ok(db)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    format_version: u32,
    previous: Installation,
    replacement: Option<Installation>,
    runtime_removed: bool,
}

fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    if path.try_exists()? {
        regular(path)?;
    }
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("state parent missing")?)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn read_journal(home: &Path) -> Result<Option<Journal>> {
    let path = reset_root(home)
        .context("not a resettable home")?
        .join(JOURNAL);
    if !path.try_exists()? {
        return Ok(None);
    }
    regular(&path)?;
    let journal: Journal = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        journal.format_version == 1
            && journal.previous.home == home
            && valid_id(&journal.previous.id),
        "reset journal belongs to another installation"
    );
    if let Some(next) = &journal.replacement {
        ensure!(
            journal.runtime_removed
                && next.home == home
                && valid_id(&next.id)
                && next.id != journal.previous.id,
            "invalid replacement installation"
        );
    }
    Ok(Some(journal))
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Read-only target resolution, used before asking for confirmation.
pub fn describe(home: &Path) -> Result<Value> {
    let source = target(home)?;
    let installation = if let Some(journal) = read_journal(home)? {
        journal.previous
    } else {
        if let Some(source) = &source {
            ensure!(
                crate::artifacts::reset_source(home)? == *source,
                "registered source differs from this checkout"
            );
        }
        Installation::load(home)?
    };
    Ok(
        json!({"home":home,"installation_id":installation.id,"cluster":installation.cluster_name(),
        "kind":if source.is_some() { "checkout" } else { "installed" }}),
    )
}

/// The caller must obtain explicit confirmation before entering this operation.
pub async fn run(home: &Path, expected_id: &str, progress: &dyn Fn(&str)) -> Result<Value> {
    let source = target(home)?;
    let work = reset_root(home).context("reset root missing")?;
    let _operation = match source {
        Some(_) => checkout_guard(home, true)?.context("checkout lock missing")?,
        None => operation_lock(work)?,
    };
    ensure!(
        describe(home)?["installation_id"] == expected_id,
        "reset target changed since confirmation; retry"
    );
    let pending = work.join(JOURNAL);
    let mut journal = match read_journal(home)? {
        Some(journal) => journal,
        None => Journal {
            format_version: 1,
            previous: Installation::load(home)?,
            replacement: None,
            runtime_removed: false,
        },
    };
    save(&pending, &journal)?;
    if !journal.runtime_removed {
        progress("Stopping GUI");
        crate::gui::stop(home).await?;
        let _state = Installation::lock_state(home, None)?;
        ensure!(
            Installation::load(home)? == journal.previous,
            "installation changed during reset"
        );
        progress("Verifying runtime ownership");
        crate::bootstrap::teardown::prepare_dev_reset(&journal.previous)?;
        crate::bootstrap::teardown::retire_locked(&journal.previous, progress)?;
        journal.runtime_removed = true;
        save(&pending, &journal)?;
    }
    progress("Creating fresh installation state");
    let archive = finish(home, source.as_deref(), &mut journal, &pending)?;
    let next = journal
        .replacement
        .as_ref()
        .context("replacement installation missing")?;
    Ok(json!({"reset":true,"home":home,"installation_id":next.id,
        "previous_installation_id":journal.previous.id,"diagnostics":archive,
        "runtime_started":false,"build_cache_preserved":true,"next":"Run storm setup, then storm gui. Reconnect coding agents after setup with storm agent configure."}))
}

fn finish(
    home: &Path,
    source: Option<&Path>,
    journal: &mut Journal,
    pending: &Path,
) -> Result<PathBuf> {
    ensure!(
        journal.runtime_removed,
        "runtime must be removed before resetting state"
    );
    let history = reset_root(home)
        .context("reset root missing")?
        .join("reset-history");
    if !history.try_exists()? {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&history)?;
    }
    directory(&history)?;
    let archive = history.join(&journal.previous.id);
    if archive.try_exists()? {
        directory(&archive)?;
        regular(&archive.join("installation.json"))?;
        let archived: Installation =
            serde_json::from_slice(&fs::read(archive.join("installation.json"))?)?;
        ensure!(
            archived == journal.previous,
            "reset archive belongs to another installation"
        );
    } else {
        directory(home)?;
        ensure!(
            Installation::load(home)? == journal.previous,
            "reset source identity changed"
        );
        fs::rename(home, &archive)?;
    }
    if journal.replacement.is_none() {
        ensure!(
            !home.try_exists()?,
            "unrecorded replacement home; refusing adoption"
        );
        journal.replacement = Some(Installation::fresh(home, None, None)?);
        save(pending, journal)?;
    }
    let next = journal.replacement.as_ref().unwrap();
    if home.try_exists()? {
        directory(home)?;
        if home.join("installation.json").try_exists()? {
            regular(&home.join("installation.json"))?;
        }
    }
    next.activate_fresh()?;
    let tools = archive.join("tools");
    if tools.try_exists()? {
        directory(&tools)?;
        ensure!(
            !home.join("tools").try_exists()?,
            "replacement tools already exist; refusing overwrite"
        );
        fs::rename(tools, home.join("tools"))?;
    }
    if let Some(source) = source {
        crate::artifacts::restore_after_reset(&archive, next, source, &journal.previous.id)?;
    }
    save(&archive.join("reset-complete.json"), journal)?;
    fs::remove_file(pending)?;
    Ok(archive)
}

#[cfg(test)]
mod tests;
