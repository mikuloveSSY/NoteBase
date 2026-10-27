pub mod application;
pub mod domain;
pub mod infra;
pub use application::App;
pub use domain::{Request, Response};
// 依赖装配入口，应用层仅持有 NoteStore 端口。
pub fn new_local_app() -> App {
    App::new(Box::new(|path| Ok(Box::new(infra::FsStore::open(path)?))))
}
