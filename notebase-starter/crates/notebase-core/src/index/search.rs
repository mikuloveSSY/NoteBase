use crate::domain::{AppError, Result};
use rusqlite::{
    params_from_iter, types::Value as SqlValue, Connection, OpenFlags, OptionalExtension,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Filters {
    #[serde(default)]
    pub tags: Vec<String>,
    pub path_prefix: Option<String>,
    pub mtime_from_ms: Option<i64>,
    pub mtime_to_ms: Option<i64>,
    pub created_from_ms: Option<i64>,
    pub created_to_ms: Option<i64>,
    pub has_attachment: Option<bool>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchQuery {
    pub query: String,
    #[serde(default)]
    pub filters: Filters,
    #[serde(default = "sort_default")]
    pub sort: String,
    #[serde(default = "limit_default")]
    pub limit: usize,
    pub cursor: Option<String>,
}
fn sort_default() -> String {
    "relevance".into()
}
fn limit_default() -> usize {
    50
}
fn syntax(message: &str) -> AppError {
    AppError::new("E_QUERY_SYNTAX", message)
}
struct Term {
    field: String,
    text: String,
    negative: bool,
}
fn parse(input: &str) -> Result<Vec<Term>> {
    if input.chars().count() > 1024 {
        return Err(AppError::new("E_QUERY_UNSUPPORTED", "查询超过 1024 字符"));
    }
    let mut chars = input.chars().peekable();
    let mut terms = Vec::new();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let negative = chars.peek() == Some(&'-');
        if negative {
            chars.next();
        }
        let mut text = String::new();
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if c == '"' {
                quoted = !quoted;
                chars.next();
                continue;
            }
            if c.is_whitespace() && !quoted {
                break;
            }
            text.push(c);
            chars.next();
        }
        if quoted {
            return Err(syntax("双引号未闭合"));
        }
        let (field, text) = match text.split_once(':') {
            Some((field, value)) if ["tag", "path", "title"].contains(&field) => {
                (field.to_owned(), value.to_owned())
            }
            Some(_) => {
                return Err(AppError::new(
                    "E_QUERY_UNSUPPORTED",
                    "仅支持 tag:、path:、title: 字段",
                ))
            }
            None => (String::new(), text),
        };
        if text.is_empty() {
            return Err(syntax("查询词不能为空"));
        }
        terms.push(Term {
            field,
            text,
            negative,
        });
        if terms.len() > 32 {
            return Err(AppError::new("E_QUERY_UNSUPPORTED", "查询词超过 32 个"));
        }
    }
    if terms.is_empty() {
        return Err(syntax("请输入查询词"));
    }
    Ok(terms)
}
fn like(text: &str) -> String {
    format!(
        "%{}%",
        text.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}
fn bind(values: &mut Vec<SqlValue>, value: impl Into<SqlValue>) -> String {
    values.push(value.into());
    format!("?{}", values.len())
}
fn text_match(column: &str, parameter: &str) -> String {
    format!("{column} LIKE {parameter} ESCAPE '\\'")
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn snippet(body: &str, terms: &[Term]) -> String {
    let chars: Vec<char> = body.chars().collect();
    let folded: Vec<char> = chars.iter().map(char::to_ascii_lowercase).collect();
    let mut spans = Vec::new();
    for term in terms.iter().filter(|t| !t.negative && t.field.is_empty()) {
        let needle: Vec<char> = term.text.chars().map(|c| c.to_ascii_lowercase()).collect();
        for (i, part) in folded.windows(needle.len()).enumerate() {
            if part == needle {
                spans.push((i, i + needle.len()));
            }
        }
    }
    let start = spans
        .iter()
        .map(|s| s.0)
        .min()
        .unwrap_or(0)
        .saturating_sub(40);
    let end = (start + 160).min(chars.len());
    let mut output = if start > 0 {
        "…".to_owned()
    } else {
        String::new()
    };
    let mut bold = false;
    for (i, c) in chars.iter().enumerate().take(end).skip(start) {
        let marked = spans.iter().any(|&(a, b)| i >= a && i < b);
        if marked != bold {
            output.push_str(if marked { "<b>" } else { "</b>" });
            bold = marked;
        }
        output.push_str(&escape(&c.to_string()));
    }
    if bold {
        output.push_str("</b>");
    }
    if end < chars.len() {
        output.push('…');
    }
    output
}
pub fn search_path(path: &Path, query: &Value) -> Result<Value> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|e| super::unavailable(e.to_string()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| super::unavailable(e.to_string()))?;
    conn.execute_batch("PRAGMA query_only=ON")
        .map_err(|e| super::unavailable(e.to_string()))?;
    search(&conn, query)
}
pub fn search(connection: &Connection, payload: &Value) -> Result<Value> {
    let started = Instant::now();
    let mut q: SearchQuery =
        serde_json::from_value(payload.clone()).map_err(|e| syntax(&e.to_string()))?;
    if !(1..=200).contains(&q.limit) {
        return Err(syntax("limit 必须在 1–200 之间"));
    }
    if !["relevance", "mtime"].contains(&q.sort.as_str()) {
        return Err(AppError::new(
            "E_QUERY_UNSUPPORTED",
            "sort 仅支持 relevance 或 mtime",
        ));
    }
    for (from, to) in [
        (q.filters.mtime_from_ms, q.filters.mtime_to_ms),
        (q.filters.created_from_ms, q.filters.created_to_ms),
    ] {
        if matches!((from,to),(Some(a),Some(b)) if a>b) {
            return Err(syntax("时间范围起点晚于终点"));
        }
    }
    let terms = parse(&q.query)?;
    let short = terms
        .iter()
        .any(|t| t.field.is_empty() && t.text.chars().count() < 3);
    let tx = connection
        .unchecked_transaction()
        .map_err(|e| super::unavailable(e.to_string()))?;
    let state: String = tx
        .query_row("SELECT value FROM meta WHERE key='build_state'", [], |r| {
            r.get(0)
        })
        .map_err(|e| super::unavailable(e.to_string()))?;
    if state != "ready" {
        return Err(super::unavailable("索引未就绪，请重新打开库完成构建"));
    }
    let build: String = tx
        .query_row(
            "SELECT value FROM meta WHERE key='last_full_build_ms'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| super::unavailable(e.to_string()))?;
    let revision: String = tx
        .query_row(
            "SELECT value FROM meta WHERE key='index_revision'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| super::unavailable(e.to_string()))?
        .unwrap_or_default();
    let cursor = q.cursor.take();
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(format!(
            "{build}:{revision}:{}",
            serde_json::to_string(&q).unwrap()
        ))
    );
    let offset = if let Some(cursor) = cursor {
        let (signature, offset) = cursor.split_once(':').ok_or_else(|| syntax("无效游标"))?;
        if signature != fingerprint {
            return Err(syntax("游标对应的查询或索引已变化"));
        }
        offset.parse::<usize>().map_err(|_| syntax("无效游标"))?
    } else {
        0
    };
    if offset > i64::MAX as usize {
        return Err(syntax("游标超出范围"));
    }
    let mut values = Vec::new();
    let mut predicates = Vec::new();
    let mut scores = Vec::new();
    let mut matches = Vec::new();
    let mut ctes = Vec::new();
    for (i, term) in terms.iter().enumerate() {
        let p = bind(&mut values, like(&term.text));
        let indexed = if term.field.is_empty() && term.text.chars().count() >= 3 {
            let fts = bind(
                &mut values,
                format!("\"{}\"", term.text.replace('"', "\"\"")),
            );
            ctes.push(format!("fts{i} AS MATERIALIZED (SELECT rowid AS id FROM note_fts WHERE note_fts MATCH {fts})"));
            format!("rowid IN (SELECT id FROM fts{i}) AND ")
        } else {
            String::new()
        };
        ctes.push(format!(
            "title{i} AS MATERIALIZED (SELECT rowid AS id FROM notes WHERE {indexed}{})",
            text_match("title", &p)
        ));
        ctes.push(format!("alias{i} AS MATERIALIZED (SELECT rowid AS id FROM notes WHERE path IN (SELECT note_path FROM aliases WHERE {}))",text_match("alias", &p)));
        ctes.push(format!("tag{i} AS MATERIALIZED (SELECT rowid AS id FROM notes WHERE path IN (SELECT note_path FROM tags WHERE {}))",text_match("tag", &p)));
        let title = format!("n.rowid IN (SELECT id FROM title{i})");
        let alias = format!("n.rowid IN (SELECT id FROM alias{i})");
        let tag = format!("n.rowid IN (SELECT id FROM tag{i})");
        let expression = match term.field.as_str() {
            "title" => title.clone(),
            "path" => text_match("n.path", &p),
            "tag" => tag.clone(),
            _ => {
                // 各命中来源只扫描一次；避免对全库逐行运行关联子查询。
                let body = if term.text.chars().count() >= 3 {
                    format!("SELECT id FROM fts{i}")
                } else {
                    // 无 ASCII 字母时，字节子串是必要条件；最终仍以转义 LIKE 判定。
                    let prefilter = if !term.text.chars().any(|c| c.is_ascii_alphabetic()) {
                        let literal = bind(&mut values, term.text.clone());
                        format!("instr(c2,{literal})>0 AND ")
                    } else {
                        String::new()
                    };
                    format!(
                        "SELECT id FROM note_fts_content WHERE {prefilter}{}",
                        text_match("c2", &p)
                    )
                };
                format!("n.rowid IN (SELECT id FROM title{i} UNION ALL SELECT id FROM alias{i} UNION ALL SELECT id FROM tag{i} UNION ALL {body})")
            }
        };
        predicates.push(if term.negative {
            format!("NOT ({expression})")
        } else {
            expression
        });
        if !term.negative {
            matches.push(format!(
                "WHEN {title} THEN 'title' WHEN {alias} THEN 'alias' WHEN {tag} THEN 'tag'"
            ));
            scores.push(format!(
                "CASE WHEN {title} THEN 10 WHEN {alias} THEN 6 WHEN {tag} THEN 4 ELSE 1 END"
            ));
        }
    }
    for tag in &q.filters.tags {
        let p = bind(&mut values, tag.clone());
        predicates.push(format!(
            "EXISTS(SELECT 1 FROM tags t WHERE t.note_path=n.path AND t.tag={p})"
        ));
    }
    if let Some(prefix) = &q.filters.path_prefix {
        let p = bind(
            &mut values,
            format!(
                "{}%",
                prefix
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            ),
        );
        predicates.push(text_match("n.path", &p));
    }
    for (column, op, value) in [
        ("mtime_ms", ">=", q.filters.mtime_from_ms),
        ("mtime_ms", "<=", q.filters.mtime_to_ms),
        ("created_ms", ">=", q.filters.created_from_ms),
        ("created_ms", "<=", q.filters.created_to_ms),
    ] {
        if let Some(value) = value {
            let p = bind(&mut values, value);
            predicates.push(format!("n.{column}{op}{p}"));
        }
    }
    if let Some(value) = q.filters.has_attachment {
        let p = bind(&mut values, i64::from(value));
        predicates.push(format!("n.has_attachment={p}"));
    }
    let score = if scores.is_empty() {
        "0".into()
    } else {
        scores.join("+")
    };
    let order = if q.sort == "mtime" {
        "n.mtime_ms DESC,n.path ASC"
    } else {
        "score DESC,n.mtime_ms DESC,n.path ASC"
    };
    let matched = if matches.is_empty() {
        "'body'".into()
    } else {
        format!("CASE {} ELSE 'body' END", matches.join(" "))
    };
    // 同一快照内物化候选一次，共用计数与分页；正文只在首屏选出后读取。
    let outer_order = order.replace("n.", "p.");
    let sql = format!("WITH {}, candidates AS MATERIALIZED (SELECT n.rowid AS id FROM notes n WHERE {}), page AS (SELECT n.rowid AS id,n.path,n.title,n.mtime_ms,({score}) AS score,{matched} AS matched FROM candidates c JOIN notes n ON n.rowid=c.id ORDER BY {order} LIMIT {} OFFSET {offset}) SELECT p.path,p.title,f.c2,p.mtime_ms,p.score,p.matched,t.total FROM (SELECT count(*) AS total FROM candidates) t LEFT JOIN page p ON 1 LEFT JOIN note_fts_content f ON f.id=p.id ORDER BY {outer_order}",ctes.join(","),predicates.join(" AND "),q.limit);
    let mut stmt = tx
        .prepare(&sql)
        .map_err(|e| super::unavailable(e.to_string()))?;
    let mut rows = stmt
        .query(params_from_iter(values.iter()))
        .map_err(|e| super::unavailable(e.to_string()))?;
    let mut items = Vec::new();
    let mut total = 0i64;
    while let Some(row) = rows.next().map_err(|e| super::unavailable(e.to_string()))? {
        let mut read = || -> rusqlite::Result<Option<Value>> {
            total = row.get(6)?;
            let Some(path) = row.get::<_, Option<String>>(0)? else {
                return Ok(None);
            };
            let title: String = row.get(1)?;
            let body: String = row.get(2)?;
            let mtime: i64 = row.get(3)?;
            let score: f64 = row.get(4)?;
            let matched: String = row.get(5)?;
            Ok(Some(
                json!({"path":path,"title":title,"snippet":snippet(&body,&terms),"score":score,"matchedIn":matched,"mtimeMs":mtime}),
            ))
        };
        if let Some(item) = read().map_err(|e| super::unavailable(e.to_string()))? {
            items.push(item);
        }
    }
    let next = offset + items.len();
    Ok(
        json!({"items":items,"nextCursor":if next<total as usize{Some(format!("{fingerprint}:{next}"))}else{None},"totalApprox":total,"tookMs":started.elapsed().as_millis() as u64,"degraded":short,"degradeReason":if short{Some("short-query-scan")}else{None}}),
    )
}
