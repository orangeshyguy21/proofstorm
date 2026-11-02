//! A Spark operator federation's identities are fixed when it is first created:
//! operators sign together, and each operator database is sealed to its key.
use kube::ResourceExt as _;
use proofstorm_core::SPARK_MAX_OPERATORS;
use rcgen::{BasicConstraints, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose};
use secp256k1::{Secp256k1, SecretKey};
use serde::Serialize;

use super::{
    Api, BTreeMap, Error, Secret, component_name, ensure_generated_secret_with, hex, random_bytes,
};
use crate::management_tls::parameters;

const KIND: &str = "spark-operators";
/// Operators reach each other on this gRPC port.
const OPERATOR_PORT: u16 = 8535;
/// Where operators and their clients mount this Secret's CA.
const CA_PATH: &str = "/spark-operators/ca.crt";

pub(super) async fn ensure_operators(
    secrets: &Api<Secret>,
    template: &Secret,
) -> Result<(), Error> {
    let federation = Federation::from_template(template)?;
    let name = template.name_any();
    ensure_generated_secret_with(
        secrets,
        template,
        || federation.generate(&name),
        |existing| federation.validate(existing),
    )
    .await
}

/// The template settings a federation is created with.
struct Federation<'a> {
    operators: u64,
    threshold: u64,
    service: &'a str,
}

/// One `operators.json` entry; field order is the file's key order.
#[derive(Serialize)]
struct Operator {
    id: u64,
    address: String,
    external_address: String,
    identity_public_key: String,
    cert_path: &'static str,
}

impl<'a> Federation<'a> {
    fn from_template(template: &'a Secret) -> Result<Self, Error> {
        let name = template.name_any();
        let data = template.string_data.as_ref().ok_or_else(|| {
            Error::SecretContract(format!("Secret template {name:?} has no stringData"))
        })?;
        let setting = |key: &str| {
            data.get(key).map(String::as_str).ok_or_else(|| {
                Error::SecretContract(format!("Secret template {name:?} has no {key}"))
            })
        };
        // Only canonical decimals, which `settings` renders back exactly.
        let count = |key: &str| {
            setting(key).map(|value| {
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|count| count.to_string() == value)
            })
        };
        let operators = count("PROOFSTORM_SPARK_OPERATORS")?;
        let threshold = count("PROOFSTORM_SPARK_THRESHOLD")?;
        let service = setting("PROOFSTORM_SPARK_SERVICE")?;
        let operators = operators
            .filter(|operators| (1..=SPARK_MAX_OPERATORS).contains(operators))
            .ok_or_else(|| {
                Error::SecretContract(format!(
                    "Secret template {name:?} needs 1 to {SPARK_MAX_OPERATORS} Spark operators"
                ))
            })?;
        let threshold = threshold
            .filter(|threshold| (1..=operators).contains(threshold))
            .ok_or_else(|| {
                Error::SecretContract(format!(
                    "Secret template {name:?} needs a Spark threshold from 1 to {operators}"
                ))
            })?;
        if service != component_name(template)? {
            return Err(Error::SecretContract(format!(
                "Secret template {name:?} names a Spark service other than its component"
            )));
        }
        Ok(Self {
            operators,
            threshold,
            service,
        })
    }

    /// Stored beside the generated data and compared verbatim afterwards.
    fn settings(&self) -> [(&'static str, String); 4] {
        [
            ("PROOFSTORM_SECRET_KIND", KIND.into()),
            ("PROOFSTORM_SPARK_OPERATORS", self.operators.to_string()),
            ("PROOFSTORM_SPARK_THRESHOLD", self.threshold.to_string()),
            ("PROOFSTORM_SPARK_SERVICE", self.service.into()),
        ]
    }

    /// Every key an intact Secret holds.
    fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .settings()
            .into_iter()
            .map(|(key, _)| key.into())
            .chain(["ca.crt", "operators.json", "threshold"].map(String::from))
            .collect();
        for index in 0..self.operators {
            keys.extend([
                format!("operator-{index}.key"),
                format!("operator-{index}.pub"),
                format!("server-{index}.crt"),
                format!("server-{index}.key"),
            ]);
        }
        keys
    }

    fn validate(&self, secret: &Secret) -> Result<(), Error> {
        let name = secret.name_any();
        let data = secret.data.as_ref().ok_or_else(|| {
            Error::SecretContract(format!("Secret {name:?} has no generated data"))
        })?;
        if self.settings().iter().any(|(key, value)| {
            data.get(*key).map(|stored| stored.0.as_slice()) != Some(value.as_bytes())
        }) {
            return Err(Error::SecretContract(format!(
                "Spark operators Secret {name:?} differs from its template; the federation's size, threshold and service are fixed at creation"
            )));
        }
        if let Some(key) = self
            .keys()
            .into_iter()
            .find(|key| data.get(key).is_none_or(|value| value.0.is_empty()))
        {
            return Err(Error::SecretContract(format!(
                "Spark operators Secret {name:?} is missing key {key:?}; the federation's size, threshold and service are fixed at creation, so restore the original Secret"
            )));
        }
        Ok(())
    }

    fn generate(&self, name: &str) -> Result<BTreeMap<String, String>, Error> {
        let tls = |error: rcgen::Error| {
            Error::SecretContract(format!(
                "could not generate Spark operator TLS identities for {name:?}: {error}"
            ))
        };
        let service = self.service;
        let mut authority = parameters(&format!("{service} operator CA"), vec![]).map_err(tls)?;
        authority.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        authority.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        // Dropped on return: nothing can issue another certificate the
        // federation would trust.
        let authority_key = KeyPair::generate().map_err(tls)?;
        let authority = authority.self_signed(&authority_key).map_err(tls)?;
        let mut data = BTreeMap::from([
            ("ca.crt".into(), authority.pem()),
            ("threshold".into(), format!("{}\n", self.threshold)),
        ]);
        let secp = Secp256k1::signing_only();
        let mut operators = Vec::new();
        for index in 0..self.operators {
            let identity = operator_key(name)?;
            let public_key = hex(&identity.public_key(&secp).serialize());
            data.insert(
                format!("operator-{index}.key"),
                format!("{}\n", hex(&identity.secret_bytes())),
            );
            data.insert(format!("operator-{index}.pub"), format!("{public_key}\n"));
            let host = format!("{service}-{index}");
            let address = format!("{host}.{service}");
            let mut params = parameters(
                &host,
                vec![
                    address.clone(),
                    host.clone(),
                    "localhost".into(),
                    "127.0.0.1".into(),
                ],
            )
            .map_err(tls)?;
            params.is_ca = IsCa::ExplicitNoCa;
            params.use_authority_key_identifier_extension = true;
            params.extended_key_usages = vec![
                ExtendedKeyUsagePurpose::ServerAuth,
                ExtendedKeyUsagePurpose::ClientAuth,
            ];
            let key = KeyPair::generate().map_err(tls)?;
            let certificate = params
                .signed_by(&key, &authority, &authority_key)
                .map_err(tls)?;
            data.insert(format!("server-{index}.crt"), certificate.pem());
            data.insert(format!("server-{index}.key"), key.serialize_pem());
            let address = format!("{address}:{OPERATOR_PORT}");
            operators.push(Operator {
                id: index,
                external_address: address.clone(),
                address,
                identity_public_key: public_key,
                cert_path: CA_PATH,
            });
        }
        let operators = serde_json::to_string(&operators).map_err(|error| {
            Error::SecretContract(format!(
                "could not render operators.json for {name:?}: {error}"
            ))
        })?;
        data.insert("operators.json".into(), operators);
        Ok(data)
    }
}

/// Fewer than one in 2^127 random 32-byte strings is not a valid scalar.
fn operator_key(name: &str) -> Result<SecretKey, Error> {
    loop {
        if let Ok(key) = SecretKey::from_slice(&random_bytes::<32>(name)?) {
            return Ok(key);
        }
    }
}
