use notebase_core::{new_stateful_app, App, Request};
use serde_json::{json, Value};
use std::{
    fs,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn call(a: &mut App, c: &str, p: Value) -> Value {
    serde_json::to_value(a.dispatch(Request {
        id: "crash".into(),
        command: c.into(),
        payload: p,
        client_seq: 0,
    }))
    .unwrap()
}
#[test]
fn crash_writer_child() {
    let Some(root) = std::env::var_os("NOTEBASE_CRASH_FIXTURE") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let iteration = std::env::var("NOTEBASE_CRASH_ITERATION").unwrap();
    let mut a = new_stateful_app(root.join("data"));
    assert_eq!(
        call(&mut a, "vault_open", json!({"path":root.join("vault")}))["ok"],
        true
    );
    let n = call(
        &mut a,
        "note_read",
        json!({"path":"A.md","clientId":"child"}),
    );
    let saved = call(
        &mut a,
        "note_save",
        json!({"path":"A.md","body":format!("saved-{iteration}"),"baseHash":n["data"]["contentHash"],"clientId":"child","clientSeq":1}),
    );
    assert_eq!(saved["ok"], true, "{saved}");
    let draft = call(
        &mut a,
        "note_save",
        json!({"action":"draft","path":"A.md","body":format!("draft-{iteration}"),"baseHash":saved["data"]["contentHash"],"clientId":"child","clientSeq":2}),
    );
    assert_eq!(draft["ok"], true, "{draft}");
    fs::write(root.join("ack"), "durable").unwrap();
    loop {
        std::thread::park();
    }
}
#[test]
fn two_hundred_forced_process_exits_preserve_acknowledged_saves_and_drafts() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("vault")).unwrap();
    let target = root.path().join("vault/A.md");
    for i in 0..200 {
        fs::write(&target, format!("before-{i}")).unwrap();
        let ack = root.path().join("ack");
        if ack.exists() {
            fs::remove_file(&ack).unwrap();
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_writer_child", "--nocapture"])
            .env("NOTEBASE_CRASH_FIXTURE", root.path())
            .env("NOTEBASE_CRASH_ITERATION", i.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !ack.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("fixture exited before acknowledgement: {status}");
            }
            if start.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("fixture timed out");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), format!("saved-{i}"));
        let mut a = new_stateful_app(root.path().join("data"));
        assert_eq!(
            call(
                &mut a,
                "vault_open",
                json!({"path":root.path().join("vault")})
            )["ok"],
            true
        );
        let drafts = call(&mut a, "note_list", json!({"scope":"drafts"}));
        let entries = drafts["data"]["items"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let restored = call(
            &mut a,
            "note_restore",
            json!({"kind":"draft","draftId":entries[0]["id"]}),
        );
        assert_eq!(restored["ok"], true, "{restored}");
        assert_eq!(fs::read_to_string(&target).unwrap(), format!("draft-{i}"));
    }
}
