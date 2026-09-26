//! One versioned definition shared by prompts, scope, controls and scoring.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub version: String,
    pub suite: String,
    pub scorer: String,
    pub prompt: String,
    pub cell_name: String,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Amounts {
    pub mint_sat: u64,
    pub melt_sat: u64,
    pub maximum_fee_sat: u64,
}
impl Task {
    pub fn remaining(&self) -> std::ops::RangeInclusive<u64> {
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
    pub fn component(&self, implementation: &str, ordinal: usize) -> &str {
        self.components
            .iter()
            .filter(|c| c["implementation"] == implementation)
            .nth(ordinal)
            .expect("task role")["id"]
            .as_str()
            .expect("component id")
    }
    pub fn mint_url(&self) -> String {
        format!("http://{}:3338", self.component("cdk", 0))
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
            id:"O1".into(), version:"0.4".into(), suite:"operate".into(), scorer:"o1-70-15-15/0.4".into(),
            prompt:String::new(), cell_name:document["name"].as_str().unwrap().into(),
            components:document["components"].as_array().unwrap().clone(), links:document["links"].as_array().unwrap().clone(),
            policy:document["policy"].clone(),
            amounts:Amounts { mint_sat:1000, melt_sat:100, maximum_fee_sat:10 },
            allowed_tools:vec!["catalog_list".into(),"catalog_entry_read".into(),"catalog_config_schema_read".into(),"cell_plan".into(),"cell_read".into(),"cell_search".into(),"cell_up".into(),"cell_inspect".into(),"cell_wait".into(),"cell_exec".into(),"cell_remove".into(),"cell_component_status_list".into(),"cell_inventory_list".into(),"operation_status".into(),"operation_wait".into(),"operation_read".into(),"operation_cancel".into(),"activity_search".into(),"benchmark_checkpoint".into()],
            target_seconds:300.0, deadline_seconds:1200, timing_calibrated:false,
            operational_required:assertions.iter().filter(|(id,_)| id != "report").map(|(id,_)| id.clone()).collect(),
            assertions, report_schema:Value::Null, score_weights:[70,15,15],
            rules:json!({"report":"Exactly one JSON object earns format credit. A single trailing JSON object after prose can validate claims. Duplicate keys, ambiguous or incorrect claims fail.","environment":"Cleanup and preservation required; otherwise accepted score/success are null.","tools":"All failures count; success deduplicated by semantic arguments excluding request IDs. Read/discovery cap 3; other calls cap 1. No expected-negative calls.","payment_flow":"Exactly one successful payer funding payment and one settled recipient invoice; no offsetting cycles. Retain terminal evidence before removal."}),
        };
        task.report_schema = json!({"type":"object","additionalProperties":false,"required":["success","minted_sat","paid_sat","remaining_sat","cleanup"],"properties":{"success":{"type":"boolean","const":true},"minted_sat":{"type":"integer","const":task.amounts.mint_sat},"paid_sat":{"type":"integer","const":task.amounts.melt_sat},"remaining_sat":{"type":"integer","minimum":*task.remaining().start(),"maximum":*task.remaining().end()},"cleanup":{"type":"boolean","const":true}}});
        task.prompt = prompt(&task);
        task
    })
}
fn prompt(task: &Task) -> String {
    format!(
        r"Complete {id} autonomously using only Proofstorm MCP tools. Create exactly one cell named {cell_name}. Use this topology, including exact component versions and config versions; do not add components or links: {document}
Discover public schemas as needed. Use native component commands through cell_exec; host filesystem, shell and web tools are unavailable. Do not write or execute Python scripts.
Fund a direct channel between the two LND nodes with liquidity in both directions. Mine maturity and channel confirmations explicitly. Initialize {wallet} at {mint_url}, obtain a {mint}-sat mint quote, pay from {payer}, and claim exactly {mint} sat. Call benchmark_checkpoint(stage=funded,mint_quote_id=...) before spending. Create a fresh {melt}-sat BOLT11 invoice on {payer} and melt wallet ecash to it. Verify recipient settlement and remaining balance, then call benchmark_checkpoint(stage=paid,mint_quote_id=...,melt_quote_id=...,payment_hash=...,minted_sat={mint},paid_sat={melt},remaining_sat=...). Total fees must be at most {fee} sat. Quote and payment identities must agree with your observations.
Checkpoints retain observations; they neither perform payments nor award success. Both successful checkpoints must precede cell removal. There must be exactly one successful {mint}-sat funding payment and one settled {melt}-sat receipt on {payer}; no offsetting mint/melt cycles. Resolve every native operation before removing the cell, then wait for verified closure with its original instance key. Keep evidence private; do not print seeds or credentials.
Finish with exactly one JSON object, no prose or fence, matching this report schema: {schema}. If anything fails, attempt cleanup and report success:false honestly. You have {deadline} seconds. No human continuation or delegation.
Scoring separates operational completion from formatting. Extra prose before one trailing JSON object loses reporting points. Missing, ambiguous, duplicate-key or incorrect structured claims fail report validation. Environment validity is checked independently by the runner.
",
        id = task.id,
        cell_name = task.cell_name,
        document = task.document(),
        wallet = task.component("nutshell-wallet", 0),
        mint_url = task.mint_url(),
        payer = task.component("lnd", 1),
        mint = task.amounts.mint_sat,
        melt = task.amounts.melt_sat,
        fee = task.amounts.maximum_fee_sat,
        schema = task.report_schema,
        deadline = task.deadline_seconds
    )
}
