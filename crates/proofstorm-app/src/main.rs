use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
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
mod server_restart;

#[derive(Parser)]
#[command(
    name = "proofstorm",
    version,
    about = "Start protocol labs, connect your app, and inspect what happened"
)]
struct Args {
    /// Isolated installation home. Initialize with `--home PATH init` first.
    #[arg(long, global = true, env = "PROOFSTORM_HOME")]
    home: Option<PathBuf>,
    /// Read only this kubeconfig; never merge or change the user's current context.
    #[arg(long, global = true, env = "PROOFSTORM_KUBECONFIG")]
    kubeconfig: Option<PathBuf>,
    #[arg(long, global = true, env = "PROOFSTORM_DB")]
    database: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        env = "PROOFSTORM_WORKSPACE",
        default_value = DEFAULT_WORKSPACE
    )]
    workspace: String,
    #[arg(
        long,
        global = true,
        env = "PROOFSTORM_PRINCIPAL",
        default_value = "developer"
    )]
    principal: String,
    #[arg(long, global = true, env = "PROOFSTORM_CONTEXT")]
    context: Option<String>,
    #[arg(
        long,
        global = true,
        env = "PROOFSTORM_CONTROL_NAMESPACE",
        default_value = DEFAULT_NAMESPACE
    )]
    namespace: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open this installation's GUI in the default browser. Does not attach tools.
    Gui {
        /// Prefill this project folder; defaults to the caller's current directory.
        #[arg(default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        allow_development: bool,
        /// Start/reuse the GUI without opening a browser (for diagnostics).
        #[arg(long)]
        no_open: bool,
    },
    /// Stop only this installation's managed GUI. Labs keep running.
    Stop,
    #[command(hide = true)]
    GuiServe {
        #[arg(long)]
        instance: String,
        #[arg(long)]
        allow_development: bool,
    },
    /// Configure project-scoped MCP and verify its server without opening an app.
    Attach {
        #[arg(value_enum)]
        harness: Harness,
        /// Project directory to connect; defaults to the current directory.
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Show the proposed managed entry without changing files or permissions.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        allow_development: bool,
    },
    /// Attach, then open Codex's native app or OpenCode/Claude Code in this terminal.
    Open {
        #[arg(value_enum)]
        harness: Harness,
        /// Project directory to connect; defaults to the current directory.
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Use Codex's CLI instead of its native app; other agents always use the terminal.
        #[arg(long)]
        cli: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        allow_development: bool,
    },
    /// Prepare and start this installed bundle's isolated runtime (never the dev cluster).
    Setup {
        /// Explicit opt-in for a local development bundle.
        #[arg(long)]
        allow_development: bool,
        /// Download verified tools and initialize private identity without starting a runtime.
        #[arg(long)]
        prepare_only: bool,
        /// Download the entire catalog now instead of only a lab's images on first use.
        #[arg(long, conflicts_with = "prepare_only")]
        prefetch_all: bool,
    },
    /// Read-only installation/runtime checks. Does not assert harness discovery.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Report embedded release contents without reading state or contacting a cluster.
    ReleaseInfo,
    /// Install a verified release bundle into a user-owned prefix (no runtime setup).
    InstallBundle {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        prefix: PathBuf,
        #[arg(long)]
        allow_development: bool,
    },
    /// Explicitly configure this local developer's permissions (no cluster changes).
    Init {
        /// Reserve a port choice in a new installation (requires --home).
        #[arg(long, requires = "home", value_parser = clap::value_parser!(u16).range(1..))]
        api_port: Option<u16>,
        /// Reserve a registry port choice in a new installation (requires --home).
        #[arg(long, requires = "home", value_parser = clap::value_parser!(u16).range(1..))]
        registry_port: Option<u16>,
    },
    /// Create or update a lab from JSON, preserving unchanged components.
    Up {
        /// Preview a live edit without applying it.
        #[arg(long)]
        preview: bool,
        /// Explicitly delete storage and credentials of removed components.
        #[arg(long)]
        delete_data: bool,
        /// Explicitly purge data from components removed by earlier edits.
        #[arg(long, value_delimiter = ',')]
        delete_retained: Vec<String>,
        file: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u32).range(0..=120))]
        wait: u32,
    },
    /// Read the environment and cached activity without starting jobs or recording results.
    Status {
        name: String,
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
    /// Read current cluster labs, topology, resource demand, sessions and activity as JSON.
    Environment {
        #[arg(long)]
        instance_id: Option<String>,
        #[arg(long, default_value = "")]
        cursor: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long, default_value = "")]
        session_cursor: String,
        #[arg(long, default_value = "")]
        activity_cursor: String,
        #[arg(long, default_value = "")]
        component_cursor: String,
        #[arg(long, default_value = "")]
        link_cursor: String,
    },
    /// Open the live web app and API on 127.0.0.1; collect runtime receipts in the background.
    Serve {
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Replace this checkout's existing server on the selected port.
        #[arg(long)]
        replace: bool,
    },
    /// Collect operation receipts; --watch continues collecting while clients disconnect.
    Sync {
        name: String,
        #[arg(long)]
        watch: bool,
    },
    /// Run a bounded native command. Stdout/stderr stay private unless --public-output is set.
    Exec {
        name: String,
        component: String,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long, default_value_t = 30)]
        timeout: u32,
        #[arg(long)]
        public_output: bool,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Read a recorded operation and its bounded artifact, without a runtime action.
    Result { id: String },
    /// Revoke admission, cancel/collect owned work and verify lab teardown.
    Down {
        name: String,
        #[arg(long, default_value_t = 120)]
        wait: u32,
    },
    /// Open a loopback connection. Keep this process running while your app uses it.
    Connect {
        name: String,
        component: String,
        endpoint: String,
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// New private JSON file for application configuration. Removed on normal disconnect.
        #[arg(long)]
        config: PathBuf,
    },
}

use proofstorm_app::harness::Harness;

#[allow(
    clippy::too_many_lines,
    reason = "CLI command dispatch keeps argument-to-application mappings together"
)]
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if matches!(
        args.command,
        Command::Gui { .. } | Command::GuiServe { .. } | Command::Stop
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
        let executable = std::env::current_exe()?.canonicalize()?;
        let bundle = executable
            .parent()
            .and_then(std::path::Path::parent)
            .context("installed bundle not found")?;
        return match &args.command {
            Command::Gui {
                project,
                allow_development,
                no_open,
            } => print(
                &proofstorm_app::gui::open(home, project, bundle, *allow_development, *no_open)
                    .await?,
            ),
            Command::Stop => print(&proofstorm_app::gui::stop(home).await?),
            Command::GuiServe {
                instance,
                allow_development,
            } => proofstorm_app::gui::serve(home, bundle, instance, *allow_development).await,
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
    } = &args.command
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
        let executable = std::env::current_exe()?.canonicalize()?;
        let bundle = executable
            .parent()
            .and_then(std::path::Path::parent)
            .context("installed bundle not found")?;
        let plan = proofstorm_app::harness::plan_for(*harness, home, project, bundle, *allow_development)?;
        let launch = if let Command::Open { cli, .. } = &args.command {
            let launch = proofstorm_app::harness::launch::detect_for(*harness, &plan.project, *cli)?;
            if launch.interface == "cli" && !dry_run {
                proofstorm_app::harness::launch::require_terminal()?;
            }
            Some(launch)
        } else {
            None
        };
        if *dry_run {
            return print(
                &serde_json::json!({"attachment":plan,"launch":launch,"changes_applied":false}),
            );
        }
        let attached = proofstorm_app::harness::apply(plan).await?;
        print(&attached)?;
        if let Some(launch) = launch {
            proofstorm_app::harness::launch::run(&launch)?;
        }
        return Ok(());
    }
    if matches!(args.command, Command::Setup { .. } | Command::Doctor { .. }) {
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
        if let Command::Doctor { json } = args.command {
            let report = proofstorm_app::bootstrap::doctor(home);
            if json {
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
                println!("MCP server and harness discovery: not checked.");
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
        } = args.command
        {
            let executable = std::env::current_exe()?.canonicalize()?;
            let bundle = executable
                .parent()
                .and_then(std::path::Path::parent)
                .context("cannot locate installed bundle")?;
            return print(&proofstorm_app::bootstrap::setup(
                home,
                bundle,
                allow_development,
                prepare_only,
                prefetch_all,
            )?);
        }
    }
    if matches!(args.command, Command::ReleaseInfo) {
        return print(&proofstorm_app::release::describe());
    }
    if let Command::InstallBundle {
        bundle,
        prefix,
        allow_development,
    } = &args.command
    {
        return print(&proofstorm_app::installer::install(
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
    ) = (&args.home, &args.command)
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
    environment.report();
    if let Some(parent) = environment
        .database
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let store = Store::open(&environment.database)?;
    if matches!(args.command, Command::Init { .. }) {
        proofstorm_app::developer::configure(&store, &args.workspace, &args.principal)?;
        return print(
            &serde_json::json!({"database":environment.database,"workspace":args.workspace,"principal":args.principal,"context":environment.context,"installation":environment.installation,"kubeconfig":environment.kubeconfig,"capabilities":store.capabilities(&args.workspace,&args.principal)?}),
        );
    }
    if matches!(args.command, Command::Result { .. }) {
        if let Command::Result { id } = args.command {
            return print(&store.operation(&args.workspace, &args.principal, &id)?);
        }
    }
    store.authorize(&args.workspace,&args.principal,Capability::LabStatus).context("developer is not configured; run proofstorm init explicitly to configure local permissions")?;
    let runtime = environment.runtime().await?;
    let labs = Labs::new(store, runtime, args.workspace, args.principal)
        .with_installation(environment.installation.clone());
    match args.command {
        Command::ReleaseInfo
        | Command::Gui { .. }
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
                print(&labs.plan_edit(name, &spec, delete_data, &delete_retained)?)?;
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
            print(&view)?;
            if !view
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
        } => print(
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
            proofstorm_app::http::serve(labs, port).await?;
        }
        Command::Status { name, after } => print(&labs.inspect(&name, after).await?)?,
        Command::Sync { name, watch } => loop {
            labs.sync(&name).await?;
            print(&labs.inspect(&name, 0).await?)?;
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
            eprintln!(
                "request_id={request_id}; reuse --request-id {request_id} if submission is interrupted"
            );
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
            print(&op)?;
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
        Command::Down { name, wait } => print(&labs.down(&name, wait).await?)?,
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
            print(&connection.descriptor)?;
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

#[cfg(test)]
mod attachment_args_tests {
    use super::*;

    #[test]
    fn gui_defaults_to_current_directory_and_default_browser() {
        let args = Args::try_parse_from(["proofstorm", "gui"]).unwrap();
        assert!(
            matches!(args.command,Command::Gui {project,no_open:false,..} if project==PathBuf::from("."))
        );
        let args = Args::try_parse_from(["proofstorm", "gui", "/a project", "--no-open"]).unwrap();
        assert!(
            matches!(args.command,Command::Gui {project,no_open:true,..} if project==PathBuf::from("/a project"))
        );
        assert!(matches!(
            Args::try_parse_from(["proofstorm", "stop"])
                .unwrap()
                .command,
            Command::Stop
        ));
        assert!(Args::try_parse_from(["proofstorm", "ui"]).is_err());
    }

    #[test]
    fn attachment_defaults_to_current_directory_and_accepts_explicit_paths() {
        for action in ["open", "attach"] {
            for path in [None, Some("/a project/with spaces")] {
                let mut input = vec!["proofstorm", action, "codex"];
                if let Some(path) = path {
                    input.push(path);
                }
                let args = Args::try_parse_from(input).unwrap();
                let (Command::Open { project, .. } | Command::Attach { project, .. }) =
                    args.command
                else {
                    panic!("expected attachment command");
                };
                assert_eq!(project, PathBuf::from(path.unwrap_or(".")));
            }
        }
    }

    #[test]
    fn current_directory_open_accepts_cli_and_dry_run_flags() {
        let args =
            Args::try_parse_from(["proofstorm", "open", "codex", "--cli", "--dry-run"]).unwrap();
        assert!(
            matches!(args.command, Command::Open { project, cli: true, dry_run: true, .. } if project == PathBuf::from("."))
        );
    }
}
