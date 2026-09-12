use super::*;

#[test]
fn publisher_checksums_require_one_exact_entry() {
    let sha = "a".repeat(64);
    assert_eq!(checksum(sha.as_bytes(), None).unwrap(), sha);
    assert_eq!(
        checksum(
            format!("{sha}  _dist/k3d-linux-amd64\n").as_bytes(),
            Some("_dist/k3d-linux-amd64")
        )
        .unwrap(),
        sha
    );
    for body in [
        format!("{sha}\n{sha}"),
        "bad".into(),
        format!("{sha} wrong-file"),
    ] {
        assert!(checksum(body.as_bytes(), None).is_err());
        assert!(checksum(body.as_bytes(), Some("helm.tar.gz")).is_err());
    }
    assert!(
        checksum(
            format!("{sha} helm.tar.gz\n{sha} helm.tar.gz").as_bytes(),
            Some("helm.tar.gz")
        )
        .is_err()
    );
}

fn archive(kind: tar::EntryType, duplicate: bool) -> Vec<u8> {
    let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gzip);
    for _ in 0..if duplicate { 2 } else { 1 } {
        let mut header = tar::Header::new_gnu();
        header.set_path("linux-amd64/helm").unwrap();
        header.set_entry_type(kind);
        header.set_size(4);
        header.set_cksum();
        tar.append(&header, &b"tool"[..]).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

#[test]
fn extraction_never_accepts_a_link_duplicate_missing_or_truncated_member() {
    let mut truncated = archive(tar::EntryType::Regular, false);
    truncated.truncate(truncated.len() - 8);
    assert!(executable(&truncated, Some("linux-amd64/helm")).is_err());
    assert_eq!(
        executable(
            &archive(tar::EntryType::Regular, false),
            Some("linux-amd64/helm")
        )
        .unwrap(),
        b"tool"
    );
    for bytes in [
        archive(tar::EntryType::Symlink, false),
        archive(tar::EntryType::Regular, true),
        b"not gzip".to_vec(),
    ] {
        assert!(executable(&bytes, Some("linux-amd64/helm")).is_err());
    }
    assert!(
        executable(
            &archive(tar::EntryType::Regular, false),
            Some("darwin-arm64/helm")
        )
        .is_err()
    );
}

#[test]
fn both_checked_in_manifests_match_versions_and_stale_existing_tools_fail_closed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    for target in [tool_pins::MAC_ARM64, tool_pins::LINUX_AMD64] {
        checked(&root, target).unwrap();
    }
    let fixture = tempfile::tempdir().unwrap();
    let dir = fixture.path().canonicalize().unwrap();
    fs::create_dir(dir.join("release")).unwrap();
    fs::create_dir(dir.join("tools")).unwrap();
    fs::copy(
        root.join("tools/versions.env"),
        dir.join("tools/versions.env"),
    )
    .unwrap();
    fs::copy(
        root.join("release/bootstrap-tools.json"),
        dir.join("release/bootstrap-tools.json"),
    )
    .unwrap();
    directory(&dir.join(".tools/bin")).unwrap();
    fs::write(dir.join(".tools/bin/k3d"), "not a reviewed tool").unwrap();
    assert!(install(&dir, tool_pins::MAC_ARM64).is_err());
    assert!(!dir.join(".tools/bin/kubectl").exists());
    assert_eq!(
        fs::read(dir.join(".tools/bin/k3d")).unwrap(),
        b"not a reviewed tool"
    );
}

#[test]
fn maintainer_install_checks_downloads_reuses_exact_files_and_refuses_links() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = tempfile::tempdir().unwrap();
    let dir = fixture.path().canonicalize().unwrap();
    fs::create_dir(dir.join("release")).unwrap();
    fs::create_dir(dir.join("tools")).unwrap();
    fs::copy(
        root.join("tools/versions.env"),
        dir.join("tools/versions.env"),
    )
    .unwrap();
    let target = tool_pins::LINUX_AMD64;
    let mut pins = checked(&root.canonicalize().unwrap(), target).unwrap();
    let helm = archive(tar::EntryType::Regular, false);
    for tool in &mut pins.tools {
        tool.sha256 = hash(if tool.archive_member.is_some() {
            &helm
        } else {
            b"tool"
        });
        tool.executable_sha256 = hash(b"tool");
    }
    fs::write(
        dir.join("release/bootstrap-tools-linux-amd64.json"),
        serde_json::to_vec(&pins).unwrap(),
    )
    .unwrap();
    assert!(install_with(&dir, target, &mut |_, _, _| Ok(b"corrupt".to_vec())).is_err());
    assert!(!dir.join(".tools/bin/k3d").exists());
    let mut downloads = 0;
    install_with(&dir, target, &mut |url, _, _| {
        downloads += 1;
        Ok(if url.ends_with(".tar.gz") {
            helm.clone()
        } else {
            b"tool".to_vec()
        })
    })
    .unwrap();
    assert_eq!(downloads, 3);
    install_with(&dir, target, &mut |_, _, _| {
        panic!("exact installed files must not download")
    })
    .unwrap();
    for tool in &pins.tools {
        let path = dir.join(".tools/bin").join(&tool.name);
        assert_eq!(fs::read(&path).unwrap(), b"tool");
        assert_ne!(fs::metadata(path).unwrap().permissions().mode() & 0o111, 0);
    }
    fs::rename(dir.join(".tools/bin/k3d"), dir.join("saved-k3d")).unwrap();
    std::os::unix::fs::symlink(dir.join("saved-k3d"), dir.join(".tools/bin/k3d")).unwrap();
    assert!(
        install_with(&dir, target, &mut |_, _, _| panic!(
            "links must fail before downloads"
        ))
        .is_err()
    );
    assert_eq!(fs::read(dir.join("saved-k3d")).unwrap(), b"tool");
}
