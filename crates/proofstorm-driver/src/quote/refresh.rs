//! Nutshell 0.20.3's explicit quote recovery, with atomic local updates.
use super::{Config, Melt, Result, Row, amount, connection, fail, normalized_mint, rows, text};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Reservation {
    count: u64,
    reserved: u64,
    available: u64,
    proofs: Vec<Row>,
}
impl Reservation {
    fn read(db: &Connection, id: &str) -> Result<Self> {
        let proofs = rows(
            db,
            "SELECT secret,amount,COALESCE(reserved,0),id,C,derivation_path,mint_id,p2pk_e,time_reserved FROM proofs WHERE melt_id=?1 ORDER BY secret",
            &[&id],
        )?;
        let mut count = 0_u64;
        let mut reserved = 0_u64;
        for proof in &proofs {
            if amount(&proof[2])? > 0 {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| fail("wallet_balance_overflow"))?;
                reserved = reserved
                    .checked_add(amount(&proof[1])?)
                    .ok_or_else(|| fail("wallet_balance_overflow"))?;
            }
        }
        let available = rows(
            db,
            "SELECT COALESCE(SUM(amount),0) FROM proofs WHERE NOT COALESCE(reserved,0)",
            &[],
        )?;
        Ok(Self {
            count,
            reserved,
            available: amount(&available[0][0])?,
            proofs,
        })
    }
}

pub(super) async fn run(config: &Config) -> Result<Value> {
    let id = config.id("PROOFSTORM_MELT_QUOTE_ID")?;
    let expected = config.get("PROOFSTORM_EXPECTED_MINT_URL")?;
    let wallet = config.wallet(None, None)?;
    let initial = Melt::by_id(&wallet, id)?;
    let db = connection(&initial.path, Duration::from_secs(1))?;
    let local = rows(
        &db,
        "SELECT mint,request,unit FROM bolt11_melt_quotes WHERE quote=?1",
        &[&id],
    )?;
    if local.len() != 1
        || normalized_mint(text(&local[0][0])?) != normalized_mint(expected)
        || local[0][2] != "sat"
    {
        return Err(fail("melt_quote_mint_mismatch"));
    }
    let invoice = text(&local[0][1])?.to_owned();
    let before = Reservation::read(&db, id)?;
    drop(db);
    let client = crate::http::client(Duration::from_secs(30))
        .map_err(|_| fail("melt_quote_refresh_failed"))?;
    let (status, remote) = crate::http::json(client.get(format!(
        "{}/v1/melt/quote/bolt11/{id}",
        expected.trim_end_matches('/')
    )))
    .await
    .map_err(|_| fail("melt_quote_refresh_failed"))?;
    if !status.is_success() || remote["quote"] != id {
        return Err(fail("melt_quote_refresh_missing"));
    }
    let state = text(&remote["state"])?.to_ascii_uppercase();
    if !matches!(state.as_str(), "UNPAID" | "PENDING" | "PAID") {
        return Err(fail("unsupported_wallet_quote_state"));
    }
    if amount(&remote["amount"])? != initial.amount
        || amount(&remote["fee_reserve"])? != initial.reserve
        || remote
            .get("unit")
            .is_some_and(|unit| !unit.is_null() && unit != "sat")
        || remote.get("request").is_some_and(|request| {
            !request.is_null()
                && request
                    .as_str()
                    .is_none_or(|value| !value.eq_ignore_ascii_case(&invoice))
        })
    {
        return Err(fail("melt_quote_identity_mismatch"));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| fail("quote_driver_failed"))?
        .as_secs();
    let now = i64::try_from(now).map_err(|_| fail("quote_driver_failed"))?;
    let after = apply(&initial, &before, &local[0], &state, &remote, now)?;
    let mut observed = initial.clone();
    observed.state.clone_from(&state);
    observed.fee = None;
    Ok(
        json!({"melt_quote_id":id,"state_before":initial.state.to_ascii_uppercase(),"state_after":state,
        "reserved_proof_count_before":before.count,"reserved_proof_count_after":after.count,
        "reserved_sat_before":before.reserved,"reserved_sat_after":after.reserved,
        "available_balance_sat_before":before.available,"available_balance_sat_after":after.available,
        "proofs_released":before.count>0 && after.count==0,
        "quote_observations":[observed.artifact(config.get("PROOFSTORM_WALLET")?,config.get("PROOFSTORM_MINT")?)]}),
    )
}

fn apply(
    initial: &Melt,
    before: &Reservation,
    identity: &Row,
    state: &str,
    remote: &Value,
    now: i64,
) -> Result<Reservation> {
    let mut db = Connection::open_with_flags(&initial.path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    db.busy_timeout(Duration::from_secs(1))?;
    let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = rows(
        &transaction,
        "SELECT quote,state,amount,fee_reserve,fee_paid,mint,request,unit FROM bolt11_melt_quotes WHERE quote=?1",
        &[&initial.id],
    )?;
    if current.len() != 1
        || Melt::parse(initial.path.clone(), &current[0])? != *initial
        || current[0][5..] != identity[..]
        || Reservation::read(&transaction, &initial.id)?.proofs != before.proofs
    {
        return Err(fail("melt_quote_changed_during_refresh"));
    }
    if initial.state.eq_ignore_ascii_case("PAID") && state != "PAID" {
        return Err(fail("melt_quote_state_conflict"));
    }
    match state {
        "UNPAID" => {
            // Upstream clears the reservation and melt binding, and updates the
            // reservation timestamp; it leaves the local quote's state untouched.
            transaction.execute(
                "UPDATE proofs SET reserved=0,melt_id=NULL,time_reserved=?1 WHERE melt_id=?2",
                params![now, initial.id],
            )?;
        }
        "PAID" if !initial.state.eq_ignore_ascii_case("PAID") => {
            let preimage = remote
                .get("payment_preimage")
                .filter(|v| !v.is_null())
                .map(text)
                .transpose()?
                .unwrap_or("");
            // Preserve upstream's local compatibility field, but never report it
            // as independent evidence of the actual Lightning fee.
            let fee = remote
                .get("fee_paid")
                .filter(|v| !v.is_null())
                .map(amount)
                .transpose()?
                .unwrap_or(0);
            let fee = i64::try_from(fee).map_err(|_| fail("wallet_amount_invalid"))?;
            transaction.execute("UPDATE bolt11_melt_quotes SET state='PAID',paid_time=?1,fee_paid=?2,payment_preimage=?3 WHERE quote=?4",params![now,fee,preimage,initial.id])?;
            let total = before.proofs.iter().try_fold(0_u64, |total, row| {
                total
                    .checked_add(amount(&row[1])?)
                    .ok_or_else(|| fail("wallet_balance_overflow"))
            })?;
            if Some(total) == initial.amount.checked_add(initial.reserve) {
                // Copy and retire proofs in one transaction. A failed schema or
                // uniqueness check rolls back both the quote and its proofs.
                transaction.execute("INSERT INTO proofs_used(amount,C,secret,time_used,id,derivation_path,mint_id,melt_id,p2pk_e) SELECT amount,C,secret,?1,id,derivation_path,mint_id,melt_id,p2pk_e FROM proofs WHERE melt_id=?2",params![now,initial.id])?;
                transaction.execute("DELETE FROM proofs WHERE melt_id=?1", [&initial.id])?;
            }
        }
        _ => {}
    }
    let after = Reservation::read(&transaction, &initial.id)?;
    transaction.commit()?;
    Ok(after)
}
