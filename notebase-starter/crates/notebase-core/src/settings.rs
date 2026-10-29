//! Shell preferences share settings.json with vault migration; unknown keys survive updates.
use crate::domain::{AppError, Result};
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, io::Write, path::PathBuf};

pub trait SettingsStore: Send + Sync {
    fn read(&self) -> Result<Value>;
    fn update(&self, patch: &Value) -> Result<()>;
}
pub fn defaults() -> Value {
    json!({"theme":"system","layoutPreset":"student","editor":{"fontSize":14,"lineWidth":90,"tabSize":4,"wordWrap":true},"keymap":{"new":"Ctrl+N","search":"Ctrl+Shift+F","save":"Ctrl+S","nextTab":"Ctrl+Tab","commandPalette":"Ctrl+Shift+P","globalQuickNote":"Ctrl+Shift+Space"}})
}
fn invalid(message: &str) -> AppError {
    AppError::new("E_PATH_INVALID", message)
}
pub fn merged(current: &Value, patch: &Value) -> Result<Value> {
    let patch = patch.as_object().ok_or_else(|| invalid("设置必须是对象"))?;
    let mut next = current.clone();
    for (key, value) in patch {
        match key.as_str() {
            "theme" if matches!(value.as_str(), Some("light" | "dark" | "system")) => {}
            "layoutPreset"
                if matches!(
                    value.as_str(),
                    Some("student" | "researcher" | "developer" | "writer")
                ) => {}
            "editor" => {
                for (name, value) in value
                    .as_object()
                    .ok_or_else(|| invalid("editor 必须是对象"))?
                {
                    let valid = match name.as_str() {
                        "fontSize" => value.as_u64().is_some_and(|n| (10..=32).contains(&n)),
                        "lineWidth" => value.as_u64().is_some_and(|n| (40..=200).contains(&n)),
                        "tabSize" => value.as_u64().is_some_and(|n| (1..=8).contains(&n)),
                        "wordWrap" => value.is_boolean(),
                        _ => false,
                    };
                    if !valid {
                        return Err(invalid("编辑器偏好超出范围"));
                    }
                }
            }
            "keymap" => {
                let allowed = defaults()["keymap"].as_object().unwrap().clone();
                for (name, value) in value
                    .as_object()
                    .ok_or_else(|| invalid("keymap 必须是对象"))?
                {
                    if !allowed.contains_key(name) || !value.as_str().is_some_and(valid_shortcut) {
                        return Err(AppError::new("E_KEYMAP_CONFLICT", "快捷键名称或格式无效"));
                    }
                }
            }
            _ => return Err(invalid("不支持的设置键或值")),
        }
        if matches!(key.as_str(), "editor" | "keymap") {
            for (name, field) in value.as_object().unwrap() {
                next[key][name] = field.clone();
            }
        } else {
            next[key] = value.clone();
        }
    }
    let mut seen = BTreeSet::new();
    for value in next["keymap"]
        .as_object()
        .ok_or_else(|| invalid("无效快捷键配置"))?
        .values()
    {
        let shortcut = value.as_str().ok_or_else(|| invalid("无效快捷键配置"))?;
        let mut parts: Vec<_> = shortcut
            .to_ascii_lowercase()
            .split('+')
            .map(str::to_owned)
            .collect();
        let key = parts.pop().unwrap_or_default();
        parts.sort();
        parts.push(key);
        if !seen.insert(parts.join("+")) {
            return Err(AppError::new(
                "E_KEYMAP_CONFLICT",
                "快捷键重复，请为每个动作选择不同组合",
            ));
        }
    }
    Ok(next)
}
fn valid_shortcut(shortcut: &str) -> bool {
    let parts: Vec<_> = shortcut.split('+').collect();
    if !(2..=4).contains(&parts.len()) {
        return false;
    }
    let mut modifiers = BTreeSet::new();
    for modifier in &parts[..parts.len() - 1] {
        if !matches!(*modifier, "Ctrl" | "Alt" | "Shift" | "Meta") || !modifiers.insert(*modifier) {
            return false;
        }
    }
    let key = parts[parts.len() - 1];
    (key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric())
        || matches!(key, "Tab" | "Space" | "Enter")
}
pub struct FileSettings(pub PathBuf);
impl SettingsStore for FileSettings {
    fn read(&self) -> Result<Value> {
        let mut value = defaults();
        match fs::read(&self.0) {
            Ok(raw) => {
                if raw.len() > 1024 * 1024 {
                    return Err(invalid("设置文件过大"));
                }
                let stored: Value = serde_json::from_slice(&raw)
                    .map_err(|_| invalid("设置文件损坏，原件已保留"))?;
                for (key, item) in stored
                    .as_object()
                    .ok_or_else(|| invalid("设置文件必须是对象"))?
                {
                    if matches!(key.as_str(), "editor" | "keymap") {
                        for (name, field) in
                            item.as_object().ok_or_else(|| invalid("设置字段损坏"))?
                        {
                            value[key][name] = field.clone();
                        }
                    } else {
                        value[key] = item.clone();
                    }
                }
                // Validate recognized preferences without discarding other members' keys.
                merged(
                    &value,
                    &json!({"theme":value["theme"],"layoutPreset":value["layoutPreset"],"editor":value["editor"],"keymap":value["keymap"]}),
                )?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(AppError::io(error)),
        }
        Ok(value)
    }
    fn update(&self, patch: &Value) -> Result<()> {
        let value = merged(&self.read()?, patch)?;
        let parent = self.0.parent().ok_or_else(|| invalid("设置路径无父目录"))?;
        fs::create_dir_all(parent).map_err(AppError::io)?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(AppError::io)?;
        file.write_all(&serde_json::to_vec_pretty(&value).map_err(|_| invalid("设置序列化失败"))?)
            .map_err(AppError::io)?;
        file.as_file().sync_all().map_err(AppError::io)?;
        file.persist(&self.0)
            .map_err(|error| AppError::io(error.error))?;
        Ok(())
    }
}
