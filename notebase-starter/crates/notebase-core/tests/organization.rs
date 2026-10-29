use notebase_core::organization::{inspect, patch, render_template, tag_matches};
use serde_json::{json, Map, Value};

#[test]
fn inspect_reports_fields_and_exact_body_start_for_crlf() {
    let raw =
        "---\r\ntitle: 标题\r\ntags: [课程/算法, 复习]\r\ncustom: keep\r\n---\r\n正文\r\n第二行";

    let metadata = inspect(raw);

    assert_eq!(metadata.error, None);
    assert_eq!(metadata.fields["title"], "标题");
    assert_eq!(metadata.fields["tags"][0], "课程/算法");
    assert_eq!(metadata.fields["custom"], "keep");
    assert_eq!(metadata.body_start, raw.find("正文").unwrap());
}

#[test]
fn inspect_treats_invalid_and_duplicate_yaml_as_located_errors() {
    let invalid = "---\nthing: [broken\n---\n正文";
    let duplicate = "---\ntitle: first\ntitle: second\n---\n正文";

    let invalid_error = inspect(invalid)
        .error
        .expect("invalid YAML must have an error");
    assert!(
        invalid_error.contains("line 2"),
        "missing original line location: {invalid_error}"
    );
    assert_eq!(inspect(invalid).fields, json!({}));
    assert_eq!(inspect(invalid).body_start, 0);

    let duplicate_error = inspect(duplicate)
        .error
        .expect("duplicate YAML key must have an error");
    // serde_yaml reports the start of the mapping; the opening delimiter is line 1.
    assert!(
        duplicate_error.contains("line 2"),
        "missing original mapping location: {duplicate_error}"
    );
    assert_eq!(inspect(duplicate).fields, json!({}));
    assert_eq!(inspect(duplicate).body_start, 0);
}

#[test]
fn patch_retains_custom_fields_and_body_bytes_while_preserving_crlf() {
    let body = "# 正文\r\n\r\n尾部 \r\n";
    let raw = format!("---\r\ntitle: 旧标题\r\ncustom: keep\r\nfavorite: false\r\n---\r\n{body}");
    let mut updates = Map::new();
    updates.insert("title".into(), json!("新标题"));
    updates.insert("tags".into(), json!(["课程/算法"]));
    updates.insert("favorite".into(), json!(true));

    let output = patch(&raw, &updates).expect("valid metadata patch");

    assert!(output.ends_with(body), "body was rewritten: {output:?}");
    assert!(!output
        .as_bytes()
        .windows(2)
        .any(|window| window[1] == b'\n' && window[0] != b'\r'));
    let metadata = inspect(&output);
    assert_eq!(metadata.error, None);
    assert_eq!(metadata.fields["title"], "新标题");
    assert_eq!(metadata.fields["custom"], "keep");
    assert_eq!(metadata.fields["favorite"], true);
    assert_eq!(metadata.fields["tags"], json!(["课程/算法"]));
}

#[test]
fn patch_deletes_null_fields_and_inserts_frontmatter_when_missing() {
    let raw = "正文开头\n保留字节\n";
    let mut updates = Map::new();
    updates.insert("title".into(), json!("新笔记"));
    updates.insert("favorite".into(), json!(true));

    let inserted = patch(raw, &updates).expect("missing frontmatter receives one");
    assert!(inserted.ends_with(raw));
    let metadata = inspect(&inserted);
    assert_eq!(metadata.fields["title"], "新笔记");
    assert_eq!(metadata.fields["favorite"], true);

    let mut deletes = Map::new();
    deletes.insert("title".into(), Value::Null);
    deletes.insert("favorite".into(), Value::Null);
    let deleted = patch(&inserted, &deletes).expect("null removes metadata keys");
    let metadata = inspect(&deleted);
    assert!(metadata.fields.get("title").is_none());
    assert!(metadata.fields.get("favorite").is_none());
    assert!(deleted.ends_with(raw));
}

#[test]
fn patch_rejects_wrong_metadata_types_without_returning_text() {
    for (key, value) in [
        ("tags", json!("课程")),
        ("aliases", json!(["ok", 7])),
        ("favorite", json!("yes")),
        ("pinned", json!(0)),
    ] {
        let mut updates = Map::new();
        updates.insert(key.into(), value);
        let error = patch("---\ntitle: 原样\n---\n正文", &updates)
            .expect_err("wrong metadata type must be rejected");
        assert_eq!(error.code, "E_INTERNAL");
        assert!(error.message.contains(key));
    }
}

#[test]
fn invalid_existing_frontmatter_is_never_rewritten_by_patch() {
    let raw = "---\ntags: [broken\n---\n原始正文\r\n";
    let mut updates = Map::new();
    updates.insert("favorite".into(), json!(true));

    let error = patch(raw, &updates).expect_err("invalid YAML must be rejected");

    assert!(error.message.contains("line") || error.message.contains("行"));
}

#[test]
fn tag_matches_exact_tags_and_parent_segments() {
    let tags = vec!["课程/算法".to_owned(), "生活".to_owned()];

    assert!(tag_matches(&tags, "课程"));
    assert!(tag_matches(&tags, "课程/算法"));
    assert!(!tag_matches(&tags, "课"));
    assert!(!tag_matches(&tags, "课程学"));
    assert!(tag_matches(&tags, "生活"));
    assert!(!tag_matches(&tags, "不存在"));
}

#[test]
fn render_template_replaces_only_the_three_supported_placeholders() {
    let template = "# {{title}}\n{{date}} {{time}}\n{{title}}\n{{author}}\n{{ date }}";

    assert_eq!(
        render_template(template, "算法", "2026-09-28", "14:30:00"),
        "# 算法\n2026-09-28 14:30:00\n算法\n{{author}}\n{{ date }}"
    );
}
