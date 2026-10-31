use super::{
    files::{self, Files, ASSET_LIMIT},
    LocalDataService,
};
use crate::{
    domain::{field, AppError, Result},
    links::{self, LinkNote},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use comrak::{nodes::NodeValue, Arena};
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

pub trait Renderer {
    fn to_html(&self, markdown: &str) -> Result<String>;
}
pub struct ComrakRenderer;
impl Renderer for ComrakRenderer {
    fn to_html(&self, markdown: &str) -> Result<String> {
        Ok(comrak::markdown_to_html(
            markdown,
            &crate::index::extract::markdown_options(),
        ))
    }
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn uri(path: &str) -> String {
    path.split('/')
        .map(|x| utf8_percent_encode(x, NON_ALPHANUMERIC).to_string())
        .collect::<Vec<_>>()
        .join("/")
}
fn relative(source: &str, target: &str) -> String {
    let a: Vec<_> = source.split('/').collect();
    let b: Vec<_> = target.split('/').collect();
    let n = a[..a.len() - 1]
        .iter()
        .zip(&b)
        .take_while(|(x, y)| x == y)
        .count();
    format!("{}{}", "../".repeat(a.len() - 1 - n), b[n..].join("/"))
}
fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| AppError::new("E_ENCODING_INVALID", "需要 UTF-8 内容"))
}
fn link_notes(files: &Files) -> Result<Vec<LinkNote>> {
    let mut out = Vec::new();
    for path in files.entries()?.into_iter().filter(|p| {
        p.ends_with(".md") && !p.starts_with("trash/") && !p.starts_with("attachments/")
    }) {
        let raw = files.read_note(&path).unwrap_or_default();
        let body = text(&raw)?.to_owned();
        let (meta, _) = crate::domain::parse_frontmatter(body.trim_start_matches('\u{feff}'));
        let title = meta["title"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                Path::new(&path)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into()
            });
        let aliases = match &meta["aliases"] {
            Value::String(s) => vec![s.clone()],
            Value::Array(v) => v
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => vec![],
        };
        out.push(LinkNote {
            path,
            title,
            aliases,
            body,
        });
    }
    Ok(out)
}
pub fn rewrite_links(source: &str, raw: &str, notes: &[LinkNote]) -> (String, Vec<Value>) {
    let mut body = raw.to_owned();
    let mut warnings = Vec::new();
    for link in links::parse(raw).into_iter().rev() {
        let r = links::resolve(source, &link.target, notes);
        if let Some(target) = r.target_path {
            if r.ambiguous {
                warnings.push(json!({"kind":"ambiguous_link","target":link.target,"candidates":r.candidates,"resolvedTo":target}));
            }
            let anchor = link
                .target
                .split_once('#')
                .map(|(_, a)| format!("#{}", uri(a)))
                .unwrap_or_default();
            let label = link
                .label
                .as_deref()
                .unwrap_or(&link.target)
                .replace('\\', "\\\\")
                .replace('[', "\\[")
                .replace(']', "\\]");
            body.replace_range(
                link.start..link.end,
                &format!("[{label}]({}{anchor})", uri(&relative(source, &target))),
            );
        } else {
            warnings.push(json!({"kind":"unresolved_link","target":link.target,"line":link.line}));
        }
    }
    warnings.reverse();
    (body, warnings)
}
fn asset_path(source: &str, url: &str) -> Result<String> {
    if url.starts_with('/') || url.contains(':') || url.contains('\\') {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "附件 URL 不允许绝对路径或协议",
        ));
    }
    let decoded = percent_decode_str(url.split(['?', '#']).next().unwrap_or(url))
        .decode_utf8()
        .map_err(|_| AppError::new("E_PATH_INVALID", "无效 URL 编码"))?;
    let mut parts: Vec<_> = source.split('/').collect();
    parts.pop();
    for p in decoded.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(AppError::new("E_PATH_INVALID", "附件越界"));
                }
            }
            _ => parts.push(p),
        }
    }
    let out = parts.join("/");
    crate::domain::validate_rel(&out, false)?;
    Ok(out)
}
fn mime(path: &str) -> Option<&'static str> {
    match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        _ => None,
    }
}
fn render(files: &Files, path: &str, raw: &str, notes: &[LinkNote]) -> Result<Value> {
    let (rewritten, mut warnings) = rewrite_links(path, raw, notes);
    let normalized = rewritten
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n");
    let (_, error) = crate::domain::parse_frontmatter(&normalized);
    let body = if error.is_none() && normalized.starts_with("---\n") {
        normalized[4..]
            .find("\n---")
            .map(|i| &normalized[4 + i + 4..])
            .unwrap_or(&normalized)
    } else {
        &normalized
    };
    let arena = Arena::new();
    let options = crate::index::extract::markdown_options();
    let root = comrak::parse_document(&arena, body, &options);
    let mut missing = Vec::new();
    let mut asset_bytes = 0u64;
    for node in root.descendants() {
        let mut data = node.data.borrow_mut();
        if let NodeValue::Image(image) = &mut data.value {
            let original = image.url.clone();
            let bytes = asset_path(path, &original).and_then(|p| {
                let m = mime(&p).ok_or_else(|| {
                    AppError::new("E_FORMAT_UNSUPPORTED", "只内联 PNG/JPEG/GIF/WebP/AVIF 图片")
                })?;
                let b = files::read_limit(&files.path(&p)?, ASSET_LIMIT)?;
                Ok((m, b))
            });
            match bytes {
                Ok((m, b)) if asset_bytes + b.len() as u64 <= ASSET_LIMIT => {
                    asset_bytes += b.len() as u64;
                    image.url = format!("data:{m};base64,{}", STANDARD.encode(b));
                }
                _ => {
                    missing.push(original);
                    image.url = String::new();
                }
            }
        } else if let NodeValue::Link(link) = &mut data.value {
            let url = link.url.to_ascii_lowercase();
            if !(url.starts_with("https://")
                || url.starts_with("http://")
                || url.starts_with("mailto:")
                || url.starts_with('#')
                || (!url.contains(':') && !url.starts_with("//")))
            {
                link.url = String::new();
                warnings.push(json!({"kind":"blocked_url"}));
            }
        }
    }
    let mut fragment = String::new();
    comrak::format_html(root, &options, &mut fragment)
        .map_err(|e| AppError::new("E_INTERNAL", e.to_string()))?;
    let title = notes
        .iter()
        .find(|n| n.path == path)
        .map(|n| n.title.as_str())
        .unwrap_or(path);
    let html=format!("<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><meta name=\"viewport\" content=\"width=device-width\"><title>{}</title><style>body{{max-width:860px;margin:36px auto;padding:0 24px;font:16px/1.8 system-ui,sans-serif;color:#243b30}}pre{{overflow:auto;background:#f2f5f1;padding:16px}}img{{max-width:100%}}table{{border-collapse:collapse}}td,th{{border:1px solid #ccc;padding:6px 12px}}blockquote{{border-left:3px solid #bbb;margin-left:0;padding-left:16px}}@media print{{body{{margin:0}}}}</style></head><body>{fragment}</body></html>",escape(title));
    Ok(json!({"html":html,"missingAssets":missing,"warnings":warnings}))
}
pub fn export(s: &LocalDataService, p: &Value) -> Result<Value> {
    let path = field(p, "path")?;
    let format = field(p, "format")?;
    if format == "pdf" {
        return Err(AppError::new(
            "E_PDF_UNSUPPORTED",
            "当前平台 PDF 打印适配器尚未装配，请先导出 HTML",
        ));
    }
    if p.get("scope").and_then(Value::as_str) == Some("vault") {
        return super::backup::create(
            s,
            &json!({"targetDir":field(p,"targetPath")?,"description":"整库导出"}),
        );
    }
    let notes = link_notes(&s.files)?;
    let original = s.files.read_note(path)?;
    let raw = if p.get("preview") == Some(&Value::Bool(true)) {
        p.get("body")
            .and_then(Value::as_str)
            .unwrap_or(text(&original)?)
    } else {
        text(&original)?
    };
    if raw.len() > crate::domain::NOTE_SIZE_LIMIT {
        return Err(AppError::new("E_FILE_TOO_LARGE", "预览内容过大"));
    }
    let mut rendered = match format {
        "html" => render(&s.files, path, raw, &notes)?,
        "md" => {
            let (body, warnings) = rewrite_links(path, raw, &notes);
            json!({"markdown":body,"missingAssets":[],"warnings":warnings})
        }
        _ => return Err(AppError::new("E_FORMAT_UNSUPPORTED", "支持 md/html/pdf")),
    };
    if p.get("preview") == Some(&Value::Bool(true)) {
        return Ok(rendered);
    }
    let target = s.files.external(field(p, "targetPath")?)?;
    let expected = if format == "html" { "html" } else { "md" };
    if target.extension().and_then(|x| x.to_str()) != Some(expected) {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "目标扩展名与导出格式不一致",
        ));
    }
    if target.exists() {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "导出目标已存在，请选择新文件名",
        ));
    }
    if format == "md" {
        // Export into an otherwise unused directory to keep attachment paths portable.
        let parent = target
            .parent()
            .ok_or_else(|| AppError::new("E_PATH_INVALID", "目标目录无效"))?;
        files::directory(parent)?;
        if std::fs::read_dir(parent)
            .map_err(AppError::io)?
            .next()
            .is_some()
        {
            return Err(AppError::new(
                "E_PATH_INVALID",
                "Markdown 请导出到空目录，避免覆盖附件",
            ));
        }
        let mut attachments = BTreeMap::<String, Vec<u8>>::new();
        let mut md = rendered["markdown"].as_str().unwrap_or_default().to_owned();
        let mut replacements = Vec::new();
        let mut total = 0u64;
        let mut starts = vec![0];
        for (i, byte) in md.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(i + 1);
            }
        }
        let arena = Arena::new();
        let root = comrak::parse_document(&arena, &md, &crate::index::extract::markdown_options());
        let mut missing = Vec::new();
        for node in root.descendants() {
            let d = node.data.borrow();
            let url = match &d.value {
                NodeValue::Image(l) | NodeValue::Link(l) => &l.url,
                _ => continue,
            };
            if url.contains(':') || url.starts_with('#') {
                continue;
            }
            let Ok(rel) = asset_path(path, url) else {
                missing.push(url.clone());
                continue;
            };
            if rel.ends_with(".md") {
                continue;
            }
            if !attachments.contains_key(&rel) {
                match files::read_limit(&s.files.path(&rel)?, ASSET_LIMIT) {
                    Ok(bytes) => {
                        total += bytes.len() as u64;
                        if total > ASSET_LIMIT {
                            return Err(AppError::new(
                                "E_FILE_TOO_LARGE",
                                "单篇导出附件总量超过 100 MiB",
                            ));
                        }
                        attachments.insert(rel.clone(), bytes);
                    }
                    Err(_) => {
                        missing.push(url.clone());
                        continue;
                    }
                }
            }
            let pos = d.sourcepos;
            let span = starts
                .get(pos.start.line.saturating_sub(1))
                .zip(starts.get(pos.end.line.saturating_sub(1)))
                .and_then(|(a, b)| {
                    let start = a + pos.start.column.saturating_sub(1);
                    let end = (b + pos.end.column).min(md.len());
                    let segment = md.get(start..end)?;
                    let destination = segment.rfind("](")? + 2;
                    let offset = segment[destination..].find(url)? + destination;
                    Some((start + offset, start + offset + url.len(), uri(&rel)))
                });
            if let Some(span) = span {
                replacements.push(span);
            } else if let Some(warnings) = rendered["warnings"].as_array_mut() {
                warnings.push(json!({"kind":"asset_url_not_rebased","url":url,"copiedTo":rel}));
            }
        }
        replacements.sort_by_key(|r| std::cmp::Reverse(r.0));
        for (start, end, url) in replacements {
            md.replace_range(start..end, &url);
        }
        // Prepare all bytes before publishing the output. Existing destinations remain protected.
        let stage = tempfile::Builder::new()
            .prefix(".notebase-export-")
            .tempdir_in(parent)
            .map_err(AppError::io)?;
        for (rel, bytes) in &attachments {
            files::atomic(&stage.path().join(rel), bytes, false)?;
        }
        for (rel, _) in attachments {
            let ready = files::read_limit(&stage.path().join(&rel), ASSET_LIMIT)?;
            files::atomic(&parent.join(rel), &ready, false)?;
        }
        rendered["missingAssets"] = json!(missing);
        files::atomic(&target, md.as_bytes(), false)?;
    } else {
        files::atomic(
            &target,
            rendered["html"].as_str().unwrap_or_default().as_bytes(),
            false,
        )?;
    }
    Ok(
        json!({"filePath":target,"missingAssets":rendered["missingAssets"],"warnings":rendered["warnings"]}),
    )
}
