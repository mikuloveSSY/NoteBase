use crate::domain::{AppError, Result};
use serde_json::{Map, Value};

/// Parsed note metadata and the byte offset at which the Markdown body starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Metadata {
    pub fields: Value,
    pub error: Option<String>,
    pub body_start: usize,
}

#[derive(Debug, Clone, Copy)]
struct FrontmatterRange {
    bom_len: usize,
    yaml_start: usize,
    yaml_end: usize,
    body_start: usize,
}

enum Frontmatter {
    Absent { bom_len: usize },
    Present(FrontmatterRange),
    Invalid(String),
}

fn empty_fields() -> Value {
    Value::Object(Map::new())
}

fn bom_len(text: &str) -> usize {
    text.strip_prefix('\u{feff}')
        .map_or(0, |rest| text.len() - rest.len())
}

fn located_error(error: serde_yaml::Error) -> String {
    match error.location() {
        Some(location) => format!(
            "frontmatter YAML error at line {} column {} (第{}行第{}列): {}",
            location.line() + 1,
            location.column(),
            location.line() + 1,
            location.column(),
            error
        ),
        None => format!("frontmatter YAML error: {error}"),
    }
}

fn missing_closing_error() -> String {
    "frontmatter YAML error at line 1 column 1 (第1行第1列): 缺少结束分隔符".to_owned()
}

fn parse_yaml(yaml: &str) -> std::result::Result<Value, String> {
    if yaml.trim().is_empty() {
        return Ok(empty_fields());
    }

    let parsed = serde_yaml::from_str::<serde_yaml::Value>(yaml).map_err(located_error)?;
    let value = serde_json::to_value(parsed).map_err(|error| {
        format!("frontmatter YAML error at line 2 column 1 (第2行第1列): {error}")
    })?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(
            "frontmatter YAML error at line 1 column 1 (第1行第1列): frontmatter 必须是对象"
                .to_owned(),
        )
    }
}

fn detect_frontmatter(text: &str) -> Frontmatter {
    let bom = bom_len(text);
    let bytes = text.as_bytes();
    let opening = &bytes[bom..];
    let Some(opening_newline_len) = (if opening.starts_with(b"---\r\n") {
        Some(2)
    } else if opening.starts_with(b"---\n") {
        Some(1)
    } else {
        None
    }) else {
        return Frontmatter::Absent { bom_len: bom };
    };

    let yaml_start = bom + 3 + opening_newline_len;
    let mut line_start = yaml_start;
    while line_start < text.len() {
        let Some(relative_newline) = text[line_start..].find('\n') else {
            if &text[line_start..] == "---" {
                return Frontmatter::Present(FrontmatterRange {
                    bom_len: bom,
                    yaml_start,
                    yaml_end: line_start,
                    body_start: text.len(),
                });
            }
            break;
        };

        let line_end = line_start + relative_newline;
        let content_end = if line_end > line_start && bytes[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };
        if &text[line_start..content_end] == "---" {
            return Frontmatter::Present(FrontmatterRange {
                bom_len: bom,
                yaml_start,
                yaml_end: line_start,
                body_start: line_end + 1,
            });
        }
        line_start = line_end + 1;
    }

    Frontmatter::Invalid(missing_closing_error())
}

fn inspect_detected(text: &str, detected: Frontmatter) -> Metadata {
    match detected {
        Frontmatter::Absent { .. } => Metadata {
            fields: empty_fields(),
            error: None,
            body_start: 0,
        },
        Frontmatter::Invalid(error) => Metadata {
            fields: empty_fields(),
            error: Some(error),
            body_start: 0,
        },
        Frontmatter::Present(range) => match parse_yaml(&text[range.yaml_start..range.yaml_end]) {
            Ok(fields) => Metadata {
                fields,
                error: None,
                body_start: range.body_start,
            },
            Err(error) => Metadata {
                fields: empty_fields(),
                error: Some(error),
                body_start: 0,
            },
        },
    }
}

/// Parse an optional YAML frontmatter block without modifying the supplied text.
pub fn inspect(text: &str) -> Metadata {
    inspect_detected(text, detect_frontmatter(text))
}

fn invalid_field(key: &str, expected: &str) -> AppError {
    AppError::new(
        "E_INTERNAL",
        format!("frontmatter 字段 {key} 必须是{expected}"),
    )
}

fn validate_field(key: &str, value: &Value) -> Result<()> {
    match key {
        "tags" | "aliases" => {
            let Some(values) = value.as_array() else {
                return Err(invalid_field(key, "字符串数组"));
            };
            if values.iter().any(|item| !item.is_string()) {
                return Err(invalid_field(key, "字符串数组"));
            }
        }
        "favorite" | "pinned" => {
            if !value.is_boolean() {
                return Err(invalid_field(key, "布尔值"));
            }
        }
        "title" | "created" | "updated" | "status" if !value.is_string() => {
            return Err(invalid_field(key, "字符串"));
        }
        _ => {}
    }
    Ok(())
}

fn validate_fields(fields: &Map<String, Value>) -> Result<()> {
    for (key, value) in fields {
        validate_field(key, value)?;
    }
    Ok(())
}

fn yaml_for(fields: &Map<String, Value>) -> Result<String> {
    serde_yaml::to_string(&Value::Object(fields.clone()))
        .map_err(|error| AppError::new("E_INTERNAL", format!("frontmatter 无法序列化: {error}")))
}

/// Apply metadata updates while retaining the Markdown body byte-for-byte.
///
/// A null update removes a key. A note without frontmatter receives a new block;
/// malformed frontmatter is rejected before any replacement text is produced.
pub fn patch(text: &str, updates: &Map<String, Value>) -> Result<String> {
    let detected = detect_frontmatter(text);
    let (mut fields, bom, body_start, has_frontmatter) = match detected {
        Frontmatter::Absent { bom_len } => (Map::new(), bom_len, bom_len, false),
        Frontmatter::Invalid(error) => return Err(AppError::new("E_INTERNAL", error)),
        Frontmatter::Present(range) => {
            let fields = parse_yaml(&text[range.yaml_start..range.yaml_end])
                .map_err(|error| AppError::new("E_INTERNAL", error))?;
            let Value::Object(fields) = fields else {
                return Err(AppError::new("E_INTERNAL", "frontmatter 必须是对象"));
            };
            (fields, range.bom_len, range.body_start, true)
        }
    };

    for (key, value) in updates {
        if !value.is_null() {
            validate_field(key, value)?;
        }
    }

    let mut changed = false;
    for (key, value) in updates {
        if value.is_null() {
            changed |= fields.remove(key).is_some();
        } else {
            changed |= fields.get(key) != Some(value);
            fields.insert(key.clone(), value.clone());
        }
    }

    if !changed {
        return Ok(text.to_owned());
    }
    validate_fields(&fields)?;

    let line_ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let yaml = yaml_for(&fields)?;
    let yaml = if line_ending == "\r\n" {
        yaml.replace('\n', "\r\n")
    } else {
        yaml
    };

    let mut output = String::with_capacity(text.len() + yaml.len() + 16);
    output.push_str(&text[..bom]);
    output.push_str("---");
    output.push_str(line_ending);
    output.push_str(&yaml);
    if !yaml.ends_with(line_ending) {
        output.push_str(line_ending);
    }
    output.push_str("---");
    output.push_str(line_ending);
    output.push_str(&text[body_start..]);

    if !has_frontmatter {
        debug_assert_eq!(&text[bom..], &text[body_start..]);
    }
    Ok(output)
}

/// Match an exact tag or any descendant of a slash-separated parent tag.
pub fn tag_matches(tags: &[String], query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    tags.iter().any(|tag| {
        tag == query
            || tag
                .strip_prefix(query)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

/// Replace only the three template placeholders defined by FR-ORG-06.
pub fn render_template(template: &str, title: &str, date: &str, time: &str) -> String {
    let mut output = String::with_capacity(template.len());
    let mut cursor = 0;
    while cursor < template.len() {
        let remaining = &template[cursor..];
        let replacement = if remaining.starts_with("{{title}}") {
            Some(("{{title}}", title))
        } else if remaining.starts_with("{{date}}") {
            Some(("{{date}}", date))
        } else if remaining.starts_with("{{time}}") {
            Some(("{{time}}", time))
        } else {
            None
        };

        if let Some((placeholder, value)) = replacement {
            output.push_str(value);
            cursor += placeholder.len();
        } else {
            let character = remaining
                .chars()
                .next()
                .expect("cursor always points at a character boundary");
            output.push(character);
            cursor += character.len_utf8();
        }
    }
    output
}
