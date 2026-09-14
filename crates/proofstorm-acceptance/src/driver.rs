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
    let operation = if let CellAction::WalletPay(pay) = &action {
        scope.store.create_wallet_pay_operation(
            &scope.instance.revision_digest,
            &scope.workspace,
            &scope.principal,
            &scope.instance.id,
            &scope.run,
            "",
            id,
            &request,
            id,
            &pay.recipient_wallet,
            &pay.recipient_mint,
            &pay.mint_quote_id,
            &pay.wallet,
            &pay.mint,
        )?
    } else {
        scope.store.create_operation(
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
        )?
    };
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

pub fn liquidity_bootstrap(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::BootstrapLiquidity(serde_json::from_value(fields(
        &request,
        &[
            "chain",
            "mint_lightning",
            "payer_lightning",
            "funding_sat",
            "channel_sat",
            "push_sat",
        ],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::BootstrapLiquidity,
        Capability::WalletFund,
        action,
    )
}

pub fn peer_connect(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::PeerConnect(serde_json::from_value(fields(
        &request,
        &["from_lightning", "to_lightning"],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::PeerConnect,
        Capability::PeerConnect,
        action,
    )
}

pub fn peer_disconnect(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::PeerDisconnect(serde_json::from_value(fields(
        &request,
        &["from_lightning", "to_lightning"],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::PeerDisconnect,
        Capability::PeerDisconnect,
        action,
    )
}

pub fn channel_open(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::ChannelOpen(serde_json::from_value(fields(
        &request,
        &[
            "chain",
            "from_lightning",
            "to_lightning",
            "channel_sat",
            "push_sat",
        ],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::ChannelOpen,
        Capability::ChannelOpen,
        action,
    )
}

pub fn channel_close(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::ChannelClose(serde_json::from_value(fields(
        &request,
        &["chain", "from_lightning", "to_lightning", "channel_id"],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::ChannelClose,
        Capability::ChannelClose,
        action,
    )
}

pub fn channel_force_close(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::ChannelForceClose(serde_json::from_value(fields(
        &request,
        &["chain", "from_lightning", "to_lightning", "channel_id"],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::ChannelForceClose,
        Capability::ChannelForceClose,
        action,
    )
}

pub fn channel_rebalance(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::ChannelRebalance(serde_json::from_value(fields(
        &request,
        &[
            "lightning",
            "outgoing_channel_id",
            "incoming_channel_id",
            "amount_sat",
            "max_fee_sat",
        ],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::ChannelRebalance,
        Capability::ChannelRebalance,
        action,
    )
}

pub fn wallet_initialize(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::WalletInitialize(serde_json::from_value(fields(
        &request,
        &["wallet", "mint"],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::WalletInitialize,
        Capability::WalletCreate,
        action,
    )
}

pub fn wallet_fund(context: &GateContext, client: &mut McpClient, request: Value) -> Result<Value> {
    let action = CellAction::WalletFund(serde_json::from_value(fields(
        &request,
        &["wallet", "mint", "payer_lightning", "amount_sat"],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::WalletFund,
        Capability::WalletFund,
        action,
    )
}

pub fn wallet_invoice(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::WalletInvoice(serde_json::from_value(fields(
        &request,
        &["wallet", "mint", "amount_sat", "timeout_seconds"],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::WalletInvoice,
        Capability::WalletControl,
        action,
    )
}

pub fn wallet_pay(context: &GateContext, client: &mut McpClient, request: Value) -> Result<Value> {
    let action = CellAction::WalletPay(serde_json::from_value(fields(
        &request,
        &[
            "wallet",
            "mint",
            "recipient_wallet",
            "recipient_mint",
            "mint_quote_id",
        ],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::WalletPay,
        Capability::WalletControl,
        action,
    )
}

pub fn wallet_quote_claim(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::WalletQuoteClaim(serde_json::from_value(fields(
        &request,
        &["wallet", "mint", "mint_quote_id", "timeout_seconds"],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::WalletQuoteClaim,
        Capability::WalletControl,
        action,
    )
}

pub fn wallet_round_trip(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let action = CellAction::WalletRoundTrip(serde_json::from_value(fields(
        &request,
        &[
            "wallet",
            "mint",
            "payer_lightning",
            "amount_sat",
            "tolerance_sat",
        ],
    )?)?);
    submit(
        context,
        client,
        request,
        OperationKind::WalletRoundTrip,
        Capability::WalletControl,
        action,
    )
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

#[allow(
    clippy::needless_pass_by_value,
    reason = "backend fixtures share an owned request signature with the gate callback contract"
)]
pub fn quote_status(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let scope = scope(context, client, &request)?;
    Ok(
        json!({"last_observation":scope.store.wallet_quote_observation(&scope.workspace,&scope.principal,&scope.instance.id,expect::string(&request,"/wallet")?,expect::string(&request,"/mint")?,serde_json::from_value(request["direction"].clone())?,expect::string(&request,"/quote_id")?)?}),
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "backend fixtures share an owned request signature with the gate callback contract"
)]
pub fn quote_observations(
    context: &GateContext,
    client: &mut McpClient,
    request: Value,
) -> Result<Value> {
    let scope = scope(context, client, &request)?;
    let end = scope.store.wallet_quote_observation_max_sequence(
        &scope.workspace,
        &scope.principal,
        &scope.run,
    )?;
    let mut observations = Vec::new();
    let mut after = 0;
    loop {
        let page = scope.store.wallet_quote_observations(
            &scope.workspace,
            &scope.principal,
            &scope.run,
            after,
            end,
            100,
        )?;
        let Some(last) = page.last() else {
            break;
        };
        after = last.observation_sequence;
        observations.extend(page);
    }
    Ok(json!({"last_observations":observations,"next_cursor":null}))
}
