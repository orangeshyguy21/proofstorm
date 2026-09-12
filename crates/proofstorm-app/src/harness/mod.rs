//! Project-scoped agent attachment. GUI callers reuse these same plan/apply/launch steps.
mod agents;
mod config;
mod desktop;
mod json_config;
mod replacement;
pub use replacement::ConnectionConflict;
pub mod launch;
#[cfg(test)]
mod tests;
mod verify;

use crate::{config::DEFAULT_WORKSPACE, installation::Installation};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "storm";

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub const RECONNECT: &str = "In Codex, trust this project only if you trust its contents. Start a new task or restart/reconnect MCP in an existing session, then check /mcp. Server verification does not prove the app loaded it. Model, provider and permission settings were not changed.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    #[default]
    Codex,
    Opencode,
    #[value(alias = "claude-code")]
    #[serde(alias = "claude-code")]
    Claude,
}

impl Harness {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Claude => "claude",
        }
    }
    #[must_use]
    pub fn guidance(self) -> &'static str {
        match self {
            Self::Codex => RECONNECT,
            Self::Opencode => {
                "Start OpenCode in this project, then check its MCP status. Existing sessions may need a restart. Project configuration is inherited by subdirectories; it is not a filesystem sandbox. Keep this machine-specific configuration out of shared commits. Model, provider and permission settings were not changed. Server verification does not prove the agent loaded it."
            }
            Self::Claude => {
                "Start Claude Code in this project and review its project trust and MCP approval prompts. Check /mcp in a new session. Project configuration is inherited by subdirectories; it is not a filesystem sandbox. Keep this machine-specific .mcp.json entry out of shared commits. Model, provider and permission settings were not changed. Server verification does not prove the agent loaded it."
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AttachmentPlan {
    pub harness: Harness,
    pub project: PathBuf,
    pub home: PathBuf,
    pub actor: String,
    pub config_path: PathBuf,
    pub entry: Value,
    pub changes_configuration: bool,
    pub preset: &'static str,
    pub guidance: &'static str,
    #[serde(skip)]
    installation: Installation,
    #[serde(skip)]
    original: Option<String>,
    #[serde(skip)]
    proposed: String,
    #[serde(skip)]
    receipt_path: PathBuf,
    #[serde(skip)]
    identity: String,
    #[serde(skip)]
    server_entry: Value,
}

pub fn codex_home() -> Result<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map_or_else(
            || std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")),
            |h| Some(PathBuf::from(h)),
        )
        .context("cannot locate Codex configuration home")?;
    ensure!(home.is_absolute(), "CODEX_HOME must be absolute");
    Ok(home)
}

/// Inspection only: no grants, folders, downloads, or harness launch.
pub fn plan(
    home: &Path,
    project: &Path,
    bundle: &Path,
    allow_development: bool,
) -> Result<AttachmentPlan> {
    plan_for(Harness::Codex, home, project, bundle, allow_development)
}

pub fn plan_for(
    harness: Harness,
    home: &Path,
    project: &Path,
    bundle: &Path,
    allow_development: bool,
) -> Result<AttachmentPlan> {
    plan_confirmed(harness, home, project, bundle, allow_development, None)
}

pub(crate) fn plan_confirmed(
    harness: Harness,
    home: &Path,
    project: &Path,
    bundle: &Path,
    allow_development: bool,
    confirmation: Option<&str>,
) -> Result<AttachmentPlan> {
    let installation = Installation::load(home)?;
    let bundle = bundle.canonicalize()?;
    crate::artifacts::verify(home, &bundle, allow_development)?;
    let project = project
        .canonicalize()
        .context("project directory must already exist")?;
    ensure!(
        project.is_dir() && project.parent().is_some(),
        "select a project directory, not a filesystem root"
    );
    if let Some(home) = std::env::var_os("HOME") {
        ensure!(
            project.as_os_str() != home,
            "select a project, not your entire home directory"
        );
    }
    let config_path = agents::path(harness, &project)?;
    config::directory(&installation.home.join("attachments"), false)?;
    agents::inherited(harness, &project)?;
    if harness != Harness::Codex {
        launch::detect_for(harness, &project, false)
            .or_else(|_| launch::detect_for(harness, &project, true))?;
    }
    let identity = serde_json::to_string(&json!([installation.id, project, harness.name()]))?;
    let actor = format!("{}-{}", harness.name(), &hash(identity.as_bytes())[..32]);
    let receipt_path = installation
        .home
        .join("attachments")
        .join(format!("{actor}.json"));
    let owned = if let Some(text) = config::read(&receipt_path)? {
        let receipt: Value =
            serde_json::from_str(&text).context("invalid attachment ownership receipt")?;
        ensure!(
            receipt["format_version"] == 1
                && receipt["identity"] == identity
                && receipt["preset"] == crate::developer::PRESET,
            "attachment owner or preset differs; explicit migration required"
        );
        receipt["entries"]
            .as_array()
            .context("managed entry receipt missing")?
            .clone()
    } else {
        Vec::new()
    };
    // Installed launchers follow atomic bundle upgrades. Unpacked bundles stay pinned.
    let command = if let Some(mcp) = crate::artifacts::checkout_mcp(home)? {
        mcp
    } else if bundle
        .parent()
        .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "versions"))
    {
        let managed = bundle
            .parent()
            .and_then(Path::parent)
            .context("managed installation root")?;
        ensure!(
            config::read(&managed.join("install.json"))?.is_some(),
            "missing installation owner marker"
        );
        ensure!(
            managed.join("current").canonicalize()? == bundle,
            "use the current installed executable before attaching"
        );
        managed.join("current/bin/proofstorm-mcp")
    } else {
        bundle.join("bin/proofstorm-mcp")
    };
    let server_entry = json!({"command":command,"args":["--home",installation.home,"--attachment",actor],"cwd":project,
        "startup_timeout_sec":60,"tool_timeout_sec":1800,"required":true,"enabled":true});
    let entry = agents::entry(harness, &server_entry)?;
    let original = config::read(&config_path)?;
    let proposed = replacement::merge(
        harness,
        &config_path,
        original.as_deref(),
        &entry,
        &owned,
        confirmation,
    )?;
    Ok(AttachmentPlan {
        harness,
        home: installation.home.clone(),
        project,
        actor,
        config_path,
        // Explicit adoption also records ownership when the existing bytes
        // already match; otherwise every subsequent click would ask again.
        changes_configuration: original.as_deref() != Some(&proposed) || confirmation.is_some(),
        entry,
        preset: crate::developer::PRESET,
        guidance: harness.guidance(),
        installation,
        original,
        proposed,
        receipt_path,
        identity,
        server_entry,
    })
}

/// Apply only to a ready installation. An interrupted apply is safely repeatable.
pub async fn apply(plan: AttachmentPlan) -> Result<Value> {
    let _guard = Installation::lock(&plan.home)?;
    ensure!(
        Installation::load(&plan.home)? == plan.installation,
        "installation changed while planning"
    );
    agents::inherited(plan.harness, &plan.project)?;
    unchanged(&plan)?;
    ensure!(
        crate::bootstrap::doctor(&plan.home)["ok"] == true,
        "installation is not ready; run proofstorm doctor and proofstorm setup before attachment"
    );
    // Database existence/type is checked separately; never create one during attachment.
    ensure!(
        std::fs::symlink_metadata(plan.installation.database())?.is_file(),
        "missing or linked installation database; run setup"
    );
    let store = proofstorm_store::Store::open(plan.installation.database())?;
    let new_actor = store.initialize_actor_once(
        DEFAULT_WORKSPACE,
        &plan.actor,
        &plan.identity,
        plan.preset,
        &crate::developer::CAPABILITIES,
    )?;
    let server = verify::server(&plan.server_entry).await.context("MCP verification failed; no project configuration was changed. Fix doctor/grants and retry attachment")?;
    unchanged(&plan)?;
    let mut backup = None;
    if plan.changes_configuration {
        if plan.harness == Harness::Codex {
            config::directory(&plan.project.join(".codex"), true)?;
        }
        config::directory(&plan.home.join("attachments"), true)?;
        if let Some(original) = &plan.original {
            backup = Some(config::backup(&plan.config_path, original)?);
        }
        let previous = plan
            .original
            .as_ref()
            .map(|s| agents::existing(plan.harness, &plan.config_path, s))
            .transpose()?
            .flatten();
        let mut entries = vec![plan.entry.clone()];
        if let Some(previous) = previous {
            entries.push(previous);
        }
        let receipt = |entries: Vec<Value>| json!({"format_version":1,"identity":plan.identity,"preset":plan.preset,"entries":entries});
        // Write both states before config activation so a crash cannot orphan the entry.
        config::save(
            &plan.receipt_path,
            &serde_json::to_vec_pretty(&receipt(entries))?,
            false,
        )?;
        unchanged(&plan)?;
        config::save(&plan.config_path, plan.proposed.as_bytes(), true)?;
        config::save(
            &plan.receipt_path,
            &serde_json::to_vec_pretty(&receipt(vec![plan.entry.clone()]))?,
            false,
        )?;
    }
    Ok(
        json!({"attached":true,"configuration_changed":plan.changes_configuration,"config":plan.config_path,"backup":backup,
        "actor":plan.actor,"actor_initialized":new_actor,"preset":plan.preset,"server_verified":server,"harness_loaded":false,
        "harness":plan.harness,"guidance":plan.guidance,"starter_request":"Use Proofstorm to inspect the local environment, then help me choose and start a cell for this project."}),
    )
}

fn unchanged(plan: &AttachmentPlan) -> Result<()> {
    ensure!(
        agents::path(plan.harness, &plan.project)? == plan.config_path,
        "project configuration path changed; retry"
    );
    agents::inherited(plan.harness, &plan.project)?;
    ensure!(
        config::read(&plan.config_path)? == plan.original,
        "project configuration changed since planning; retry (nothing overwritten)"
    );
    Ok(())
}
