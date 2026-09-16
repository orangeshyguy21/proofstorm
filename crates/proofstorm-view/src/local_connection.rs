use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalConnectionState {
    Connecting,
    Connected,
    Disconnecting,
    Disconnected,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalConnectionView {
    pub id: String,
    pub cell: String,
    pub incarnation: String,
    pub component: String,
    pub state: LocalConnectionState,
    pub url: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenLocalConnection {
    pub cell: String,
    pub incarnation: String,
    pub component: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CloseLocalConnection {
    pub id: String,
}
