use notebase_core::domain::Result;
use notebase_core::note_state::{FsNoteStateStore, MemoryNoteStateStore, NoteStateStore};
use serde_json::{json, Value};
use std::fs;

fn code<T: std::fmt::Debug>(result: Result<T>) -> String {
    result.unwrap_err().code
}

fn ids(values: &[Value]) -> Vec<&str> {
    values
        .iter()
        .map(|value| value["id"].as_str().unwrap())
        .collect()
}

#[test]
fn memory_store_roundtrips_removes_and_lists_in_key_order() {
    let store = MemoryNoteStateStore::default();
    store
        .write("drafts", "z", &json!({"id":"z","body":"last"}))
        .unwrap();
    store
        .write("drafts", "a", &json!({"id":"a","body":"first"}))
        .unwrap();
    store.write("renames", "m", &json!({"id":"m"})).unwrap();

    assert_eq!(
        store.read("drafts", "a").unwrap(),
        Some(json!({"id":"a","body":"first"}))
    );
    assert_eq!(ids(&store.list("drafts").unwrap()), vec!["a", "z"]);
    store.remove("drafts", "a").unwrap();
    assert_eq!(store.read("drafts", "a").unwrap(), None);
    assert_eq!(ids(&store.list("renames").unwrap()), vec!["m"]);
}

#[test]
fn fs_store_persists_state_and_leaves_index_db_untouched() {
    let root = tempfile::tempdir().unwrap();
    let index = root.path().join("index.db");
    fs::write(&index, b"authoritative sqlite bytes").unwrap();

    {
        let store = FsNoteStateStore::open(root.path()).unwrap();
        store
            .write("drafts", "z", &json!({"id":"z","body":"last"}))
            .unwrap();
        store
            .write("drafts", "a", &json!({"id":"a","body":"first"}))
            .unwrap();
        assert_eq!(ids(&store.list("drafts").unwrap()), vec!["a", "z"]);
    }

    let reopened = FsNoteStateStore::open(root.path()).unwrap();
    assert_eq!(
        reopened.read("drafts", "a").unwrap(),
        Some(json!({"id":"a","body":"first"}))
    );
    assert_eq!(ids(&reopened.list("drafts").unwrap()), vec!["a", "z"]);
    assert_eq!(fs::read(&index).unwrap(), b"authoritative sqlite bytes");
    assert!(root.path().join("drafts/a.json").is_file());
    assert!(root.path().join("renames").is_dir());
}

#[test]
fn fs_store_accepts_ten_mib_body_after_json_escaping() {
    let root = tempfile::tempdir().unwrap();
    let store = FsNoteStateStore::open(root.path()).unwrap();
    // A control byte expands to six JSON bytes (the worst string escaping).
    let body = "\0".repeat(10 * 1024 * 1024);
    let value = json!({"id":"large","body":body});
    assert!(serde_json::to_vec(&value).unwrap().len() < 64 * 1024 * 1024);
    store.write("drafts", "large", &value).unwrap();
    assert_eq!(store.read("drafts", "large").unwrap(), Some(value));
}

#[test]
fn invalid_buckets_and_keys_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let store = FsNoteStateStore::open(root.path()).unwrap();
    let value = json!({"id":"safe"});

    for bucket in ["other", "../drafts", "drafts/"] {
        assert_eq!(code(store.write(bucket, "safe", &value)), "E_PATH_INVALID");
    }
    for key in ["", ".", "..", "../escape", "a/b", r"a\\b", "中", "a\0b"] {
        assert_eq!(code(store.write("drafts", key, &value)), "E_PATH_INVALID");
    }
}

#[test]
fn corrupt_json_is_an_error_and_remains_on_disk() {
    let root = tempfile::tempdir().unwrap();
    let store = FsNoteStateStore::open(root.path()).unwrap();
    let path = root.path().join("drafts/broken.json");
    fs::write(&path, b"{not json").unwrap();

    assert_eq!(code(store.read("drafts", "broken")), "E_FORMAT_UNSUPPORTED");
    assert_eq!(code(store.list("drafts")), "E_FORMAT_UNSUPPORTED");
    assert_eq!(
        code(store.write("drafts", "broken", &json!({"id":"replacement"}))),
        "E_FORMAT_UNSUPPORTED"
    );
    assert_eq!(
        code(store.remove("drafts", "broken")),
        "E_FORMAT_UNSUPPORTED"
    );
    assert_eq!(fs::read(&path).unwrap(), b"{not json");
}

#[test]
fn oversized_json_is_an_error_and_remains_on_disk() {
    let root = tempfile::tempdir().unwrap();
    let store = FsNoteStateStore::open(root.path()).unwrap();
    let path = root.path().join("drafts/huge.json");
    let oversized = vec![b' '; 64 * 1024 * 1024 + 1];
    fs::write(&path, &oversized).unwrap();

    assert_eq!(code(store.read("drafts", "huge")), "E_FILE_TOO_LARGE");
    assert_eq!(code(store.list("drafts")), "E_FILE_TOO_LARGE");
    assert_eq!(
        code(store.write("drafts", "huge", &json!({"id":"replacement"}))),
        "E_FILE_TOO_LARGE"
    );
    assert_eq!(code(store.remove("drafts", "huge")), "E_FILE_TOO_LARGE");
    assert_eq!(fs::metadata(&path).unwrap().len(), oversized.len() as u64);
}

#[test]
fn state_keys_reject_windows_devices_and_overlong_components() {
    let memory = MemoryNoteStateStore::default();
    let root = tempfile::tempdir().unwrap();
    let store = FsNoteStateStore::open(root.path()).unwrap();
    for key in ["NUL", "con", "PRN.txt", "AUX", "COM1", "lpt9", "bad:key"] {
        assert_eq!(
            code(memory.write("drafts", key, &Value::Null)),
            "E_PATH_INVALID"
        );
        assert_eq!(
            code(store.write("drafts", key, &Value::Null)),
            "E_PATH_INVALID"
        );
    }
    let too_long = "a".repeat(251);
    assert_eq!(
        code(memory.write("drafts", &too_long, &Value::Null)),
        "E_PATH_INVALID"
    );
    assert_eq!(
        code(store.write("drafts", &too_long, &Value::Null)),
        "E_PATH_INVALID"
    );
}

#[test]
fn open_creates_and_checks_all_missing_parent_directories() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("missing/parent/state");
    let store = FsNoteStateStore::open(&root).unwrap();
    store.write("drafts", "one", &json!({"id":"one"})).unwrap();
    assert!(root.join("drafts/one.json").is_file());

    let file_parent = parent.path().join("regular-file");
    fs::write(&file_parent, b"keep").unwrap();
    assert_eq!(
        code(FsNoteStateStore::open(&file_parent.join("state"))),
        "E_PATH_INVALID"
    );
    assert_eq!(fs::read(&file_parent).unwrap(), b"keep");
}

#[test]
fn remove_is_idempotent_and_rejects_non_files() {
    let root = tempfile::tempdir().unwrap();
    let store = FsNoteStateStore::open(root.path()).unwrap();
    store.remove("drafts", "missing").unwrap();
    fs::create_dir(root.path().join("drafts/directory.json")).unwrap();
    assert_eq!(code(store.remove("drafts", "directory")), "E_PATH_INVALID");
}

#[cfg(unix)]
#[test]
fn symlinks_in_state_paths_are_rejected_without_touching_target() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.json"), br#"{"id":"secret"}"#).unwrap();
    let store = FsNoteStateStore::open(root.path()).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.json"),
        root.path().join("drafts/link.json"),
    )
    .unwrap();

    assert_eq!(code(store.read("drafts", "link")), "E_PATH_INVALID");
    assert_eq!(code(store.list("drafts")), "E_PATH_INVALID");
    assert_eq!(
        fs::read(outside.path().join("secret.json")).unwrap(),
        br#"{"id":"secret"}"#
    );
}

#[cfg(unix)]
#[test]
fn symlink_root_is_rejected() {
    let real = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let linked = parent.path().join("state");
    std::os::unix::fs::symlink(real.path(), &linked).unwrap();
    assert_eq!(code(FsNoteStateStore::open(&linked)), "E_PATH_INVALID");
}

#[cfg(windows)]
#[test]
fn windows_junctions_in_state_paths_are_rejected() {
    use std::os::windows::process::CommandExt;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.json"), br#"{"id":"secret"}"#).unwrap();
    let store = FsNoteStateStore::open(root.path()).unwrap();
    let drafts = root.path().join("drafts");
    fs::remove_dir(&drafts).unwrap();
    let output = std::process::Command::new("cmd.exe")
        .raw_arg(format!(
            "/d /c mklink /J \"{}\" \"{}\"",
            drafts.display(),
            outside.path().display()
        ))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(code(store.read("drafts", "secret")), "E_PATH_INVALID");
    fs::remove_dir(&drafts).unwrap();
}
