//! Wallet seeds may be created only before their owned storage exists.
use k8s_openapi::api::core::v1::PersistentVolumeClaim;

use super::{
    Api, BTreeMap, Error, Secret, component_name, ensure_generated_secret, random_bytes,
    random_hex, validate,
};

/// An identity bound to the data its component writes to `data-{component}-0`.
pub(super) struct Identity {
    kind: &'static str,
    /// Names the identity in errors, which never carry its values.
    pub(super) label: &'static str,
    /// `mnemonic` is always generated; `admin-token` only where listed.
    required: &'static [&'static str],
}

const IDENTITIES: &[Identity] = &[
    Identity {
        kind: "bark-processor",
        label: "Bark",
        required: &["PROOFSTORM_SECRET_KIND", "mnemonic"],
    },
    Identity {
        kind: "open-ssp",
        label: "Open SSP",
        required: &["PROOFSTORM_SECRET_KIND", "mnemonic", "admin-token"],
    },
    Identity {
        kind: "spark-processor",
        label: "Spark processor",
        required: &["PROOFSTORM_SECRET_KIND", "mnemonic"],
    },
];

pub(super) fn identity(kind: &str) -> Option<&'static Identity> {
    IDENTITIES.iter().find(|identity| identity.kind == kind)
}

pub(crate) async fn ensure(
    secrets: &Api<Secret>,
    claims: &Api<PersistentVolumeClaim>,
    template: &Secret,
) -> Result<(), Error> {
    use kube::ResourceExt as _;
    let name = template.name_any();
    let identity = template
        .string_data
        .as_ref()
        .and_then(|data| data.get("PROOFSTORM_SECRET_KIND"))
        .and_then(|kind| identity(kind))
        .ok_or_else(|| {
            Error::SecretContract(format!(
                "Secret template {name:?} is not an owned-storage identity"
            ))
        })?;
    if let Some(existing) = secrets.get_opt(&name).await? {
        return identity.validate(&existing);
    }
    let component = component_name(template)?;
    if claims
        .get_opt(&format!("data-{component}-0"))
        .await?
        .is_some()
    {
        // Another reconciler can have created the seed and then its PVC after
        // our first read. Keep its winning identity, never rotate it.
        if let Some(existing) = secrets.get_opt(&name).await? {
            return identity.validate(&existing);
        }
        return Err(Error::SecretContract(format!(
            "{} identity Secret {name:?} is missing while owned storage exists; restore the identity instead of regenerating it",
            identity.label
        )));
    }
    ensure_generated_secret(secrets, template, identity.required, || {
        identity.generate(&name)
    })
    .await?;
    identity.validate(&secrets.get(&name).await?)
}

impl Identity {
    fn generate(&self, name: &str) -> Result<BTreeMap<String, String>, Error> {
        let mnemonic = bip39::Mnemonic::from_entropy(&random_bytes::<16>(name)?).map_err(|_| {
            Error::SecretContract(format!("could not encode {} mnemonic", self.label))
        })?;
        let mut data = BTreeMap::from([("mnemonic".into(), mnemonic.to_string())]);
        if self.required.contains(&"admin-token") {
            data.insert("admin-token".into(), random_hex(name)?);
        }
        Ok(data)
    }

    fn validate(&self, secret: &Secret) -> Result<(), Error> {
        validate(secret, self.required)?;
        let data = secret.data.as_ref().expect("validated data");
        if data["PROOFSTORM_SECRET_KIND"].0 != self.kind.as_bytes()
            || std::str::from_utf8(&data["mnemonic"].0)
                .ok()
                .and_then(|value| value.parse::<bip39::Mnemonic>().ok())
                .is_none()
            || (self.required.contains(&"admin-token") && {
                let token = &data["admin-token"].0;
                token.len() != 64
                    || !token
                        .iter()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            })
        {
            return Err(Error::SecretContract(format!(
                "{} identity Secret has invalid data; refusing replacement",
                self.label
            )));
        }
        Ok(())
    }
}
