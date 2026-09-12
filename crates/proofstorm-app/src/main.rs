use anyhow::{Context, Result, bail};
use proofstorm_app::{
    config::{DEFAULT_NAMESPACE, DEFAULT_WORKSPACE},
    lab::Labs,
};
use proofstorm_core::{
    Capability, InstancePhase, LabSpec, OperationPhase,
    native::{NativeCommand, NativeOutput, OutputMode},
};
use proofstorm_store::Store;
use std::{fmt::Write, path::PathBuf, time::Duration};
mod cli_output;
mod server_restart;

mod cli;
use cli::Action as Command;

#[allow(
    clippy::too_many_lines,
    reason = "CLI command dispatch keeps argument-to-application mappings together"
)]
#[tokio::main]
async fn main() -> Result<()> {
    let (args, command) = cli::parse();
    let (output_kind, label) = match &command {
        Command::Setup { .. } => ("setup", Some("Checking installation")),
        Command::Gui { .. } | Command::GuiStart { .. } => {
            ("gui", Some("Checking Proofstorm files"))
        }
        Command::GuiStatus => ("gui-status", None),
        Command::Stop => ("stop", Some("Stopping GUI")),
        Command::Attach { .. } => ("attach", Some("Checking project connection")),
        Command::Open { .. } => ("open", Some("Preparing coding agent")),
        Command::Doctor { .. } => ("doctor", Some("Checking Proofstorm health")),
        Command::InstallBundle { .. } => ("install", Some("Installing Proofstorm")),
        Command::Init { .. } => ("init", Some("Configuring local permissions")),
        Command::Up { preview: true, .. } => ("up", Some("Reviewing lab changes")),
        Command::Up { .. } => ("up", Some("Starting lab; checking images and readiness")),
        Command::Down { .. } => ("down", Some("Removing lab")),
        Command::Status { .. } => ("status", Some("Reading lab status")),
        Command::Environment { .. } => ("environment", Some("Reading labs")),
        Command::OpsList { .. } => ("ops-list", None),
        Command::Exec { .. } => ("exec", Some("Running lab command")),
        Command::Result { .. } => ("result", Some("Reading operation result")),
        Command::Sync { .. } => ("sync", Some("Syncing lab activity")),
        Command::Connect { .. } => ("connect", Some("Opening lab connection")),
        Command::Serve { .. } => ("serve", Some("Preparing server")),
        Command::Version { .. } | Command::CheckoutRegister { .. } | Command::GuiServe { .. } => {
            ("internal", None)
        }
    };
    let mut output = cli_output::Output::new(args.json, output_kind, label);
    // Metadata and registration must work before a coherent checkout is selected.
    if let Command::Version { verbose } = command {
        let info = proofstorm_app::release::describe();
        if args.json {
            return print(&info);
        }
        println!(
            "Proofstorm {}",
            info["version"].as_str().unwrap_or("unknown")
        );
        if verbose {
            for (label, key) in [
                ("Target", "target"),
                ("Build", "build_profile"),
                ("Revision", "source_revision"),
            ] {
                if let Some(value) = info[key].as_str() {
                    println!("{label}: {value}");
                }
            }
        }
        return Ok(());
    }
    if let Command::CheckoutRegister {
        source,
        resources,
        mcp,
        web_dist,
    } = &command
    {
        let home = args
            .home
            .as_ref()
            .context("checkout registration requires --home")?;
        return print(&proofstorm_app::artifacts::register(
            home, source, resources, mcp, web_dist,
        )?);
    }
    // Stopping an owned GUI remains possible even after a checkout was rebuilt.
    if matches!(command, Command::Stop) {
        return output.show(
            &proofstorm_app::gui::stop(args.home.as_ref().context("gui stop requires --home")?)
                .await?,
        );
    }
    if matches!(command, Command::GuiStatus) {
        return output.show(
            &proofstorm_app::gui::status(args.home.as_ref().context("gui status requires --home")?)
                .await?,
        );
    }
    // GUI startup owns one verified snapshot in each process. Do not repeat its
    // verification here and again while locating resources or checking runtime.
    if !matches!(
        command,
        Command::InstallBundle { .. }
            | Command::Gui { .. }
            | Command::GuiStart { .. }
            | Command::GuiServe { .. }
    ) {
        if let Some(home) = &args.home {
            proofstorm_app::artifacts::check_checkout(home)?;
        }
    }
    if matches!(
        command,
        Command::Gui { .. } | Command::GuiStart { .. } | Command::GuiServe { .. } | Command::Stop
    ) {
        let home = args
            .home
            .as_ref()
            .context("GUI requires an installed --home; run setup first")?;
        anyhow::ensure!(
            args.context.is_none()
                && args.kubeconfig.is_none()
                && args.database.is_none()
                && args.workspace == DEFAULT_WORKSPACE
                && args.principal == "developer"
                && args.namespace == DEFAULT_NAMESPACE,
            "the managed GUI uses only the installation's private runtime; remove overrides"
        );
        return match &command {
            Command::GuiStart { allow_development } => output.show(
                &proofstorm_app::gui::start_service(home, *allow_development, &|label| {
                    output.update(label);
                })
                .await?,
            ),
            Command::Gui {
                project,
                allow_development,
                no_open,
            } => output.show(
                &proofstorm_app::gui::open(home, project, *allow_development, *no_open, &|label| {
                    output.update(label);
                })
                .await?,
            ),
            Command::Stop => output.show(&proofstorm_app::gui::stop(home).await?),
            Command::GuiServe {
                instance,
                allow_development,
            } => proofstorm_app::gui::serve(home, instance, *allow_development).await,
            _ => unreachable!(),
        };
    }
    if let Command::Attach {
        harness,
        project,
        dry_run,
        allow_development,
        ..
    }
    | Command::Open {
        harness,
        project,
        dry_run,
        allow_development,
        ..
    } = &command
    {
        let home = args
            .home
            .as_ref()
            .context("attachment requires an installed --home; run setup first")?;
        anyhow::ensure!(
            args.context.is_none()
                && args.kubeconfig.is_none()
                && args.database.is_none()
                && args.workspace == DEFAULT_WORKSPACE
                && args.principal == "developer"
                && args.namespace == DEFAULT_NAMESPACE,
            "attachment uses only this installation's private runtime and default workspace; remove overrides"
        );
        let bundle = proofstorm_app::artifacts::root(home)?;
        let plan = proofstorm_app::harness::plan_for(
            *harness,
            home,
            project,
            &bundle,
            *allow_development,
        )?;
        let launch = if let Command::Open { gui, .. } = &command {
            let launch =
                proofstorm_app::harness::launch::detect_for(*harness, &plan.project, !*gui)?;
            if launch.interface == "cli" && !dry_run {
                proofstorm_app::harness::launch::require_terminal()?;
            }
            Some(launch)
        } else {
            None
        };
        if *dry_run {
            return output.show(
                &serde_json::json!({"attachment":plan,"launch":launch,"changes_applied":false}),
            );
        }
        let attached = proofstorm_app::harness::apply(plan).await?;
        output.show(&attached)?;
        if let Some(launch) = launch {
            proofstorm_app::harness::launch::run(&launch)?;
        }
        return Ok(());
    }
    if matches!(command, Command::Setup { .. } | Command::Doctor { .. }) {
        let home = args.home.as_ref().context(
            "installed setup/doctor requires --home (installed launchers supply a private default)",
        )?;
        anyhow::ensure!(home.is_absolute(), "--home must be absolute");
        anyhow::ensure!(
            args.context.is_none() && args.kubeconfig.is_none() && args.database.is_none(),
            "setup/doctor refuses context, kubeconfig, and database overrides; use only the installation home"
        );
        anyhow::ensure!(
            args.workspace == DEFAULT_WORKSPACE
                && args.principal == "developer"
                && args.namespace == DEFAULT_NAMESPACE,
            "installed setup/doctor currently requires the default developer, workspace, and namespace"
        );
        if let Command::Doctor {} = command {
            let report = proofstorm_app::bootstrap::doctor(home);
            output.stop();
            if args.json {
                print(&report)?;
            } else {
                for check in report["checks"]
                    .as_array()
                    .context("doctor checks missing")?
                {
                    println!(
                        "{}: {}{}",
                        check["name"].as_str().unwrap_or("check"),
                        if check["ok"] == true {
                            "OK"
                        } else {
                            "NOT READY"
                        },
                        check["message"]
                            .as_str()
                            .map_or_else(String::new, |message| format!(" — {message}"))
                    );
                }
                println!("Agent connections: not checked.");
            }
            anyhow::ensure!(
                report["ok"] == true,
                "installation is not ready; see doctor checks"
            );
            return Ok(());
        }
        if let Command::Setup {
            allow_development,
            prepare_only,
            prefetch_all,
        } = command
        {
            let bundle = proofstorm_app::artifacts::root(home)?;
            let result = proofstorm_app::bootstrap::setup_with_progress(
                home,
                &bundle,
                allow_development,
                prepare_only,
                prefetch_all,
                &|label| output.update(label),
            )?;
            return output.show(&result);
        }
    }
    if let Command::InstallBundle {
        bundle,
        prefix,
        allow_development,
    } = &command
    {
        return output.show(&proofstorm_app::installer::install(
            bundle,
            prefix,
            *allow_development,
        )?);
    }
    if let (
        Some(home),
        Command::Init {
            api_port,
            registry_port,
        },
    ) = (&args.home, &command)
    {
        anyhow::ensure!(!home.as_os_str().is_empty(), "--home must not be empty");
        proofstorm_app::installation::Installation::initialize(home, *api_port, *registry_port)?;
    }
    let environment = proofstorm_app::config::Environment::resolve(
        |key| match key {
            "PROOFSTORM_HOME" => args
                .home
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            "PROOFSTORM_KUBECONFIG" => args
                .kubeconfig
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            "PROOFSTORM_DB" => args
                .database
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            "PROOFSTORM_WORKSPACE" => Some(args.workspace.clone()),
            "PROOFSTORM_PRINCIPAL" => Some(args.principal.clone()),
            "PROOFSTORM_CONTEXT" => args.context.clone(),
            "PROOFSTORM_CONTROL_NAMESPACE" => Some(args.namespace.clone()),
            _ => None,
        },
        &std::env::current_dir()?,
    )?;
    if args.json {
        environment.report();
    }
    if let Some(parent) = environment
        .database
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let store = Store::open(&environment.database)?;
    if matches!(command, Command::Init { .. }) {
        proofstorm_app::developer::configure(&store, &args.workspace, &args.principal)?;
        return output.show(
            &serde_json::json!({"database":environment.database,"workspace":args.workspace,"principal":args.principal,"context":environment.context,"installation":environment.installation,"kubeconfig":environment.kubeconfig,"capabilities":store.capabilities(&args.workspace,&args.principal)?}),
        );
    }
    if matches!(command, Command::Result { .. }) {
        if let Command::Result { id } = command {
            return output.show(&store.operation(&args.workspace, &args.principal, &id)?);
        }
    }
    if let Command::OpsList {
        name,
        cursor,
        limit,
    } = &command
    {
        let lab = store.resolve_lab(&args.workspace, &args.principal, name)?;
        let (items, next_cursor) = store.instance_activity(
            &args.workspace,
            &args.principal,
            &lab.instance_id,
            cursor,
            *limit,
        )?;
        let items: Vec<proofstorm_view::Activity> = items.into_iter().map(Into::into).collect();
        return output.show(&serde_json::json!({"items":items,"next_cursor":next_cursor}));
    }
    store
        .authorize(&args.workspace, &args.principal, Capability::LabStatus)
        .with_context(|| {
            format!(
                "local permissions missing; run {} dev init",
                proofstorm_app::command_name()
            )
        })?;
    let runtime = environment.runtime().await?;
    let labs = Labs::new(store, runtime, args.workspace, args.principal)
        .with_installation(environment.installation.clone());
    match command {
        Command::Version { .. }
        | Command::CheckoutRegister { .. }
        | Command::Gui { .. }
        | Command::GuiStart { .. }
        | Command::GuiStatus
        | Command::OpsList { .. }
        | Command::GuiServe { .. }
        | Command::Stop
        | Command::Attach { .. }
        | Command::Open { .. }
        | Command::InstallBundle { .. }
        | Command::Setup { .. }
        | Command::Doctor { .. } => {
            unreachable!("release metadata is handled before environment resolution")
        }
        Command::Up {
            file,
            name,
            wait,
            preview,
            delete_data,
            delete_retained,
        } => {
            let spec: LabSpec = serde_json::from_slice(&std::fs::read(file)?)?;
            let name = name.as_deref().unwrap_or(&spec.name);
            if preview {
                output.show(&labs.plan_edit(name, &spec, delete_data, &delete_retained)?)?;
                return Ok(());
            }
            let mut view = if delete_data || !delete_retained.is_empty() {
                labs.edit(name, &spec, delete_data, &delete_retained)
                    .await?
            } else {
                labs.up(name, &spec).await?
            };
            let recovery = proofstorm_app::updates::start_recovery(
                labs.runtime.clone(),
                labs.store.clone(),
                labs.workspace.clone(),
                labs.principal.clone(),
            );
            let waited = if wait == 0 {
                Ok(None)
            } else {
                let instance = view.runtime.as_ref().map(|status| &status.instance);
                labs.wait(proofstorm_app::lab::WaitRequest {
                    reference: &view.lab.instance_id,
                    expected_instance_key: instance.map(|instance| instance.instance_key.as_str()),
                    expected_generation: instance.map(|instance| instance.generation),
                    target_phase: InstancePhase::Ready,
                    timeout_seconds: wait,
                })
                .await
                .map(Some)
            };
            recovery.abort();
            if let Some(waited) = waited? {
                view.runtime = Some(waited.status);
            }
            output.show(&view)?;
            if wait != 0
                && !view
                    .runtime
                    .as_ref()
                    .is_some_and(|r| r.phase == InstancePhase::Ready)
            {
                bail!(
                    "lab has not reached Ready; inspect the reported phase and blockers before retrying"
                );
            }
        }
        Command::Environment {
            instance_id,
            cursor,
            limit,
            session_cursor,
            activity_cursor,
            component_cursor,
            link_cursor,
        } => output.show(
            &labs
                .environment(&proofstorm_app::environment::EnvironmentQuery {
                    instance_id,
                    cursor,
                    limit,
                    session_cursor,
                    activity_cursor,
                    component_cursor,
                    link_cursor,
                })
                .await?,
        )?,
        Command::Serve { port, replace } => {
            if replace {
                anyhow::ensure!(
                    environment.installation.is_none(),
                    "--replace is only for the contributor server; an isolated installation must use its own free port"
                );
                server_restart::stop_previous(port).await?;
            }
            output.stop();
            proofstorm_app::http::serve(labs, port).await?;
        }
        Command::Status { name, after } => output.show(&labs.inspect(&name, after).await?)?,
        Command::Sync { name, watch } => loop {
            // Re-arm after each snapshot, but never animate during the watch interval.
            output.stop();
            output = cli_output::Output::new(args.json, "sync", Some("Syncing lab activity"));
            labs.sync(&name).await?;
            output.show(&labs.inspect(&name, 0).await?)?;
            if !watch {
                break;
            }
            tokio::select! {_=tokio::signal::ctrl_c()=>break,()=tokio::time::sleep(Duration::from_secs(2))=>{}}
        },
        Command::Exec {
            name,
            component,
            request_id,
            timeout,
            public_output,
            argv,
        } => {
            let request_id = request_id.map_or_else(new_request_id, Ok)?;
            // Preserve retry identity before submission without interleaving with progress.
            output.stop();
            eprintln!("Request: {request_id}; reuse --request-id {request_id} if interrupted");
            output = cli_output::Output::new(args.json, "exec", Some("Running lab command"));
            let command = NativeCommand {
                private_io: None,
                script: String::new(),
                argv,
                timeout_seconds: timeout,
                output: NativeOutput {
                    mode: if public_output {
                        OutputMode::Public
                    } else {
                        OutputMode::Private
                    },
                    fields: Vec::new(),
                },
            };
            let mut op = labs.exec(&name, &component, command, &request_id).await?;
            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(u64::from(timeout) + 30);
            while matches!(op.phase, OperationPhase::Pending | OperationPhase::Running)
                && tokio::time::Instant::now() < deadline
            {
                labs.sync(&name).await?;
                op = labs
                    .store
                    .operation(&labs.workspace, &labs.principal, &request_id)?;
                if matches!(op.phase, OperationPhase::Pending | OperationPhase::Running) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            output.show(&op)?;
            if op.phase != OperationPhase::Succeeded
                || !op.artifact.as_ref().is_some_and(|artifact| {
                    artifact.content["exit_code"] == 0
                        && artifact.content["cleanup_verified"] == true
                        && artifact.content["timed_out"] != true
                        && artifact.content["exit_signal"].is_null()
                })
            {
                bail!("operation did not report success; inspect its receipt before any retry");
            }
        }
        Command::Down { name, wait } => output.show(
            &labs
                .down_with_progress(&name, wait, &|label| output.update(label))
                .await?,
        )?,
        Command::Connect {
            name,
            component,
            endpoint,
            port,
            config,
        } => {
            let connection = labs.connect(&name, &component, &endpoint, port).await?;
            connection.write_config(&config)?;
            let _config_guard = ConfigFile(config);
            output.show(&connection.descriptor)?;
            tokio::select! {result=connection.serve()=>result?, result=tokio::signal::ctrl_c()=>result?}
        }
        Command::Init { .. } | Command::Result { .. } => {
            unreachable!("handled before connecting to runtime")
        }
    }
    Ok(())
}

struct ConfigFile(PathBuf);
impl Drop for ConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn new_request_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| anyhow::anyhow!("request identity generation failed: {e}"))?;
    let mut id = String::from("exec-");
    for byte in bytes {
        write!(id, "{byte:02x}")?;
    }
    Ok(id)
}
fn print(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
