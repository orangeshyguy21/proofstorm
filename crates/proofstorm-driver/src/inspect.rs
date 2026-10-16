//! Bounded passive inspection of native wallet evidence.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, params, types::ValueRef};
use serde_json::{Value, json};
use std::{
    fmt::Write as _,
    fs,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

/// Most rows one `sqlite` query returns; `truncated` reports any more.
const SQLITE_MAX_ROWS: usize = 1000;

/// Run one SQL statement over a component's database file without changing it.
/// The connection is read-only with `query_only`, so data and schema stay as
/// they are; a running writer's WAL is read normally. Blobs are returned as hex.
/// # Errors
/// Rejects missing databases, writes, multiple statements and database errors.
pub fn sqlite_rows(path: &Path, sql: &str) -> Result<Value> {
    ensure!(path.is_file(), "database not found: {}", path.display());
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA query_only=ON")?;
    let mut statement = db.prepare(sql)?;
    let columns = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut rows = statement.query([])?;
    let (mut out, mut truncated) = (Vec::new(), false);
    while let Some(row) = rows.next()? {
        if out.len() == SQLITE_MAX_ROWS {
            truncated = true;
            break;
        }
        let values = (0..columns.len())
            .map(|index| {
                Ok(match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Real(value) => json!(value),
                    ValueRef::Text(value) => json!(String::from_utf8_lossy(value)),
                    ValueRef::Blob(value) => json!({
                        "hex": value.iter().fold(String::new(), |mut hex, byte| {
                            let _ = write!(hex, "{byte:02x}");
                            hex
                        })
                    }),
                })
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        out.push(Value::Array(values));
    }
    Ok(json!({"columns": columns, "rows": out, "truncated": truncated}))
}

/// Select one exact CDK receive quote without starting a wallet or writing its DB.
/// `invoice` intentionally returns private material to a private transport caller.
/// # Errors
/// Rejects missing/ambiguous quotes, unknown fields and unavailable database state.
pub fn cdk_quote(path: &Path, mint: &str, amount: u64, state: &str, field: &str) -> Result<String> {
    ensure!(
        matches!(field, "await" | "id" | "invoice") && matches!(state, "UNPAID" | "any"),
        "unsupported quote observation"
    );
    let amount = i64::try_from(amount)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let observed = (|| -> rusqlite::Result<Vec<(String, String)>> {
            let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            db.busy_timeout(Duration::from_millis(100))?;
            db.execute_batch("PRAGMA query_only=ON; BEGIN")?;
            let mut query=db.prepare("SELECT id,request FROM mint_quote WHERE amount=?1 AND mint_url=?2 AND (?3='any' OR state=?3) LIMIT 2")?;
            query
                .query_map(params![amount, mint, state], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?
                .collect()
        })();
        if let Ok(rows) = &observed {
            ensure!(rows.len() <= 1, "ambiguous native quote");
            if let Some((id, request)) = rows.first() {
                ensure!(
                    !id.is_empty() && !request.is_empty(),
                    "incomplete native quote"
                );
                return Ok(match field {
                    "await" => "real unpaid quote observed".into(),
                    "id" => id.clone(),
                    _ => request.clone(),
                });
            }
        }
        ensure!(Instant::now() < deadline, "native quote not observed");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Project only the structured line the native CDK melt command actually emitted.
/// # Errors
/// Rejects oversized logs, absent/duplicate receipts and invalid amounts or states.
pub fn cdk_melt_receipt(path: &Path) -> Result<Value> {
    let mut log = String::new();
    fs::File::open(path)?
        .take(1_048_577)
        .read_to_string(&mut log)?;
    ensure!(log.len() <= 1_048_576, "native log exceeds bound");
    parse_receipt(&log)
}
fn parse_receipt(log: &str) -> Result<Value> {
    let mut receipt = None;
    for line in log.lines() {
        let Some(line) = line.strip_prefix("Payment successful: state=") else {
            continue;
        };
        ensure!(receipt.is_none(), "duplicate native melt receipt");
        let (state, amount) = line
            .split_once(", amount=")
            .context("invalid native receipt")?;
        let (amount, fee) = amount
            .split_once(", fee_paid=")
            .context("invalid native receipt")?;
        ensure!(
            matches!(state, "UNPAID" | "PENDING" | "PAID"),
            "invalid native receipt state"
        );
        ensure!(
            [amount, fee]
                .iter()
                .all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())),
            "invalid native receipt amount"
        );
        receipt = Some(
            json!({"state":state,"amount_sat":amount.parse::<u64>()?,"fee_paid_sat":fee.parse::<u64>()?}),
        );
    }
    receipt.context("native melt receipt missing")
}

/// Confirm a process executable is absent in this PID namespace.
/// # Errors
/// Rejects invalid names, unreadable proc state and any still-live matching process.
pub fn process_absent(root: &Path, name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains('/'),
        "invalid executable name"
    );
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        match fs::read_link(entry.path().join("exe")) {
            Ok(path) if path.file_name().is_some_and(|value| value == name) => {
                bail!("native wallet process still running")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_native_receipt_ignores_private_logs_and_rejects_ambiguous_or_malformed_evidence() {
        let receipt = "Payment successful: state=PAID, amount=300, fee_paid=2";
        assert_eq!(
            parse_receipt(&format!("lnbcrt-private\n{receipt}\nsecret-proof\n")).unwrap(),
            json!({"state":"PAID","amount_sat":300,"fee_paid_sat":2})
        );
        for input in [
            format!("{receipt}\n{receipt}"),
            receipt.replace("300", "-1"),
            receipt.replace("300", "18446744073709551616"),
            receipt.replace("PAID", "private-value"),
            "no receipt".into(),
        ] {
            assert!(parse_receipt(&input).is_err());
        }
    }
    #[test]
    fn sqlite_rows_read_a_live_wal_database_without_writing_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mint.sqlite");
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE payment(id TEXT, amount INTEGER, fee REAL, outpoint BLOB, note TEXT);
            INSERT INTO payment VALUES ('quote-1', 5000, 0.5, x'00ff', NULL);").unwrap();
        // An open write transaction must not hide committed rows or block the read.
        writer
            .execute_batch(
                "BEGIN IMMEDIATE; INSERT INTO payment VALUES ('pending', 1, 0, NULL, NULL);",
            )
            .unwrap();
        assert_eq!(
            sqlite_rows(&path, "SELECT * FROM payment").unwrap(),
            json!({"columns":["id","amount","fee","outpoint","note"],
                "rows":[["quote-1",5000,0.5,{"hex":"00ff"},null]],"truncated":false})
        );
        writer.execute_batch("COMMIT").unwrap();
        for refused in [
            "DELETE FROM payment",
            "UPDATE payment SET amount = 0",
            "CREATE TABLE extra(x)",
            "SELECT 1; DELETE FROM payment",
        ] {
            assert!(sqlite_rows(&path, refused).is_err(), "{refused}");
        }
        assert_eq!(
            sqlite_rows(&path, "SELECT count(*) FROM payment").unwrap()["rows"],
            json!([[2]])
        );
        assert!(sqlite_rows(&root.path().join("absent.sqlite"), "SELECT 1").is_err());
    }

    #[test]
    fn sqlite_rows_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bounded.sqlite");
        Connection::open(&path).unwrap().execute_batch(
            "CREATE TABLE n(v INTEGER); WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 1001) INSERT INTO n SELECT x FROM c;",
        ).unwrap();
        let result = sqlite_rows(&path, "SELECT v FROM n").unwrap();
        assert_eq!(result["rows"].as_array().unwrap().len(), SQLITE_MAX_ROWS);
        assert_eq!(result["truncated"], true);
    }

    #[test]
    fn cdk_quote_matching_is_exact_and_read_only() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("wallet.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE mint_quote(id TEXT,request TEXT,mint_url TEXT,amount INTEGER,state TEXT);
            INSERT INTO mint_quote VALUES ('quote-1','lnbcrt-private','http://mint:3338',5000,'UNPAID'),('foreign','other-private','http://different:3338',5000,'UNPAID')").unwrap();
        for (field, expected) in [
            ("await", "real unpaid quote observed"),
            ("id", "quote-1"),
            ("invoice", "lnbcrt-private"),
        ] {
            assert_eq!(
                cdk_quote(&path, "http://mint:3338", 5000, "UNPAID", field).unwrap(),
                expected
            );
        }
        db.execute(
            "INSERT INTO mint_quote SELECT * FROM mint_quote WHERE id='quote-1'",
            [],
        )
        .unwrap();
        assert!(cdk_quote(&path, "http://mint:3338", 5000, "any", "id").is_err());
    }
}
