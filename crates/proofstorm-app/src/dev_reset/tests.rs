use super::*;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    home: PathBuf,
    installation: Installation,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root
            .path()
            .canonicalize()
            .unwrap()
            .join("checkout with 'quotes'");
        let work = source.join(".proofstorm-dev");
        let home = work.join("state");
        fs::create_dir_all(source.join("crates/proofstorm-app")).unwrap();
        fs::write(source.join("crates/proofstorm-app/Cargo.toml"), "fixture").unwrap();
        fs::create_dir(&work).unwrap();
        save(&work.join("owner.json"), &json!({"source":source})).unwrap();
        let installation = Installation::initialize(&home, None, None).unwrap();
        save(
            &home.join("checkout-artifacts.json"),
            &json!({"format_version":1,"installation_id":installation.id,
            "source":source,"resources":source.join("resources"),"web_dist":source.join("web"),
            "cli":source.join("target/proofstorm"),"mcp":source.join("target/proofstorm-mcp"),
            "cli_sha256":"stale","mcp_sha256":"stale","files":{},"metadata":{}}),
        )
        .unwrap();
        fs::create_dir(work.join("target")).unwrap();
        fs::write(work.join("target/cache"), "build cache").unwrap();
        fs::create_dir(home.join("tools")).unwrap();
        fs::write(home.join("tools/helper"), "pinned tool").unwrap();
        fs::write(installation.database(), "old database fixture").unwrap();
        Self {
            _root: root,
            source,
            home,
            installation,
        }
    }

    fn journal(&self) -> Journal {
        Journal {
            format_version: 1,
            previous: self.installation.clone(),
            replacement: None,
            runtime_removed: true,
        }
    }
}

#[test]
fn reset_is_checkout_only_and_description_never_writes_or_requires_current_builds() {
    let fixture = Fixture::new();
    let before = fs::read(fixture.home.join("checkout-artifacts.json")).unwrap();
    assert_eq!(
        describe(&fixture.home).unwrap()["installation_id"],
        fixture.installation.id
    );
    assert_eq!(
        fs::read(fixture.home.join("checkout-artifacts.json")).unwrap(),
        before
    );
    assert!(
        !fixture
            .source
            .join(".proofstorm-dev/reset-pending.json")
            .exists()
    );
    assert!(describe(fixture.source.as_path()).is_err());
    assert!(describe(&fixture.source.join("release-home")).is_err());
    save(
        &fixture.source.join(".proofstorm-dev/owner.json"),
        &json!({"source":"/foreign"}),
    )
    .unwrap();
    assert!(describe(&fixture.home).is_err());
}

#[test]
fn replacement_preserves_caches_archives_old_state_and_can_resume_after_rename() {
    let fixture = Fixture::new();
    let pending = fixture.source.join(".proofstorm-dev").join(JOURNAL);
    let mut journal = fixture.journal();
    save(&pending, &journal).unwrap();
    let history = fixture.source.join(".proofstorm-dev/reset-history");
    fs::create_dir(&history).unwrap();
    fs::rename(&fixture.home, history.join(&fixture.installation.id)).unwrap();
    assert!(describe(&fixture.home).is_ok());
    assert!(check_pending(&fixture.home).is_err());
    let archive = finish(&fixture.home, &fixture.source, &mut journal, &pending).unwrap();
    let next = Installation::load(&fixture.home).unwrap();
    assert_ne!(next.id, fixture.installation.id);
    assert!(!next.database().exists());
    assert!(!next.kubeconfig().exists());
    assert_eq!(
        fs::read_to_string(archive.join("proofstorm.sqlite3")).unwrap(),
        "old database fixture"
    );
    assert_eq!(
        fs::read_to_string(fixture.home.join("tools/helper")).unwrap(),
        "pinned tool"
    );
    assert_eq!(
        fs::read_to_string(fixture.source.join(".proofstorm-dev/target/cache")).unwrap(),
        "build cache"
    );
    assert_eq!(
        crate::artifacts::reset_source(&fixture.home).unwrap(),
        fixture.source
    );
    assert!(!pending.exists());
    // Crash after activation/registration but before dropping the pending marker.
    save(&pending, &journal).unwrap();
    finish(&fixture.home, &fixture.source, &mut journal, &pending).unwrap();
    assert_eq!(Installation::load(&fixture.home).unwrap(), next);
}

#[test]
fn reset_refuses_symlinks_foreign_archives_and_unremoved_runtime() {
    let fixture = Fixture::new();
    let pending = fixture.source.join(".proofstorm-dev").join(JOURNAL);
    let mut journal = fixture.journal();
    journal.runtime_removed = false;
    assert!(finish(&fixture.home, &fixture.source, &mut journal, &pending).is_err());
    journal.runtime_removed = true;
    let outside = fixture.source.join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(
        &outside,
        fixture.source.join(".proofstorm-dev/reset-history"),
    )
    .unwrap();
    assert!(finish(&fixture.home, &fixture.source, &mut journal, &pending).is_err());
    assert!(outside.read_dir().unwrap().next().is_none());
    assert!(fixture.home.exists());
    fs::set_permissions(
        fixture.home.join("checkout-artifacts.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(describe(&fixture.home).is_err());
}

#[test]
fn reset_barrier_blocks_setup_and_gui_until_recovery_and_survives_archiving() {
    let fixture = Fixture::new();
    let guard = checkout_guard(&fixture.home, true).unwrap();
    assert!(Installation::lock(&fixture.home).is_err());
    drop(guard);
    let pending = fixture.source.join(".proofstorm-dev").join(JOURNAL);
    save(&pending, &fixture.journal()).unwrap();
    assert!(Installation::lock(&fixture.home).is_err());
    assert!(crate::artifacts::check_checkout(&fixture.home).is_err());
    assert!(checkout_guard(&fixture.home, true).unwrap().is_some());
}
