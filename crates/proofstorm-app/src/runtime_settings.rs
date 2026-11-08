//! `runtime.toml`: the user's runtime settings, edited by `storm config` and
//! applied by `storm setup`. The file is the only source of truth.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use proofstorm_core::{NO_LIMIT, RuntimeSettings, SETTING_KEYS, SettingKey};
use serde_json::{Value, json};
use toml_edit::DocumentMut;

pub const FILE_NAME: &str = "runtime.toml";

#[must_use]
pub fn path(home: &Path) -> PathBuf {
    home.join(FILE_NAME)
}

fn document(home: &Path) -> Result<DocumentMut> {
    let path = path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => text
            .parse()
            .with_context(|| format!("{} is not valid TOML", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn parse(document: &DocumentMut, home: &Path) -> Result<RuntimeSettings> {
    let settings: RuntimeSettings =
        toml_edit::de::from_document(document.clone()).with_context(|| {
            format!(
                "{} has an unknown key or a wrong type",
                path(home).display()
            )
        })?;
    settings.validate().map_err(anyhow::Error::msg)?;
    Ok(settings)
}

/// The settings in force; a missing file means every default.
///
/// # Errors
///
/// Rejects an unreadable file, unknown keys and invalid values.
pub fn load(home: &Path) -> Result<RuntimeSettings> {
    parse(&document(home)?, home)
}

fn key(name: &str) -> Result<&'static SettingKey> {
    SETTING_KEYS
        .iter()
        .find(|key| key.name == name)
        .with_context(|| {
            let names: Vec<_> = SETTING_KEYS.iter().map(|key| key.name).collect();
            format!(
                "unknown setting {name:?}; settings are: {}",
                names.join(", ")
            )
        })
}

/// The table and field a dotted key lives at: `controller.cpu.limit` is
/// field `limit` of table `controller.cpu`.
fn location(name: &str) -> (Vec<&str>, &str) {
    let mut parts: Vec<&str> = name.split('.').collect();
    let field = parts.pop().expect("keys have a field");
    (parts, field)
}

fn table<'a>(document: &'a mut DocumentMut, path: &[&str]) -> &'a mut toml_edit::Table {
    let mut table = document.as_table_mut();
    for part in path {
        let entry = table
            .entry(part)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        if let Some(inline) = entry.as_inline_table().cloned() {
            *entry = toml_edit::Item::Table(inline.into_table());
        }
        table = entry.as_table_mut().expect("settings tables are tables");
        table.set_implicit(true);
    }
    table
}

/// What one change does, for the CLI to show before the user applies it.
#[derive(Debug, serde::Serialize)]
pub struct Change {
    pub key: &'static str,
    pub value: Option<String>,
    pub effect: &'static str,
}

/// Set `key=value` pairs, then validate the whole file before writing it.
///
/// # Errors
///
/// Rejects unknown keys, values of the wrong kind and invalid combinations;
/// nothing is written then.
pub fn set(home: &Path, pairs: &[String]) -> Result<Vec<Change>> {
    let mut document = document(home)?;
    let mut changes = Vec::new();
    for pair in pairs {
        let (name, value) = pair
            .split_once('=')
            .with_context(|| format!("expected KEY=VALUE, not {pair:?}"))?;
        let key = key(name.trim())?;
        let value = value.trim();
        let (path, field) = location(key.name);
        let item = if key.quantity {
            toml_edit::value(value)
        } else {
            let number: i64 = value
                .parse()
                .with_context(|| format!("{} must be a whole number, not {value:?}", key.name))?;
            toml_edit::value(number)
        };
        table(&mut document, &path).insert(field, item);
        changes.push(Change {
            key: key.name,
            value: Some(value.to_owned()),
            effect: key.effect,
        });
    }
    write(home, &document)?;
    Ok(changes)
}

/// Remove keys so they return to their defaults.
///
/// # Errors
///
/// Rejects unknown keys, or a file that would no longer be valid.
pub fn unset(home: &Path, names: &[String]) -> Result<Vec<Change>> {
    let mut document = document(home)?;
    let mut changes = Vec::new();
    for name in names {
        let key = key(name.trim())?;
        let (path, field) = location(key.name);
        table(&mut document, &path).remove(field);
        changes.push(Change {
            key: key.name,
            value: None,
            effect: key.effect,
        });
    }
    write(home, &document)?;
    Ok(changes)
}

fn write(home: &Path, document: &DocumentMut) -> Result<()> {
    parse(document, home)?;
    let path = path(home);
    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, document.to_string())
        .with_context(|| format!("write {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("replace {}", path.display()))
}

/// Every key with its value, its default and where the value comes from.
#[must_use]
pub fn show(settings: &RuntimeSettings) -> Value {
    Value::Array(
        SETTING_KEYS
            .iter()
            .map(|key| {
                let value = settings.get(key.name);
                json!({
                    "key": key.name,
                    "value": value.clone().unwrap_or_else(|| key.default.to_owned()),
                    "default": key.default,
                    "source": if value.is_some() { FILE_NAME } else { "default" },
                })
            })
            .collect(),
    )
}

/// Helm arguments that apply the settings over the chart's defaults. A
/// `none` limit is sent as null, which removes the chart's value.
#[must_use]
pub fn helm_args(settings: &RuntimeSettings) -> Vec<String> {
    let mut args = Vec::new();
    for (resource, amount) in [
        ("cpu", &settings.controller.cpu),
        ("memory", &settings.controller.memory),
    ] {
        let Some(amount) = amount else { continue };
        for (kind, value) in [("requests", &amount.request), ("limits", &amount.limit)] {
            let Some(value) = value else { continue };
            let json = if value == NO_LIMIT {
                Value::Null
            } else {
                Value::String(value.clone())
            };
            args.push("--set-json".to_owned());
            args.push(format!("resources.{kind}.{resource}={json}"));
        }
    }
    let limits = settings.controller_limits();
    for (name, value) in [
        ("actionSlots", limits.action_slots.to_string()),
        ("nativeStarts", limits.native_starts.to_string()),
        (
            "nativeStartsPerComponent",
            limits.native_starts_per_component.to_string(),
        ),
        ("nativePolls", limits.native_polls.to_string()),
        (
            "startDeadlineSeconds",
            limits.start_deadline_seconds.to_string(),
        ),
    ] {
        args.push("--set".to_owned());
        args.push(format!("controller.{name}={value}"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_keeps_comments_validates_and_round_trips() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            path(home.path()),
            "# my notes\n[controller]\n# busy cell\naction_slots = 32\n",
        )
        .unwrap();
        let changes = set(
            home.path(),
            &[
                "controller.cpu.limit=none".into(),
                "controller.native_starts=8".into(),
            ],
        )
        .unwrap();
        assert_eq!(changes.len(), 2);
        let text = std::fs::read_to_string(path(home.path())).unwrap();
        assert!(
            text.contains("# my notes") && text.contains("# busy cell"),
            "{text}"
        );
        let settings = load(home.path()).unwrap();
        assert_eq!(settings.controller_limits().action_slots, 32);
        assert_eq!(settings.controller_limits().native_starts, 8);
        assert_eq!(
            settings.get("controller.cpu.limit").as_deref(),
            Some("none")
        );

        unset(home.path(), &["controller.cpu.limit".into()]).unwrap();
        assert_eq!(load(home.path()).unwrap().get("controller.cpu.limit"), None);
    }

    #[test]
    fn invalid_changes_write_nothing() {
        let home = tempfile::tempdir().unwrap();
        set(home.path(), &["controller.action_slots=32".into()]).unwrap();
        let before = std::fs::read_to_string(path(home.path())).unwrap();
        for bad in [
            "controller.native_polls=40",
            "controller.native_starts=many",
            "controller.memory.limit=lots",
            "controller.slots=1",
            "controller.cpu.limit",
        ] {
            assert!(set(home.path(), &[bad.into()]).is_err(), "{bad}");
        }
        assert_eq!(std::fs::read_to_string(path(home.path())).unwrap(), before);
    }

    #[test]
    fn a_missing_file_means_defaults_and_shows_every_key() {
        let home = tempfile::tempdir().unwrap();
        let settings = load(home.path()).unwrap();
        assert_eq!(settings, RuntimeSettings::default());
        let rows = show(&settings);
        assert_eq!(rows.as_array().unwrap().len(), SETTING_KEYS.len());
        assert!(
            rows.as_array()
                .unwrap()
                .iter()
                .all(|row| row["source"] == "default")
        );
    }

    #[test]
    fn helm_args_override_only_what_is_set_and_null_removes_a_limit() {
        let settings: RuntimeSettings = toml_edit::de::from_str(
            "[controller]\nmemory = { limit = \"none\" }\ncpu = { request = \"1\" }\nnative_starts = 8\n",
        )
        .unwrap();
        let args = helm_args(&settings);
        assert!(
            args.windows(2)
                .any(|w| w == ["--set-json", "resources.limits.memory=null"])
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--set-json", "resources.requests.cpu=\"1\""])
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--set", "controller.nativeStarts=8"])
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--set", "controller.actionSlots=16"])
        );
        assert!(!args.iter().any(|arg| arg.contains("requests.memory")));
    }
}
