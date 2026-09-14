use crate::{Error, environment::EnvironmentQuery};
use proofstorm_core::{ComponentKind, InstancePhase, digest_json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EnvironmentReadQuery {
    pub instance_id: Option<String>,
    pub cursor: String,
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
    pub session_cursor: String,
    pub activity_cursor: String,
    pub component_cursor: String,
    pub link_cursor: String,
    /// Exact directory name and owner filters.
    pub name: Option<String>,
    pub owner: Option<String>,
    /// Observed phase; consult runtime.state for freshness.
    pub phase: Option<InstancePhase>,
    /// Match a component with this kind and/or implementation in desired configuration.
    pub component_kind: Option<ComponentKind>,
    pub implementation: Option<String>,
    /// Search directory header JSON (identity, name, owner and runtime observation).
    /// Configuration and receipts have their own dedicated search tools.
    pub query: String,
    pub regex: bool,
    pub case_insensitive: bool,
    /// Compact directory headers only. Mutually exclusive with sections and fields.
    pub scan: bool,
    /// With `instance_id`, load only these sections: components, links, resources, sessions, activity.
    /// MCP accepts arrays; HTTP also accepts comma-separated values or a JSON array string.
    #[serde(deserialize_with = "strings")]
    pub sections: Vec<String>,
    /// RFC 6901 pointers relative to a directory cell, e.g. /runtime/phase or
    /// /components/items/0/id. Only required sections are loaded. Empty returns requested sections.
    #[serde(deserialize_with = "strings")]
    pub fields: Vec<String>,
}

impl Default for EnvironmentReadQuery {
    fn default() -> Self {
        Self {
            instance_id: None,
            cursor: String::new(),
            limit: 20,
            session_cursor: String::new(),
            activity_cursor: String::new(),
            component_cursor: String::new(),
            link_cursor: String::new(),
            name: None,
            owner: None,
            phase: None,
            component_kind: None,
            implementation: None,
            query: String::new(),
            regex: false,
            case_insensitive: false,
            scan: false,
            sections: vec![],
            fields: vec![],
        }
    }
}

impl EnvironmentReadQuery {
    pub(super) fn selective(&self) -> bool {
        self.scan
            || !self.sections.is_empty()
            || !self.fields.is_empty()
            || self.name.is_some()
            || self.owner.is_some()
            || self.phase.is_some()
            || self.component_kind.is_some()
            || self.implementation.is_some()
            || !self.query.is_empty()
            || self.regex
            || self.case_insensitive
    }
    pub(super) fn page(&self) -> EnvironmentQuery {
        EnvironmentQuery {
            instance_id: self.instance_id.clone(),
            cursor: self.cursor.clone(),
            limit: self.limit,
            session_cursor: self.session_cursor.clone(),
            activity_cursor: self.activity_cursor.clone(),
            component_cursor: self.component_cursor.clone(),
            link_cursor: self.link_cursor.clone(),
        }
    }
    pub(super) fn validate(&self) -> Result<(), Error> {
        if !self.selective() {
            return self
                .page()
                .validate()
                .map_err(|message| Error::problem("invalid_page", message));
        }
        if !(1..=50).contains(&self.limit)
            || self.query.len() > 4096
            || self.fields.len() > 32
            || self.sections.len() > 5
            || self.scan && (!self.sections.is_empty() || !self.fields.is_empty())
            || self.fields.iter().any(|field| !valid_pointer(field))
            || self.sections.iter().any(|section| {
                !["components", "links", "resources", "sessions", "activity"]
                    .contains(&section.as_str())
            })
        {
            return Err(Error::problem(
                "environment_query_invalid",
                "use limit 1..=50, query at most 4096 bytes, and at most 32 RFC 6901 fields; choose scan or sections/fields",
            ));
        }
        if [
            &self.cursor,
            &self.session_cursor,
            &self.activity_cursor,
            &self.component_cursor,
            &self.link_cursor,
        ]
        .iter()
        .any(|c| c.len() > 256)
            || [
                &self.instance_id,
                &self.name,
                &self.owner,
                &self.implementation,
            ]
            .into_iter()
            .flatten()
            .any(|id| id.is_empty() || id.len() > 128)
        {
            return Err(Error::problem(
                "environment_query_invalid",
                "IDs must be 1..=128 bytes and cursors at most 256 bytes",
            ));
        }
        let sections = self.load_sections();
        if !sections.is_empty() && self.instance_id.is_none() {
            return Err(Error::problem(
                "environment_query_invalid",
                "detail sections require instance_id; scan the directory first, then select a cell",
            ));
        }
        for (section, cursor) in [
            ("components", &self.component_cursor),
            ("links", &self.link_cursor),
            ("sessions", &self.session_cursor),
            ("activity", &self.activity_cursor),
        ] {
            if !cursor.is_empty()
                && (self.instance_id.is_none() || !sections.iter().any(|s| s == section))
            {
                return Err(Error::problem(
                    "environment_query_invalid",
                    "section cursors require instance_id and selection of that section",
                ));
            }
        }
        if self.instance_id.is_some() && !self.cursor.is_empty() {
            return Err(Error::problem(
                "environment_query_invalid",
                "cell cursor cannot be combined with instance_id",
            ));
        }
        Ok(())
    }
    pub(super) fn pattern(&self) -> Result<regex::Regex, Error> {
        regex::RegexBuilder::new(&if self.regex {
            self.query.clone()
        } else {
            regex::escape(&self.query)
        })
        .case_insensitive(self.case_insensitive)
        .size_limit(1 << 20)
        .build()
        .map_err(|error| Error::problem("search_regex_invalid", error.to_string()))
    }
    pub(super) fn load_sections(&self) -> Vec<String> {
        let mut sections = self.sections.clone();
        for field in &self.fields {
            let first = field.split('/').nth(1).unwrap_or_default();
            if field.is_empty() {
                sections.extend(
                    ["components", "links", "resources", "sessions", "activity"].map(str::to_owned),
                );
            } else if ["components", "links", "resources", "sessions", "activity"].contains(&first)
            {
                sections.push(first.into());
            }
        }
        sections.sort();
        sections.dedup();
        sections
    }
    pub(super) fn needs_endpoints(&self) -> bool {
        self.sections.iter().any(|s| s == "components")
            || self.fields.iter().any(|field| {
                if field.is_empty() || field == "/components" || field == "/components/items" {
                    return true;
                }
                let parts: Vec<_> = field.split('/').collect();
                parts.len() >= 4
                    && parts[1] == "components"
                    && parts[2] == "items"
                    && (parts.len() == 4 || parts[4] == "endpoints")
            })
    }
    pub(super) fn minimum_items(&self, section: &str) -> usize {
        self.fields
            .iter()
            .filter_map(|field| {
                let parts: Vec<_> = field.split('/').collect();
                if parts.len() >= 4 && parts[1] == section && parts[2] == "items" {
                    parts[3].parse::<usize>().ok().map(|i| i.saturating_add(1))
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(1)
    }
    pub(super) fn only_instance_selector(&self) -> bool {
        self.name.is_none()
            && self.owner.is_none()
            && self.phase.is_none()
            && self.component_kind.is_none()
            && self.implementation.is_none()
            && self.query.is_empty()
    }
    pub(super) fn fingerprint(&self, workspace: &str, principal: &str, snapshot: &str) -> String {
        let mut selectors = self.clone();
        selectors.cursor.clear();
        selectors.component_cursor.clear();
        selectors.link_cursor.clear();
        selectors.activity_cursor.clear();
        selectors.session_cursor.clear();
        selectors.limit = 0;
        digest_json(&(workspace, principal, snapshot, selectors))
    }
    pub(super) fn cursor_for(&self, section: &str, context: &str, id: &str) -> String {
        format!(
            "{}:{id}",
            digest_json(&(context, section, &self.instance_id, id))
        )
    }
    pub(super) fn boundary(
        &self,
        section: &str,
        cursor: &str,
        context: &str,
    ) -> Result<String, Error> {
        if cursor.is_empty() {
            return Ok(String::new());
        }
        let (digest, id) = cursor.rsplit_once(':').ok_or_else(stale)?;
        if id.is_empty() || digest != digest_json(&(context, section, &self.instance_id, id)) {
            return Err(stale());
        }
        Ok(id.into())
    }
}

fn stale() -> Error {
    Error::problem(
        "environment_cursor_invalid",
        "The directory or selectors changed, or the cursor is invalid. Repeat without cursor",
    )
}

fn valid_pointer(field: &str) -> bool {
    if field.len() > 512 || !field.is_empty() && !field.starts_with('/') {
        return false;
    }
    let mut chars = field.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}

fn strings<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Many(Vec<String>),
        One(String),
    }
    match Input::deserialize(deserializer)? {
        Input::Many(values) => Ok(values),
        Input::One(value) if value.starts_with('[') => {
            serde_json::from_str(&value).map_err(serde::de::Error::custom)
        }
        Input::One(value) => Ok(value
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn field_dependencies_are_precise_and_http_uses_the_same_selectors() {
        let query: EnvironmentReadQuery = serde_json::from_value(
            json!({"instance_id":"cell","fields":["/components/items/5/id","/runtime/message"]}),
        )
        .unwrap();
        query.validate().unwrap();
        assert_eq!(query.load_sections(), ["components"]);
        assert!(!query.needs_endpoints());
        assert_eq!(query.minimum_items("components"), 6);
        for field in ["/components/items", "/components/items/0/endpoints/0"] {
            let query: EnvironmentReadQuery =
                serde_json::from_value(json!({"fields":[field]})).unwrap();
            assert!(query.needs_endpoints());
        }
        let unrelated: EnvironmentReadQuery = serde_json::from_value(
            json!({"sections":["activity"],"fields":["/sessions/items/0/endpoints"]}),
        )
        .unwrap();
        assert!(!unrelated.needs_endpoints());
        let http: EnvironmentReadQuery =
            serde_urlencoded::from_str("sections=components%2Clinks&fields=%5B%22%2Fa%2Cb%22%5D")
                .unwrap();
        assert_eq!(http.sections, ["components", "links"]);
        assert_eq!(http.fields, ["/a,b"]);
        for invalid in [
            json!({"query":"[","regex":true}),
            json!({"fields":["/bad~escape"]}),
            json!({"scan":true,"sections":["sessions"]}),
        ] {
            let query: EnvironmentReadQuery = serde_json::from_value(invalid).unwrap();
            assert!(
                query
                    .validate()
                    .and_then(|()| query.pattern().map(|_| ()))
                    .is_err()
            );
        }
    }
}
