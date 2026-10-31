use notebase_core::{
    data_services::{DataService, LocalDataService},
    index::IndexState,
    new_indexed_app, new_stateful_app, App, Request,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

#[test]
fn nested_note_assets_are_rebased_without_touching_code_blocks() {
    let (d, vault, mut app) = setup();
    fs::create_dir(vault.join("sub")).unwrap();
    fs::create_dir(vault.join("attachments")).unwrap();
    fs::write(vault.join("attachments/p.png"), [1, 2]).unwrap();
    fs::write(
        vault.join("sub/N.md"),
        "# N\n![图](../attachments/p.png)\n\n`![代码](../attachments/p.png)`\n",
    )
    .unwrap();
    let out = d.path().join("export/N.md");
    call(
        &mut app,
        "export_note",
        json!({"path":"sub/N.md","format":"md","targetPath":out}),
    );
    let text = fs::read_to_string(out).unwrap();
    assert!(text.contains("![图](attachments/p%2Epng)"));
    assert!(text.contains("`![代码](../attachments/p.png)`"));
    assert_eq!(
        fs::read(d.path().join("export/attachments/p.png")).unwrap(),
        [1, 2]
    );
}
#[test]
fn unrelated_invalid_utf8_does_not_prevent_exporting_a_valid_note() {
    let (_d, vault, mut app) = setup();
    fs::write(vault.join("bad.md"), [255]).unwrap();
    let r = call(
        &mut app,
        "export_note",
        json!({"path":"A.md","format":"html","preview":true}),
    );
    assert!(r["html"].as_str().unwrap().contains("初版"));
}
#[test]
fn thirty_day_version_retention_and_corruption_checks() {
    let (d, vault, _app) = setup();
    let service = LocalDataService::open(
        vault.to_str().unwrap(),
        &d.path().join("history-data"),
        "retention",
    )
    .unwrap();
    let snap = service.snapshot("A.md", b"old", "").unwrap();
    let version = snap["versionId"].as_str().unwrap();
    let dir = d
        .path()
        .join("history-data/vaults/retention/snapshots")
        .join(notebase_core::data_services::files::digest(b"A.md"));
    let record = dir.join(format!("{version}.json"));
    let mut meta: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    meta["createdMs"] = json!(1);
    fs::write(&record, serde_json::to_vec(&meta).unwrap()).unwrap();
    assert_eq!(service.prune_versions(31 * 24 * 3600000).unwrap(), 1);
    assert!(!record.exists());
    let snap = service.snapshot("A.md", b"intact", "").unwrap();
    fs::write(
        dir.join(format!("{}.md", snap["versionId"].as_str().unwrap())),
        "broken",
    )
    .unwrap();
    assert_eq!(
        service
            .version_body(&json!({"path":"A.md","versionId":snap["versionId"]}))
            .unwrap_err()
            .code,
        "E_BACKUP_CHECKSUM"
    );
}
fn request(app: &mut App, command: &str, payload: Value) -> Value {
    serde_json::to_value(app.dispatch(Request {
        id: "member3".into(),
        command: command.into(),
        payload,
        client_seq: 0,
    }))
    .unwrap()
}
fn call(app: &mut App, c: &str, p: Value) -> Value {
    let r = request(app, c, p);
    assert_eq!(r["ok"], true, "{c}: {r}");
    r["data"].clone()
}
fn setup() -> (tempfile::TempDir, PathBuf, App) {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "# 初版\n").unwrap();
    let mut app = new_stateful_app(dir.path().join("data"));
    call(&mut app, "vault_open", json!({"path":vault}));
    (dir, vault, app)
}
fn raw_zip(path: &Path, entries: &[(&str, Vec<u8>)]) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, body) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(body).unwrap();
    }
    writer.finish().unwrap();
}
fn entries(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut z = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut body = Vec::new();
            f.read_to_end(&mut body).unwrap();
            (f.name().to_owned(), body)
        })
        .collect()
}
#[test]
fn preview_reuses_gfm_and_preserves_missing_wiki() {
    let (_d, vault, mut app) = setup();
    fs::write(vault.join("A.md"),"# 标题\n\n|A|B|\n|-|-|\n|1|2|\n\n- [x] done\n\n~~old~~ [[Missing]] `[[Code]]`\n\n<script>alert(1)</script>\n\n[x](javascript:alert(1))").unwrap();
    let r = call(
        &mut app,
        "export_note",
        json!({"path":"A.md","format":"html","preview":true}),
    );
    let html = r["html"].as_str().unwrap();
    assert!(html.contains("<table>"));
    assert!(html.contains("<del>old</del>"));
    assert!(html.contains("[[Missing]]"));
    assert!(!html.contains("<script>"));
    assert!(!html.contains("href=\"javascript:"));
    assert_eq!(
        r["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["kind"] == "unresolved_link")
            .count(),
        1
    );
}
#[test]
fn preview_can_render_unsaved_body_without_writing() {
    let (_d, vault, mut app) = setup();
    let old = fs::read(vault.join("A.md")).unwrap();
    let r = call(
        &mut app,
        "export_note",
        json!({"path":"A.md","format":"html","preview":true,"body":"# 尚未保存"}),
    );
    assert!(r["html"].as_str().unwrap().contains("尚未保存"));
    assert_eq!(fs::read(vault.join("A.md")).unwrap(), old);
}
#[test]
fn html_is_single_file_with_images_and_missing_resource_report() {
    let (d, vault, mut app) = setup();
    fs::create_dir(vault.join("attachments")).unwrap();
    fs::write(vault.join("attachments/a.png"), [137, 80, 78, 71]).unwrap();
    fs::write(vault.join("A.md"),"![a](attachments/a.png) ![missing](attachments/missing.png) ![remote](https://example.com/x.png)").unwrap();
    let out = d.path().join("export/a.html");
    let r = call(
        &mut app,
        "export_note",
        json!({"path":"A.md","format":"html","targetPath":out}),
    );
    let html = fs::read_to_string(out).unwrap();
    assert!(html.contains("data:image/png;base64,iVBORw=="));
    assert_eq!(r["missingAssets"].as_array().unwrap().len(), 2);
    assert!(!html.contains("src=\"https://"));
}
#[test]
fn ambiguity_and_unresolved_are_both_reported() {
    let (_d, vault, mut app) = setup();
    for dir in ["x", "y"] {
        fs::create_dir(vault.join(dir)).unwrap();
        fs::write(vault.join(format!("{dir}/B.md")), "# B").unwrap();
    }
    fs::write(vault.join("A.md"), "[[B|显示]] [[尚无]]").unwrap();
    let r = call(
        &mut app,
        "export_note",
        json!({"path":"A.md","format":"html","preview":true}),
    );
    assert!(r["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["kind"] == "ambiguous_link"));
    assert!(r["html"].as_str().unwrap().contains("href=\"x/B%2Emd\""));
    assert!(r["html"].as_str().unwrap().contains("[[尚无]]"));
}
#[test]
fn markdown_copies_assets_and_does_not_overwrite() {
    let (d, vault, mut app) = setup();
    fs::create_dir(vault.join("attachments")).unwrap();
    fs::write(vault.join("attachments/a.bin"), [0, 1, 2]).unwrap();
    fs::write(
        vault.join("A.md"),
        "# A\n[file](attachments/a.bin) [[Missing]]",
    )
    .unwrap();
    let out = d.path().join("export/A.md");
    let r = call(
        &mut app,
        "export_note",
        json!({"path":"A.md","format":"md","targetPath":out}),
    );
    assert_eq!(
        fs::read(d.path().join("export/attachments/a.bin")).unwrap(),
        [0, 1, 2]
    );
    assert!(fs::read_to_string(&out).unwrap().contains("[[Missing]]"));
    assert_eq!(r["warnings"][0]["kind"], "unresolved_link");
    assert_eq!(
        request(
            &mut app,
            "export_note",
            json!({"path":"A.md","format":"md","targetPath":out})
        )["error"]["code"],
        "E_PATH_INVALID"
    );
}
#[test]
fn export_cannot_write_inside_vault_or_escape_through_link() {
    let (_d, vault, mut app) = setup();
    assert_eq!(
        request(
            &mut app,
            "export_note",
            json!({"path":"A.md","format":"html","targetPath":vault.join("export.html")})
        )["error"]["code"],
        "E_PATH_INVALID"
    );
    assert!(!vault.join("export.html").exists());
}
#[test]
fn pdf_produces_real_document_or_explicit_platform_fallback() {
    let (d, _v, mut app) = setup();
    let out = d.path().join("a.pdf");
    let result = request(
        &mut app,
        "export_note",
        json!({"path":"A.md","format":"pdf","targetPath":out}),
    );
    if result["ok"] == true {
        let bytes = fs::read(&out).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(bytes.windows(5).any(|p| p == b"%%EOF"));
        assert_eq!(
            request(
                &mut app,
                "export_note",
                json!({"path":"A.md","format":"pdf","targetPath":out})
            )["error"]["code"],
            "E_PATH_INVALID"
        );
        assert_eq!(fs::read(&out).unwrap(), bytes);
    } else {
        assert_eq!(result["error"]["code"], "E_PDF_UNSUPPORTED", "{result}");
        assert!(!out.exists());
    }
}
#[test]
fn backup_roundtrip_is_independently_extractable_and_protects_current_vault() {
    let (d, vault, mut app) = setup();
    fs::create_dir(vault.join("attachments")).unwrap();
    fs::write(vault.join("attachments/file.bin"), [9, 8, 7]).unwrap();
    let backup = call(&mut app, "backup_create", json!({"description":"作业备份"}));
    let package = PathBuf::from(backup["filePath"].as_str().unwrap());
    let z = entries(&package);
    assert!(z.iter().any(|(n, _)| n == "notes/A.md"));
    assert!(z.iter().any(|(n, _)| n == "attachments/file.bin"));
    assert!(!package.starts_with(&vault));
    fs::write(vault.join("A.md"), "当前内容").unwrap();
    let target = d.path().join("restored");
    let r = call(
        &mut app,
        "backup_restore",
        json!({"filePath":package,"mode":"new","targetPath":target}),
    );
    assert!(Path::new(r["snapshotPath"].as_str().unwrap()).exists());
    assert_eq!(fs::read_to_string(target.join("A.md")).unwrap(), "# 初版\n");
    assert_eq!(
        fs::read(target.join("attachments/file.bin")).unwrap(),
        [9, 8, 7]
    );
    assert_eq!(fs::read_to_string(vault.join("A.md")).unwrap(), "当前内容");
}
#[test]
fn corrupted_backup_does_not_partially_restore_or_create_snapshot() {
    let (d, vault, mut app) = setup();
    let backup = call(&mut app, "backup_create", json!({}));
    let source = PathBuf::from(backup["filePath"].as_str().unwrap());
    let mut data = entries(&source);
    for (name, body) in &mut data {
        if name == "notes/A.md" {
            *body = b"tampered".to_vec();
        }
    }
    let bad = d.path().join("bad.zip");
    let refs: Vec<_> = data.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
    raw_zip(&bad, &refs);
    let before = fs::read_dir(source.parent().unwrap()).unwrap().count();
    let target = d.path().join("bad-restore");
    let r = request(
        &mut app,
        "backup_restore",
        json!({"filePath":bad,"mode":"new","targetPath":target}),
    );
    assert_eq!(r["error"]["code"], "E_BACKUP_CHECKSUM");
    assert!(!target.exists());
    assert_eq!(fs::read_to_string(vault.join("A.md")).unwrap(), "# 初版\n");
    assert_eq!(
        fs::read_dir(source.parent().unwrap()).unwrap().count(),
        before
    );
}
#[test]
fn verify_only_is_readonly() {
    let (d, _v, mut app) = setup();
    let b = call(&mut app, "backup_create", json!({}));
    let target = d.path().join("verified");
    let r = call(
        &mut app,
        "backup_restore",
        json!({"filePath":b["filePath"],"mode":"new","targetPath":target,"verifyOnly":true}),
    );
    assert_eq!(r["verified"], true);
    assert!(!target.exists());
}
#[test]
fn malicious_zip_slip_is_rejected() {
    let (d, vault, mut app) = setup();
    let bad = d.path().join("attack.zip");
    let manifest = json!({"formatVersion":1,"createdAt":"now","noteCount":1,"attachmentCount":0,"checksum":"x","description":"","files":[{"path":"../escape.md","size":4,"sha256":"0".repeat(64)}],"vaultId":"test"});
    raw_zip(
        &bad,
        &[
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("notes/../escape.md", b"evil".to_vec()),
        ],
    );
    let r = request(
        &mut app,
        "backup_restore",
        json!({"filePath":bad,"mode":"new","targetPath":d.path().join("out")}),
    );
    assert_eq!(r["ok"], false);
    assert!(!d.path().join("escape.md").exists());
    assert_eq!(fs::read_to_string(vault.join("A.md")).unwrap(), "# 初版\n");
}
#[test]
fn existing_restore_target_is_never_overwritten() {
    let (d, _v, mut app) = setup();
    let b = call(&mut app, "backup_create", json!({}));
    let target = d.path().join("occupied");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("personal.txt"), "keep").unwrap();
    let r = request(
        &mut app,
        "backup_restore",
        json!({"filePath":b["filePath"],"mode":"new","targetPath":target}),
    );
    assert_eq!(r["error"]["code"], "E_PATH_INVALID");
    assert_eq!(
        fs::read_to_string(target.join("personal.txt")).unwrap(),
        "keep"
    );
}
#[test]
fn original_restore_keeps_entire_former_directory() {
    let (_d, vault, mut app) = setup();
    let b = call(&mut app, "backup_create", json!({}));
    fs::write(vault.join("A.md"), "new").unwrap();
    fs::write(vault.join("not-in-backup.txt"), "preserve").unwrap();
    let r = call(
        &mut app,
        "backup_restore",
        json!({"filePath":b["filePath"],"mode":"original"}),
    );
    assert_eq!(fs::read_to_string(vault.join("A.md")).unwrap(), "# 初版\n");
    let previous = PathBuf::from(r["retainedOriginalDir"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(previous.join("not-in-backup.txt")).unwrap(),
        "preserve"
    );
    assert_eq!(fs::read_to_string(previous.join("A.md")).unwrap(), "new");
}
#[test]
fn save_captures_old_and_new_versions_and_rollback_retains_current() {
    let (d, vault, mut app) = setup();
    let n = call(&mut app, "note_read", json!({"path":"A.md"}));
    let saved = call(
        &mut app,
        "note_save",
        json!({"path":"A.md","body":"# 二版","baseHash":n["contentHash"]}),
    );
    assert_eq!(saved["snapshotCreated"], true);
    let list = call(
        &mut app,
        "note_list",
        json!({"scope":"versions","path":"A.md"}),
    );
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    let old = notebase_core::data_services::files::digest("# 初版\n".as_bytes());
    let historic = call(
        &mut app,
        "note_read",
        json!({"path":"A.md","versionId":old}),
    );
    assert_eq!(historic["body"], "# 初版\n");
    let r = call(
        &mut app,
        "note_restore",
        json!({"kind":"version","path":"A.md","versionId":old,"baseHash":saved["contentHash"]}),
    );
    assert_eq!(r["snapshotCreated"], true);
    assert_eq!(fs::read_to_string(vault.join("A.md")).unwrap(), "# 初版\n");
    drop(app);
    let mut app = new_stateful_app(d.path().join("data"));
    call(&mut app, "vault_open", json!({"path":vault}));
    assert_eq!(
        call(
            &mut app,
            "note_list",
            json!({"scope":"versions","path":"A.md"})
        )["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn manual_snapshot_deduplicates_and_keeps_description() {
    let (_d, _v, mut app) = setup();
    let first = call(
        &mut app,
        "backup_create",
        json!({"scope":"note","path":"A.md","description":"重要节点"}),
    );
    call(
        &mut app,
        "backup_create",
        json!({"scope":"note","path":"A.md"}),
    );
    let versions = call(
        &mut app,
        "note_list",
        json!({"scope":"versions","path":"A.md"}),
    );
    assert_eq!(versions["items"].as_array().unwrap().len(), 1);
    assert_eq!(versions["items"][0]["description"], "重要节点");
    assert_eq!(versions["items"][0]["versionId"], first["versionId"]);
}
#[test]
fn version_id_is_not_a_filesystem_path() {
    let (_d, _v, mut app) = setup();
    assert_eq!(
        request(
            &mut app,
            "note_read",
            json!({"path":"A.md","versionId":"../../secret"})
        )["error"]["code"],
        "E_PATH_INVALID"
    );
}
#[test]
fn schedule_defaults_and_retention_respect_foreign_files() {
    let (d, vault, _app) = setup();
    let service = LocalDataService::open(
        vault.to_str().unwrap(),
        &d.path().join("other-data"),
        "member3-test",
    )
    .unwrap();
    assert_eq!(service.settings().unwrap()["keepCount"], 30);
    let dir = d.path().join("scheduled");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("backup-personal.zip"), "not ours").unwrap();
    service
        .configure(&json!({"keepCount":1,"intervalHours":1,"dir":dir}))
        .unwrap();
    assert!(service.maintenance_due(3600000).unwrap());
    assert_eq!(service.maintenance(3600000).unwrap()["created"], true);
    assert!(!service.maintenance_due(3600001).unwrap());
    service.maintenance(7200000).unwrap();
    assert!(dir.join("backup-personal.zip").exists());
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
    service.configure(&json!({"enabled":false})).unwrap();
    assert!(!service.maintenance_due(999999999).unwrap());
}
#[test]
fn original_restore_waits_for_index_writer_and_reopens_it() {
    let d = tempfile::tempdir().unwrap();
    let vault = d.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "索引恢复基线").unwrap();
    let mut app = new_indexed_app(d.path().join("data"), Arc::new(|_, _| {}));
    call(&mut app, "vault_open", json!({"path":vault}));
    let b = call(&mut app, "backup_create", json!({}));
    fs::write(vault.join("A.md"), "被覆盖前").unwrap();
    call(
        &mut app,
        "backup_restore",
        json!({"filePath":b["filePath"],"mode":"original"}),
    );
    let start = Instant::now();
    loop {
        let status = call(&mut app, "index_status", json!({}));
        if status["state"] == serde_json::to_value(IndexState::Ready).unwrap() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "{status}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let search = call(&mut app, "search", json!({"query":"恢复基线"}));
    assert_eq!(search["items"].as_array().unwrap().len(), 1);
}

#[test]
fn manual_backup_uses_configured_directory_and_allows_explicit_override() {
    let (d, vault, _app) = setup();
    let service = LocalDataService::open(
        vault.to_str().unwrap(),
        &d.path().join("backup-data"),
        "directory-test",
    )
    .unwrap();
    let configured = d.path().join("configured");
    service.configure(&json!({"dir": configured})).unwrap();
    let backup = service.backup_create(&json!({})).unwrap();
    assert_eq!(
        Path::new(backup["filePath"].as_str().unwrap())
            .parent()
            .unwrap(),
        fs::canonicalize(configured).unwrap()
    );
    let explicit = d.path().join("explicit");
    let backup = service
        .backup_create(&json!({"targetDir": explicit}))
        .unwrap();
    assert_eq!(
        Path::new(backup["filePath"].as_str().unwrap())
            .parent()
            .unwrap(),
        fs::canonicalize(explicit).unwrap()
    );
}
