use super::*;

fn parse(args: &[&str]) -> Result<Action, clap::Error> {
    let input = std::iter::once("storm")
        .chain(args.iter().copied())
        .map(OsString::from);
    try_parse_from(input, "storm").map(|(_, action)| action)
}

#[test]
fn command_tree_is_valid() {
    help_tree("storm").debug_assert();
}

#[test]
fn runtime_retirement_is_explicit_and_not_a_public_cell_command() {
    assert!(parse(&["internal", "runtime-delete"]).is_err());
    assert!(matches!(
        parse(&[
            "internal",
            "runtime-delete",
            "--installation-id",
            &"a".repeat(32)
        ])
        .unwrap(),
        Action::RuntimeDelete { .. }
    ));
    assert!(parse(&["runtime-delete", "--help"]).is_err());
    assert!(
        !parse(&["--help"])
            .unwrap_err()
            .to_string()
            .contains("runtime-delete")
    );
}

#[test]
fn gui_has_explicit_lifecycle_and_project_scope() {
    assert!(
        matches!(parse(&["gui"]).unwrap(), Action::Gui { project, no_open: false, .. } if project == PathBuf::from("."))
    );
    for args in [
        vec!["gui", "open", "--project", "/a project"],
        vec!["gui", "--project", "/a project", "open"],
    ] {
        assert!(
            matches!(parse(&args).unwrap(), Action::Gui { project, .. } if project == PathBuf::from("/a project"))
        );
    }
    assert!(matches!(
        parse(&["gui", "start"]).unwrap(),
        Action::GuiStart { .. }
    ));
    assert!(matches!(parse(&["gui", "stop"]).unwrap(), Action::Stop));
    assert!(matches!(
        parse(&["gui", "status"]).unwrap(),
        Action::GuiStatus
    ));
    for action in ["start", "stop", "status"] {
        assert!(parse(&["gui", action, "--project", "/unused"]).is_err());
    }
    assert!(parse(&["gui", "/a project"]).is_err());
    assert!(parse(&["gui", "--no-open"]).is_err());
}

#[test]
fn agent_configuration_and_launch_are_distinct() {
    for agent in ["codex", "opencode", "claude"] {
        assert!(matches!(
            parse(&["agent", "configure", agent, "--dry-run"]).unwrap(),
            Action::Attach {
                dry_run: true,
                replace: false,
                ..
            }
        ));
        assert!(matches!(
            parse(&["agent", "open", agent]).unwrap(),
            Action::Open {
                gui: false,
                replace: false,
                ..
            }
        ));
        assert!(matches!(
            parse(&["agent", "configure", agent, "--replace", "--dry-run"]).unwrap(),
            Action::Attach {
                replace: true,
                dry_run: true,
                ..
            }
        ));
        assert!(matches!(
            parse(&["agent", "open", agent, "--replace", "--dry-run"]).unwrap(),
            Action::Open {
                replace: true,
                dry_run: true,
                gui: false,
                ..
            }
        ));
        assert!(
            matches!(parse(&["agent", "open", agent, "--desktop", "--project", "/a project"]).unwrap(), Action::Open { gui: true, project, .. } if project == PathBuf::from("/a project"))
        );
        assert!(parse(&["agent", "open", agent, "--gui"]).is_err());
    }
}

#[test]
fn removed_command_names_are_rejected() {
    for args in [
        vec!["dev", "serve"],
        vec!["dev", "serve", "--replace"],
        vec!["dev", "serve", "--help"],
    ] {
        assert!(
            parse(&args).is_err_and(|error| error.kind() != clap::error::ErrorKind::DisplayHelp)
        );
    }
    for old in [
        "stop",
        "attach",
        "open",
        "init",
        "serve",
        "sync",
        "result",
        "release-info",
        "environment",
        "down",
        "checkout-register",
        "install-bundle",
        "gui-serve",
    ] {
        assert!(
            parse(&[old, "--help"])
                .is_err_and(|error| error.kind() != clap::error::ErrorKind::DisplayHelp),
            "{old}"
        );
    }
    assert!(matches!(parse(&["rm", "demo"]).unwrap(), Action::Down { name, .. } if name == "demo"));
    assert!(matches!(
        parse(&["ls"]).unwrap(),
        Action::Environment { .. }
    ));
    assert!(
        matches!(parse(&["ops", "show", "exec-1"]).unwrap(), Action::Result { id } if id == "exec-1")
    );
    assert!(matches!(
        parse(&["ops", "sync", "demo", "--watch"]).unwrap(),
        Action::Sync { watch: true, .. }
    ));
}

#[test]
fn global_options_work_at_each_depth() {
    for args in [
        vec![
            "storm",
            "--json",
            "--home",
            "/installation",
            "agent",
            "configure",
            "codex",
        ],
        vec![
            "storm",
            "agent",
            "--json",
            "configure",
            "codex",
            "--home",
            "/installation",
        ],
    ] {
        let (options, _) = try_parse_from(args.into_iter().map(OsString::from), "storm").unwrap();
        assert!(options.json);
        assert_eq!(options.home, Some(PathBuf::from("/installation")));
    }
}

#[test]
fn help_is_concise_and_advanced_options_are_discoverable() {
    let root = parse(&[]).err().unwrap();
    assert_eq!(root.kind(), clap::error::ErrorKind::DisplayHelp);
    let text = root.to_string();
    assert!(!text.starts_with("error:"));
    assert_eq!(text, parse(&["--help"]).err().unwrap().to_string());
    assert_eq!(text, parse(&["help"]).err().unwrap().to_string());
    assert!(text.contains("storm <command> --help"));
    for hidden in [
        "checkout-register",
        "install-bundle",
        "--database",
        "--principal",
    ] {
        assert!(!text.contains(hidden), "{hidden}");
    }
    let advanced = parse(&["help", "advanced"]).err().unwrap().to_string();
    assert!(
        advanced.contains("--database") && advanced.contains("--principal"),
        "{advanced}"
    );
    let nested = parse(&["help", "gui", "stop"]).err().unwrap().to_string();
    assert!(nested.contains("storm gui stop") && nested.contains("cells keep running"));
    assert_eq!(
        nested,
        parse(&["gui", "stop", "--help"]).err().unwrap().to_string()
    );
}
