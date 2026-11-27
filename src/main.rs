//! 桌面图标排列器 —— 图形界面入口。
//!
//! **为什么 GUI 和 CLI 是两个独立的二进制**:
//!
//! | | 子系统 | 用途 |
//! |---|---|---|
//! | `desktop-icon-arranger.exe` | `windows` (GUI) | 双击运行, **完全不弹控制台** |
//! | `desktop-icon-arranger-cli.exe` | 默认 (console) | 命令行, 有 stdout/stderr |
//!
//! Rust 的 `windows_subsystem` 是**编译期**属性, 没法按运行参数切换:
//! - 用 GUI 子系统 -> CLI 没 stdout
//! - 用 console 子系统 -> 双击会弹黑框; 而且在 Windows Terminal (ConPTY) 下
//!   `GetConsoleWindow()` 返回的是伪句柄, `ShowWindow` 根本无效, 藏不掉
//!
//! 所以只能拆成两个二进制 —— 这也是正规产品的做法。

// GUI 子系统: 不分配控制台, 双击无黑框。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(feature = "gui")]
mod gui;

fn main() {
    #[cfg(feature = "gui")]
    {
        if let Err(e) = gui::run() {
            // GUI 子系统没有 stderr, 错误只能弹窗
            show_error_dialog(&e.to_string());
            std::process::exit(1);
        }
        return;
    }

    #[cfg(not(feature = "gui"))]
    {
        eprintln!("此二进制未启用 gui 特性, 请使用 desktop-icon-arranger-cli.exe");
        std::process::exit(2);
    }
}

/// 弹一个原生错误对话框 (GUI 子系统没有 stderr 可用)。
fn show_error_dialog(msg: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MessageBoxW,
    };
    use windows::core::PCWSTR;

    let wide: Vec<u16> = format!("桌面图标排列器\n\n{msg}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(wide.as_ptr()),
            PCWSTR(wide.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}
