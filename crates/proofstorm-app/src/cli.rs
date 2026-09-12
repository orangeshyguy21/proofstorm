//! Public command grammar and application dispatch.
mod action;
pub use action::Action;

use clap::{Args as ClapArgs, CommandFactory, FromArgMatches, Parser, Subcommand};
use proofstorm_app::{
    config::{DEFAULT_NAMESPACE, DEFAULT_WORKSPACE},
    harness::Harness,
};
use std::{ffi::OsString, path::PathBuf};

#[derive(ClapArgs)]
pub struct Options {
    /// Print JSON.
    #[arg(long, global = true)]
    pub json: bool,
    /// Installation directory.
    #[arg(long, global = true, env = "PROOFSTORM_HOME", hide_env_values = true)]
    pub home: Option<PathBuf>,
    /// Kubeconfig for an external runtime.
    #[arg(long, global = true, env = "PROOFSTORM_KUBECONFIG", hide = true)]
    pub kubeconfig: Option<PathBuf>,
    /// Lab state database.
    #[arg(long, global = true, env = "PROOFSTORM_DB", hide = true)]
    pub database: Option<PathBuf>,
    /// Authorization workspace.
    #[arg(long, global = true, env = "PROOFSTORM_WORKSPACE", default_value = DEFAULT_WORKSPACE, hide = true)]
    pub workspace: String,
    /// Configured local identity.
    #[arg(
        long,
        global = true,
        env = "PROOFSTORM_PRINCIPAL",
        default_value = "developer",
        hide = true
    )]
    pub principal: String,
    /// External Kubernetes context.
    #[arg(long, global = true, env = "PROOFSTORM_CONTEXT", hide = true)]
    pub context: Option<String>,
    /// Controller namespace.
    #[arg(long, global = true, env = "PROOFSTORM_CONTROL_NAMESPACE", default_value = DEFAULT_NAMESPACE, hide = true)]
    pub namespace: String,
}

#[derive(Parser)]
#[command(
    name = "proofstorm",
    version,
    disable_help_subcommand = true,
    about = "Proofstorm — local labs for Bitcoin, Lightning, and Cashu"
)]
struct Cli {
    #[command(flatten)]
    options: Options,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Prepare and start the local runtime.
    Setup(SetupArgs),
    /// Diagnose installation and runtime problems.
    Doctor,
    /// Open or manage the GUI.
    Gui(GuiArgs),
    /// Configure and launch coding agents.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Create or update a lab from a JSON specification.
    Up(UpArgs),
    /// List current labs.
    #[command(name = "ls")]
    List(ListArgs),
    /// Show lab status.
    Status {
        /// Lab name or ID.
        #[arg(value_name = "LAB")]
        name: String,
        /// Read recorded activity after this sequence number.
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
    /// Open a local connection to a lab service.
    Connect {
        /// Lab name or ID.
        #[arg(value_name = "LAB")]
        name: String,
        /// Component ID.
        component: String,
        /// Endpoint: http (mint) or rpc (Bitcoin).
        endpoint: String,
        /// Local port; 0 picks a free port.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// New private connection file; deleted on disconnect.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
    },
    /// Run a command in a component and record its outcome.
    Exec {
        /// Lab name or ID.
        #[arg(value_name = "LAB")]
        name: String,
        /// Component ID.
        component: String,
        /// Operation ID to reuse after interruption.
        #[arg(long)]
        request_id: Option<String>,
        /// Execution deadline in seconds (1–300).
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=300))]
        timeout: u32,
        /// Include stdout/stderr in the recorded result.
        #[arg(long)]
        public_output: bool,
        /// Command and arguments.
        #[arg(last = true, required = true, value_name = "COMMAND")]
        argv: Vec<String>,
    },
    /// Delete a lab, its storage, and its activity history.
    #[command(name = "rm")]
    Remove(RemoveArgs),
    /// Inspect recorded operations.
    Ops {
        #[command(subcommand)]
        command: OpsCommand,
    },
    /// Show version and build details.
    Version {
        /// Include embedded build and release details.
        #[arg(long)]
        verbose: bool,
    },
    /// Show command help.
    Help {
        /// Command path, for example: gui start.
        #[arg(value_name = "COMMAND")]
        path: Vec<String>,
    },
    /// Contributor initialization and foreground server tools.
    #[command(hide = true)]
    Dev {
        #[command(subcommand)]
        command: DevCommand,
    },
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommand,
    },
}

#[derive(ClapArgs)]
struct SetupArgs {
    /// Permit a verified local development bundle.
    #[arg(long, hide = true)]
    allow_development: bool,
    /// Prepare tools without starting the runtime.
    #[arg(long)]
    prepare_only: bool,
    /// Download all lab images now.
    #[arg(long, conflicts_with = "prepare_only")]
    prefetch_all: bool,
}

#[derive(ClapArgs)]
#[command(subcommand_precedence_over_arg = true)]
struct GuiArgs {
    #[command(subcommand)]
    command: Option<GuiCommand>,
    /// Project folder; defaults to the current directory.
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
    /// Permit a verified local development bundle.
    #[arg(long, global = true, hide = true)]
    allow_development: bool,
}

#[derive(Subcommand)]
enum GuiCommand {
    /// Open the GUI in your browser.
    Open {
        /// Project folder.
        #[arg(long, value_name = "PATH")]
        project: Option<PathBuf>,
    },
    /// Start the GUI service without a browser.
    Start,
    /// Stop the GUI service; labs keep running.
    Stop,
    /// Show GUI service status.
    Status,
}

#[derive(ClapArgs)]
struct AgentArgs {
    /// Coding agent.
    #[arg(value_enum, value_name = "AGENT")]
    harness: Harness,
    /// Project folder.
    #[arg(long, default_value = ".", value_name = "PATH")]
    project: PathBuf,
    /// Preview configuration and access changes.
    #[arg(long)]
    dry_run: bool,
    /// Permit a verified local development bundle.
    #[arg(long, hide = true)]
    allow_development: bool,
}

#[derive(Subcommand)]
enum AgentCommand {
    /// Configure tools and launch the agent.
    Open {
        #[command(flatten)]
        agent: AgentArgs,
        /// Launch the desktop app (macOS).
        #[arg(long)]
        desktop: bool,
    },
    /// Configure tools for the agent.
    Configure(AgentArgs),
}

#[derive(ClapArgs)]
struct UpArgs {
    /// JSON lab specification.
    file: PathBuf,
    /// Override the lab name.
    #[arg(long)]
    name: Option<String>,
    /// Preview an edit; saves the plan locally.
    #[arg(long)]
    preview: bool,
    /// Delete data for components removed by this edit.
    #[arg(long = "delete-removed-data")]
    delete_data: bool,
    /// Delete retained data for these component IDs.
    #[arg(
        long = "delete-retained-data",
        value_delimiter = ',',
        value_name = "COMPONENTS"
    )]
    delete_retained: Vec<String>,
    /// Seconds to wait for readiness (0–120); 0 returns after acceptance.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u32).range(0..=120))]
    wait: u32,
}

#[derive(ClapArgs)]
struct RemoveArgs {
    /// Lab to delete, including its data.
    #[arg(value_name = "LAB")]
    name: String,
    /// Seconds to wait for verified removal.
    #[arg(long, default_value_t = 120)]
    wait: u32,
}

#[derive(ClapArgs)]
struct ListArgs {
    /// Filter by runtime instance ID.
    #[arg(long)]
    instance_id: Option<String>,
    /// Next-page cursor.
    #[arg(long, default_value = "")]
    cursor: String,
    /// Maximum labs per page (1–50).
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=50))]
    limit: u32,
    /// Continue the sessions page in detailed JSON output.
    #[arg(long, default_value = "", hide = true)]
    session_cursor: String,
    /// Continue the activity page in detailed JSON output.
    #[arg(long, default_value = "", hide = true)]
    activity_cursor: String,
    /// Continue the components page in detailed JSON output.
    #[arg(long, default_value = "", hide = true)]
    component_cursor: String,
    /// Continue the links page in detailed JSON output.
    #[arg(long, default_value = "", hide = true)]
    link_cursor: String,
}

#[derive(Subcommand)]
enum OpsCommand {
    /// List recorded operations.
    Ls {
        /// Lab name or ID.
        #[arg(value_name = "LAB")]
        name: String,
        /// Next-page cursor.
        #[arg(long, default_value = "")]
        cursor: String,
        /// Maximum operations per page (1–50).
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=50))]
        limit: u32,
    },
    /// Show a recorded operation.
    Show {
        /// Operation identifier printed by exec.
        id: String,
    },
    /// Collect operation results into history.
    Sync(SyncArgs),
}

#[derive(ClapArgs)]
struct SyncArgs {
    /// Lab name or ID.
    #[arg(value_name = "LAB")]
    name: String,
    /// Keep collecting every two seconds until Ctrl-C.
    #[arg(long)]
    watch: bool,
}

#[derive(Subcommand)]
enum DevCommand {
    /// Initialize local state and permissions.
    Init(InitArgs),
    /// Run the foreground web/API server.
    Serve(ServeArgs),
}

#[derive(ClapArgs)]
struct InitArgs {
    /// Reserve this API port when creating an installation (requires --home).
    #[arg(long, requires = "home", value_parser = clap::value_parser!(u16).range(1..))]
    api_port: Option<u16>,
    /// Reserve this registry port when creating an installation (requires --home).
    #[arg(long, requires = "home", value_parser = clap::value_parser!(u16).range(1..))]
    registry_port: Option<u16>,
}

#[derive(ClapArgs)]
struct ServeArgs {
    /// Local loopback port for the foreground server.
    #[arg(long, default_value_t = 8787)]
    port: u16,
    /// Replace this checkout's existing foreground server on the selected port.
    #[arg(long)]
    replace: bool,
}

#[derive(Subcommand)]
enum InternalCommand {
    /// Register verified checkout artifacts with a development installation.
    CheckoutRegister(RegisterArgs),
    /// Install a verified local bundle into a user-owned prefix.
    InstallBundle(InstallArgs),
    /// Run the private worker for this installation's managed GUI.
    GuiServe(WorkerArgs),
}

#[derive(ClapArgs)]
struct RegisterArgs {
    #[arg(long)]
    source: PathBuf,
    #[arg(long)]
    resources: PathBuf,
    #[arg(long)]
    web_dist: PathBuf,
    #[arg(long)]
    mcp: PathBuf,
}

#[derive(ClapArgs)]
struct InstallArgs {
    /// Unpacked, verified bundle directory.
    #[arg(long)]
    bundle: PathBuf,
    /// Absolute destination prefix for binaries and managed installation files.
    #[arg(long)]
    prefix: PathBuf,
    /// Permit a verified local development bundle.
    #[arg(long)]
    allow_development: bool,
}

#[derive(ClapArgs)]
struct WorkerArgs {
    #[arg(long)]
    instance: String,
    #[arg(long)]
    allow_development: bool,
}

impl AgentArgs {
    fn action(self, desktop: Option<bool>) -> Action {
        let Self {
            harness,
            project,
            dry_run,
            allow_development,
        } = self;
        match desktop {
            Some(gui) => Action::Open {
                harness,
                project,
                dry_run,
                allow_development,
                gui,
            },
            None => Action::Attach {
                harness,
                project,
                dry_run,
                allow_development,
            },
        }
    }
}

impl Command {
    #[allow(
        clippy::too_many_lines,
        reason = "command normalization maps each public command to its application action"
    )]
    fn action(self) -> Result<Action, clap::Error> {
        Ok(match self {
            Self::Setup(SetupArgs {
                allow_development,
                prepare_only,
                prefetch_all,
            }) => Action::Setup {
                allow_development,
                prepare_only,
                prefetch_all,
            },
            Self::Doctor => Action::Doctor {},
            Self::Gui(GuiArgs {
                command,
                project,
                allow_development,
            }) => {
                if matches!(
                    command,
                    Some(GuiCommand::Start | GuiCommand::Stop | GuiCommand::Status)
                ) && project.is_some()
                {
                    return Err(clap::Error::raw(
                        clap::error::ErrorKind::ArgumentConflict,
                        "--project applies to gui open",
                    ));
                }
                match command {
                    Some(GuiCommand::Stop) => Action::Stop,
                    Some(GuiCommand::Status) => Action::GuiStatus,
                    Some(GuiCommand::Start) => Action::GuiStart { allow_development },
                    Some(GuiCommand::Open { project: selected }) => {
                        if project.is_some() && selected.is_some() {
                            return Err(clap::Error::raw(
                                clap::error::ErrorKind::ArgumentConflict,
                                "pass --project once",
                            ));
                        }
                        Action::Gui {
                            project: selected.or(project).unwrap_or_else(|| PathBuf::from(".")),
                            allow_development,
                            no_open: false,
                        }
                    }
                    None => Action::Gui {
                        project: project.unwrap_or_else(|| PathBuf::from(".")),
                        allow_development,
                        no_open: false,
                    },
                }
            }
            Self::Agent {
                command: AgentCommand::Open { agent, desktop },
            } => agent.action(Some(desktop)),
            Self::Agent {
                command: AgentCommand::Configure(agent),
            } => agent.action(None),
            Self::Up(UpArgs {
                file,
                name,
                preview,
                delete_data,
                delete_retained,
                wait,
            }) => Action::Up {
                file,
                name,
                preview,
                delete_data,
                delete_retained,
                wait,
            },
            Self::Remove(RemoveArgs { name, wait }) => Action::Down { name, wait },
            Self::Status { name, after } => Action::Status { name, after },
            Self::List(ListArgs {
                instance_id,
                cursor,
                limit,
                session_cursor,
                activity_cursor,
                component_cursor,
                link_cursor,
            }) => Action::Environment {
                instance_id,
                cursor,
                limit,
                session_cursor,
                activity_cursor,
                component_cursor,
                link_cursor,
            },
            Self::Connect {
                name,
                component,
                endpoint,
                port,
                config,
            } => Action::Connect {
                name,
                component,
                endpoint,
                port,
                config,
            },
            Self::Exec {
                name,
                component,
                request_id,
                timeout,
                public_output,
                argv,
            } => Action::Exec {
                name,
                component,
                request_id,
                timeout,
                public_output,
                argv,
            },
            Self::Ops {
                command:
                    OpsCommand::Ls {
                        name,
                        cursor,
                        limit,
                    },
            } => Action::OpsList {
                name,
                cursor,
                limit,
            },
            Self::Ops {
                command: OpsCommand::Show { id },
            } => Action::Result { id },
            Self::Ops {
                command: OpsCommand::Sync(SyncArgs { name, watch }),
            } => Action::Sync { name, watch },
            Self::Dev {
                command:
                    DevCommand::Init(InitArgs {
                        api_port,
                        registry_port,
                    }),
            } => Action::Init {
                api_port,
                registry_port,
            },
            Self::Dev {
                command: DevCommand::Serve(ServeArgs { port, replace }),
            } => Action::Serve { port, replace },
            Self::Version { verbose } => Action::Version { verbose },
            Self::Internal {
                command:
                    InternalCommand::CheckoutRegister(RegisterArgs {
                        source,
                        resources,
                        web_dist,
                        mcp,
                    }),
            } => Action::CheckoutRegister {
                source,
                resources,
                web_dist,
                mcp,
            },
            Self::Internal {
                command:
                    InternalCommand::InstallBundle(InstallArgs {
                        bundle,
                        prefix,
                        allow_development,
                    }),
            } => Action::InstallBundle {
                bundle,
                prefix,
                allow_development,
            },
            Self::Internal {
                command:
                    InternalCommand::GuiServe(WorkerArgs {
                        instance,
                        allow_development,
                    }),
            } => Action::GuiServe {
                instance,
                allow_development,
            },
            Self::Help { .. } => unreachable!("help is handled before action normalization"),
        })
    }
}

fn help_tree(program: &'static str) -> clap::Command {
    let examples = format!(
        "Examples:\n  {program} setup\n  {program} gui\n  {program} up lab.json --name demo\n  {program} status demo\n\nDetails: {program} <command> --help\nAdvanced: {program} help advanced"
    );
    Cli::command().bin_name(program).after_help(examples)
}

fn help(mut command: clap::Command, path: &[String], program: &str) -> clap::Error {
    let advanced = path.first().is_some_and(|part| part == "advanced");
    let path = if advanced { &path[1..] } else { path };
    // Build first so inherited globals are available when rendering a nested command.
    command.build();
    for name in path {
        let Some(child) = command
            .get_subcommands()
            .find(|child| {
                child.get_name() == name || child.get_all_aliases().any(|alias| alias == name)
            })
            .cloned()
        else {
            return clap::Error::raw(
                clap::error::ErrorKind::InvalidSubcommand,
                format!("unknown command {name:?}; run {program} help"),
            );
        };
        command = child;
    }
    if advanced {
        let ids: Vec<_> = command
            .get_arguments()
            .filter(|arg| arg.is_hide_set())
            .map(|arg| arg.get_id().clone())
            .collect();
        for id in ids {
            command = command.mut_arg(id, |arg| arg.hide(false).hide_env_values(true));
        }
        if path.is_empty() {
            command = command.mut_subcommand("dev", |c| c.hide(false))
                .after_help(format!("External overrides require an external runtime.\n\nContributor tools: {program} dev --help\nAll command flags: {program} help advanced <command>\n\nManual install (use the bundle’s executable):\n  {program} internal install-bundle --bundle PATH --prefix PATH"));
        }
    }
    // Let clap render a successful help response without reparsing the selected command.
    clap::Command::new(program.to_owned())
        .override_help(command.render_help())
        .try_get_matches_from([program, "--help"])
        .expect_err("the help flag returns a display error")
}

fn try_parse_from(
    input: impl IntoIterator<Item = OsString>,
    program: &'static str,
) -> Result<(Options, Action), clap::Error> {
    let mut tree = help_tree(program);
    let matches = tree.try_get_matches_from_mut(input)?;
    let cli = Cli::from_arg_matches(&matches)?;
    match cli.command {
        None => Err(help(tree, &[], program)),
        Some(Command::Help { path }) => Err(help(tree, &path, program)),
        Some(command) => Ok((cli.options, command.action()?)),
    }
}

pub fn parse() -> (Options, Action) {
    try_parse_from(std::env::args_os(), proofstorm_app::command_name())
        .unwrap_or_else(|error| error.exit())
}

#[cfg(test)]
mod tests;
