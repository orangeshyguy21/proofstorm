//! Private, bounded Docker captures and cleanup for the experimental stack probe.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub struct Docker {
    pub work: PathBuf,
    pub owner: String,
    pub cancelled: Arc<AtomicBool>,
    sequence: usize,
    cleaning_up: bool,
}

impl Docker {
    pub fn new(work: &Path, cancelled: Arc<AtomicBool>) -> Result<Self> {
        fs::DirBuilder::new().mode(0o700).create(work)?;
        let work = work.canonicalize()?;
        let owner = format!(
            "ps-bark-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
        );
        let result = Self {
            work,
            owner,
            cancelled,
            sequence: 0,
            cleaning_up: false,
        };
        result.save("owner.json", &json!({"label":result.label(),"scope":"experimental Docker dependency probe; not a catalog cell"}))?;
        Ok(result)
    }

    pub fn label(&self) -> String {
        format!("proofstorm.bark-probe={}", self.owner)
    }
    pub fn name(&self, role: &str) -> String {
        format!("{}-{role}", self.owner)
    }

    pub fn save(&self, name: &str, value: &Value) -> Result<()> {
        fs::write(self.work.join(name), serde_json::to_vec_pretty(value)?)?;
        Ok(())
    }

    pub fn file(&self, name: &str, contents: &str) -> Result<String> {
        let path = self.work.join(name);
        // The enclosing evidence directory is 0700. Read-only bind mounts expose
        // only the explicitly selected file to the non-root container user.
        fs::write(&path, contents)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444))?;
        Ok(path.display().to_string())
    }

    pub fn raw(&mut self, args: &[&str]) -> Result<Output> {
        ensure!(
            self.cleaning_up || !self.cancelled.load(Ordering::SeqCst),
            "probe cancelled"
        );
        self.sequence += 1;
        let mut command = Command::new("docker");
        command.args(args);
        let started = Instant::now();
        let output = proofstorm_acceptance::process::capture(command, 120)?;
        fs::write(
            self.work.join(format!("{:04}.stdout", self.sequence)),
            &output.stdout,
        )?;
        fs::write(
            self.work.join(format!("{:04}.stderr", self.sequence)),
            &output.stderr,
        )?;
        writeln!(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.work.join("commands.jsonl"))?,
            "{}",
            json!({"sequence":self.sequence,"args":args,"exit":output.status.code(),"elapsed_ms":started.elapsed().as_millis()})
        )?;
        Ok(output)
    }

    pub fn run(&mut self, args: &[&str]) -> Result<String> {
        let output = self.raw(args)?;
        ensure!(
            output.status.success(),
            "Docker command {} failed; inspect private capture",
            self.sequence
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    pub fn json(&mut self, args: &[&str]) -> Result<Value> {
        serde_json::from_str(&self.run(args)?).context("Docker/native JSON response")
    }

    pub fn wait(&mut self, args: &[&str]) -> Result<String> {
        let start = Instant::now();
        loop {
            ensure!(!self.cancelled.load(Ordering::SeqCst), "probe cancelled");
            let output = self.raw(args)?;
            if output.status.success() {
                return Ok(String::from_utf8(output.stdout)?.trim().to_owned());
            }
            ensure!(
                start.elapsed() < Duration::from_secs(180),
                "dependency readiness deadline; inspect private captures"
            );
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    /// Retry observations only. Callers submit each payment exactly once.
    pub fn poll(&mut self, args: &[&str], ready: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let output = self.raw(args)?;
            if output.status.success() {
                let value: Value = serde_json::from_slice(&output.stdout)?;
                if ready(&value) {
                    return Ok(value);
                }
            }
            ensure!(
                Instant::now() < deadline,
                "observation deadline; inspect private captures"
            );
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    pub fn image(&mut self, reference: &str, pull: bool) -> Result<String> {
        if !self.raw(&["image", "inspect", reference])?.status.success() && pull {
            self.run(&["pull", reference])?;
        }
        let image = self.json(&["image", "inspect", reference])?;
        ensure!(
            image[0]["Os"] == "linux" && image[0]["Architecture"] == "arm64",
            "this probe currently requires native Linux ARM64 images"
        );
        image[0]["Id"]
            .as_str()
            .map(str::to_owned)
            .context("image ID missing")
    }

    pub fn volume(&mut self, role: &str, image: &str) -> Result<String> {
        let name = self.name(role);
        ensure!(
            !self.raw(&["volume", "inspect", &name])?.status.success(),
            "volume already exists; refusing adoption"
        );
        self.run(&["volume", "create", "--label", &self.label(), &name])?;
        // Record a named helper before starting it, so cancellation cannot orphan it.
        let helper = self.name(&format!("init-{role}"));
        self.run(&[
            "create",
            "--name",
            &helper,
            "--label",
            &self.label(),
            "--network",
            "none",
            "--user",
            "0:0",
            "--entrypoint",
            "sh",
            "-v",
            &format!("{name}:/data"),
            image,
            "-ec",
            "chown 1000:1000 /data; chmod 700 /data",
        ])?;
        self.run(&["start", "-a", &helper])?;
        self.run(&["rm", "-v", &helper])?;
        Ok(name)
    }

    pub fn cleanup(&mut self) -> Result<()> {
        self.cleaning_up = true;
        let label = format!("label={}", self.label());
        let mut failures = Vec::new();
        // Detached native wallet operations write private logs inside the owned
        // container. Preserve them before deletion, including on a failed run.
        for file in ["mint.log", "mint.exit", "melt.log"] {
            let _ = self.raw(&[
                "cp",
                &format!("{}:/wallet/{file}", self.name("wallet")),
                &self.work.join(file).display().to_string(),
            ]);
        }
        if let Ok(output) = self.raw(&["logs", "--tail", "1000", &self.name("server")]) {
            let mut log = output.stdout;
            log.extend(output.stderr);
            if let Err(error) = fs::write(self.work.join("server.log"), log) {
                failures.push(error.to_string());
            }
        }
        // Cleanup is authorized only by the unique per-run label, never a
        // repository-wide prune. Continue after failures to remove other resources.
        for (kind, listing, removal) in [
            (
                "container",
                vec!["ps", "-aq", "--filter", &label],
                vec!["rm", "-f", "-v"],
            ),
            (
                "volume",
                vec!["volume", "ls", "-q", "--filter", &label],
                vec!["volume", "rm"],
            ),
            (
                "network",
                vec!["network", "ls", "-q", "--filter", &label],
                vec!["network", "rm"],
            ),
        ] {
            let identifiers = match self.run(&listing) {
                Ok(value) => value,
                Err(error) => {
                    failures.push(error.to_string());
                    continue;
                }
            };
            for id in identifiers.lines() {
                if kind == "container" {
                    let _ = self.raw(&["logs", "--tail", "300", id]);
                }
                let mut args = removal.clone();
                args.push(id);
                if let Err(error) = self.run(&args) {
                    failures.push(error.to_string());
                }
            }
            match self.run(&listing) {
                Ok(remaining) if remaining.is_empty() => (),
                _ => failures.push(format!("{kind} absence not verified")),
            }
        }
        self.save(
            "cleanup.json",
            &json!({"passed":failures.is_empty(),"failures":failures}),
        )?;
        ensure!(
            failures.is_empty(),
            "owned cleanup failed; consult cleanup.json"
        );
        Ok(())
    }
}
