//! The wallet seed may be created only before its owned storage exists.
use k8s_openapi::api::core::v1::PersistentVolumeClaim;

use super::{
    Api, BTreeMap, Error, Secret, component_name, ensure_generated_secret, random_bytes, validate,
};

const REQUIRED: &[&str] = &["PROOFSTORM_SECRET_KIND", "mnemonic"];

pub(crate) async fn ensure(
    secrets: &Api<Secret>,
    claims: &Api<PersistentVolumeClaim>,
    template: &Secret,
) -> Result<(), Error> {
    use kube::ResourceExt as _;
    let name = template.name_any();
    if let Some(existing) = secrets.get_opt(&name).await? {
        return validate_identity(&existing);
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
            return validate_identity(&existing);
        }
        return Err(Error::SecretContract(format!(
            "Bark identity Secret {name:?} is missing while owned storage exists; restore the identity instead of regenerating it"
        )));
    }
    ensure_generated_secret(secrets, template, REQUIRED, || {
        let mnemonic = bip39::Mnemonic::from_entropy(&random_bytes::<16>(&name)?)
            .map_err(|_| Error::SecretContract("could not encode Bark mnemonic".into()))?;
        Ok(BTreeMap::from([("mnemonic".into(), mnemonic.to_string())]))
    })
    .await?;
    validate_identity(&secrets.get(&name).await?)
}

fn validate_identity(secret: &Secret) -> Result<(), Error> {
    validate(secret, REQUIRED)?;
    let data = secret.data.as_ref().expect("validated data");
    if data["PROOFSTORM_SECRET_KIND"].0 != b"bark-processor"
        || std::str::from_utf8(&data["mnemonic"].0)
            .ok()
            .and_then(|value| value.parse::<bip39::Mnemonic>().ok())
            .is_none()
    {
        return Err(Error::SecretContract(
            "Bark identity Secret has invalid data; refusing replacement".into(),
        ));
    }
    Ok(())
}
