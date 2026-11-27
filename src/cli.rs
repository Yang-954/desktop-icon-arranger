//! 命令行模式: 参数解析 + 排序应用 + 撤销。
//!
//! 独立的 `desktop-icon-arranger-cli.exe` 使用本模块 (console 子系统, 有 stdout)。
//! GUI 版 (`desktop-icon-arranger.exe`) 是 `windows_subsystem = "windows"`,
//! 双击时完全不弹控制台 —— 所以两者必须分成两个二进制。

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::classify::{Kind, build_lookup};
use crate::sorting::{Direction, SortMode, build_slots, plan};
use crate::state::StateStore;
use crate::win32::{DesktopIcons, find_desktop_listview, get_work_area, is_auto_arrange};

pub const AUTO_ARRANGE_HINT: &str =
    "桌面开启了 [自动排列图标], 图标位置会被系统锁定, 无法自由排列。\n\
     请在桌面空白处右键 -> 查看 -> 取消勾选 [自动排列图标] 后重试。";

/// 两阶段落位的结果
#[derive(Debug, Default)]
pub struct ApplyResult {
    pub placed: usize,
    pub missing: Vec<String>,
    pub stuck: Vec<(String, i32, i32)>,
}

/// 计算与目标区域完全不重叠的中转区域坐标。
///
/// 中转区的作用: Explorer 在槽位冲突时会自动挤压避让原图标,
/// 所以先把全体图标撤离到目标区之外, 再精准落位目标槽位。
pub fn temp_slots(
    slots: &[(i32, i32)],
    n: usize,
    work: &windows::Win32::Foundation::RECT,
) -> anyhow::Result<Vec<(i32, i32)>> {
    if n == 0 {
        return Ok(vec![]);
    }
    let cell_w = crate::DEFAULT_CELL_W;
    let cell_h = crate::DEFAULT_CELL_H;
    let (t_left, t_top, t_right, t_bottom) = rect_of(slots, cell_w, cell_h);

    let generate = |origin_x: i32, origin_y: i32| -> Option<Vec<(i32, i32)>> {
        let cols = (work.right - origin_x) / cell_w;
        let rows = (work.bottom - origin_y) / cell_h;
        if cols < 1 || rows < 1 || (cols as usize * rows as usize) < n {
            return None;
        }
        let mut out = Vec::with_capacity(n);
        for c in 0..cols {
            for r in 0..rows {
                out.push((origin_x + c * cell_w, origin_y + r * cell_h));
                if out.len() == n {
                    return Some(out);
                }
            }
        }
        Some(out)
    };

    let candidates = [
        (t_right + cell_w, work.top.max(t_top)),
        (work.left.max(t_left), t_bottom + cell_h),
    ];
    for (origin_x, origin_y) in candidates {
        let Some(tmp) = generate(origin_x, origin_y) else {
            continue;
        };
        let r = rect_of(&tmp, cell_w, cell_h);
        if overlaps(r, (t_left, t_top, t_right, t_bottom)) {
            continue;
        }
        return Ok(tmp);
    }

    anyhow::bail!("找不到与目标区不重叠的中转区域 (需要 {n} 个单元格), 无法执行两阶段重排")
}

/// 计算槽位集合的外接矩形。
fn rect_of(slots: &[(i32, i32)], cell_w: i32, cell_h: i32) -> (i32, i32, i32, i32) {
    let xs: Vec<i32> = slots.iter().map(|s| s.0).collect();
    let ys: Vec<i32> = slots.iter().map(|s| s.1).collect();
    let min_x = xs.iter().min().copied().unwrap_or(0);
    let max_x = xs.iter().max().copied().unwrap_or(0);
    let min_y = ys.iter().min().copied().unwrap_or(0);
    let max_y = ys.iter().max().copied().unwrap_or(0);
    (min_x, min_y, max_x + cell_w, max_y + cell_h)
}

/// 两个矩形是否重叠。
fn overlaps(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

/// 两阶段重排并落地到桌面。
pub fn apply_positions(
    lv: isize,
    names: &[String],
    slots: &[(i32, i32)],
) -> anyhow::Result<ApplyResult> {
    let n = names.len();
    if n != slots.len() {
        anyhow::bail!("图标数 {n} 与槽位数 {} 不一致", slots.len());
    }
    if n == 0 {
        return Ok(ApplyResult::default());
    }
    if is_auto_arrange(lv) {
        anyhow::bail!("桌面开启了 [自动排列图标], 位置设置会被忽略, 已中止");
    }

    let work = get_work_area();
    let tmp = temp_slots(slots, n, &work)?;

    let mut missing: Vec<String> = Vec::new();
    let mut stuck: Vec<(String, i32, i32)> = Vec::new();

    let mut di = DesktopIcons::new(lv)?;
    di.read_all();
    di.begin_stage();

    let mut stage1_ops: Vec<(i32, i32, i32)> = Vec::new();
    let mut stage2_ops: Vec<(i32, i32, i32, String)> = Vec::new();
    for ((name, (tx, ty)), (sx, sy)) in names.iter().zip(tmp.iter()).zip(slots.iter()) {
        let Some(idx) = di.index_of(name) else {
            missing.push(name.clone());
            continue;
        };
        stage1_ops.push((idx, *tx, *ty));
        stage2_ops.push((idx, *sx, *sy, name.clone()));
    }

    // 阶段 1: 全体撤离到中转区 (避开目标区, 防止碰撞挤位)
    let mut staged: HashMap<i32, (i32, i32)> = HashMap::new();
    for (idx, tx, ty) in &stage1_ops {
        staged.insert(*idx, (*tx, *ty));
        di.set_position(*idx, *tx, *ty);
    }
    di.redraw(n as i32);
    std::thread::sleep(Duration::from_millis(120));

    // 阶段 2: 落到目标槽位
    let mut placed = 0usize;
    for (idx, sx, sy, name) in &stage2_ops {
        if !di.set_position(*idx, *sx, *sy) {
            let (tx, ty) = staged.get(idx).copied().unwrap_or((0, 0));
            stuck.push((name.clone(), tx, ty));
        } else {
            placed += 1;
        }
    }
    di.redraw(n as i32);

    missing.sort();
    missing.dedup();
    Ok(ApplyResult {
        placed,
        missing,
        stuck,
    })
}

/// 还原到 depth 层之前的布局。
pub fn do_undo(lv: isize, store: &StateStore, depth: usize) -> anyhow::Result<()> {
    if is_auto_arrange(lv) {
        anyhow::bail!("{AUTO_ARRANGE_HINT}");
    }
    let Some(data) = store.load() else {
        anyhow::bail!("找不到位置记录: {}", store.path().display());
    };
    let (records, saved_at) = data.pick(depth)?;

    let slots: Vec<(i32, i32)> = records.iter().map(|(_, x, y)| (*x, *y)).collect();
    let work = get_work_area();
    let tmp = temp_slots(&slots, records.len(), &work)?;

    let mut missing: Vec<String> = Vec::new();
    let mut di = DesktopIcons::new(lv)?;
    di.read_all();
    di.begin_stage();

    let mut stage1_ops = Vec::new();
    let mut stage2_ops = Vec::new();
    for ((name, x, y), (tx, ty)) in records.iter().zip(tmp.iter()) {
        let Some(idx) = di.index_of(name) else {
            missing.push(name.clone());
            continue;
        };
        stage1_ops.push((idx, *tx, *ty));
        stage2_ops.push((idx, *x, *y));
    }

    for (idx, tx, ty) in &stage1_ops {
        di.set_position(*idx, *tx, *ty);
    }
    di.redraw(records.len() as i32);
    std::thread::sleep(Duration::from_millis(120));

    let mut placed = 0usize;
    for (idx, x, y) in &stage2_ops {
        if di.set_position(*idx, *x, *y) {
            placed += 1;
        }
    }
    di.redraw(records.len() as i32);

    println!("已还原 {placed} 个图标位置 (快照时间 {saved_at}, depth={depth})");
    if !missing.is_empty() {
        missing.sort();
        missing.dedup();
        println!("桌面上已不存在 {} 个图标, 跳过: {:?}", missing.len(), missing);
    }
    Ok(())
}

/// CLI 参数
#[derive(Debug, Clone)]
pub struct Cli {
    pub apply: bool,
    pub mode: String,
    pub direction: String,
    pub reverse: bool,
    pub state: PathBuf,
    pub undo: bool,
    pub undo_depth: usize,
    pub list_history: bool,
    pub clear_history: bool,
    /// 输出 JSON (供对照测试)
    pub json: bool,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            apply: false,
            mode: "standard".into(),
            direction: "col".into(),
            reverse: false,
            state: default_state_path(),
            undo: false,
            undo_depth: 0,
            list_history: false,
            clear_history: false,
            json: false,
        }
    }
}

/// 默认状态文件路径 (撤销历史)。
///
/// **exe 放到任何位置都能用** —— 状态文件不跟着 exe 走, 也不跟着"当前工作目录"走。
///
/// 按优先级挑一个**当前进程确实可写**的目录:
/// 1. `%APPDATA%\<APP_IDENTIFIER>\`   —— 用户漫游配置 (首选, 跟随用户)
/// 2. `%LOCALAPPDATA%\<APP_IDENTIFIER>\` —— 本地配置 (漫游被禁时)
/// 3. `%TEMP%\<APP_IDENTIFIER>\`      —— 临时目录 (前两个都不可写时)
/// 4. exe 同目录                         —— 最后的兜底
///
/// 另外会自动**迁移**旧版本留下的位置记录 (一次性), 保证历史不丢:
/// - exe 同目录的 `.icon_arranger_state.json` (Python 版时代的习惯)
/// - `%APPDATA%\desktop-icon-arranger\` (v3.0 早期的目录名)
///
/// ⚠️ 旧实现是"exe 旁边有就用, 否则用当前工作目录的相对路径", 有三个问题:
/// - exe 放在 `C:\Program Files\` 等不可写目录 → 回退到 cwd → 双击时 cwd
///   就是那个不可写目录 → **保存快照直接失败**
/// - 从不同 cwd 运行同一个 exe → 状态文件落在不同地方 → **历史分裂**
/// - 状态跟着"当前目录"跑, 行为反直觉
///
/// CLI 的 `--state` 参数仍可显式覆盖。
pub fn default_state_path() -> PathBuf {
    const FILE_NAME: &str = ".icon_arranger_state.json";

    // 1) 挑一个可写的候选目录 (目录名统一用 APP_IDENTIFIER, 与 WebView2 缓存目录同风格)
    let mut candidates: Vec<PathBuf> = Vec::new();
    for var in ["APPDATA", "LOCALAPPDATA", "TEMP"] {
        if let Some(d) = std::env::var_os(var) {
            candidates.push(PathBuf::from(d).join(crate::APP_IDENTIFIER));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.to_path_buf());
        }
    }

    let mut target = PathBuf::from(FILE_NAME);
    let mut chosen = false;
    for dir in &candidates {
        let file = dir.join(FILE_NAME);
        // 已存在 -> 直接用 (无需探测可写性)
        if file.exists() {
            target = file;
            chosen = true;
            break;
        }
        // 不存在 -> 试着建目录并写一个空文件, 确认真的可写
        // (create_dir_all 成功不代表文件可写 —— 目录 ACL 可能只给了 RX)
        if std::fs::create_dir_all(dir).is_ok() && std::fs::write(&file, b"").is_ok() {
            target = file;
            chosen = true;
            break;
        }
    }
    if !chosen {
        // 所有用户目录都不可写 (极少见: 受限的企业环境) -> 退到 exe 同目录
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                target = dir.join(FILE_NAME);
            }
        }
        eprintln!(
            "[state] 用户目录均不可写, 位置记录将存到: {}",
            target.display()
        );
    }

    // 2) 一次性迁移: 旧位置 -> 目标
    //
    //    迁移源可能有两处:
    //    - exe 同目录 (Python 版时代的习惯)
    //    - `%APPDATA%\desktop-icon-arranger\` (v3.0 早期的目录名)
    //
    //    注意: 上面选目标时可能刚创建了一个**空文件**, 所以判断条件是
    //    "目标为空 或 目标不存在", 不能只看 exists()。
    let mut legacy: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            legacy.push(dir.join(FILE_NAME));
        }
    }
    for var in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(d) = std::env::var_os(var) {
            legacy.push(PathBuf::from(d).join("desktop-icon-arranger").join(FILE_NAME));
        }
    }

    for old in legacy {
        if old == target || !old.exists() {
            continue;
        }
        // 源文件有内容才值得迁移
        let Ok(meta) = std::fs::metadata(&old) else { continue };
        if meta.len() == 0 {
            continue;
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::copy(&old, &target) {
            Ok(_) => eprintln!("[state] 已把旧位置记录迁移到: {}", target.display()),
            Err(e) => eprintln!("[state] 迁移旧位置记录失败: {e}"),
        }
        break;
    }

    target
}

/// 解析命令行参数。
pub fn parse_cli(args: &[String]) -> Cli {
    let mut cli = Cli::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--apply" => cli.apply = true,
            "--undo" => cli.undo = true,
            "--reverse" => cli.reverse = true,
            "--list-history" => cli.list_history = true,
            "--clear-history" => cli.clear_history = true,
            "--json" => cli.json = true,
            "--mode" => {
                i += 1;
                if i < args.len() {
                    cli.mode = args[i].clone();
                }
            }
            "--direction" => {
                i += 1;
                if i < args.len() {
                    cli.direction = args[i].clone();
                }
            }
            "--state" => {
                i += 1;
                if i < args.len() {
                    cli.state = PathBuf::from(&args[i]);
                }
            }
            "--depth" => {
                i += 1;
                if i < args.len() {
                    cli.undo_depth = args[i].parse().unwrap_or(0);
                }
            }
            _ => {}
        }
        i += 1;
    }
    cli
}

/// CLI 主流程。
pub fn run(cli: Cli) -> anyhow::Result<()> {
    let store = StateStore::new(cli.state.clone());

    if cli.list_history {
        for (depth, saved_at, count) in store.list() {
            println!("depth={depth:2}  {count:3} 个图标  {}", saved_at.unwrap_or_default());
        }
        return Ok(());
    }

    if cli.clear_history {
        store.clear_history()?;
        println!("已清空历史快照");
        return Ok(());
    }

    // 必须附加到交互桌面
    crate::win32::ensure_interactive_desktop();

    let Some(lv) = find_desktop_listview() else {
        anyhow::bail!("未找到桌面 ListView (SHELLDLL_DefView / SysListView32)");
    };

    if cli.undo {
        return do_undo(lv, &store, cli.undo_depth);
    }

    let mut di = DesktopIcons::new(lv)?;
    let icons = di.read_all();
    drop(di);

    let dirs = crate::win32::desktop_dirs();
    let mut conflicts = Vec::new();
    let lookup = build_lookup(&dirs, &mut conflicts);

    let mode = SortMode::parse(&cli.mode);
    let direction = Direction::parse(&cli.direction);
    let names = plan(&icons, &lookup, mode, cli.reverse);
    let slots = build_slots(&icons, None, direction)?;

    // JSON 输出模式: 与 Python 版 `desktop_icon_arranger.py --json` 格式完全一致,
    // 供 tools/compare_impl.py 做双实现逐项对照。
    if cli.json {
        let payload = serde_json::json!({
            "mode": mode.as_str(),
            "direction": direction.as_str(),
            "reverse": cli.reverse,
            "items": names.iter().zip(slots.iter())
                .map(|(n, (x, y))| serde_json::json!({"name": n, "x": x, "y": y}))
                .collect::<Vec<_>>(),
            "conflicts": conflicts,
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    // 打印方案
    println!("{:<4}  {:>13}  {:<28} {:<8} {:>10}", "新序", "槽位", "原名", "类别", "原位置");
    let orig: HashMap<&str, (i32, i32)> = icons.iter().map(|i| (i.name.as_str(), (i.x, i.y))).collect();
    let kind_label = |n: &str| -> &str {
        lookup
            .get(&n.to_lowercase())
            .map(|(k, _)| match k {
                Kind::System => "系统图标",
                Kind::Software => "软件应用",
                Kind::Folder => "文件夹",
                Kind::Doc => "办公文档",
                Kind::Media => "图片影音",
                Kind::Archive => "压缩安装",
                Kind::File => "普通文件",
            })
            .unwrap_or("系统图标")
    };
    for (i, (name, (x, y))) in names.iter().zip(slots.iter()).enumerate() {
        let (ox, oy) = orig.get(name.as_str()).copied().unwrap_or((-1, -1));
        println!(
            "{:<4}  ({:>4},{:>4})  {:<28} {:<8} ({:>4},{:>4})",
            i + 1,
            x,
            y,
            name,
            kind_label(name),
            ox,
            oy
        );
    }

    if !cli.apply {
        println!("\n(仅预览, 未修改桌面; 加 --apply 执行真实排序)");
        return Ok(());
    }

    // 应用前保存快照
    let snapshot: Vec<(String, i32, i32)> = icons.iter().map(|i| (i.name.clone(), i.x, i.y)).collect();
    let hist = store.save(&snapshot, &crate::state::now_string())?;
    println!("\n已保存快照 (历史 {hist} 层)");

    let res = apply_positions(lv, &names, &slots)?;
    println!("已重排 {} 个图标", res.placed);
    if !res.missing.is_empty() {
        println!("警告: {} 个图标未找到: {:?}", res.missing.len(), res.missing);
    }
    if !res.stuck.is_empty() {
        println!("警告: {} 个图标遗留在中转区:", res.stuck.len());
        for (name, x, y) in &res.stuck {
            println!("        {name:?} 当前约在 ({x},{y})");
        }
    }
    Ok(())
}
