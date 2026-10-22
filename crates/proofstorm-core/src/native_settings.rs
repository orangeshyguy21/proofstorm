//! Native config overrides: mint settings Proofstorm does not model.
//!
//! A Nutshell mint takes extra environment variables (`native_env`). A CDK
//! mint takes extra `config.toml` keys (`native_toml`), not environment
//! variables, because cdk-mintd lets the environment override its config file.
//! Only keys Proofstorm neither sets nor manages are accepted, the cell must
//! opt in with `mint.native_config_override`, and the lock lists the
//! overrides as unverified.
use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::{
    Capability, CellSpec, ComponentSpec, ConfigFieldContract, ConfigSettingClass, ConfigValueKind,
    ValidationIssue,
};

const MAX_TOML_BYTES: usize = 8192;

/// Nutshell settings Proofstorm sets or manages, with the config field each
/// comes from ("" for a constant). Nutshell reads names ignoring case. A
/// trailing `_` reserves every name with that prefix. Prefix entries come
/// last, so an exact name wins.
const NUTSHELL_ENV: &[(&str, &str)] = &[
    ("CASHU_DIR", "data_directory"),
    ("LIGHTNING_FEE_PERCENT", "lightning_fee_percent"),
    ("LIGHTNING_RESERVE_FEE_MIN", "lightning_reserve_fee_min_sat"),
    ("MELT_QUOTE_TTL", "melt_quote_ttl_seconds"),
    ("MINT_AUTH_DATABASE", "authentication_database"),
    ("MINT_AUTH_DATABASE_PASSWORD", "authentication_database"),
    ("MINT_AUTH_MAX_BLIND_TOKENS", "auth_max_blind_tokens"),
    ("MINT_AUTH_OICD_CLIENT_ID", "oidc_client_id"),
    ("MINT_AUTH_OICD_DISCOVERY_URL", "oidc_discovery_url"),
    (
        "MINT_AUTH_RATE_LIMIT_PER_MINUTE",
        "auth_rate_limit_per_minute",
    ),
    ("MINT_BOLT11_DISABLE_MELT", "disable_melt"),
    ("MINT_BOLT11_DISABLE_MINT", "disable_mint"),
    ("MINT_CLNREST_ENABLE_MPP", "clnrest_enable_mpp"),
    ("MINT_DATABASE", "database"),
    ("MINT_DATABASE_LOCK_TIMEOUT", "database_lock_timeout_ms"),
    ("MINT_DATABASE_PASSWORD", "database"),
    ("MINT_DERIVATION_PATH", "derivation_path"),
    ("MINT_DERIVATION_PATH_LIST", "derivation_path"),
    ("MINT_FORWARDED_ALLOW_IPS", "forwarded_allow_ips"),
    (
        "MINT_GLOBAL_RATE_LIMIT_PER_MINUTE",
        "global_rate_limit_per_minute",
    ),
    ("MINT_INFO_CONTACT", "contact_email"),
    ("MINT_INFO_DESCRIPTION", "description"),
    ("MINT_INFO_DESCRIPTION_LONG", "description_long"),
    ("MINT_INFO_ICON_URL", "icon_url"),
    ("MINT_INFO_MOTD", "motd"),
    ("MINT_INFO_NAME", "name"),
    ("MINT_INFO_TOS_URL", "tos_url"),
    ("MINT_INFO_URLS", "public_url"),
    ("MINT_INPUT_FEE_PPK", "input_fee_ppk"),
    ("MINT_LIGHTNING_BACKEND", "backend_kind"),
    ("MINT_LISTEN_HOST", "listen_address"),
    ("MINT_LISTEN_PORT", "listen_address"),
    ("MINT_LND_ENABLE_MPP", "lnd_enable_mpp"),
    ("MINT_MAX_BALANCE", "max_balance_sat"),
    ("MINT_MAX_MELT_BOLT11_SAT", "max_melt_sat"),
    ("MINT_MAX_MINT_BOLT11_SAT", "max_mint_sat"),
    ("MINT_MAX_REQUEST_LENGTH", "max_request_length"),
    ("MINT_MAX_SECRET_LENGTH", "max_secret_length"),
    ("MINT_MAX_WITNESS_LENGTH", "max_witness_length"),
    ("MINT_PAYMENT_BACKENDS", "backend_kind"),
    ("MINT_PRIVATE_KEY", "mint_private_key"),
    (
        "MINT_QUOTE_BACKEND_CHECK_RATE_LIMIT",
        "quote_backend_check_rate_limit_seconds",
    ),
    ("MINT_QUOTE_TTL", "mint_quote_ttl_seconds"),
    ("MINT_RATE_LIMIT", "rate_limit"),
    ("MINT_RATE_LIMIT_PROXY_TRUST", "rate_limit_proxy_trust"),
    ("MINT_REDIS_CACHE_TTL", "redis_cache_ttl_seconds"),
    (
        "MINT_REGULAR_TASKS_INTERVAL_SECONDS",
        "regular_tasks_interval_seconds",
    ),
    ("MINT_SEED_DECRYPTION_KEY", "mint_private_key"),
    (
        "MINT_TRANSACTION_RATE_LIMIT_PER_MINUTE",
        "transaction_rate_limit_per_minute",
    ),
    (
        "MINT_WATCHDOG_BALANCE_CHECK_INTERVAL_SECONDS",
        "watchdog_balance_check_interval_seconds",
    ),
    ("MINT_WATCHDOG_ENABLED", "watchdog_enabled"),
    (
        "MINT_WEBSOCKET_READ_TIMEOUT",
        "websocket_read_timeout_seconds",
    ),
    ("HTTP_PROXY", "tor"),
    ("SOCKS_HOST", "tor"),
    ("SOCKS_PORT", "tor"),
    ("SOCKS_PROXY", "tor"),
    ("TOR", "tor"),
    ("MINT_AUTH_OICD_", "authentication"),
    ("MINT_BACKEND_", "backend_kind"),
    ("MINT_CLNREST_", "lightning_backend_credentials"),
    ("MINT_LND_", "lightning_backend_credentials"),
    ("MINT_REDIS_CACHE_", "redis_cache"),
    ("MINT_REQUIRE_", "authentication"),
    ("MINT_RPC_SERVER_", "management_rpc"),
    ("PROOFSTORM_", ""),
];

/// CDK `config.toml` keys and whole tables Proofstorm writes or manages, with
/// the config field each comes from ("" for a constant). A reserved table
/// blocks every key inside it; new keys beside a reserved key are allowed.
const CDK_TOML: &[(&str, &str)] = &[
    ("info.url", "public_url"),
    ("info.listen_host", "listen_address"),
    ("info.listen_port", "listen_address"),
    ("info.mnemonic", "mint_mnemonic"),
    ("info.signatory_url", "mint_mnemonic"),
    ("info.signatory_certs", "mint_mnemonic"),
    ("info.enable_info_page", "enable_info_page"),
    ("info.input_fee_ppk", "input_fee_ppk"),
    ("info.use_keyset_v2", "use_keyset_v2"),
    ("info.quote_ttl.mint_ttl", "mint_quote_ttl_seconds"),
    ("info.quote_ttl.melt_ttl", "melt_quote_ttl_seconds"),
    ("info.http_cache.backend", ""),
    ("info.http_cache.ttl", "http_cache_ttl_seconds"),
    ("info.http_cache.tti", "http_cache_tti_seconds"),
    ("mint_info.name", "name"),
    ("mint_info.description", "description"),
    ("mint_info.description_long", "description_long"),
    ("mint_info.motd", "motd"),
    ("mint_info.icon_url", "icon_url"),
    ("mint_info.contact_email", "contact_email"),
    (
        "mint_info.contact_nostr_public_key",
        "contact_nostr_public_key",
    ),
    ("mint_info.tos_url", "tos_url"),
    ("limits.max_inputs", "max_inputs"),
    ("limits.max_outputs", "max_outputs"),
    ("mint_management_rpc", "management_rpc"),
    ("payment_backend", "payment_backends"),
    ("fake_wallet", "payment_backends"),
    ("grpc_processor", "lightning_backend_endpoint"),
    ("lnd", "lightning_backend_endpoint"),
    ("cln", "lightning_backend_endpoint"),
    ("ldk_node", "embedded_lightning"),
    ("onchain", "embedded_onchain"),
    ("bdk", "embedded_onchain"),
    ("database", "database_engine"),
    ("auth_database", "authentication_database"),
    ("auth", "authentication"),
    ("signatory", "mint_mnemonic"),
];

/// How one mint implementation takes overrides.
struct Overrides {
    field: &'static str,
    /// Environment names, matched ignoring case; otherwise dotted TOML paths.
    env: bool,
    reserved: &'static [(&'static str, &'static str)],
    description: &'static str,
}

const NUTSHELL: Overrides = Overrides {
    field: "native_env",
    env: true,
    reserved: NUTSHELL_ENV,
    description: "Extra Nutshell environment variables for settings Proofstorm does not model, passed unchanged and marked unverified. Requires mint.native_config_override in policy.allow; names Proofstorm sets are refused.",
};

const CDK: Overrides = Overrides {
    field: "native_toml",
    env: false,
    reserved: CDK_TOML,
    description: "Extra cdk-mintd config.toml keys and tables for settings Proofstorm does not model, merged into the rendered file and marked unverified. Requires mint.native_config_override in policy.allow; keys and tables Proofstorm sets are refused.",
};

fn overrides(implementation: &str) -> Option<&'static Overrides> {
    match implementation {
        "nutshell" => Some(&NUTSHELL),
        "cdk" => Some(&CDK),
        _ => None,
    }
}

/// The override field for one implementation.
#[must_use]
pub fn field(implementation: &str) -> Option<&'static str> {
    overrides(implementation).map(|overrides| overrides.field)
}

/// The authorable, optional config field declared by the backend contract.
pub(crate) fn config_field(implementation: &str) -> Option<(String, ConfigFieldContract)> {
    let overrides = overrides(implementation)?;
    Some((
        overrides.field.into(),
        ConfigFieldContract {
            required: false,
            ..crate::backend::managed_field(
                overrides.description,
                ConfigValueKind::Object,
                ConfigSettingClass::AgentAuthorable,
            )
        },
    ))
}

/// The config field Proofstorm sets `key` from ("" for a constant), or
/// `None` when an override may use it. CDK keys are dotted paths.
#[must_use]
pub fn reserved(implementation: &str, key: &str) -> Option<&'static str> {
    let overrides = overrides(implementation)?;
    let upper = key.to_ascii_uppercase();
    let matches = |(name, _): &&(&str, &str)| {
        if overrides.env {
            upper == *name || (name.ends_with('_') && upper.starts_with(name))
        } else {
            let inside = |outer: &str, inner: &str| {
                inner
                    .strip_prefix(outer)
                    .is_some_and(|rest| rest.starts_with('.'))
            };
            key == *name || inside(name, key) || inside(key, name)
        }
    };
    overrides
        .reserved
        .iter()
        .find(matches)
        .map(|(_, field)| *field)
}

/// The override keys a component sets: env names, or dotted TOML leaf paths.
#[must_use]
pub fn override_keys(component: &ComponentSpec) -> Vec<String> {
    let Some(value) = field(&component.implementation).and_then(|f| component.config.get(f)) else {
        return vec![];
    };
    let mut keys = Vec::new();
    leaf_paths(value, &mut Vec::new(), &mut keys);
    // Map order depends on serde_json features; the lock must not.
    keys.sort();
    keys
}

fn leaf_paths<'a>(value: &'a Value, path: &mut Vec<&'a str>, paths: &mut Vec<String>) {
    match value {
        Value::Object(map) if !map.is_empty() || path.is_empty() => {
            for (key, value) in map {
                path.push(key);
                leaf_paths(value, path, paths);
                path.pop();
            }
        }
        _ => paths.push(path.join(".")),
    }
}

/// Plan-time checks: opt-in, shape, and keys Proofstorm sets.
pub(crate) fn validate(cell: &CellSpec, issues: &mut Vec<ValidationIssue>) {
    let allowed = cell
        .policy
        .allow
        .contains(&Capability::MintNativeConfigOverride);
    for (index, component) in cell.components.iter().enumerate() {
        let Some(overrides) = overrides(&component.implementation) else {
            continue;
        };
        let Some(value) = component.config.get(overrides.field) else {
            continue;
        };
        let path = format!("/components/{index}/config/{}", overrides.field);
        if !allowed {
            crate::validation::issue(
                issues,
                "native_config_override_not_allowed",
                &path,
                "passes settings Proofstorm does not verify; add \"mint.native_config_override\" to policy.allow to enable it",
            );
        }
        if let Err(message) = shape(overrides, value) {
            crate::validation::issue(issues, "native_config_override_invalid", path, message);
            continue;
        }
        for key in override_keys(component) {
            let Some(source) = reserved(&component.implementation, &key) else {
                continue;
            };
            let pointer = key
                .split('.')
                .map(|segment| segment.replace('~', "~0").replace('/', "~1"))
                .collect::<Vec<_>>()
                .join("/");
            let message = if source.is_empty() {
                "Proofstorm sets this".into()
            } else {
                format!("Proofstorm sets this from /config/{source}")
            };
            crate::validation::issue(
                issues,
                "native_config_override_managed",
                format!("{path}/{pointer}"),
                message,
            );
        }
    }
}

fn shape(overrides: &Overrides, value: &Value) -> Result<(), String> {
    if overrides.env {
        let env = BTreeMap::<String, String>::deserialize(value)
            .map_err(|_| "expected an object of string values".to_owned())?;
        return crate::workspace::validate_env(&env).map_err(Into::into);
    }
    let Some(table) = value.as_object() else {
        return Err("expected an object of TOML keys".into());
    };
    if table.values().any(contains_null) {
        return Err("TOML has no null; omit the key instead".into());
    }
    if value.to_string().len() > MAX_TOML_BYTES {
        return Err(format!("exceeds {MAX_TOML_BYTES} bytes as JSON"));
    }
    Ok(())
}

fn contains_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(contains_null),
        Value::Object(map) => map.values().any(contains_null),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::reserved;
    use crate::validate_cell;

    fn cell(nutshell: &Value, cdk: &Value, allow: &Value) -> crate::CellSpec {
        let mint = |id: &str, implementation: &str, version: &str, config: &Value| json!({"id": id, "kind": "mint", "implementation": implementation, "config_version": version, "control": "target", "config": config});
        serde_json::from_value(json!({
            "api_version": crate::API_VERSION,
            "name": "native-overrides",
            "components": [
                mint("nutshell", "nutshell", "nutshell-mint/0.21/v1", nutshell),
                mint("cdk", "cdk", "cdk-mintd/0.18/v1", cdk)
            ],
            "links": [],
            "policy": {"allow": allow, "limits": {"max_components": 64, "max_links": 256, "max_config_bytes": 65536}}
        }))
        .unwrap()
    }

    fn issues(cell: &crate::CellSpec) -> Vec<(String, String, String)> {
        validate_cell(cell)
            .issues
            .into_iter()
            .filter(|issue| issue.code.starts_with("native_config_override"))
            .map(|issue| (issue.code, issue.path, issue.message))
            .collect()
    }

    const ALLOW: &str = "mint.native_config_override";

    #[test]
    fn overrides_require_the_cell_policy_opt_in_on_their_own_field() {
        let env = json!({"native_env": {"MINT_MNEMONIC": "abandon"}});
        let toml = json!({"native_toml": {"info": {"new_flag": true}}});
        assert!(issues(&cell(&json!({}), &json!({}), &json!([]))).is_empty());
        let refused = issues(&cell(&env, &toml, &json!(["mint.quote_payment_override"])));
        assert_eq!(
            refused
                .iter()
                .map(|(code, path, _)| (code.as_str(), path.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    "native_config_override_not_allowed",
                    "/components/0/config/native_env"
                ),
                (
                    "native_config_override_not_allowed",
                    "/components/1/config/native_toml"
                ),
            ]
        );
        assert!(issues(&cell(&env, &toml, &json!([ALLOW]))).is_empty());
    }

    #[test]
    fn keys_proofstorm_sets_are_refused_with_their_source_field() {
        let refused = issues(&cell(
            &json!({"native_env": {
                "mint_database": "/tmp", "MINT_PRIVATE_KEY": "x", "MINT_LND_REST_ADMIN_MACAROON": "x",
                "MINT_REDIS_CACHE_TTL": "1", "MINT_MNEMONIC": "abandon"
            }}),
            &json!({"native_toml": {
                "info": {"input_fee_ppk": 1, "new_flag": true, "http_cache": {"backend": "redis"}},
                "database": {"postgres": {"url": "x"}},
                "limits": "flat",
                "prometheus": {"enabled": true}
            }}),
            &json!([ALLOW]),
        ));
        assert_eq!(
            refused
                .iter()
                .map(|(_, path, message)| (path.as_str(), message.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    "/components/0/config/native_env/MINT_LND_REST_ADMIN_MACAROON",
                    "Proofstorm sets this from /config/lightning_backend_credentials"
                ),
                (
                    "/components/0/config/native_env/MINT_PRIVATE_KEY",
                    "Proofstorm sets this from /config/mint_private_key"
                ),
                (
                    "/components/0/config/native_env/MINT_REDIS_CACHE_TTL",
                    "Proofstorm sets this from /config/redis_cache_ttl_seconds"
                ),
                (
                    "/components/0/config/native_env/mint_database",
                    "Proofstorm sets this from /config/database"
                ),
                (
                    "/components/1/config/native_toml/database/postgres/url",
                    "Proofstorm sets this from /config/database_engine"
                ),
                (
                    "/components/1/config/native_toml/info/http_cache/backend",
                    "Proofstorm sets this"
                ),
                (
                    "/components/1/config/native_toml/info/input_fee_ppk",
                    "Proofstorm sets this from /config/input_fee_ppk"
                ),
                (
                    "/components/1/config/native_toml/limits",
                    "Proofstorm sets this from /config/max_inputs"
                ),
            ]
        );
    }

    #[test]
    fn reserved_cdk_paths_cover_tables_and_prefixes_but_not_neighbours() {
        assert_eq!(reserved("cdk", "info.url"), Some("public_url"));
        assert_eq!(reserved("cdk", "info"), Some("public_url"));
        assert_eq!(reserved("cdk", "info.url.scheme"), Some("public_url"));
        assert_eq!(
            reserved("cdk", "lnd.address"),
            Some("lightning_backend_endpoint")
        );
        assert_eq!(reserved("cdk", "info.urls"), None);
        assert_eq!(reserved("cdk", "info.new_flag"), None);
        assert_eq!(reserved("cdk", "lndx.address"), None);
        assert_eq!(reserved("nutshell", "Tor"), Some("tor"));
        assert_eq!(
            reserved("nutshell", "MINT_BACKEND_BOLT12_SAT"),
            Some("backend_kind")
        );
        assert_eq!(reserved("nutshell", "MINT_MNEMONIC"), None);
        assert_eq!(reserved("workspace", "PATH"), None);
    }

    #[test]
    fn override_shapes_are_bounded() {
        let codes = |nutshell: Value, cdk: Value| {
            issues(&cell(&nutshell, &cdk, &json!([ALLOW])))
                .into_iter()
                .map(|(code, path, _)| format!("{code} {path}"))
                .collect::<Vec<_>>()
        };
        let invalid_env = |env: Value| codes(json!({"native_env": env}), json!({}));
        for env in [
            json!({"PROOFSTORM_X": "1"}),
            json!({"1BAD": "1"}),
            json!({"MINT_X": 1}),
            json!({"MINT_X": "x".repeat(4097)}),
            Value::Object(
                (0..65)
                    .map(|i| (format!("MINT_X{i}"), json!("1")))
                    .collect(),
            ),
        ] {
            assert_eq!(
                invalid_env(env.clone()),
                ["native_config_override_invalid /components/0/config/native_env"],
                "{env}"
            );
        }
        for toml in [
            json!({"info": {"new_flag": null}}),
            json!({"new": [1, null]}),
            json!({"new": "x".repeat(8193)}),
        ] {
            assert_eq!(
                codes(json!({}), json!({"native_toml": toml})),
                ["native_config_override_invalid /components/1/config/native_toml"]
            );
        }
        assert_eq!(
            codes(json!({}), json!({"native_toml": ["x"]})),
            ["native_config_override_invalid /components/1/config/native_toml"]
        );
        // Each bound says which one was crossed.
        let message = |env: Value| {
            issues(&cell(
                &json!({"native_env": env}),
                &json!({}),
                &json!([ALLOW]),
            ))
            .remove(0)
            .2
        };
        assert_eq!(
            message(json!({"1BAD": "1"})),
            "environment names must match [A-Za-z_][A-Za-z0-9_]* within 128 bytes"
        );
        assert_eq!(
            message(json!({"PROOFSTORM_X": "1"})),
            "PROOFSTORM_ environment variables are reserved"
        );
    }
}
