//! Same setup/retry/on-demand/CLI/MCP checks for checkout and bundle artifacts.
use crate::GateContext;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, path::Path};

pub fn hash(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

pub fn runtime(context: &GateContext) -> Result<Value> {
    let deployment =
        context
            .kubectl
            .get_json(&["get", "deployment/proofstormd", "-n", "proofstorm-system"])?;
    let pods = context.kubectl.get_json(&[
        "get",
        "pods",
        "-n",
        "proofstorm-system",
        "-l",
        "app.kubernetes.io/name=proofstormd",
    ])?;
    let cells = context
        .kubectl
        .get_json(&["get", "proofstormcells", "-A"])?;
    let mut controllers: Vec<_> = pods["items"]
        .as_array()
        .context("controller pods missing")?
        .iter()
        .map(|pod| json!([pod["metadata"]["uid"], pod["status"]["containerStatuses"]]))
        .collect();
    controllers.sort_by_cached_key(Value::to_string);
    let mut cells: Vec<_> = cells["items"]
        .as_array()
        .context("cell inventory missing")?
        .iter()
        .map(|cell| json!([cell["metadata"]["uid"], cell["spec"]]))
        .collect();
    cells.sort_by_cached_key(Value::to_string);
    ensure!(
        deployment["status"]["readyReplicas"] == 1,
        "controller not ready"
    );
    Ok(
        json!({"uid":deployment["metadata"]["uid"],"generation":deployment["metadata"]["generation"],
        "image":deployment["spec"]["template"]["spec"]["containers"][0]["image"],"controllers":controllers,"cells":cells}),
    )
}

fn repositories(context: &GateContext) -> Result<BTreeSet<String>> {
    let value = crate::http::get_json(&format!(
        "http://{}/v2/_catalog",
        context.installation.host_registry()
    ))?;
    Ok(serde_json::from_value(value["repositories"].clone())?)
}

pub fn run(context: &GateContext) -> Result<()> {
    eprintln!("Checking unchanged setup and controller reuse...");
    let before = runtime(context)?;
    let identity = fs::read(context.installation.home.join("installation.json"))?;
    let owner = fs::read(context.installation.home.join("runtime-owner.json"))?;
    let database = hash(context.database())?;
    let local_receipt = context.installation.home.join("checkout-controller.json");
    let controller_receipt = local_receipt
        .exists()
        .then(|| fs::read(&local_receipt))
        .transpose()?;
    ensure!(
        context.cli(&["setup", "--allow-development"])?["ready"] == true,
        "setup retry failed"
    );
    ensure!(context.cli(&["doctor"])?["ok"] == true, "doctor failed");
    ensure!(
        before == runtime(context)?,
        "unchanged setup replaced controller or cells"
    );
    ensure!(
        identity == fs::read(context.installation.home.join("installation.json"))?
            && owner == fs::read(context.installation.home.join("runtime-owner.json"))?
            && database == hash(context.database())?,
        "setup retry changed identity or permissions"
    );
    if let Some(receipt) = controller_receipt {
        ensure!(
            receipt == fs::read(local_receipt)?,
            "setup rewrote controller receipt"
        );
    }
    let baseline = repositories(context)?;
    ensure!(
        baseline.iter().all(|name| name == "proofstormd"),
        "onboarding must be the first gate; setup prefetched workload images"
    );
    let mut example: Value =
        serde_json::from_str(include_str!("../../../../examples/developer-cell.json"))?;
    let mut bitcoin = example.clone();
    bitcoin["name"] = json!("onboarding-bitcoin");
    bitcoin["components"] = json!([example["components"][0]]);
    bitcoin["links"] = json!([]);
    let path = context.work().join("onboarding-bitcoin.json");
    fs::write(&path, serde_json::to_vec_pretty(&bitcoin)?)?;
    eprintln!("Creating the CLI Bitcoin cell and checking on-demand images...");
    let first = context.cli(&[
        "up",
        path.to_str().context("fixture path")?,
        "--wait",
        "120",
    ])?;
    ensure!(first["runtime"]["phase"] == "ready", "CLI cell not ready");
    let selected = repositories(context)?;
    let added: BTreeSet<_> = selected.difference(&baseline).map(String::as_str).collect();
    ensure!(
        added == BTreeSet::from(["bitcoin-core", "upstream/docker.io/library/busybox"]),
        "CLI fetched unexpected images: {added:?}"
    );
    let caps: Vec<String> = serde_json::from_value(json!(proofstorm_app::developer::CAPABILITIES))?;
    let mut client = context.session(
        proofstorm_app::config::DEFAULT_WORKSPACE,
        "onboarding",
        &caps.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    example["name"] = json!("onboarding-mcp");
    eprintln!("Creating the MCP Bitcoin/CDK cell and waiting for readiness...");
    let result = client.call("cell_up", json!({"name":"onboarding-mcp","cell":example}))?;
    ensure!(result.get("cell").is_some(), "MCP did not return cell data");
    crate::cell::wait_ready(&mut client, "onboarding-mcp")?;
    let images = repositories(context)?;
    ensure!(
        images
            .difference(&selected)
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == BTreeSet::from(["cdk-mint-management"]),
        "MCP fetched unexpected images"
    );
    let status = context.cli(&["status", "onboarding-mcp"])?;
    ensure!(
        status["cell"]["name"] == "onboarding-mcp",
        "CLI did not read MCP's cell"
    );
    // Exercise GUI reuse/stop while real cell workloads exist, without a browser.
    eprintln!("Checking managed GUI while cells are running...");
    super::gui::run(context)?;
    for name in ["onboarding-bitcoin", "onboarding-mcp"] {
        eprintln!("Removing test cell {name}...");
        context.cli(&["rm", name])?;
    }
    context.kubectl.assert_no_instance_namespaces()?;
    context.kubectl.assert_no_cell_actions()?;
    context.record("onboarding.json", &json!({"passed":true,"setup_retry_preserves_controller_and_permissions":true,
        "cli_and_mcp_on_demand_images":true,"gui_stop_preserves_cells":true,"cell_cleanup":true,"model_tool_call":false}))
}
