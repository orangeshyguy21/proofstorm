//! Per-mint management identities. The CA signing key is never persisted or mounted.

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

fn generate(name: &str) -> Result<BTreeMap<String, String>, rcgen::Error> {
    let mut params = parameters(name, vec![])?;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages.push(KeyUsagePurpose::KeyCertSign);
    let ca_key = KeyPair::generate()?;
    let ca = params.self_signed(&ca_key)?;
    let mut data = BTreeMap::from([
        ("PROOFSTORM_SECRET_KIND".into(), KIND.into()),
        ("ca.pem".into(), ca.pem()),
    ]);
    for (role, usage, names) in [
        (
            "server",
            ExtendedKeyUsagePurpose::ServerAuth,
            vec!["localhost".into(), "127.0.0.1".into()],
        ),
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

fn validate(secret: &Secret) -> Result<(), Error> {
    let data = secret
        .data
        .as_ref()
        .ok_or_else(|| Error::SecretContract("management TLS Secret has no data".into()))?;
    if data.get("PROOFSTORM_SECRET_KIND").map(|v| v.0.as_slice()) != Some(KIND.as_bytes())
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
    if let Some(existing) = secrets.get_opt(&name).await? {
        return validate(&existing);
    }
    let mut desired = template.clone();
    desired.string_data = Some(generate(&name).map_err(|error| {
        Error::SecretContract(format!(
            "could not generate management TLS identities: {error}"
        ))
    })?);
    // Create, never apply: concurrent reconcilers must not rotate each other's keys.
    match secrets.create(&PostParams::default(), &desired).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(response)) if response.code == 409 => {
            validate(&secrets.get(&name).await?)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::ByteString;

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
