pub mod application;
pub mod domain;
pub mod index;
pub mod infra;
pub mod links;
pub mod logger;
pub mod migration;
pub mod note_files;
pub mod note_service;
pub mod note_state;
pub mod organization;
pub mod settings;
pub use application::App;
pub use domain::{Request, Response};
/// Disk notes with ephemeral private state, intended for tests and embedded callers.
pub fn new_local_app() -> App {
    App::new(Box::new(|path| Ok(Box::new(infra::FsStore::open(path)?)))).with_support(Box::new(
        |vault, _| {
            Ok((
                Box::new(note_files::FsNoteFiles::open(vault)?),
                Box::new(note_state::MemoryNoteStateStore::default()),
            ))
        },
    ))
}
/// Persistent drafts and rename recovery, without requiring an index session.
pub fn new_stateful_app(data_root: std::path::PathBuf) -> App {
    App::new(Box::new(|path| Ok(Box::new(infra::FsStore::open(path)?)))).with_support(Box::new(
        move |vault, id| {
            let root = note_files::private_state_root(&data_root, vault, id)?;
            Ok((
                Box::new(note_files::FsNoteFiles::open(vault)?),
                match note_state::FsNoteStateStore::open(&root) {
                    Ok(state) => Box::new(state) as Box<dyn note_state::NoteStateStore>,
                    Err(error) => Box::new(UnavailableState(error.message)),
                },
            ))
        },
    ))
}
pub fn new_indexed_app(data_root: std::path::PathBuf, events: index::worker::EventSink) -> App {
    let settings = data_root.join("settings.json");
    new_stateful_app(data_root.clone())
        .with_settings(Box::new(settings::FileSettings(settings.clone())))
        .with_location(Box::new(migration::LocalVaultLocation {
            settings_path: settings,
        }))
        .with_events(events.clone())
        .with_index(Box::new(move |vault, id| {
            std::sync::Arc::new(index::worker::BackgroundIndex::start(
                data_root.clone(),
                vault.into(),
                id.into(),
                events.clone(),
            ))
        }))
}
// Failure to open private state must not make authoritative notes uneditable.
struct UnavailableState(String);
impl note_state::NoteStateStore for UnavailableState {
    fn read(&self, _: &str, _: &str) -> domain::Result<Option<serde_json::Value>> {
        Err(domain::AppError::new(
            "E_IO_PERMISSION",
            format!("恢复存储不可用：{}", self.0),
        ))
    }
    fn write(&self, _: &str, _: &str, _: &serde_json::Value) -> domain::Result<()> {
        Err(domain::AppError::new(
            "E_IO_PERMISSION",
            format!("恢复存储不可用：{}", self.0),
        ))
    }
    fn remove(&self, _: &str, _: &str) -> domain::Result<()> {
        Err(domain::AppError::new(
            "E_IO_PERMISSION",
            format!("恢复存储不可用：{}", self.0),
        ))
    }
    fn list(&self, _: &str) -> domain::Result<Vec<serde_json::Value>> {
        Err(domain::AppError::new(
            "E_IO_PERMISSION",
            format!("恢复存储不可用：{}", self.0),
        ))
    }
}
