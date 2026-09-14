use super::*;
use native::Output;
use rusqlite::params;
use std::cell::Cell;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const RECEIVE: &str = "01234567-89ab-cdef-0123-456789abcdef";
const MELT: &str = "fedcba98-7654-3210-fedc-ba9876543210";
const INVOICE: &str = "lnbcrt-private-material";

struct Fixture {
    root: TempDir,
    db: Connection,
    config: Config,
}
impl Fixture {
    fn new(name: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(".cashu/wallet");
        fs::create_dir_all(&directory).unwrap();
        let db = Connection::open(directory.join("wallet.sqlite3")).unwrap();
        db.execute_batch("CREATE TABLE bolt11_mint_quotes (
            quote TEXT, mint TEXT, state TEXT, amount INTEGER, created_time INTEGER,
            paid_time INTEGER, expiry INTEGER, request TEXT);
            CREATE TABLE bolt11_melt_quotes (
            quote TEXT, state TEXT, amount INTEGER, fee_reserve INTEGER, fee_paid INTEGER,
            request TEXT, created_time INTEGER, mint TEXT, unit TEXT, paid_time INTEGER,
            payment_preimage TEXT);
            CREATE TABLE proofs (amount INTEGER, C TEXT, secret TEXT UNIQUE, reserved INTEGER,
            melt_id TEXT, time_reserved INTEGER, id TEXT, derivation_path TEXT, mint_id TEXT, p2pk_e TEXT);
            CREATE TABLE proofs_used (amount INTEGER, C TEXT, secret TEXT UNIQUE, time_used INTEGER,
            id TEXT, derivation_path TEXT, mint_id TEXT, melt_id TEXT, p2pk_e TEXT);
            CREATE TABLE keysets (id TEXT, mint_url TEXT, input_fee_ppk INTEGER);").unwrap();
        db.execute(
            "INSERT INTO bolt11_mint_quotes VALUES (?1,?2,'UNPAID',100,1,NULL,301,?3)",
            params![RECEIVE, format!("http://{name}-mint:3338"), INVOICE],
        )
        .unwrap();
        let config = Config {
            variables: [
                ("HOME".into(), root.path().to_str().unwrap().into()),
                ("PROOFSTORM_WALLET".into(), name.into()),
                ("PROOFSTORM_MINT".into(), format!("{name}-mint")),
                (
                    "PROOFSTORM_EXPECTED_MINT_URL".into(),
                    format!("http://{name}-mint:3338"),
                ),
                ("PROOFSTORM_DB_TIMEOUT_SECONDS".into(), "1".into()),
                ("PROOFSTORM_DB_RETRY_SECONDS".into(), "0.05".into()),
                ("PROOFSTORM_MINT_QUOTE_ID".into(), RECEIVE.into()),
                ("PROOFSTORM_MELT_QUOTE_ID".into(), MELT.into()),
                ("PROOFSTORM_INVOICE".into(), INVOICE.into()),
            ]
            .into(),
        };
        Self { root, db, config }
    }
    fn melt(&self) {
        self.db.execute("INSERT INTO bolt11_melt_quotes VALUES (?1,'UNPAID',100,2,NULL,?2,2,?3,'sat',NULL,NULL)",
            params![MELT,INVOICE,self.config.get("PROOFSTORM_EXPECTED_MINT_URL").unwrap()]).unwrap();
    }
    fn reserve(&self) {
        self.db.execute("INSERT INTO proofs(amount,secret,reserved,melt_id,id,C) VALUES
            (60,'secret-a',1,?1,'keyset-a','point-a'),(42,'secret-b',1,?1,'keyset-a','point-b'),
            (7,'secret-c',0,NULL,'keyset-a','point-c'),(13,'secret-d',NULL,NULL,'keyset-a','point-d')",[MELT]).unwrap();
    }
    fn set(&mut self, name: &str, value: impl Into<String>) {
        self.config.variables.insert(name.into(), value.into());
    }
}

fn private(value: &Value) {
    let serialized = value.to_string();
    for secret in [
        INVOICE,
        "secret-a",
        "secret-b",
        "private-preimage",
        "point-a",
    ] {
        assert!(!serialized.contains(secret));
    }
}

#[test]
fn invoice_and_melt_observations_are_exact_and_sanitized() {
    let mut fixture = Fixture::new("recipient");
    fixture.melt();
    let output = fixture.root.path().join("invoice.log");
    fs::write(&output, format!("Pay {INVOICE} with --id {RECEIVE}\n")).unwrap();
    fixture.set("PROOFSTORM_INVOICE_OUTPUT_PATH", output.to_str().unwrap());
    let receive = observe("observe-invoice", &fixture.config).unwrap();
    assert_eq!(receive["mint_quote_id"], RECEIVE);
    assert_eq!(receive["quote_observations"][0]["state"], "UNPAID");
    assert_eq!(receive["quote_observations"][0]["direction"], "receive");
    assert_eq!(
        receive["quote_observations"][0]["wallet_created_at_unix"],
        1
    );
    private(&receive);
    let melt = observe("observe-melt", &fixture.config).unwrap();
    assert_eq!(melt["quote_id"], MELT);
    assert_eq!(melt["state"], "UNPAID");
    assert_eq!(melt["fee_reserve_sat"], 2);
    private(&melt);
    fixture.db.execute("INSERT INTO bolt11_melt_quotes SELECT 'another-quote',state,amount,fee_reserve,fee_paid,request,3,mint,unit,paid_time,payment_preimage FROM bolt11_melt_quotes",[]).unwrap();
    assert_eq!(
        observe("observe-melt", &fixture.config).unwrap_err().0,
        "melt_quote_ambiguous"
    );
    fixture.set("PROOFSTORM_MELT_BEFORE_IDS", r#"["another-quote"]"#);
    assert_eq!(
        observe("observe-melt", &fixture.config).unwrap()["quote_id"],
        MELT
    );
}

#[test]
fn fee_evidence_uses_the_mint_database_and_never_infers_a_missing_fee() {
    let mut fixture = Fixture::new("recipient");
    fixture.melt();
    fixture
        .db
        .execute("UPDATE bolt11_melt_quotes SET state='PAID',fee_paid=93", [])
        .unwrap();
    let mint = tempfile::tempdir().unwrap();
    fixture.set("PROOFSTORM_MINT_DB_DIR", mint.path().to_str().unwrap());
    assert!(observe("observe-melt", &fixture.config).unwrap()["fee_paid_sat"].is_null());
    let db = Connection::open(mint.path().join("mint.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE melt_quotes (quote TEXT,state TEXT,amount INTEGER,fee_reserve INTEGER,fee_paid INTEGER)").unwrap();
    db.execute("INSERT INTO melt_quotes VALUES (?1,'PAID',100,2,1)", [MELT])
        .unwrap();
    let observation = observe("observe-melt", &fixture.config).unwrap();
    assert_eq!(observation["fee_paid_sat"], 1);
    private(&observation);
}

struct FakeCli {
    fail_claim: Cell<bool>,
    calls: Cell<usize>,
}
impl FakeCli {
    fn new() -> Self {
        Self {
            fail_claim: Cell::new(true),
            calls: Cell::new(0),
        }
    }
}
impl WalletCli for FakeCli {
    async fn run(
        &self,
        home: &str,
        _wallet: &str,
        _mint: &str,
        args: &[&str],
        _duration: Duration,
    ) -> Result<Output> {
        self.calls.set(self.calls.get() + 1);
        let db = Connection::open(Path::new(home).join(".cashu/wallet").join("wallet.sqlite3"))?;
        let mut code = 0;
        match args {
            ["pay", invoice] => {
                assert_eq!(*invoice, INVOICE);
                db.execute("INSERT INTO bolt11_melt_quotes(quote,state,amount,fee_reserve,fee_paid,request,created_time) VALUES (?1,'PAID',100,2,1,?2,3)",params![MELT,invoice])?;
                db.execute(
                    "INSERT INTO proofs_used(id,melt_id) VALUES ('keyset-a',?1)",
                    [MELT],
                )?;
            }
            ["invoice", "100", "--id", id] => {
                assert_eq!(*id, RECEIVE);
                if self.fail_claim.get() {
                    code = 124;
                } else {
                    db.execute(
                        "UPDATE bolt11_mint_quotes SET state='ISSUED',paid_time=4 WHERE quote=?1",
                        [id],
                    )?;
                }
            }
            ["balance"] => {
                return Ok(Output {
                    code: 0,
                    stdout: b"Private CLI text\nBalance: 100\n".to_vec(),
                    truncated: false,
                });
            }
            _ => panic!("unexpected wallet command"),
        }
        Ok(Output {
            code,
            stdout: INVOICE.as_bytes().to_vec(),
            truncated: false,
        })
    }
}

#[tokio::test]
async fn already_issued_claim_is_idempotent_without_a_wallet_command() {
    let fixture = Fixture::new("recipient");
    fixture
        .db
        .execute(
            "UPDATE bolt11_mint_quotes SET state='ISSUED',paid_time=2",
            [],
        )
        .unwrap();
    let cli = FakeCli::new();
    let result = claim(&fixture.config, &cli).await.unwrap();
    assert_eq!(result["already_issued"], true);
    assert_eq!(result["claim_exit_code"], 0);
    assert_eq!(cli.calls.get(), 0);
    private(&result);
}

#[tokio::test]
async fn payment_survives_a_failed_claim_and_can_be_claimed_explicitly() {
    let mut payer = Fixture::new("payer");
    let recipient = Fixture::new("recipient");
    payer
        .db
        .execute(
            "INSERT INTO keysets VALUES ('keyset-a','http://payer-mint:3338',100)",
            [],
        )
        .unwrap();
    for (key, value) in [
        (
            "PROOFSTORM_RECIPIENT_HOME",
            recipient.config.get("HOME").unwrap(),
        ),
        ("PROOFSTORM_RECIPIENT_WALLET", "recipient"),
        ("PROOFSTORM_RECIPIENT_MINT", "recipient-mint"),
        (
            "PROOFSTORM_RECIPIENT_MINT_URL",
            "http://recipient-mint:3338",
        ),
    ] {
        payer.set(key, value);
    }
    let cli = FakeCli::new();
    let paid = pay(&payer.config, &cli).await.unwrap();
    assert_eq!(paid["melt_quote_id"], MELT);
    assert_eq!(paid["quote_observations"][0]["state"], "PAID");
    assert_eq!(paid["quote_observations"][1]["state"], "UNPAID");
    assert_eq!(paid["claim_exit_code"], 124);
    assert_eq!(paid["payer_balance_sat"], 100);
    assert_eq!(paid["input_fee_sat"], 1);
    assert_eq!(paid["input_proof_count"], 1);
    assert_eq!(paid["code"], "payment_paid_claim_unverified");
    assert!(paid["quote_observations"][0].get("input_fee_sat").is_none());
    private(&paid);
    cli.fail_claim.set(false);
    let recovered = claim(&recipient.config, &cli).await.unwrap();
    assert_eq!(recovered["already_issued"], false);
    assert_eq!(recovered["quote_observations"][0]["state"], "ISSUED");
    private(&recovered);
}

#[test]
fn fee_accounting_rejects_unknown_inputs_and_rounds_the_total_once() {
    let fixture = Fixture::new("payer");
    fixture.melt();
    fixture
        .db
        .execute("UPDATE bolt11_melt_quotes SET state='PAID'", [])
        .unwrap();
    fixture
        .db
        .execute(
            "INSERT INTO proofs_used(id,melt_id) VALUES ('keyset-a',?1),('keyset-b',?1)",
            [MELT],
        )
        .unwrap();
    let wallet = fixture.config.wallet(None, None).unwrap();
    let melt = Melt::by_id(&wallet, MELT).unwrap();
    let mint = "http://payer-mint:3338";
    assert_eq!(
        input_fee(&wallet, &melt, mint).unwrap_err().0,
        "melt_input_keyset_missing"
    );
    fixture
        .db
        .execute(
            "INSERT INTO keysets VALUES ('keyset-a',?1,501),('keyset-b',?1,500)",
            [mint],
        )
        .unwrap();
    assert_eq!(input_fee(&wallet, &melt, mint).unwrap(), (2, 2));
    let mut unpaid = melt;
    unpaid.state = "UNPAID".into();
    assert_eq!(
        input_fee(&wallet, &unpaid, mint).unwrap_err().0,
        "unpaid_melt_spent_proofs_present"
    );
}

async fn refresh_fixture(remote: Value) -> (Fixture, tokio::task::JoinHandle<()>) {
    refresh_fixture_changed(remote, None).await
}

async fn refresh_fixture_changed(
    remote: Value,
    change: Option<&'static str>,
) -> (Fixture, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut fixture = Fixture::new("recipient");
    fixture.set(
        "PROOFSTORM_EXPECTED_MINT_URL",
        format!("http://{}", listener.local_addr().unwrap()),
    );
    fixture.melt();
    fixture.reserve();
    let database = fixture.root.path().join(".cashu/wallet/wallet.sqlite3");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 1024];
            let size = stream.read(&mut buffer).await.unwrap();
            assert!(size > 0 && request.len() < 8192);
            request.extend_from_slice(&buffer[..size]);
            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        assert!(
            String::from_utf8(request)
                .unwrap()
                .starts_with(&format!("GET /v1/melt/quote/bolt11/{MELT} HTTP/1.1\r\n"))
        );
        if let Some(change) = change {
            Connection::open(database)
                .unwrap()
                .execute_batch(change)
                .unwrap();
        }
        let body = remote.to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    (fixture, server)
}

#[tokio::test]
async fn refresh_rejects_proof_and_quote_changes_while_the_mint_request_is_in_flight() {
    for change in [
        "UPDATE proofs SET C='changed-point' WHERE secret='secret-a'",
        "UPDATE bolt11_melt_quotes SET request='changed-private-invoice'",
        "UPDATE bolt11_melt_quotes SET mint='http://different-mint:3338'",
        "UPDATE bolt11_melt_quotes SET state='PAID'",
    ] {
        let (fixture, server) = refresh_fixture_changed(
            json!({"quote":MELT,"state":"UNPAID","amount":100,"fee_reserve":2}),
            Some(change),
        )
        .await;
        assert_eq!(
            refresh::run(&fixture.config).await.unwrap_err().0,
            "melt_quote_changed_during_refresh"
        );
        server.await.unwrap();
        assert_eq!(
            fixture
                .db
                .query_row("SELECT COUNT(*) FROM proofs WHERE reserved", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}

#[tokio::test]
async fn refresh_unpaid_releases_only_the_matching_wallet_reservations() {
    let (fixture, server) =
        refresh_fixture(json!({"quote":MELT,"state":"UNPAID","amount":100,"fee_reserve":2})).await;
    // An auth wallet sorts first but its independent proofs must not be used.
    let auth = Connection::open(fixture.root.path().join(".cashu/wallet/auth.sqlite3")).unwrap();
    auth.execute_batch("CREATE TABLE proofs(amount INTEGER); INSERT INTO proofs VALUES(999)")
        .unwrap();
    let result = refresh::run(&fixture.config).await.unwrap();
    server.await.unwrap();
    assert_eq!(result["reserved_proof_count_before"], 2);
    assert_eq!(result["reserved_proof_count_after"], 0);
    assert_eq!(result["reserved_sat_before"], 102);
    assert_eq!(result["available_balance_sat_before"], 20);
    assert_eq!(result["available_balance_sat_after"], 122);
    assert_eq!(result["proofs_released"], true);
    assert!(result["quote_observations"][0]["fee_paid_sat"].is_null());
    assert_eq!(
        auth.query_row("SELECT amount FROM proofs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        999
    );
    private(&result);
}

#[tokio::test]
async fn refresh_pending_preserves_reservations_and_wrong_identity_changes_nothing() {
    for (state, amount, accepted) in [
        ("PENDING", 100, true),
        ("UNPAID", 101, false),
        ("UNKNOWN", 100, false),
    ] {
        let (fixture, server) =
            refresh_fixture(json!({"quote":MELT,"state":state,"amount":amount,"fee_reserve":2}))
                .await;
        let result = refresh::run(&fixture.config).await;
        server.await.unwrap();
        assert_eq!(result.is_ok(), accepted);
        assert_eq!(
            fixture
                .db
                .query_row("SELECT SUM(amount) FROM proofs WHERE reserved", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            102
        );
    }
}

#[tokio::test]
async fn paid_refresh_retires_inputs_atomically_and_does_not_report_a_legacy_fee() {
    let (fixture,server)=refresh_fixture(json!({"quote":MELT,"state":"PAID","amount":100,"fee_reserve":2,"payment_preimage":"private-preimage"})).await;
    let result = refresh::run(&fixture.config).await.unwrap();
    server.await.unwrap();
    assert_eq!(result["state_after"], "PAID");
    assert_eq!(result["available_balance_sat_after"], 20);
    assert!(result["quote_observations"][0]["fee_paid_sat"].is_null());
    assert_eq!(
        fixture
            .db
            .query_row(
                "SELECT COUNT(*) FROM proofs_used WHERE melt_id=?1",
                [MELT],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        fixture
            .db
            .query_row("SELECT COUNT(*) FROM proofs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    private(&result);
}

#[tokio::test]
async fn a_failed_retirement_rolls_back_the_quote_and_all_proofs() {
    let (fixture, server) =
        refresh_fixture(json!({"quote":MELT,"state":"PAID","amount":100,"fee_reserve":2})).await;
    fixture
        .db
        .execute("INSERT INTO proofs_used(secret) VALUES ('secret-a')", [])
        .unwrap();
    assert!(refresh::run(&fixture.config).await.is_err());
    server.await.unwrap();
    assert_eq!(
        fixture
            .db
            .query_row("SELECT state FROM bolt11_melt_quotes", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "UNPAID"
    );
    assert_eq!(
        fixture
            .db
            .query_row("SELECT COUNT(*) FROM proofs WHERE reserved", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn wallet_names_and_duplicate_database_quotes_fail_closed() {
    let mut fixture = Fixture::new("recipient");
    fixture.set("PROOFSTORM_WALLET", "../recipient");
    assert!(fixture.config.wallet(None, None).is_err());
    fixture.set("PROOFSTORM_WALLET", "recipient");
    let path = fixture.root.path().join(".cashu/wallet");
    fs::copy(path.join("wallet.sqlite3"), path.join("duplicate.sqlite3")).unwrap();
    assert_eq!(
        Receive::read(&fixture.config.wallet(None, None).unwrap(), RECEIVE, None)
            .err()
            .unwrap()
            .0,
        "wallet_quote_ambiguous"
    );
}

#[test]
fn legacy_named_wallets_are_not_silently_selected_or_migrated() {
    let fixture = Fixture::new("recipient");
    let canonical = fixture.root.path().join(".cashu/wallet");
    let legacy = fixture.root.path().join(".cashu/recipient");
    fs::rename(&canonical, &legacy).unwrap();
    assert_eq!(
        fixture.config.wallet(None, None).err().unwrap().0,
        "wallet_database_missing"
    );
    assert!(!canonical.exists());
    assert!(legacy.join("wallet.sqlite3").is_file());
}
