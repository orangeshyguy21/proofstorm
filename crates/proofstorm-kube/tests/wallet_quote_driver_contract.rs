use std::{fs, path::Path, process::Command};

use rusqlite::Connection;
use serde_json::Value;

const DRIVER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/drivers/wallet_quote_driver.py"
);
const MINT_QUOTE: &str = "01234567-89ab-cdef-0123-456789abcdef";
const MELT_QUOTE: &str = "fedcba98-7654-3210-fedc-ba9876543210";

fn python() -> Option<&'static str> {
    ["python3", "python"]
        .into_iter()
        .find(|command| Command::new(command).arg("--version").output().is_ok())
}

fn wallet_fixture(root: &Path) -> Connection {
    let wallet = root.join(".cashu").join("recipient");
    fs::create_dir_all(&wallet).expect("wallet directory");
    let connection = Connection::open(wallet.join("wallet.sqlite3")).expect("wallet database");
    connection
        .execute_batch(&format!(
            "CREATE TABLE bolt11_mint_quotes (
               quote TEXT, mint TEXT, state TEXT, amount INTEGER,
               created_time INTEGER, paid_time INTEGER, expiry INTEGER, request TEXT
             );
             CREATE TABLE bolt11_melt_quotes (
               quote TEXT, state TEXT, amount INTEGER, fee_reserve INTEGER,
               fee_paid INTEGER, request TEXT, created_time INTEGER
             );
             INSERT INTO bolt11_mint_quotes VALUES (
               '{MINT_QUOTE}', 'http://recipient-mint:3338', 'UNPAID', 100,
               1, NULL, 301, 'lnbcrt-private-material'
             );
             INSERT INTO bolt11_melt_quotes VALUES (
               '{MELT_QUOTE}', 'UNPAID', 100, 2, NULL,
               'lnbcrt-private-material', 2
             );"
        ))
        .expect("quote schema and rows");
    connection
}

fn run_driver(python: &str, root: &Path, variables: &[(&str, &str)]) -> (bool, Value) {
    let mut command = Command::new(python);
    command
        .arg(DRIVER)
        .env("HOME", root)
        .env("PROOFSTORM_WALLET", "recipient")
        .env("PROOFSTORM_MINT", "recipient-mint")
        .env("PROOFSTORM_DB_TIMEOUT_SECONDS", "1")
        .env("PROOFSTORM_DB_RETRY_SECONDS", "0.05");
    for (name, value) in variables {
        command.env(name, value);
    }
    let output = command.output().expect("run wallet quote driver");
    let value = serde_json::from_slice(&output.stdout).expect("driver emits JSON");
    (output.status.success(), value)
}

#[test]
fn invoice_and_melt_observations_are_exact_and_sanitized() {
    let Some(python) = python() else {
        return;
    };
    let directory = tempfile::tempdir().expect("temporary wallet");
    let _connection = wallet_fixture(directory.path());
    let invoice_output = directory.path().join("invoice.log");
    fs::write(
        &invoice_output,
        format!("Pay lnbcrt-private-material with --id {MINT_QUOTE}\n"),
    )
    .expect("private invoice output");

    let (success, receive) = run_driver(
        python,
        directory.path(),
        &[
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "observe-invoice"),
            (
                "PROOFSTORM_INVOICE_OUTPUT_PATH",
                invoice_output.to_str().expect("invoice path"),
            ),
            ("PROOFSTORM_EXPECTED_MINT_URL", "http://recipient-mint:3338"),
        ],
    );
    assert!(success, "receive driver failed: {receive}");
    assert_eq!(receive["quote_id"], MINT_QUOTE);
    assert_eq!(receive["state"], "UNPAID");
    assert_eq!(receive["direction"], "receive");
    assert!(!receive.to_string().contains("lnbcrt"));

    let (success, melt) = run_driver(
        python,
        directory.path(),
        &[
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "observe-melt"),
            ("PROOFSTORM_INVOICE", "lnbcrt-private-material"),
            ("PROOFSTORM_MELT_BEFORE_IDS", "[]"),
        ],
    );
    assert!(success, "melt driver failed: {melt}");
    assert_eq!(melt["quote_id"], MELT_QUOTE);
    assert_eq!(melt["state"], "UNPAID");
    assert_eq!(melt["fee_reserve_sat"], 2);
    assert!(!melt.to_string().contains("lnbcrt"));
}

#[test]
fn already_issued_claim_is_idempotent_without_invoking_the_cli() {
    let Some(python) = python() else {
        return;
    };
    let directory = tempfile::tempdir().expect("temporary wallet");
    let connection = wallet_fixture(directory.path());
    connection
        .execute(
            "UPDATE bolt11_mint_quotes SET state = 'ISSUED', paid_time = 2 WHERE quote = ?1",
            [MINT_QUOTE],
        )
        .expect("issued quote");
    let (success, claim) = run_driver(
        python,
        directory.path(),
        &[
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "claim-receive"),
            ("PROOFSTORM_MINT_QUOTE_ID", MINT_QUOTE),
            ("PROOFSTORM_EXPECTED_MINT_URL", "http://recipient-mint:3338"),
        ],
    );
    assert!(success, "claim driver failed: {claim}");
    assert_eq!(claim["state"], "ISSUED");
    assert_eq!(claim["already_issued"], true);
    assert_eq!(claim["claim_exit_code"], 0);
    assert!(!claim.to_string().contains("lnbcrt"));
}

#[test]
fn melt_correlation_rejects_ambiguous_new_rows() {
    let Some(python) = python() else {
        return;
    };
    let directory = tempfile::tempdir().expect("temporary wallet");
    let connection = wallet_fixture(directory.path());
    connection
        .execute(
            "INSERT INTO bolt11_melt_quotes VALUES (
               'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee', 'PENDING', 100, 2, NULL,
               'lnbcrt-private-material', 3
             )",
            [],
        )
        .expect("second melt quote");
    let (success, error) = run_driver(
        python,
        directory.path(),
        &[
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "observe-melt"),
            ("PROOFSTORM_INVOICE", "lnbcrt-private-material"),
            ("PROOFSTORM_MELT_BEFORE_IDS", "[]"),
        ],
    );
    assert!(!success);
    assert_eq!(error["reason"], "melt_quote_ambiguous");
    assert!(!error.to_string().contains("lnbcrt"));
}
