//! Maintainer-only filesystem/metadata operations; not part of installed Proofstorm.
mod development;

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let command = args
        .next()
        .context("expected prepare, resources, launchers, or shell")?;
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
