//! 构建脚本：把 `assets/icon.rc` 编译成资源对象塞进 exe。
//!
//! 这份资源是唯一的图标来源：资源管理器/任务栏读 exe 的图标资源，托盘图标也直接从里面
//! `LoadImageW`（见 `src/tray.rs`），不再内嵌一份 ICO 在运行时解析——那套做法喂不了
//! PE 资源表，只能二选一。所以这里直接调 MSVC 的 `rc.exe`，不引入额外的构建依赖。
//!
//! 找不到 `rc.exe` 会直接让构建失败：图标已经是必需产物，静默少个图标比报错更难查。

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if let Err(message) = compile_icon() {
        // 构建脚本没有 `cargo:error` 这种输出，靠非零退出让 cargo 把 stderr 打出来。
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn compile_icon() -> Result<(), String> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    // 只对 MSVC 有意义：rc.exe 产出的是 COFF 资源对象，链接器直接当输入文件吃。
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return Ok(());
    }

    let rc = find_rc().ok_or_else(|| {
        "没找到 rc.exe。它随 Windows SDK 一起安装，通常在 \
         `%ProgramFiles(x86)%\\Windows Kits\\10\\bin\\<版本>\\x64\\`；\
         也可以用 RC 环境变量直接指定完整路径"
            .to_string()
    })?;

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let res = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("icon.res");

    // 工作目录设成项目根：icon.rc 里的 "assets/icon.ico" 是相对项目根写的。
    let output = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(manifest.join("assets/icon.rc"))
        .current_dir(&manifest)
        .output()
        .map_err(|e| format!("执行 {} 失败：{e}", rc.display()))?;

    if !output.status.success() {
        // rc.exe 的诊断有时走 stdout 有时走 stderr，两路都带上
        let detail: String = [output.stdout.as_slice(), output.stderr.as_slice()]
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        return Err(format!(
            "{} 编译 assets/icon.rc 失败：{detail}",
            rc.display()
        ));
    }
    println!("cargo:rustc-link-arg-bins={}", res.display());
    Ok(())
}

/// 按优先级找一个 rc.exe：`RC` 环境变量 → PATH → Windows SDK 安装目录。
fn find_rc() -> Option<PathBuf> {
    if let Some(rc) = env::var_os("RC").map(PathBuf::from) {
        if rc.is_file() {
            return Some(rc);
        }
    }
    from_path().or_else(from_sdk)
}

fn from_path() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join("rc.exe"))
        .find(|candidate| candidate.is_file())
}

/// Windows SDK 把 rc.exe 放在 `bin\<版本>\<架构>\` 下，架构目录名要按目标平台推。
fn from_sdk() -> Option<PathBuf> {
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x64",
        Ok("x86") => "x86",
        Ok("aarch64") => "arm64",
        _ => return None,
    };

    // 开发者命令提示符里会设 WindowsSdkVerBinPath，直接指向某个版本的 bin 目录
    if let Some(dir) = env::var_os("WindowsSdkVerBinPath") {
        let candidate = PathBuf::from(dir).join(arch).join("rc.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    let mut bin_dirs = Vec::new();
    if let Some(dir) = env::var_os("WindowsSdkDir") {
        bin_dirs.push(PathBuf::from(dir).join("bin"));
    }
    // 普通命令行下没有上面那些变量，退回 SDK 的默认安装位置
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(pf) = env::var_os(var) {
            bin_dirs.push(PathBuf::from(pf).join("Windows Kits").join("10").join("bin"));
        }
    }

    bin_dirs
        .into_iter()
        .find_map(|bin| newest_version_dir(&bin, arch))
}

/// 版本目录名形如 `10.0.22621.0`，取字典序最大的那个（版本号位数相同，对本项目够用）。
fn newest_version_dir(bin: &Path, arch: &str) -> Option<PathBuf> {
    let mut versions: Vec<PathBuf> = std::fs::read_dir(bin)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|dir| dir.join(arch).join("rc.exe").is_file())
        .collect();
    versions.sort();
    versions.pop().map(|dir| dir.join(arch).join("rc.exe"))
}
