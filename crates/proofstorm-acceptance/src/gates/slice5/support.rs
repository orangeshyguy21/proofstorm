//! Failure-safe, incarnation-scoped cleanup for these disposable live gates.
use std::{collections::BTreeSet, thread::sleep, time::Duration};

use anyhow::{Result, bail, ensure};
use serde_json::Value;

use crate::{GateContext, Kubectl, gate::CONTROL_NAMESPACE, json as expect};

/// Armed before stopping the controller, so a partial stop also gets restored.
pub(super) struct ControllerPause<'a> {
    kubectl: &'a Kubectl,
    armed: bool,
}

impl<'a> ControllerPause<'a> {
    pub(super) fn stop(kubectl: &'a Kubectl) -> Result<Self> {
        let guard = Self {
            kubectl,
            armed: true,
        };
        kubectl.stop_controller()?;
        Ok(guard)
    }

    pub(super) fn resume(mut self) -> Result<()> {
        self.kubectl.start_controller()?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for ControllerPause<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = self.kubectl.start_controller() {
                eprintln!("Controller restoration failed; manual recovery required: {error:#}");
            }
        }
    }
}

pub(super) fn preflight(context: &GateContext) -> Result<()> {
    let cells = context
        .kubectl
        .get_json(&["get", "proofstormcells", "-n", CONTROL_NAMESPACE])?;
    ensure!(
        expect::array(&cells, "/items")?.is_empty(),
        "these gates require an idle cluster because recovery scenarios restart the shared controller; no cells were changed"
    );
    context.kubectl.assert_no_instance_namespaces()?;
    let controller =
        context
            .kubectl
            .get_json(&["get", "deployment/proofstormd", "-n", CONTROL_NAMESPACE])?;
    ensure!(
        controller.pointer("/spec/replicas").and_then(Value::as_u64) == Some(1)
            && controller
                .pointer("/status/availableReplicas")
                .and_then(Value::as_u64)
                == Some(1),
        "expected one available controller before running the gate; no controller state was changed"
    );
    Ok(())
}

fn owns_cell(cell: &Value, workspace: &str, instance: &str) -> bool {
    cell.pointer("/spec/workspaceId").and_then(Value::as_str) == Some(workspace)
        && cell.pointer("/spec/instanceId").and_then(Value::as_str) == Some(instance)
}

/// This fallback does not depend on a healthy MCP child or its private database.
/// Unique workspace identity fences cleanup, including partial materialization.
pub(super) struct CellCleanup<'a> {
    context: &'a GateContext,
    workspace: String,
    instance: &'static str,
    keys: BTreeSet<String>,
    armed: bool,
}

impl<'a> CellCleanup<'a> {
    pub(super) fn new(context: &'a GateContext, workspace: String, instance: &'static str) -> Self {
        Self {
            context,
            workspace,
            instance,
            keys: BTreeSet::new(),
            armed: true,
        }
    }

    pub(super) fn record(&mut self, key: &str) {
        self.keys.insert(key.to_owned());
    }

    pub(super) fn finish(&mut self) -> Result<()> {
        let kubectl = &self.context.kubectl;
        let cells = kubectl.get_json(&["get", "proofstormcells", "-n", CONTROL_NAMESPACE])?;
        for cell in expect::array(&cells, "/items")? {
            if !owns_cell(cell, &self.workspace, self.instance) {
                continue;
            }
            let key = expect::string(cell, "/spec/instanceKey")?;
            ensure!(
                self.keys.is_empty() || self.keys.contains(key),
                "refusing cleanup of a replacement incarnation in workspace {}",
                self.workspace
            );
            self.keys.insert(key.to_owned());
            let name = expect::string(cell, "/metadata/name")?;
            kubectl.run(&[
                "delete",
                "proofstormcell",
                name,
                "-n",
                CONTROL_NAMESPACE,
                "--wait=false",
                "--ignore-not-found",
            ])?;
        }
        // The controller's finalizer reclaims resources. Never force-remove it.
        for _ in 0..90 {
            let cells = kubectl.get_json(&["get", "proofstormcells", "-n", CONTROL_NAMESPACE])?;
            let mut remains = expect::array(&cells, "/items")?
                .iter()
                .any(|cell| owns_cell(cell, &self.workspace, self.instance));
            for key in &self.keys {
                let selector = format!("proofstorm.dev/instance={key}");
                for args in [
                    vec!["get", "namespaces", "-l", &selector, "-o", "name"],
                    vec![
                        "get",
                        "proofstormcellactions",
                        "-n",
                        CONTROL_NAMESPACE,
                        "-l",
                        &selector,
                        "-o",
                        "name",
                    ],
                ] {
                    remains |= !kubectl.run(&args)?.is_empty();
                }
            }
            if !remains {
                self.armed = false;
                return Ok(());
            }
            sleep(Duration::from_secs(2));
        }
        bail!(
            "cleanup was not verified for workspace {}; retained instance keys: {:?}",
            self.workspace,
            self.keys
        )
    }
}

impl Drop for CellCleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = self.finish() {
                eprintln!("Disposable cell cleanup failed: {error:#}");
            }
        }
    }
}

pub(super) fn with_cleanup<T>(
    body: impl FnOnce() -> Result<T>,
    cleanup: impl FnOnce() -> Result<()>,
) -> Result<T> {
    let result = body();
    let cleaned = cleanup();
    match (result, cleaned) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error.context("gate passed but cleanup failed")),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("cleanup also failed: {cleanup:#}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    #[test]
    fn cleanup_requires_both_workspace_and_instance_identity() {
        let cell = json!({"spec":{"workspaceId":"run-a","instanceId":"cell"}});
        assert!(owns_cell(&cell, "run-a", "cell"));
        assert!(!owns_cell(&cell, "run-b", "cell"));
        assert!(!owns_cell(&cell, "run-a", "other"));
        assert!(!owns_cell(&json!({}), "run-a", "cell"));
    }

    #[test]
    fn cleanup_runs_on_error_and_cannot_hide_either_failure() {
        let called = Cell::new(false);
        let result: Result<()> = with_cleanup(
            || bail!("scenario failed"),
            || {
                called.set(true);
                bail!("cleanup failed")
            },
        );
        let message = format!("{:#}", result.unwrap_err());
        assert!(called.get());
        assert!(message.contains("scenario failed") && message.contains("cleanup failed"));
        assert!(with_cleanup(|| Ok(()), || bail!("cleanup failed")).is_err());
        assert!(with_cleanup(|| Ok(()), || Ok(())).is_ok());
    }
}
