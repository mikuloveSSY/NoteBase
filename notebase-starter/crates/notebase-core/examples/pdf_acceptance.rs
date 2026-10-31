//! Generates a synthetic PDF for visual acceptance, without touching real notes.
use notebase_core::data_services::{DataService, LocalDataService};
use serde_json::json;
fn main() {
    let output = std::env::args()
        .nth(1)
        .expect("absolute PDF output path required");
    let temp = tempfile::tempdir().unwrap();
    let vault = temp.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    let mut body = String::from("# NoteBase PDF 验收\n\n中文、English、数字 123。**粗体**与*斜体*。\n\n| 项目 | 结果 |\n| --- | --- |\n| 中文表格 | 正常 |\n\n```rust\nfn main() { println!(\"Hello 中文\"); }\n```\n\n> 引用内容与换行。\n\n");
    for i in 1..=45 {
        body.push_str(&format!(
            "段落 {i}：用于检查分页、中文显示和文本可搜索性。\n\n"
        ));
    }
    std::fs::write(vault.join("验收.md"), body).unwrap();
    let service = LocalDataService::open(
        vault.to_str().unwrap(),
        &temp.path().join("data"),
        "pdf-acceptance",
    )
    .unwrap();
    let result = service
        .export(&json!({"path":"验收.md","format":"pdf","targetPath":output}))
        .unwrap();
    println!("{result}");
}
