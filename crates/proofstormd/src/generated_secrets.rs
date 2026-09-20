//! Generated service credentials are preserved after their first successful application.
use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Secret;
use kube::{
    Api, ResourceExt,
    api::{Patch, PatchParams},
};

use crate::Error;

type GenerateData = fn(&Secret) -> Result<BTreeMap<String, String>, Error>;

pub(super) async fn ensure(
    secrets: &Api<Secret>,
    template: &Secret,
    patch: &PatchParams,
) -> Result<(), Error> {
    let kind = template
        .string_data
        .as_ref()
        .and_then(|data| data.get("PROOFSTORM_SECRET_KIND"))
        .map(String::as_str);
    let (required, generate): (&[&str], GenerateData) = match kind {
        Some("nutshell-mint") => (
            &["PROOFSTORM_SECRET_KIND", "MINT_PRIVATE_KEY"],
            nutshell_data,
        ),
        Some("redis-cache") => (
            &["PROOFSTORM_SECRET_KIND", "REDIS_PASSWORD", "REDIS_URL"],
            redis_data,
        ),
        Some("keycloak-oidc") => (
            &[
                "PROOFSTORM_SECRET_KIND",
                "KEYCLOAK_ADMIN_PASSWORD",
                "OIDC_TEST_USERNAME",
                "OIDC_TEST_PASSWORD",
                "realm.json",
            ],
            keycloak_data,
        ),
        _ => (
            &[
                "POSTGRES_USER",
                "POSTGRES_PASSWORD",
                "POSTGRES_DB",
                "DATABASE_URL",
                "database.toml",
            ],
            postgres_data,
        ),
    };
    ensure_generated_secret(secrets, template, patch, required, || generate(template)).await
}

async fn ensure_generated_secret(
    secrets: &Api<Secret>,
    template: &Secret,
    patch: &PatchParams,
    required: &[&str],
    generate: impl FnOnce() -> Result<BTreeMap<String, String>, Error>,
) -> Result<(), Error> {
    let name = template.name_any();
    if let Some(existing) = secrets.get_opt(&name).await? {
        let data = existing.data.as_ref().ok_or_else(|| {
            Error::SecretContract(format!("Secret {name:?} has no generated data"))
        })?;
        for key in required {
            if !data.contains_key(*key) {
                return Err(Error::SecretContract(format!(
                    "Secret {name:?} is missing key {key:?}"
                )));
            }
        }
        return Ok(());
    }
    let mut desired = template.clone();
    desired
        .string_data
        .get_or_insert_default()
        .extend(generate()?);
    secrets.patch(&name, patch, &Patch::Apply(&desired)).await?;
    Ok(())
}

fn random_hex(name: &str) -> Result<String, Error> {
    let mut entropy = [0_u8; 32];
    getrandom::fill(&mut entropy).map_err(|error| {
        Error::SecretContract(format!(
            "could not generate credentials for {name:?}: {error}"
        ))
    })?;
    Ok(entropy
        .iter()
        .fold(String::with_capacity(64), |mut encoded, byte| {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
            encoded
        }))
}

fn postgres_data(template: &Secret) -> Result<BTreeMap<String, String>, Error> {
    let name = template.name_any();
    let template_data = template.string_data.as_ref().ok_or_else(|| {
        Error::SecretContract(format!("Secret template {name:?} has no stringData"))
    })?;
    let username = template_data.get("POSTGRES_USER").ok_or_else(|| {
        Error::SecretContract(format!("Secret template {name:?} has no POSTGRES_USER"))
    })?;
    let database = template_data.get("POSTGRES_DB").ok_or_else(|| {
        Error::SecretContract(format!("Secret template {name:?} has no POSTGRES_DB"))
    })?;
    let component = component_name(template)?;
    let password = random_hex(&name)?;
    let url = format!("postgresql://{username}:{password}@{component}:5432/{database}");
    let database_config = format!(
        "\n[database]\nengine = \"postgres\"\n\n[database.postgres]\nurl = {url:?}\ntls_mode = \"disable\"\nmax_connections = 20\nconnection_timeout_seconds = 10\n"
    );
    Ok(BTreeMap::from([
        ("POSTGRES_PASSWORD".into(), password),
        ("DATABASE_URL".into(), url),
        ("database.toml".into(), database_config),
    ]))
}

fn nutshell_data(template: &Secret) -> Result<BTreeMap<String, String>, Error> {
    let name = template.name_any();
    let private_key = random_hex(&name)?;
    Ok(BTreeMap::from([("MINT_PRIVATE_KEY".into(), private_key)]))
}

fn redis_data(template: &Secret) -> Result<BTreeMap<String, String>, Error> {
    let name = template.name_any();
    let component = component_name(template)?;
    let password = random_hex(&name)?;
    let url = format!("redis://:{password}@{component}:6379/0");
    Ok(BTreeMap::from([
        ("REDIS_PASSWORD".into(), password),
        ("REDIS_URL".into(), url),
    ]))
}

fn keycloak_data(template: &Secret) -> Result<BTreeMap<String, String>, Error> {
    let name = template.name_any();
    let template_data = template.string_data.as_ref().ok_or_else(|| {
        Error::SecretContract(format!("Secret template {name:?} has no stringData"))
    })?;
    let access_token_lifespan = template_data
        .get("OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS")
        .ok_or_else(|| {
            Error::SecretContract(format!(
                "Secret template {name:?} has no OIDC_ACCESS_TOKEN_LIFESPAN_SECONDS"
            ))
        })?
        .parse::<u64>()
        .map_err(|error| {
            Error::SecretContract(format!(
                "Secret template {name:?} has an invalid OIDC token lifespan: {error}"
            ))
        })?;
    let administrator_password = random_hex(&name)?;
    let test_password = random_hex(&name)?;
    let test_username = "proofstorm-user";
    let realm = serde_json::to_string_pretty(&serde_json::json!({
        "realm": "proofstorm",
        "enabled": true,
        "sslRequired": "none",
        "accessTokenLifespan": access_token_lifespan,
        "clients": [{
            "clientId": "cashu-client",
            "protocol": "openid-connect",
            "enabled": true,
            "publicClient": true,
            "standardFlowEnabled": true,
            "directAccessGrantsEnabled": true,
            "defaultClientScopes": ["web-origins", "acr", "roles", "profile", "basic", "email"],
            "optionalClientScopes": ["offline_access"],
            "redirectUris": ["http://127.0.0.1:*", "http://localhost:*"],
            "webOrigins": ["*"]
        }],
        "users": [{
            "username": test_username,
            "email": "proofstorm-user@example.invalid",
            "firstName": "Proofstorm",
            "lastName": "User",
            "enabled": true,
            "emailVerified": true,
            "requiredActions": [],
            "realmRoles": ["offline_access"],
            "credentials": [{
                "type": "password",
                "value": test_password,
                "temporary": false
            }]
        }]
    }))
    .map_err(|error| {
        Error::SecretContract(format!("could not render realm for {name:?}: {error}"))
    })?;
    Ok(BTreeMap::from([
        ("KEYCLOAK_ADMIN_PASSWORD".into(), administrator_password),
        ("OIDC_TEST_USERNAME".into(), test_username.into()),
        ("OIDC_TEST_PASSWORD".into(), test_password),
        ("realm.json".into(), realm),
    ]))
}

fn component_name(template: &Secret) -> Result<&str, Error> {
    let name = template.name_any();
    template
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get("proofstorm.dev/component"))
        .ok_or_else(|| {
            Error::SecretContract(format!(
                "Secret template {name:?} has no component identity"
            ))
        })
        .map(String::as_str)
}

#[cfg(test)]
mod tests;
