//! 排序模式与槽位网格计算。
//!
//! 与 Python 版 (`desktop_icon_arranger.py`) 的 `natural_sort_key` / `plan` /
//! `build_slots` 逐行对应, 对照测试会逐项 diff。

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::classify::Kind;
use crate::win32::{IconEntry, get_work_area};

/// 排序模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    /// 系统图标置顶 -> 软件应用 -> 文件夹 -> 普通文件 (默认推荐)
    Standard,
    /// 纯名称混排 (系统图标置顶, 其余自然拼音混排)
    Name,
    /// 最新修改优先
    TimeDesc,
    /// 最早创建优先
    TimeAsc,
    /// 详细分类聚合: 系统 -> 软件 -> 文件夹 -> 文档 -> 媒体 -> 压缩包 -> 其它
    Type,
}

impl SortMode {
    /// 从字符串解析模式名。
    pub fn parse(s: &str) -> SortMode {
        match s {
            "name" => SortMode::Name,
            "time_desc" => SortMode::TimeDesc,
            "time_asc" => SortMode::TimeAsc,
            "type" => SortMode::Type,
            _ => SortMode::Standard,
        }
    }

    /// 模式标识符 (与 Python 版 CLI 一致)。
    pub fn as_str(&self) -> &'static str {
        match self {
            SortMode::Standard => "standard",
            SortMode::Name => "name",
            SortMode::TimeDesc => "time_desc",
            SortMode::TimeAsc => "time_asc",
            SortMode::Type => "type",
        }
    }
}

/// 排列流向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 列优先 (Windows 经典默认): 先从上往下排满一列
    Col,
    /// 行优先: 先从左往右排满一行
    Row,
}

impl Direction {
    pub fn parse(s: &str) -> Direction {
        match s {
            "row" => Direction::Row,
            _ => Direction::Col,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Col => "col",
            Direction::Row => "row",
        }
    }
}

// ---------------------------------------------------------------------------
// 自然拼音混排排序键
// ---------------------------------------------------------------------------

/// 排序键的一段
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPart {
    /// 文本段 (拼音音节或英文字符, 已转小写)
    Text(String),
    /// 数字段 (按数值比较, 使 `2` 排在 `10` 之前)
    Number(u128),
}

/// 自然拼音混排排序键。
///
/// 中文按拼音音节、英文按字符, 连续数字按整数数值比较。
/// 与 Python 版 `natural_sort_key` 完全一致:
///
/// ```python
/// parts = re.split(r"(\d+)", s)
/// for part in parts:
///     if part.isdigit(): key.append((1, int(part), ""))
///     else:
///         for syll in _lazy_pinyin(part): key.append((0, 0, syll.casefold()))
/// ```
///
/// 注意 Python 版的元组比较规则: `(1, num, "")` 与 `(0, 0, text)` 比较时,
/// 第一分量不同则先比第一分量 —— 即**数字段永远排���文本段之前**。
/// Rust 里用 enum 的自然序无法直接表达这一点, 故显式实现 `cmp_key`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey(pub Vec<KeyPart>);

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_key(other)
    }
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp_key(other))
    }
}

impl SortKey {
    /// 按 Python 元组比较语义比较两个键。
    ///
    /// Python 键是 `(type_rank, number, text)` 三元组:
    /// - 文本段: `(0, 0, syllable)`
    /// - 数字段: `(1, number, "")`
    ///
    /// 逐分量比较, 短的排前面 (Python list 比较规则)。
    pub fn cmp_key(&self, other: &SortKey) -> Ordering {
        use KeyPart::*;
        let a = &self.0;
        let b = &other.0;
        let n = a.len().min(b.len());
        for i in 0..n {
            let ord = match (&a[i], &b[i]) {
                (Text(x), Text(y)) => x.cmp(y),
                (Number(x), Number(y)) => x.cmp(y),
                // 数字段 (type_rank=1) 永远排在文本段 (type_rank=0) 之后
                (Number(_), Text(_)) => Ordering::Greater,
                (Text(_), Number(_)) => Ordering::Less,
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        a.len().cmp(&b.len())
    }
}

/// 生成自然拼音混排排序键。
///
/// 汉字通过 `pinyin` crate 转拼音音节, 与英文字母同台混排
/// (如 `千问(Q)` 排在 `PixPin(P)` 之后、`ToDesk(T)` 之前)。
pub fn natural_sort_key(s: &str) -> SortKey {
    let mut parts: Vec<KeyPart> = Vec::new();

    // Python: re.split(r"(\d+)", s) —— 数字块被单独切出来
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            if !current.is_empty() {
                push_text(&mut parts, &current);
                current.clear();
            }
            // 注意: 外层循环已经消费了 c (第一个数字字符), 内层从下一个字符继续收集
            let mut num = String::new();
            num.push(c);
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    num.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            // Python 的 int() 支持任意长度, Rust 用 u128 并在溢出时截断
            let v = num.parse::<u128>().unwrap_or(u128::MAX);
            parts.push(KeyPart::Number(v));
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        push_text(&mut parts, &current);
    }

    SortKey(parts)
}

/// 把一段文本转成拼音音节序列后推入键。
///
/// **必须与 Python `pypinyin.lazy_pinyin` 的元素粒度完全一致**:
/// - 连续的汉字 -> 各自的拼音音节 (每个汉字一个元素)
/// - 连续的**非汉字** -> **整段作为一个元素** (不是逐字符!)
///
/// 举例: `"dsh-startup"` -> `["dsh-startup"]` (一个元素),
/// `"个人 - Edge"` -> `["ge", "ren", " - edge"]` (三个元素)。
///
/// 粒度不同会导致排序结果不同 —— 例如 `"电脑配置.docx"` 的首元素是 `"dian"`,
/// 而 `"dsh-startup"` 的首元素是 `"dsh-startup"`, 比较 `"dsh-startup" < "dian"`
/// (逐字符则会是 `"d" < "dian"` 的另一种结果)。
fn push_text(parts: &mut Vec<KeyPart>, text: &str) {
    use pinyin::ToPinyin;

    // 先把文本切成"汉字段"和"非汉字段"
    let mut buf = String::new();
    let flush_non_han = |buf: &mut String, parts: &mut Vec<KeyPart>| {
        if !buf.is_empty() {
            parts.push(KeyPart::Text(buf.to_lowercase()));
            buf.clear();
        }
    };

    for ch in text.chars() {
        let is_han = is_han_char(ch);
        if is_han {
            flush_non_han(&mut buf, parts);
            let syll = ch.to_pinyin().map(|p| p.plain().to_lowercase());
            match syll {
                Some(s) => parts.push(KeyPart::Text(s)),
                // 不在拼音表里的罕见汉字: 保留原字符 (Python 也是这样)
                None => parts.push(KeyPart::Text(ch.to_string().to_lowercase())),
            }
        } else {
            buf.push(ch);
        }
    }
    flush_non_han(&mut buf, parts);
}

/// 判断是否为 CJK 汉字。
fn is_han_char(ch: char) -> bool {
    matches!(ch as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF |
        0x20000..=0x2A6DF | 0x2A700..=0x2EBEF | 0x2F800..=0x2FA1F)
}

/// 兼容旧接口的名称排序键。
pub fn name_key(name: &str) -> SortKey {
    natural_sort_key(name)
}

// ---------------------------------------------------------------------------
// 排序计划
// ---------------------------------------------------------------------------

/// 一个待排序项: 显示名 + 分类 + 路径
#[derive(Debug, Clone)]
pub struct SortItem {
    pub name: String,
    pub kind: Kind,
    pub path: Option<PathBuf>,
}

impl SortItem {
    /// 安全获取修改时间 (失败返回 `f64::INFINITY`, 与 Python 版 `_safe_mtime` 一致)。
    pub fn mtime(&self) -> f64 {
        match &self.path {
            Some(p) => std::fs::metadata(p)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(f64::INFINITY),
            None => f64::INFINITY,
        }
    }
}

/// 按指定模式生成排好序的图标显示名列表。
///
/// - `standard`  : 系统图标置顶 -> 软件应用 -> 文件夹 -> 普通文件
/// - `name`      : 纯名称混排 (系统图标置顶)
/// - `time_desc` : 按修改时间倒序 (最新排最前)
/// - `time_asc`  : 按修改时间正序 (最早排最前)
/// - `type`      : 详细分类聚合
pub fn plan(
    icons: &[IconEntry],
    lookup: &HashMap<String, (Kind, Option<PathBuf>)>,
    mode: SortMode,
    reverse: bool,
) -> Vec<String> {
    let mut sys_icons: Vec<(i32, String)> = Vec::new();
    let mut others: Vec<SortItem> = Vec::new();

    for e in icons {
        match lookup.get(&e.name.to_lowercase()) {
            None => sys_icons.push((e.index, e.name.clone())),
            Some((kind, path)) => others.push(SortItem {
                name: e.name.clone(),
                kind: *kind,
                path: path.clone(),
            }),
        }
    }

    // 系统图标始终保持相对物理顺序置顶
    sys_icons.sort_by_key(|(i, _)| *i);
    let sys_names: Vec<String> = sys_icons.into_iter().map(|(_, n)| n).collect();

    let sorted_others = match mode {
        SortMode::Name => {
            let mut v = others;
            sort_by_key(&mut v, reverse, |t| natural_sort_key(&t.name));
            v
        }
        SortMode::TimeDesc => {
            let mut v = others;
            // Python: key=(-mtime, natural_sort_key(name)), reverse=reverse
            sort_by_key(&mut v, reverse, |t| (NegMtime(t.mtime()), natural_sort_key(&t.name)));
            v
        }
        SortMode::TimeAsc => {
            let mut v = others;
            // Python: key=(mtime, natural_sort_key(name)), reverse=reverse
            sort_by_key(&mut v, reverse, |t| (OrderedMtime(t.mtime()), natural_sort_key(&t.name)));
            v
        }
        SortMode::Type => {
            let mut v = others;
            sort_by_key(&mut v, reverse, |t| (TypeRank::of(t.kind), natural_sort_key(&t.name)));
            v
        }
        SortMode::Standard => {
            // 软件 -> 文件夹 -> 文件 (DOC/MEDIA/ARCH/F 统一为文件组)
            let mut software: Vec<SortItem> = Vec::new();
            let mut dirs: Vec<SortItem> = Vec::new();
            let mut files: Vec<SortItem> = Vec::new();
            for t in others {
                match t.kind {
                    Kind::Software => software.push(t),
                    Kind::Folder => dirs.push(t),
                    _ => files.push(t),
                }
            }
            sort_by_key(&mut software, reverse, |t| natural_sort_key(&t.name));
            sort_by_key(&mut dirs, reverse, |t| natural_sort_key(&t.name));
            // 文件按修改时间倒序 (新文件排在前面), 同时间按名称
            // Python: key=(-_safe_mtime(t[2]), natural_sort_key(t[0])), reverse=reverse
            sort_by_key(&mut files, reverse, |t| {
                (NegMtime(t.mtime()), natural_sort_key(&t.name))
            });
            software.extend(dirs);
            software.extend(files);
            software
        }
    };

    let mut result = sys_names;
    result.extend(sorted_others.into_iter().map(|t| t.name));
    result
}

/// 排序辅助: 按 key 排序, `reverse` 时整体反转 (与 Python `sorted(reverse=True)` 一致)。
fn sort_by_key<T, K, F>(items: &mut [T], reverse: bool, key_fn: F)
where
    F: Fn(&T) -> K,
    K: Ord,
{
    items.sort_by(|a, b| key_fn(a).cmp(&key_fn(b)));
    if reverse {
        items.reverse();
    }
}

/// 修改时间的可比较包装 (`f64` 不能直接 `Ord`)。
///
/// Python 版在 `time_desc` 用 `key=(-mtime, ...)` 表示"最新优先",
/// 在 `time_asc` 用 `key=(mtime, ...)`。
///
/// **注意 Python 的 `sorted(reverse=True)` 会同时反转两个分量**, 所以
/// `time_desc + reverse` 实际变成"最旧优先"。
///
/// 这里用 `OrderedMtime` 保持同样的数值语义 (数值越小越靠前)。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct OrderedMtime(pub f64);

impl Eq for OrderedMtime {}

impl Ord for OrderedMtime {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.partial_cmp(&other.0).unwrap_or(Ordering::Equal)
    }
}

/// 修改时间的**取负**包装 (对应 Python 的 `-mtime`)。
///
/// 用于 `time_desc` 模式: 时间越大 -> 值越小 -> 排得越前。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NegMtime(pub f64);

impl Eq for NegMtime {}

impl Ord for NegMtime {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .partial_cmp(&self.0)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for NegMtime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 类型分组序号 (与 Python 版 `order_map` 一致)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TypeRank(pub u8);

impl TypeRank {
    pub fn of(kind: Kind) -> TypeRank {
        TypeRank(match kind {
            Kind::Software => 1,
            Kind::Folder => 2,
            Kind::Doc => 3,
            Kind::Media => 4,
            Kind::Archive => 5,
            Kind::File => 6,
            Kind::System => 99,
        })
    }
}
// ---------------------------------------------------------------------------
// 槽位网格
// ---------------------------------------------------------------------------

/// 把坐标值聚类成网格线 (容差 `tol`)。
pub fn cluster(vals: &[i32], tol: i32) -> Vec<i32> {
    let mut uniq: Vec<i32> = vals.to_vec();
    uniq.sort_unstable();
    uniq.dedup();
    let mut out: Vec<i32> = Vec::new();
    for v in uniq {
        if let Some(&last) = out.last() {
            if v - last <= tol {
                continue;
            }
        }
        out.push(v);
    }
    out
}

/// 把网格线拟合成规则网格, 返回 (网格线, 步长)。
///
/// Python 版用 `statistics.median`; Rust 的 `f64` 无内置 median, 这里自行实现
/// (偶数个取中间两数平均, 与 Python 一致)。
pub fn regular_grid(lines: &[i32], default_step: i32, min_step: i32) -> (Vec<i32>, i32) {
    if lines.len() < 2 {
        return (lines.to_vec(), default_step);
    }
    let gaps: Vec<i32> = lines
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|g| *g >= min_step)
        .collect();
    let step = if gaps.is_empty() {
        default_step
    } else {
        let mut g = gaps.clone();
        g.sort_unstable();
        let med = median_i32(&g);
        med.max(min_step)
    };
    let base = lines[0];
    let mut out: Vec<i32> = Vec::new();
    for &v in lines {
        // Python 的 round() 是"银行家舍入"; Rust 的 f64::round 是"四舍五入"。
        // 这里用 Python 语义实现, 保证网格线与旧版完全一致。
        let snapped = base + py_round((v - base) as f64 / step as f64) as i32 * step;
        if out.last().map_or(true, |&l| snapped > l) {
            out.push(snapped);
        }
    }
    (out, step)
}

/// Python 风格的 round (银行家舍入: .5 舍入到偶数)。
fn py_round(x: f64) -> f64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
        r - x.signum()
    } else {
        r
    }
}

/// 中位数 (偶数个取中间两数平均, 向下取整 —— 与 Python `statistics.median` 一致)。
fn median_i32(sorted: &[i32]) -> i32 {
    let n = sorted.len();
    if n == 0 {
        return 0;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        let a = sorted[n / 2 - 1] as i64;
        let b = sorted[n / 2] as i64;
        ((a + b) / 2) as i32
    }
}

/// 校验槽位合法性。
fn validate_slots(slots: &[(i32, i32)], work: &windows::Win32::Foundation::RECT, cell_w: i32, cell_h: i32) -> anyhow::Result<()> {
    use std::collections::HashSet;
    let set: HashSet<(i32, i32)> = slots.iter().copied().collect();
    if set.len() != slots.len() {
        anyhow::bail!("内部错误: 槽位存在重复坐标");
    }
    const COORD_MIN: i64 = -(1i64 << 31);
    const COORD_MAX: i64 = (1i64 << 31) - 1;
    for &(x, y) in slots {
        if !(COORD_MIN <= x as i64 && x as i64 <= COORD_MAX && COORD_MIN <= y as i64 && y as i64 <= COORD_MAX) {
            anyhow::bail!("槽位 ({x},{y}) 超出可表示坐标范围");
        }
        if x < work.left || y < work.top || x + cell_w > work.right || y + cell_h > work.bottom {
            anyhow::bail!(
                "槽位 ({x},{y}) 超出工作区 ({},{})-({},{})",
                work.left,
                work.top,
                work.right,
                work.bottom
            );
        }
    }
    Ok(())
}

/// 生成完整规整网格槽位。
///
/// - `col`: 列优先 (先自上而下填满一列, 再换到右边一列, Windows 默认)
/// - `row`: 行优先 (先自左向右填满一行, 再换到下方一行)
pub fn build_slots(
    icons: &[IconEntry],
    work: Option<windows::Win32::Foundation::RECT>,
    direction: Direction,
) -> anyhow::Result<Vec<(i32, i32)>> {
    let n = icons.len();
    if n == 0 {
        return Ok(vec![]);
    }
    let work = work.unwrap_or_else(get_work_area);

    let xs_raw: Vec<i32> = icons.iter().map(|i| i.x).collect();
    let ys_raw: Vec<i32> = icons.iter().map(|i| i.y).collect();

    let (mut xs, mut slot_w) = regular_grid(&cluster(&xs_raw, 8), crate::DEFAULT_CELL_W, 32);
    let (mut ys, mut slot_h) = regular_grid(&cluster(&ys_raw, 8), crate::DEFAULT_CELL_H, 32);

    xs[0] = xs[0].max(work.left);
    ys[0] = ys[0].max(work.top);

    let mut xs_fit: Vec<i32> = xs.iter().copied().filter(|x| x + slot_w <= work.right).collect();
    let mut ys_fit: Vec<i32> = ys.iter().copied().filter(|y| y + slot_h <= work.bottom).collect();
    if ys_fit.is_empty() || xs_fit.is_empty() {
        slot_w = slot_w.min((work.right - work.left).max(1));
        slot_h = slot_h.min((work.bottom - work.top).max(1));
        if xs_fit.is_empty() {
            xs_fit = vec![work.left];
        }
        if ys_fit.is_empty() {
            ys_fit = vec![work.top];
        }
    }
    let mut xs = xs_fit;
    let mut ys = ys_fit;

    let slots: Vec<(i32, i32)> = match direction {
        Direction::Row => {
            while xs.last().unwrap() + 2 * slot_w <= work.right {
                let nxt = xs.last().unwrap() + slot_w;
                xs.push(nxt);
            }
            while xs.len() * ys.len() < n {
                let ny = ys.last().unwrap() + slot_h;
                if ny + slot_h <= work.bottom {
                    ys.push(ny);
                } else {
                    anyhow::bail!(
                        "工作区放不下 {} 个图标 (当前 {} 列 x {} 行, 单元格 {}x{})",
                        n,
                        xs.len(),
                        ys.len(),
                        slot_w,
                        slot_h
                    );
                }
            }
            let mut v = Vec::with_capacity(n);
            'outer: for &y in &ys {
                for &x in &xs {
                    if v.len() >= n {
                        break 'outer;
                    }
                    v.push((x, y));
                }
            }
            v
        }
        Direction::Col => {
            while ys.last().unwrap() + 2 * slot_h <= work.bottom {
                let nxt = ys.last().unwrap() + slot_h;
                ys.push(nxt);
            }
            while xs.len() * ys.len() < n {
                let nx = xs.last().unwrap() + slot_w;
                if nx + slot_w <= work.right {
                    xs.push(nx);
                } else {
                    anyhow::bail!(
                        "工作区放不下 {} 个图标 (当前 {} 列 x {} 行, 单元格 {}x{})",
                        n,
                        xs.len(),
                        ys.len(),
                        slot_w,
                        slot_h
                    );
                }
            }
            let mut v = Vec::with_capacity(n);
            'outer: for &x in &xs {
                for &y in &ys {
                    if v.len() >= n {
                        break 'outer;
                    }
                    v.push((x, y));
                }
            }
            v
        }
    };

    validate_slots(&slots, &work, slot_w, slot_h)?;
    Ok(slots)
}

/// 计算桌面网格容量指标 (供前端显示"一列能排多少个")。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DesktopMetrics {
    pub screen_w: i32,
    pub screen_h: i32,
    pub slot_w: i32,
    pub slot_h: i32,
    pub max_rows: usize,
    pub max_cols: usize,
}

/// 分析当前屏幕工作区与网格容量指标。
pub fn desktop_metrics(icons: &[IconEntry]) -> DesktopMetrics {
    let work = get_work_area();
    let mut slot_w = crate::DEFAULT_CELL_W;
    let mut slot_h = crate::DEFAULT_CELL_H;
    let mut raw_x0 = work.left;
    let mut raw_y0 = work.top;

    if !icons.is_empty() {
        let xs: Vec<i32> = icons.iter().map(|i| i.x).collect();
        let ys: Vec<i32> = icons.iter().map(|i| i.y).collect();
        let (gx, w) = regular_grid(&cluster(&xs, 8), crate::DEFAULT_CELL_W, 32);
        let (gy, h) = regular_grid(&cluster(&ys, 8), crate::DEFAULT_CELL_H, 32);
        slot_w = w;
        slot_h = h;
        if let Some(&first_x) = gx.first() {
            raw_x0 = first_x;
        }
        if let Some(&first_y) = gy.first() {
            raw_y0 = first_y;
        }
    }

    let raw_x0 = raw_x0.max(work.left);
    let raw_y0 = raw_y0.max(work.top);

    let mut full_ys = vec![raw_y0];
    while full_ys.last().unwrap() + 2 * slot_h <= work.bottom {
        let nxt = full_ys.last().unwrap() + slot_h;
        full_ys.push(nxt);
    }
    let max_rows = full_ys.len();

    let mut full_xs = vec![raw_x0];
    while full_xs.last().unwrap() + 2 * slot_w <= work.right {
        let nxt = full_xs.last().unwrap() + slot_w;
        full_xs.push(nxt);
    }
    let max_cols = full_xs.len();

    DesktopMetrics {
        screen_w: work.right - work.left,
        screen_h: work.bottom - work.top,
        slot_w,
        slot_h,
        max_rows,
        max_cols,
    }
}
