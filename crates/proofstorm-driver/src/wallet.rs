//! Passive wallet observations. Read transactions never start a wallet or recover operations.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn database(path: &Path) -> Result<Connection> {
    // Do not use immutable mode with a live WAL. SQLite may coordinate through
    // its WAL/SHM files, but the connection cannot modify wallet records.
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(3))?;
    db.execute_batch("PRAGMA query_only=ON; BEGIN")?;
    Ok(db)
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn columns(db: &Connection, table: &str, required: &[&str]) -> Result<()> {
    let mut statement = db.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let observed = statement
        .query_map([table], |row| row.get::<_, String>(0))?
        .collect::<Result<BTreeSet<_>, _>>()?;
    ensure!(
        required.iter().all(|name| observed.contains(*name)),
        "wallet_schema_mismatch"
    );
    Ok(())
}

fn add(
    amounts: &mut BTreeMap<&'static str, u64>,
    category: &'static str,
    value: u64,
) -> Result<()> {
    let amount = amounts
        .get_mut(category)
        .context("wallet_proof_state_invalid")?;
    *amount = amount
        .checked_add(value)
        .context("wallet_balance_overflow")?;
    Ok(())
}

fn validate_total(amounts: &BTreeMap<&str, u64>) -> Result<()> {
    amounts
        .values()
        .try_fold(0_u64, |total, value| total.checked_add(*value))
        .context("wallet_balance_overflow")?;
    Ok(())
}

/// Observe the pinned CDK CLI `SQLite` layout without loading its SDK.
/// # Errors
/// Rejects missing/busy databases, incompatible schemas, unknown states and invalid amounts.
pub fn cdk(path: &Path, wallet: &str, mint: &str, mint_url: &str) -> Result<Value> {
    let db = database(path)?;
    columns(&db, "proof", &["mint_url", "unit", "state", "amount"])?;
    let mut amounts = BTreeMap::from([
        ("UNSPENT", 0),
        ("RESERVED", 0),
        ("PENDING", 0),
        ("PENDING_SPENT", 0),
        ("SPENT", 0),
    ]);
    let mut statement =
        db.prepare("SELECT state, amount FROM proof WHERE mint_url=?1 AND unit='sat'")?;
    let mut rows = statement.query([mint_url])?;
    while let Some(row) = rows.next()? {
        let state: String = row.get(0)?;
        let category = match state.as_str() {
            "UNSPENT" => "UNSPENT",
            "RESERVED" => "RESERVED",
            "PENDING" => "PENDING",
            "PENDING_SPENT" => "PENDING_SPENT",
            "SPENT" => "SPENT",
            _ => bail!("wallet_proof_state_invalid"),
        };
        let amount = u64::try_from(row.get::<_, i64>(1)?).context("wallet_amount_invalid")?;
        add(&mut amounts, category, amount)?;
    }
    validate_total(&amounts)?;
    Ok(json!({"wallet":wallet,"mint":mint,"unit":"sat",
        "balance_sat":amounts["UNSPENT"],"reserved_sat":amounts["RESERVED"],
        "pending_sat":amounts["PENDING"],"pending_spent_sat":amounts["PENDING_SPENT"],
        "observation_source":"cdk-cli/0.18/sqlite-read-transaction/v1","observed_at_unix":now()?}))
}

/// Observe the pinned Coco `SQLite` layout without loading or starting its daemon.
/// # Errors
/// Rejects unsupported migrations, unknown mints/states and noncanonical or overflowing amounts.
pub fn coco(path: &Path, wallet: &str, mint: &str, mint_url: &str) -> Result<Value> {
    let db = database(path)?;
    columns(
        &db,
        "coco_cashu_proofs",
        &["mintUrl", "unit", "state", "amount", "usedByOperationId"],
    )?;
    let mut migrations = db.prepare("SELECT id FROM coco_cashu_migrations")?;
    let migrations = migrations
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<BTreeSet<_>, _>>()?;
    let latest = "038_keypair_derivation_allocations";
    ensure!(
        migrations.contains(latest) && migrations.iter().all(|id| id.as_str() <= latest),
        "wallet_schema_mismatch"
    );
    ensure!(
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM coco_cashu_mints WHERE mintUrl=?1)",
            [mint_url],
            |row| row.get::<_, bool>(0)
        )?,
        "mint_not_registered"
    );
    let mut amounts = BTreeMap::from([
        ("spendable", 0),
        ("reserved", 0),
        ("inflight", 0),
        ("spent", 0),
    ]);
    let mut statement = db.prepare("SELECT state, amount, usedByOperationId FROM coco_cashu_proofs WHERE mintUrl=?1 AND unit='sat'")?;
    let mut rows = statement.query([mint_url])?;
    while let Some(row) = rows.next()? {
        let state: String = row.get(0)?;
        let raw: String = row.get(1)?;
        let amount: u64 = raw.parse().context("wallet_amount_invalid")?;
        ensure!(amount.to_string() == raw, "wallet_amount_invalid");
        let owner: Option<String> = row.get(2)?;
        let category = match state.as_str() {
            "ready" if owner.is_some_and(|owner| !owner.is_empty()) => "reserved",
            "ready" => "spendable",
            "inflight" => "inflight",
            "spent" => "spent",
            _ => bail!("wallet_proof_state_invalid"),
        };
        add(&mut amounts, category, amount)?;
    }
    validate_total(&amounts)?;
    Ok(json!({"wallet":wallet,"mint":mint,"unit":"sat",
        "balance_sat":amounts["spendable"],"reserved_sat":amounts["reserved"],
        "inflight_sat":amounts["inflight"],"total_ready_sat":amounts["spendable"]+amounts["reserved"],
        "observation_source":"cocod/44e5101c/sqlite-read-transaction/v1","observed_at_unix":now()?}))
}

/// Observe every sat mint held by a native wallet, preserving unknown/error semantics.
/// # Errors
/// Returns an error for missing state, incompatible schemas and invalid wallet balances.
pub fn holdings(implementation: &str, root: &Path, wallet: &str) -> Result<Value> {
    if implementation == "nutshell-wallet" {
        return nutshell_holdings(root);
    }
    let (path, query) = match implementation {
        "cdk-cli-wallet" => (
            root.join("cdk/cdk-cli.sqlite"),
            "SELECT DISTINCT mint_url FROM proof WHERE unit='sat'",
        ),
        "cocod-wallet" => (
            root.join(".cocod/coco.db"),
            "SELECT mintUrl FROM coco_cashu_mints",
        ),
        _ => bail!("unsupported_wallet"),
    };
    let db = database(&path)?;
    let mut statement = db.prepare(query)?;
    let urls = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut rows = Vec::new();
    for url in urls {
        let mut row = if implementation == "cdk-cli-wallet" {
            cdk(&path, wallet, "", &url)?
        } else {
            coco(&path, wallet, "", &url)?
        };
        row["mint_url"] = json!(url);
        rows.push(row);
    }
    Ok(json!({"mints":rows}))
}

fn nutshell_holdings(root: &Path) -> Result<Value> {
    let mut found = false;
    let mut holdings: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for entry in std::fs::read_dir(root.join(".cashu"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry
            .path()
            .join(format!("{}.sqlite3", entry.file_name().to_string_lossy()));
        if !path.is_file() {
            continue;
        }
        found = true;
        let db = database(&path)?;
        let mut statement = db.prepare("SELECT id, mint_url FROM keysets WHERE unit='sat'")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut keysets = BTreeMap::new();
        for row in rows {
            let (id, url) = row?;
            ensure!(!url.is_empty(), "mint unavailable");
            if let Some(previous) = keysets.insert(id, url.clone()) {
                ensure!(previous == url, "ambiguous keyset");
            }
        }
        let mut proofs = db.prepare("SELECT id, amount, COALESCE(reserved,0) FROM proofs")?;
        let mut rows = proofs.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let Some(url) = keysets.get(&id) else {
                ensure!(
                    db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM keysets WHERE id=?1)",
                        params![id],
                        |row| row.get::<_, bool>(0)
                    )?,
                    "unknown keyset"
                );
                continue;
            };
            let amount = u64::try_from(row.get::<_, i64>(1)?).context("invalid amount")?;
            let held = row.get::<_, i64>(2)? != 0;
            let totals = holdings.entry(url.clone()).or_default();
            let selected = if held { &mut totals.1 } else { &mut totals.0 };
            *selected = selected.checked_add(amount).context("balance overflow")?;
            totals.0.checked_add(totals.1).context("balance overflow")?;
        }
    }
    ensure!(found, "wallet not initialized");
    Ok(
        json!({"mints":holdings.into_iter().map(|(url,(available,reserved))|
        json!({"mint_url":url,"balance_sat":available,"reserved_sat":reserved})).collect::<Vec<_>>()}),
    )
}
