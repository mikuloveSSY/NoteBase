use notebase_core::{
    domain::RelPath,
    index::{search::search, sqlite::SqliteIndexStore, IndexStore, NoteRecord},
};
use serde_json::{json, Value};
fn fixture() -> (tempfile::TempDir, SqliteIndexStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteIndexStore::open(&dir.path().join("index.db"), "0123456789abcdef").unwrap();
    store.begin_build().unwrap();
    let notes = [
        (
            "课/A.md",
            "动态规划",
            "跨词边界 数据结构 <script>alert(1)</script> 100% a_b Alpha Beta",
            vec!["复习"],
            vec!["DP"],
            1,
            true,
        ),
        (
            "课/B.md",
            "数据结构",
            "动态规划与数据库",
            vec!["复习"],
            vec![],
            2,
            false,
        ),
        (
            "C.md",
            "其他",
            "只有动态排除",
            vec!["杂项"],
            vec![],
            3,
            false,
        ),
    ];
    for (path, title, body, tags, aliases, time, attachment) in notes {
        store
            .upsert_batch(&[NoteRecord {
                path: RelPath::new(path).unwrap(),
                title: title.into(),
                body: body.into(),
                size_bytes: body.len() as u64,
                mtime_ms: time,
                content_hash: "a".repeat(64),
                created_ms: Some(time as i64),
                updated_ms: None,
                has_attachment: attachment,
                tags: tags.into_iter().map(str::to_owned).collect(),
                aliases: aliases.into_iter().map(str::to_owned).collect(),
            }])
            .unwrap();
    }
    store.finish_build().unwrap();
    (dir, store)
}
fn query(store: &SqliteIndexStore, q: Value) -> Value {
    store.read(|conn| Ok(search(conn, &q))).unwrap().unwrap()
}
fn paths(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["path"].as_str().unwrap().into())
        .collect()
}
#[test]
fn chinese_long_short_substring_alias_tag_and_literal_symbols() {
    let (_d, s) = fixture();
    for (text, count) in [
        ("态规划", 2),
        ("动态", 3),
        ("DP", 1),
        ("复习", 2),
        ("词边界", 1),
        ("100%", 1),
        ("a_b", 1),
        ("\"Alpha Beta\"", 1),
        ("不存在", 0),
    ] {
        let p = query(&s, json!({"query":text}));
        assert_eq!(p["totalApprox"], count, "{text}");
    }
    assert_eq!(query(&s, json!({"query":"动态"}))["degraded"], true);
    assert_eq!(query(&s, json!({"query":"态规划"}))["degraded"], false);
}
#[test]
fn phrase_negation_fields_and_metadata_filters() {
    let (_d, s) = fixture();
    assert_eq!(
        paths(&query(&s, json!({"query":"动态 -排除"}))),
        vec!["课/A.md", "课/B.md"]
    );
    assert_eq!(
        paths(&query(&s, json!({"query":"tag:复习 path:课/ title:数据"}))),
        vec!["课/B.md"]
    );
    assert_eq!(
        paths(&query(
            &s,
            json!({"query":"动态","filters":{"tags":["复习"],"pathPrefix":"课/","mtimeFromMs":1,"mtimeToMs":2,"createdFromMs":1,"createdToMs":1,"hasAttachment":true}})
        )),
        vec!["课/A.md"]
    );
}
#[test]
fn stable_pagination_rejects_query_or_snapshot_mismatch() {
    let (_d, s) = fixture();
    let first = query(&s, json!({"query":"动态","sort":"mtime","limit":1}));
    assert_eq!(paths(&first), vec!["C.md"]);
    let second = query(
        &s,
        json!({"query":"动态","sort":"mtime","limit":1,"cursor":first["nextCursor"]}),
    );
    assert_eq!(paths(&second), vec!["课/B.md"]);
    assert!(s
        .read(|c| Ok(search(
            c,
            &json!({"query":"其他","sort":"mtime","limit":1,"cursor":first["nextCursor"]})
        )))
        .unwrap()
        .is_err());
    s.mark_offline().unwrap();
    assert_eq!(
        s.read(|c| Ok(search(c, &json!({"query":"动态"}))))
            .unwrap()
            .unwrap_err()
            .code,
        "E_INDEX_UNAVAILABLE"
    );
}
#[test]
fn malformed_queries_are_distinct_from_empty_results_and_html_is_escaped() {
    let (_d, s) = fixture();
    for text in ["", "\"broken", "title:", "-"] {
        assert_eq!(
            s.read(|c| Ok(search(c, &json!({"query":text}))))
                .unwrap()
                .unwrap_err()
                .code,
            "E_QUERY_SYNTAX"
        );
    }
    assert_eq!(
        s.read(|c| Ok(search(c, &json!({"query":"unknown:value"}))))
            .unwrap()
            .unwrap_err()
            .code,
        "E_QUERY_UNSUPPORTED"
    );
    let page = query(&s, json!({"query":"script"}));
    let snippet = page["items"][0]["snippet"].as_str().unwrap();
    assert!(!snippet.contains("<script>"));
    assert!(snippet.contains("&lt;<b>script</b>&gt;"));
}

#[test]
fn parallel_pool_queries_preserve_counts_and_empty_past_end_pages() {
    let (_dir, store) = fixture();
    std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    for _ in 0..20 {
                        assert_eq!(
                            store.search(&json!({"query":"动态"})).unwrap()["totalApprox"],
                            3
                        );
                        assert_eq!(
                            store.search(&json!({"query":"DP"})).unwrap()["totalApprox"],
                            1
                        );
                    }
                })
            })
            .collect();
        for job in jobs {
            job.join().unwrap();
        }
    });
    let first = query(&store, json!({"query":"动态","limit":1}));
    let signature = first["nextCursor"]
        .as_str()
        .unwrap()
        .split_once(':')
        .unwrap()
        .0;
    let page = query(
        &store,
        json!({"query":"动态","limit":1,"cursor":format!("{signature}:999")}),
    );
    assert_eq!(page["totalApprox"], 3);
    assert_eq!(page["items"], json!([]));
    assert!(page["nextCursor"].is_null());
}
