use std::{fs, path::Path, process::Command};

use proofstorm_core::wallet_quote_observations_from_artifact;
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
             CREATE TABLE proofs (
               amount INTEGER, reserved INTEGER, melt_id TEXT
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
    assert_eq!(receive["mint_quote_id"], MINT_QUOTE);
    assert_eq!(receive["quote_observations"][0]["quote_id"], MINT_QUOTE);
    assert_eq!(receive["quote_observations"][0]["state"], "UNPAID");
    assert_eq!(receive["quote_observations"][0]["direction"], "receive");
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
fn melt_fee_comes_from_the_authoritative_mint_database() {
    let Some(python) = python() else {
        return;
    };
    let wallet = tempfile::tempdir().expect("wallet directory");
    let wallet_connection = wallet_fixture(wallet.path());
    wallet_connection
        .execute(
            "UPDATE bolt11_melt_quotes SET state = 'PAID', fee_paid = 93 WHERE quote = ?1",
            [MELT_QUOTE],
        )
        .expect("legacy wallet-local fee value");

    let mint = tempfile::tempdir().expect("mint directory");
    let mint_connection =
        Connection::open(mint.path().join("mint.sqlite3")).expect("authoritative mint database");
    mint_connection
        .execute_batch(&format!(
            "CREATE TABLE melt_quotes (
               quote TEXT, state TEXT, amount INTEGER,
               fee_reserve INTEGER, fee_paid INTEGER
             );
             INSERT INTO melt_quotes VALUES ('{MELT_QUOTE}', 'PAID', 100, 2, 1);"
        ))
        .expect("authoritative melt quote");

    let (success, melt) = run_driver(
        python,
        wallet.path(),
        &[
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "observe-melt"),
            ("PROOFSTORM_INVOICE", "lnbcrt-private-material"),
            ("PROOFSTORM_MELT_BEFORE_IDS", "[]"),
            (
                "PROOFSTORM_MINT_DB_DIR",
                mint.path().to_str().expect("mint path"),
            ),
        ],
    );
    assert!(success, "melt driver failed: {melt}");
    assert_eq!(melt["fee_paid_sat"], 1);
    assert_ne!(melt["fee_paid_sat"], 93);
}

#[test]
fn melt_refresh_polls_the_mint_and_proves_reserved_proofs_were_released() {
    let Some(python) = python() else {
        return;
    };
    let directory = tempfile::tempdir().expect("temporary wallet");
    let connection = wallet_fixture(directory.path());
    connection
        .execute(
            "INSERT INTO proofs VALUES (60, 1, ?1), (42, 1, ?1), (7, 0, NULL), (13, NULL, NULL)",
            [MELT_QUOTE],
        )
        .expect("reserved proofs");

    let module = directory.path().join("cashu/wallet");
    fs::create_dir_all(&module).expect("fake cashu wallet module");
    for package in ["cashu", "cashu/wallet"] {
        fs::write(directory.path().join(package).join("__init__.py"), "").expect("package marker");
    }
    fs::write(
        module.join("wallet.py"),
        r#"import os, sqlite3
class Quote:
    state = "UNPAID"
class Wallet:
    def __init__(self, db):
        self.db = db
    @classmethod
    async def with_db(cls, mint, db, name=None, unit=None):
        return cls(os.path.join(db, "wallet.sqlite3"))
    async def get_melt_quote(self, quote_id):
        connection = sqlite3.connect(self.db)
        connection.execute(
            "UPDATE proofs SET reserved = 0, melt_id = NULL WHERE melt_id = ?",
            (quote_id,),
        )
        connection.commit()
        connection.close()
        return Quote()
"#,
    )
    .expect("fake wallet API");

    let (success, artifact) = run_driver(
        python,
        directory.path(),
        &[
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "refresh-melt"),
            ("PROOFSTORM_MELT_QUOTE_ID", MELT_QUOTE),
            ("PROOFSTORM_EXPECTED_MINT_URL", "http://payer-mint:3338"),
            (
                "PYTHONPATH",
                directory.path().to_str().expect("python module root"),
            ),
        ],
    );
    assert!(success, "refresh driver failed: {artifact}");
    assert_eq!(artifact["melt_quote_id"], MELT_QUOTE);
    assert_eq!(artifact["state_before"], "UNPAID");
    assert_eq!(artifact["state_after"], "UNPAID");
    assert_eq!(artifact["reserved_proof_count_before"], 2);
    assert_eq!(artifact["reserved_proof_count_after"], 0);
    assert_eq!(artifact["reserved_sat_before"], 102);
    assert_eq!(artifact["reserved_sat_after"], 0);
    assert_eq!(artifact["available_balance_sat_before"], 20);
    assert_eq!(artifact["available_balance_sat_after"], 122);
    assert_eq!(artifact["proofs_released"], true);
    assert_eq!(artifact["quote_observations"][0]["direction"], "pay");
    assert!(!artifact.to_string().contains("lnbcrt"));
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
    assert_eq!(claim["quote_observations"][0]["state"], "ISSUED");
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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the end-to-end fixture keeps payment, exact fee evidence, and explicit claim recovery in one lifecycle"
)]
fn paid_but_unclaimed_melt_is_recoverable_with_explicit_claim() {
    let Some(python) = python() else {
        return;
    };
    let payer = tempfile::tempdir().expect("payer wallet");
    let recipient = tempfile::tempdir().expect("recipient wallet");
    let recipient_connection = wallet_fixture(recipient.path());
    let payer_wallet = payer.path().join(".cashu").join("payer");
    fs::create_dir_all(&payer_wallet).expect("payer directory");
    let payer_connection =
        Connection::open(payer_wallet.join("wallet.sqlite3")).expect("payer database");
    payer_connection
        .execute_batch(
            "CREATE TABLE bolt11_melt_quotes (
           quote TEXT, state TEXT, amount INTEGER, fee_reserve INTEGER,
           fee_paid INTEGER, request TEXT, created_time INTEGER
         );
         CREATE TABLE keysets (
           id TEXT, mint_url TEXT, input_fee_ppk INTEGER
         );
         CREATE TABLE proofs_used (
           id TEXT, melt_id TEXT
         );
         INSERT INTO keysets VALUES ('keyset-a', 'http://payer-mint:3338', 100);",
        )
        .expect("payer quote schema");

    let module = payer.path().join("cashu/wallet/cli");
    fs::create_dir_all(&module).expect("fake cashu module");
    for package in ["cashu", "cashu/wallet", "cashu/wallet/cli"] {
        fs::write(payer.path().join(package).join("__init__.py"), "").expect("package marker");
    }
    fs::write(
        module.join("cli.py"),
        r#"import os, sqlite3, sys
def cli():
    args = sys.argv
    wallet = args[args.index('-w') + 1]
    db = os.path.join(os.environ['HOME'], '.cashu', wallet, 'wallet.sqlite3')
    command = args[args.index('-y') + 1]
    connection = sqlite3.connect(db)
    if command == 'pay':
        quote = 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee'
        connection.execute("INSERT INTO bolt11_melt_quotes VALUES (?, 'PAID', 100, 2, 1, ?, 3)", (quote, args[-1]))
        connection.execute("INSERT INTO proofs_used VALUES ('keyset-a', ?)", (quote,))
        connection.commit()
    elif command == 'invoice' and os.environ.get('PROOFSTORM_FAKE_CLAIM_FAILURE') != '1':
        quote = args[args.index('--id') + 1]
        connection.execute("UPDATE bolt11_mint_quotes SET state = 'ISSUED', paid_time = 4 WHERE quote = ?", (quote,))
        connection.commit()
    elif command == 'balance':
        print('Balance: 100')
"#,
    ).expect("fake cashu CLI");

    let recipient_home = recipient.path().to_str().expect("recipient path");
    let python_path = payer.path().to_str().expect("python path");
    let (success, artifact) = run_driver(
        python,
        payer.path(),
        &[
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "pay-and-claim"),
            ("PROOFSTORM_WALLET", "payer"),
            ("PROOFSTORM_MINT", "payer-mint"),
            ("PROOFSTORM_EXPECTED_MINT_URL", "http://payer-mint:3338"),
            ("PROOFSTORM_RECIPIENT_HOME", recipient_home),
            ("PROOFSTORM_RECIPIENT_WALLET", "recipient"),
            ("PROOFSTORM_RECIPIENT_MINT", "recipient-mint"),
            (
                "PROOFSTORM_RECIPIENT_MINT_URL",
                "http://recipient-mint:3338",
            ),
            ("PROOFSTORM_MINT_QUOTE_ID", MINT_QUOTE),
            ("PROOFSTORM_FAKE_CLAIM_FAILURE", "1"),
            ("PYTHONPATH", python_path),
        ],
    );
    assert!(success, "compound payment driver failed: {artifact}");
    assert_eq!(
        artifact["melt_quote_id"],
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
    );
    assert_eq!(artifact["quote_observations"][0]["state"], "PAID");
    assert_eq!(artifact["quote_observations"][1]["state"], "UNPAID");
    assert_eq!(artifact["payer_balance_sat"], 100);
    assert_eq!(artifact["input_fee_sat"], 1);
    assert_eq!(artifact["input_proof_count"], 1);
    assert!(
        artifact["quote_observations"][0]
            .get("input_fee_sat")
            .is_none()
    );
    assert!(
        artifact["quote_observations"][0]
            .get("input_proof_count")
            .is_none()
    );
    assert!(wallet_quote_observations_from_artifact(&artifact).is_ok());
    assert_eq!(artifact["code"], "payment_paid_claim_unverified");
    assert!(!artifact.to_string().contains("lnbcrt"));

    let (success, claim) = run_driver(
        python,
        recipient.path(),
        &[
            ("PROOFSTORM_QUOTE_DRIVER_MODE", "claim-receive"),
            ("PROOFSTORM_MINT_QUOTE_ID", MINT_QUOTE),
            ("PROOFSTORM_EXPECTED_MINT_URL", "http://recipient-mint:3338"),
            ("PYTHONPATH", python_path),
        ],
    );
    assert!(success, "explicit recovery claim failed: {claim}");
    assert_eq!(claim["quote_observations"][0]["state"], "ISSUED");
    assert_eq!(claim["already_issued"], false);
    assert!(!claim.to_string().contains("lnbcrt"));
    let state: String = recipient_connection
        .query_row(
            "SELECT state FROM bolt11_mint_quotes WHERE quote = ?1",
            [MINT_QUOTE],
            |row| row.get(0),
        )
        .expect("claimed receive state");
    assert_eq!(state, "ISSUED");
}
