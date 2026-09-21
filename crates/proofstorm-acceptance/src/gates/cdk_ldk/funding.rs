//! Fund the upstream LDK Node wallet through its loopback dashboard. The
//! dashboard's cookie/CSRF exchange stays in private host memory and files.
use anyhow::{Context, Result, ensure};
use std::{
    thread::sleep,
    time::{Duration, Instant},
};

use crate::{GateContext, http};

pub(super) struct Dashboard {
    forward: http::PortForward,
    cookies: tempfile::NamedTempFile,
}

impl Dashboard {
    pub(super) fn open(context: &GateContext, namespace: &str) -> Result<Self> {
        Ok(Self {
            forward: http::PortForward::open(&context.kubectl, namespace, "deployment/mint", 8091)?,
            cookies: tempfile::NamedTempFile::new_in(context.work())?,
        })
    }

    fn get(&mut self) -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            ensure!(self.forward.running(), "LDK dashboard port-forward stopped");
            match http::curl(&[
                "--silent",
                "--show-error",
                "--fail",
                "--max-time",
                "5",
                "--cookie-jar",
                self.cookies.path().to_str().context("cookie path")?,
                &self.forward.url("/onchain?action=receive"),
            ]) {
                Ok(html) => return Ok(html),
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => sleep(Duration::from_secs(1)),
            }
        }
    }

    pub(super) fn new_address(&mut self) -> Result<String> {
        let html = self.get()?;
        let csrf = between(&html, "name=\"_csrf\" value=\"", "\"")
            .context("LDK dashboard address form has no CSRF field")?;
        ensure!(
            csrf.len() == 64 && csrf.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid dashboard CSRF field"
        );
        let html = http::curl(&[
            "--silent",
            "--show-error",
            "--fail",
            "--max-time",
            "10",
            "--cookie",
            self.cookies.path().to_str().context("cookie path")?,
            "--data-urlencode",
            &format!("_csrf={csrf}"),
            &self.forward.url("/onchain/new-address"),
        ])?;
        let address = between(&html, "class=\"address-text\">", "<")
            .context("LDK dashboard did not return an on-chain address")?;
        ensure!(
            address.starts_with("bcrt1") && address.bytes().all(|b| b.is_ascii_alphanumeric()),
            "LDK dashboard returned a non-regtest address"
        );
        Ok(address.into())
    }

    pub(super) fn wait_spendable(&mut self, minimum_sat: u64) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let html = self.get()?;
            if spendable(&html).is_some_and(|sat| sat >= minimum_sat) {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "LDK on-chain reserve did not become spendable"
            );
            sleep(Duration::from_secs(1));
        }
    }
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    text.split_once(start)?
        .1
        .split_once(end)
        .map(|(value, _)| value)
}

fn spendable(html: &str) -> Option<u64> {
    let before = html
        .split_once("<div class=\"metric-label\">Spendable Balance</div>")?
        .0;
    let value = before.rsplit_once("<div class=\"metric-value\">₿")?.1;
    value.split_once("</div>")?.0.replace(',', "").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spendable_balance_does_not_confuse_total_balance_or_unrelated_numbers() {
        let page = |spendable| {
            format!(
                "<div class=\"metric-value\">₿100,000</div><div class=\"metric-label\">Total Balance</div><div class=\"metric-value\">₿{spendable}</div><div class=\"metric-label\">Spendable Balance</div>"
            )
        };
        assert_eq!(super::spendable(&page("0")), Some(0));
        assert_eq!(super::spendable(&page("100,000")), Some(100_000));
        assert_eq!(super::spendable(&page("invalid")), None);
        assert_eq!(super::spendable("100,000"), None);
        assert_eq!(between("", "missing", "end"), None);
    }
}
