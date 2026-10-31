//! A self-contained temporary-library demo, usable without a Tauri desktop environment.
use notebase_core::data_services::{DataService, LocalDataService};
use serde_json::json;
fn main() {
    let output = std::env::args()
        .nth(1)
        .expect("usage: cargo run --example member3_demo -- ABSOLUTE_OUTPUT_DIRECTORY");
    let temporary = tempfile::tempdir().unwrap();
    let vault = temporary.path().join("vault");
    std::fs::create_dir(&vault).unwrap();
    std::fs::write(
        vault.join("演示.md"),
        "# 成员三演示\n\n- [x] Markdown\n\n[[尚未创建]]\n",
    )
    .unwrap();
    let service = LocalDataService::open(
        vault.to_str().unwrap(),
        &temporary.path().join("app-data"),
        "demo",
    )
    .unwrap();
    let result = service
        .backup_create(&json!({"targetDir":output,"description":"可独立校验的成员三演示"}))
        .unwrap();
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}
