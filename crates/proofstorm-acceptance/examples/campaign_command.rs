//! Deadline/output-bounded child process for opt-in shell diagnostics.
use anyhow::{Context, Result};
use proofstorm_acceptance::process;
use std::{io::Write, process::Command};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let seconds = args
        .next()
        .context("expected deadline in seconds")?
        .to_str()
        .context("invalid deadline")?
        .parse()?;
    let mut command = Command::new(args.next().context("expected command")?);
    command.args(args);
    let result = process::capture(command, seconds)?;
    std::io::stdout().write_all(&result.stdout)?;
    std::io::stderr().write_all(&result.stderr)?;
    std::process::exit(result.status.code().unwrap_or(1));
}
