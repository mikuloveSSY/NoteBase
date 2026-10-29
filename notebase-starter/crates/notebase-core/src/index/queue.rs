//! 有界、按路径合并的索引队列。满载和不明确的目录事件请求全库对账。
use crate::domain::RelPath;
use std::{
    collections::BTreeMap,
    sync::Mutex,
    time::{Duration, Instant},
};
pub const QUEUE_CAPACITY: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Local,
    External,
}
#[derive(Default)]
struct Pending {
    paths: BTreeMap<String, Source>,
    reconcile: bool,
    first: Option<Instant>,
    last: Option<Instant>,
    warning: Option<String>,
}
pub struct IndexQueue {
    inner: Mutex<Pending>,
    capacity: usize,
}
pub struct Work {
    pub paths: BTreeMap<String, Source>,
    pub reconcile: bool,
    pub warning: Option<String>,
}
impl Default for IndexQueue {
    fn default() -> Self {
        Self::new(QUEUE_CAPACITY)
    }
}
impl IndexQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Pending::default()),
            capacity: capacity.max(1),
        }
    }
    pub fn push(&self, path: &RelPath, source: Source) {
        let mut pending = self.inner.lock().unwrap();
        if !pending.paths.contains_key(path.as_str()) && pending.paths.len() >= self.capacity {
            pending.paths.clear();
            pending.reconcile = true;
            pending.warning = Some("索引队列溢出，已安排全库对账".into());
        }
        if !pending.reconcile || (source == Source::Local && pending.paths.len() < self.capacity) {
            pending
                .paths
                .entry(path.as_str().into())
                .and_modify(|old| {
                    if source == Source::Local {
                        *old = source;
                    }
                })
                .or_insert(source);
        }
        touch(&mut pending);
    }
    pub fn reconcile(&self, warning: Option<String>) {
        let mut pending = self.inner.lock().unwrap();
        pending.reconcile = true;
        if warning.is_some() {
            pending.warning = warning;
        }
        touch(&mut pending);
    }
    pub fn pending(&self) -> bool {
        let p = self.inner.lock().unwrap();
        p.reconcile || !p.paths.is_empty()
    }
    pub fn take(&self, debounce: Duration) -> Option<Work> {
        let mut p = self.inner.lock().unwrap();
        let now = Instant::now();
        if !p
            .first
            .is_some_and(|first| now.duration_since(first) >= Duration::from_millis(500))
            && !p
                .last
                .is_some_and(|last| now.duration_since(last) >= debounce)
        {
            return None;
        }
        let old = std::mem::take(&mut *p);
        Some(Work {
            paths: old.paths,
            reconcile: old.reconcile,
            warning: old.warning,
        })
    }
}
fn touch(p: &mut Pending) {
    let now = Instant::now();
    p.first.get_or_insert(now);
    p.last = Some(now);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicates_preserve_local_source_and_overflow_requests_reconcile() {
        let q = IndexQueue::new(2);
        let a = RelPath::new("A.md").unwrap();
        q.push(&a, Source::Local);
        q.push(&a, Source::External);
        let w = q.take(Duration::ZERO).unwrap();
        assert_eq!(w.paths.len(), 1);
        assert_eq!(w.paths["A.md"], Source::Local);
        for p in ["A.md", "B.md", "C.md"] {
            q.push(&RelPath::new(p).unwrap(), Source::External);
        }
        let w = q.take(Duration::ZERO).unwrap();
        assert!(w.reconcile);
        assert!(w.paths.is_empty());
        assert!(w.warning.is_some());
        assert!(!q.pending());
    }
}
