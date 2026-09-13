//! Private Nutshell quote correlation and explicit settlement/recovery.
//! Observation paths never start a wallet or mutate its database.
mod native;
mod refresh;
#[cfg(test)]
mod tests;
use native::{Native, WalletCli};
use rusqlite::{Connection, ErrorCode, OpenFlags, ToSql, types::ValueRef};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Failure(pub String);
impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for Failure {}
type Result<T> = std::result::Result<T, Failure>;
fn fail(reason: &str) -> Failure {
    Failure(reason.into())
}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        fail("quote_driver_failed")
    }
}
impl From<serde_json::Error> for Failure {
    fn from(_: serde_json::Error) -> Self {
        fail("quote_driver_failed")
    }
}
impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        fail(db_error(&error))
    }
}

/// Inputs are captured once, allowing deterministic tests without changing process env.
pub struct Config {
    pub variables: BTreeMap<String, String>,
}
impl Config {
    #[must_use]
    pub fn environment() -> Self {
        Self {
            variables: std::env::vars().collect(),
        }
    }
    fn get(&self, name: &str) -> Result<&str> {
        self.variables
            .get(name)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Failure(format!("{}_missing", name.to_ascii_lowercase())))
    }
    fn optional(&self, name: &str) -> Option<&str> {
        self.variables.get(name).map(String::as_str)
    }
    fn seconds(&self, name: &str, default: f64, min: f64, max: f64) -> Result<Duration> {
        let value = self
            .optional(name)
            .map_or(Ok(default), str::parse::<f64>)
            .map_err(|_| Failure(format!("{}_invalid", name.to_ascii_lowercase())))?;
        if !value.is_finite() || !(min..=max).contains(&value) {
            return Err(Failure(format!("{}_invalid", name.to_ascii_lowercase())));
        }
        Ok(Duration::from_secs_f64(value))
    }
    fn id(&self, name: &str) -> Result<&str> {
        let value = self.get(name)?;
        if !valid_id(value) {
            return Err(Failure(format!("{}_invalid", name.to_ascii_lowercase())));
        }
        Ok(value)
    }
    fn wallet(&self, home: Option<&str>, name: Option<&str>) -> Result<Wallet> {
        let home = home.map_or_else(|| self.get("HOME"), Ok)?;
        let name = name.map_or_else(|| self.get("PROOFSTORM_WALLET"), Ok)?;
        // Wallet identities are single path components, never arbitrary filesystem paths.
        if !valid_id(name) {
            return Err(fail("wallet_name_invalid"));
        }
        let directory = Path::new(home).join(".cashu").join(name);
        let mut paths = Vec::new();
        for entry in fs::read_dir(&directory).map_err(|_| fail("wallet_database_missing"))? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().is_some_and(|ext| ext == "sqlite3")
            {
                paths.push(entry.path());
            }
        }
        paths.sort();
        if paths.is_empty() {
            return Err(fail("wallet_database_missing"));
        }
        Ok(Wallet {
            paths,
            timeout: self.seconds("PROOFSTORM_DB_TIMEOUT_SECONDS", 10.0, 1.0, 30.0)?,
            retry: self.seconds("PROOFSTORM_DB_RETRY_SECONDS", 0.2, 0.05, 2.0)?,
        })
    }
}
fn valid_id(value: &str) -> bool {
    let valid_end = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    (1..=63).contains(&value.len())
        && value.as_bytes().first().is_some_and(|b| valid_end(*b))
        && value.as_bytes().last().is_some_and(|b| valid_end(*b))
        && value.bytes().all(|b| valid_end(b) || b == b'-')
}
fn normalized_mint(value: &str) -> String {
    value.trim_end_matches('/').to_ascii_lowercase()
}

struct Wallet {
    paths: Vec<PathBuf>,
    timeout: Duration,
    retry: Duration,
}
type Row = Vec<Value>;
struct Rows {
    values: Vec<(PathBuf, Row)>,
    busy: bool,
    schema: bool,
}
fn db_error(error: &rusqlite::Error) -> &'static str {
    match error {
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) =>
        {
            "wallet_database_busy"
        }
        rusqlite::Error::SqliteFailure(_, Some(message))
            if message.contains("no such table") || message.contains("no such column") =>
        {
            "wallet_schema_mismatch"
        }
        _ => "wallet_database_read_failed",
    }
}
fn connection(path: &Path, duration: Duration) -> rusqlite::Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(duration.min(Duration::from_secs(1)))?;
    db.execute_batch("PRAGMA query_only=ON; BEGIN")?;
    Ok(db)
}
fn rows(db: &Connection, query: &str, args: &[&dyn ToSql]) -> rusqlite::Result<Vec<Row>> {
    let mut statement = db.prepare(query)?;
    let count = statement.column_count();
    statement
        .query_map(args, |row| {
            (0..count)
                .map(|i| {
                    Ok(match row.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Text(s) => json!(
                            std::str::from_utf8(s).map_err(|_| rusqlite::Error::InvalidQuery)?
                        ),
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    })
                })
                .collect()
        })?
        .collect()
}
impl Wallet {
    fn scan(&self, query: &str, args: &[&dyn ToSql], deadline: Instant) -> Result<Rows> {
        let mut result = Rows {
            values: Vec::new(),
            busy: false,
            schema: false,
        };
        for (index, path) in self.paths.iter().enumerate() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let observed = connection(path, remaining).and_then(|db| rows(&db, query, args));
            match observed {
                Ok(rows) => {
                    result.schema = true;
                    result
                        .values
                        .extend(rows.into_iter().map(|row| (path.clone(), row)));
                }
                Err(error) => match db_error(&error) {
                    "wallet_database_busy" => result.busy = true,
                    "wallet_schema_mismatch" => {}
                    other => return Err(fail(other)),
                },
            }
            if Instant::now() >= deadline {
                // A partial scan cannot rule out another matching database.
                result.busy |= index + 1 < self.paths.len();
                break;
            }
        }
        Ok(result)
    }
    fn select<T>(
        &self,
        query: &str,
        args: &[&dyn ToSql],
        missing: &str,
        choose: impl Fn(&Rows) -> Result<Option<T>>,
    ) -> Result<T> {
        let deadline = Instant::now() + self.timeout;
        let mut schema = false;
        loop {
            let observed = self.scan(query, args, deadline)?;
            schema |= observed.schema;
            if let Some(value) = choose(&observed)? {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                return Err(fail(if observed.busy {
                    "wallet_database_busy"
                } else if !schema {
                    "wallet_schema_mismatch"
                } else {
                    missing
                }));
            }
            sleep(
                self.retry
                    .min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
    fn one(&self, query: &str, args: &[&dyn ToSql], missing: &str) -> Result<(PathBuf, Row)> {
        self.select(query, args, missing, |rows| {
            if rows.busy {
                return Ok(None);
            }
            if rows.values.len() > 1 {
                return Err(fail("wallet_quote_ambiguous"));
            }
            Ok(rows.values.first().cloned())
        })
    }
}
fn text(value: &Value) -> Result<&str> {
    value.as_str().ok_or_else(|| fail("wallet_schema_mismatch"))
}
fn amount(value: &Value) -> Result<u64> {
    value.as_u64().ok_or_else(|| fail("wallet_amount_invalid"))
}

struct Receive {
    id: String,
    state: String,
    amount: u64,
    created: Value,
    paid: Value,
    expiry: Value,
    request: String,
}
impl Receive {
    fn read(wallet: &Wallet, id: &str, mint: Option<&str>) -> Result<Self> {
        let (_,row) = wallet.one("SELECT quote,mint,state,amount,created_time,paid_time,expiry,request FROM bolt11_mint_quotes WHERE quote=?1",&[&id],"mint_quote_missing")?;
        if text(&row[0])? != id {
            return Err(fail("mint_quote_identity_mismatch"));
        }
        if let Some(mint) = mint {
            if normalized_mint(text(&row[1])?) != normalized_mint(mint) {
                return Err(fail("mint_quote_mint_mismatch"));
            }
        }
        let amount = amount(&row[3])?;
        if !(1..=500_000).contains(&amount) {
            return Err(fail("mint_quote_amount_out_of_bounds"));
        }
        let request = text(&row[7])
            .map_err(|_| fail("mint_quote_request_missing"))?
            .to_owned();
        if request.is_empty() {
            return Err(fail("mint_quote_request_missing"));
        }
        Ok(Self {
            id: id.into(),
            state: text(&row[2])?.into(),
            amount,
            created: row[4].clone(),
            paid: row[5].clone(),
            expiry: row[6].clone(),
            request,
        })
    }
    fn artifact(&self, role: &str, wallet: &str, mint: &str) -> Value {
        let mut value = json!({"role":role,"direction":"receive","quote_id":self.id,"wallet_id":wallet,"mint_id":mint,"state":self.state,"amount_sat":self.amount});
        for (key, item) in [
            ("wallet_created_at_unix", &self.created),
            ("wallet_paid_at_unix", &self.paid),
            ("wallet_expires_at_unix", &self.expiry),
        ] {
            if !item.is_null() {
                value[key] = item.clone();
            }
        }
        value
    }
}
#[derive(Clone, PartialEq)]
struct Melt {
    path: PathBuf,
    id: String,
    state: String,
    amount: u64,
    reserve: u64,
    fee: Option<u64>,
}
impl Melt {
    fn parse(path: PathBuf, row: &Row) -> Result<Self> {
        Ok(Self {
            path,
            id: text(&row[0])?.into(),
            state: text(&row[1])?.into(),
            amount: amount(&row[2])?,
            reserve: amount(&row[3])?,
            fee: if row[4].is_null() {
                None
            } else {
                Some(amount(&row[4])?)
            },
        })
    }
    fn by_id(wallet: &Wallet, id: &str) -> Result<Self> {
        let (path, row) = wallet.one(
            "SELECT quote,state,amount,fee_reserve,fee_paid FROM bolt11_melt_quotes WHERE quote=?1",
            &[&id],
            "melt_quote_missing",
        )?;
        Self::parse(path, &row)
    }
    fn correlate(wallet: &Wallet, invoice: &str, before: &BTreeSet<String>) -> Result<Self> {
        wallet.select("SELECT quote,state,amount,fee_reserve,fee_paid FROM bolt11_melt_quotes WHERE lower(request)=lower(?1) ORDER BY created_time DESC",&[&invoice],"melt_quote_missing",|rows| {
            if rows.busy { return Ok(None); }
            let mut found = None;
            for (path,row) in &rows.values {
                if !before.contains(text(&row[0])?) {
                    if found.is_some() { return Err(fail("melt_quote_ambiguous")); }
                    found=Some(Self::parse(path.clone(),row)?);
                }
            }
            Ok(found)
        })
    }
    fn authoritative(mut self, config: &Config) -> Result<Self> {
        let Some(directory) = config.optional("PROOFSTORM_MINT_DB_DIR") else {
            return Ok(self);
        };
        let mut pending = vec![PathBuf::from(directory)];
        let mut paths = Vec::new();
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push(entry.path());
                } else if kind.is_file() && entry.file_name() == "mint.sqlite3" {
                    paths.push(entry.path());
                }
            }
        }
        if paths.is_empty() {
            self.fee = None;
            return Ok(self);
        }
        paths.sort();
        let database = Wallet {
            paths,
            timeout: config.seconds("PROOFSTORM_DB_TIMEOUT_SECONDS", 10.0, 1.0, 30.0)?,
            retry: config.seconds("PROOFSTORM_DB_RETRY_SECONDS", 0.2, 0.05, 2.0)?,
        };
        let (_, row) = database.one(
            "SELECT quote,state,amount,fee_reserve,fee_paid FROM melt_quotes WHERE quote=?1",
            &[&self.id],
            "mint_melt_quote_missing",
        )?;
        let observed = Self::parse(self.path, &row)?;
        if observed.id != self.id {
            return Err(fail("mint_melt_quote_identity_mismatch"));
        }
        Ok(observed)
    }
    fn artifact(&self, wallet: &str, mint: &str) -> Value {
        json!({"role":"payment_melt","direction":"pay","quote_id":self.id,"wallet_id":wallet,"mint_id":mint,"state":self.state,"amount_sat":self.amount,"fee_reserve_sat":self.reserve,"fee_paid_sat":self.fee})
    }
}

fn invoice_id(config: &Config) -> Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(config.get("PROOFSTORM_INVOICE_OUTPUT_PATH")?)?
        .take(65_537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err(fail("mint_quote_id_not_observed"));
    }
    let decoded = String::from_utf8_lossy(&bytes);
    let mut ids = BTreeSet::new();
    for rest in decoded.split("--id").skip(1) {
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        if let Some(value) = rest.split_whitespace().next() {
            if valid_id(value) {
                ids.insert(value.to_owned());
            }
        }
    }
    if ids.len() != 1 {
        return Err(fail("mint_quote_id_not_observed"));
    }
    ids.into_iter()
        .next()
        .ok_or_else(|| fail("mint_quote_id_not_observed"))
}
fn before_ids(config: &Config) -> Result<BTreeSet<String>> {
    let values: Vec<String> = serde_json::from_str(
        config
            .optional("PROOFSTORM_MELT_BEFORE_IDS")
            .unwrap_or("[]"),
    )
    .map_err(|_| fail("melt_before_ids_invalid"))?;
    if !values.iter().all(|s| valid_id(s)) {
        return Err(fail("melt_before_ids_invalid"));
    }
    Ok(values.into_iter().collect())
}

/// Read an explicitly requested private invoice for a private transport caller.
/// # Errors
/// Rejects mismatched mint, malformed identity and missing or ambiguous quote state.
pub fn private_invoice(home: &str, wallet: &str, mint: &str, id: &str) -> Result<String> {
    if !valid_id(id) {
        return Err(fail("mint_quote_id_invalid"));
    }
    let config = Config {
        variables: BTreeMap::from([
            ("HOME".into(), home.into()),
            ("PROOFSTORM_WALLET".into(), wallet.into()),
        ]),
    };
    Ok(Receive::read(&config.wallet(None, None)?, id, Some(mint))?.request)
}

/// Observe a quote using only private local state.
/// # Errors
/// Rejects missing, busy, incompatible, ambiguous or out-of-scope records.
pub fn observe(mode: &str, config: &Config) -> Result<Value> {
    let wallet = config.wallet(None, None)?;
    match mode {
        "observe-invoice" => {
            let quote = Receive::read(
                &wallet,
                &invoice_id(config)?,
                config.optional("PROOFSTORM_EXPECTED_MINT_URL"),
            )?;
            if !quote.state.eq_ignore_ascii_case("UNPAID") {
                return Err(fail("mint_quote_initial_state_unexpected"));
            }
            Ok(
                json!({"mint_quote_id":quote.id,"quote_observations":[quote.artifact("invoice_receive",config.get("PROOFSTORM_WALLET")?,config.get("PROOFSTORM_MINT")?)]}),
            )
        }
        "observe-receive" => {
            let quote = Receive::read(
                &wallet,
                config.id("PROOFSTORM_MINT_QUOTE_ID")?,
                config.optional("PROOFSTORM_EXPECTED_MINT_URL"),
            )?;
            Ok(quote.artifact(
                config.get("PROOFSTORM_OBSERVATION_ROLE")?,
                config.get("PROOFSTORM_WALLET")?,
                config.get("PROOFSTORM_MINT")?,
            ))
        }
        "observe-melt" => Ok(Melt::correlate(
            &wallet,
            config.get("PROOFSTORM_INVOICE")?,
            &before_ids(config)?,
        )?
        .authoritative(config)?
        .artifact(
            config.get("PROOFSTORM_WALLET")?,
            config.get("PROOFSTORM_MINT")?,
        )),
        _ => Err(fail("quote_driver_mode_invalid")),
    }
}

fn melt_ids(wallet: &Wallet) -> Result<BTreeSet<String>> {
    wallet.select(
        "SELECT quote FROM bolt11_melt_quotes",
        &[],
        "wallet_schema_mismatch",
        |rows| {
            if rows.busy {
                return Ok(None);
            }
            rows.values
                .iter()
                .map(|(_, row)| Ok(text(&row[0])?.to_owned()))
                .collect::<Result<_>>()
                .map(Some)
        },
    )
}

fn input_fee(wallet: &Wallet, quote: &Melt, mint: &str) -> Result<(u64, u64)> {
    wallet.select("SELECT COUNT(*), COALESCE(SUM(COALESCE((SELECT MAX(k.input_fee_ppk) FROM keysets k WHERE k.id=p.id AND lower(rtrim(k.mint_url,'/'))=lower(rtrim(?1,'/'))),0)),0), COALESCE(SUM(CASE WHEN (SELECT MAX(k.input_fee_ppk) FROM keysets k WHERE k.id=p.id AND lower(rtrim(k.mint_url,'/'))=lower(rtrim(?1,'/'))) IS NULL THEN 1 ELSE 0 END),0) FROM proofs_used p WHERE p.melt_id=?2",&[&mint,&quote.id],"melt_input_proofs_missing",|rows| {
        if rows.busy { return Ok(None); }
        let mut matched=Vec::new();
        for (_,row) in &rows.values { if amount(&row[0])?>0 { matched.push(row); } }
        if matched.len()>1 { return Err(fail("melt_input_proofs_ambiguous")); }
        if quote.state.eq_ignore_ascii_case("PAID") {
            if let Some(row)=matched.first() {
                if amount(&row[2])?>0 { return Err(fail("melt_input_keyset_missing")); }
                let count=amount(&row[0])?; let ppk=amount(&row[1])?;
                if count>10_000 || ppk>100_000_000 { return Err(fail("melt_input_fee_out_of_bounds")); }
                return Ok(Some((ppk.div_ceil(1000),count)));
            }
        } else {
            if !matched.is_empty() { return Err(fail("unpaid_melt_spent_proofs_present")); }
            if rows.schema { return Ok(Some((0,0))); }
        }
        Ok(None)
    })
}

async fn balance(cli: &impl WalletCli, home: &str, wallet: &str, mint: &str) -> Result<u64> {
    let output = cli
        .run(home, wallet, mint, &["balance"], Duration::from_secs(30))
        .await?;
    if output.code != 0 || output.truncated {
        return Err(fail("wallet_balance_unavailable"));
    }
    let decoded = String::from_utf8_lossy(&output.stdout);
    decoded
        .split("Balance:")
        .skip(1)
        .filter_map(|part| {
            let raw: String = part
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            raw.parse().ok()
        })
        .last()
        .ok_or_else(|| fail("wallet_balance_unavailable"))
}

async fn claim(config: &Config, cli: &impl WalletCli) -> Result<Value> {
    let wallet = config.wallet(None, None)?;
    let id = config.id("PROOFSTORM_MINT_QUOTE_ID")?;
    let mut quote = Receive::read(&wallet, id, config.optional("PROOFSTORM_EXPECTED_MINT_URL"))?;
    let already_issued = quote.state == "ISSUED";
    let code = if already_issued {
        0
    } else {
        let output = cli
            .run(
                config.get("HOME")?,
                config.get("PROOFSTORM_WALLET")?,
                config.get("PROOFSTORM_EXPECTED_MINT_URL")?,
                &["invoice", &quote.amount.to_string(), "--id", id],
                config.seconds("PROOFSTORM_CLAIM_TIMEOUT_SECONDS", 30.0, 1.0, 120.0)?,
            )
            .await?;
        quote = Receive::read(&wallet, id, config.optional("PROOFSTORM_EXPECTED_MINT_URL"))?;
        output.code
    };
    let mut artifact = json!({"mint_quote_id":id,"claim_exit_code":code,"already_issued":already_issued,
        "quote_observations":[quote.artifact("claim_receive",config.get("PROOFSTORM_WALLET")?,config.get("PROOFSTORM_MINT")?)]});
    if !matches!(
        quote.state.to_ascii_uppercase().as_str(),
        "UNPAID" | "PAID" | "ISSUED"
    ) {
        artifact["code"] = json!("unsupported_wallet_quote_state");
    }
    Ok(artifact)
}

async fn pay(config: &Config, cli: &impl WalletCli) -> Result<Value> {
    let payer_home = config.get("HOME")?;
    let payer_name = config.get("PROOFSTORM_WALLET")?;
    let payer_mint = config.get("PROOFSTORM_MINT")?;
    let payer_url = config.get("PROOFSTORM_EXPECTED_MINT_URL")?;
    let home = config.get("PROOFSTORM_RECIPIENT_HOME")?;
    let name = config.get("PROOFSTORM_RECIPIENT_WALLET")?;
    let mint = config.get("PROOFSTORM_RECIPIENT_MINT")?;
    let url = config.get("PROOFSTORM_RECIPIENT_MINT_URL")?;
    let id = config.id("PROOFSTORM_MINT_QUOTE_ID")?;
    let recipient = config.wallet(Some(home), Some(name))?;
    let receive = Receive::read(&recipient, id, Some(url))?;
    if !receive.state.eq_ignore_ascii_case("UNPAID") {
        let code = if matches!(
            receive.state.to_ascii_uppercase().as_str(),
            "PAID" | "ISSUED"
        ) {
            "mint_quote_not_payable"
        } else {
            "unsupported_wallet_quote_state"
        };
        return Ok(
            json!({"code":code,"mint_quote_id":id,"quote_observations":[receive.artifact("payment_receive",name,mint)]}),
        );
    }
    let payer = config.wallet(None, None)?;
    let before = melt_ids(&payer)?;
    let paid = cli
        .run(
            payer_home,
            payer_name,
            payer_url,
            &["pay", &receive.request],
            config.seconds("PROOFSTORM_PAY_TIMEOUT_SECONDS", 120.0, 1.0, 180.0)?,
        )
        .await?;
    let melt = Melt::correlate(&payer, &receive.request, &before)?.authoritative(config)?;
    let (input_fee_sat, input_proof_count) = input_fee(&payer, &melt, payer_url)?;
    let claim_code = if melt.state.eq_ignore_ascii_case("PAID") {
        Some(
            cli.run(
                home,
                name,
                url,
                &["invoice", &receive.amount.to_string(), "--id", id],
                config.seconds("PROOFSTORM_CLAIM_TIMEOUT_SECONDS", 30.0, 1.0, 120.0)?,
            )
            .await?
            .code,
        )
    } else {
        None
    };
    let receive = Receive::read(&recipient, id, Some(url))?;
    let mut artifact = json!({"mint_quote_id":id,"melt_quote_id":melt.id,"pay_exit_code":paid.code,"claim_exit_code":claim_code,
        "payer_balance_sat":balance(cli,payer_home,payer_name,payer_url).await?,"recipient_balance_sat":balance(cli,home,name,url).await?,
        "input_fee_sat":input_fee_sat,"input_proof_count":input_proof_count,
        "quote_observations":[melt.artifact(payer_name,payer_mint),receive.artifact("payment_receive",name,mint)]});
    if melt.state.eq_ignore_ascii_case("PAID") && !receive.state.eq_ignore_ascii_case("ISSUED") {
        artifact["code"] = json!("payment_paid_claim_unverified");
    } else if !matches!(
        melt.state.to_ascii_uppercase().as_str(),
        "UNPAID" | "PENDING" | "PAID"
    ) || !matches!(
        receive.state.to_ascii_uppercase().as_str(),
        "UNPAID" | "PAID" | "ISSUED"
    ) {
        artifact["code"] = json!("unsupported_wallet_quote_state");
    }
    Ok(artifact)
}

/// Run an explicitly requested wallet action; no private invoice appears in its result.
/// # Errors
/// Returns fixed failure reasons without raw CLI/RPC output or private database values.
pub async fn run(mode: &str, config: &Config) -> Result<Value> {
    match mode {
        "claim-receive" => claim(config, &Native).await,
        "pay-and-claim" => pay(config, &Native).await,
        "refresh-melt" => refresh::run(config).await,
        _ => observe(mode, config),
    }
}
