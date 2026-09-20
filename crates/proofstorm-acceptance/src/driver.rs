//! Direct controller/driver fixtures. These are not MCP tools or agent workflow coverage.
//! Native wallet and public-surface gates use `native` and `cell_exec` instead.
use crate::{GateContext, McpClient, json as expect};
use anyhow::{Context, Result, ensure};
use proofstorm_core::{Capability, OperationKind, OperationPhase};
use proofstorm_kube::CellAction;
use proofstorm_store::Store;
use serde_json::{Value, json};

pub(crate) struct Scope {
    pub store: Store,
    pub workspace: String,
    pub principal: String,
    pub instance: proofstorm_core::CellInstance,
    pub run: String,
}

pub(crate) fn scope(
    context: &GateContext,
    client: &mut McpClient,
    request: &Value,
) -> Result<Scope> {
    let environment = client.call("environment_read", json!({"scan":true,"limit":1}))?;
    let workspace = expect::string(&environment, "/workspace/id")?.to_owned();
    let run = if let Some(run) = request["run_id"].as_str() {
        run.to_owned()
    } else {
        let inspected = client.call(
            "cell_inspect",
            json!({"name":expect::string(request,"/name")?}),
        )?;
        expect::string(&inspected, "/run_id")?.to_owned()
    };
    let record = client.call("run_read", json!({"run_id":run}))?;
    let principal = expect::string(&record, "/owner_principal_id")?.to_owned();
    let store = Store::open(context.database())?;
    let instance = store.instance(
        &workspace,
        &principal,
        expect::string(&record, "/instance_id")?,
    )?;
    if let Some(name) = request["name"].as_str() {
        ensure!(
            store
                .resolve_cell(&workspace, &principal, name)?
                .instance_id
                == instance.id,
            "fixture run belongs to a different cell"
        );
    }
    Ok(Scope {
        store,
        workspace,
        principal,
        instance,
        run,
    })
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "backend fixtures share an owned request signature with the gate callback contract"
)]
fn submit(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
    kind: OperationKind,
    capability: Capability,
    action: CellAction,
) -> Result<Value> {
    let scope = scope(context, client, &request)?;
    // Explicit test-owned driver authority. This cannot expose an extra MCP route.
    scope
        .store
        .grant(&scope.workspace, &scope.principal, capability)?;
    let id = expect::string(&request, "/request_id")?;
    let operation = scope.store.create_operation(
        &scope.workspace,
        &scope.principal,
        &scope.instance.id,
        &scope.run,
        "",
        id,
        kind,
        &request,
        id,
        capability,
    )?;
    if operation.phase != OperationPhase::Pending {
        return Ok(json!(operation));
    }
    let resource = proofstorm_app::runtime::runtime_action_resource(
        crate::gate::CONTROL_NAMESPACE,
        &scope.instance,
        &operation,
        action,
    );
    context
        .kubectl
        .apply_stdin(&serde_json::to_string(&resource)?)?;
    Ok(json!(scope.store.update_operation_phase(
        &scope.workspace,
        &operation.id,
        OperationPhase::Running
    )?))
}

fn fields(request: &Value, keys: &[&str]) -> Result<Value> {
    let mut result = serde_json::Map::new();
    for key in keys {
        let value = request
            .get(*key)
            .cloned()
            .or_else(|| match *key {
                "timeout_seconds" => Some(json!(60)),
                "tolerance_sat" => Some(json!(0)),
                _ => None,
            })
            .with_context(|| format!("driver fixture missing {key}"))?;
        let mut words = key.split('_');
        let mut camel = words.next().unwrap().to_owned();
        for word in words {
            let mut chars = word.chars();
            if let Some(c) = chars.next() {
                camel.extend(c.to_uppercase());
                camel.extend(chars);
            }
        }
        result.insert(camel, value);
    }
    Ok(Value::Object(result))
}

pub fn authentication_conformance(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::AuthenticationConformance(serde_json::from_value(fields(
        &request,
        &["mint", "identity_provider"],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::AuthenticationConformance,
        Capability::AuthenticationTest,
        action,
    )
}

pub fn authentication_protected_spend(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::AuthenticationProtectedSpend(serde_json::from_value(fields(
        &request,
        &["mint", "identity_provider"],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::AuthenticationProtectedSpend,
        Capability::AuthenticationTest,
        action,
    )
}

pub fn authentication_replay(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let scope = scope(context, client, &request)?;
    let source = scope.store.operation(
        &scope.workspace,
        &scope.principal,
        expect::string(&request, "/source_operation_id")?,
    )?;
    ensure!(
        source.instance_id == scope.instance.id
            && source.experiment_id == scope.run
            && source.principal_id == scope.principal
            && source.kind == OperationKind::AuthenticationProtectedSpend
            && source.phase == OperationPhase::Succeeded
            && source
                .artifact
                .as_ref()
                .is_some_and(|a| a.content["conformant"] == true),
        "invalid authentication replay source"
    );
    let action = CellAction::AuthenticationReplay(proofstorm_kube::AuthenticationReplayAction {
        mint: expect::string(&request, "/mint")?.into(),
        identity_provider: expect::string(&request, "/identity_provider")?.into(),
        session_secret: format!("{}-auth-session", source.resource_name),
        source_operation_id: source.id,
    });
    submit(
        context,
        client,
        request,
        OperationKind::AuthenticationReplay,
        Capability::AuthenticationTest,
        action,
    )
}
