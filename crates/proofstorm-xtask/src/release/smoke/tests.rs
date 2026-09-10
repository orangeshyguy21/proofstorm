use super::*;
use crate::release::bundle::tests::Bundle;

fn executable_bundle(target: &str, behavior: &str) -> Bundle {
    let mut bundle = Bundle::new(target, "development");
    for (name, flag) in [
        ("proofstorm", "release-info"),
        ("proofstorm-mcp", "--release-info"),
    ] {
        let code = format!(
            "#!/bin/sh\nset -eu\n[ \"$PROOFSTORM_HOME\" = \"$PWD/must-not-be-created\" ] || exit 97\n[ -z \"$PROOFSTORM_PRINCIPAL\" ] || exit 97\n{behavior}\ncase \"$1\" in --version|--help) echo fixture ;; {flag}) printf '%s\\n' '{}' ;; *) exit 97 ;; esac\n",
            bundle.info.to_string().replace('\'', "'\\''")
        );
        let path = format!("bin/{name}");
        fs::write(bundle.root().join(&path), code).unwrap();
        bundle.refresh(&path);
    }
    bundle
}

#[test]
fn trusted_bundle_relocates_with_exact_metadata_without_runtime_claims() {
    let temp = tempfile::tempdir().unwrap();
    let bundle = executable_bundle(host_target().unwrap(), "");
    let packaged = archive::pack(bundle.root(), &temp.path().join("archives")).unwrap();
    let archive = Path::new(packaged["archive"].as_str().unwrap());
    let destination = temp.path().join("relocated 'directory'");
    let result = smoke(archive, &destination, &[]).unwrap();
    assert_eq!(
        result,
        json!({"integrity_verified":true,"relocated_binaries_verified":true,"source_read_access_denied":false,"release_ready":false})
    );
    assert!(!destination.join("must-not-be-created").exists());
    assert!(smoke(archive, &destination, &[]).is_err());
}

#[test]
fn failed_empty_mismatched_and_stateful_executables_do_not_get_receipts() {
    for behavior in [
        "exit 23",
        "exit 0",
        "if [ \"$1\" = release-info ]; then echo '{}'; exit 0; fi",
        "mkdir -p \"$PROOFSTORM_HOME\"",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let bundle = executable_bundle(host_target().unwrap(), behavior);
        let packaged = archive::pack(bundle.root(), &temp.path().join("archives")).unwrap();
        let destination = temp.path().join("failed");
        assert!(
            smoke(
                Path::new(packaged["archive"].as_str().unwrap()),
                &destination,
                &[]
            )
            .is_err()
        );
        assert!(!destination.join("smoke-report.json").exists());
    }
}

#[test]
fn wrong_platform_and_corrupt_archives_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let other = if host_target().unwrap() == "x86_64-unknown-linux-gnu" {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let bundle = executable_bundle(other, "");
    let packaged = archive::pack(bundle.root(), &temp.path().join("archives")).unwrap();
    let archive = Path::new(packaged["archive"].as_str().unwrap());
    let destination = temp.path().join("wrong-host");
    assert!(
        smoke(archive, &destination, &[])
            .unwrap_err()
            .to_string()
            .contains("target host")
    );
    assert!(!destination.join("smoke-report.json").exists());
    fs::write(archive, "tampered").unwrap();
    assert!(smoke(archive, &temp.path().join("corrupt"), &[]).is_err());
    assert!(!temp.path().join("corrupt").exists());
}

#[test]
fn malformed_relocation_arguments_do_not_execute_anything() {
    for values in [
        vec![],
        vec!["archive"],
        vec!["archive", "output", "--unknown"],
        vec!["archive", "output", "--deny-source"],
    ] {
        assert!(cli(values.into_iter().map(OsString::from)).is_err());
    }
}
