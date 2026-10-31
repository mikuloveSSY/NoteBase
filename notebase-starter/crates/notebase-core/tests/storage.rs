use notebase_core::domain::{
    sanitize_title, AppError, NoteStore, RelPath, StoreError, NOTE_SIZE_LIMIT, PREVIEW_SIZE_LIMIT,
};
use notebase_core::infra::FsStore;
use std::fs;

fn path(value: &str) -> RelPath {
    RelPath::new(value).unwrap()
}

fn setup() -> (tempfile::TempDir, FsStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = FsStore::open(dir.path().to_str().unwrap()).unwrap();
    (dir, store)
}

#[test]
fn mixed_non_markdown_files_are_ignored_without_modification() {
    let (dir, store) = setup();
    fs::create_dir(dir.path().join("课程")).unwrap();
    fs::write(dir.path().join("A.md"), "笔记").unwrap();
    fs::write(dir.path().join("课程/B.MD"), "第二篇").unwrap();
    for name in [
        "image.png",
        "document.pdf",
        "archive.zip",
        "program.exe",
        "raw.bin",
    ] {
        fs::write(dir.path().join(name), [0xff, 0x00, 0xfe]).unwrap();
    }
    assert_eq!(
        store
            .list(&RelPath::root())
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect::<Vec<_>>(),
        ["A.md", "课程/B.MD"]
    );
    for name in [
        "image.png",
        "document.pdf",
        "archive.zip",
        "program.exe",
        "raw.bin",
    ] {
        assert_eq!(fs::read(dir.path().join(name)).unwrap(), [0xff, 0x00, 0xfe]);
    }
}

#[test]
fn directory_scan_cancellation_returns_no_partial_list_and_can_be_retried() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let (dir, mut store) = setup();
    fs::write(dir.path().join("A.md"), "unchanged").unwrap();
    fs::write(dir.path().join("ignored.bin"), [0xff]).unwrap();
    let flag = Arc::new(AtomicBool::new(true));
    store.set_scan_cancel(flag.clone());
    assert_eq!(
        AppError::from(store.list(&RelPath::root()).unwrap_err()).code,
        "E_CANCELLED"
    );
    flag.store(false, Ordering::Release);
    assert_eq!(store.list(&RelPath::root()).unwrap().len(), 1);
    assert_eq!(fs::read(dir.path().join("A.md")).unwrap(), b"unchanged");
}

#[test]
fn path_rules_cover_unicode_devices_separators_and_lengths() {
    for input in [
        "COM¹.md",
        "LPT².txt.md",
        "ＣＯＮ.md",
        "NUL .md",
        "a／b.md",
        "．．/x.md",
        "C:/x.md",
        "a//b.md",
        "a\0.md",
    ] {
        assert!(RelPath::new(input).is_err(), "{input:?}");
    }
    assert!(RelPath::new(&format!("{}.md", "x".repeat(256))).is_err());
    let original = "课程/ＡＢＣ.md";
    assert_eq!(path(original).as_str(), original);
}

#[test]
fn new_titles_are_sanitized_without_accepting_empty_input() {
    for (input, expected) in [
        ("CON", "_CON"),
        ("NUL.txt", "_NUL.txt"),
        ("COM¹", "_COM1"),
        ("a<b>c", "a_b_c"),
        ("name.", "name"),
        ("..", "未命名"),
        ("ＡＢＣ", "ABC"),
        ("a/b", "a_b"),
    ] {
        assert_eq!(sanitize_title(input).unwrap(), expected);
    }
    assert!(sanitize_title("  ").is_err());
}

#[test]
fn create_stat_rename_and_delete_use_the_same_port() {
    let (dir, store) = setup();
    let a = path("原名.md");
    let b = path("新名.md");
    store.create_new(&a).unwrap();
    let written = store.write_atomic(&a, "正文\n".as_bytes()).unwrap();
    assert_eq!(written.bytes_written, 7);
    assert_eq!(store.stat(&a).unwrap().size_bytes, 7);
    store.rename(&a, &b).unwrap();
    assert!(!dir.path().join("原名.md").exists());
    assert_eq!(store.read(&b).unwrap().raw, "正文\n".as_bytes());
    store.rename(&b, &b).unwrap();
    store.delete(&b).unwrap();
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn duplicate_creation_and_rename_never_overwrite_existing_content() {
    let (dir, store) = setup();
    fs::write(dir.path().join("A.md"), "A").unwrap();
    fs::write(dir.path().join("B.md"), "B").unwrap();
    assert!(matches!(
        store.create_new(&path("A.md")),
        Err(StoreError::AlreadyExists)
    ));
    assert!(matches!(
        store.rename(&path("A.md"), &path("B.md")),
        Err(StoreError::AlreadyExists)
    ));
    assert_eq!(fs::read_to_string(dir.path().join("A.md")).unwrap(), "A");
    assert_eq!(fs::read_to_string(dir.path().join("B.md")).unwrap(), "B");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn failed_rename_and_missing_parent_leave_source_untouched() {
    let (dir, store) = setup();
    fs::write(dir.path().join("A.md"), "原文").unwrap();
    assert!(store.rename(&path("A.md"), &path("missing/B.md")).is_err());
    assert!(store.create_new(&path("missing/C.md")).is_err());
    assert_eq!(fs::read_to_string(dir.path().join("A.md")).unwrap(), "原文");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn full_path_limit_is_checked_before_creating_a_file() {
    let (dir, store) = setup();
    // Target the actual full-path boundary instead of assuming a long Windows TEMP path.
    let root = fs::canonicalize(dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let logical = root
        .strip_prefix(r"\\?\UNC\")
        .map(|p| format!(r"\\{p}"))
        .unwrap_or_else(|| root.strip_prefix(r"\\?\").unwrap_or(&root).to_owned());
    let count = 260usize.saturating_sub(logical.encode_utf16().count() + 1 + 3);
    let name = format!("{}.md", "x".repeat(count));
    let error = AppError::from(store.create_new(&path(&name)).unwrap_err());
    assert_eq!(error.code, "E_PATH_INVALID");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn listing_is_scoped_and_only_excludes_special_directories_at_vault_root() {
    let (dir, store) = setup();
    for folder in [
        "trash",
        "attachments",
        ".git",
        "课程/trash",
        "课程/attachments",
        "其他",
    ] {
        fs::create_dir_all(dir.path().join(folder)).unwrap();
        fs::write(dir.path().join(folder).join("A.MD"), "text").unwrap();
    }
    let root = store.list(&RelPath::root()).unwrap();
    assert_eq!(root.len(), 3);
    assert_eq!(store.list(&path("课程")).unwrap().len(), 2);
    assert!(store.list(&path("trash")).unwrap().is_empty());
    assert_eq!(store.read(&path("其他/A.MD")).unwrap().raw, b"text");
}

#[test]
fn large_file_read_is_bounded_and_write_is_rejected() {
    let (dir, store) = setup();
    let file = fs::File::create(dir.path().join("large.md")).unwrap();
    file.set_len((NOTE_SIZE_LIMIT + 4096) as u64).unwrap();
    drop(file);
    let note = store.read(&path("large.md")).unwrap();
    assert!(note.truncated);
    assert_eq!(note.size_bytes, (NOTE_SIZE_LIMIT + 4096) as u64);
    assert_eq!(note.raw.len(), PREVIEW_SIZE_LIMIT);
    let error = AppError::from(store.write_atomic(&path("large.md"), b"small").unwrap_err());
    assert_eq!(error.code, "E_FILE_TOO_LARGE");
    assert_eq!(
        fs::metadata(dir.path().join("large.md")).unwrap().len(),
        note.size_bytes
    );
}

#[test]
fn oversized_replacement_does_not_touch_a_small_file() {
    let (dir, store) = setup();
    fs::write(dir.path().join("A.md"), "small").unwrap();
    let error = AppError::from(
        store
            .write_atomic(&path("A.md"), &vec![b'a'; NOTE_SIZE_LIMIT + 1])
            .unwrap_err(),
    );
    assert_eq!(error.code, "E_FILE_TOO_LARGE");
    assert_eq!(fs::read(dir.path().join("A.md")).unwrap(), b"small");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn readonly_file_is_reported_and_replacement_is_refused() {
    let (dir, store) = setup();
    let target = dir.path().join("A.md");
    fs::write(&target, "原文").unwrap();
    let original = fs::metadata(&target).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&target, readonly).unwrap();
    let read = store.read(&path("A.md")).unwrap();
    let result = store.write_atomic(&path("A.md"), b"replace");
    fs::set_permissions(&target, original).unwrap();
    assert!(read.read_only);
    assert_eq!(AppError::from(result.unwrap_err()).code, "E_IO_PERMISSION");
    assert_eq!(fs::read_to_string(target).unwrap(), "原文");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn directories_cannot_be_read_deleted_or_replaced_as_notes() {
    let (dir, store) = setup();
    fs::create_dir(dir.path().join("A.md")).unwrap();
    fs::write(dir.path().join("A.md/child.md"), "保留").unwrap();
    assert!(store.read(&path("A.md")).is_err());
    assert!(store.delete(&path("A.md")).is_err());
    assert!(store.write_atomic(&path("A.md"), b"overwrite").is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("A.md/child.md")).unwrap(),
        "保留"
    );
}

#[cfg(windows)]
#[test]
fn windows_sharing_violation_reports_retryable_error_and_cleans_temp() {
    use std::os::windows::fs::OpenOptionsExt;
    let (dir, store) = setup();
    let target = dir.path().join("A.md");
    fs::write(&target, "原文").unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&target)
        .unwrap();
    let error = AppError::from(store.write_atomic(&path("A.md"), b"replace").unwrap_err());
    drop(lock);
    assert_eq!(error.code, "E_IO_LOCKED");
    assert!(error.retryable);
    assert_eq!(fs::read_to_string(&target).unwrap(), "原文");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[cfg(windows)]
#[test]
fn windows_junction_cannot_escape_vault() {
    let (dir, store) = setup();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.md"), "保留").unwrap();
    let link = dir.path().join("link");
    let result = std::process::Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let read = store.read(&path("link/secret.md"));
    let listing = store.list(&RelPath::root());
    fs::remove_dir(&link).unwrap();
    let error = match read {
        Err(error) => AppError::from(error),
        Ok(_) => panic!("junction followed"),
    };
    assert_eq!(error.code, "E_PATH_INVALID");
    assert!(listing.unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(outside.path().join("secret.md")).unwrap(),
        "保留"
    );
}
