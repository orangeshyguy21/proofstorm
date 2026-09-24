//! Run the real shell fixture against deterministic CLI/observer stand-ins.
//! This covers both race orderings without requiring a lucky live schedule.
use super::{DRIVER, Receipt};
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

const CLI: &str = r#"#!/bin/sh
set -eu
work=$2
shift 5
mkdir -p "$work"
slot=${work##*/}
case "$1" in
    balance) printf '%s\n' "$slot" > "$work/seed" ;;
    send)
        case "$*" in
            *32) printf '32\n' > "$work/balance" ;;
            *16) printf '16\n' > "$work/balance" ;;
            *) exit 9 ;;
        esac
        printf 'cashuATEST\n'
        ;;
    receive)
        case "$slot" in
            recipient)
                if [ ! -f "$work/balance" ]; then printf '32\n' > "$work/balance"; exit 0; fi
                ;;
            race-replay)
                printf '%s\n' "$FIXTURE_FINAL_ERROR" >&2
                exit 1
                ;;
            race-a|race-b)
                if [ "$slot" = "race-$FIXTURE_WINNER" ]; then
                    printf '16\n' > "$work/balance"
                    exit 0
                fi
                printf '%s\n' "$FIXTURE_RACE_ERROR" >&2
                exit "$FIXTURE_RACE_RC"
                ;;
        esac
        printf 'Token Already Spent\n' >&2
        exit 1
        ;;
    *) exit 9 ;;
esac
"#;

fn executable(path: &std::path::Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn exercise(winner: &str, error: &str, code: &str, final_error: &str) -> Receipt {
    let root = tempfile::TempDir::new().unwrap();
    let wallet = root.path().join("wallet");
    fs::create_dir_all(wallet.join("cdk")).unwrap();
    fs::write(wallet.join("cdk/balance"), "64\n").unwrap();
    executable(&root.path().join("cdk-cli"), CLI);
    executable(
        &root.path().join("timeout"),
        "#!/bin/sh\nshift 3\nexec \"$@\"\n",
    );
    executable(
        &root.path().join("observer"),
        r#"#!/bin/sh
balance=${PROOFSTORM_DATABASE%/*}/balance
if [ -f "$balance" ]; then cat "$balance"; else printf '0\n'; fi
"#,
    );
    let driver = DRIVER.replace("/wallet", wallet.to_str().unwrap()).replace(
        "/opt/proofstorm/driver",
        root.path().join("observer").to_str().unwrap(),
    );
    let script = root.path().join("fixture.sh");
    fs::write(&script, driver).unwrap();
    let output = Command::new("sh")
        .arg(script)
        .env(
            "PATH",
            format!(
                "{}:{}",
                root.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("FIXTURE_WINNER", winner)
        .env("FIXTURE_RACE_ERROR", error)
        .env("FIXTURE_RACE_RC", code)
        .env("FIXTURE_FINAL_ERROR", final_error)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn race_accepts_spent_or_pending_for_either_loser_only_with_final_spent_evidence() {
    for winner in ["a", "b"] {
        for error in ["Token Already Spent", "Token Pending", "proofs are pending"] {
            exercise(winner, error, "1", "Token Already Spent")
                .verify()
                .unwrap();
        }
    }
    for (error, code, final_error) in [
        ("connection refused", "1", "Token Already Spent"),
        ("Token Pending", "124", "Token Already Spent"),
        ("Token Pending", "1", "Token Pending"),
        ("Token Pending", "1", "connection refused"),
        ("request pending", "1", "Token Already Spent"),
    ] {
        assert!(exercise("a", error, code, final_error).verify().is_err());
    }
}

#[test]
fn public_failure_contains_typed_receipt_without_private_error_text() {
    let receipt = exercise("b", "connection refused", "1", "Token Already Spent");
    let error = receipt
        .verify()
        .unwrap_err()
        .context(receipt)
        .context("private-token-log");
    let diagnostic = crate::diagnostics::gate_failure(&error);
    assert_eq!(
        diagnostic["cashu_double_spend"]["race_rc"],
        serde_json::json!([1, 0])
    );
    assert_eq!(
        diagnostic["cashu_double_spend"]["race_balance"],
        serde_json::json!([0, 16])
    );
    assert!(!diagnostic.to_string().contains("private-token-log"));
    assert!(!diagnostic.to_string().contains("connection refused"));
}
