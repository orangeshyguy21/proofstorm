//! One versioned definition shared by prompts, scope, controls and scoring.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub version: String,
    pub suite: String,
    pub scorer: String,
    pub prompt: String,
    pub cell_name: String,
    pub roles: BTreeMap<String, String>,
    pub payment_expectation: PaymentExpectation,
    pub final_checkpoint: String,
    pub components: Vec<Value>,
    pub links: Vec<Value>,
    pub policy: Value,
    pub amounts: Amounts,
    pub allowed_tools: Vec<String>,
    pub target_seconds: f64,
    pub deadline_seconds: u32,
    pub timing_calibrated: bool,
    pub assertions: Vec<(String, u32)>,
    pub operational_required: Vec<String>,
    pub report_schema: Value,
    pub rules: Value,
    pub score_weights: [u32; 3],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentExpectation {
    Settled,
    UnpaidNoRoute,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Amounts {
    pub mint_sat: u64,
    pub melt_sat: u64,
    pub maximum_fee_sat: u64,
}
impl Task {
    pub fn remaining(&self) -> std::ops::RangeInclusive<u64> {
        if self.payment_expectation == PaymentExpectation::UnpaidNoRoute {
            return self.amounts.mint_sat..=self.amounts.mint_sat;
        }
        let maximum = self
            .amounts
            .mint_sat
            .checked_sub(self.amounts.melt_sat)
            .expect("task amounts");
        maximum
            .checked_sub(self.amounts.maximum_fee_sat)
            .expect("task fee")..=maximum
    }
    pub fn allowed(&self, tool: &str) -> bool {
        self.allowed_tools.iter().any(|name| name == tool)
    }
    pub fn document(&self) -> Value {
        json!({"api_version":"proofstorm/v1alpha1","name":self.cell_name,"components":self.components,"links":self.links,"policy":self.policy})
    }
    pub fn role(&self, name: &str) -> &str {
        self.roles.get(name).expect("declared task role")
    }
    pub fn mint_url(&self) -> String {
        format!("http://{}:3338", self.role("mint"))
    }
}

pub fn o1() -> &'static Task {
    static TASK: OnceLock<Task> = OnceLock::new();
    TASK.get_or_init(|| {
        let components = [
        ("chain","bitcoin","bitcoin-core","31.1","bitcoin-core/31/v1","cell"),
        ("mint-lnd","lightning","lnd","0.21.3-beta","lnd/0.20/v1","cell"),
        ("payer-lnd","lightning","lnd","0.21.3-beta","lnd/0.20/v1","cell"),
        ("mint","mint","cdk","0.18.1","cdk-mintd/0.18/v1","target"),
        ("wallet","wallet","nutshell-wallet","0.21.0","nutshell-wallet/0.20/v1","cell")
    ].map(|(id,kind,implementation,version,config_version,control)|json!({"id":id,"kind":kind,"implementation":implementation,"version":version,"config_version":config_version,"control":control,"config":if id=="mint" {json!({"input_fee_ppk":0})} else {json!({})}}));
        let document = json!({"api_version":"proofstorm/v1alpha1","name":"benchmark-o1","components":components,"links":[
        {"id":"mint-chain","kind":"chain_backend","from":"mint-lnd","to":"chain","binding":{"type":"chain","network":"regtest"}},
        {"id":"payer-chain","kind":"chain_backend","from":"payer-lnd","to":"chain","binding":{"type":"chain","network":"regtest"}},
        {"id":"mint-backend","kind":"payment_backend","from":"mint","to":"mint-lnd","binding":{"type":"payment","method":"bolt11","unit":"sat"}}
    ],"policy":{"allow":[],"limits":{"max_components":8,"max_links":8,"max_config_bytes":16384}}});
        let assertions: Vec<(String,u32)> = [
    ("components", 10),
    ("bindings", 10),
    ("mint_settled", 10),
    ("recipient_settled", 15),
    ("accounting", 10),
    ("terminal", 10),
    ("evidence", 10),
    ("report", 10),
    ("autonomy", 5),
    ("agent_cleanup", 10),
].into_iter().map(|(name, weight)| (name.into(),weight)).collect();
        let mut task = Task {
            id:"O1".into(), version:"0.6".into(), suite:"operate".into(), scorer:"o1-70-15-15/0.6".into(),
            prompt:String::new(), cell_name:document["name"].as_str().unwrap().into(),
            roles:[("chain","chain"),("backend","mint-lnd"),("payer","payer-lnd"),("recipient","payer-lnd"),("mint","mint"),("wallet","wallet")].into_iter().map(|(k,v)|(k.into(),v.into())).collect(),
            payment_expectation:PaymentExpectation::Settled, final_checkpoint:"paid".into(),
            components:document["components"].as_array().unwrap().clone(), links:document["links"].as_array().unwrap().clone(),
            policy:document["policy"].clone(),
            amounts:Amounts { mint_sat:1000, melt_sat:100, maximum_fee_sat:10 },
            allowed_tools:vec!["catalog_list".into(),"catalog_entry_read".into(),"catalog_config_schema_read".into(),"cell_plan".into(),"cell_read".into(),"cell_search".into(),"cell_up".into(),"cell_inspect".into(),"cell_wait".into(),"cell_exec".into(),"cell_remove".into(),"cell_component_status_list".into(),"cell_inventory_list".into(),"operation_status".into(),"operation_wait".into(),"operation_read".into(),"operation_cancel".into(),"activity_search".into(),"benchmark_checkpoint".into()],
            target_seconds:300.0, deadline_seconds:1200, timing_calibrated:false,
            operational_required:assertions.iter().filter(|(id,_)| id != "report").map(|(id,_)| id.clone()).collect(),
            assertions, report_schema:Value::Null, score_weights:[70,15,15],
            rules:json!({"composition":"Discover schemas and compose the cell from semantic requirements; no complete cell document is supplied. Component identity and semantic bindings are scored, not a particular configuration schema version.","report":"Exactly one JSON object earns format credit. A single trailing JSON object after prose can validate claims. The schema defines shape only; claims must match independent observations, including success and cleanup. Missing evidence cannot validate a claim. Duplicate keys, ambiguous or incorrect claims fail.","environment":"Cleanup and preservation required; otherwise accepted score/success are null.","tools":"All failures count; success deduplicated by semantic arguments excluding request IDs. Read/discovery cap 3; other calls cap 1. No expected-negative calls.","payment_flow":"Exactly one successful payer funding payment and one settled recipient invoice; no offsetting cycles. Retain terminal evidence before removal."}),
        };
        task.report_schema = json!({"type":"object","additionalProperties":false,"required":["success","minted_sat","paid_sat","remaining_sat","cleanup"],"properties":{"success":{"type":"boolean"},"minted_sat":{"type":"integer"},"paid_sat":{"type":"integer"},"remaining_sat":{"type":"integer"},"cleanup":{"type":"boolean"}}});
        task.prompt = prompt(&task);
        task
    })
}
/// Only currently implemented contracts are regradable by this binary.
pub fn lookup(id: &str, version: &str) -> Option<&'static Task> {
    [o1(), o5()]
        .into_iter()
        .find(|task| task.id == id && task.version == version)
}

pub fn o5() -> &'static Task {
    static TASK: OnceLock<Task> = OnceLock::new();
    TASK.get_or_init(|| {
        let mut task = o1().clone();
        task.id = "O5".into();
        task.version = "0.1".into();
        task.scorer = "o5-70-15-15/0.1".into();
        task.cell_name = "benchmark-o5".into();
        task.payment_expectation = PaymentExpectation::UnpaidNoRoute;
        task.final_checkpoint = "evaluated".into();
        task.amounts.maximum_fee_sat = 0;
        task.roles.insert("recipient".into(), "island-lnd".into());
        let mut recipient = task.components.iter().find(|c| c["id"] == task.role("payer")).unwrap().clone();
        recipient["id"] = json!(task.role("recipient"));
        task.components.push(recipient);
        task.links.push(json!({"id":"island-chain","kind":"chain_backend","from":task.role("recipient"),"to":task.role("chain"),"binding":{"type":"chain","network":"regtest"}}));
        task.assertions = [("components",10),("bindings",10),("mint_settled",10),("payment_attempt",10),("recipient_unpaid",10),("accounting",10),("no_route",10),("terminal",5),("evidence",5),("report",10),("autonomy",5),("agent_cleanup",5)].into_iter().map(|(id,weight)|(id.into(),weight)).collect();
        task.operational_required = task.assertions.iter().filter(|(id,_)| id != "report").map(|(id,_)|id.clone()).collect();
        task.report_schema["properties"]["payment_occurred"] = json!({"type":"boolean"});
        task.report_schema["properties"]["diagnosis"] = json!({"type":"string"});
        task.report_schema["required"].as_array_mut().unwrap().extend([json!("payment_occurred"),json!("diagnosis")]);
        task.rules["payment_flow"] = json!("Exactly one funding payment and one attempted melt to an isolated recipient. Record the failed backend payment with the invoice hash and NO_ROUTE reason, an unpaid mint/wallet quote, zero consumed proofs/fees, full unreserved wallet balance, and no recipient channels or settlement. Never-attempted, pending, missing or conflicting evidence fails.");
        task.rules["tools"] = json!("Payment refusal is the expected economic outcome, not a tool error. A successful tool receipt reporting a failed native payment remains a successful tool call. All actual MCP/harness errors count as failures without exemptions; successes are deduplicated with the same caps as O1.");
        task.prompt = prompt(&task);
        task
    })
}

fn prompt(task: &Task) -> String {
    let components = task
        .components
        .iter()
        .map(|component| {
            format!(
                "{}: {} role, implementation {} version {}",
                component["id"].as_str().unwrap(),
                component["kind"].as_str().unwrap(),
                component["implementation"].as_str().unwrap(),
                component["version"].as_str().unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let links = task
        .links
        .iter()
        .map(|link| {
            let binding = &link["binding"];
            let requirement = if binding["type"] == "chain" {
                format!("network {}", binding["network"].as_str().unwrap())
            } else {
                format!(
                    "method {} in {}",
                    binding["method"].as_str().unwrap(),
                    binding["unit"].as_str().unwrap()
                )
            };
            format!(
                "{} -> {} using {} ({requirement})",
                link["from"].as_str().unwrap(),
                link["to"].as_str().unwrap(),
                link["kind"].as_str().unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let payment_instructions = if task.payment_expectation == PaymentExpectation::Settled {
        format!(
            "Fund a direct channel between {backend} and {payer} with liquidity in both directions. Mine maturity and channel confirmations explicitly. Initialize {wallet} at {mint_url}, obtain a {mint}-sat mint quote, pay from {payer}, and claim exactly {mint} sat. Call benchmark_checkpoint(stage=funded,mint_quote_id=...) before spending. Create a fresh {melt}-sat BOLT11 invoice on {recipient} and melt wallet ecash to it. Verify recipient settlement and remaining balance, then call benchmark_checkpoint(stage={stage},mint_quote_id=...,melt_quote_id=...,payment_hash=...,minted_sat={mint},paid_sat={melt},remaining_sat=...). Total fees must be at most {fee} sat. Quote and payment identities must agree with observations. Both successful checkpoints must precede removal. Exactly one funding payment and one settled recipient invoice are allowed; no offsetting cycles.",
            backend = task.role("backend"),
            payer = task.role("payer"),
            recipient = task.role("recipient"),
            wallet = task.role("wallet"),
            mint_url = task.mint_url(),
            mint = task.amounts.mint_sat,
            melt = task.amounts.melt_sat,
            fee = task.amounts.maximum_fee_sat,
            stage = task.final_checkpoint
        )
    } else {
        format!(
            "Fund a direct channel only between {backend} and {payer}. Leave {recipient} isolated: never open, close or fund channels involving it. Configure the mint with zero input fees. Mine maturity and confirmations explicitly. Initialize {wallet} at {mint_url}, mint exactly {mint} sat with one funding payment from {payer}, and call benchmark_checkpoint(stage=funded,mint_quote_id=...) BEFORE the attempted melt. Then create one fresh {melt}-sat BOLT11 invoice on {recipient} and actually attempt to melt wallet ecash to it. Investigate whether payment occurred. Retain the original mint/melt quote and invoice identities, inspect terminal wallet/mint quote state, recipient settlement, wallet balance, and the backend payment failure reason. Do not repair the route or substitute another recipient. Merely requesting a quote is not a payment attempt. Wait for a terminal outcome and restore any reserved proofs through supported wallet recovery if needed. Call benchmark_checkpoint(stage={stage},mint_quote_id=...,melt_quote_id=...,payment_hash=...,minted_sat=...,paid_sat=...,remaining_sat=...) before removal. Both checkpoints retain independent evidence; neither performs payments nor awards success. An unpaid terminal melt with NO_ROUTE, zero consumed proofs/fees and the full unreserved balance is the expected result. Report success:true if the investigation and cleanup completed, payment_occurred:false, paid_sat:0 and diagnosis:no_route when the evidence supports those conclusions. A pending payment or missing evidence is not a successful negative result. Tool receipt success is distinct from payment success; actual tool errors still count as failures.",
            backend = task.role("backend"),
            payer = task.role("payer"),
            recipient = task.role("recipient"),
            wallet = task.role("wallet"),
            mint_url = task.mint_url(),
            mint = task.amounts.mint_sat,
            melt = task.amounts.melt_sat,
            stage = task.final_checkpoint
        )
    };
    format!(
        r"Complete {id} autonomously using only Proofstorm MCP tools. Create exactly one cell named {cell_name}. Compose these components using the specified IDs, roles, implementations and versions: {components}. Required links: {links}. Do not add components or links. Discover the supported configuration schemas, control settings and link bindings through the catalog, then construct the cell document yourself.
Discover public schemas as needed. Use native component commands through cell_exec; host filesystem, shell and web tools are unavailable. Do not write or execute Python scripts.
{payment_instructions}
Resolve every native operation before removing the cell, then wait for verified closure with its original instance key. Keep evidence private; do not print seeds or credentials.
Finish with exactly one JSON object, no prose or fence, matching this shape-only report schema: {schema}. Report the actual amounts observed, whether the task completed, and whether cleanup was verified. If the task cannot be completed, attempt cleanup and report success:false honestly; a failure report is permitted by the schema. Do not invent values when evidence is missing. You have {deadline} seconds. No human continuation or delegation.
Scoring separates operational completion from formatting. Extra prose before one trailing JSON object loses reporting points. Missing, ambiguous, duplicate-key or incorrect structured claims fail report validation. Environment validity is checked independently by the runner.
",
        id = task.id,
        cell_name = task.cell_name,
        components = components,
        links = links,
        payment_instructions = payment_instructions,
        schema = task.report_schema,
        deadline = task.deadline_seconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_and_roles_are_explicit_and_order_independent() {
        for task in [o1(), o5()] {
            assert!(lookup(&task.id, &task.version).is_some());
            assert!(lookup(&task.id, "unknown").is_none());
            assert_eq!(
                task.assertions
                    .iter()
                    .map(|(_, weight)| weight)
                    .sum::<u32>(),
                100
            );
            for id in task.roles.values() {
                assert_eq!(
                    task.components
                        .iter()
                        .filter(|component| component["id"] == *id)
                        .count(),
                    1
                );
            }
            let mut reordered = task.clone();
            reordered.components.reverse();
            assert_eq!(reordered.role("recipient"), task.role("recipient"));
            assert_eq!(reordered.mint_url(), task.mint_url());
        }
        assert_ne!(o5().role("recipient"), o5().role("payer"));
        assert_eq!(o1().role("recipient"), o1().role("payer"));
        assert!(lookup("O9", &o1().version).is_none());
        assert_eq!(o5().remaining(), 1000..=1000);
    }

    #[test]
    fn prompt_requires_composition_without_disclosing_the_cell_payload() {
        let task = o1();
        for forbidden in [
            "api_version",
            "config_version",
            "input_fee_ppk",
            "\"control\"",
            "max_components",
        ] {
            assert!(!task.prompt.contains(forbidden), "{forbidden}");
        }
        for component in &task.components {
            for key in ["id", "kind", "implementation", "version"] {
                assert!(task.prompt.contains(component[key].as_str().unwrap()));
            }
        }
        for link in &task.links {
            assert!(!task.prompt.contains(link["id"].as_str().unwrap()));
            assert!(task.prompt.contains(link["kind"].as_str().unwrap()));
        }
        let mut changed = task.clone();
        changed.components[0]["version"] = json!("new-version");
        changed.links[0]["binding"]["network"] = json!("other-network");
        let instructions = prompt(&changed);
        assert!(instructions.contains("new-version") && instructions.contains("other-network"));
        assert!(task.document()["components"][0]["config_version"].is_string());
    }

    #[test]
    fn public_report_schema_contains_shape_only() {
        for property in o1().report_schema["properties"]
            .as_object()
            .unwrap()
            .values()
        {
            assert_eq!(property.as_object().unwrap().len(), 1);
            assert!(matches!(
                property["type"].as_str(),
                Some("boolean" | "integer" | "string")
            ));
        }
    }
}
