//! Tauri 后端: 把核心引擎包装成前端可调用的命令。
//!
//! **设计要点: 保留 QWebChannel 的调用形态。**
//!
//! 现有前端 (`web/index.html`) 通过 QWebChannel 调用 `backend.xxx(参数, 回调)`。
//! 为了让 2200 行前端代码一行不改, 这里:
//! - Rust 侧提供 `invoke` 命令, 参数与返回都是字符串/布尔 (与 Python 版 Slot 签名一致)
//! - 前端注入一个 `qwebchannel-shim.js`, 把 `backend.xxx(args..., cb)` 翻译成
//!   `window.__TAURI__.core.invoke('xxx', args)` 并在回调里回填结果
//!
//! 对应的 Python 方法 -> Rust 命令名映射:
//!
//! | Python Slot | Rust 命令 | 参数 | 返回 |
//! |---|---|---|---|
//! | `scan_icons()` | `scan_icons` | - | String (JSON) |
//! | `get_desktop_info()` | `get_desktop_info` | - | String (JSON) |
//! | `preview_sort(mode, direction, reverse)` | `preview_sort` | 3 | String (JSON) |
//! | `apply_sort(mode, direction, reverse)` | `apply_sort` | 3 | bool |
//! | `undo_snapshot()` | `undo_snapshot` | - | bool |
//! | `restore_snapshot(depth)` | `restore_snapshot` | 1 | bool |
//! | `apply_custom_order(order_json, direction)` | `apply_custom_order` | 2 | bool |
//! | `preview_snapshot(depth)` | `preview_snapshot` | 1 | String (JSON) |
//! | `clear_history()` | `clear_history` | - | bool |
//! | `get_snapshot_list()` | `get_snapshot_list` | - | String (JSON) |
//! | `get_scan_sources()` | `get_scan_sources` | - | String (JSON) |
//! | `save_setting(key, value)` | `save_setting` | 2 | bool |
//! | `get_all_settings()` | `get_all_settings` | - | String (JSON) |
//! | `get_app_version()` | `get_app_version` | - | String |

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use desktop_icon_arranger::{
    StateStore,
    classify::{Kind, build_lookup},
    win32::{DesktopIcons, find_desktop_listview},
};

/// 应用状态 (与 Python 版 Backend 的成员一一对应)。
#[derive(Default)]
pub struct AppState {
    /// 当前扫描到的图标 (index, name, x, y)
    pub cached_icons: Vec<desktop_icon_arranger::IconEntry>,
    /// 显示名 -> (类别, 路径)
    pub cached_lookup: HashMap<String, (Kind, Option<PathBuf>)>,
    /// 前端用的条目列表
    pub cached_items: Vec<serde_json::Value>,
    /// 图标 Base64 缓存
    pub icon_cache: HashMap<String, String>,
}

/// Tauri 全局状态
pub struct App {
    pub state: Mutex<AppState>,
    pub store: StateStore,
    pub settings: Mutex<HashMap<String, String>>,
}

/// 在已附加到交互式桌面的工作线程里执行 `f`。
///
/// **为什么必须用工作线程**:
/// `SetThreadDesktop` 会把线程切到 default 桌面, 但该桌面句柄是**按权限打开**的。
/// 如果打开时缺少 `DESKTOP_CREATEWINDOW (0x0002)`, 切换后该线程就无权创建窗口 ——
/// 而 Tauri 的主线程恰恰要创建窗口。
///
/// 所以:
/// - **主线程** (Tauri 事件循环): 绝不切桌面, 保持原桌面
/// - **工作线程** (访问 Explorer): 每次操作开一个新线程, 切桌面后干活, 干完就退出
///
/// 对应 Python 版 `Backend._run_win32` 的做法 (在独立线程里调 `ensure_interactive_desktop`)。
pub fn on_desktop_thread<T, F>(f: F) -> anyhow::Result<T>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        desktop_icon_arranger::win32::ensure_interactive_desktop();
        let _ = tx.send(f());
    });
    rx.recv()
        .map_err(|_| anyhow::anyhow!("桌面操作线程异常退出"))?
}

/// 读取当前桌面的图标与查找表 (在工作线程里执行)。
pub fn scan_desktop(
) -> anyhow::Result<(Vec<desktop_icon_arranger::IconEntry>, HashMap<String, (Kind, Option<PathBuf>)>)> {
    on_desktop_thread(|| scan_desktop_in_thread())
}

/// 读取当前桌面的图标与查找表 —— **假定当前线程已经附加到交互桌面**。
///
/// 供已经处在 `on_desktop_thread` 里的代码调用, 避免嵌套开线程。
pub fn scan_desktop_in_thread(
) -> anyhow::Result<(Vec<desktop_icon_arranger::IconEntry>, HashMap<String, (Kind, Option<PathBuf>)>)> {
    let Some(lv) = find_desktop_listview() else {
        anyhow::bail!("未找到桌面 ListView");
    };
    let mut di = DesktopIcons::new(lv)?;
    let icons = di.read_all();
    drop(di);

    let dirs = desktop_icon_arranger::win32::desktop_dirs();
    let mut conflicts = Vec::new();
    let lookup = build_lookup(&dirs, &mut conflicts);
    Ok((icons, lookup))
}

/// 构造前端条目列表 (与 Python 版 scan_icons 的 items 结构一致)。
///
/// **同时返回每个条目的图标缓存 key** —— 缓存的读和写必须用**同一个** key,
/// 所以让构造函数把 key 一并产出, 调用方直接拿去写缓存,
/// 避免"读用 kind_raw、写用 cat"这种各算一半的分叉
/// (分叉的后果: 缓存永远 miss, 每次刷新图标都消失)。
///
/// 返回: `(items, cache_keys)`, `cache_keys[i]` 对应 `items[i]`。
pub fn build_items(
    icons: &[desktop_icon_arranger::IconEntry],
    lookup: &HashMap<String, (Kind, Option<PathBuf>)>,
    icon_cache: &HashMap<String, String>,
) -> (Vec<serde_json::Value>, Vec<String>) {
    use chrono_like::format_date;

    let mut items = Vec::with_capacity(icons.len());
    let mut cache_keys = Vec::with_capacity(icons.len());
    for (idx, e) in icons.iter().enumerate() {
        let hit = lookup.get(&e.name.to_lowercase());
        let (kind_raw, path_str) = match hit {
            None => ("SYS".to_string(), String::new()),
            Some((k, p)) => (k.code().to_string(), p.as_ref().map(|x| x.display().to_string()).unwrap_or_default()),
        };
        let cat = match kind_raw.as_str() {
            "SYS" => "system",
            "S" => "app",
            "D" => "folder",
            "DOC" => "doc",
            "MEDIA" => "media",
            "ARCH" => "archive",
            _ => "file",
        };
        let mtime = if path_str.is_empty() {
            "-".to_string()
        } else {
            let p = PathBuf::from(&path_str);
            if p.exists() {
                format_date(&p)
            } else {
                "-".to_string()
            }
        };
        let cache_key = icon_cache_key(&kind_raw, &path_str, &e.name);
        let icon_b64 = icon_cache.get(&cache_key).cloned().unwrap_or_default();
        cache_keys.push(cache_key);

        items.push(serde_json::json!({
            "id": idx + 1,
            "name": e.name,
            "cat": cat,
            "sys": cat == "system",
            "path": path_str,
            "modified": mtime,
            "icon": icon_b64,
        }));
    }
    (items, cache_keys)
}

/// 图标缓存 key。
///
/// ⚠️ **读写必须都调这个函数** —— 之前读用 `kind_raw`、写用 `cat`,
/// 两个值恰好不同 ("SYS" vs "system"), 导致缓存永远不命中。
///
/// `kind_raw` 用**大写原始分类码** (SYS / S / D / DOC / MEDIA / ARCH),
/// 不要用 `build_items` 输出里的小写 `cat`。
pub fn icon_cache_key(kind_raw: &str, path: &str, name: &str) -> String {
    format!("{kind_raw}|{path}|{name}")
}

/// 极简日期格式化 (不引入 chrono)。
mod chrono_like {
    use std::path::Path;

    /// 返回 `YYYY-MM-DD` 形式的修改日期。
    pub fn format_date(p: &Path) -> String {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Time::FileTimeToSystemTime;

        let Ok(meta) = std::fs::metadata(p) else {
            return "-".into();
        };
        let Ok(t) = meta.modified() else {
            return "-".into();
        };
        // SystemTime -> FILETIME (100ns since 1601-01-01)
        let epoch = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let ft = FILETIME {
            dwLowDateTime: (epoch.as_nanos() / 100) as u32,
            dwHighDateTime: (epoch.as_nanos() / 100 >> 32) as u32,
        };
        let mut st = Default::default();
        unsafe {
            if FileTimeToSystemTime(&ft, &mut st).is_err() {
                return "-".into();
            }
        }
        format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay)
    }
}
