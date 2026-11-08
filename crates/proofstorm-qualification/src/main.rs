use anyhow::{Context, Result, bail};
use proofstorm_qualification::{Identity, Mode, Plan, Receipt};
use std::{fs, path::Path};

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
        ["plan", revision, run_id, attempt, mode, output] => {
            let mode = match *mode {
                "full" => Mode::Full,
                "compatibility" => Mode::Compatibility,
                "documentation" => Mode::Documentation,
                "pull" => Mode::Pull,
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
        ["matrix", input] => {
            let plan: Plan = read(input)?;
            plan.validate()?;
            println!("{}", proofstorm_qualification::matrix(&plan));
        }
        ["shard", plan_path, cases, work, receipts, previous @ ..] if previous.len() <= 1 => {
            let plan: Plan = read(plan_path)?;
            let ids: Vec<String> = read(cases)?;
            let passed = proofstorm_qualification::run_shard(
                Path::new(plan_path),
                &plan,
                &ids,
                Path::new(work),
                Path::new(receipts),
                previous.first().map(Path::new),
            )?;
            if !passed {
                std::process::exit(1);
            }
        }
        ["case", input, id] => {
            let plan: Plan = read(input)?;
            plan.validate()?;
            println!("{}", serde_json::to_string(plan.case(id)?)?);
        }
        ["images", input] => {
            let plan: Plan = read(input)?;
            println!(
                "{}",
                serde_json::to_string(&proofstorm_qualification::image_inputs(&plan)?)?
            );
        }
        _ => bail!(
            "usage: qualification plan REVISION RUN_ID ATTEMPT compatibility|full|documentation|pull OUTPUT; matrix PLAN; shard PLAN CASES WORK RECEIPTS [EARLIER_RECEIPTS]; case PLAN ID; images PLAN; verify PLAN RECEIPTS"
        ),
    }
    Ok(())
}
