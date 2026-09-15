use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DirectoryQuery {
    pub id: Option<String>,
    pub owner: Option<String>,
    pub phase: Option<String>,
    pub query: String,
    pub regex: bool,
    pub case_insensitive: bool,
    pub scan: bool,
    pub fields: Vec<String>,
    pub cursor: Option<String>,
    pub limit: usize,
}
impl Default for DirectoryQuery {
    fn default() -> Self {
        Self {
            id: None,
            owner: None,
            phase: None,
            query: String::new(),
            regex: false,
            case_insensitive: false,
            scan: false,
            fields: vec![],
            cursor: None,
            limit: 20,
        }
    }
}
