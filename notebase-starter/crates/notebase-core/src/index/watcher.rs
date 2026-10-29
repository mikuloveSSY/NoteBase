use super::queue::{IndexQueue, Source};
use crate::domain::RelPath;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc,
    },
};

/// 回调只归一化事件并入队，不读取文件、不访问数据库。
pub fn start(
    root: &Path,
    queue: Arc<IndexQueue>,
    wake: SyncSender<()>,
    failed: Arc<AtomicBool>,
) -> notify::Result<RecommendedWatcher> {
    let base = root.to_owned();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        match result {
            Ok(event) => route(&base, event, &queue),
            Err(error) => {
                failed.store(true, Ordering::Release);
                queue.reconcile(Some(format!("文件监视失败，已降级为周期对账：{error}")));
            }
        }
        let _ = wake.try_send(());
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok(watcher)
}
pub fn route(root: &Path, event: Event, queue: &IndexQueue) {
    if event.need_rescan() {
        queue.reconcile(Some("文件监视事件丢失，已安排全库对账".into()));
        return;
    }
    if matches!(event.kind, EventKind::Access(_)) {
        return;
    }
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    ) {
        return;
    }
    for path in event.paths {
        let Some(relative) = relative(root, &path) else {
            continue;
        };
        if relative
            .split('/')
            .next()
            .is_some_and(|p| matches!(p, "trash" | "attachments" | ".git"))
        {
            continue;
        }
        if relative.to_ascii_lowercase().ends_with(".md") {
            if let Ok(path) = RelPath::new(&relative) {
                queue.push(&path, Source::External);
            }
        } else if matches!(
            event.kind,
            EventKind::Create(notify::event::CreateKind::Folder)
                | EventKind::Remove(notify::event::RemoveKind::Folder)
                | EventKind::Modify(notify::event::ModifyKind::Name(_))
                | EventKind::Any
        ) {
            queue.reconcile(None);
        }
    }
}
fn relative(root: &Path, path: &Path) -> Option<String> {
    // Windows 回调路径与 canonicalize 的扩展前缀可能不同。
    let logical = |p: &Path| {
        PathBuf::from(
            p.to_string_lossy()
                .strip_prefix(r"\\?\")
                .unwrap_or(&p.to_string_lossy()),
        )
    };
    logical(path)
        .strip_prefix(logical(root))
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}
