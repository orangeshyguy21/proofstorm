//! One headless OpenCode attempt; models are caller-selected, never substituted.
use super::{Context, events, observer, read, report::Report, save, score};
use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const PROMPT: &str = r#"Complete O1 autonomously using only Proofstorm MCP tools. Create exactly one cell named benchmark-o1 with these components and versions: chain (bitcoin-core 31.1), mint-lnd and payer-lnd (lnd 0.21.3-beta), mint (cdk 0.18.1), wallet (nutshell-wallet 0.21.0). Both LND nodes use chain/regtest; mint uses mint-lnd for bolt11/sat. Exactly three links: two chain_backend links from each LND node to chain, and one payment_backend link from mint to mint-lnd. Do not add a lightning_peer link. Discover the public schemas as needed. Use native component commands through cell_exec; host filesystem/shell/web tools are unavailable. Do not write or execute Python scripts.
Fund a direct channel between the two LND nodes with enough liquidity in both directions. Mine maturity and channel confirmations explicitly. Initialize wallet at http://mint:3338, obtain a 1000-sat mint quote, pay it from payer-lnd, and claim exactly 1000 sat. Use benchmark_checkpoint(stage=funded,mint_quote_id=...) to retain independent evidence before spending. Create a fresh 100-sat BOLT11 invoice on payer-lnd and melt wallet ecash to it. Verify recipient settlement and remaining wallet balance, then call benchmark_checkpoint(stage=paid,mint_quote_id=...,melt_quote_id=...,payment_hash=...,minted_sat=1000,paid_sat=100,remaining_sat=...). Total fees must be at most 10 sat. Quote/payment identity must agree with your observations.
Checkpoint calls only retain observations; they do not perform payments or award success. Both successful checkpoints must precede cell removal. Total successful Lightning payments must be exactly one 1000-sat funding payment from payer-lnd and one 100-sat receipt on payer-lnd: no additional offsetting mint/melt cycles. Resolve all native operations before removing the cell, then wait for verified closure with the original instance key. Keep evidence private; do not print seeds or credentials. Finish with exactly one JSON object and no surrounding prose or code fence: {"success":true,"minted_sat":1000,"paid_sat":100,"remaining_sat":NUMBER,"cleanup":true}. If anything fails, attempt cleanup and report success:false honestly. You have 1200 seconds. No human continuation or delegation.
Scoring separates task completion from report formatting. Extra prose before one complete trailing JSON object loses reporting points. Missing, ambiguous or incorrect structured claims fail report validation. Environment validity is checked separately by the runner.
"#;

fn command(config: &Context) -> Command {
    let mut cmd = Command::new(&config.opencode);
    crate::client::clear_runtime_environment(&mut cmd);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("OPENCODE_") {
            cmd.env_remove(key);
        }
    }
    cmd.current_dir(config.work.join("agent"))
        // OpenCode run resolves its directory from PWD before process.cwd().
        .env("PWD", config.work.join("agent"))
        .env("OPENCODE_DISABLE_AUTOUPDATE", "true")
        .env("OPENCODE_DISABLE_PROJECT_CONFIG", "true")
        .env("OPENCODE_CONFIG", config.work.join("agent/opencode.json"));
    cmd
}

pub fn run(config: &Context) -> Result<()> {
    let project = config.work.join("agent");
    fs::DirBuilder::new().mode(0o700).create(&project)?;
    let executable = std::env::current_exe()?;
    let entry = json!({"model":config.model,"share":"disabled","autoupdate":false,"snapshot":false,
        "permission":{"*":"deny","proofstorm_*":"allow"},
        "agent":{"benchmark":{"mode":"primary","description":"Proofstorm O1 benchmark",
            "prompt":"Complete the user's task autonomously using only the supplied Proofstorm MCP tools. Follow the checkpoint and cleanup contract.",
            "steps":150,"permission":{"*":"deny","proofstorm_*":"allow"}}},
        "mcp":{"proofstorm":{"type":"local","command":[executable,"--benchmark-proxy",config.work.join("benchmark-context.json")],"enabled":true,"timeout":120_000}}});
    save(&project.join("opencode.json"), &entry)?;
    let mut version = command(config);
    version.arg("--version");
    let version = crate::process::capture(version, 30)?;
    ensure!(version.status.success(), "OpenCode version check failed");
    let mut models = command(config);
    models.arg("models").arg(
        config
            .model
            .split('/')
            .next()
            .context("provider required")?,
    );
    let models = crate::process::capture(models, 60)?;
    ensure!(
        models.status.success()
            && String::from_utf8_lossy(&models.stdout)
                .lines()
                .any(|s| s.trim() == config.model),
        "requested provider/model unavailable; no substitution"
    );
    let mut effective = command(config);
    effective.args(["debug", "config"]);
    let effective = crate::process::capture(effective, 60)?;
    ensure!(
        effective.status.success(),
        "OpenCode configuration audit failed"
    );
    let effective: Value = serde_json::from_slice(&effective.stdout)?;
    save(&config.work.join("harness-config.private.json"), &effective)?;
    ensure!(
        effective["plugin"].as_array().is_none_or(Vec::is_empty),
        "plugins outside pilot profile"
    );
    ensure!(
        effective["mcp"]
            .as_object()
            .is_some_and(|m| m.len() == 1 && m.contains_key("proofstorm")),
        "additional MCP servers outside pilot profile"
    );
    ensure!(
        effective["mcp"]["proofstorm"]["command"] == entry["mcp"]["proofstorm"]["command"],
        "MCP command changed during configuration resolution"
    );
    ensure!(
        effective["permission"] == entry["permission"]
            && effective["agent"]["benchmark"]["permission"]
                == entry["agent"]["benchmark"]["permission"]
            && effective["agent"]["benchmark"]["steps"] == 150,
        "effective tool permissions differ"
    );
    // Verify a real connection before spending model tokens. Give discovery its
    // own capture directory, so it cannot consume the attempt's proxy identity.
    let preflight = config.work.join("transport-preflight");
    fs::DirBuilder::new().mode(0o700).create(&preflight)?;
    let mut preflight_context = json!(config);
    preflight_context["work"] = json!(preflight);
    let preflight_context_path = preflight.join("context.json");
    save(&preflight_context_path, &preflight_context)?;
    let mut preflight_entry = entry.clone();
    preflight_entry["mcp"]["proofstorm"]["command"][2] = json!(preflight_context_path);
    let preflight_config = preflight.join("opencode.json");
    save(&preflight_config, &preflight_entry)?;
    let mut probe = command(config);
    probe
        .env("OPENCODE_CONFIG", preflight_config)
        .args(["mcp", "list", "--pure"]);
    let probe = crate::process::capture(probe, 60)?;
    fs::write(
        config.work.join("harness-preflight.private.txt"),
        &probe.stdout,
    )?;
    ensure!(
        probe.status.success() && preflight.join("proxy-ready.json").exists(),
        "OpenCode did not connect to the owned benchmark proxy; model not started"
    );
    let executable_sha256 = format!("{:x}", sha2::Sha256::digest(fs::read(&executable)?));
    let mut source = Command::new("git");
    source.current_dir(&config.root).args(["rev-parse", "HEAD"]);
    let source = crate::process::capture(source, 30)?;
    ensure!(source.status.success(), "source revision unavailable");
    let mut dirty = Command::new("git");
    dirty
        .current_dir(&config.root)
        .args(["status", "--porcelain"]);
    let dirty = crate::process::capture(dirty, 30)?;
    ensure!(dirty.status.success(), "source state unavailable");
    save(
        &config.work.join("benchmark-manifest.json"),
        &json!({"task":score::task(),"model_requested":config.model,
        "model_resolved":config.model,"model_is_alias":true,"harness":"opencode","harness_version":String::from_utf8_lossy(&version.stdout).trim(),
        "harness_config_sha256":proofstorm_core::digest_json(&effective),"prompt_sha256":proofstorm_core::digest_json(&PROMPT),
        "source_revision":String::from_utf8_lossy(&source.stdout).trim(),"source_dirty":!dirty.stdout.is_empty(),"runner_sha256":executable_sha256,
        "platform":std::env::consts::ARCH,"os":std::env::consts::OS,"budget":{"wall_seconds":score::DEADLINE_SECONDS,"agent_steps":150,"spend_hard_limit":null,"token_hard_limit":null},
        "limitations":["provider credentials/config inherited; effective config audited and privately retained","provider model alias may change","time target provisional","no hard token/spend budget claimed"]}),
    )?;
    fs::write(config.work.join("prompt.txt"), PROMPT)?;
    let output = fs::File::create(config.work.join("harness.jsonl"))?;
    let error = fs::File::create(config.work.join("harness.stderr"))?;
    let start = Instant::now();
    save(
        &config.work.join("benchmark-attempt.json"),
        &json!({"outcome":"running","elapsed_seconds":null}),
    )?;
    let mut cmd = command(config);
    cmd.args([
        "run",
        "--pure",
        "--dir",
        project.to_str().context("project path is not UTF-8")?,
        "--format",
        "json",
        "--agent",
        "benchmark",
        "--model",
        &config.model,
        PROMPT,
    ])
    .stdin(Stdio::null())
    .stdout(output)
    .stderr(error);
    let mut child = cmd.spawn().context("start OpenCode")?;
    let outcome = loop {
        if let Some(status) = child.try_wait()? {
            break if status.success() {
                "completed"
            } else {
                "harness_failure"
            };
        }
        if start.elapsed().as_secs() >= score::DEADLINE_SECONDS {
            child.kill()?;
            child.wait()?;
            break "timeout";
        }
        if fs::metadata(config.work.join("harness.jsonl"))?.len() > 32 * 1024 * 1024 {
            child.kill()?;
            child.wait()?;
            break "output_limit";
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let elapsed = start.elapsed().as_secs_f64();
    let mut attempt = json!({"outcome":outcome,"elapsed_seconds":elapsed,"model":config.model,"usage":null,"cost":null});
    save(&config.work.join("benchmark-attempt.json"), &attempt)?;
    if outcome != "completed" {
        return Ok(());
    }
    let transcript = fs::read_to_string(config.work.join("harness.jsonl"))?;
    let rows = transcript
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let final_text = rows
        .iter()
        .rev()
        .find(|v| v["type"] == "text")
        .and_then(|v| v["part"]["text"].as_str())
        .unwrap_or("");
    let report = Report::parse(final_text);
    let errors = rows.iter().any(|v| v["type"] == "error");
    if errors {
        attempt["outcome"] = json!("provider_or_harness_failure");
    }
    let usage:Vec<_>=rows.iter().filter(|v|v["type"]=="step_finish").map(|v|json!({"tokens":v["part"]["tokens"],"cost":v["part"]["cost"],"reason":v["part"]["reason"]})).collect();
    attempt["usage"] = json!(usage);
    save(&config.work.join("benchmark-attempt.json"), &attempt)?;
    save(&config.work.join("agent-final.json"), &json!(report.claims))?;
    save(&config.work.join("agent-report.json"), &json!(report))?;
    observe_final(config, &report, errors)
}

pub(super) fn observe_final(config: &Context, report: &Report, errors: bool) -> Result<()> {
    let funded = read(&config.work.join("funded.json")).unwrap_or(Value::Null);
    let paid = read(&config.work.join("paid.json")).unwrap_or(Value::Null);
    let mut observations = observer::assertions(&funded, &paid);
    observations["autonomy"] = json!(!errors);
    let consistent = report.consistent(
        observations["report"] == true,
        paid["wallet"]["balance_sat"].as_u64(),
    );
    observations["report_valid"] = json!(consistent);
    observations["report_format"] = json!(report.format_valid);
    observations["report"] = json!(consistent && report.format_valid);
    let events = events(&config.work)?;
    observations["terminal"] = json!(observer::terminal_assertion(config, &events));
    save(
        &config.work.join("benchmark-observations.json"),
        &observations,
    )?;
    // A verified close must have been observed by the agent. Runner emergency cleanup earns no credit.
    let closed = events.iter().rev().find(|v| {
        v["kind"] == "end"
            && (v["tool"] == "cell_remove"
                || (v["tool"] == "cell_wait" && v["arguments"]["target_phase"] == "closed"))
            && v["success"] == true
            && observer::content(v)["teardown_receipt"]["verified_absent"] == true
    });
    let close = closed.map_or(Value::Null, observer::content);
    let ns = close["teardown_receipt"]["instance_namespace"].as_str();
    let mut absent = false;
    if let Some(ns) = ns {
        let install = proofstorm_app::installation::Installation::load(&config.home)?;
        let kube = crate::Kubectl::for_installation(&install)?;
        let namespaces = kube.get_json(&["get", "namespaces"])?;
        save(&config.work.join("cleanup-observation.json"), &namespaces)?;
        absent = namespaces["items"]
            .as_array()
            .is_some_and(|items| items.iter().all(|item| item["metadata"]["name"] != ns));
    }
    observations["agent_cleanup"] = json!(
        (close["reached"] == true || close["complete"] == true)
            && close["teardown_receipt"]["verified_absent"] == true
            && absent
            && (paid.is_null() || close["instance_key"] == paid["runtime"]["instance_key"])
    );
    save(
        &config.work.join("benchmark-observations.json"),
        &observations,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn harness_directory_and_ambient_config_are_explicit() {
        let config = Context {
            root: "/repo".into(),
            work: "/private/run".into(),
            home: "/private/run/state".into(),
            mcp: "/bin/mcp".into(),
            model: "provider/model".into(),
            opencode: "opencode".into(),
        };
        let command = command(&config);
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("/private/run/agent"))
        );
        let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env[std::ffi::OsStr::new("PWD")],
            Some(std::ffi::OsStr::new("/private/run/agent"))
        );
        assert_eq!(
            env[std::ffi::OsStr::new("OPENCODE_DISABLE_PROJECT_CONFIG")],
            Some(std::ffi::OsStr::new("true"))
        );
    }
}
