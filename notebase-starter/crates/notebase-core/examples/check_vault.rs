//! Read-only opening diagnostic: metadata/listing only, no app/session or cleanup.
use notebase_core::{domain::NoteStore, infra::FsStore};
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("check_vault <absolute folder>");
    match FsStore::open(&path).and_then(|store| {
        store
            .list(&notebase_core::domain::RelPath::root())
            .map(|notes| notes.len())
            .map_err(notebase_core::domain::AppError::from)
    }) {
        Ok(count) => println!("目录读取成功，Markdown文件数：{count}"),
        Err(error) => {
            eprintln!("{}：{}", error.code, error.message);
            std::process::exit(1);
        }
    }
}
