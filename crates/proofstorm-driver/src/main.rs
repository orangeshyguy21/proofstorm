use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::Path;

fn required(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("missing {name}"))
}

fn install() -> Result<()> {
    let destination = Path::new(proofstorm_driver::BINARY);
    let temporary =
        tempfile::NamedTempFile::new_in(destination.parent().context("driver directory missing")?)?;
    std::fs::copy(std::env::current_exe()?, temporary.path())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o555))?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(destination)?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "explicit native command dispatch keeps each input and output contract together"
)]
async fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [command] if command == "install" => return install(),
        #[cfg(unix)]
        [command] if command == "exec-ldk-processor" => {
            return proofstorm_driver::processor::exec_ldk_processor();
        }
        [command, address, tls, profile] if command == "processor-settings" => {
            serde_json::to_value(
                proofstorm_driver::processor::settings_for(
                    address,
                    Path::new(tls),
                    profile.parse()?,
                )
                .await?,
            )?
        }
        [command] if command == "--self-check" => {
            json!({"driver_version":proofstorm_driver::VERSION})
        }
        [command, implementation, fields @ ..] if command == "observe" && fields.len() <= 1 => {
            if fields.first().is_some_and(|field| field != "balance_sat") {
                bail!("unsupported observation field");
            }
            let path = required("PROOFSTORM_DATABASE")?;
            let wallet = required("PROOFSTORM_WALLET")?;
            let mint = required("PROOFSTORM_MINT")?;
            let url = required("PROOFSTORM_MINT_URL")?;
            let observation = match implementation.as_str() {
                "cdk-cli-wallet" => {
                    proofstorm_driver::wallet::cdk(Path::new(&path), &wallet, &mint, &url)?
                }
                "cocod-wallet" => {
                    proofstorm_driver::wallet::coco(Path::new(&path), &wallet, &mint, &url)?
                }
                _ => bail!("unsupported wallet"),
            };
            if fields.is_empty() {
                observation
            } else {
                observation["balance_sat"].clone()
            }
        }
        [command, implementation, wallet] if command == "holdings" => {
            proofstorm_driver::wallet::holdings(implementation, Path::new("/wallet"), wallet)?
        }
        [command, mode] if command == "authentication" => {
            let config = proofstorm_driver::authentication::Config::environment()?;
            proofstorm_driver::authentication::run(mode, &config).await?
        }
        [command, mode] if command == "nutshell" => {
            #[cfg(unix)]
            if mode == "rune-probe" {
                println!("{}", proofstorm_driver::nutshell::rune_probe().await?);
                return Ok(());
            }
            proofstorm_driver::nutshell::settings(mode, &std::env::vars().collect()).await?
        }
        [command, mode] if command == "quote" => {
            let config = proofstorm_driver::quote::Config::environment();
            proofstorm_driver::quote::observe(mode, &config)?
        }
        [command, mode, arguments @ ..] if command == "coco" => {
            proofstorm_driver::coco::Coco::new(
                Path::new("/wallet/.cocod"),
                Path::new("/wallet/session.passphrase"),
                "http://127.0.0.1:62626",
            )?
            .run(mode, arguments)
            .await?
        }
        [command, mode, address, directory] if command == "management" => serde_json::to_value(
            proofstorm_driver::management::inspect(address, Path::new(directory), mode).await?,
        )?,
        [command, url] if command == "http-json" => {
            let client = proofstorm_driver::http::client(std::time::Duration::from_secs(3))?;
            let (status, value) = proofstorm_driver::http::json(client.get(url)).await?;
            anyhow::ensure!(status.is_success(), "HTTP request failed");
            value
        }
        [command, host, port] if command == "tcp" => {
            let port: u16 = port.parse()?;
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                tokio::net::TcpStream::connect((host.as_str(), port)),
            )
            .await??;
            return Ok(());
        }
        [command, field, state, path, mint, amount] if command == "cdk-quote" => {
            println!(
                "{}",
                proofstorm_driver::inspect::cdk_quote(
                    Path::new(path),
                    mint,
                    amount.parse()?,
                    state,
                    field
                )?
            );
            return Ok(());
        }
        [command, path] if command == "cdk-melt-receipt" => {
            proofstorm_driver::inspect::cdk_melt_receipt(Path::new(path))?
        }
        [command, name] if command == "process-absent" => {
            proofstorm_driver::inspect::process_absent(Path::new("/proc"), name)?;
            return Ok(());
        }
        [command, home, wallet, mint, id] if command == "private-invoice" => {
            println!(
                "{}",
                proofstorm_driver::quote::private_invoice(home, wallet, mint, id)?
            );
            return Ok(());
        }
        [command, implementation] if command == "ready" && implementation == "cocod" => {
            proofstorm_driver::http::coco_ready().await?;
            return Ok(());
        }
        [command, implementation, url] if command == "ready" && implementation == "http" => {
            proofstorm_driver::http::mint_ready(url).await?;
            return Ok(());
        }
        [command, implementation, url] if command == "ready" && implementation == "nutshell" => {
            proofstorm_driver::management::nutshell_ready(url).await?;
            return Ok(());
        }
        #[cfg(unix)]
        [command, payment] if command == "cln-mint-rune" && payment == "xpay" => {
            proofstorm_driver::cln::mint_rune_for_payment(
                Path::new("/cln/regtest/lightning-rpc"),
                Path::new("/app/data/.proofstorm/cln.rune"),
                payment,
            )
            .await?;
            return Ok(());
        }
        #[cfg(unix)]
        [command] if command == "cln-mint-rune" => {
            proofstorm_driver::cln::mint_rune(
                Path::new("/cln/regtest/lightning-rpc"),
                Path::new("/app/data/.proofstorm/cln.rune"),
            )
            .await?;
            return Ok(());
        }
        _ => bail!("unsupported driver command"),
    };
    println!("{result}");
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        // Paths, credentials, proofs and remote response bodies are private.
        let arguments: Vec<_> = std::env::args().skip(1).collect();
        let code = match arguments.as_slice() {
            [command, mode] if command == "authentication" => match mode.as_str() {
                "conformance" => "authentication_conformance_failed",
                "protected-spend" => "authentication_protected_spend_failed",
                "replay" => "authentication_replay_failed",
                _ => "component_driver_failed",
            },
            [command, ..] if matches!(command.as_str(), "holdings" | "observe" | "quote") => {
                "wallet_orchestration_failed"
            }
            _ => "component_driver_failed",
        };
        let (stage, reason) = match arguments.first().map(String::as_str) {
            Some("quote") => (
                "quote",
                error
                    .downcast_ref::<proofstorm_driver::quote::Failure>()
                    .map_or("quote_driver_failed", |failure| failure.0.as_str()),
            ),
            Some("holdings" | "observe") => {
                ("observation", "database_missing_busy_or_incompatible")
            }
            _ => ("driver", "unexpected_driver_error"),
        };
        println!("{}", json!({"code":code,"stage":stage,"reason":reason}));
        std::process::exit(1);
    }
}
