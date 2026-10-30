//! Shared lifecycle of the managed payment-stack gates (Bark and Spark): one
//! owned cell and run built from pinned catalog images, original-quote
//! reconciliation, component restarts, mTLS refusal probes and verified teardown.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    thread::sleep,
    time::Duration,
};

use crate::{GateContext, McpClient, cell, http, json as expect, native};

pub(crate) mod transport;

/// One managed cell, its run, and the names its evidence and failures carry.
#[derive(Clone, Copy)]
pub(crate) struct Stack {
    pub(crate) instance: &'static str,
    pub(crate) run: &'static str,
    /// Evidence file and request prefix, such as `bark`.
    pub(crate) prefix: &'static str,
    /// Name in failure messages, such as `Bark`.
    pub(crate) name: &'static str,
}

impl Stack {
    /// Plan the cell, refuse unpinned images, then exercise it inside one run.
    /// Teardown always runs, and both outcomes are recorded together.
    pub(crate) fn run(
        self,
        context: &GateContext,
        document: Value,
        pinned: &[&str],
        exercise: impl FnOnce(&mut McpClient, &str) -> Result<()>,
    ) -> Result<()> {
        let prefix = self.prefix;
        let mut client = context.default_session(&format!("{prefix}-managed"), "designer")?;
        let document = context.document(document)?;
        // Planning is deliberately ordinary catalog resolution. Missing image
        // entries must fail here, before any cell is materialized.
        let preview = client.call(
            "cell_plan",
            json!({"name":self.instance,"cell":document,"request_id":format!("{prefix}-plan")}),
        )?;
        let published = cell::review(&mut client, &preview)?;
        context.record(&format!("{prefix}-plan.json"), &published)?;
        self.check_pinned(&published, pinned)?;
        let result = (|| {
            cell::apply(&mut client, &preview)?;
            let ready = cell::wait_ready_recorded(context, &mut client, self.instance)?;
            context.record(&format!("{prefix}-ready.json"), &ready)?;
            let namespace = expect::string(&ready, "/instance_namespace")?;
            client.call(
                "run_start",
                json!({"name":self.instance,"run_id":self.run,"request_id":format!("{prefix}-run")}),
            )?;
            exercise(&mut client, namespace)
        })();
        let _ = client.call(
            "run_finish",
            json!({"run_id":self.run,"request_id":format!("{prefix}-finish")}),
        );
        let cleanup = self.cleanup(context, &mut client);
        context.record(&format!("{prefix}-result.json"), &json!({"passed":result.is_ok() && cleanup.is_ok(),"exercise_error":result.as_ref().err().map(|e|format!("{e:#}")),"cleanup_error":cleanup.as_ref().err().map(|e|format!("{e:#}"))}))?;
        cleanup?;
        result
    }

    /// Every lock entry of each listed implementation runs an immutable image
    /// with recorded source provenance.
    pub(crate) fn check_pinned(self, published: &Value, implementations: &[&str]) -> Result<()> {
        for implementation in implementations {
            cell::lock_entry(published, implementation)?;
            for entry in expect::array(published, "/lock/entries")?
                .iter()
                .filter(|entry| entry["catalog_id"] == *implementation)
            {
                ensure!(
                    expect::string(entry, "/image")?.contains("@sha256:"),
                    "{} requires immutable catalog images",
                    self.name
                );
                ensure!(
                    entry["build_provenance"].is_object(),
                    "{} requires source provenance",
                    self.name
                );
            }
        }
        Ok(())
    }

    fn cleanup(self, context: &GateContext, client: &mut McpClient) -> Result<()> {
        // A failed preliminary read must not prevent the removal attempt.
        let status = cell::status(client, self.instance);
        client.call("cell_remove", json!({"name":self.instance}))?;
        let closed = cell::wait_closed(client, self.instance)?;
        context.record(&format!("{}-cleanup.json", self.prefix), &closed)?;
        // This runner owns a disposable cluster. These reads never touch or demand
        // that unrelated Docker/Orchard installations be empty.
        context.kubectl.assert_no_instance_namespaces()?;
        context.kubectl.assert_no_cell_actions()?;
        let status =
            status.context("could not establish the namespace for storage verification")?;
        let namespace = expect::string(&status, "/instance_namespace")?;
        // PersistentVolumes outlive their namespace while reclamation is in
        // progress. Check only claims belonging to this removed instance.
        for _ in 0..60 {
            let volumes = context.kubectl.get_json(&["get", "persistentvolumes"])?;
            if !expect::array(&volumes, "/items")?
                .iter()
                .any(|v| v["spec"]["claimRef"]["namespace"] == namespace)
            {
                return context.record(
                    &format!("{}-storage-cleanup.json", self.prefix),
                    &json!({"namespace":namespace,"remaining_volumes":0}),
                );
            }
            sleep(Duration::from_secs(1));
        }
        anyhow::bail!("owned {} volumes remain after namespace removal", self.name)
    }

    pub(crate) fn control(
        self,
        client: &mut McpClient,
        tool: &str,
        component: &str,
        id: &str,
    ) -> Result<()> {
        client.call(
            tool,
            json!({"name":self.instance,"run_id":self.run,"request_id":id,"component":component}),
        )?;
        cell::wait_succeeded(client, id)?;
        Ok(())
    }

    pub(crate) fn balance(
        self,
        context: &GateContext,
        client: &mut McpClient,
        id: &str,
        expected: u64,
    ) -> Result<()> {
        let state = native::observe_wallet(
            client,
            "cdk-cli-wallet",
            &json!({"name":self.instance,"run_id":self.run,"request_id":id,"wallet":"wallet","mint":"mint"}),
        )?;
        check_balance(&state, expected)?;
        context.record(&format!("{id}.json"), &state)
    }

    /// CDK registers exactly the processor's advertised rails for minting and melting.
    pub(crate) fn registered(
        self,
        context: &GateContext,
        namespace: &str,
        advertised: &str,
    ) -> Result<()> {
        let mut forward =
            http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
        let info = http::get_json_retrying(&mut forward, "/v1/info", 30)?;
        context.record(&format!("{}-mint-info.json", self.prefix), &info)?;
        check_registered(&info, advertised)
    }

    /// Restart each component in order, then require unchanged identities and
    /// retained-state seals. `components` pairs each id with its pod count.
    pub(crate) fn restart_stack(
        self,
        context: &GateContext,
        client: &mut McpClient,
        namespace: &str,
        phase: &str,
        components: &[(&str, usize)],
        mut identities: impl FnMut(&mut McpClient, &str) -> Result<Value>,
    ) -> Result<()> {
        let before = identities(client, &format!("{phase}-before"))?;
        for (component, replicas) in components {
            self.restart(
                context,
                client,
                namespace,
                component,
                *replicas,
                &format!("{}-{phase}-restart-{component}", self.prefix),
            )?;
        }
        let after = identities(client, &format!("{phase}-after"))?;
        ensure!(
            before == after,
            "{} identities or retained state seals changed on restart",
            self.name
        );
        context.record(
            &format!("{}-{phase}-identities.json", self.prefix),
            &json!({"before":before,"after":after}),
        )
    }

    /// Restart one component and wait until every one of its `replicas` pods
    /// has been replaced by a ready pod.
    pub(crate) fn restart(
        self,
        context: &GateContext,
        client: &mut McpClient,
        namespace: &str,
        component: &str,
        replicas: usize,
        id: &str,
    ) -> Result<()> {
        let before =
            ready_pods(context, namespace, component, replicas)?.context(if replicas == 1 {
                "component has no single ready pod"
            } else {
                "component is missing ready pods"
            })?;
        self.control(client, "component_restart", component, id)?;
        for _ in 0..120 {
            if let Some(after) = ready_pods(context, namespace, component, replicas)?
                && after.iter().all(|pod| !before.contains(pod))
            {
                cell::wait_ready_recorded(context, client, self.instance)?;
                return context.record(
                    &format!("{id}.json"),
                    &json!({"component":component,"previous_pod_uid":uids(&before),"ready_pod_uid":uids(&after)}),
                );
            }
            sleep(Duration::from_secs(1));
        }
        anyhow::bail!(
            "{component} did not replace its {} and become ready",
            if replicas == 1 { "pod" } else { "pods" }
        )
    }

    /// Digests of identity Secrets and of each component's retained-state seal.
    pub(crate) fn fingerprints(
        self,
        context: &GateContext,
        client: &mut McpClient,
        namespace: &str,
        phase: &str,
        secrets: &[&'static str],
        seals: &[(&'static str, &str)],
    ) -> Result<BTreeMap<&'static str, String>> {
        let mut fingerprints = BTreeMap::new();
        for name in secrets {
            let secret =
                context
                    .kubectl
                    .get_json(&["get", &format!("secret/{name}"), "-n", namespace])?;
            ensure!(secret["data"].is_object(), "missing identity Secret data");
            fingerprints.insert(*name, proofstorm_core::digest_json(&secret["data"]));
        }
        for (component, marker) in seals {
            fingerprints.insert(
                *component,
                native::stdout(
                    client,
                    self.instance,
                    self.run,
                    component,
                    &format!("{}-{phase}-{component}-seal", self.prefix),
                    &format!("sha256sum {marker}"),
                )?,
            );
        }
        Ok(fingerprints)
    }
}

/// One pod uid as a string, several as a sorted array.
fn uids(pods: &[String]) -> Value {
    match pods {
        [pod] => json!(pod),
        pods => json!(pods),
    }
}

/// The uids of a component's ready pods, once exactly `replicas` are ready.
fn ready_pods(
    context: &GateContext,
    namespace: &str,
    component: &str,
    replicas: usize,
) -> Result<Option<Vec<String>>> {
    let pods = context.kubectl.get_json(&[
        "get",
        "pods",
        "-n",
        namespace,
        "-l",
        &format!("{}={component}", proofstorm_kube::COMPONENT_LABEL),
    ])?;
    let ready = expect::array(&pods, "/items")?
        .iter()
        .filter(|pod| {
            pod["metadata"]["deletionTimestamp"].is_null()
                && pod["status"]["conditions"]
                    .as_array()
                    .is_some_and(|conditions| {
                        conditions
                            .iter()
                            .any(|c| c["type"] == "Ready" && c["status"] == "True")
                    })
        })
        .collect::<Vec<_>>();
    if ready.len() != replicas {
        return Ok(None);
    }
    let mut uids = ready
        .iter()
        .map(|pod| Ok(expect::string(pod, "/metadata/uid")?.to_owned()))
        .collect::<Result<Vec<_>>>()?;
    uids.sort();
    Ok(Some(uids))
}

/// The mint's keysets, which must survive every restart unchanged.
pub(crate) fn mint_keysets(context: &GateContext, namespace: &str) -> Result<Value> {
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    let keysets = http::get_json_retrying(&mut forward, "/v1/keysets", 30)?;
    ensure!(
        !expect::array(&keysets, "/keysets")?.is_empty(),
        "missing mint keysets"
    );
    Ok(keysets)
}

pub(crate) fn check_balance(state: &Value, expected: u64) -> Result<()> {
    ensure!(
        state["balance_sat"] == expected
            && state["reserved_sat"] == 0
            && state["pending_sat"] == 0
            && state["pending_spent_sat"] == 0,
        "wallet does not conserve settled funds"
    );
    Ok(())
}

/// CDK registers every advertised rail, all in sat, for both minting and melting.
pub(crate) fn check_registered(info: &Value, advertised: &str) -> Result<()> {
    let expected = advertised.split(',').collect::<BTreeSet<_>>();
    for nut in ["4", "5"] {
        let mut methods = BTreeSet::new();
        for method in expect::array(info, &format!("/nuts/{nut}/methods"))? {
            ensure!(
                method["unit"] == "sat",
                "mint registered a non-sat rail: {method}"
            );
            ensure!(
                methods.insert(expect::string(method, "/method")?),
                "mint registered one rail twice: {method}"
            );
        }
        ensure!(
            methods == expected,
            "mint NUT-{nut:0>2} rails {methods:?} differ from the processor's {advertised}"
        );
    }
    Ok(())
}

pub(crate) fn quote_state(
    context: &GateContext,
    namespace: &str,
    kind: &str,
    id: &str,
    expected: &str,
) -> Result<Value> {
    quote_states(context, namespace, kind, id, &[expected])
}

pub(crate) fn quote_states(
    context: &GateContext,
    namespace: &str,
    kind: &str,
    id: &str,
    expected: &[&str],
) -> Result<Value> {
    let mut forward = http::PortForward::open(&context.kubectl, namespace, "service/mint", 3338)?;
    for _ in 0..90 {
        let state =
            http::get_json_retrying(&mut forward, &format!("/v1/{kind}/quote/bolt11/{id}"), 3)?;
        ensure!(state["quote"] == id, "quote identity changed");
        if expected.contains(&expect::string(&state, "/state")?) {
            return Ok(state);
        }
        sleep(Duration::from_secs(1));
    }
    anyhow::bail!("original {kind} quote did not reach {expected:?}")
}

/// Every melt quote a native `cdk-cli melt` log reports.
pub(crate) fn melt_quotes(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Quote ID: "))
        .collect()
}

pub(crate) fn melt_quote_id(output: &str) -> Result<&str> {
    let quotes = melt_quotes(output);
    ensure!(
        quotes.len() == 1 && !quotes[0].is_empty(),
        "missing or ambiguous native melt quote"
    );
    Ok(quotes[0])
}

/// The wallet balance left by a settled melt whose native receipt and mint
/// quote agree, with its fee inside the quote's reserve.
pub(crate) fn melt_remaining(
    receipt: &Value,
    quote: &Value,
    amount: u64,
    before: u64,
) -> Result<u64> {
    ensure!(
        receipt["state"] == "PAID"
            && receipt["amount_sat"] == amount
            && quote["state"] == "PAID"
            && quote["amount"] == amount,
        "melt receipt and quote disagree"
    );
    let fee = expect::integer(receipt, "/fee_paid_sat")?;
    ensure!(
        fee <= expect::integer(quote, "/fee_reserve")?,
        "melt exceeded its fee reserve"
    );
    before
        .checked_sub(amount)
        .and_then(|n| n.checked_sub(fee))
        .context("melt exceeded the wallet balance")
}

/// Keep a native wallet log before the gate removes the owned wallet/PVC.
/// Diagnostic capture does not replace or turn an execution failure into success.
pub(crate) fn capture_log(
    context: &GateContext,
    namespace: &str,
    label: &str,
    log: &str,
) -> Result<()> {
    let diagnostic = match context
        .kubectl
        .exec(namespace, "deployment/wallet", &["cat", log])
    {
        Ok(output) => json!({"output":output}),
        Err(error) => json!({"capture_error":format!("{error:#}")}),
    };
    context.record(&format!("{label}-native-log.private.json"), &diagnostic)
}

/// Exact inputs for preparing a gate's owned image cache without a runtime:
/// its cell's lock images and the runtime probe.
pub(crate) fn image_cache_inputs(mut images: Vec<String>) -> Result<Value> {
    images.push(proofstorm_kube::images::PROBE_IMAGE.into());
    images.sort();
    images.dedup();
    let inputs = images
        .into_iter()
        .map(|image| {
            let source =
                proofstorm_core::catalog_image_source(&image).map_err(anyhow::Error::msg)?;
            Ok(json!({"image":image,"source":source}))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!(inputs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passive_wallet_must_have_no_pending_or_reserved_value() {
        let state =
            json!({"balance_sat":69_993,"reserved_sat":0,"pending_sat":0,"pending_spent_sat":0});
        check_balance(&state, 69_993).unwrap();
        for field in [
            "balance_sat",
            "reserved_sat",
            "pending_sat",
            "pending_spent_sat",
        ] {
            let mut bad = state.clone();
            bad[field] = json!(1);
            assert!(check_balance(&bad, 69_993).is_err());
            bad.as_object_mut().unwrap().remove(field);
            assert!(check_balance(&bad, 69_993).is_err());
        }
    }

    #[test]
    fn settled_melt_conserves_amount_and_fee_within_its_reserve() {
        let receipt = json!({"state":"PAID","amount_sat":30_000,"fee_paid_sat":7});
        let quote = json!({"state":"PAID","amount":30_000,"fee_reserve":10});
        assert_eq!(
            melt_remaining(&receipt, &quote, 30_000, 100_000).unwrap(),
            69_993
        );
        assert!(melt_remaining(&receipt, &quote, 30_001, 100_000).is_err());
        assert!(melt_remaining(&receipt, &quote, 30_000, 30_001).is_err());
        let mut pending = quote.clone();
        pending["state"] = json!("PENDING");
        assert!(melt_remaining(&receipt, &pending, 30_000, 100_000).is_err());
        let mut over = receipt.clone();
        over["fee_paid_sat"] = json!(11);
        assert!(melt_remaining(&over, &quote, 30_000, 100_000).is_err());
        assert!(melt_quote_id("Quote ID: a\nQuote ID: b").is_err());
        assert!(melt_quote_id("no quote").is_err());
        assert_eq!(melt_quote_id("Quote ID: a").unwrap(), "a");
        assert!(melt_quotes("no quote").is_empty());
    }

    #[test]
    fn pinned_lock_requires_digest_and_provenance_for_every_entry() {
        let stack = Stack {
            instance: "i",
            run: "r",
            prefix: "p",
            name: "Test",
        };
        let entry = |image: &str| json!({"catalog_id":"node","image":image,"build_provenance":{}});
        let pinned = entry("registry/node@sha256:00");
        let published = json!({"lock":{"entries":[pinned.clone(),pinned.clone()]}});
        stack.check_pinned(&published, &["node"]).unwrap();
        assert!(stack.check_pinned(&published, &["other"]).is_err());
        for bad in [
            entry("registry/node:latest"),
            json!({"catalog_id":"node","image":"registry/node@sha256:00"}),
        ] {
            let published = json!({"lock":{"entries":[pinned.clone(),bad]}});
            assert!(stack.check_pinned(&published, &["node"]).is_err());
        }
    }

    #[test]
    fn restart_records_keep_a_single_pod_as_a_string() {
        assert_eq!(uids(&["a".into()]), json!("a"));
        assert_eq!(uids(&["a".into(), "b".into()]), json!(["a", "b"]));
    }
}
