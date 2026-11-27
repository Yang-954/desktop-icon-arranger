//! Tauri 命令实现 —— 与 Python 版 Backend 的 @Slot 方法一一对应。

use std::collections::HashMap;

use desktop_icon_arranger::{
    Direction, SortMode,
    sorting::{build_slots, desktop_metrics, plan},
    win32::{DesktopIcons, find_desktop_listview, is_auto_arrange},
};
use tauri::{AppHandle, Emitter, Manager, State};

use super::backend::{App, build_items, on_desktop_thread, scan_desktop, scan_desktop_in_thread};

/// 事件名 (对应 Python 版的 4 个信号)
pub const EV_ICON_SCANNED: &str = "icon-scanned";
pub const EV_ICON_THUMBNAILS: &str = "icon-thumbnails-loaded";
pub const EV_ICONS_LOAD_FINISHED: &str = "icons-load-finished";
pub const EV_ERROR: &str = "error-occurred";
pub const EV_SORT_APPLIED: &str = "sort-applied";
pub const EV_SNAPSHOT_RESTORED: &str = "snapshot-restored";

/// 扫描桌面图标, 返回 JSON 字符串 (与 Python 版 `scan_icons` 同结构)。
///
/// 两阶段策略 (与 Python 版一致):
/// 1. 立即返回桌面骨架 (名称/分类/路径/日期), 前端瞬间显示
/// 2. 后台线程提取真实图标, 通过 `icon-thumbnails-loaded` 事件增量注入
#[tauri::command]
pub fn scan_icons(app: AppHandle<tauri::Wry>, state: State<'_, App>) -> Result<String, String> {
    let (icons, lookup) = scan_desktop().map_err(|e| e.to_string())?;

    // 先算出哪些图标还没提取过 (走缓存)
    // `cache_keys` 由 build_items 一并产出 —— 读写用同一套 key, 不会分叉
    let (mut items, cache_keys) = {
        let s = state.state.lock().unwrap();
        build_items(&icons, &lookup, &s.icon_cache)
    };

    // 立即把骨架发给前端
    let payload = serde_json::to_string(&items).map_err(|e| e.to_string())?;
    let _ = app.emit(EV_ICON_SCANNED, payload.clone());

    // 第二阶段: 后台提取图标
    let uncached: Vec<(usize, String, String, String)> = {
        items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                let icon = it["icon"].as_str().unwrap_or("");
                if !icon.is_empty() {
                    return None;
                }
                let name = it["name"].as_str().unwrap_or("").to_string();
                let path = it["path"].as_str().unwrap_or("").to_string();
                let cache_key = cache_keys.get(i).cloned().unwrap_or_default();
                Some((i, name, path, cache_key))
            })
            .collect()
    };

    if uncached.is_empty() {
        let _ = app.emit(EV_ICONS_LOAD_FINISHED, ());
        // 缓存更新放在 return 之前, 保持顺序一致
        {
            let mut s = state.state.lock().unwrap();
            s.cached_icons = icons;
            s.cached_lookup = lookup;
            s.cached_items = items;
        }
        return Ok(payload);
    }

    // **先**更新缓存, 再 spawn 后台线程 ——
    // 否则后台线程写进 `cached_items` 的 icon 会被这里的骨架覆盖 (竞态)。
    {
        let mut s = state.state.lock().unwrap();
        s.cached_icons = icons;
        s.cached_lookup = lookup;
        s.cached_items = items;
    }

    let app2 = app.clone();
    std::thread::spawn(move || {
        let state = app2.state::<App>();
        let mut updates = serde_json::Map::new();
        for (idx, name, path, cache_key) in uncached {
            let b64 = extract_icon_b64(&name, &path);
            if b64.is_empty() {
                continue;
            }
            {
                let mut s = state.state.lock().unwrap();
                s.icon_cache.insert(cache_key, b64.clone());
                if let Some(it) = s.cached_items.get_mut(idx) {
                    it["icon"] = serde_json::Value::String(b64.clone());
                }
            }
            updates.insert((idx + 1).to_string(), serde_json::Value::String(b64));
        }
        if !updates.is_empty() {
            let _ = app2.emit(EV_ICON_THUMBNAILS, serde_json::Value::Object(updates));
        }
        let _ = app2.emit(EV_ICONS_LOAD_FINISHED, ());
    });

    Ok(payload)
}

/// 按类别提取图标 (data URL)。
///
/// 系统虚拟目录走 Shell PIDL 路径, 普通文件走路径路径 —— 与 Python 版一致。
fn extract_icon_b64(name: &str, path: &str) -> String {
    const SIZE: i32 = 48;
    if path.is_empty() {
        // 系统图标 (没有对应文件)
        return desktop_icon_arranger::icons::extract_system_icon(name, SIZE).unwrap_or_default();
    }
    desktop_icon_arranger::icons::extract_file_icon(path, SIZE).unwrap_or_default()
}

/// 返回桌面规格与行列容量信息 (JSON)。
#[tauri::command]
pub fn get_desktop_info(state: State<'_, App>) -> Result<String, String> {
    let s = state.state.lock().unwrap();
    let icons = if s.cached_icons.is_empty() {
        scan_desktop().map(|(i, _)| i).unwrap_or_default()
    } else {
        s.cached_icons.clone()
    };
    let m = desktop_metrics(&icons);
    serde_json::to_string(&m).map_err(|e| e.to_string())
}

/// 计算排序预览 (JSON), 不修改桌面。
#[tauri::command]
pub fn preview_sort(
    mode: String,
    direction: String,
    reverse: bool,
    state: State<'_, App>,
) -> Result<String, String> {
    let s = state.state.lock().unwrap();
    let icons = s.cached_icons.clone();
    let lookup = s.cached_lookup.clone();
    let items = s.cached_items.clone();

    if icons.is_empty() || items.is_empty() {
        return Ok(serde_json::json!({"items": Vec::<serde_json::Value>::new(), "desktop": serde_json::Value::Null}).to_string());
    }

    let sorted_names = plan(&icons, &lookup, SortMode::parse(&mode), reverse);
    let slots = build_slots(&icons, None, Direction::parse(&direction))
        .map_err(|e| e.to_string())?;

    // 绑定槽位信息 (与 Python 版 preview_sort 完全一致)
    let xs: Vec<i32> = slots.iter().map(|(x, _)| *x).collect();
    let ys: Vec<i32> = slots.iter().map(|(_, y)| *y).collect();
    let mut xs_sorted = xs.clone();
    xs_sorted.sort_unstable();
    xs_sorted.dedup();
    let mut ys_sorted = ys.clone();
    ys_sorted.sort_unstable();
    ys_sorted.dedup();

    let mut item_map: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        let name = it["name"].as_str().unwrap_or("").to_lowercase();
        item_map.entry(name).or_default().push(i);
    }

    let mut sorted_items: Vec<serde_json::Value> = Vec::new();
    let mut used: Vec<bool> = vec![false; items.len()];
    for (idx, nm) in sorted_names.iter().enumerate() {
        let bucket = item_map.get(&nm.to_lowercase()).cloned().unwrap_or_default();
        for i in bucket {
            if !used[i] {
                let mut it = items[i].clone();
                if idx < slots.len() {
                    let (sx, sy) = slots[idx];
                    let c_idx = xs_sorted.iter().position(|v| *v == sx).map_or(1, |p| p + 1);
                    let r_idx = ys_sorted.iter().position(|v| *v == sy).map_or(1, |p| p + 1);
                    let (slot_label, slot_col, slot_row) = if direction == "col" {
                        (format!("第{c_idx}列·行{r_idx}"), c_idx, r_idx)
                    } else {
                        (format!("第{r_idx}行·列{c_idx}"), c_idx, r_idx)
                    };
                    it["slot_label"] = serde_json::Value::String(slot_label);
                    it["slot_coord"] = serde_json::Value::String(format!("({sx},{sy})"));
                    it["slot_col"] = serde_json::Value::from(c_idx);
                    it["slot_row"] = serde_json::Value::from(r_idx);
                }
                sorted_items.push(it);
                used[i] = true;
                break;
            }
        }
    }
    for (i, it) in items.iter().enumerate() {
        if !used[i] {
            sorted_items.push(it.clone());
            used[i] = true;
        }
    }

    let desktop = desktop_metrics(&icons);
    let mut d = serde_json::to_value(&desktop).map_err(|e| e.to_string())?;
    if let Some(obj) = d.as_object_mut() {
        obj.insert("active_cols".into(), serde_json::Value::from(xs_sorted.len()));
        obj.insert("active_rows".into(), serde_json::Value::from(ys_sorted.len()));
        obj.insert("direction".into(), serde_json::Value::String(direction.clone()));
        obj.insert("icon_count".into(), serde_json::Value::from(sorted_items.len()));
    }

    serde_json::to_string(&serde_json::json!({
        "items": sorted_items,
        "desktop": d,
    }))
    .map_err(|e| e.to_string())
}

/// 真实重排桌面图标坐标。
#[tauri::command]
pub fn apply_sort(
    app: AppHandle<tauri::Wry>,
    state: State<'_, App>,
    mode: String,
    direction: String,
    reverse: bool,
) -> Result<bool, String> {
    let result = apply_sort_inner(&state, &mode, &direction, reverse);
    match result {
        Ok(ok) => {
            let _ = app.emit(EV_SORT_APPLIED, ok);
            Ok(ok)
        }
        Err(msg) => {
            let _ = app.emit(EV_ERROR, msg.clone());
            Ok(false)
        }
    }
}

fn apply_sort_inner(
    state: &State<'_, App>,
    mode: &str,
    direction: &str,
    reverse: bool,
) -> Result<bool, String> {
    let mode = mode.to_string();
    let direction = direction.to_string();

    // 全部 Explorer 操作都在同一个工作线程里完成 (切桌面 -> 干活 -> 线程退出)。
    // 绝不能在 Tauri 主线程上切桌面, 那会让主线程失去创建窗口的权限。
    let (names, slots, snapshot, mismatches, stuck) = on_desktop_thread(move || -> anyhow::Result<(
        Vec<String>,
        Vec<(i32, i32)>,
        Vec<(String, i32, i32)>,
        Vec<String>,
        Vec<(String, i32, i32)>,
    )> {
        let Some(lv) = find_desktop_listview() else {
            anyhow::bail!("未找到桌面 ListView");
        };
        if is_auto_arrange(lv) {
            anyhow::bail!("桌面开启了「自动排列图标」, 请先在桌面右键 → 查看 → 取消勾选。");
        }

        let (icons, lookup) = scan_desktop_in_thread()?;
        let names = plan(&icons, &lookup, SortMode::parse(&mode), reverse);
        let slots = build_slots(&icons, None, Direction::parse(&direction))?;

        // 应用前保存快照
        let snapshot: Vec<(String, i32, i32)> =
            icons.iter().map(|i| (i.name.clone(), i.x, i.y)).collect();

        // 真正落地
        let res = desktop_icon_arranger::cli::apply_positions(lv, &names, &slots)?;
        if !res.stuck.is_empty() {
            anyhow::bail!(
                "应用未完全成功: {} 个图标卡在中转区",
                res.stuck.len()
            );
        }

        // 预期位置序列与桌面实际坐标严格对比
        std::thread::sleep(std::time::Duration::from_millis(150));
        let mut di = DesktopIcons::new(lv)?;
        let actual_icons = di.read_all();
        drop(di);

        let mut actual_pos_map: HashMap<String, Vec<(i32, i32)>> = HashMap::new();
        for a in &actual_icons {
            actual_pos_map
                .entry(a.name.to_lowercase())
                .or_default()
                .push((a.x, a.y));
        }

        let mut mismatches: Vec<String> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for (nm, (exp_x, exp_y)) in names.iter().zip(slots.iter()) {
            let k = nm.to_lowercase();
            let seq = seen.entry(k.clone()).or_insert(0);
            let idx = *seq;
            *seq += 1;
            let bucket = actual_pos_map.get(&k).cloned().unwrap_or_default();
            if idx >= bucket.len() {
                mismatches.push(format!("{nm}(未在桌面找到)"));
                continue;
            }
            let (act_x, act_y) = bucket[idx];
            // 桌面「对齐到网格」会把图标吸附到最近的网格线, 按单元格步长容差
            if (act_x - exp_x).abs() > desktop_icon_arranger::DEFAULT_CELL_W / 2
                || (act_y - exp_y).abs() > desktop_icon_arranger::DEFAULT_CELL_H / 2
            {
                mismatches.push(format!(
                    "{nm}(预期: {exp_x},{exp_y} 实际: {act_x},{act_y})"
                ));
            }
        }

        Ok((names, slots, snapshot, mismatches, res.stuck))
    })
    .map_err(|e: anyhow::Error| e.to_string())?;

    // 保存快照 (文件操作不需要桌面上下文)
    state
        .store
        .save(&snapshot, &desktop_icon_arranger::state::now_string())
        .map_err(|e| e.to_string())?;

    if !mismatches.is_empty() {
        return Err(format!(
            "位置校验未通过: {} 个图标实际坐标与预期不符 (如: {})",
            mismatches.len(),
            mismatches[0]
        ));
    }
    let _ = stuck;

    // 成功后按新顺序重排缓存的 items
    {
        let mut s = state.state.lock().unwrap();
        let mut item_map: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, it) in s.cached_items.iter().enumerate() {
            let name = it["name"].as_str().unwrap_or("").to_lowercase();
            item_map.entry(name).or_default().push(i);
        }
        let mut sorted_items: Vec<serde_json::Value> = Vec::new();
        let mut used: Vec<bool> = vec![false; s.cached_items.len()];
        for nm in &names {
            let bucket = item_map.get(&nm.to_lowercase()).cloned().unwrap_or_default();
            for i in bucket {
                if !used[i] {
                    sorted_items.push(s.cached_items[i].clone());
                    used[i] = true;
                    break;
                }
            }
        }
        for (i, it) in s.cached_items.iter().enumerate() {
            if !used[i] {
                sorted_items.push(it.clone());
            }
        }
        s.cached_items = sorted_items;
    }

    Ok(true)
}

/// 还原到上一次应用前的快照。
#[tauri::command]
pub fn undo_snapshot(
    app: AppHandle<tauri::Wry>,
    state: State<'_, App>,
) -> Result<bool, String> {
    let store = state.store.clone();
    let res = on_desktop_thread(move || {
        let Some(lv) = find_desktop_listview() else {
            anyhow::bail!("未找到桌面 ListView");
        };
        desktop_icon_arranger::cli::do_undo(lv, &store, 0)
    });
    let ok = res.is_ok();
    if let Err(e) = res {
        let _ = app.emit(EV_ERROR, e.to_string());
    }
    let _ = app.emit(EV_SNAPSHOT_RESTORED, ok);
    Ok(ok)
}

/// 还原到指定深度的快照。
#[tauri::command]
pub fn restore_snapshot(
    app: AppHandle<tauri::Wry>,
    state: State<'_, App>,
    depth: usize,
) -> Result<bool, String> {
    let store = state.store.clone();
    let res = on_desktop_thread(move || {
        let Some(lv) = find_desktop_listview() else {
            anyhow::bail!("未找到桌面 ListView");
        };
        desktop_icon_arranger::cli::do_undo(lv, &store, depth)
    });
    let ok = res.is_ok();
    if let Err(e) = res {
        let _ = app.emit(EV_ERROR, e.to_string());
    }
    let _ = app.emit(EV_SNAPSHOT_RESTORED, ok);
    Ok(ok)
}

/// 按自定义顺序应用排序 (order_json 是显示名数组的 JSON)。
#[tauri::command]
pub fn apply_custom_order(
    app: AppHandle<tauri::Wry>,
    state: State<'_, App>,
    order_json: String,
    direction: String,
) -> Result<bool, String> {
    let order: Vec<String> = serde_json::from_str(&order_json).map_err(|e| e.to_string())?;

    let dir = direction.clone();
    let (snapshot, stuck) = on_desktop_thread(move || {
        let Some(lv) = find_desktop_listview() else {
            anyhow::bail!("未找到桌面 ListView");
        };
        if is_auto_arrange(lv) {
            anyhow::bail!("桌面开启了「自动排列图标」, 请先在桌面右键 → 查看 → 取消勾选。");
        }

        let (icons, _) = scan_desktop_in_thread()?;
        let slots = build_slots(&icons, None, Direction::parse(&dir)).map_err(|e| anyhow::anyhow!(e))?;
        if order.len() != slots.len() {
            anyhow::bail!(
                "自定义顺序数量 {} 与槽位数 {} 不一致",
                order.len(),
                slots.len()
            );
        }

        let snapshot: Vec<(String, i32, i32)> =
            icons.iter().map(|i| (i.name.clone(), i.x, i.y)).collect();

        let res = desktop_icon_arranger::cli::apply_positions(lv, &order, &slots).map_err(|e| anyhow::anyhow!(e))?;
        Ok((snapshot, res.stuck))
    })
    .map_err(|e: anyhow::Error| e.to_string())?;

    state
        .store
        .save(&snapshot, &desktop_icon_arranger::state::now_string())
        .map_err(|e| e.to_string())?;

    if !stuck.is_empty() {
        let msg = format!("应用未完全成功: {} 个图标卡在中转区", stuck.len());
        let _ = app.emit(EV_ERROR, msg.clone());
        return Ok(false);
    }
    let _ = app.emit(EV_SORT_APPLIED, true);
    Ok(true)
}

/// 预览某个历史快照 (JSON)。
#[tauri::command]
pub fn preview_snapshot(state: State<'_, App>, depth: usize) -> Result<String, String> {
    let Some(data) = state.store.load() else {
        return Ok(serde_json::json!({"items": Vec::<serde_json::Value>::new(), "desktop": serde_json::Value::Null}).to_string());
    };
    let (records, saved_at) = data.pick(depth).map_err(|e| e.to_string())?;

    let icons = {
        let s = state.state.lock().unwrap();
        s.cached_icons.clone()
    };
    let slots: Vec<(i32, i32)> = records.iter().map(|(_, x, y)| (*x, *y)).collect();
    let xs: Vec<i32> = slots.iter().map(|(x, _)| *x).collect();
    let ys: Vec<i32> = slots.iter().map(|(_, y)| *y).collect();

    let mut items: Vec<serde_json::Value> = Vec::new();
    for ((name, x, y), _) in records.iter().zip(slots.iter()) {
        items.push(serde_json::json!({
            "id": items.len() + 1,
            "name": name,
            "cat": "file",
            "sys": false,
            "path": "",
            "modified": "-",
            "icon": "",
            "slot_label": format!("({x},{y})"),
            "slot_coord": format!("({x},{y})"),
            "slot_col": xs.iter().position(|v| v == x).map_or(1, |p| p + 1),
            "slot_row": ys.iter().position(|v| v == y).map_or(1, |p| p + 1),
        }));
    }

    let desktop = desktop_metrics(&icons);
    serde_json::to_string(&serde_json::json!({
        "items": items,
        "desktop": desktop,
        "saved_at": saved_at,
        "depth": depth,
    }))
    .map_err(|e| e.to_string())
}

/// 清空历史快照。
#[tauri::command]
pub fn clear_history(state: State<'_, App>) -> Result<bool, String> {
    state.store.clear_history().map_err(|e| e.to_string())?;
    Ok(true)
}

/// 返回历史快照列表 (JSON)。
#[tauri::command]
pub fn get_snapshot_list(state: State<'_, App>) -> Result<String, String> {
    let layers = state.store.list();
    let items: Vec<serde_json::Value> = layers
        .iter()
        .map(|(depth, saved_at, count)| {
            serde_json::json!({
                "depth": depth,
                "saved_at": saved_at.clone().unwrap_or_default(),
                "count": count,
            })
        })
        .collect();
    serde_json::to_string(&serde_json::json!({ "snapshots": items })).map_err(|e| e.to_string())
}

/// 返回扫描来源目录 (JSON)。
#[tauri::command]
pub fn get_scan_sources() -> Result<String, String> {
    let dirs = desktop_icon_arranger::win32::desktop_dirs();
    serde_json::to_string(&serde_json::json!({
        "auto": dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>(),
        "custom": Vec::<String>::new(),
    }))
    .map_err(|e| e.to_string())
}

/// 保存一项设置。
#[tauri::command]
pub fn save_setting(state: State<'_, App>, key: String, value: String) -> Result<bool, String> {
    let mut s = state.settings.lock().unwrap();
    s.insert(key, value);
    Ok(true)
}

/// 读取全部设置 (JSON)。
#[tauri::command]
pub fn get_all_settings(state: State<'_, App>) -> Result<String, String> {
    let s = state.settings.lock().unwrap();
    let theme = s.get("theme").cloned().unwrap_or_else(|| "light".into());
    let auto_apply = s.get("auto_apply").cloned().unwrap_or_else(|| "0".into());
    serde_json::to_string(&serde_json::json!({"theme": theme, "auto_apply": auto_apply}))
        .map_err(|e| e.to_string())
}

/// 应用版本号。
#[tauri::command]
pub fn get_app_version() -> String {
    desktop_icon_arranger::APP_VERSION.to_string()
}

/// 健康检查。
#[tauri::command]
pub fn ping() -> String {
    "pong".to_string()
}
