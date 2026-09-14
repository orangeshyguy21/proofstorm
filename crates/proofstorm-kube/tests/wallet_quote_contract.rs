//! The native driver's allowlisted artifacts must remain valid core observations.
use proofstorm_core::wallet_quote_observations_from_artifact;
use proofstorm_driver::quote::{Config, observe};
use rusqlite::Connection;
use serde_json::json;

#[test]
fn native_wallet_quotes_satisfy_the_public_contract_without_private_material() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join(".cashu/wallet");
    std::fs::create_dir_all(&directory).unwrap();
    let db = Connection::open(directory.join("wallet.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE bolt11_mint_quotes (quote TEXT,mint TEXT,state TEXT,amount INTEGER,
        created_time INTEGER,paid_time INTEGER,expiry INTEGER,request TEXT);
        INSERT INTO bolt11_mint_quotes VALUES ('quote-1','http://mint:3338','ISSUED',100,1,2,301,'lnbcrt-private');
        CREATE TABLE bolt11_melt_quotes (quote TEXT,state TEXT,amount INTEGER,fee_reserve INTEGER,
        fee_paid INTEGER,request TEXT,created_time INTEGER);
        INSERT INTO bolt11_melt_quotes VALUES ('melt-1','PAID',100,2,1,'lnbcrt-private',3);").unwrap();
    let config = Config {
        variables: [
            ("HOME".into(), root.path().to_str().unwrap().into()),
            ("PROOFSTORM_WALLET".into(), "recipient".into()),
            ("PROOFSTORM_MINT".into(), "mint".into()),
            (
                "PROOFSTORM_EXPECTED_MINT_URL".into(),
                "http://mint:3338".into(),
            ),
            ("PROOFSTORM_MINT_QUOTE_ID".into(), "quote-1".into()),
            (
                "PROOFSTORM_OBSERVATION_ROLE".into(),
                "payment_receive".into(),
            ),
            ("PROOFSTORM_INVOICE".into(), "lnbcrt-private".into()),
        ]
        .into(),
    };
    let artifact = json!({"quote_observations":[
        observe("observe-melt",&config).unwrap(),
        observe("observe-receive",&config).unwrap(),
    ]});
    assert_eq!(
        wallet_quote_observations_from_artifact(&artifact)
            .unwrap()
            .len(),
        2
    );
    assert!(!artifact.to_string().contains("lnbcrt"));
}
