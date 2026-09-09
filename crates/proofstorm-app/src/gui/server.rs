use super::state::{self, RECORD, Record};
use crate::{
    config::{DEFAULT_WORKSPACE, Environment},
    installation::Installation,
    lab::Labs,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Notify, Semaphore};

pub(super) struct Activation {
    pub generation: u64,
    pub project: String,
    pub focused: bool,
    pub created: Instant,
}

pub(crate) struct Session {
    pub(crate) web_dist: Option<PathBuf>,
    pub(super) record: Record,
    pub(super) home: PathBuf,
    pub(super) bundle: PathBuf,
    pub(super) allow_development: bool,
    pub(super) actions: Arc<Semaphore>,
    pub(super) stopping: std::sync::atomic::AtomicBool,
    pub(super) activation: Mutex<Activation>,
    pub(crate) shutdown: Notify,
}

impl Session {
    pub(super) fn new(
        record: Record,
        home: PathBuf,
        bundle: PathBuf,
        allow_development: bool,
    ) -> Self {
        Self {
            web_dist: None,
            record,
            home,
            bundle,
            allow_development,
            actions: Arc::new(Semaphore::new(1)),
            stopping: std::sync::atomic::AtomicBool::new(false),
            shutdown: Notify::new(),
            activation: Mutex::new(Activation {
                generation: 0,
                project: String::new(),
                focused: false,
                created: Instant::now(),
            }),
        }
    }
    pub(super) fn cookie_name(&self) -> String {
        format!("proofstorm_gui_{}", self.record.instance)
    }
    pub(super) fn recent(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.home.join("attachments")) else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for file in entries.take(200).flatten() {
            let Ok(Some(bytes)) = state::read(&file.path()) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let identity = value["identity"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            let Some(path) = value["entries"][0]["cwd"]
                .as_str()
                .or_else(|| identity.as_ref().and_then(|v| v[1].as_str()))
            else {
                continue;
            };
            if Path::new(path).is_absolute() && Path::new(path).is_dir() {
                paths.push((
                    file.metadata().and_then(|m| m.modified()).ok(),
                    path.to_owned(),
                ));
            }
        }
        paths.sort_by(|a, b| b.0.cmp(&a.0));
        let mut result = Vec::new();
        for (_, path) in paths {
            if !result.contains(&path) {
                result.push(path);
            }
        }
        result.truncate(8);
        result
    }
    pub(super) fn context(&self) -> Value {
        let runtime = Installation::load(&self.home)
            .and_then(|i| crate::bootstrap::check_installed_runtime(&i));
        let codex = crate::harness::launch::detect(&self.home, false);
        json!({"managed":true,"home":self.home,"recent_projects":self.recent(),"csrf":self.record.token,
            "runtime_ready":runtime.is_ok(),"runtime_error":runtime.err().map(|e|format!("{e:#}")),
            "codex_available":codex.is_ok(),"codex_error":codex.err().map(|e|format!("{e:#}")),
            "activation_generation":self.activation.lock().unwrap().generation})
    }
    pub(super) async fn open_project(
        &self,
        harness: crate::harness::Harness,
        project: PathBuf,
        preview: bool,
    ) -> Result<Value> {
        ensure!(
            self.record
                .build_sha256
                .as_ref()
                .is_none_or(|sha| crate::artifacts::hash(&self.record.executable)
                    .is_ok_and(|current| &current == sha)),
            "GUI build changed; run proofstorm stop, then proofstorm gui"
        );
        ensure!(
            project.is_absolute() && !project.as_os_str().is_empty(),
            "choose an absolute project folder"
        );
        let home = self.home.clone();
        let bundle = self.bundle.clone();
        let allow = self.allow_development;
        let (plan, launch) = tokio::task::spawn_blocking(move || {
            let plan = crate::harness::plan_for(harness, &home, &project, &bundle, allow)?;
            ensure!(
                preview || plan.project == project,
                "project folder changed after its preview; select it again"
            );
            let launch = crate::harness::launch::detect_for(harness, &plan.project, false)?;
            Ok::<_, anyhow::Error>((plan, launch))
        })
        .await??;
        if preview {
            return Ok(
                json!({"project":plan.project,"config":plan.config_path,"preset":plan.preset,
                "harness":harness,"interface":launch.interface,"configuration_will_change":plan.changes_configuration,"guidance":plan.guidance}),
            );
        }
        let project = plan.project.clone();
        let terminal =
            (launch.interface == "cli").then(|| crate::harness::launch::terminal_command(&launch));
        let mut attached = crate::harness::apply(plan).await?;
        if let Some(command) = terminal {
            attached["project"] = json!(project);
            attached["app_opened"] = json!(false);
            attached["terminal_required"] = json!(true);
            attached["terminal_command"] = json!(command);
            return Ok(attached);
        }
        let opened =
            tokio::task::spawn_blocking(move || crate::harness::launch::run(&launch)).await?;
        attached["project"] = json!(project);
        attached["app_opened"] = json!(opened.is_ok());
        if let Err(error) = opened {
            attached["launch_error"] = json!(format!("{error:#}"));
        }
        Ok(attached)
    }
    pub(super) async fn activate(&self, project: &str) -> Value {
        let generation = {
            let mut request = self.activation.lock().unwrap();
            request.generation = request.generation.wrapping_add(1);
            request.project = project.into();
            request.focused = false;
            request.created = Instant::now();
            request.generation
        };
        let deadline = Instant::now() + Duration::from_millis(1600);
        while Instant::now() < deadline {
            if self.activation.lock().unwrap().focused {
                return json!({"focused":true,"generation":generation});
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        json!({"focused":false,"generation":generation})
    }
}

pub(super) fn environment(installation: &Installation) -> Result<Environment> {
    Environment::resolve(
        |key| match key {
            "PROOFSTORM_HOME" => Some(installation.home.to_string_lossy().into_owned()),
            "PROOFSTORM_PRINCIPAL" => Some("developer".into()),
            _ => None,
        },
        &installation.home,
    )
}

pub(super) async fn serve(
    home: &Path,
    bundle: &Path,
    instance: &str,
    allow_development: bool,
) -> Result<()> {
    let installation = Installation::load(home)?;
    let _lifetime = state::lease(&installation.home, "gui-runtime-lock.sqlite3")?;
    let mut record = state::record(&installation.home, &installation.id)?
        .context("missing GUI launch reservation")?;
    ensure!(
        record.instance == instance
            && record.pid == 0
            && record.port == 0
            && record.executable == std::env::current_exe()?.canonicalize()?,
        "GUI launch reservation differs; refusing adoption"
    );
    ensure!(
        record
            .build_sha256
            .as_ref()
            .is_none_or(|sha| crate::artifacts::hash(&record.executable)
                .is_ok_and(|current| &current == sha)),
        "GUI executable changed during launch; stop and reopen the GUI"
    );
    let allow_development = crate::artifacts::verify(home, bundle, allow_development)?;
    crate::bootstrap::check_installed_runtime(&installation)?;
    let environment = environment(&installation)?;
    ensure!(
        std::fs::symlink_metadata(environment.database.as_path())?.is_file(),
        "missing installation database; run setup first"
    );
    let store = proofstorm_store::Store::open(&environment.database)?;
    store.authorize(
        DEFAULT_WORKSPACE,
        "developer",
        proofstorm_core::Capability::LabStatus,
    )?;
    let runtime = environment.runtime().await?;
    let labs = Labs::new(store, runtime, DEFAULT_WORKSPACE.into(), "developer".into())
        .with_installation(Some(installation.clone()));
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    record.port = listener.local_addr()?.port();
    record.pid = std::process::id();
    state::save(&installation.home.join(RECORD), &record)?;
    let mut session = Session::new(
        record.clone(),
        installation.home.clone(),
        bundle.to_path_buf(),
        allow_development,
    );
    session.web_dist = crate::artifacts::web_dist(home)?;
    let session = Arc::new(session);
    let result = crate::http::serve_managed(labs, listener, session).await;
    state::remove_owned(&installation.home, &record)?;
    result.map_err(Into::into)
}
