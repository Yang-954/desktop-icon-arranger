//! 图标分类。
//!
//! 与 Python 版 `_classify_entry` / `build_lookup` 一一对应。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 图标类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// 系统图标 (此电脑 / 回收站 / 控制面板 等, 不在文件系统里)
    System,
    /// 软件应用
    Software,
    /// 文件夹
    Folder,
    /// 办公文档
    Doc,
    /// 图片影音
    Media,
    /// 压缩安装包
    Archive,
    /// 普通文件
    File,
}

impl Kind {
    /// Python 版内部的 kind 编码 (用于与旧版状态文件互读)。
    pub fn code(&self) -> &'static str {
        match self {
            Kind::System => "SYS",
            Kind::Software => "S",
            Kind::Folder => "D",
            Kind::Doc => "DOC",
            Kind::Media => "MEDIA",
            Kind::Archive => "ARCH",
            Kind::File => "F",
        }
    }

    /// 从 kind 编码解析。
    pub fn from_code(s: &str) -> Kind {
        match s {
            "SYS" => Kind::System,
            "S" => Kind::Software,
            "D" => Kind::Folder,
            "DOC" => Kind::Doc,
            "MEDIA" => Kind::Media,
            "ARCH" => Kind::Archive,
            _ => Kind::File,
        }
    }

    /// 前端使用的友好名称 (与 Python 版 KIND_MAP 对应)。
    pub fn frontend(&self) -> &'static str {
        match self {
            Kind::System => "system",
            Kind::Software => "app",
            Kind::Folder => "folder",
            Kind::Doc => "doc",
            Kind::Media => "media",
            Kind::Archive => "archive",
            Kind::File => "file",
        }
    }
}

/// 各类别的中文标签 (与 Python 版 `KIND_LABEL` 一致)。
pub const KIND_LABELS: [(&str, &str); 7] = [
    ("SYS", "系统图标"),
    ("S", "软件应用"),
    ("D", "文件夹"),
    ("DOC", "办公文档"),
    ("MEDIA", "图片影音"),
    ("ARCH", "压缩安装"),
    ("F", "普通文件"),
];

/// 软件类扩展名
const SOFTWARE_EXTS: [&str; 6] = [".lnk", ".exe", ".url", ".bat", ".cmd", ".appref-ms"];
/// 办公文档扩展名
const DOC_EXTS: [&str; 11] = [
    ".docx", ".doc", ".xlsx", ".xls", ".pptx", ".ppt", ".pdf", ".txt", ".md", ".csv", ".wps",
];
/// 图片影音扩展名
const MEDIA_EXTS: [&str; 11] = [
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".svg", ".mp4", ".mkv", ".mp3", ".wav",
];
/// 压缩安装包扩展名
const ARCHIVE_EXTS: [&str; 6] = [".zip", ".7z", ".rar", ".tar", ".gz", ".iso"];

/// 判断一个路径的类别。
///
/// 与 Python 版 `_classify_entry` 完全一致:
/// - 目录 -> 文件夹
/// - `.lnk` -> 软件 (快捷方式去掉 `.lnk` 后缀作为查找键)
/// - 其余按扩展名归类
pub fn classify_entry(p: &Path) -> (String, Kind) {
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    // Python 用 `Path.suffix` —— **带点号** (如 ".lnk");
    // Rust 的 `Path::extension()` 不带点号, 这里补上。
    let suf = p
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy().to_lowercase()))
        .unwrap_or_default();

    if p.is_dir() {
        return (name.to_lowercase(), Kind::Folder);
    }
    if suf == ".lnk" {
        // 快捷方式: 去掉 .lnk 后缀作为查找键 (与桌面上显示的名字一致)
        let stem = name.strip_suffix(".lnk").unwrap_or(&name);
        return (stem.to_lowercase(), Kind::Software);
    }
    if SOFTWARE_EXTS.contains(&suf.as_str()) {
        return (name.to_lowercase(), Kind::Software);
    }
    if DOC_EXTS.contains(&suf.as_str()) {
        return (name.to_lowercase(), Kind::Doc);
    }
    if MEDIA_EXTS.contains(&suf.as_str()) {
        return (name.to_lowercase(), Kind::Media);
    }
    if ARCHIVE_EXTS.contains(&suf.as_str()) {
        return (name.to_lowercase(), Kind::Archive);
    }
    (name.to_lowercase(), Kind::File)
}

/// 建立 "显示名(小写) -> (类别, 路径)" 映射。
///
/// 与 Python 版 `build_lookup` 一致:
/// - 按名称排序后遍历, 保证冲突时"先出现的"被保留
/// - 跳过 `.` 开头的条目与 `desktop.ini`
pub fn build_lookup(
    dirs: &[PathBuf],
    conflicts: &mut Vec<String>,
) -> HashMap<String, (Kind, Option<PathBuf>)> {
    let mut lookup: HashMap<String, (Kind, Option<PathBuf>)> = HashMap::new();

    for d in dirs {
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(d) {
            Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
            Err(e) => {
                conflicts.push(format!("无法读取目录 {}: {}", d.display(), e));
                continue;
            }
        };
        entries.sort_by_key(|p| {
            p.file_name()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        });

        for p in entries {
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with('.') || name.to_lowercase() == "desktop.ini" {
                continue;
            }
            let (key, kind) = classify_entry(&p);
            if let Some(existing) = lookup.get(&key) {
                conflicts.push(format!(
                    "显示名冲突: {name:?} 与 {} 都映射到 {key:?}, 已保留先出现的 {}",
                    existing.1.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                    existing.1.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
                ));
                continue;
            }
            lookup.insert(key, (kind, Some(p)));
        }
    }

    lookup
}
