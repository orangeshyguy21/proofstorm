use anyhow::{Result, ensure};
use serde_json::{Value, json};

use crate::{GateContext, cell, native};

fn document(instance: &str) -> Value {
    json!({
        "api_version":"proofstorm/v1alpha1", "name":instance,
        "components":[{"id":"workspace","kind":"workspace","implementation":"workspace",
            "version":"0.1.0-alpha.1","config_version":"workspace/0.1/v1","control":"workspace","config":{}}],
        "links":[], "policy":{"allow":["component.exec_live","component.control"],
            "limits":{"max_components":4,"max_links":4,"max_config_bytes":16384}}
    })
}

pub(super) fn run(context: &GateContext) -> Result<()> {
    let instance = "qualification-workspace";
    let run = "workspace-persistence";
    let mut client = context.default_session(instance, "qualifier")?;
    let document = context.document(document(instance))?;
    let preview = client.call(
        "cell_plan",
        json!({"name":instance,"request_id":"create","cell":document}),
    )?;
    cell::review(&mut client, &preview)?;
    cell::apply(&mut client, &preview)?;
    cell::wait_ready(&mut client, instance)?;
    client.call(
        "run_start",
        json!({"name":instance,"run_id":run,"request_id":"run"}),
    )?;
    let mut session = native::Session::new(&mut client, instance, run);
    session.execute("workspace", "write", "set -eu; test \"$(pwd)\" = /workspace; printf 'qualification-persistent-state' > /workspace/qualification-state")?;
    drop(session);
    client.call(
        "component_restart",
        json!({"name":instance,"run_id":run,"request_id":"restart","component":"workspace"}),
    )?;
    cell::wait_operation(&mut client, "restart", 80)?;
    cell::wait_ready(&mut client, instance)?;
    let mut session = native::Session::new(&mut client, instance, run);
    let read = session.execute("workspace", "read", "cat /workspace/qualification-state")?;
    ensure!(
        read["stdout"] == "qualification-persistent-state",
        "workspace state did not survive restart"
    );
    drop(session);
    client.call("run_finish", json!({"run_id":run,"request_id":"finish"}))?;
    client.call("cell_remove", json!({"name":instance}))?;
    cell::wait_closed(&mut client, instance)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_core::{CellSpec, ComponentKind, ControlClass, resolve_lock};
    use proofstorm_qualification::{Identity, Scenario};

    #[test]
    fn persistence_fixture_resolves_for_every_planned_platform_and_version() {
        let plan = proofstorm_qualification::plan(
            Identity {
                revision: "a".repeat(40),
                run_id: "0".into(),
                attempt: 1,
            },
            proofstorm_qualification::Mode::Compatibility,
        )
        .unwrap();
        let mut platforms = std::collections::BTreeSet::new();
        for case in &plan.cases {
            if !matches!(&case.scenario, Scenario::Gate { name, .. } if name == "workspace-persistence")
            {
                continue;
            }
            platforms.insert(case.platform.as_str());
            let catalog = proofstorm_qualification::catalog(&case.platform).unwrap();
            let mut fixture = document("qualification-workspace");
            // Also validate the authored contract, before version selection can repair it.
            let authored: CellSpec = serde_json::from_value(fixture.clone()).unwrap();
            resolve_lock(&authored, &catalog).unwrap();
            let observer = crate::qualification::Observer::new(case.clone());
            observer.document(&mut fixture).unwrap();
            observer.finish().unwrap();
            let cell: CellSpec = serde_json::from_value(fixture).unwrap();
            let component = &cell.components[0];
            assert_eq!(component.kind, ComponentKind::Workspace);
            assert_eq!(component.control, ControlClass::Workspace);
            let lock = resolve_lock(&cell, &catalog).unwrap();
            assert_eq!(lock.entries.len(), 1);
            assert_eq!(lock.entries[0].catalog_id, "workspace");
        }
        assert_eq!(
            platforms,
            std::collections::BTreeSet::from(["linux/amd64", "linux/arm64"])
        );
    }
}
