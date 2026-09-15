use super::*;

fn journal(installation: &Installation, phase: Phase) -> Journal {
    Journal {
        format_version: 1,
        installation_id: installation.id.clone(),
        phase,
        controller: None,
        workloads: None,
        workloads_stopped: false,
        tools_running: true,
    }
}

#[tokio::test]
async fn shutdown_fences_new_calls_and_waits_for_existing_leases() {
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(root.path(), None, None).unwrap();
    let first = access(Some(&installation)).unwrap();
    let second = access(Some(&installation)).unwrap();
    save(&installation.home, &journal(&installation, Phase::Stopping)).unwrap();
    assert!(
        access(Some(&installation))
            .unwrap_err()
            .to_string()
            .contains("stopping")
    );
    assert!(exclusive(&installation.home, Instant::now()).await.is_err());
    drop(first);
    assert!(exclusive(&installation.home, Instant::now()).await.is_err());
    drop(second);
    let shutdown = exclusive(&installation.home, Instant::now()).await.unwrap();
    fs::remove_file(installation.home.join(JOURNAL)).unwrap();
    assert!(access(Some(&installation)).is_err());
    drop(shutdown);
    assert!(access(Some(&installation)).is_ok());
}

#[test]
fn partial_transitions_remain_fenced_and_never_affect_another_home() {
    let root = tempfile::tempdir().unwrap();
    let first = Installation::initialize(&root.path().join("first"), None, None).unwrap();
    let other = Installation::initialize(&root.path().join("other"), None, None).unwrap();
    for phase in [Phase::Stopping, Phase::Stopped, Phase::Starting] {
        save(&first.home, &journal(&first, phase)).unwrap();
        assert!(access(Some(&first)).is_err());
        assert!(access(Some(&other)).is_ok());
        assert_eq!(read(&first.home).unwrap().unwrap().phase, phase);
    }
    fs::copy(first.home.join(JOURNAL), other.home.join(JOURNAL)).unwrap();
    assert!(read(&other.home).is_err());
    fs::write(first.home.join(JOURNAL), b"interrupted json").unwrap();
    assert!(access(Some(&first)).is_err());
}

#[test]
fn linked_transition_and_lock_files_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let installation = Installation::initialize(root.path(), None, None).unwrap();
    let unrelated = root.path().join("unrelated");
    fs::write(&unrelated, b"untouched").unwrap();
    std::os::unix::fs::symlink(&unrelated, root.path().join(JOURNAL)).unwrap();
    assert!(read(root.path()).is_err());
    fs::remove_file(root.path().join(JOURNAL)).unwrap();
    std::os::unix::fs::symlink(&unrelated, root.path().join("runtime-access.lock")).unwrap();
    assert!(access(Some(&installation)).is_err());
    assert_eq!(fs::read(unrelated).unwrap(), b"untouched");
}
