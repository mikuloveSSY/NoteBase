use std::sync::mpsc;
pub struct GlobalShortcut(mpsc::Sender<(String, mpsc::Sender<Result<(), String>>)>);
impl GlobalShortcut {
    pub fn start(activate: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel::<(String, mpsc::Sender<Result<(), String>>)>();
        std::thread::spawn(move || run(rx, activate));
        Self(tx)
    }
    pub fn configure(&self, value: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.0
            .send((value.to_owned(), tx))
            .map_err(|_| "快捷键线程已停止")?;
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| "快捷键注册超时")?
    }
}
#[cfg(windows)]
fn run(rx: mpsc::Receiver<(String, mpsc::Sender<Result<(), String>>)>, activate: impl Fn()) {
    use windows_sys::Win32::UI::{
        Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
            MOD_WIN,
        },
        WindowsAndMessaging::{PeekMessageW, MSG, PM_REMOVE, WM_HOTKEY},
    };
    let mut current = String::new();
    let mut id = 1;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok((value, reply)) => {
                if value == current {
                    let _ = reply.send(Ok(()));
                    continue;
                }
                let mut modifiers = MOD_NOREPEAT;
                let parts: Vec<_> = value.split('+').collect();
                for part in &parts[..parts.len().saturating_sub(1)] {
                    modifiers |= match *part {
                        "Ctrl" => MOD_CONTROL,
                        "Shift" => MOD_SHIFT,
                        "Alt" => MOD_ALT,
                        "Meta" => MOD_WIN,
                        _ => 0,
                    };
                }
                let key = match parts.last().copied().unwrap_or("") {
                    "Space" => 32,
                    "Tab" => 9,
                    "Enter" => 13,
                    key if key.len() == 1 => u32::from(key.as_bytes()[0].to_ascii_uppercase()),
                    _ => 0,
                };
                let next = if id == 1 { 2 } else { 1 };
                if key == 0
                    || unsafe { RegisterHotKey(std::ptr::null_mut(), next, modifiers, key) } == 0
                {
                    let _ = reply.send(Err(
                        "全局快捷键被占用或不受系统支持，原快捷键保持不变".into()
                    ));
                } else {
                    if !current.is_empty() {
                        unsafe {
                            UnregisterHotKey(std::ptr::null_mut(), id);
                        }
                    }
                    current = value;
                    id = next;
                    let _ = reply.send(Ok(()));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let mut message: MSG = unsafe { std::mem::zeroed() };
        while unsafe {
            PeekMessageW(
                &mut message,
                std::ptr::null_mut(),
                WM_HOTKEY,
                WM_HOTKEY,
                PM_REMOVE,
            )
        } != 0
        {
            activate();
        }
    }
    if !current.is_empty() {
        unsafe {
            UnregisterHotKey(std::ptr::null_mut(), id);
        }
    }
}
#[cfg(not(windows))]
fn run(rx: mpsc::Receiver<(String, mpsc::Sender<Result<(), String>>)>, _activate: impl Fn()) {
    for (_, reply) in rx {
        let _ = reply.send(Err("此构建暂未接入当前平台全局快捷键".into()));
    }
}
