#![cfg(feature = "observation")]
use proofstorm_driver::wallet;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::fs;

const MINT: &str = "http://mint:3338";

#[test]
fn cdk_preserves_live_transactions_and_rejects_invalid_or_missing_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wallet with spaces.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE proof(mint_url TEXT,unit TEXT,state TEXT,amount INTEGER,secret TEXT)").unwrap();
    for (mint, unit, state, amount) in [
        (MINT, "sat", "UNSPENT", 64),
        (MINT, "sat", "RESERVED", 32),
        (MINT, "sat", "PENDING", 16),
        (MINT, "sat", "PENDING_SPENT", 8),
        (MINT, "sat", "SPENT", 128),
        ("http://other:3338", "sat", "UNSPENT", 1000),
        (MINT, "msat", "UNSPENT", 1000),
    ] {
        db.execute(
            "INSERT INTO proof VALUES(?1,?2,?3,?4,'private-proof-canary')",
            params![mint, unit, state, amount],
        )
        .unwrap();
    }
    let read = || wallet::cdk(&path, "wallet", "mint", MINT);
    let value = read().unwrap();
    assert_eq!(value["balance_sat"], 64);
    assert_eq!(value["reserved_sat"], 32);
    assert_eq!(value["pending_sat"], 16);
    assert_eq!(value["pending_spent_sat"], 8);
    assert!(!value.to_string().contains("private-proof-canary"));
    db.execute_batch("BEGIN; UPDATE proof SET amount=99 WHERE state='RESERVED'")
        .unwrap();
    assert_eq!(read().unwrap()["reserved_sat"], 32);
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(read().unwrap()["reserved_sat"], 99);
    for (state, amount) in [("UNKNOWN", "1"), ("UNSPENT", "-1"), ("UNSPENT", "1.5")] {
        db.execute(
            "INSERT INTO proof VALUES(?1,'sat',?2,?3,'private-proof-canary')",
            params![MINT, state, amount],
        )
        .unwrap();
        assert!(read().is_err());
        db.execute_batch("DELETE FROM proof WHERE rowid=(SELECT MAX(rowid) FROM proof)")
            .unwrap();
    }
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM proof", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 7);
    drop(db);
    let digest = Sha256::digest(fs::read(&path).unwrap());
    assert_eq!(read().unwrap()["balance_sat"], 64);
    assert_eq!(Sha256::digest(fs::read(&path).unwrap()), digest);
    let missing = directory.path().join("missing.sqlite");
    assert!(wallet::cdk(&missing, "wallet", "mint", MINT).is_err());
    assert!(!missing.exists());
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE proof").unwrap();
    assert!(read().is_err());
}

#[test]
fn coco_preserves_transactions_and_refuses_future_schema_or_noncanonical_amounts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("coco.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL;
        CREATE TABLE coco_cashu_proofs(mintUrl TEXT,unit TEXT,state TEXT,amount TEXT,usedByOperationId TEXT,secret TEXT);
        CREATE TABLE coco_cashu_mints(mintUrl TEXT); INSERT INTO coco_cashu_mints VALUES('http://mint:3338');
        CREATE TABLE coco_cashu_migrations(id TEXT); INSERT INTO coco_cashu_migrations VALUES('038_keypair_derivation_allocations')").unwrap();
    for (mint, unit, state, amount, owner) in [
        (MINT, "sat", "ready", "64", None),
        (MINT, "sat", "ready", "32", Some("operation")),
        (MINT, "sat", "inflight", "16", None),
        (MINT, "sat", "spent", "128", None),
        ("http://other:3338", "sat", "ready", "1000", None),
        (MINT, "msat", "ready", "1000", None),
    ] {
        db.execute(
            "INSERT INTO coco_cashu_proofs VALUES(?1,?2,?3,?4,?5,'private-proof-canary')",
            params![mint, unit, state, amount, owner],
        )
        .unwrap();
    }
    let read = || wallet::coco(&path, "wallet", "mint", MINT);
    let value = read().unwrap();
    assert_eq!(value["balance_sat"], 64);
    assert_eq!(value["reserved_sat"], 32);
    assert_eq!(value["inflight_sat"], 16);
    assert_eq!(value["total_ready_sat"], 96);
    assert!(value.get("pending_sat").is_none());
    assert!(!value.to_string().contains("private-proof-canary"));
    db.execute_batch(
        "BEGIN; UPDATE coco_cashu_proofs SET amount='33' WHERE usedByOperationId='operation'",
    )
    .unwrap();
    assert_eq!(read().unwrap()["reserved_sat"], 32);
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(read().unwrap()["reserved_sat"], 33);
    for (state, amount) in [
        ("unknown", "1"),
        ("ready", "-1"),
        ("ready", "1.5"),
        ("ready", "18446744073709551616"),
        ("ready", "01"),
        ("ready", "+1"),
    ] {
        db.execute(
            "INSERT INTO coco_cashu_proofs VALUES(?1,'sat',?2,?3,NULL,'private-proof-canary')",
            params![MINT, state, amount],
        )
        .unwrap();
        assert!(read().is_err());
        db.execute_batch(
            "DELETE FROM coco_cashu_proofs WHERE rowid=(SELECT MAX(rowid) FROM coco_cashu_proofs)",
        )
        .unwrap();
    }
    db.execute_batch("INSERT INTO coco_cashu_migrations VALUES('039_unknown')")
        .unwrap();
    assert!(read().is_err());
    db.execute_batch("DELETE FROM coco_cashu_migrations WHERE id='039_unknown'")
        .unwrap();
    drop(db);
    let digest = Sha256::digest(fs::read(&path).unwrap());
    assert_eq!(read().unwrap()["balance_sat"], 64);
    assert_eq!(Sha256::digest(fs::read(&path).unwrap()), digest);
    assert!(wallet::coco(&path, "wallet", "mint", "http://unknown:3338").is_err());
    let missing = directory.path().join("missing.db");
    assert!(wallet::coco(&missing, "wallet", "mint", MINT).is_err());
    assert!(!missing.exists());
}

#[test]
fn nutshell_holdings_count_sat_proofs_without_starting_wallets() {
    let directory = tempfile::tempdir().unwrap();
    let native = directory.path().join(".cashu/native-wallet");
    fs::create_dir_all(&native).unwrap();
    let db = Connection::open(native.join("native-wallet.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE keysets(id TEXT,mint_url TEXT,unit TEXT);
        CREATE TABLE proofs(id TEXT,amount INTEGER,reserved INTEGER,secret TEXT);
        INSERT INTO keysets VALUES('sat','http://mint:3338','sat'),('msat','http://mint:3338','msat');
        INSERT INTO proofs VALUES('sat',64,0,'private-proof-canary'),('sat',32,1,'private-proof-canary'),('msat',1000,0,'private-proof-canary')").unwrap();
    let read = || {
        wallet::holdings(
            "nutshell-wallet",
            directory.path(),
            "different-component-name",
        )
    };
    let value = read().unwrap();
    assert_eq!(value["mints"][0]["balance_sat"], 64);
    assert_eq!(value["mints"][0]["reserved_sat"], 32);
    assert!(!value.to_string().contains("private-proof-canary"));
    db.execute_batch("INSERT INTO proofs VALUES('missing',1,0,'private-proof-canary')")
        .unwrap();
    assert!(read().is_err());
    db.execute_batch("DELETE FROM proofs WHERE id='missing'; INSERT INTO keysets VALUES('sat','http://other:3338','sat')").unwrap();
    assert!(read().is_err());
}
