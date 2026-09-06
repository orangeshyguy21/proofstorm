use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Invalid,
    Missing,
    Failure,
}

/// A domain failure; transports choose their own wire envelope.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    pub details: Option<Value>,
}

impl Error {
    pub fn problem(code: &str, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Invalid,
            message: message.into(),
            details: Some(serde_json::json!({"code": code})),
        }
    }

    pub fn failure(message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            kind: ErrorKind::Failure,
            message: message.into(),
            details,
        }
    }

    pub fn missing(message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            kind: ErrorKind::Missing,
            message: message.into(),
            details,
        }
    }
}

impl From<proofstorm_store::StoreError> for Error {
    fn from(error: proofstorm_store::StoreError) -> Self {
        use proofstorm_store::StoreError;
        let kind = match &error {
            StoreError::Io(_)
            | StoreError::Database(_)
            | StoreError::Serialization(_)
            | StoreError::Poisoned
            | StoreError::VersionOverflow(_)
            | StoreError::InvalidStoredVersion(_) => ErrorKind::Failure,
            StoreError::NotFound { .. } => ErrorKind::Missing,
            _ => ErrorKind::Invalid,
        };
        Self {
            kind,
            message: error.to_string(),
            details: Some(serde_json::json!({"code": match &error {
                StoreError::Serialization(_) => "stored_record_incompatible",
                _ => error.code(),
            }})),
        }
    }
}

impl From<kube::Error> for Error {
    fn from(error: kube::Error) -> Self {
        let status = match &error {
            kube::Error::Api(response) => Some(response.code),
            _ => None,
        };
        Self::failure(
            format!("Kubernetes runtime failure: {error}"),
            Some(serde_json::json!({"code":"runtime_failure", "http_status":status})),
        )
    }
}
