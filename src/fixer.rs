//! 目标窗口查找，以及向"默认 IME 窗口"补发 WM_IME_SETCONTEXT。
//!
//! 背景：客户端把 `WM_IME_SETCONTEXT` 吞掉了（carbonui 的 `_OnSetContext` 无条件返回
//! True，引擎就不交给 `DefWindowProc`），IME 因此收不到"允许画 UI"的通知，候选框不显示。
//! 这条消息的语义是"授权"而不是"显示"，所以外部进程只要把它补发给游戏进程内的默认
//! IME 窗口，就能把候选框放行；不需要注入，也不需要改游戏文件。
//!
//! 三条实测约束，代码里都体现在注释处：
//!   1. 生效目标是 `ImmGetDefaultIMEWnd` 返回的那个窗口（class `IME`），不是游戏主窗口；
//!   2. 只在游戏窗口处于前台时发送才生效；
//!   3. 目标句柄时有时无（没关联 IME 上下文时为 0），所以每次现取，不缓存。

use std::collections::HashMap;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, TRUE};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsWindow,
    IsWindowVisible, SendMessageTimeoutW, SMTO_ABORTIFHUNG,
};

/// 让 IME 知道"你可以画 UI 了"的通知。windows crate 没有导出，自己定义。
const WM_IME_SETCONTEXT: u32 = 0x0281;

/// `ISC_SHOWUIALL`：composition 框、候选框、guideline 全部放行。
///
/// 这正是 Windows 原生流程下 `DefWindowProc` 会转发给 IME 的完整授权，也就是
/// "什么都不清理"的默认形态，所以行为最接近原本该有的样子。
/// 实测不会和游戏自绘的拼音框冲突（早期版本曾用 0x4000_000F 刻意去掉
/// `ISC_SHOWUICOMPOSITIONWINDOW` 那一位，现已确认没必要）。
pub const FLAGS: isize = 0xC000_000F;

/// 当前前台窗口。没有前台窗口时 Win32 返回 NULL，这里统一成空句柄。
pub fn foreground_window() -> HWND {
    unsafe { GetForegroundWindow() }
}

pub fn is_valid(hwnd: HWND) -> bool {
    !hwnd.0.is_null() && unsafe { IsWindow(Some(hwnd)).as_bool() }
}

pub fn same(a: HWND, b: HWND) -> bool {
    a.0 == b.0
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// 进程可执行文件名（小写，不含路径）。拿不到就返回空串。
fn process_basename(pid: u32) -> String {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit(['\\', '/'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
}

fn window_area(hwnd: HWND) -> i64 {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return 0;
    }
    i64::from(rect.right - rect.left).max(0) * i64::from(rect.bottom - rect.top).max(0)
}

struct Scan<'a> {
    exe: &'a str,
    /// 同一次扫描里同一个进程只解析一次镜像名，避免几十个窗口重复 OpenProcess。
    seen: HashMap<u32, bool>,
    found: Vec<HWND>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let scan = &mut *(lparam.0 as *mut Scan);
    if !IsWindowVisible(hwnd).as_bool() {
        return TRUE;
    }
    let pid = window_pid(hwnd);
    if pid == 0 {
        return TRUE;
    }
    let hit = match scan.seen.get(&pid) {
        Some(v) => *v,
        None => {
            let v = process_basename(pid) == scan.exe;
            scan.seen.insert(pid, v);
            v
        }
    };
    if hit {
        scan.found.push(hwnd);
    }
    TRUE
}

/// 找客户端主窗口：同进程的可见顶层窗口里，优先当前前台的那个，否则取面积最大的。
///
/// 每次调用都重新枚举，不缓存结果——这样"先开工具后开游戏"、游戏重启、多开切换都能跟上。
pub fn find_target(exe: &str) -> HWND {
    let mut scan = Scan {
        exe,
        seen: HashMap::new(),
        found: Vec::new(),
    };
    let ptr = LPARAM(&mut scan as *mut Scan as isize);
    if unsafe { EnumWindows(Some(enum_proc), ptr) }.is_err() || scan.found.is_empty() {
        return HWND(std::ptr::null_mut());
    }
    let fg = foreground_window();
    if scan.found.iter().any(|w| same(*w, fg)) {
        return fg;
    }
    scan.found
        .into_iter()
        .max_by_key(|w| window_area(*w))
        .unwrap_or(HWND(std::ptr::null_mut()))
}

pub enum Outcome {
    /// 已补发。delivered 是 SendMessageTimeout 的返回值，result 是窗口过程的返回值。
    Sent { delivered: bool, result: usize },
    /// 此刻没有默认 IME 窗口（没关联输入法上下文），稍后重试。
    NoImeWindow,
    /// 目标不在前台——实测这种时候发出去不生效，直接跳过。
    NotForeground,
    /// 没有目标窗口。
    NoTarget,
}

/// 向目标窗口的默认 IME 窗口补发一次 WM_IME_SETCONTEXT。
pub fn nudge(target: HWND, flags: isize) -> Outcome {
    if !is_valid(target) {
        return Outcome::NoTarget;
    }
    if !same(foreground_window(), target) {
        return Outcome::NotForeground;
    }
    let ime = unsafe { ImmGetDefaultIMEWnd(target) };
    if ime.0.is_null() {
        return Outcome::NoImeWindow;
    }
    let mut result = 0usize;
    let ret = unsafe {
        SendMessageTimeoutW(
            ime,
            WM_IME_SETCONTEXT,
            windows::Win32::Foundation::WPARAM(1),
            LPARAM(flags),
            SMTO_ABORTIFHUNG,
            500, // 目标是别的进程的窗口，超时保护，别把自己卡住
            Some(&mut result),
        )
    };
    Outcome::Sent {
        delivered: ret.0 != 0,
        result,
    }
}
