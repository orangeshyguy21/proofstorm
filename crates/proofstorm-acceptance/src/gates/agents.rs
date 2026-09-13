//! Attachment gates with private homes/projects. No models, trust bypass or native launch.
use crate::{GateContext, McpClient, process};
use anyhow::{Context, Result, ensure};
use proofstorm_core::Capability;
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn verify_launch(value: &serde_json::Value, agent: &str, project: &Path) -> Result<()> {
    let args = if agent == "codex" {
        json!(["--cd", project])
    } else {
        json!([])
    };
    ensure!(
        value["changes_applied"] == false
            && value["launch"]["interface"] == "cli"
            && value["launch"]["project"] == json!(project)
            && value["launch"]["arguments"] == args,
        "{agent} preview is not the expected project-scoped terminal launch"
    );
    Ok(())
}

fn private_user(command: &mut Command, user: &Path) {
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if ["CODEX_", "OPENCODE_", "CLAUDE_", "ANTHROPIC_", "OPENAI_"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            command.env_remove(key);
        }
    }
    let docker = std::env::var_os("DOCKER_CONFIG").map_or_else(
        || Path::new(&std::env::var_os("HOME").unwrap_or_default()).join(".docker"),
        Into::into,
    );
    command
        .env("HOME", user)
        .env("DOCKER_CONFIG", docker)
        .env("CODEX_HOME", user.join(".codex"))
        .env("CLAUDE_CONFIG_DIR", user.join(".claude"))
        .env("XDG_CONFIG_HOME", user.join(".config"))
        .env("XDG_CACHE_HOME", user.join(".cache"))
        .env("XDG_DATA_HOME", user.join(".local/share"))
        .env("XDG_STATE_HOME", user.join(".local/state"))
        .env("OPENCODE_DISABLE_AUTOUPDATE", "true")
        .env("OPENCODE_DISABLE_MODELS_FETCH", "true");
}

fn verify_client_edit(agent: &str, before: &[u8], after: &[u8]) -> Result<bool> {
    if before == after {
        return Ok(false);
    }
    // OpenCode adds its schema annotation when it first reads an existing config.
    // Admit only that annotation/JSON formatting, never a changed MCP command.
    ensure!(
        agent == "opencode",
        "{agent} client changed its project configuration"
    );
    let mut expected: serde_json::Value = serde_json::from_slice(before)?;
    expected["$schema"] = json!("https://opencode.ai/config.json");
    ensure!(
        serde_json::from_slice::<serde_json::Value>(after)? == expected,
        "OpenCode changed more than its schema annotation/formatting"
    );
    Ok(true)
}

fn discovery_status(agent: &str, text: &str, attached: bool) -> Result<&'static str> {
    let text = text.to_lowercase();
    if !attached {
        ensure!(
            !text.contains("proofstorm"),
            "{agent} discovered Proofstorm outside its selected project"
        );
        return Ok("not_configured");
    }
    ensure!(
        text.contains("proofstorm") && !text.contains("failed") && !text.contains("disconnected"),
        "{agent} did not discover a healthy or approval-pending Proofstorm entry"
    );
    // A fresh Claude project must ask its user before connecting. Discovery is
    // testable without granting trust; it must not be reported as a connection.
    if agent == "claude" && text.contains("pending approval") {
        Ok("pending_approval")
    } else {
        ensure!(text.contains("connected"), "{agent} MCP is not connected");
        Ok("connected")
    }
}

fn cli(
    context: &GateContext,
    user: &Path,
    project: &Path,
    fake_bin: Option<&Path>,
    args: &[&str],
) -> Result<Command> {
    let mut command = context.command(&["--json"])?;
    command.args(args).current_dir(project);
    private_user(&mut command, user);
    if let Some(bin) = fake_bin {
        command.env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH")?),
        );
    }
    Ok(command)
}

fn verify_replacement(
    context: &GateContext,
    user: &Path,
    project: &Path,
    fake_bin: Option<&Path>,
    agent: &str,
    path: &Path,
) -> Result<()> {
    let manual = fs::read(path)?;
    let database = super::onboarding::hash(context.database())?;
    for action in ["configure", "open"] {
        let preview = process::json(
            cli(
                context,
                user,
                project,
                fake_bin,
                &[
                    "agent",
                    action,
                    agent,
                    "--replace",
                    "--dry-run",
                    "--allow-development",
                ],
            )?,
            240,
        )?;
        ensure!(
            preview["changes_applied"] == false
                && preview["attachment"]["changes_configuration"] == true
                && fs::read(path)? == manual
                && super::onboarding::hash(context.database())? == database,
            "{agent} replacement preview wrote state"
        );
        if action == "open" {
            verify_launch(&preview, agent, project)?;
        }
    }
    let arguments = [
        "agent",
        "configure",
        agent,
        "--replace",
        "--allow-development",
    ];
    let replaced = process::json(cli(context, user, project, fake_bin, &arguments)?, 240)?;
    let backup = Path::new(
        replaced["backup"]
            .as_str()
            .context("replacement backup missing")?,
    );
    let expected = String::from_utf8(manual.clone())?.replace("manual-mcp", "proofstorm-mcp");
    ensure!(
        replaced["configuration_changed"] == true
            && replaced["actor_initialized"] == false
            && replaced["server_verified"]["environment_read"] == true
            && fs::read(backup)? == manual
            && fs::metadata(backup)?.permissions().mode() & 0o777 == 0o600
            && fs::read_to_string(path)? == expected,
        "{agent} replacement did not preserve settings, grants, or its private backup"
    );
    let repeated = process::json(cli(context, user, project, fake_bin, &arguments)?, 240)?;
    ensure!(
        repeated["configuration_changed"] == false
            && repeated["actor_initialized"] == false
            && repeated["backup"].is_null()
            && fs::read_to_string(path)? == expected,
        "{agent} --replace is not idempotent"
    );
    // A surviving connection can outlive its ownership receipt after a state reset.
    let actor = replaced["actor"]
        .as_str()
        .context("replacement actor missing")?;
    fs::remove_file(
        context
            .installation
            .home
            .join("attachments")
            .join(format!("{actor}.json")),
    )?;
    let ordinary = ["agent", "configure", agent, "--allow-development"];
    let refused = process::capture(cli(context, user, project, fake_bin, &ordinary)?, 240)?;
    ensure!(
        !refused.status.success()
            && String::from_utf8_lossy(&refused.stderr).contains("--replace")
            && fs::read_to_string(path)? == expected,
        "{agent} silently adopted an unowned connection"
    );
    let adopted = process::json(cli(context, user, project, fake_bin, &arguments)?, 240)?;
    let repeated = process::json(cli(context, user, project, fake_bin, &ordinary)?, 240)?;
    ensure!(
        adopted["configuration_changed"] == true
            && adopted["actor_initialized"] == false
            && repeated["configuration_changed"] == false
            && fs::read_to_string(path)? == expected,
        "{agent} did not record ownership of an identical replacement"
    );
    // Explicit replacement still refuses ambiguous duplicate connections.
    let duplicate = if agent == "codex" {
        format!("{expected}\n[mcp_servers.duplicate]\ncommand='/legacy/proofstorm-mcp'\n")
    } else {
        let mut value: serde_json::Value = serde_json::from_str(&expected)?;
        let key = if agent == "opencode" {
            "mcp"
        } else {
            "mcpServers"
        };
        value[key]["duplicate"] = value[key]["proofstorm"].clone();
        serde_json::to_string_pretty(&value)?
    };
    fs::write(path, &duplicate)?;
    let mut command = cli(context, user, project, fake_bin, &arguments)?;
    command.arg("--dry-run");
    let refused = process::capture(command, 240)?;
    ensure!(
        !refused.status.success()
            && String::from_utf8_lossy(&refused.stderr).contains("multiple Proofstorm connections")
            && fs::read_to_string(path)? == duplicate,
        "{agent} --replace accepted duplicate connections"
    );
    fs::write(path, expected)?;
    Ok(())
}

pub fn run(context: &GateContext, native_clients: bool) -> Result<()> {
    let work = context.work().join(if native_clients {
        "agent-clients"
    } else {
        "agent-config"
    });
    fs::create_dir(&work)?;
    let user = work.join("private user");
    let project = work.join("project with 'quotes'");
    let other = work.join("unconnected project");
    let bin = work.join("fake clients");
    for path in [&user, &project, &other, &bin] {
        fs::create_dir(path)?;
    }
    fs::create_dir(user.join(".codex"))?;
    fs::create_dir(project.join(".codex"))?;
    for (name, version, help) in [
        ("codex", "codex-cli 0.154.0", "--cd PATH"),
        ("opencode", "1.0.0", "opencode [project]"),
        ("claude", "2.0.0 (Claude Code)", "Claude Code --mcp-config"),
    ] {
        fs::write(
            bin.join(name),
            format!(
                "#!/bin/sh\ncase \"$1\" in --version) printf '%s\\n' '{version}';; --help) printf '%s\\n' '{help}';; *) exit 97;; esac\n"
            ),
        )?;
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755))?;
    }
    let fake = (!native_clients).then_some(bin.as_path());
    let mut actors = std::collections::BTreeSet::new();
    let mut report = json!({"passed":false,"actual_client_discovery":native_clients,"native_app_launch":"not_run","model_tool_call":false,"agents":[]});
    for (agent, filename, original) in [
        (
            "codex",
            ".codex/config.toml",
            "# preserve preferences\nmodel = \"fixture-model\"\n[mcp_servers.unrelated]\ncommand = \"unrelated\"\n",
        ),
        ("opencode", "opencode.json", "{\n  \"mcp\": {}\n}\n"),
        ("claude", ".mcp.json", "{\n  \"mcpServers\": {}\n}\n"),
    ] {
        let path = project.join(filename);
        eprintln!("Checking {agent} attachment preview, backup, and MCP connection...");
        fs::write(&path, original)?;
        let arguments = ["agent", "configure", agent, "--allow-development"];
        let mut preview = cli(context, &user, &project, fake, &arguments)?;
        preview.arg("--dry-run");
        let database = super::onboarding::hash(context.database())?;
        let dry = process::json(preview, 240)?;
        let launch = process::json(
            cli(
                context,
                &user,
                &project,
                fake,
                &["agent", "open", agent, "--dry-run", "--allow-development"],
            )?,
            240,
        )?;
        verify_launch(&launch, agent, &project)?;
        ensure!(
            dry["changes_applied"] == false
                && fs::read_to_string(&path)? == original
                && database == super::onboarding::hash(context.database())?,
            "attachment preview wrote state"
        );
        let attached = process::json(cli(context, &user, &project, fake, &arguments)?, 240)?;
        ensure!(
            actors.insert(
                attached["actor"]
                    .as_str()
                    .context("actor missing")?
                    .to_owned()
            ),
            "agents share an actor"
        );
        ensure!(
            attached["server_verified"]["environment_read"] == true
                && attached["harness_loaded"] == false,
            "managed MCP not verified"
        );
        ensure!(
            fs::read_to_string(attached["backup"].as_str().context("backup missing")?)? == original,
            "backup differs"
        );
        let mut configured = fs::read(&path)?;
        let repeated = process::json(cli(context, &user, &project, fake, &arguments)?, 240)?;
        ensure!(
            repeated["configuration_changed"] == false
                && repeated["actor_initialized"] == false
                && fs::read(&path)? == configured
                && !other.join(filename).exists(),
            "attachment not repeatable/project-scoped"
        );
        let entry = &dry["attachment"]["entry"];
        let (program, args) = if agent == "opencode" {
            let args = entry["command"].as_array().context("managed command")?;
            (&args[0], &args[1..])
        } else {
            (
                &entry["command"],
                entry["args"].as_array().context("managed args")?.as_slice(),
            )
        };
        let mut command = Command::new(program.as_str().context("MCP binary missing")?);
        command
            .args(
                args.iter()
                    .map(|arg| arg.as_str().context("MCP arg"))
                    .collect::<Result<Vec<_>>>()?,
            )
            .current_dir(&project);
        private_user(&mut command, &user);
        let poisoned = work.join("must-not-create.sqlite3");
        command
            .env("PROOFSTORM_MODE", "memory")
            .env("PROOFSTORM_DB", &poisoned)
            .env("PROOFSTORM_CONTEXT", "foreign")
            .env("PROOFSTORM_KUBECONFIG", work.join("foreign-kubeconfig"))
            .env("PROOFSTORM_WORKSPACE", "foreign")
            .env("PROOFSTORM_TOOLSET", "design");
        let mut client = McpClient::from_command(command, "attachment-verification")?;
        client.call("environment_read", json!({}))?;
        ensure!(
            !poisoned.exists(),
            "ambient overrides changed managed routing"
        );
        drop(client);
        let mut client_added_schema = false;
        let mut client_discovery = None;
        if native_clients && agent != "codex" {
            eprintln!("Checking actual {agent} client discovery (no model)...");
            for (directory, connected) in [(&project, true), (&other, false)] {
                let mut command = Command::new(agent);
                command.args(["mcp", "list"]).current_dir(directory);
                private_user(&mut command, &user);
                let output = process::capture(command, 180)?;
                context.record(&format!("agent-client-{agent}-{}.json", if connected { "attached" } else { "unrelated" }),
                    &json!({"exit_code":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"model_tool_call":false}))?;
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                ensure!(
                    output.status.success(),
                    "native MCP listing failed for {agent}"
                );
                let status = discovery_status(agent, &text, connected)?;
                if connected {
                    client_discovery = Some(status);
                }
            }
        }
        if native_clients && agent != "codex" {
            let current = fs::read(&path)?;
            client_added_schema = verify_client_edit(agent, &configured, &current)?;
            // Revocation must preserve the bytes present immediately before it.
            configured = current;
        }
        let actor = attached["actor"].as_str().context("actor missing")?;
        eprintln!("Checking {agent} revocation and manual-edit refusal...");
        let store = proofstorm_store::Store::open(context.database())?;
        store.replace_grants(
            proofstorm_app::config::DEFAULT_WORKSPACE,
            actor,
            proofstorm_app::developer::CAPABILITIES
                .into_iter()
                .filter(|cap| *cap != Capability::CellMaterialize),
        )?;
        let mut revoked = cli(context, &user, &project, fake, &arguments)?;
        revoked.arg("--replace");
        ensure!(
            !process::capture(revoked, 240)?.status.success(),
            "attachment restored revoked grants"
        );
        ensure!(
            fs::read(&path)? == configured,
            "failed attachment changed project config"
        );
        ensure!(
            store
                .authorize(
                    proofstorm_app::config::DEFAULT_WORKSPACE,
                    actor,
                    Capability::CellMaterialize
                )
                .is_err(),
            "revoked grant returned"
        );
        // Restore only this fixture's grants to test conflicts independently.
        store.replace_grants(
            proofstorm_app::config::DEFAULT_WORKSPACE,
            actor,
            proofstorm_app::developer::CAPABILITIES,
        )?;
        // Deliberately edited fixture bytes require explicit replacement consent.
        fs::write(
            &path,
            String::from_utf8(configured)?.replace("proofstorm-mcp", "manual-mcp"),
        )?;
        let manual = fs::read(&path)?;
        ensure!(
            !process::capture(cli(context, &user, &project, fake, &arguments)?, 240)?
                .status
                .success()
                && fs::read(&path)? == manual,
            "manual edit was overwritten"
        );
        eprintln!("Checking {agent} explicit replacement, preview, and backup...");
        verify_replacement(context, &user, &project, fake, agent, &path)?;
        report["agents"].as_array_mut().unwrap().push(json!({"name":agent,"version":launch["launch"]["version"],
            "server_verified":true,"backup_and_repeat":true,"revocation_preserved":true,"client_added_schema":client_added_schema,
            "replacement_preview_backup_and_repeat":true,"replacement_duplicates_refused":true,
            "replacement_adopts_unowned_connection":true,
            "client_discovery":client_discovery,
            "client_mcp_connected":client_discovery.map(|status| status == "connected")}));
    }
    report["passed"] = json!(true);
    context.record(
        if native_clients {
            "agent-clients.json"
        } else {
            "agent-config.json"
        },
        &report,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_distinguishes_approval_from_connection_and_rejects_failure() {
        assert_eq!(
            discovery_status("claude", "proofstorm: command - Pending approval", true).unwrap(),
            "pending_approval"
        );
        for agent in ["claude", "opencode"] {
            assert_eq!(
                discovery_status(agent, "proofstorm: Connected", true).unwrap(),
                "connected"
            );
            assert_eq!(
                discovery_status(agent, "No MCP servers configured", false).unwrap(),
                "not_configured"
            );
            for bad in [
                "proofstorm: disconnected",
                "proofstorm: Failed to connect",
                "proofstorm: configured",
                "unrelated: Connected",
            ] {
                assert!(discovery_status(agent, bad, true).is_err());
            }
            assert!(discovery_status(agent, "proofstorm: Connected", false).is_err());
        }
        assert!(discovery_status("opencode", "proofstorm: Pending approval", true).is_err());
        assert!(discovery_status("claude", "proofstorm: Pending approval; failed", true).is_err());
    }
    #[test]
    fn client_schema_annotation_never_permits_a_changed_mcp_entry() {
        let before = json!({"mcp":{"proofstorm":{"command":["proofstorm-mcp"]}}});
        let mut after = before.clone();
        after["$schema"] = json!("https://opencode.ai/config.json");
        let before = serde_json::to_vec(&before).unwrap();
        let valid = serde_json::to_vec(&after).unwrap();
        assert!(verify_client_edit("opencode", &before, &valid).unwrap());
        assert!(!verify_client_edit("claude", &before, &before).unwrap());
        assert!(verify_client_edit("claude", &before, &valid).is_err());
        after["mcp"]["proofstorm"]["command"] = json!(["foreign"]);
        assert!(
            verify_client_edit("opencode", &before, &serde_json::to_vec(&after).unwrap()).is_err()
        );
    }
    #[test]
    fn launch_oracle_checks_each_clients_actual_project_arguments() {
        let project = Path::new("/test/project with 'quotes'");
        for agent in ["codex", "opencode", "claude"] {
            let mut value = json!({"changes_applied":false,"launch":{"interface":"cli","project":project,
                "arguments":if agent == "codex" { json!(["--cd",project]) } else { json!([]) }}});
            verify_launch(&value, agent, project).unwrap();
            value["launch"]["arguments"] = json!(["--prompt", "do something"]);
            assert!(verify_launch(&value, agent, project).is_err());
            value["launch"]["arguments"] = if agent == "codex" {
                json!(["--cd", project])
            } else {
                json!([])
            };
            value["launch"]["interface"] = json!("desktop");
            assert!(verify_launch(&value, agent, project).is_err());
        }
    }
}
