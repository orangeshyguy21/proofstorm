//! Qualify the identity provider independently of unsupported mint authentication.
use std::io::Write;

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

use crate::{GateContext, cell, gate::CONTROL_NAMESPACE, http, json as expect};

const INSTANCE: &str = "keycloak-instance";
const REALM: &str = "/realms/proofstorm";

fn document() -> Value {
    json!({
        "api_version":"proofstorm/v1alpha1", "name":"keycloak-provider",
        "components":[
            {"id":"identity-db","kind":"database","implementation":"postgresql","version":"17.11","config_version":"postgresql/17/v1","control":"cell","config":{"database_name":"keycloak","storage_size":"2Gi"}},
            {"id":"identity","kind":"identity_provider","implementation":"keycloak","version":"25.0.6","config_version":"keycloak/25/v1","control":"cell","config":{"access_token_lifespan_seconds":600}}
        ],
        "links":[{"id":"identity-database","kind":"database_backend","from":"identity","to":"identity-db","binding":{"type":"database","role":"primary"}}],
        "policy":{"allow":[],"limits":{"max_components":8,"max_links":16,"max_config_bytes":16384}}
    })
}

fn credential(context: &GateContext, namespace: &str, key: &str) -> Result<String> {
    context.kubectl.run(&[
        "get",
        "secret/identity-credentials",
        "-n",
        namespace,
        "-o",
        &format!("go-template={{{{index .data {key:?} | base64decode}}}}"),
    ])
}

fn login(context: &GateContext, url: &str, username: &str, password: &str) -> Result<(u16, Value)> {
    // Credentials stay in private files, never subprocess arguments or errors.
    let mut user = tempfile::NamedTempFile::new_in(context.work())?;
    let mut secret = tempfile::NamedTempFile::new_in(context.work())?;
    user.write_all(username.as_bytes())?;
    secret.write_all(password.as_bytes())?;
    let response = http::curl(&[
        "--silent",
        "--show-error",
        "--max-time",
        "15",
        "--data-urlencode",
        "grant_type=password",
        "--data-urlencode",
        "client_id=cashu-client",
        "--data-urlencode",
        "scope=openid",
        "--data-urlencode",
        &format!("username@{}", user.path().display()),
        "--data-urlencode",
        &format!("password@{}", secret.path().display()),
        "--write-out",
        "\n%{http_code}",
        url,
    ])?;
    let (body, status) = response
        .rsplit_once('\n')
        .context("missing login HTTP status")?;
    Ok((
        status.parse()?,
        serde_json::from_str(body).context("invalid login response")?,
    ))
}

fn observe(context: &GateContext, namespace: &str) -> Result<Value> {
    let mut forward =
        http::PortForward::open(&context.kubectl, namespace, "service/identity", 8080)?;
    let discovery = http::get_json_retrying(
        &mut forward,
        &format!("{REALM}/.well-known/openid-configuration"),
        30,
    )?;
    let issuer = format!("http://identity:8080{REALM}");
    ensure!(discovery["issuer"] == issuer, "unexpected OIDC issuer");
    ensure!(
        discovery["token_endpoint"] == format!("{issuer}/protocol/openid-connect/token"),
        "unexpected OIDC token endpoint"
    );
    let mut keys = http::get_json(&forward.url(&format!("{REALM}/protocol/openid-connect/certs")))?;
    ensure!(
        keys["keys"].as_array().is_some_and(|keys| !keys.is_empty()),
        "OIDC signing keys missing"
    );
    // Discovery may enumerate the same signing keys in a different order.
    keys["keys"]
        .as_array_mut()
        .context("invalid OIDC keys")?
        .sort_by_cached_key(|key| key["kid"].as_str().unwrap_or_default().to_owned());
    let username = credential(context, namespace, "OIDC_TEST_USERNAME")?;
    let password = credential(context, namespace, "OIDC_TEST_PASSWORD")?;
    let url = forward.url(&format!("{REALM}/protocol/openid-connect/token"));
    let (status, rejected) = login(context, &url, &username, "incorrect-password")?;
    ensure!(
        status == 401 && rejected["error"] == "invalid_grant",
        "invalid OIDC password was not rejected"
    );
    let (status, accepted) = login(context, &url, &username, &password)?;
    ensure!(
        status == 200,
        "generated OIDC credentials were not accepted"
    );
    let token = accepted["access_token"]
        .as_str()
        .context("OIDC access token missing")?;
    let payload = token
        .split('.')
        .nth(1)
        .context("OIDC token payload missing")?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
    ensure!(
        claims["iss"] == issuer && claims["azp"] == "cashu-client",
        "unexpected OIDC token claims"
    );
    ensure!(
        expect::integer(&claims, "/exp")? - expect::integer(&claims, "/iat")? == 600,
        "unexpected OIDC token lifetime"
    );
    let subject = expect::string(&claims, "/sub")?;
    ensure!(!subject.is_empty(), "OIDC subject missing");
    Ok(json!({"subject":subject,"keys":keys}))
}

pub fn run(context: &GateContext) -> Result<()> {
    context.qualification_stage("materialize")?;
    let mut client = context.default_session("keycloak-provider", "designer")?;
    let preview = client.call("cell_plan", json!({"name":INSTANCE,"cell":context.document(document())?,"request_id":"create-keycloak"}))?;
    cell::apply(&mut client, &preview)?;
    let ready = cell::wait_ready_recorded(context, &mut client, INSTANCE)?;
    let namespace = expect::string(&ready, "/instance_namespace")?;
    context.qualification_stage("configuration")?;
    let before = observe(context, namespace)?;
    let secrets = [
        "secret/identity-credentials",
        "secret/identity-db-credentials",
    ];
    let digests = secrets.map(|secret| {
        context
            .kubectl
            .digest(&["get", secret, "-n", namespace, "-o", "jsonpath={.data}"])
    });
    let [identity_digest, database_digest] = digests;
    let digests = [identity_digest?, database_digest?];
    context.qualification_stage("restart")?;
    context
        .kubectl
        .rollout_restart(CONTROL_NAMESPACE, "deployment/proofstormd")?;
    for target in ["statefulset/identity-db", "deployment/identity"] {
        context.kubectl.rollout_restart(namespace, target)?;
    }
    for (secret, digest) in secrets.into_iter().zip(digests) {
        ensure!(
            context
                .kubectl
                .digest(&["get", secret, "-n", namespace, "-o", "jsonpath={.data}"])?
                == digest,
            "OIDC restart changed generated credentials"
        );
    }
    ensure!(
        observe(context, namespace)? == before,
        "OIDC restart changed user identity or signing keys"
    );
    context.qualification_stage("teardown")?;
    client.call("cell_remove", json!({"name":INSTANCE}))?;
    cell::wait_closed(&mut client, INSTANCE)?;
    println!(
        "Keycloak discovery, login/rejection, PostgreSQL persistence, stable credentials and signing keys, and teardown passed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proofstorm_core::{CellSpec, resolve_lock};
    use proofstorm_qualification::{Identity, Scenario};

    #[test]
    fn identity_provider_fixture_uses_every_planned_platform_image() {
        let plan = proofstorm_qualification::plan(
            Identity {
                revision: "a".repeat(40),
                run_id: "0".into(),
                attempt: 1,
            },
            proofstorm_qualification::Mode::Compatibility,
        )
        .unwrap();
        let mut covered = std::collections::BTreeSet::new();
        for case in &plan.cases {
            let Scenario::Gate { name, versions } = &case.scenario else {
                continue;
            };
            if name != "keycloak" {
                continue;
            }
            covered.insert(case.platform.as_str());
            let mut fixture = document();
            let observer = crate::qualification::Observer::new(case.clone());
            observer.document(&mut fixture).unwrap();
            observer.finish().unwrap();
            let cell: CellSpec = serde_json::from_value(fixture).unwrap();
            let catalog = proofstorm_qualification::catalog(&case.platform).unwrap();
            for entry in resolve_lock(&cell, &catalog).unwrap().entries {
                assert_eq!(entry.version, versions[&entry.catalog_id]);
            }
        }
        assert_eq!(covered, ["linux/amd64", "linux/arm64"].into());
    }
}
