//! 桌面图标排列器 —— 核心引擎 (Rust 重构版)
//!
//! 与 Python 版 (`desktop_icon_arranger.py`) 行为一一对应:
//! - `win32`  : Win32 桌面图标读写 (LVM_* 消息 / 跨进程内存)
//! - `sorting` : 5 种排序模式 + 自然拼音混排 + 槽位网格计算
//! - `classify`: 图标分类 (系统/软件/文件夹/文档/媒体/压缩包/其它)
//! - `state`   : 撤销快照的持久化 (与 Python 版同格式)

pub mod classify;
pub mod cli;
pub mod icons;
pub mod sorting;
pub mod state;
pub mod win32;

pub use classify::{KIND_LABELS, Kind, build_lookup, classify_entry};
pub use sorting::{Direction, SortMode, build_slots, desktop_metrics, natural_sort_key, plan};
pub use state::{Layer, StateFile, StateStore};
pub use win32::{DesktopIcons, IconEntry, find_desktop_listview};

/// 应用版本号 (前端通过 get_app_version 显示)
pub const APP_VERSION: &str = "桌面图标排列器 v3.0 (Rust)";

/// 应用标识符。
///
/// **必须与 `tauri.conf.json` 的 `identifier` 完全一致** ——
/// Tauri 用它决定 WebView2 用户数据目录 (`%LOCALAPPDATA%\<identifier>\EBWebView\`)、
/// 注册表项和单实例互斥体; 本项目的运行时数据目录也用它, 保持全局只有一套命名。
///
/// ⚠️ 两处各写一份必然会分叉 (本项目已经吃过一次亏: 图标缓存的 key 就是两处各算一半,
/// 结果缓存永远不命中)。所以 `tests/paths.rs` 里有断言盯着, 改了一处忘了另一处会直接测试失败。
///
/// 格式要求 (Tauri 规定): 只能小写 ASCII 字母 / 数字 / 连字符, 用 `.` 分段。
pub const APP_IDENTIFIER: &str = "com.desktopiconarranger.app";

/// 桌面图标工作区网格的默认单元格尺寸
pub const DEFAULT_CELL_W: i32 = 76;
pub const DEFAULT_CELL_H: i32 = 84;

/// 撤销历史最多保留份数 (与 Python 版一致)
pub const HISTORY_KEEP: usize = 20;
