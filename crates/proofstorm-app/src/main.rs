use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use proofstorm_app::{
    config::{DEFAULT_CONTEXT, DEFAULT_DATABASE, DEFAULT_NAMESPACE, DEFAULT_WORKSPACE},
    lab::Labs,
};
use proofstorm_core::{
    Capability, InstancePhase, LabSpec, OperationPhase,
    native::{NativeCommand, NativeOutput, OutputMode},
};
use proofstorm_store::{Store, Workspace};
use std::{fmt::Write, path::PathBuf, time::Duration};
mod server_restart;

#[derive(Parser)]
#[command(
    name = "proofstorm",
    about = "Start protocol labs, connect your app, and inspect what happened"
)]
struct Args {
    #[arg(
        long,
        global = true,
        env = "PROOFSTORM_DB",
        default_value = DEFAULT_DATABASE
    )]
    database: PathBuf,
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
    #[arg(
        long,
        global = true,
        env = "PROOFSTORM_CONTEXT",
        default_value = DEFAULT_CONTEXT
    )]
    context: String,
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
    /// Explicitly configure this local developer's permissions (no cluster changes).
    Init,
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
        #[arg(long, default_value_t = 120)]
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

#[allow(
    clippy::too_many_lines,
    reason = "CLI command dispatch keeps argument-to-application mappings together"
)]
#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();
    let environment = proofstorm_app::config::Environment::resolve(
        |key| match key {
            "PROOFSTORM_DB" => Some(args.database.to_string_lossy().into_owned()),
            "PROOFSTORM_WORKSPACE" => Some(args.workspace.clone()),
            "PROOFSTORM_PRINCIPAL" => Some(args.principal.clone()),
            "PROOFSTORM_CONTEXT" => Some(args.context.clone()),
            "PROOFSTORM_CONTROL_NAMESPACE" => Some(args.namespace.clone()),
            _ => None,
        },
        &std::env::current_dir()?,
    )?;
    args.database.clone_from(&environment.database);
    environment.report();
    if let Some(parent) = args.database.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let store = Store::open(&args.database)?;
    if matches!(args.command, Command::Init) {
        store.put_workspace(&Workspace {
            id: args.workspace.clone(),
            name: args.workspace.clone(),
        })?;
        store.put_principal(&args.principal)?;
        store.replace_grants(
            &args.workspace,
            &args.principal,
            [
                Capability::CatalogRead,
                Capability::LabCreate,
                Capability::LabEdit,
                Capability::LabRead,
                Capability::LabPublish,
                Capability::LabMaterialize,
                Capability::LabStatus,
                Capability::LabClose,
                Capability::LabConnect,
                Capability::ExperimentCreate,
                Capability::ExperimentRead,
                Capability::ExperimentClose,
                Capability::LabOperate,
                Capability::ExperimentRead,
                Capability::ComponentExecLive,
                Capability::ArtifactRead,
                Capability::ActionCancel,
            ],
        )?;
        return print(
            &serde_json::json!({"database":args.database,"workspace":args.workspace,"principal":args.principal,"context":args.context,"capabilities":store.capabilities(&args.workspace,&args.principal)?}),
        );
    }
    if matches!(args.command, Command::Result { .. }) {
        if let Command::Result { id } = args.command {
            return print(&store.operation(&args.workspace, &args.principal, &id)?);
        }
    }
    store.authorize(&args.workspace,&args.principal,Capability::LabStatus).context("developer is not configured; run proofstorm init explicitly to configure local permissions")?;
    let runtime = environment.runtime().await?;
    let labs = Labs::new(store, runtime, args.workspace, args.principal);
    match args.command {
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
            let deadline = tokio::time::Instant::now() + Duration::from_secs(u64::from(wait));
            while !view
                .runtime
                .as_ref()
                .is_some_and(|r| r.phase == InstancePhase::Ready)
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(500)).await;
                view = labs.inspect(name, 0).await?;
            }
            recovery.abort();
            if let Some(status) = view
                .runtime
                .as_ref()
                .filter(|s| s.phase == InstancePhase::Ready)
            {
                labs.store.mark_update_applied(
                    &labs.workspace,
                    &status.instance.id,
                    status.instance.generation,
                    Some(&status.instance.revision_digest),
                )?;
            }
            print(&view)?;
            if !view
                .runtime
                .as_ref()
                .is_some_and(|r| r.phase == InstancePhase::Ready)
            {
                bail!(
                    "lab is still starting; status and recovery exec remain available; no second lab was created"
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
        Command::Init | Command::Result { .. } => {
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
