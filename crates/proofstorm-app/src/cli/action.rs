//! Normalized application actions.
use proofstorm_app::harness::Harness;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Action {
    CheckoutRegister {
        source: PathBuf,
        resources: PathBuf,
        web_dist: PathBuf,
        mcp: PathBuf,
    },
    Gui {
        project: PathBuf,
        allow_development: bool,
        no_open: bool,
    },
    Stop,
    GuiServe {
        instance: String,
        allow_development: bool,
    },
    Attach {
        harness: Harness,
        project: PathBuf,
        dry_run: bool,
        allow_development: bool,
    },
    Open {
        harness: Harness,
        project: PathBuf,
        gui: bool,
        dry_run: bool,
        allow_development: bool,
    },
    Setup {
        allow_development: bool,
        prepare_only: bool,
        prefetch_all: bool,
    },
    Doctor {},
    InstallBundle {
        bundle: PathBuf,
        prefix: PathBuf,
        allow_development: bool,
    },
    Init {
        api_port: Option<u16>,
        registry_port: Option<u16>,
    },
    Up {
        preview: bool,
        delete_data: bool,
        delete_retained: Vec<String>,
        file: PathBuf,
        name: Option<String>,
        wait: u32,
    },
    Status {
        name: String,
        after: u64,
    },
    Environment {
        instance_id: Option<String>,
        cursor: String,
        limit: u32,
        session_cursor: String,
        activity_cursor: String,
        component_cursor: String,
        link_cursor: String,
    },
    Serve {
        port: u16,
        replace: bool,
    },
    Sync {
        name: String,
        watch: bool,
    },
    Exec {
        name: String,
        component: String,
        request_id: Option<String>,
        timeout: u32,
        public_output: bool,
        argv: Vec<String>,
    },
    Result {
        id: String,
    },
    Down {
        name: String,
        wait: u32,
    },
    Connect {
        name: String,
        component: String,
        endpoint: String,
        port: u16,
        config: PathBuf,
    },
    GuiStart {
        allow_development: bool,
    },
    GuiStatus,
    Version {
        verbose: bool,
    },
    OpsList {
        name: String,
        cursor: String,
        limit: u32,
    },
}
