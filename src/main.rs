//! eve-ime-fix —— 让 EVE Online 客户端里的输入法候选框显示出来（零注入）。
//!
//! 没有任何窗口，只有一个托盘图标：右键菜单是"作者 / 退出"，启动时弹一条气泡通知。
//!
//! 线程划分：
//!   主线程：只跑消息循环，负责托盘图标和前台事件钩子。所有跨进程调用都不在这里做，
//!           否则发消息给游戏进程时可能卡住（最坏 200ms 超时），菜单就会跟着迟钝。
//!   工作线程：找窗口、补发、定时保底、打开主页链接，全部在这里。
//!
//! 补发的原理见 `fixer` 模块。

#![windows_subsystem = "windows"]

mod fixer;
mod logging;
mod tray;

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, Error, PCWSTR, Result};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW,
    EVENT_SYSTEM_FOREGROUND, GetMessageW, HICON, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, TranslateMessage, MSG, WNDCLASSW, WNDCLASS_STYLES,
    PostMessageW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_CONTEXTMENU,
    WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WS_EX_TOOLWINDOW,
};

use logging::Log;

const CLASS_NAME: PCWSTR = w!("EveImeFixHiddenWindow");
const WINDOW_TITLE: PCWSTR = w!("eve-ime-fix");
const MUTEX_NAME: PCWSTR = w!("Local\\eve-ime-fix");

/// 目标窗口重新解析的间隔
const RESCAN_MS: u64 = 5_000;
/// 前台期间的保底补发间隔
const RESEND_MS: u64 = 2_000;
/// 工作线程在没有命令时的轮询间隔
const WORKER_POLL_MS: u64 = 200;
/// 保底重发成功后，同类日志最多隔这么久记一条
const HEARTBEAT_SECS: u64 = 60;

const EV_FOREGROUND: u32 = 1 << 0;
const EV_MENU: u32 = 1 << 1;
const EV_READD_TRAY: u32 = 1 << 2;

/// 用来把消息循环叫醒的自定义消息，本身不携带信息，见 `pend`。
const WM_IME_FIX_WAKE: u32 = WM_APP + 2;

/// `NOTIFYICON_VERSION_4` 下左键点击的等价事件（老格式是 WM_LBUTTONUP）。
const NIN_SELECT: u32 = 0x0400;

thread_local! {
    /// 窗口过程/事件回调只置位；真正的活儿交给主循环，免得重入消息循环时出问题。
    static PENDING: Cell<u32> = const { Cell::new(0) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

/// 主线程和工作线程共用一个日志文件，靠这把锁串行化。
static LOG: OnceLock<Mutex<Log>> = OnceLock::new();

/// 我们自己的隐藏窗口句柄，供事件回调把消息循环叫醒。
static SELF_HWND: AtomicIsize = AtomicIsize::new(0);

// --------------------------------------------------------------------------
// 日志
// --------------------------------------------------------------------------
fn init_log() {
    let _ = LOG.set(Mutex::new(Log::create()));
}

pub(crate) fn say(msg: &str) {
    if let Some(log) = LOG.get() {
        if let Ok(mut log) = log.lock() {
            log.line(msg);
        }
    }
}

/// 后台进程没有控制台，panic 默认什么都不留，这里补一条。
/// 注意不能用全局 LOG——panic 有可能正发生在持锁期间。
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        Log::append().line(&format!("panic: {info}"));
    }));
}

// --------------------------------------------------------------------------
// 主线程：窗口过程与事件回调只置位
// --------------------------------------------------------------------------
/// 置位，并**叫醒消息循环**。
///
/// 这一步不能省：托盘回调是发送（SendMessage）过来的，不经过消息队列。
/// 只置位的话主线程会一直阻塞在 GetMessageW 上直到下一条队列消息，
/// `take_pending()` 根本轮不到执行——菜单点了没反应就是这么来的。
fn pend(bit: u32) {
    let before = PENDING.with(|p| p.replace(p.get() | bit));
    if before & bit == 0 {
        wake();
    }
}

fn wake() {
    let raw = SELF_HWND.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(raw as *mut c_void)),
                WM_IME_FIX_WAKE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

fn take_pending() -> u32 {
    PENDING.with(|p| p.replace(0))
}

fn empty_hwnd() -> HWND {
    HWND(std::ptr::null_mut())
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // 唤醒消息本身不带信息，活儿都留给主循环处理
        WM_IME_FIX_WAKE => LRESULT(0),
        tray::WM_TRAY => {
            // lParam 带回的是鼠标事件（老格式）或 NIN_* 事件（v4 格式）。
            // 只在要响应的事件上记日志：鼠标划过会刷出上千条 WM_MOUSEMOVE。
            let event = lp.0 as u32;
            if event == WM_RBUTTONUP
                || event == WM_LBUTTONUP
                || event == WM_CONTEXTMENU
                || event == NIN_SELECT
            {
                say(&format!("托盘事件 0x{event:X}"));
                pend(EV_MENU);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => {
            // 资源管理器重启会广播 TaskbarCreated，收到后要重新添加托盘图标
            if msg != 0 && msg == TASKBAR_CREATED.with(Cell::get) {
                pend(EV_READD_TRAY);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // OBJID_WINDOW = 0, CHILDID_SELF = 0：只关心顶层窗口自身的可访问性事件
    if id_object == 0 && id_child == 0 {
        pend(EV_FOREGROUND);
    }
}

// --------------------------------------------------------------------------
// 工作线程：所有跨进程调用都在这里
// --------------------------------------------------------------------------
enum Cmd {
    /// 前台窗口变了，可能是切回了客户端。
    Foreground,
    /// 菜单里点了"作者"，去打开主页。
    OpenHomepage,
    /// 主线程要退出了。
    Quit,
}

struct App {
    exe: String,
    target: HWND,
    sent: u32,
    last_attempt: Instant,
    last_rescan: Instant,
    /// 上次写"已补发"日志的时刻，用来给保底重发节流
    last_report: Instant,
}

impl App {
    fn new(exe: String) -> Self {
        Self {
            exe,
            target: empty_hwnd(),
            sent: 0,
            // 置成"很久以前"，让首次进入前台立刻允许一次补发
            last_attempt: Instant::now() - Duration::from_secs(3600),
            last_rescan: Instant::now() - Duration::from_secs(3600),
            last_report: Instant::now() - Duration::from_secs(HEARTBEAT_SECS),
        }
    }

    /// 重新解析目标窗口；句柄变了才记日志，避免刷屏。
    fn rescan(&mut self) {
        self.last_rescan = Instant::now();
        let found = fixer::find_target(&self.exe);
        if fixer::same(found, self.target) {
            return;
        }
        self.target = found;
        if fixer::is_valid(found) {
            say(&format!("发现客户端窗口 hwnd=0x{:X}", found.0 as usize));
        } else {
            say("客户端窗口消失，继续等待");
        }
    }

    /// 补发一次。
    ///
    /// `always_log` 为假时给日志节流：保底重发是每 2 秒重复同一件事，
    /// 全记下来会把日志刷满，完全不记又看不出"工具还活着"或者"一直没成功"。
    /// 所以成功和失败都按 `HEARTBEAT_SECS` 节流，只有"切回前台时补发"这类
    /// 真正的关键事件才无条件记录。
    fn nudge(&mut self, why: &str, always_log: bool) {
        self.last_attempt = Instant::now();
        let outcome = fixer::nudge(self.target, fixer::FLAGS);
        let report =
            always_log || self.last_report.elapsed() >= Duration::from_secs(HEARTBEAT_SECS);
        if report {
            self.last_report = Instant::now();
        }
        match outcome {
            fixer::Outcome::Sent { delivered, result } => {
                self.sent += 1;
                if report {
                    say(&format!(
                        "已补发({why}) lParam=0x{:08X} delivered={delivered} result={result}，累计 {} 次",
                        fixer::FLAGS as u32,
                        self.sent
                    ));
                }
            }
            fixer::Outcome::NoImeWindow => {
                if report {
                    say(&format!("补发({why})失败：默认 IME 窗口当前为 0，稍后重试"));
                }
            }
            // 这两种情况很常见（客户端没在前台、客户端没开），静默跳过
            fixer::Outcome::NotForeground | fixer::Outcome::NoTarget => {}
        }
    }

    fn on_foreground(&mut self) {
        if !fixer::is_valid(self.target) {
            self.rescan();
        }
        if fixer::is_valid(self.target) && fixer::same(fixer::foreground_window(), self.target) {
            self.nudge("前台激活", true);
        }
    }

    /// 无命令时的例行检查：到点了就重解析目标、保底补发一次。
    fn tick(&mut self, now: Instant) {
        if now.duration_since(self.last_rescan) >= Duration::from_millis(RESCAN_MS) {
            self.rescan();
        }
        if fixer::is_valid(self.target)
            && fixer::same(fixer::foreground_window(), self.target)
            && now.duration_since(self.last_attempt) >= Duration::from_millis(RESEND_MS)
        {
            self.nudge("保底重发", false);
        }
    }
}

fn worker_main(rx: Receiver<Cmd>, exe: String) {
    say(&format!("监听进程：{exe}"));
    let mut app = App::new(exe);
    app.rescan();
    loop {
        app.tick(Instant::now());
        match rx.recv_timeout(Duration::from_millis(WORKER_POLL_MS)) {
            Ok(Cmd::Foreground) => app.on_foreground(),
            Ok(Cmd::OpenHomepage) => match tray::open_homepage() {
                Ok(()) => say(&format!("已打开主页 {}", tray::HOMEPAGE)),
                Err(code) => say(&format!(
                    "打开主页 {} 失败：ShellExecuteW 返回 {code}",
                    tray::HOMEPAGE
                )),
            },
            Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    say("工作线程结束");
}

// --------------------------------------------------------------------------
// 入口
// --------------------------------------------------------------------------
fn main() {
    unsafe {
        // 单实例：第二个实例直接静默退出
        let mutex = CreateMutexW(None, true, MUTEX_NAME);
        if let Ok(handle) = &mutex {
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(*handle);
                return;
            }
        }

        init_log();
        install_panic_hook();

        if let Err(e) = run() {
            say(&format!("运行失败：{e}"));
        }

        if let Ok(handle) = mutex {
            let _ = CloseHandle(handle);
        }
    }
}

fn run() -> Result<()> {
    say(&format!(
        "======== eve-ime-fix {} 启动 ========",
        env!("CARGO_PKG_VERSION")
    ));
    say(&format!("日志文件：{}", logging::log_path().display()));

    let hwnd = unsafe {
        let instance = GetModuleHandleW(None)?;
        let hinstance = HINSTANCE(instance.0);

        let wc = WNDCLASSW {
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_thread());
        }

        TASKBAR_CREATED.with(|c| c.set(RegisterWindowMessageW(w!("TaskbarCreated"))));

        // 不传 WS_VISIBLE：窗口永远不显示，只是一个消息接收器
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS_NAME,
            WINDOW_TITLE,
            Default::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )?
    };
    say(&format!("隐藏窗口创建完成 hwnd=0x{:X}", hwnd.0 as usize));
    SELF_HWND.store(hwnd.0 as isize, Ordering::Relaxed);

    let (tx, rx) = mpsc::channel();
    let exe = client_exe_name();
    let worker = thread::spawn(move || worker_main(rx, exe));

    let tray = match tray::load_icon() {
        Some(icon) => {
            let t = tray::Tray::new(hwnd, icon);
            if t.add() {
                say("托盘图标已添加");
                t.notify(tray::DISPLAY_NAME, "已启动至托盘");
            } else {
                say("警告：托盘图标添加失败（Shell_NotifyIcon 返回 false）");
            }
            Some((t, icon))
        }
        None => {
            say("警告：加载图标资源失败，托盘图标不会显示");
            None
        }
    };

    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if hook.0.is_null() {
        say("警告：前台事件钩子安装失败，将只依赖保底重发");
    }

    event_loop(hwnd, tray.as_ref(), &tx);

    drop(tx); // 断开通道，工作线程随即退出
    let _ = worker.join();

    unsafe {
        if !hook.0.is_null() {
            let _ = UnhookWinEvent(hook);
        }
        if let Some((t, icon)) = tray {
            t.remove();
            let _ = DestroyIcon(icon);
        }
        let _ = DestroyWindow(hwnd);
    }
    say("已退出");
    Ok(())
}

/// 主线程的消息循环。什么重活都不干，只发命令、管托盘。
fn event_loop(hwnd: HWND, tray: Option<&(tray::Tray, HICON)>, tx: &Sender<Cmd>) {
    let mut msg = MSG::default();
    loop {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break; // 0 = WM_QUIT，-1 = 出错
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let events = take_pending();
        if events & EV_READD_TRAY != 0 {
            if let Some((t, _)) = tray {
                let ok = if t.add() { "成功" } else { "失败" };
                say(&format!("资源管理器重启，重新添加托盘图标：{ok}"));
            }
        }
        if events & EV_FOREGROUND != 0 {
            let _ = tx.send(Cmd::Foreground);
        }
        if events & EV_MENU != 0 {
            let menu = tray::show_menu(hwnd);
            say(&format!(
                "弹出托盘菜单：SetForegroundWindow={} 选择退出={} 选择作者链接={}",
                menu.foreground_ok, menu.exit, menu.homepage
            ));
            if menu.homepage {
                let _ = tx.send(Cmd::OpenHomepage);
            }
            if menu.exit {
                let _ = tx.send(Cmd::Quit);
                break;
            }
        }
    }
}

/// 客户端进程名，默认 `exefile.exe`；可以用 `--exe <名字>` 覆盖。
fn client_exe_name() -> String {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--exe=") {
            return value.to_ascii_lowercase();
        }
        if arg == "--exe" {
            if let Some(value) = args.next() {
                return value.to_ascii_lowercase();
            }
        }
    }
    "exefile.exe".to_string()
}
