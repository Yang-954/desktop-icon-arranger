//! Tauri 图形界面外壳。
//!
//! 关键设计: **前端一行不改**, 通过注入一个 QWebChannel 兼容层
//! (`qwebchannel-shim.js`) 把 `backend.xxx(args..., callback)` 翻译成
//! Tauri 的 `invoke` 调用。
//!
//! shim 会在页面加载前注入, 定义全局 `window.qt.webChannelTransport`
//! 与 `window.QWebChannel`, 使现有 `web/index.html` 的
//! `initBackendBridge()` 逻辑可以原样工作。

pub mod backend;
pub mod commands;

use std::collections::HashMap;

use tauri::Manager;

use backend::{App, AppState};

/// 启动 Tauri 应用。
pub fn run() -> anyhow::Result<()> {
    let store = desktop_icon_arranger::StateStore::new(desktop_icon_arranger::cli::default_state_path());

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // 第二次启动 -> 把已有窗口拉到前台
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .manage(App {
            state: std::sync::Mutex::new(AppState::default()),
            store,
            settings: std::sync::Mutex::new(HashMap::new()),
        })
        .invoke_handler(tauri::generate_handler![
            commands::scan_icons,
            commands::get_desktop_info,
            commands::preview_sort,
            commands::apply_sort,
            commands::undo_snapshot,
            commands::restore_snapshot,
            commands::apply_custom_order,
            commands::preview_snapshot,
            commands::clear_history,
            commands::get_snapshot_list,
            commands::get_scan_sources,
            commands::save_setting,
            commands::get_all_settings,
            commands::get_app_version,
            commands::ping,
        ])
        .run(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("Tauri 运行失败: {e}"))
}
