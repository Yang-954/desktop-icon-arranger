//! 图标提取的冒烟测试: 验证能从真实系统里提取到**互不相同**的 PNG。

use desktop_icon_arranger::icons::{extract_file_icon, extract_system_icon};
use std::collections::HashSet;

/// PNG data URL 的 base64 主体。
fn body(s: &str) -> &str {
    s.trim_start_matches("data:image/png;base64,")
}

#[test]
fn 文件图标能提取出png() {
    let path = r"C:\Windows\System32\notepad.exe";
    if !std::path::Path::new(path).exists() {
        eprintln!("跳过 (文件不存在): {path}");
        return;
    }
    let out = extract_file_icon(path, 48).expect("文件图标提取失败");
    assert!(body(&out).starts_with("iVBORw0KGgo"), "不是合法 PNG");
    println!("notepad.exe 图标 base64 长度: {}", body(&out).len());
}

#[test]
fn 不同文件产出不同图标() {
    let mut seen = HashSet::new();
    let mut checked = 0;
    for p in [
        r"C:\Windows\System32\notepad.exe",
        r"C:\Windows\explorer.exe",
        r"C:\Windows\System32\cmd.exe",
    ] {
        if !std::path::Path::new(p).exists() {
            continue;
        }
        if let Some(d) = extract_file_icon(p, 48) {
            seen.insert(body(&d).to_string());
            checked += 1;
        }
    }
    if checked < 2 {
        eprintln!("跳过 (可用文件不足)");
        return;
    }
    assert!(
        seen.len() == checked,
        "不同文件产出了相同图标: {}/{} 唯一",
        seen.len(),
        checked
    );
}

#[test]
fn 不存在的文件返回none() {
    assert!(extract_file_icon(r"C:\不存在的文件\xxx.exe", 48).is_none());
}

/// **必须是完整的 data URL** —— 不能只返回裸 base64。
///
/// 前端拿到返回值后直接塞进 `<img src="...">`。
/// 裸 base64 会被浏览器当成**相对路径**去请求 -> 404 -> 界面显示成空方框,
/// 看着像"图标提取失败", 但后端其实提取成功了。
///
/// (这个坑真实发生过: `encode_png` 一度只返回裸 base64,
///  而 `body()` 里的 `trim_start_matches` 把缺前缀的情况掩盖了, 测试假阳性。)
#[test]
fn 提取结果带data_url前缀() {
    let mut checked = 0;

    for p in [
        r"C:\Windows\System32\notepad.exe",
        r"C:\Windows\explorer.exe",
    ] {
        if !std::path::Path::new(p).exists() {
            continue;
        }
        let out = extract_file_icon(p, 48).expect("文件图标提取失败");
        assert!(
            out.starts_with("data:image/png;base64,"),
            "文件图标缺少 data URL 前缀: {out:.40}..."
        );
        checked += 1;
    }

    for name in ["此电脑", "回收站", "控制面板"] {
        if let Some(out) = extract_system_icon(name, 48) {
            assert!(
                out.starts_with("data:image/png;base64,"),
                "系统图标 {name} 缺少 data URL 前缀: {out:.40}..."
            );
            checked += 1;
        }
    }

    assert!(checked > 0, "没有任何图标可供检查");
}

#[test]
fn 系统图标能提取出png() {
    // 至少要有一个系统图标能提出来
    let mut got = 0;
    for name in ["此电脑", "回收站", "控制面板", "网络", "用户文件"] {
        if let Some(d) = extract_system_icon(name, 48) {
            assert!(body(&d).starts_with("iVBORw0KGgo"), "{name} 不是合法 PNG");
            println!("{name} 图标 base64 长度: {}", body(&d).len());
            got += 1;
        }
    }
    assert!(got > 0, "一个系统图标都没提取出来");
}
