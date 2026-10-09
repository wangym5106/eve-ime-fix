//! 日志。位置 `%LOCALAPPDATA%\eve-ime-fix\eve-ime-fix.log`，**每次启动覆盖**。
//!
//! 程序是后台进程、没有控制台，出问题时这里是唯一的线索，默认一直开着。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use windows::Win32::System::SystemInformation::GetLocalTime;

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

pub struct Log {
    file: Option<File>,
}

impl Log {
    /// 启动时用：清空旧日志从头开始。
    ///
    /// 顺手写个 UTF-8 BOM——日志内容全是中文，没有 BOM 时记事本和 PowerShell 的
    /// `Get-Content` 会按 ANSI 解码成一堆乱码。
    pub fn create() -> Self {
        let path = log_path();
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .ok();
        if let Some(f) = file.as_mut() {
            let _ = f.write_all(BOM);
        }
        Self { file }
    }

    /// 追加模式。给 panic 钩子用：既不能截掉已有内容，也不能走主线程那个全局锁
    /// （panic 有可能正是发生在持锁期间）。
    pub fn append() -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path())
            .ok();
        Self { file }
    }

    pub fn line(&mut self, msg: &str) {
        let text = format!("[{}] {}\r\n", timestamp(), msg);
        if let Some(file) = self.file.as_mut() {
            let _ = file.write_all(text.as_bytes());
            let _ = file.flush(); // 日志量很小，逐行落盘，崩溃时不会丢内容
        }
    }
}

pub fn log_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("eve-ime-fix")
        .join("eve-ime-fix.log")
}

fn timestamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}
