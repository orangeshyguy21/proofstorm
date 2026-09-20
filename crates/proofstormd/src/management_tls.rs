//! Per-service management and payment identities. The CA signing key is never persisted.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Secret;
use kube::{Api, ResourceExt, api::PostParams};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use time::{Duration, OffsetDateTime};

use crate::Error;

const KIND: &str = "mint-management-tls";
const KEYS: &[&str] = &[
    "ca.pem",
    "server.pem",
    "server.key",
    "client.pem",
    "client.key",
];

fn parameters(name: &str, names: Vec<String>) -> Result<CertificateParams, rcgen::Error> {
    let mut params = CertificateParams::new(names)?;
    params.distinguished_name.push(DnType::CommonName, name);
    params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
    params.not_after = OffsetDateTime::now_utc() + Duration::days(365);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    Ok(params)
}

#[cfg(test)]
fn generate(name: &str) -> Result<BTreeMap<String, String>, rcgen::Error> {
    generate_for(name, KIND, None)
}

fn generate_for(
    name: &str,
    kind: &str,
    server_name: Option<&str>,
) -> Result<BTreeMap<String, String>, rcgen::Error> {
    let mut params = parameters(name, vec![])?;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages.push(KeyUsagePurpose::KeyCertSign);
    let ca_key = KeyPair::generate()?;
    let ca = params.self_signed(&ca_key)?;
    let mut data = BTreeMap::from([
        ("PROOFSTORM_SECRET_KIND".into(), kind.into()),
        ("ca.pem".into(), ca.pem()),
    ]);
    let mut server_names = vec!["localhost".into(), "127.0.0.1".into()];
    if let Some(server_name) = server_name {
        server_names.push(server_name.into());
        data.insert("PROOFSTORM_TLS_SERVER_NAME".into(), server_name.into());
    }
    for (role, usage, names) in [
        ("server", ExtendedKeyUsagePurpose::ServerAuth, server_names),
        ("client", ExtendedKeyUsagePurpose::ClientAuth, vec![]),
    ] {
        let mut params = parameters(&format!("{name}-{role}"), names)?;
        params.extended_key_usages.push(usage);
        let key = KeyPair::generate()?;
        let cert = params.signed_by(&key, &ca, &ca_key)?;
        data.insert(format!("{role}.pem"), cert.pem());
        data.insert(format!("{role}.key"), key.serialize_pem());
    }
    Ok(data)
}

#[cfg(test)]
fn validate(secret: &Secret) -> Result<(), Error> {
    validate_for(secret, KIND, None)
}

fn validate_for(secret: &Secret, kind: &str, server_name: Option<&str>) -> Result<(), Error> {
    let data = secret
        .data
        .as_ref()
        .ok_or_else(|| Error::SecretContract("management TLS Secret has no data".into()))?;
    if data.get("PROOFSTORM_SECRET_KIND").map(|v| v.0.as_slice()) != Some(kind.as_bytes())
        || server_name.is_some_and(|name| {
            data.get("PROOFSTORM_TLS_SERVER_NAME")
                .map(|v| v.0.as_slice())
                != Some(name.as_bytes())
        })
        || KEYS
            .iter()
            .any(|key| data.get(*key).is_none_or(|value| value.0.is_empty()))
    {
        return Err(Error::SecretContract(format!(
            "management TLS Secret {:?} is incomplete; refusing to replace existing identities",
            secret.name_any()
        )));
    }
    Ok(())
}

pub(super) async fn ensure(secrets: &Api<Secret>, template: &Secret) -> Result<(), Error> {
    let name = template.name_any();
    let data = template
        .string_data
        .as_ref()
        .ok_or_else(|| Error::SecretContract("TLS template data missing".into()))?;
    let kind = data
        .get("PROOFSTORM_SECRET_KIND")
        .map_or(KIND, String::as_str);
    let server_name = data.get("PROOFSTORM_TLS_SERVER_NAME").map(String::as_str);
    if kind == "payment-processor-tls" && server_name.is_none() {
        return Err(Error::SecretContract(
            "Payment processor TLS requires a service DNS identity".into(),
        ));
    }
    if let Some(existing) = secrets.get_opt(&name).await? {
        return validate_for(&existing, kind, server_name);
    }
    let mut desired = template.clone();
    desired.string_data = Some(generate_for(&name, kind, server_name).map_err(|error| {
        Error::SecretContract(format!(
            "could not generate management TLS identities: {error}"
        ))
    })?);
    // Create, never apply: concurrent reconcilers must not rotate each other's keys.
    match secrets.create(&PostParams::default(), &desired).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(response)) if response.code == 409 => {
            validate_for(&secrets.get(&name).await?, kind, server_name)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::ByteString;

    #[test]
    fn payment_tls_preserves_its_service_identity_and_rejects_unrelated_secrets() {
        let data = generate_for(
            "processor-payment-tls",
            "payment-processor-tls",
            Some("processor"),
        )
        .unwrap();
        assert!(!data.contains_key("ca.key"));
        let secret = Secret {
            data: Some(
                data.into_iter()
                    .map(|(k, v)| (k, ByteString(v.into_bytes())))
                    .collect(),
            ),
            ..Secret::default()
        };
        validate_for(&secret, "payment-processor-tls", Some("processor")).unwrap();
        assert!(validate_for(&secret, "payment-processor-tls", Some("other-processor")).is_err());
        assert!(validate(&secret).is_err());
    }

    #[test]
    fn independent_mints_have_distinct_identities_without_ca_signing_keys() {
        let a = generate("mint-a").unwrap();
        let b = generate("mint-b").unwrap();
        for key in KEYS {
            assert_ne!(a[*key], b[*key]);
        }
        assert!(!a.contains_key("ca.key"));
        let mut secret = Secret {
            data: Some(
                a.into_iter()
                    .map(|(k, v)| (k, ByteString(v.into_bytes())))
                    .collect(),
            ),
            ..Secret::default()
        };
        validate(&secret).unwrap();
        secret.data.as_mut().unwrap().remove("client.key");
        assert!(validate(&secret).is_err());
    }
}
