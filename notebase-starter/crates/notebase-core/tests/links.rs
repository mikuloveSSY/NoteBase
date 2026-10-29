use links::{backlinks, graph, mentions, parse, rename_body, resolve, LinkNote};
// Exercise the scoped module before the application composition root registers it.
pub use notebase_core::index;
#[path = "../src/links.rs"]
mod links;

fn note(path: &str, title: &str, aliases: &[&str], body: &str) -> LinkNote {
    LinkNote {
        path: path.into(),
        title: title.into(),
        aliases: aliases.iter().map(|s| (*s).into()).collect(),
        body: body.into(),
    }
}

#[test]
fn parses_labels_anchors_and_unicode_positions_without_changing_byte_offsets() {
    let raw = "前言\r\n中文🙂 [[目录/A#小节|显示文本]]，[[#本节]] [[B]]";
    let links = parse(raw);
    assert_eq!(links.len(), 3);
    assert_eq!(links[0].target, "目录/A#小节");
    assert_eq!(links[0].label.as_deref(), Some("显示文本"));
    assert_eq!((links[0].line, links[0].col), (2, 5));
    assert_eq!(
        &raw[links[0].start..links[0].end],
        "[[目录/A#小节|显示文本]]"
    );
    assert_eq!(links[1].target, "#本节");
    assert_eq!(links[2].label, None);
    assert_eq!(
        serde_json::to_value(&links[0]).unwrap()["start"],
        links[0].start
    );
}

#[test]
fn ignores_frontmatter_html_comments_code_and_embeds() {
    let raw = "\u{feff}---\r\naliases: [\"[[metadata]]\"]\r\n---\r\n[[real]] `[[inline]]` ![[embed]] \\[[escaped]]\n\n```md\n[[fence]]\n```\n\n    [[indented]]\n\n> ```\n> [[quote-code]]\n> ```\n\n<!-- [[comment]] -->\n\n<div>\n[[html-block]]\n</div>\n\n<span data-link=\"[[attribute]]\">[[visible]]</span>\n";
    let targets: Vec<_> = parse(raw).into_iter().map(|l| l.target).collect();
    assert_eq!(targets, ["real", "visible"]);
}

#[test]
fn escaped_delimiters_even_backslashes_and_unmatched_markup_are_handled() {
    let raw = r"\[[skip]] \\[[keep]] ![[skip-embed]] \![[literal-bang]] [[A\|B|shown\|label]] [[unfinished [[valid]]";
    let targets: Vec<_> = parse(raw).into_iter().map(|l| l.target).collect();
    assert_eq!(targets, ["keep", "literal-bang", "A|B", "valid"]);
    assert!(parse("[[]] [[   ]] [[A\nB]]").is_empty());
}

#[test]
fn parses_links_inside_markdown_containers_and_gfm_tables() {
    let raw = "- **[[list]]**\n> [[quote]]\n\n| item |\n| --- |\n| [[table\\|label]] |\n\n`` [[not-code]] `\n[[after-unmatched-code]]";
    let targets: Vec<_> = parse(raw).into_iter().map(|l| l.target).collect();
    assert!(targets.contains(&"list".to_string()));
    assert!(targets.contains(&"quote".to_string()));
    assert!(targets.contains(&"after-unmatched-code".to_string()));
}

#[test]
fn bom_and_carriage_return_lines_do_not_change_markdown_indentation() {
    let raw = "\u{feff} [[real]] `[[hidden]]`\r\r    [[indented]]\r\r[[last]]";
    let parsed = parse(raw);
    let targets: Vec<_> = parsed.iter().map(|l| l.target.as_str()).collect();
    assert_eq!(targets, ["real", "last"]);
    assert_eq!((parsed[0].line, parsed[0].col), (1, 3));
    assert_eq!((parsed[1].line, parsed[1].col), (5, 1));
    assert_eq!(&raw[parsed[0].start..parsed[0].end], "[[real]]");
}

#[test]
fn rename_preserves_raw_escaped_labels_whitespace_and_anchor_only_links() {
    let source = note(
        "A.md",
        "A",
        &[],
        r"[[ A#h | shown\|label ]] [[#local]] [[A\|name#x|display]]",
    );
    let notes = vec![source.clone(), note("Other.md", "A|name", &[], "")];
    assert_eq!(
        rename_body(&source, "A.md", "C.md", &notes),
        r"[[ C#h | shown\|label ]] [[C#local]] [[A\|name#x|display]]"
    );
    assert_eq!(
        rename_body(&source, "Other.md", "D.md", &notes),
        r"[[ A#h | shown\|label ]] [[#local]] [[D#x|display]]"
    );
}

#[test]
fn explicit_paths_take_precedence_over_titles_and_relative_paths_resolve() {
    let notes = vec![
        note("A.md", "Root", &[], ""),
        note("folder/A.md", "A", &[], ""),
        note("folder/B.md", "Other", &["A"], ""),
    ];
    let root = resolve("folder/source.md", "A#section", &notes);
    assert_eq!(root.target_path.as_deref(), Some("A.md"));
    assert!(!root.ambiguous);
    assert_eq!(root.candidates, ["A.md"]);
    assert_eq!(
        resolve("folder/source.md", "./A.md", &notes)
            .target_path
            .as_deref(),
        Some("folder/A.md")
    );
    assert_eq!(
        resolve("folder/source.md", "../A", &notes)
            .target_path
            .as_deref(),
        Some("A.md")
    );
    assert_eq!(
        resolve("folder/source.md", "#heading", &notes)
            .target_path
            .as_deref(),
        Some("folder/source.md")
    );
}

#[test]
fn ambiguity_is_reported_even_when_the_nearest_candidate_is_preferred() {
    let notes = vec![
        note("z/A.md", "Same", &["Alias"], ""),
        note("work/A.md", "Same", &["Alias"], ""),
        note("a/A.md", "Same", &["Alias"], ""),
    ];
    let result = resolve("work/source.md", "Alias", &notes);
    assert_eq!(result.target_path.as_deref(), Some("work/A.md"));
    assert!(result.ambiguous);
    assert_eq!(result.candidates, ["work/A.md", "a/A.md", "z/A.md"]);
    let tied = resolve("else/source.md", "Same", &notes);
    assert_eq!(tied.target_path.as_deref(), Some("a/A.md"));
    assert!(tied.ambiguous);
    let mut reversed = notes.clone();
    reversed.reverse();
    assert_eq!(
        resolve("else/source.md", "Same", &reversed).candidates,
        tied.candidates
    );
}

#[test]
fn normalizes_separators_unicode_and_case_without_permitting_vault_escape() {
    let notes = vec![note("Folder/Café.md", "Résumé", &["Guide"], "")];
    assert_eq!(
        resolve("S.md", "folder\\Cafe\u{301}.MD#x", &notes)
            .target_path
            .as_deref(),
        Some("Folder/Café.md")
    );
    assert_eq!(
        resolve("S.md", "guide", &notes).target_path.as_deref(),
        Some("Folder/Café.md")
    );
    for target in ["../../Folder/Café", "https://example.com", "", "missing"] {
        let result = resolve("S.md", target, &notes);
        assert!(result.target_path.is_none(), "{target}");
        assert!(result.candidates.is_empty());
        assert!(!result.ambiguous);
    }
}

#[test]
fn unicode_targets_without_extensions_and_combining_mentions_keep_valid_spans() {
    let notes = vec![
        note("éé.md", "éé", &["Café"], ""),
        note("S.md", "S", &[], "Cafe\u{301} and Cafe\u{301}s"),
    ];
    assert_eq!(
        resolve("S.md", "éé", &notes).target_path.as_deref(),
        Some("éé.md")
    );
    let found = mentions("éé.md", &notes);
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    assert_eq!(found["items"][0]["text"], "Cafe\u{301}");
}

#[test]
fn rename_rewrites_resolved_targets_only_and_preserves_every_other_byte() {
    let source = note("work/source.md", "Source", &[], "\u{feff}---\r\nx: '[[Alias]]'\r\n---\r\n🙂 [[Alias#标题|原 显示]] [[../Old.md]] `[[Alias]]` ![[Alias]] \\[[Alias]] [[missing]]\r\n");
    let notes = vec![note("Old.md", "Old title", &["Alias"], ""), source.clone()];
    assert_eq!(rename_body(&source, "Old.md", "nested/New.md", &notes), "\u{feff}---\r\nx: '[[Alias]]'\r\n---\r\n🙂 [[nested/New#标题|原 显示]] [[nested/New]] `[[Alias]]` ![[Alias]] \\[[Alias]] [[missing]]\r\n");
    assert_eq!(
        rename_body(
            &note("S.md", "S", &[], "[[Old|显示]]"),
            "Old.md",
            "C.md",
            &notes
        ),
        "[[C|显示]]"
    );
}

#[test]
fn rename_uses_the_selected_ambiguous_candidate_instead_of_text_matching() {
    let source = note("local/Source.md", "Source", &[], "[[Same]] [[away/A]]");
    let notes = vec![
        note("local/A.md", "Same", &[], ""),
        note("away/A.md", "Same", &[], ""),
        source.clone(),
    ];
    assert_eq!(
        rename_body(&source, "away/A.md", "away/B.md", &notes),
        "[[Same]] [[away/B]]"
    );
    assert_eq!(
        rename_body(&source, "local/A.md", "local/B.md", &notes),
        "[[local/B]] [[away/A]]"
    );
}

#[test]
fn renamed_paths_containing_hashes_round_trip_without_losing_the_anchor() {
    let source = note("S.md", "S", &[], "[[A#section|label]]");
    let before = vec![note("A.md", "A", &[], ""), source.clone()];
    let body = rename_body(&source, "A.md", "folder/New#name.md", &before);
    assert_eq!(body, r"[[folder/New\#name#section|label]]");
    let after = vec![note("folder/New#name.md", "New", &[], "")];
    let link = parse(&body).remove(0);
    assert_eq!(
        resolve("S.md", &link.target, &after).target_path.as_deref(),
        Some("folder/New#name.md")
    );
}

#[test]
fn backlinks_include_each_real_occurrence_and_detect_transitive_cycles() {
    let notes = vec![
        note("A.md", "A", &[], "[[B]]"),
        note("B.md", "B", &[], "[[C]]"),
        note("C.md", "C", &[], "前 [[A#h|label]] and [[A]] `[[A]]`"),
        note("D.md", "D", &[], "[[A]]"),
    ];
    let result = backlinks("A.md", &notes);
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["source"], "C.md");
    assert_eq!(items[0]["sourcePath"], "C.md");
    assert_eq!(items[0]["line"], 1);
    assert_eq!(items[0]["col"], 3);
    assert_eq!(items[0]["circular"], true);
    assert!(items[0]["snippet"].as_str().unwrap().contains("label"));
    assert_eq!(items[0]["context"], items[0]["snippet"]);
    assert_eq!(items[2]["circular"], false);
}

#[test]
fn mentions_return_original_byte_spans_excluding_links_code_metadata_and_self() {
    let body = "---\ntitle: Topic\n---\nTopic and Alias, [[Topic]] `Alias` ![[Topic]] [Topic](Topic) <span title=\"Topic\">Alias</span> StopTopic";
    let notes = vec![
        note("A.md", "Topic", &["Alias", "Topic"], "Topic"),
        note("B.md", "B", &[], body),
    ];
    let result = mentions("A.md", &notes);
    let items = result["items"].as_array().unwrap();
    let texts: Vec<_> = items.iter().map(|v| v["text"].as_str().unwrap()).collect();
    assert_eq!(texts, ["Topic", "Alias", "Alias"]);
    for item in items {
        assert_eq!(item["sourcePath"], "B.md");
        let start = item["start"].as_u64().unwrap() as usize;
        let end = item["end"].as_u64().unwrap() as usize;
        assert_eq!(&body[start..end], item["text"].as_str().unwrap());
    }
}

#[test]
fn mentions_match_chinese_prose_and_longest_alias_without_overlapping_duplicates() {
    let notes = vec![
        note("A.md", "笔记", &["知识笔记", "Café"], ""),
        note("B.md", "B", &[], "这是知识笔记与笔记，café。"),
    ];
    let result = mentions("A.md", &notes);
    let texts: Vec<_> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["知识笔记", "笔记", "café"]);
}

#[test]
fn graph_is_deterministic_bounded_and_keeps_the_requested_neighborhood() {
    let notes = vec![
        note("unrelated.md", "Unrelated", &[], ""),
        note("C.md", "C", &[], "[[B]]"),
        note("B.md", "B", &[], "[[A]] [[A]]"),
        note("A.md", "A", &[], "[[missing]]"),
    ];
    let result = graph(Some("A.md"), 2, &notes);
    let paths: Vec<_> = result["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["A.md", "B.md"]);
    assert_eq!(result["truncated"], true);
    let edges = result["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["from"], "B.md");
    assert_eq!(edges[0]["to"], "A.md");
    assert_eq!(edges[0]["source"], edges[0]["from"]);
    assert_eq!(edges[0]["target"], edges[0]["to"]);
    let complete = graph(Some("A.md"), 20, &notes);
    assert_eq!(complete["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(complete["truncated"], false);
    assert!(graph(Some("absent.md"), 20, &notes)["nodes"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn graph_caps_total_nodes_at_five_hundred_and_honors_zero() {
    let notes: Vec<_> = (0..510)
        .map(|i| note(&format!("N{i:03}.md"), "Node", &[], ""))
        .collect();
    let result = graph(None, 1000, &notes);
    assert_eq!(result["nodes"].as_array().unwrap().len(), 500);
    assert_eq!(result["truncated"], true);
    assert!(graph(None, 0, &notes)["nodes"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(graph(None, 10, &[])["edges"].as_array().unwrap().is_empty());
}
