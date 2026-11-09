//! JSON for Kubernetes server-side apply.
//!
//! The API server parses an apply body as YAML. YAML rejects some characters
//! that JSON writes raw (DEL, the C1 controls, U+FFFE and U+FFFF) and reads
//! others as line breaks, which it folds into spaces (U+0085, U+2028 and
//! U+2029). Native output can hold any of them: in swarm season 2, `grep -a`
//! over SQLite files made action statuses the API server refused on every
//! retry, so those actions never finished and `storm stop` could not drain.
//! This writes them as `\u` escapes, which both parsers read back exactly.

use std::io;

use serde::Serialize;

/// Whether apply JSON writes `character` as a `\u` escape. `serde_json`
/// already escapes U+0000 to U+001F.
#[must_use]
pub const fn escapes(character: char) -> bool {
    matches!(
        character,
        '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}' | '\u{fffe}' | '\u{ffff}'
    )
}

/// Compact JSON that a server-side apply carries unchanged.
///
/// # Errors
///
/// Returns the `serde_json` error when `value` does not serialize.
pub fn to_vec<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Vec<u8>> {
    let mut json = Vec::new();
    value.serialize(&mut serde_json::Serializer::with_formatter(
        &mut json,
        ApplyFormatter,
    ))?;
    Ok(json)
}

struct ApplyFormatter;

impl serde_json::ser::Formatter for ApplyFormatter {
    fn write_string_fragment<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let mut rest = fragment;
        while let Some((index, character)) = rest.char_indices().find(|&(_, c)| escapes(c)) {
            let (before, after) = rest.split_at(index);
            writer.write_all(before.as_bytes())?;
            write!(writer, "\\u{:04x}", u32::from(character))?;
            rest = &after[character.len_utf8()..];
        }
        writer.write_all(rest.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Characters YAML 1.1 reads raw and keeps: its printable set
    /// without the line breaks it folds inside a quoted scalar.
    fn yaml_keeps(character: char) -> bool {
        matches!(
            u32::from(character),
            0x09 | 0x0a | 0x0d
                | 0x20..=0x7e
                | 0xa0..=0xd7ff
                | 0xe000..=0xfffd
                | 0x1_0000..=0x10_ffff
        ) && !matches!(character, '\u{2028}' | '\u{2029}')
    }

    #[test]
    fn every_character_reaches_the_api_server_unchanged() {
        let text = (0..=0x10_ffff_u32)
            .filter_map(char::from_u32)
            .collect::<String>();
        let document = json!({"status": {"stdout": text, text.clone(): "key"}});
        let encoded = to_vec(&document).unwrap();
        let encoded = std::str::from_utf8(&encoded).unwrap();
        let refused = encoded.chars().find(|&c| !yaml_keeps(c));
        assert_eq!(refused, None, "YAML refuses or folds {refused:?}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(encoded).unwrap(),
            document
        );
    }

    #[test]
    fn plain_documents_encode_as_serde_json_does() {
        let document = json!({"a": ["x\n\"y\"\u{1}", 1, null, true], "é": "naïve ✓ 🦀"});
        assert_eq!(
            to_vec(&document).unwrap(),
            serde_json::to_vec(&document).unwrap()
        );
        assert_eq!(
            to_vec(&json!("grep\u{7f}\u{85}\u{9f}\u{2028}\u{ffff}")).unwrap(),
            br#""grep\u007f\u0085\u009f\u2028\uffff""#
        );
    }
}
