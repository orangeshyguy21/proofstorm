//! Acceptance scenarios composed from independent native commands and receipts.
use super::{LND, Session, nutshell_balance, quote};
use crate::json as expect;
use anyhow::{Result, ensure};
use serde_json::{Value, json};

fn cashu(mint: &str) -> String {
    format!(
        "cashu -h {} -u sat -w wallet -t -y",
        quote(&format!("http://{mint}:3338"))
    )
}

impl Session<'_> {
    pub fn nutshell_initialize(&mut self, wallet: &str, mint: &str, id: &str) -> Result<()> {
        self.execute(
            wallet,
            id,
            &format!(
                "set -eu; cd /app; {} balance >/dev/null; exec /opt/proofstorm/driver holdings nutshell-wallet {} >/dev/null",
                cashu(mint), quote(wallet)
            ),
        )?;
        Ok(())
    }

    pub fn nutshell_balance(&mut self, wallet: &str, mint: &str, id: &str) -> Result<u64> {
        let receipt = self.execute(
            wallet,
            id,
            &format!(
                "exec /opt/proofstorm/driver holdings nutshell-wallet {}",
                quote(wallet)
            ),
        )?;
        let observed = nutshell_balance(&receipt, mint)?;
        ensure!(
            expect::integer(&observed, "/reserved_sat")? == 0,
            "wallet retains reserved proofs"
        );
        expect::integer(&observed, "/balance_sat")
    }

    pub fn nutshell_fund(
        &mut self,
        wallet: &str,
        mint: &str,
        payer: &str,
        id: &str,
        amount: u64,
    ) -> Result<u64> {
        ensure!(amount > 0, "funding amount must be positive");
        let before = self.nutshell_balance(wallet, mint, &format!("{id}-before"))?;
        let quote_id = self.nutshell_invoice(wallet, mint, &format!("{id}-quote"), amount)?;
        let invoice = self.nutshell_invoice_projection(
            wallet,
            mint,
            &format!("{id}-invoice"),
            &quote_id,
            amount,
        )?;
        let paid = self.projected(
            payer,
            &format!("{id}-pay"),
            &format!(
                "{LND} payinvoice --force --json {}",
                quote(expect::string(&invoice, "/payment_request")?)
            ),
            &json!({"mode":"json_fields","fields":["status","value_sat"]}),
        )?;
        ensure!(
            paid == json!({"status":"SUCCEEDED","value_sat":amount.to_string()}),
            "funding payment did not settle"
        );
        self.nutshell_claim(wallet, mint, &format!("{id}-claim"), &quote_id, amount)?;
        let after = self.nutshell_balance(wallet, mint, &format!("{id}-after"))?;
        ensure!(
            after.checked_sub(before) == Some(amount),
            "funding did not issue the expected amount"
        );
        Ok(after)
    }

    pub fn nutshell_invoice(
        &mut self,
        wallet: &str,
        mint: &str,
        id: &str,
        amount: u64,
    ) -> Result<String> {
        ensure!(
            (1..=500_000).contains(&amount),
            "invoice amount is outside the fixture bounds"
        );
        // Only the quote identifier leaves this command. The installed passive
        // reader supplies the invoice separately through the public BOLT11 projection.
        let created = self.execute(wallet, id, &format!(
            "set -eu; umask 077; cd /app; log=$(mktemp); trap 'rm -f \"$log\"' EXIT; {} invoice {amount} --no-check >\"$log\" 2>&1; sed -n 's/.*--id \\([^[:space:]]*\\).*/\\1/p' \"$log\"",
            cashu(mint)
        ))?;
        let quote_id = expect::string(&created, "/stdout")?.trim();
        ensure!(
            !quote_id.is_empty()
                && quote_id.len() <= 128
                && quote_id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-'),
            "native invoice did not return one quote identifier"
        );
        let observed = self.nutshell_receive(wallet, mint, &format!("{id}-observe"), quote_id)?;
        ensure!(
            observed["state"] == "UNPAID" && observed["amount_sat"].as_u64() == Some(amount),
            "native invoice did not create the requested unpaid quote"
        );
        Ok(quote_id.to_owned())
    }

    pub fn nutshell_invoice_projection(
        &mut self,
        wallet: &str,
        mint: &str,
        id: &str,
        quote_id: &str,
        amount: u64,
    ) -> Result<Value> {
        let invoice = self.projected(
            wallet,
            id,
            &format!(
                "exec /opt/proofstorm/driver private-invoice /wallet {} {} {}",
                quote(wallet),
                quote(&format!("http://{mint}:3338")),
                quote(quote_id)
            ),
            &json!({"mode":"bolt11"}),
        )?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        ensure!(
            invoice["currency"] == "bcrt"
                && invoice["amount_msat"].as_u64() == amount.checked_mul(1000)
                && expect::integer(&invoice, "/expires_at_unix")? > now,
            "native invoice amount, network, or expiry differs from the funding request"
        );
        Ok(invoice)
    }

    pub fn nutshell_receive(
        &mut self,
        wallet: &str,
        mint: &str,
        id: &str,
        quote_id: &str,
    ) -> Result<Value> {
        let observed = self.json(wallet, id, &format!(
            "exec env HOME=/wallet PROOFSTORM_WALLET={} PROOFSTORM_MINT={} PROOFSTORM_EXPECTED_MINT_URL={} PROOFSTORM_MINT_QUOTE_ID={} PROOFSTORM_OBSERVATION_ROLE=payment_receive /opt/proofstorm/driver quote observe-receive",
            quote(wallet), quote(mint), quote(&format!("http://{mint}:3338")), quote(quote_id)
        ))?;
        ensure!(
            observed["quote_id"] == quote_id
                && observed["wallet_id"] == wallet
                && observed["mint_id"] == mint
                && observed["direction"] == "receive",
            "receive quote identity differs"
        );
        Ok(observed)
    }

    pub fn nutshell_melt(
        &mut self,
        wallet: &str,
        mint: &str,
        id: &str,
        invoice: &str,
        amount: u64,
    ) -> Result<Value> {
        self.execute(
            wallet,
            id,
            &format!(
                "set -eu; cd /app; {} pay {} >/dev/null",
                cashu(mint),
                quote(invoice)
            ),
        )?;
        let observed = self.json(wallet, &format!("{id}-observe"), &format!(
            "exec env HOME=/wallet PROOFSTORM_WALLET={} PROOFSTORM_MINT={} PROOFSTORM_EXPECTED_MINT_URL={} PROOFSTORM_INVOICE={} /opt/proofstorm/driver quote observe-melt",
            quote(wallet), quote(mint), quote(&format!("http://{mint}:3338")), quote(invoice)
        ))?;
        ensure!(
            observed["wallet_id"] == wallet
                && observed["mint_id"] == mint
                && observed["amount_sat"].as_u64() == Some(amount)
                && observed["source"] == "wallet"
                && observed["direction"] == "pay",
            "melt quote identity or amount differs"
        );
        Ok(observed)
    }

    pub fn nutshell_mint_melt(
        &mut self,
        wallet: &str,
        mint: &str,
        id: &str,
        melt: &Value,
    ) -> Result<Value> {
        let quote_id = expect::string(melt, "/quote_id")?;
        let observed = self.json(mint, id, &format!(
            "exec env PROOFSTORM_WALLET={} PROOFSTORM_MINT={} PROOFSTORM_MINT_DB_DIR=/app/data PROOFSTORM_MELT_QUOTE_ID={} /opt/proofstorm/driver quote observe-mint-melt",
            quote(wallet), quote(mint), quote(quote_id)
        ))?;
        for key in ["wallet_id", "mint_id", "quote_id", "state", "amount_sat"] {
            ensure!(
                observed.get(key).is_some() && observed[key] == melt[key],
                "mint and wallet disagree on {key}"
            );
        }
        ensure!(observed["source"] == "mint", "missing mint observation");
        Ok(observed)
    }

    pub fn nutshell_claim(
        &mut self,
        wallet: &str,
        mint: &str,
        id: &str,
        quote_id: &str,
        amount: u64,
    ) -> Result<Value> {
        ensure!(amount > 0, "claim amount must be positive");
        self.execute(
            wallet,
            id,
            &format!(
                "set -eu; cd /app; {} invoice {amount} --id {} >/dev/null",
                cashu(mint),
                quote(quote_id)
            ),
        )?;
        let observed = self.nutshell_receive(wallet, mint, &format!("{id}-observe"), quote_id)?;
        ensure!(
            observed["quote_id"] == quote_id
                && observed["wallet_id"] == wallet
                && observed["mint_id"] == mint
                && observed["state"] == "ISSUED"
                && observed["amount_sat"].as_u64() == Some(amount),
            "native claim did not issue the expected quote: {observed}"
        );
        Ok(observed)
    }

    pub fn nutshell_swap(
        &mut self,
        wallet: &str,
        mint: &str,
        id: &str,
        tolerance: u64,
    ) -> Result<u64> {
        let before = self.nutshell_balance(wallet, mint, &format!("{id}-before"))?;
        self.execute(
            wallet,
            id,
            &format!("set -eu; cd /app; {} selfpay >/dev/null", cashu(mint)),
        )?;
        let after = self.nutshell_balance(wallet, mint, &format!("{id}-after"))?;
        ensure!(
            before
                .checked_sub(after)
                .is_some_and(|fee| fee <= tolerance),
            "self-swap inflated value or exceeded its fee tolerance"
        );
        Ok(after)
    }
}

/// Scenario accounting from independent native receipts. Unknown mint fees cannot
/// establish conservation, and the wallet's local fee field is never substituted.
pub fn assert_payment_accounting(
    before: u64,
    after: u64,
    melt: &Value,
    mint: &Value,
) -> Result<()> {
    for key in ["wallet_id", "mint_id", "quote_id", "amount_sat", "state"] {
        ensure!(
            melt.get(key).is_some() && melt[key] == mint[key],
            "mismatched payment accounting identity: {key}"
        );
    }
    ensure!(
        melt["source"] == "wallet" && mint["source"] == "mint" && mint["state"] == "PAID",
        "payment accounting requires a settled mint observation"
    );
    ensure!(
        expect::integer(melt, "/input_proof_count")? > 0,
        "paid melt has no input proofs"
    );
    let spent = expect::integer(melt, "/amount_sat")?
        .checked_add(expect::integer(mint, "/fee_paid_sat")?)
        .and_then(|amount| amount.checked_add(melt["input_fee_sat"].as_u64()?))
        .ok_or_else(|| anyhow::anyhow!("payment fee evidence missing or overflowing"))?;
    ensure!(
        before.checked_sub(spent) == Some(after),
        "payment balance does not match amount and observed fees"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounting_requires_exact_mint_fees_and_matching_native_observations() {
        let wallet = json!({"wallet_id":"payer","mint_id":"mint","quote_id":"quote","amount_sat":100,"state":"PAID","source":"wallet","fee_paid_sat":93,"input_fee_sat":2,"input_proof_count":3});
        let mint = json!({"wallet_id":"payer","mint_id":"mint","quote_id":"quote","amount_sat":100,"state":"PAID","source":"mint","fee_paid_sat":1});
        assert!(assert_payment_accounting(1000, 897, &wallet, &mint).is_ok());
        assert!(assert_payment_accounting(1000, 896, &wallet, &mint).is_err());
        for (key, value) in [
            ("fee_paid_sat", Value::Null),
            ("fee_paid_sat", json!(u64::MAX)),
            ("quote_id", json!("other")),
            ("state", json!("UNPAID")),
            ("source", json!("wallet")),
        ] {
            let mut invalid = mint.clone();
            invalid[key] = value;
            assert!(assert_payment_accounting(1000, 897, &wallet, &invalid).is_err());
        }
        assert!(assert_payment_accounting(10, 0, &wallet, &mint).is_err());
    }
}
