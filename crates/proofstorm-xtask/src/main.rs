//! Maintainer-only filesystem/metadata operations; not part of installed Proofstorm.
mod development;
mod release;

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let command = args.next().context(
        "expected prepare, resources, launchers, shell, release-check, or release-verify",
    )?;
    if command == "release-check" {
        return release::cli(args);
    }
    if command == "release-verify" {
        return release::verify_cli(args);
    }
    if let Some(command @ ("linux-install-prepare" | "linux-install-finish" | "release-run")) =
        command.to_str()
    {
        return release::linux_install_cli(command, args);
    }
    if let Some(
        command @ ("release-prepare"
        | "release-host-check"
        | "release-worker-prepare"
        | "linux-build-prepare"),
    ) = command.to_str()
    {
        return release::build_cli(command, args);
    }
    if command == "release-smoke" {
        return release::smoke_cli(args);
    }
    if command == "release-promotion" {
        return release::promotion_cli(args);
    }
    if let Some(command @ ("release-pack" | "release-extract" | "release-package")) =
        command.to_str()
    {
        return release::artifact_cli(command, args);
    }
    let source =
        PathBuf::from(args.next().context("expected checkout directory")?).canonicalize()?;
    let extra = args.next().map(PathBuf::from);
    if args.next().is_some() {
        bail!("unexpected arguments");
    }
    match command.to_str() {
        Some("prepare") => println!(
            "{}",
            development::prepare(&source, extra.as_deref())?.display()
        ),
        Some("resources") if extra.is_none() => {
            println!("{}", development::resources(&source)?.display());
        }
        Some("launchers") if extra.is_none() => development::launchers(&source)?,
        Some("shell") if extra.is_none() => std::process::exit(development::shell(&source)?),
        _ => bail!("invalid maintainer command or arguments"),
    }
    Ok(())
}
