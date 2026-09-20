use super::*;
use rusqlite::params;
use tempfile::TempDir;

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
fn retired_mutation_modes_refuse_before_accessing_wallet_state() {
    let fixture = Fixture::new("recipient");
    fixture.melt();
    fixture.reserve();
    let path = fixture.root.path().join(".cashu/wallet/wallet.sqlite3");
    let before = fs::read(&path).unwrap();
    for mode in [
        "claim-receive",
        "refresh-melt",
        "pay-and-claim",
        "observe-invoice",
    ] {
        assert_eq!(
            observe(mode, &fixture.config).unwrap_err().0,
            "quote_driver_mode_invalid"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        // Even invalid configuration must be rejected by dispatch, before I/O.
        let missing = Config {
            variables: BTreeMap::new(),
        };
        assert_eq!(
            observe(mode, &missing).unwrap_err().0,
            "quote_driver_mode_invalid"
        );
    }
}

#[test]
fn invoice_and_melt_observations_are_exact_and_sanitized() {
    let mut fixture = Fixture::new("recipient");
    fixture.melt();
    fixture.set("PROOFSTORM_OBSERVATION_ROLE", "invoice_receive");
    let receive = observe("observe-receive", &fixture.config).unwrap();
    assert_eq!(receive["quote_id"], RECEIVE);
    assert_eq!(receive["state"], "UNPAID");
    assert_eq!(receive["direction"], "receive");
    assert_eq!(receive["wallet_created_at_unix"], 1);
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
    assert_eq!(
        observe("observe-mint-melt", &fixture.config).unwrap_err().0,
        "mint_database_missing"
    );
    let db = Connection::open(mint.path().join("mint.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE melt_quotes (quote TEXT,state TEXT,amount INTEGER,fee_reserve INTEGER,fee_paid INTEGER)").unwrap();
    db.execute("INSERT INTO melt_quotes VALUES (?1,'PAID',100,2,1)", [MELT])
        .unwrap();
    let observation = observe("observe-mint-melt", &fixture.config).unwrap();
    assert_eq!(observation["fee_paid_sat"], 1);
    private(&observation);
    fixture.config.variables.remove("HOME");
    db.execute("UPDATE melt_quotes SET fee_paid=NULL", [])
        .unwrap();
    assert!(observe("observe-mint-melt", &fixture.config).unwrap()["fee_paid_sat"].is_null());
    fixture.set("PROOFSTORM_MELT_QUOTE_ID", "different");
    assert_eq!(
        observe("observe-mint-melt", &fixture.config).unwrap_err().0,
        "mint_melt_quote_missing"
    );
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
    let wallet = fixture.config.wallet().unwrap();
    let melt = Melt::correlate(&wallet, INVOICE, &BTreeSet::new()).unwrap();
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

#[test]
fn wallet_names_and_duplicate_database_quotes_fail_closed() {
    let mut fixture = Fixture::new("recipient");
    fixture.set("PROOFSTORM_WALLET", "../recipient");
    assert!(fixture.config.wallet().is_err());
    fixture.set("PROOFSTORM_WALLET", "recipient");
    let path = fixture.root.path().join(".cashu/wallet");
    fs::copy(path.join("wallet.sqlite3"), path.join("duplicate.sqlite3")).unwrap();
    assert_eq!(
        Receive::read(&fixture.config.wallet().unwrap(), RECEIVE, None)
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
        fixture.config.wallet().err().unwrap().0,
        "wallet_database_missing"
    );
    assert!(!canonical.exists());
    assert!(legacy.join("wallet.sqlite3").is_file());
}
