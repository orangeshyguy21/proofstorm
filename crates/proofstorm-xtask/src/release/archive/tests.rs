use super::*;
use crate::release::bundle::tests::Bundle;

fn fixture() -> Bundle {
    Bundle::new("x86_64-unknown-linux-gnu", "development")
}

fn checksum_receipt(path: &Path) {
    let sha = bundle::checksum(path, fs::metadata(path).unwrap().len()).unwrap();
    fs::write(
        sidecar(path),
        format!("{sha}  {}\n", path.file_name().unwrap().to_str().unwrap()),
    )
    .unwrap();
}

fn malicious_archive(parent: &Path, names: &[(&str, u8, u32, u64)]) -> PathBuf {
    let path = parent.join("malicious.tar.gz");
    let mut gzip = GzBuilder::new()
        .mtime(0)
        .write(File::create(&path).unwrap(), Compression::default());
    for (name, kind, mode, size) in names {
        let mut header = tar::Header::new_ustar();
        // Deliberately bypass the writer's safe-path API to model hostile input.
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_entry_type(tar::EntryType::new(*kind));
        header.set_mode(*mode);
        header.set_size(*size);
        header.set_cksum();
        gzip.write_all(header.as_bytes()).unwrap();
    }
    gzip.write_all(&[0; 1024]).unwrap();
    gzip.finish().unwrap();
    checksum_receipt(&path);
    path
}

#[test]
fn archives_are_repeatable_relocatable_and_have_normalized_headers() {
    let bundle = fixture();
    let output = tempfile::tempdir().unwrap();
    let first = pack(bundle.root(), &output.path().join("first")).unwrap();
    let second = pack(bundle.root(), &output.path().join("second")).unwrap();
    assert_eq!(first["sha256"], second["sha256"]);
    let path = Path::new(first["archive"].as_str().unwrap());
    let reader = GzDecoder::new(BufReader::new(File::open(path).unwrap()));
    assert_eq!(reader.header().unwrap().mtime(), 0);
    let mut tar = tar::Archive::new(reader);
    let mut names = Vec::new();
    for entry in tar.entries().unwrap() {
        let entry = entry.unwrap();
        assert!(entry.header().entry_type().is_file());
        assert_eq!(entry.header().uid().unwrap(), 0);
        assert_eq!(entry.header().gid().unwrap(), 0);
        assert_eq!(entry.header().mtime().unwrap(), 0);
        names.push(entry.path_bytes().into_owned());
    }
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    let destination = output.path().join("relocated");
    let receipt = extract(path, &destination).unwrap();
    assert_eq!(receipt["integrity_verified"], true);
    assert_eq!(receipt["release_ready"], false);
    assert_eq!(
        fs::read(destination.join("proofstorm/bin/proofstorm")).unwrap(),
        b"fixture payload\n"
    );
    // Use the install script's system-tar flags as a format compatibility check.
    let system_unpack = output.path().join("system-tar");
    fs::create_dir(&system_unpack).unwrap();
    let result = std::process::Command::new("tar")
        .arg("-xpzf")
        .arg(path)
        .arg("-C")
        .arg(&system_unpack)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(bundle::verify(&system_unpack.join("proofstorm")).is_ok());
    assert!(
        path.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("-dev-debug-bbbbbbbbbbbb-")
    );
}

#[test]
fn existing_archives_checksums_and_destinations_are_never_replaced() {
    let bundle = fixture();
    let output = tempfile::tempdir().unwrap();
    let receipt = pack(bundle.root(), output.path()).unwrap();
    let archive = Path::new(receipt["archive"].as_str().unwrap());
    let original = fs::read(archive).unwrap();
    assert!(pack(bundle.root(), output.path()).is_err());
    assert_eq!(fs::read(archive).unwrap(), original);
    let destination = output.path().join("existing");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("keep"), "user data").unwrap();
    assert!(extract(archive, &destination).is_err());
    assert_eq!(
        fs::read_to_string(destination.join("keep")).unwrap(),
        "user data"
    );
    assert!(pack(bundle.root(), &bundle.root().join("output")).is_err());
    assert!(!bundle.root().join("output").exists());
}

#[test]
fn traversal_links_extensions_duplicates_and_unsafe_modes_never_escape_staging() {
    for entry in [
        ("proofstorm/../../escaped", b'0', 0o644, 0),
        ("/proofstorm/absolute", b'0', 0o644, 0),
        ("proofstorm//double", b'0', 0o644, 0),
        ("proofstorm/./dot", b'0', 0o644, 0),
        ("other/file", b'0', 0o644, 0),
        ("proofstorm/link", b'2', 0o644, 0),
        ("proofstorm/hardlink", b'1', 0o644, 0),
        ("proofstorm/device", b'3', 0o644, 0),
        ("proofstorm/pipe", b'6', 0o644, 0),
        ("proofstorm/pax", b'x', 0o644, u64::MAX),
        ("proofstorm/global-pax", b'g', 0o644, 0),
        ("proofstorm/longname", b'L', 0o644, 0),
        ("proofstorm/sparse", b'S', 0o644, 0),
        ("proofstorm/suid", b'0', 0o4755, 0),
        (
            "proofstorm/huge",
            b'0',
            0o644,
            bundle::MAX_PAYLOAD_BYTES + 1,
        ),
    ] {
        let output = tempfile::tempdir().unwrap();
        let archive = malicious_archive(output.path(), &[entry]);
        let destination = output.path().join("result");
        assert!(extract(&archive, &destination).is_err(), "{}", entry.0);
        assert!(!destination.exists());
        assert!(!output.path().join("escaped").exists());
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 2);
    }
    let output = tempfile::tempdir().unwrap();
    let entry = ("proofstorm/duplicate", b'0', 0o644, 0);
    let archive = malicious_archive(output.path(), &[entry, entry]);
    assert!(
        extract(&archive, &output.path().join("result"))
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
}

#[test]
fn checksum_mismatch_truncated_gzip_and_trailing_members_fail() {
    let bundle = fixture();
    let output = tempfile::tempdir().unwrap();
    let result = pack(bundle.root(), output.path()).unwrap();
    let path = Path::new(result["archive"].as_str().unwrap());
    let original = fs::read(path).unwrap();
    fs::write(path, "corrupt").unwrap();
    assert!(
        extract(path, &output.path().join("checksum"))
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
    fs::write(path, &original[..original.len() - 8]).unwrap();
    checksum_receipt(path);
    assert!(extract(path, &output.path().join("truncated")).is_err());
    let mut bad_crc = original.clone();
    let trailer = bad_crc.len() - 8;
    bad_crc[trailer] ^= 1;
    fs::write(path, bad_crc).unwrap();
    checksum_receipt(path);
    assert!(extract(path, &output.path().join("bad-crc")).is_err());
    fs::write(path, &original).unwrap();
    let mut extra = GzBuilder::new().write(
        OpenOptions::new().append(true).open(path).unwrap(),
        Compression::default(),
    );
    extra.write_all(b"hidden content").unwrap();
    extra.finish().unwrap();
    checksum_receipt(path);
    assert!(
        extract(path, &output.path().join("trailing"))
            .unwrap_err()
            .to_string()
            .contains("trailing")
    );
    for name in ["checksum", "truncated", "bad-crc", "trailing"] {
        assert!(!output.path().join(name).exists());
    }
}

#[test]
fn invalid_payload_is_not_packaged() {
    let bundle = fixture();
    let output = tempfile::tempdir().unwrap();
    fs::write(bundle.root().join("LICENSE"), "tampered").unwrap();
    assert!(pack(bundle.root(), output.path()).is_err());
    assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
}
