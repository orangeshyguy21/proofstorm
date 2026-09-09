use super::*;
use std::fs;

fn entry() -> Value {
    json!({"command":"/a path/it's installed/proofstorm-mcp","args":["--home","/home with spaces","--attachment","codex-example"],
        "cwd":"/project with spaces","required":true,"enabled":true,"startup_timeout_sec":60,"tool_timeout_sec":1800})
}

#[test]
fn merge_preserves_comments_other_servers_and_provider_settings() {
    let original = "# Keep my configuration\nmodel = 'my-model' # model note\n\n[model_providers.custom]\nbase_url = 'https://example.invalid'\n\n[mcp_servers.other]\ncommand = 'other-server' # other note\n";
    let path = Path::new("/fixture/.codex/config.toml");
    let output = config::merge(path, Some(original), &entry(), &[]).unwrap();
    assert!(output.starts_with(original), "{output}");
    assert_eq!(
        config::value(&config::document(path, &output).unwrap()).unwrap()["mcp_servers"]["proofstorm"],
        entry()
    );
    let again = config::merge(path, Some(&output), &entry(), &[entry()]).unwrap();
    assert_eq!(output, again);
}

#[test]
fn manual_modified_duplicate_and_malformed_entries_are_refused() {
    let path = Path::new("/fixture/config.toml");
    for text in [
        "broken = [",
        "[mcp_servers.proofstorm]\ncommand='manual'",
        "mcp_servers = 'wrong-type'",
        "[mcp_servers.another]\ncommand='/somewhere/proofstorm-mcp'",
    ] {
        assert!(config::merge(path, Some(text), &entry(), &[]).is_err());
    }
    let output = config::merge(path, None, &entry(), &[]).unwrap();
    let changed = output.replace("1800", "100");
    assert!(config::merge(path, Some(&changed), &entry(), &[entry()]).is_err());
    assert!(config::merge(path, Some(&output), &entry(), &[]).is_err());
}

#[test]
fn recorded_previous_entry_supports_an_interrupted_upgrade() {
    let path = Path::new("/fixture/config.toml");
    let old = entry();
    let output = config::merge(path, None, &old, &[]).unwrap();
    let mut new = old.clone();
    new["startup_timeout_sec"] = json!(90);
    let pending = [new.clone(), old];
    let updated = config::merge(path, Some(&output), &new, &pending).unwrap();
    assert_eq!(
        updated,
        config::merge(path, Some(&updated), &new, &pending).unwrap()
    );
}

#[test]
fn inherited_conflicts_and_selected_profiles_fail_without_writes() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("codex-home");
    let project = root.path().join("project");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&project).unwrap();
    let system = root.path().join("system.toml");
    config::inherited(&project, &home, &system).unwrap();
    fs::write(home.join("config.toml"), "profile='custom'").unwrap();
    fs::write(
        home.join("custom.config.toml"),
        "[mcp_servers.proofstorm]\ncommand='manual'",
    )
    .unwrap();
    assert!(config::inherited(&project, &home, &system).is_err());
    assert!(!project.join(".codex").exists());
}

#[test]
fn backups_are_private_repeatable_and_never_overwrite_conflicts() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    fs::write(&path, "# original").unwrap();
    let backup = config::backup(&path, "# original").unwrap();
    assert_eq!(backup, config::backup(&path, "# original").unwrap());
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(&backup, "foreign").unwrap();
    assert!(config::backup(&path, "# original").is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), "# original");
}

#[test]
fn linked_files_and_directories_are_not_adopted() {
    let root = tempfile::tempdir().unwrap();
    let foreign = root.path().join("foreign");
    fs::write(&foreign, "# do not change").unwrap();
    let link = root.path().join("config.toml");
    std::os::unix::fs::symlink(&foreign, &link).unwrap();
    assert!(config::read(&link).is_err());
    assert!(config::save(&link, b"new", false).is_err());
    let dir = root.path().join(".codex");
    std::os::unix::fs::symlink(root.path(), &dir).unwrap();
    assert!(config::directory(&dir, true).is_err());
    assert_eq!(fs::read_to_string(foreign).unwrap(), "# do not change");
}

#[test]
fn json_agents_have_native_config_shapes_and_distinct_actor_arguments() {
    let normalized = entry();
    let oc = agents::entry(Harness::Opencode, &normalized).unwrap();
    assert_eq!(oc["command"][0], normalized["command"]);
    assert_eq!(oc["command"][4], "codex-example");
    assert_eq!(oc["enabled"], true);
    assert_eq!(oc["timeout"], 60000);
    assert!(oc.get("cwd").is_none());
    let claude = agents::entry(Harness::Claude, &normalized).unwrap();
    assert_eq!(
        claude,
        json!({"type":"stdio","command":normalized["command"],"args":normalized["args"]})
    );
    for harness in [Harness::Codex, Harness::Opencode, Harness::Claude] {
        let identity =
            serde_json::to_string(&json!(["installation", "/project", harness.name()])).unwrap();
        let other =
            serde_json::to_string(&json!(["installation", "/other", harness.name()])).unwrap();
        assert_ne!(hash(identity.as_bytes()), hash(other.as_bytes()));
    }
}

#[test]
fn jsonc_preserves_comments_order_other_servers_and_idempotency() {
    let original = "// keep header\n{\n  \"model\": \"custom/model\", // keep model\n  \"mcp\": {\n    \"other\": {\"type\":\"local\", \"command\":[\"other\"],}, /* keep other */\n  },\n  \"permission\": {\"bash\": \"ask\",},\n}\n// keep footer\n";
    let entry = agents::entry(Harness::Opencode, &entry()).unwrap();
    let out = json_config::merge(Some(original), "mcp", &entry, &[], true).unwrap();
    for part in [
        "// keep header",
        "// keep model",
        "/* keep other */",
        "\"permission\": {\"bash\": \"ask\",}",
        "// keep footer",
    ] {
        assert!(out.contains(part));
    }
    assert_eq!(
        out,
        json_config::merge(Some(&out), "mcp", &entry, &[entry.clone()], true).unwrap()
    );
    let mut upgraded = entry.clone();
    upgraded["timeout"] = json!(90000);
    let next = json_config::merge(Some(&out), "mcp", &upgraded, &[entry], true).unwrap();
    assert!(next.contains("/* keep other */"));
    assert_eq!(
        json_config::value(&next, true).unwrap()["mcp"]["proofstorm"],
        upgraded
    );
}

#[test]
fn json_edits_reject_ambiguous_manual_modified_and_duplicate_entries() {
    let entry = agents::entry(Harness::Claude, &entry()).unwrap();
    for text in [
        "[]",
        "{broken}",
        "{\"mcpServers\":[]}",
        "{\"a\":1,\"a\":2}",
        "{\"x\":{\"a\":1,\"a\":2}}",
        "{\"mcpServers\":{\"proofstorm\":{}}}",
        "{\"mcpServers\":{\"alias\":{\"command\":\"/path/proofstorm-mcp\"}}}",
        "{\"a\":1,}",
        "{/* comment */}",
        "{\"a\":01}",
    ] {
        assert!(
            json_config::merge(Some(text), "mcpServers", &entry, &[], false).is_err(),
            "accepted {text}"
        );
    }
    let out = json_config::merge(None, "mcpServers", &entry, &[], false).unwrap();
    assert!(json_config::merge(Some(&out), "mcpServers", &entry, &[], false).is_err());
    assert!(
        json_config::merge(
            Some(&out.replace("stdio", "http")),
            "mcpServers",
            &entry,
            &[entry.clone()],
            false
        )
        .is_err()
    );
    assert_eq!(
        out,
        json_config::merge(Some(&out), "mcpServers", &entry, &[entry.clone()], false).unwrap()
    );
}

#[test]
fn jsonc_insertion_handles_empty_trailing_commas_escaped_keys_and_arrays() {
    let entry = json!({"command":["/a path/it's/proofstorm-mcp","--home","/home"]});
    for text in [
        "{}",
        "{/*hi*/}",
        "{\"mcp\":{}}",
        "{\"mcp\":{\"other\":{},}}",
        "{\"mcp\":{\"other\":{} /*hi*/}}",
        "{\"a\":[1,{\"b\":[true,null,\"é\\\"\",],},],}",
        "{\"m\\u0063p\":{}}",
    ] {
        let out = json_config::merge(Some(text), "mcp", &entry, &[], true).unwrap();
        assert_eq!(
            json_config::value(&out, true).unwrap()["mcp"]["proofstorm"],
            entry
        );
    }
    for text in [
        "{\"mcp\":{\"proofstorm\":1,\"proof\\u0073torm\":2}}",
        "{/*",
        "{\"a\": [1,,]}",
        "{\"a\": \"unterminated\\",
    ] {
        assert!(json_config::value(text, true).is_err());
    }
}

#[test]
fn agent_paths_detect_alternate_files_links_and_back_up_correct_filename() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        agents::path(Harness::Claude, root.path()).unwrap(),
        root.path().join(".mcp.json")
    );
    assert_eq!(
        agents::path(Harness::Opencode, root.path()).unwrap(),
        root.path().join("opencode.json")
    );
    fs::write(root.path().join("opencode.jsonc"), "{}").unwrap();
    assert_eq!(
        agents::path(Harness::Opencode, root.path()).unwrap(),
        root.path().join("opencode.jsonc")
    );
    let backup = config::backup(&root.path().join("opencode.jsonc"), "{}").unwrap();
    assert!(
        backup
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("opencode.jsonc.proofstorm-backup-")
    );
    fs::write(root.path().join("opencode.json"), "{}").unwrap();
    assert!(agents::path(Harness::Opencode, root.path()).is_err());
}

#[test]
fn literal_paths_versions_and_terminal_quoting_fail_safe() {
    for (harness, marker) in [
        (Harness::Opencode, "{env:HOME}"),
        (Harness::Opencode, "{file:secret}"),
        (Harness::Claude, "${HOME}"),
    ] {
        let mut entry = entry();
        entry["cwd"] = json!(format!("/tmp/{marker}"));
        assert!(agents::entry(harness, &entry).is_err());
    }
    assert!(launch::supported_version(Harness::Opencode, "1.18.30\n").is_ok());
    for bad in ["2.0.0", "1.beta", "", "opencode 1.2.3"] {
        assert!(launch::supported_version(Harness::Opencode, bad).is_err());
    }
    assert!(launch::supported_version(Harness::Claude, "2.1.69 (Claude Code)").is_ok());
    assert!(launch::supported_version(Harness::Claude, "3.0.0 (Claude Code)").is_err());
    let plan = launch::LaunchPlan {
        executable: "/a path/it's/claude".into(),
        project: "/project/$(no); it's".into(),
        version: "test".into(),
        interface: "cli",
        arguments: vec![],
        desktop: None,
    };
    assert_eq!(
        launch::terminal_command(&plan),
        "cd -- '/project/$(no); it'\\''s' && '/a path/it'\\''s/claude'"
    );
}

#[test]
fn json_inherited_global_local_and_aliased_servers_are_read_only_conflicts() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("global.json");
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    for (agent, content) in [
        (
            Harness::Claude,
            json!({"mcpServers":{"proofstorm":{"command":"manual"}}}),
        ),
        (
            Harness::Claude,
            json!({"projects":{project.to_str().unwrap():{"mcpServers":{"proofstorm":{}}}}}),
        ),
        (
            Harness::Opencode,
            json!({"mcp":{"alias":{"command":["/path/proofstorm-mcp"]}}}),
        ),
    ] {
        let text = serde_json::to_string(&content).unwrap();
        fs::write(&path, &text).unwrap();
        assert!(agents::check_paths(agent, &project, [path.clone()]).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert_eq!(fs::read_dir(&project).unwrap().count(), 0);
    }
    fs::write(
        &path,
        "{\"mcpServers\":{\"unrelated\":{\"command\":\"other\"}}}",
    )
    .unwrap();
    agents::check_paths(Harness::Claude, &project, [path]).unwrap();
}

#[test]
fn successful_help_on_stderr_is_inspected_without_exposing_launch_diagnostics() {
    let output = launch::capture(
        Path::new("/bin/sh"),
        &["-c", "printf 'opencode [project]' >&2", "--help"],
    )
    .unwrap();
    assert_eq!(output, "opencode [project]");
    let output = launch::capture(
        Path::new("/bin/sh"),
        &[
            "-c",
            "printf '1.18.30'; printf 'private diagnostic' >&2",
            "--version",
        ],
    )
    .unwrap();
    assert_eq!(output, "1.18.30");
}
