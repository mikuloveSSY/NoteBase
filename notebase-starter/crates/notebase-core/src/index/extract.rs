//! 单一 comrak 配置入口，供成员三后续 renderer 复用。
use super::NoteRecord;
use crate::domain::{AppError, RelPath, Result, StoredNote};
use comrak::{nodes::NodeValue, Arena, Options};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn markdown_options() -> Options<'static> {
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;
    // comrak 0.57 无独立 tagfilter 开关；保持安全渲染，索引抽取不收录 HTML 节点。
    options.render.r#unsafe = false;
    options.extension.footnotes = false;
    options.extension.description_lists = false;
    options.extension.superscript = false;
    options
}

fn strings(value: Option<&Value>) -> Vec<String> {
    let mut values: Vec<String> = match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(item)) => vec![item.clone()],
        _ => Vec::new(),
    };
    values.retain(|v| !v.trim().is_empty());
    values.sort();
    values.dedup();
    values
}
fn timestamp(value: Option<&Value>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value?.as_str()?)
        .ok()
        .map(|v| v.timestamp_millis())
}

pub fn extract(path: RelPath, stored: StoredNote) -> Result<NoteRecord> {
    if stored.truncated {
        return Err(AppError::new(
            "E_FILE_TOO_LARGE",
            "超大文件不建立部分正文索引",
        ));
    }
    let text = std::str::from_utf8(&stored.raw)
        .map_err(|_| AppError::new("E_ENCODING_INVALID", "非法 UTF-8 不建立替换字符索引"))?;
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n");
    let (metadata, error) = crate::domain::parse_frontmatter(&text);
    // 仅在合法 YAML 时去掉 frontmatter；解析失败整篇作为正文，不重写源文件。
    let body = if error.is_none() && text.starts_with("---\n") {
        text[4..]
            .find("\n---")
            .map(|end| &text[4 + end + 4..])
            .unwrap_or(&text)
    } else {
        &text
    };
    let arena = Arena::new();
    let root = comrak::parse_document(&arena, body, &markdown_options());
    let mut plain = String::new();
    let mut has_attachment = false;
    for edge in root.traverse() {
        match edge {
            comrak::arena_tree::NodeEdge::Start(node) => match &node.data.borrow().value {
                NodeValue::Text(text) => plain.push_str(text),
                NodeValue::Code(code) => plain.push_str(&code.literal),
                NodeValue::CodeBlock(code) => {
                    plain.push_str(&code.literal);
                    plain.push('\n');
                }
                NodeValue::SoftBreak | NodeValue::LineBreak => plain.push('\n'),
                NodeValue::Image(link) | NodeValue::Link(link) => {
                    let url = link.url.trim_start_matches("./");
                    has_attachment |=
                        url.starts_with("attachments/") || url.starts_with("../attachments/");
                }
                _ => {}
            },
            comrak::arena_tree::NodeEdge::End(node) => match &node.data.borrow().value {
                NodeValue::Paragraph
                | NodeValue::Heading(_)
                | NodeValue::TableCell
                | NodeValue::Item(_) => plain.push('\n'),
                _ => {}
            },
        }
    }
    let title = metadata
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            path.as_str()
                .rsplit('/')
                .next()
                .unwrap()
                .rsplit_once('.')
                .map(|(name, _)| name)
                .unwrap_or(path.as_str())
                .into()
        });
    Ok(NoteRecord {
        path,
        title,
        body: plain,
        size_bytes: stored.size_bytes,
        mtime_ms: stored.mtime_ms,
        content_hash: format!("{:x}", Sha256::digest(&stored.raw)),
        created_ms: timestamp(metadata.get("created")),
        updated_ms: timestamp(metadata.get("updated")),
        has_attachment,
        aliases: strings(metadata.get("aliases")),
        tags: strings(metadata.get("tags")),
    })
}
