//! Bounded native byte bindings. No token body is part of a public request.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_PRIVATE_BYTES: u32 = 1024 * 1024;
pub const MAX_PRIVATE_ARG_BYTES: u32 = 64 * 1024;
pub const PRIVATE_ARG: &str = "@proofstorm-private-input";
pub const PRIVATE_ACCESS_ANNOTATION: &str = "proofstorm.dev/private-access-grants";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFormat {
    Bytes,
    CashuToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputBinding {
    Stdin,
    Argv { index: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PayloadBinding {
    Capture {
        reference: String,
        format: CaptureFormat,
    },
    Consume {
        reference: String,
        input: InputBinding,
    },
}
impl PayloadBinding {
    #[must_use]
    pub fn reference(&self) -> &str {
        match self {
            Self::Capture { reference, .. } | Self::Consume { reference, .. } => reference,
        }
    }
}

/// Internal runner request, constructed after runtime custody/session admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrivateIo {
    Capture {
        maximum_bytes: u32,
        format: CaptureFormat,
    },
    Consume {
        bytes: u32,
        sha256: String,
        input: InputBinding,
    },
}
impl PrivateIo {
    /// # Errors
    /// Returns a fixed diagnostic; no private input appears here.
    pub fn validate(&self, command: &crate::native::NativeCommand) -> Result<(), &'static str> {
        if command.output.mode != crate::native::OutputMode::Private
            || !command.output.fields.is_empty()
        {
            return Err("private payload requires private output");
        }
        match self {
            Self::Capture { maximum_bytes, .. }
                if (1..=MAX_PRIVATE_BYTES).contains(maximum_bytes) =>
            {
                Ok(())
            }
            Self::Consume {
                bytes,
                sha256,
                input,
            } if (1..=MAX_PRIVATE_BYTES).contains(bytes)
                && sha256.len() == 64
                && sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) =>
            {
                if let InputBinding::Argv { index } = input {
                    if *bytes > MAX_PRIVATE_ARG_BYTES
                        || *index == 0
                        || command.argv.get(*index as usize).map(String::as_str)
                            != Some(PRIVATE_ARG)
                    {
                        return Err("private argv binding invalid or too large");
                    }
                }
                Ok(())
            }
            _ => Err("private payload bounds invalid"),
        }
    }
}

/// Select exactly one token-shaped line, allowing native startup diagnostics on
/// other lines. This extracts bytes; it does not validate Cashu proofs or value.
/// # Errors
/// Ambiguous, non-UTF8 or malformed output returns a fixed diagnostic.
pub fn select_capture(bytes: &[u8], format: CaptureFormat) -> Result<Vec<u8>, &'static str> {
    if format == CaptureFormat::Bytes {
        return Ok(bytes.to_vec());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "private_capture_format_invalid")?;
    let candidates: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("cashu"))
        .collect();
    if candidates.len() != 1 {
        return Err("private_capture_ambiguous");
    }
    let token = candidates[0];
    if !(token.starts_with("cashuA") || token.starts_with("cashuB"))
        || token.len() <= 6
        || !token[6..]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_=+/".contains(&c))
    {
        return Err("private_capture_format_invalid");
    }
    Ok(token.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_selection_handles_startup_text_without_leaking_ambiguity() {
        assert_eq!(
            select_capture(
                b"Recovered native saga\ncashuBabc_-12=\n",
                CaptureFormat::CashuToken
            )
            .unwrap(),
            b"cashuBabc_-12="
        );
        for bytes in [
            b"cashuBfirst\ncashuBsecond".as_slice(),
            b"cashuBsecret with spaces",
            b"not a token",
        ] {
            assert!(select_capture(bytes, CaptureFormat::CashuToken).is_err());
        }
    }
}
