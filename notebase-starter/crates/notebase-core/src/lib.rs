pub mod application;
pub mod domain;
pub mod index;
pub mod infra;
pub mod migration;
pub use application::App;
pub use domain::{Request, Response};
// 依赖装配入口，应用层仅持有 NoteStore 端口。
pub fn new_local_app() -> App {
    App::new(Box::new(|path| Ok(Box::new(infra::FsStore::open(path)?))))
}
pub fn new_indexed_app(data_root: std::path::PathBuf, events: index::worker::EventSink) -> App {
    let settings = data_root.join("settings.json");
    new_local_app()
        .with_index(Box::new(move |vault, id| {
            std::sync::Arc::new(index::worker::BackgroundIndex::start(
                data_root.clone(),
                vault.into(),
                id.into(),
                events.clone(),
            ))
        }))
        .with_location(Box::new(migration::LocalVaultLocation {
            settings_path: settings,
        }))
}
