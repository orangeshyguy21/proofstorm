//! Project pinned passive wallet readers into amounts per mint, without exposing URLs.
use super::balances::{amount, integer};
use proofstorm_core::ComponentKind;
use proofstorm_kube::{ProofstormLab, instance_namespace};
use proofstorm_view::{BalanceAmount, HoldingsObservation, MintHolding};
use serde_json::Value;

const CDK_READER: &str = include_str!("../../../proofstorm-kube/drivers/cdk_wallet_balance.py");
const COCO_READER: &str = include_str!("../../../proofstorm-kube/drivers/cocod_wallet_balance.py");

pub(super) fn script(implementation: &str) -> String {
    if implementation == "nutshell-wallet" {
        return include_str!("nutshell_balance.py").into();
    }
    let (reader, database, query) = if implementation == "cdk-cli-wallet" {
        (
            CDK_READER,
            "/wallet/cdk/cdk-cli.sqlite",
            "SELECT DISTINCT mint_url FROM proof WHERE unit='sat'",
        )
    } else {
        (
            COCO_READER,
            "/wallet/.cocod/coco.db",
            "SELECT mintUrl FROM coco_cashu_mints",
        )
    };
    format!(
        "__name__='dashboard_reader'\n{reader}\nimport sys\ndatabase={database:?}\nwith sqlite3.connect(Path(database).as_uri()+'?mode=ro', uri=True, timeout=1) as db:\n urls=[r[0] for r in db.execute({query:?})]\nrows=[dict(observe(database,sys.argv[1],'',url),mint_url=url) for url in sorted(set(urls))]\nprint(json.dumps({{'mints':rows}}))"
    )
}
pub(super) fn project(
    lab: &ProofstormLab,
    implementation: &str,
    data: &Value,
) -> Option<(Vec<BalanceAmount>, HoldingsObservation)> {
    let keys: &[(&str, &str)] = match implementation {
        "cdk-cli-wallet" => &[
            ("balance_sat", "Spendable"),
            ("reserved_sat", "Reserved"),
            ("pending_sat", "Pending"),
            ("pending_spent_sat", "Pending spent"),
        ],
        "cocod-wallet" => &[
            ("balance_sat", "Spendable"),
            ("reserved_sat", "Reserved"),
            ("inflight_sat", "In flight"),
        ],
        _ => &[("balance_sat", "Spendable"), ("reserved_sat", "Reserved")],
    };
    let mut totals = keys
        .iter()
        .map(|(_, label)| amount(label, 0))
        .collect::<Vec<_>>();
    let mut mints = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for row in data["mints"].as_array()? {
        let url = row["mint_url"].as_str()?;
        if !seen.insert(url) {
            return None;
        }
        let amounts = keys
            .iter()
            .zip(&mut totals)
            .map(|((key, label), total)| {
                let value = integer(&row[key])?;
                total.sat = total.sat.checked_add(value)?;
                Some(amount(label, value))
            })
            .collect::<Option<Vec<_>>>()?;
        mints.push(MintHolding {
            id: proofstorm_core::digest_json(&url),
            mint: match_mint(lab, url),
            amounts,
        });
    }
    totals
        .iter()
        .try_fold(0_u64, |sum, a| sum.checked_add(a.sat))?;
    mints.sort_by(|a, b| (&a.mint, &a.id).cmp(&(&b.mint, &b.id)));
    Some((
        totals,
        HoldingsObservation {
            observed_at_unix: super::now(),
            error: None,
            mints,
        },
    ))
}
fn match_mint(lab: &ProofstormLab, url: &str) -> Option<String> {
    // Match only exact in-lab service aliases. Never infer an external URL by its first label.
    let namespace = instance_namespace(&lab.spec.instance_key);
    lab.spec
        .lab
        .components
        .iter()
        .filter(|c| c.kind == ComponentKind::Mint)
        .find(|c| {
            [
                c.id.clone(),
                format!("{}.{}", c.id, namespace),
                format!("{}.{}.svc", c.id, namespace),
                format!("{}.{}.svc.cluster.local", c.id, namespace),
            ]
            .iter()
            .any(|host| url.trim_end_matches('/') == format!("http://{host}:3338"))
        })
        .map(|c| c.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    fn lab() -> ProofstormLab {
        ProofstormLab::new(
            "lab",
            proofstorm_kube::ProofstormLabSpec {
                workspace_id: "workspace".into(),
                instance_id: "lab".into(),
                instance_key: "instance".into(),
                revision_digest: "revision".into(),
                lock: proofstorm_core::ResolvedLock {
                    api_version: "lock".into(),
                    digest: "lock".into(),
                    entries: vec![],
                },
                lab: serde_json::from_str(include_str!("../../../../examples/developer-lab.json"))
                    .unwrap(),
            },
        )
    }
    #[test]
    fn mint_matching_is_exact_and_unknown_urls_stay_private() {
        let lab = lab();
        let ns = instance_namespace(&lab.spec.instance_key);
        assert_eq!(
            match_mint(&lab, &format!("http://mint.{ns}.svc.cluster.local:3338/")),
            Some("mint".into())
        );
        for url in [
            "http://mint.evil:3338",
            "http://user:secret@mint:3338",
            "http://mint:3338/path",
        ] {
            assert_eq!(match_mint(&lab, url), None);
        }
        let row = |url| json!({"mint_url":url,"balance_sat":10,"reserved_sat":2,"pending_sat":3,"pending_spent_sat":4});
        let (totals, observation) = project(
            &lab,
            "cdk-cli-wallet",
            &json!({"mints":[row("http://mint:3338"),row("http://secret:token@external:3338")]}),
        )
        .unwrap();
        assert_eq!(totals[0].sat, 20);
        assert_eq!(
            observation
                .mints
                .iter()
                .map(MintHolding::held_sat)
                .sum::<u64>(),
            24
        );
        assert!(
            !serde_json::to_string(&observation)
                .unwrap()
                .contains("secret")
        );
        assert!(
            project(
                &lab,
                "cdk-cli-wallet",
                &json!({"mints":[row("same"),row("same")]})
            )
            .is_none()
        );
        assert!(
            project(
                &lab,
                "cdk-cli-wallet",
                &json!({"mints":[{"mint_url":"mint"}]})
            )
            .is_none()
        );
    }
    fn run(script: &str) -> Value {
        let output = std::process::Command::new("python3")
            .args(["-c", script, "wallet"])
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
    fn pinned_cdk_reader_keeps_mints_and_proof_states_separate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wallet.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE proof(mint_url TEXT, unit TEXT, state TEXT, amount INTEGER);
            INSERT INTO proof VALUES ('http://mint:3338','sat','UNSPENT',30),('http://mint:3338','sat','RESERVED',5),('http://mint:3338','sat','PENDING',7),('http://mint:3338','sat','PENDING_SPENT',11),('http://mint:3338','sat','SPENT',1000),('http://other:3338','sat','UNSPENT',9),('http://other:3338','usd','UNSPENT',200);").unwrap();
        let data =
            run(&script("cdk-cli-wallet")
                .replace("/wallet/cdk/cdk-cli.sqlite", path.to_str().unwrap()));
        let (totals, observation) = project(&lab(), "cdk-cli-wallet", &data).unwrap();
        assert_eq!(
            totals.iter().map(|a| a.sat).collect::<Vec<_>>(),
            vec![39, 5, 7, 11]
        );
        assert_eq!(observation.mints.len(), 2);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM proof", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            7
        );
    }
    #[test]
    fn coco_reader_keeps_reserved_and_inflight_out_of_spendable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("coco.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE coco_cashu_migrations(id TEXT); INSERT INTO coco_cashu_migrations VALUES ('038_keypair_derivation_allocations');
            CREATE TABLE coco_cashu_mints(mintUrl TEXT); INSERT INTO coco_cashu_mints VALUES ('http://mint:3338'),('http://other:3338');
            CREATE TABLE coco_cashu_proofs(mintUrl TEXT,unit TEXT,state TEXT,amount TEXT,usedByOperationId TEXT);
            INSERT INTO coco_cashu_proofs VALUES ('http://mint:3338','sat','ready','10',NULL),('http://mint:3338','sat','ready','4','op'),('http://mint:3338','sat','inflight','6',NULL),('http://mint:3338','sat','spent','200',NULL),('http://other:3338','sat','ready','9',NULL);").unwrap();
        let data =
            run(&script("cocod-wallet").replace("/wallet/.cocod/coco.db", path.to_str().unwrap()));
        let (totals, observation) = project(&lab(), "cocod-wallet", &data).unwrap();
        assert_eq!(
            totals.iter().map(|a| a.sat).collect::<Vec<_>>(),
            vec![19, 4, 6]
        );
        assert_eq!(observation.mints.len(), 2);
    }
    #[test]
    fn nutshell_groups_keysets_once_across_named_wallets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("alice");
        std::fs::create_dir(&root).unwrap();
        let db = Connection::open(root.join("alice.sqlite3")).unwrap();
        db.execute_batch("CREATE TABLE keysets(id TEXT,mint_url TEXT,unit TEXT); CREATE TABLE proofs(id TEXT,amount INTEGER,reserved INTEGER);
            INSERT INTO keysets VALUES ('a','http://mint:3338','sat'),('a','http://mint:3338','sat'),('b','http://other:3338','sat');
            INSERT INTO proofs VALUES ('a',12,0),('a',3,1),('b',20,0);").unwrap();
        let data =
            run(&script("nutshell-wallet").replace("/wallet/.cashu", dir.path().to_str().unwrap()));
        let (totals, observation) = project(&lab(), "nutshell-wallet", &data).unwrap();
        assert_eq!(
            totals.iter().map(|a| a.sat).collect::<Vec<_>>(),
            vec![32, 3]
        );
        assert_eq!(observation.mints.len(), 2);
    }
}
