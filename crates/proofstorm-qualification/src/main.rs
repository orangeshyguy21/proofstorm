use anyhow::{Context, Result, bail};
use proofstorm_qualification::{Identity, Mode, Plan, Receipt};
use std::fs;

fn read<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 16 * 1024 * 1024,
        "invalid qualification file"
    );
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn read_receipt(path: &std::path::Path) -> Result<Receipt> {
    anyhow::ensure!(
        path.extension().is_some_and(|value| value == "json"),
        "unexpected receipt artifact"
    );
    read(path.to_str().context("receipt path is not UTF-8")?)
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["policy", input] => {
            let paths = fs::read_to_string(input)?
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            println!(
                "{}",
                if proofstorm_qualification::documentation_only(&paths) {
                    "documentation"
                } else {
                    "compatibility"
                }
            );
        }
        ["plan", revision, run_id, attempt, mode, output] => {
            let mode = match *mode {
                "full" => Mode::Full,
                "compatibility" => Mode::Compatibility,
                "documentation" => Mode::Documentation,
                _ => bail!("unknown qualification mode"),
            };
            let plan = proofstorm_qualification::plan(
                Identity {
                    revision: (*revision).into(),
                    run_id: (*run_id).into(),
                    attempt: attempt.parse()?,
                },
                mode,
            )?;
            fs::write(output, serde_json::to_vec_pretty(&plan)?)?;
            println!(
                "{} cases; {} scheduled; {}",
                plan.cases.len(),
                plan.cases.iter().filter(|case| case.required).count(),
                plan.digest()
            );
        }
        ["verify", input, directory] => {
            let plan: Plan = read(input)?;
            let mut receipts = Vec::new();
            for file in fs::read_dir(directory)? {
                let file = file?;
                let path = file.path();
                if file.file_type()?.is_dir() {
                    for artifact in fs::read_dir(path)? {
                        receipts.push(read_receipt(&artifact?.path())?);
                    }
                } else {
                    receipts.push(read_receipt(&path)?);
                }
            }
            proofstorm_qualification::verify_receipts(&plan, &receipts)?;
            println!("Verified {} qualification receipts", receipts.len());
        }
        ["aggregate", input] => proofstorm_qualification::aggregate(&read(input)?)?,
        ["matrix", input] => {
            let plan: Plan = read(input)?;
            plan.validate()?;
            let mut entries = Vec::new();
            for (platform, runner) in [
                ("linux/amd64", "ubuntu-24.04"),
                ("linux/arm64", "ubuntu-24.04-arm"),
            ] {
                let cases: Vec<_> = plan
                    .cases
                    .iter()
                    .filter(|case| case.required && case.platform == platform)
                    .map(|case| case.id.as_str())
                    .collect();
                for (index, chunk) in cases.chunks(8).enumerate() {
                    let arch = platform.rsplit('/').next().unwrap();
                    entries.push(serde_json::json!({"platform":platform,"arch":arch,"runner":runner,"shard":format!("{arch}-{index}"),"cases":chunk}));
                }
            }
            println!("{}", serde_json::json!({"include":entries}));
        }
        ["case", input, id] => {
            let plan: Plan = read(input)?;
            plan.validate()?;
            println!("{}", serde_json::to_string(plan.case(id)?)?);
        }
        _ => bail!(
            "usage: qualification plan REVISION RUN_ID ATTEMPT compatibility|full|documentation OUTPUT; matrix PLAN; case PLAN ID; verify PLAN RECEIPTS; aggregate NEEDS"
        ),
    }
    Ok(())
}
