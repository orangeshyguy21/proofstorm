use super::common::{EXPERIMENT, INSTANCE};
use crate::{McpClient, json as expect};
use anyhow::Result;

pub(super) fn run(client: &mut McpClient) -> Result<Vec<String>> {
    let mut native = crate::native::Session::new(client, INSTANCE, EXPERIMENT);
    native.nutshell_initialize("wallet", "mint", "wallet-initialize")?;
    anyhow::ensure!(
        native.nutshell_balance("wallet", "mint", "wallet-balance")? == 0,
        "new wallet is not empty"
    );
    anyhow::ensure!(
        native.nutshell_fund("wallet", "mint", "payer-lnd", "wallet-fund", 1000)? == 1000,
        "wallet funding balance differs"
    );
    anyhow::ensure!(
        native.nutshell_fund("wallet", "mint", "payer-lnd", "round-trip-fund", 1000)? == 2000,
        "round-trip funding balance differs"
    );
    native.nutshell_swap("wallet", "mint", "round-trip", 100)?;
    native.nutshell_initialize("receiver-wallet", "mint", "receiver-initialize")?;
    anyhow::ensure!(
        native.nutshell_balance("receiver-wallet", "mint", "receiver-empty")? == 0,
        "receiver wallet is not empty"
    );
    let quote_id = native.nutshell_invoice("receiver-wallet", "mint", "wallet-invoice", 100)?;
    let invoice = native.nutshell_invoice_projection(
        "receiver-wallet",
        "mint",
        "wallet-invoice-read",
        &quote_id,
        100,
    )?;
    let before = native.nutshell_balance("wallet", "mint", "wallet-balance-before-pay")?;
    let melt = native.nutshell_melt(
        "wallet",
        "mint",
        "wallet-pay",
        expect::string(&invoice, "/payment_request")?,
        100,
    )?;
    let mint = native.nutshell_mint_melt("wallet", "mint", "wallet-pay-mint-observe", &melt)?;
    let after = native.nutshell_balance("wallet", "mint", "wallet-balance-after-pay")?;
    crate::native::assert_payment_accounting(before, after, &melt, &mint)?;
    native.nutshell_claim("receiver-wallet", "mint", "wallet-claim", &quote_id, 100)?;
    anyhow::ensure!(
        native.nutshell_balance("receiver-wallet", "mint", "wallet-received")? == 100,
        "recipient balance differs"
    );
    Ok(native.operations)
}
