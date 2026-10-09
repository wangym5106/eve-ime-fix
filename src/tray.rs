//! 托盘图标、气泡通知和右键菜单。全部用原生 Shell_NotifyIconW，
//! 不引入任何托盘封装库（那类库会拉进几十个依赖，违背单文件零依赖的目标）。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, ShellExecuteW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetSystemMetrics, LoadImageW,
    PostMessageW, SetForegroundWindow, TrackPopupMenu, HICON, IMAGE_ICON, LR_DEFAULTCOLOR,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, SM_CXSMICON, SM_CYSMICON, SW_SHOWNORMAL, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_NULL,
};

/// 托盘回调消息。`lParam` 会带回鼠标事件（WM_RBUTTONUP 等）。
pub const WM_TRAY: u32 = WM_APP + 1;

const TRAY_UID: u32 = 1;
const IDM_EXIT: usize = 1001;
const IDM_HOMEPAGE: usize = 1002;

/// 工具的完整中文名：托盘提示、启动通知和右键菜单都用它。
pub const DISPLAY_NAME: &str = "EVE输入法修复工具";
const AUTHOR: &str = "作者：Kidding Serenity";

/// 项目主页，点菜单里的"作者"那一项会打开它。
pub const HOMEPAGE: &str = "https://eve.009889.xyz/";

/// 菜单弹出结果。
#[derive(Default)]
pub struct MenuResult {
    /// 用户选择了"退出"。
    pub exit: bool,
    /// 用户点了"作者"那一项。
    pub homepage: bool,
    /// 弹出前 `SetForegroundWindow` 是否成功。纯诊断值，会写进日志。
    pub foreground_ok: bool,
}

/// 取出托盘用的小图标，直接读 exe 自己的图标资源。
///
/// 资源是 `build.rs` 调 `rc.exe` 把 `assets/icon.rc`（ID 为 1）编进去的，和资源管理器、
/// 任务栏看到的是同一份，所以不需要再内嵌一份 ICO 在运行时解析。
///
/// `LoadImageW` 会在图标组里挑和请求尺寸最接近的一张（系统小图标通常是 16×16），
/// 这里问 `SM_CXSMICON`，正好就是我们想要的那张。
pub fn load_icon() -> Option<HICON> {
    unsafe {
        let cx = GetSystemMetrics(SM_CXSMICON);
        let cy = GetSystemMetrics(SM_CYSMICON);
        // 不带 LR_SHARED：这里只取一次，退出时按常规 DestroyIcon 销毁更干净。
        // 资源 ID 1 用 MAKEINTRESOURCE 的形式表达，也就是把整数当地址传。
        LoadImageW(
            Some(HINSTANCE(GetModuleHandleW(None).ok()?.0)),
            PCWSTR(1 as *const u16),
            IMAGE_ICON,
            cx,
            cy,
            LR_DEFAULTCOLOR,
        )
        .ok()
        .map(|handle| HICON(handle.0))
    }
}

pub struct Tray {
    data: NOTIFYICONDATAW,
}

impl Tray {
    pub fn new(hwnd: HWND, icon: HICON) -> Self {
        let mut data = NOTIFYICONDATAW::default();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = TRAY_UID;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = icon;
        write_wide(&mut data.szTip, DISPLAY_NAME);
        Self { data }
    }

    /// 添加托盘图标。返回 false 基本只有一种可能：图标资源或 shell 状态异常。
    pub fn add(&self) -> bool {
        unsafe { Shell_NotifyIconW(NIM_ADD, &self.data).as_bool() }
    }

    pub fn remove(&self) {
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data) };
    }

    /// 气泡通知。注意用的是副本，不改动常驻的 data。
    pub fn notify(&self, title: &str, text: &str) {
        let mut data = self.data;
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO;
        write_wide(&mut data.szInfoTitle, title);
        write_wide(&mut data.szInfo, text);
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }
}

/// 弹出右键菜单。返回 true 表示用户选择了"退出"。
///
/// 按 Win32 的老规矩：弹出前先 `SetForegroundWindow`，弹出后补一条空消息，
/// 否则菜单点空白处不会消失。`foreground_ok` 会把 SetForegroundWindow 的结果带出去，
/// 菜单响应异常时它是最关键的线索。
pub fn show_menu(hwnd: HWND) -> MenuResult {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return MenuResult::default();
        };
        // 第一项是标题，灰显，免得看起来像能点的命令
        let name = to_wide(DISPLAY_NAME);
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(name.as_ptr()));
        // "作者"那一项是链接：不灰显才点得动，选中后交给 ShellExecuteW 打开 HOMEPAGE
        let author = to_wide(AUTHOR);
        let _ = AppendMenuW(menu, MF_STRING, IDM_HOMEPAGE, PCWSTR(author.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let exit = to_wide("退出");
        let _ = AppendMenuW(menu, MF_STRING, IDM_EXIT, PCWSTR(exit.as_ptr()));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let foreground_ok = SetForegroundWindow(hwnd).as_bool();
        let chosen = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        let picked = chosen.0 as usize;
        MenuResult {
            exit: picked == IDM_EXIT,
            homepage: picked == IDM_HOMEPAGE,
            foreground_ok,
        }
    }
}

/// 用默认浏览器打开项目主页。
///
/// 由工作线程调用：`ShellExecuteW` 会去启动 / 唤起浏览器进程，慢起来能顿住消息循环，
/// 和补发一样属于"不该占着主线程"的活儿。
///
/// 返回值按 Win32 的老约定判断：`HINSTANCE` 数值小于等于 32 表示失败，
/// 那是个错误码（比如 31 = 没有关联的处理器），不是句柄。
pub fn open_homepage() -> Result<(), u32> {
    let url = to_wide(HOMEPAGE);
    let handle = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(url.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = handle.0 as usize;
    if code > 32 {
        Ok(())
    } else {
        Err(code as u32)
    }
}

/// 把字符串写进定长的 UTF-16 缓冲，保证以 NUL 结尾；超长部分截断。
fn write_wide(dst: &mut [u16], src: &str) {
    let mut i = 0usize;
    for ch in src.encode_utf16() {
        if i + 1 >= dst.len() {
            break;
        }
        dst[i] = ch;
        i += 1;
    }
    dst[i] = 0;
}

fn to_wide(src: &str) -> Vec<u16> {
    let mut v: Vec<u16> = src.encode_utf16().collect();
    v.push(0);
    v
}
