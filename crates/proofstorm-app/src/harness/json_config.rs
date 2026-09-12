//! Small lossless JSON/JSONC editor: change only the managed server, retaining
//! surrounding bytes. Reject duplicate keys and ambiguous documents before edits.
use anyhow::{Result, bail, ensure};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, ops::Range};

struct Node {
    key_span: Option<Range<usize>>,
    value: Value,
    span: Range<usize>,
    members: BTreeMap<String, Node>,
    trailing_comma: bool,
}
struct Parser<'a> {
    text: &'a str,
    pos: usize,
    comments: bool,
}
impl Parser<'_> {
    fn skip(&mut self) -> Result<()> {
        loop {
            while self
                .text
                .as_bytes()
                .get(self.pos)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.pos += 1;
            }
            if !self.comments {
                return Ok(());
            }
            let rest = &self.text[self.pos..];
            if rest.starts_with("//") {
                self.pos += rest.find('\n').unwrap_or(rest.len());
            } else if rest.starts_with("/*") {
                self.pos += rest
                    .find("*/")
                    .ok_or_else(|| anyhow::anyhow!("unterminated JSONC comment"))?
                    + 2;
            } else {
                return Ok(());
            }
        }
    }
    fn string(&mut self) -> Result<String> {
        let start = self.pos;
        ensure!(
            self.text.as_bytes().get(start) == Some(&b'"'),
            "expected JSON property string"
        );
        self.pos += 1;
        while let Some(&byte) = self.text.as_bytes().get(self.pos) {
            self.pos += 1;
            if byte == b'"' {
                return Ok(serde_json::from_str(&self.text[start..self.pos])?);
            }
            if byte == b'\\' {
                self.pos += 1;
            }
        }
        bail!("unterminated JSON string")
    }
    fn node(&mut self, depth: usize) -> Result<Node> {
        ensure!(depth < 64, "configuration nesting too deep");
        self.skip()?;
        let start = self.pos;
        let byte = *self
            .text
            .as_bytes()
            .get(start)
            .ok_or_else(|| anyhow::anyhow!("incomplete JSON"))?;
        let mut members = BTreeMap::new();
        let mut trailing_comma = false;
        let value = if byte == b'{' || byte == b'[' {
            self.pos += 1;
            let close = if byte == b'{' { b'}' } else { b']' };
            let mut array = Vec::new();
            loop {
                self.skip()?;
                if self.text.as_bytes().get(self.pos) == Some(&close) {
                    ensure!(
                        !trailing_comma || self.comments,
                        "trailing comma is not valid JSON"
                    );
                    self.pos += 1;
                    break;
                }
                if byte == b'{' {
                    let key_start = self.pos;
                    let key = self.string()?;
                    let key_end = self.pos;
                    self.skip()?;
                    ensure!(
                        self.text.as_bytes().get(self.pos) == Some(&b':'),
                        "expected JSON colon"
                    );
                    self.pos += 1;
                    let mut node = self.node(depth + 1)?;
                    node.key_span = Some(key_start..key_end);
                    ensure!(
                        members.insert(key, node).is_none(),
                        "duplicate JSON property; resolve it explicitly"
                    );
                } else {
                    array.push(self.node(depth + 1)?.value);
                }
                self.skip()?;
                trailing_comma = self.text.as_bytes().get(self.pos) == Some(&b',');
                if trailing_comma {
                    self.pos += 1;
                } else {
                    ensure!(
                        self.text.as_bytes().get(self.pos) == Some(&close),
                        "expected JSON comma or closing delimiter"
                    );
                    self.pos += 1;
                    break;
                }
            }
            if byte == b'{' {
                Value::Object(
                    members
                        .iter()
                        .map(|(k, n)| (k.clone(), n.value.clone()))
                        .collect(),
                )
            } else {
                Value::Array(array)
            }
        } else if byte == b'"' {
            Value::String(self.string()?)
        } else {
            while self
                .text
                .as_bytes()
                .get(self.pos)
                .is_some_and(|b| !b.is_ascii_whitespace() && !b",]} /".contains(b))
            {
                self.pos += 1;
            }
            serde_json::from_str(&self.text[start..self.pos])?
        };
        Ok(Node {
            key_span: None,
            value,
            span: start..self.pos,
            members,
            trailing_comma,
        })
    }
}
fn parse(text: &str, comments: bool) -> Result<Node> {
    let mut parser = Parser {
        text,
        pos: 0,
        comments,
    };
    let node = parser.node(0)?;
    parser.skip()?;
    ensure!(
        parser.pos == text.len() && node.value.is_object(),
        "configuration must be one JSON object"
    );
    Ok(node)
}
pub(super) fn value(text: &str, comments: bool) -> Result<Value> {
    Ok(parse(text, comments)?.value)
}

pub(super) fn rename(
    text: &str,
    key: &str,
    from: &str,
    to: &str,
    comments: bool,
) -> Result<String> {
    let root = parse(text, comments)?;
    let servers = root
        .members
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("missing MCP object"))?;
    ensure!(
        !servers.members.contains_key(to),
        "target MCP name already exists"
    );
    let span = servers
        .members
        .get(from)
        .and_then(|n| n.key_span.clone())
        .ok_or_else(|| anyhow::anyhow!("missing MCP key"))?;
    Ok(format!(
        "{}{}{}",
        &text[..span.start],
        serde_json::to_string(to)?,
        &text[span.end..]
    ))
}

fn insert(text: &str, object: &Node, key: &str, value: &Value) -> Result<String> {
    ensure!(
        object.value.is_object(),
        "MCP servers must be a JSON object"
    );
    let comma = if object.members.is_empty() || object.trailing_comma {
        ""
    } else {
        ","
    };
    let at = object.span.end - 1;
    Ok(format!(
        "{}{}\n  {}: {}\n{}",
        &text[..at],
        comma,
        serde_json::to_string(key)?,
        serde_json::to_string_pretty(value)?,
        &text[at..]
    ))
}

pub(super) fn is_proofstorm(name: &str, value: &Value) -> bool {
    let command = value["command"]
        .as_str()
        .or_else(|| value["command"][0].as_str());
    name == "proofstorm"
        || command.is_some_and(|s| {
            std::path::Path::new(s)
                .file_name()
                .is_some_and(|f| f == "proofstorm-mcp")
        })
}

pub(super) fn merge(
    text: Option<&str>,
    key: &str,
    entry: &Value,
    owned: &[Value],
    comments: bool,
) -> Result<String> {
    let text = text.unwrap_or("{\n}\n");
    let root = parse(text, comments)?;
    let output = if let Some(servers) = root.members.get(key) {
        ensure!(
            servers.value.is_object(),
            "MCP servers must be a JSON object"
        );
        for (name, node) in &servers.members {
            ensure!(
                name == "proofstorm" || !is_proofstorm(name, &node.value),
                "another project MCP entry already starts Proofstorm; resolve it explicitly"
            );
        }
        if let Some(existing) = servers.members.get("proofstorm") {
            ensure!(
                owned.contains(&existing.value),
                "the project Proofstorm entry is manual or was changed; refusing to overwrite it"
            );
            if existing.value == *entry {
                return Ok(text.into());
            }
            format!(
                "{}{}{}",
                &text[..existing.span.start],
                serde_json::to_string_pretty(entry)?,
                &text[existing.span.end..]
            )
        } else {
            insert(text, servers, "proofstorm", entry)?
        }
    } else {
        insert(
            text,
            &root,
            key,
            &Value::Object(Map::from_iter([("proofstorm".into(), entry.clone())])),
        )?
    };
    ensure!(
        value(&output, comments)?[key]["proofstorm"] == *entry,
        "generated configuration did not round-trip"
    );
    Ok(output)
}
