//! Real shell installer boundary checks; fixture archives never launch a runtime.
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn archive(root: &Path, name: &str, kind: tar::EntryType) {
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let body =
        b"#!/bin/sh\n[ -x \"$0\" ] || exit 19\nprintf '%s\\n' \"$@\" > \"$INSTALL_MARKER\"\n";
    let mut header = tar::Header::new_gnu();
    // Raw field lets the test construct a malicious traversal archive.
    header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
    header.set_mode(0o755);
    header.set_entry_type(kind);
    if kind.is_symlink() {
        header.set_link_name("/outside").unwrap();
        header.set_size(0);
    } else {
        header.set_size(body.len() as u64);
    }
    header.set_cksum();
    tar.append(&header, if kind.is_symlink() { &b""[..] } else { body })
        .unwrap();
    let bytes = tar.into_inner().unwrap().finish().unwrap();
    fs::write(root.join("proofstorm-fixture.tar.gz"), &bytes).unwrap();
    fs::write(
        root.join("proofstorm-fixture.tar.gz.sha256"),
        format!("{:x}  proofstorm-fixture.tar.gz\n", Sha256::digest(&bytes)),
    )
    .unwrap();
}

fn installer(root: &Path) -> Command {
    let mut command = Command::new("sh");
    command.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh"));
    command.env("INSTALL_MARKER", root.join("executed"));
    command.env(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap()
        ),
    );
    command
        .args(["--artifact-dir"])
        .arg(root)
        .args(["--archive", "proofstorm-fixture.tar.gz", "--prefix"])
        .arg(root.join("new prefix"));
    command
}

#[test]
fn local_install_rejects_corrupt_unsafe_and_linked_payloads_before_activation() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    fs::create_dir(root.join("bin")).unwrap();
    for name in ["python", "python3", "cargo", "rustc", "curl"] {
        script(&root.join("bin").join(name), "#!/bin/sh\nexit 97\n");
    }
    script(
        &root.join("bin/uname"),
        "#!/bin/sh\ncase \"$1\" in -s) echo Linux;; -m) echo x86_64;; esac\n",
    );
    for (name, kind) in [
        ("proofstorm/../../escaped", tar::EntryType::Regular),
        ("proofstorm/bin/proofstorm", tar::EntryType::Symlink),
    ] {
        archive(root, name, kind);
        assert!(!installer(root).output().unwrap().status.success());
        assert!(!root.join("executed").exists());
        assert!(!root.join("new prefix").exists());
    }
    archive(root, "proofstorm/bin/proofstorm", tar::EntryType::Regular);
    fs::write(root.join("proofstorm-fixture.tar.gz"), b"corrupt").unwrap();
    let failed = installer(root).output().unwrap();
    assert!(!failed.status.success());
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("checksum mismatch"),
        "{}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(!root.join("executed").exists());
    archive(root, "proofstorm/bin/proofstorm", tar::EntryType::Regular);
    let passed = installer(root).arg("--allow-development").output().unwrap();
    assert!(
        passed.status.success(),
        "{}",
        String::from_utf8_lossy(&passed.stderr)
    );
    assert!(
        fs::read_to_string(root.join("executed"))
            .unwrap()
            .contains("--allow-development")
    );
    assert!(String::from_utf8_lossy(&passed.stdout).contains("No cluster"));
}

#[test]
fn unsupported_hosts_and_unsafe_download_options_fail_before_network_or_state() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    fs::create_dir(root.join("bin")).unwrap();
    script(
        &root.join("bin/curl"),
        "#!/bin/sh\nprintf called > \"$INSTALL_MARKER\"\nexit 97\n",
    );
    for (os, arch) in [
        ("Linux", "aarch64"),
        ("Darwin", "x86_64"),
        ("Windows", "x86_64"),
    ] {
        script(
            &root.join("bin/uname"),
            &format!("#!/bin/sh\ncase \"$1\" in -s) echo {os};; -m) echo {arch};; esac\n"),
        );
        let output = installer(root).output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("supports macOS Apple Silicon and Linux x86-64")
        );
    }
    for args in [
        vec!["--allow-development"],
        vec!["--archive", "../escape.tar.gz"],
    ] {
        let output = Command::new("sh")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh"))
            .args(args)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    root.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("INSTALL_MARKER", root.join("executed"))
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    assert!(!root.join("executed").exists());
    assert!(!root.join("new prefix").exists());
}
