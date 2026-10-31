//! Optional Windows Chromium print adapter. Uses an isolated profile and no shell.
use super::files;
use crate::domain::{AppError, Result};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn browser() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    for root in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(root) {
            for relative in [
                "Google/Chrome/Application/chrome.exe",
                "Microsoft/Edge/Application/msedge.exe",
            ] {
                let path = PathBuf::from(&root).join(relative);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

pub fn print(html: &str, cancelled: &std::sync::atomic::AtomicBool) -> Result<Vec<u8>> {
    let browser = browser().ok_or_else(|| {
        AppError::new(
            "E_PDF_UNSUPPORTED",
            "PDF 导出需要 Windows 上安装 Chrome 或 Edge；也可先导出 HTML",
        )
    })?;
    let stage = tempfile::tempdir().map_err(AppError::io)?;
    let input = stage.path().join("note.html");
    let output = stage.path().join("note.pdf");
    std::fs::write(&input, html).map_err(AppError::io)?;
    let url = reqwest::Url::from_file_path(&input)
        .map_err(|_| AppError::new("E_PATH_INVALID", "PDF 临时路径无效"))?;
    let mut command = Command::new(browser);
    command
        .args([
            "--headless",
            "--disable-gpu",
            "--disable-extensions",
            "--disable-background-networking",
            "--no-first-run",
            "--no-default-browser-check",
            "--no-pdf-header-footer",
            "--timeout=15000",
        ])
        .arg(format!(
            "--user-data-dir={}",
            stage.path().join("profile").display()
        ))
        .arg(format!("--print-to-pdf={}", output.display()))
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(AppError::io)?;
    let start = Instant::now();
    loop {
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::new("E_CANCELLED", "PDF 打印已取消，未发布输出"));
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return Err(AppError::new("E_INTERNAL", "PDF 打印进程失败，未发布输出")),
            Ok(None) if start.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(100))
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(e) => AppError::io(e),
                    _ => AppError::new("E_INTERNAL", "PDF 打印超时，未发布输出"),
                });
            }
        }
    }
    let bytes = files::read_limit(&output, files::ASSET_LIMIT)?;
    if !bytes.starts_with(b"%PDF-") || !bytes.windows(5).any(|part| part == b"%%EOF") {
        return Err(AppError::new(
            "E_INTERNAL",
            "打印结果不是完整 PDF，未发布输出",
        ));
    }
    Ok(bytes)
}
