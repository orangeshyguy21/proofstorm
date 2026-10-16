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

/// The MCP servers object at `path`, or null when it is absent.
pub(super) fn servers<'v>(value: &'v Value, path: &[&str]) -> &'v Value {
    path.iter().fold(value, |value, key| &value[*key])
}

/// Follow `path` as far as it exists; every existing level must be an object.
fn descend<'n>(root: &'n Node, path: &[&str]) -> Result<(&'n Node, usize)> {
    let mut node = root;
    for (depth, key) in path.iter().enumerate() {
        let Some(child) = node.members.get(*key) else {
            return Ok((node, depth));
        };
        ensure!(child.value.is_object(), "MCP servers must be a JSON object");
        node = child;
    }
    Ok((node, path.len()))
}

fn existing<'n>(root: &'n Node, path: &[&str]) -> Result<&'n Node> {
    let (servers, depth) = descend(root, path)?;
    ensure!(depth == path.len(), "missing MCP object");
    Ok(servers)
}

pub(super) fn rename(
    text: &str,
    path: &[&str],
    from: &str,
    to: &str,
    comments: bool,
) -> Result<String> {
    let root = parse(text, comments)?;
    let servers = existing(&root, path)?;
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

/// Delete one member and the comma that separated it. Other bytes, including
/// comments, stay; the result must equal the original minus that member.
pub(super) fn remove(text: &str, path: &[&str], name: &str, comments: bool) -> Result<String> {
    let root = parse(text, comments)?;
    let object = existing(&root, path)?;
    let member = object
        .members
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("missing MCP key"))?;
    let key = member
        .key_span
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing MCP key"))?;
    let bytes = text.as_bytes();
    let comma_after = |pos| -> Result<Option<usize>> {
        let mut parser = Parser {
            text,
            pos,
            comments,
        };
        parser.skip()?;
        Ok((bytes.get(parser.pos) == Some(&b',')).then_some(parser.pos))
    };
    // Members are stored by name, so neighbours are found by position.
    let mut previous_comma = None;
    let mut end = member.span.end;
    if let Some(comma) = comma_after(end)? {
        end = comma + 1;
    } else if let Some(previous) = object
        .members
        .values()
        .map(|node| node.span.end)
        .filter(|&end| end < key.start)
        .max()
    {
        previous_comma =
            Some(comma_after(previous)?.ok_or_else(|| anyhow::anyhow!("expected JSON comma"))?);
    }
    // Drop the member's own line when nothing else shares it.
    let mut start = key.start;
    while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
        start -= 1;
    }
    while matches!(bytes.get(end), Some(b' ' | b'\t')) {
        end += 1;
    }
    if bytes.get(end) == Some(&b'\n') && (start == 0 || bytes[start - 1] == b'\n') {
        end += 1;
    }
    let mut output = text.to_owned();
    output.replace_range(start..end, "");
    if let Some(comma) = previous_comma {
        output.remove(comma);
    }
    let mut expected = parse(text, comments)?.value;
    path.iter()
        .try_fold(&mut expected, |value, key| value.get_mut(*key))
        .and_then(Value::as_object_mut)
        .and_then(|servers| servers.remove(name));
    ensure!(
        value(&output, comments)? == expected,
        "removing the MCP entry did not round-trip"
    );
    Ok(output)
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
    path: &[&str],
    entry: &Value,
    owned: &[Value],
    comments: bool,
) -> Result<String> {
    let text = text.unwrap_or("{\n}\n");
    let root = parse(text, comments)?;
    let (object, depth) = descend(&root, path)?;
    let output = if depth == path.len() {
        for (name, node) in &object.members {
            ensure!(
                name == "proofstorm" || !is_proofstorm(name, &node.value),
                "another project MCP entry already starts Proofstorm; resolve it explicitly"
            );
        }
        if let Some(existing) = object.members.get("proofstorm") {
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
            insert(text, object, "proofstorm", entry)?
        }
    } else {
        // Create the missing levels, e.g. `servers` inside an existing `mcp`.
        let missing = path[depth + 1..].iter().rev().fold(
            Value::Object(Map::from_iter([("proofstorm".into(), entry.clone())])),
            |inner, key| Value::Object(Map::from_iter([((*key).into(), inner)])),
        );
        insert(text, object, path[depth], &missing)?
    };
    ensure!(
        servers(&value(&output, comments)?, path)["proofstorm"] == *entry,
        "generated configuration did not round-trip"
    );
    Ok(output)
}
