//! 运行时路径策略的回归测试。
//!
//! 这些路径决定了 **exe 能不能放到任何地方都能用**, 一旦改错会静默出问题
//! (比如写到不可写目录 → 保存快照失败; 或跟着 cwd 跑 → 历史分裂)。

use desktop_icon_arranger::cli::default_state_path;
use desktop_icon_arranger::APP_IDENTIFIER;
use std::path::PathBuf;

/// `tauri.conf.json` 的 `identifier` 必须与 `APP_IDENTIFIER` 一致。
///
/// Tauri 用它决定 WebView2 用户数据目录; 本项目的运行时数据目录也用它。
/// **两处各写一份必然分叉** —— 本项目已经吃过一次亏 (图标缓存的 key 两处各算一半,
/// 结果缓存永远不命中、刷新丢图标)。这个断言就是防重蹈覆辙。
#[test]
fn identifier与配置一致() {
    let conf = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"),
    )
    .expect("读不到 tauri.conf.json");

    // 不引 serde_json 依赖, 直接做一次粗解析 (配置是我们自己写的, 格式稳定)
    let found = conf
        .lines()
        .find(|l| l.contains("\"identifier\""))
        .and_then(|l| l.split('"').nth(3))
        .map(|s| s.to_string());

    assert_eq!(
        found.as_deref(),
        Some(APP_IDENTIFIER),
        "tauri.conf.json 的 identifier 与 APP_IDENTIFIER 不一致 —— 两处必须同步改"
    );
}

/// identifier 必须符合 Tauri 的格式要求 (小写 ASCII / 数字 / 连字符, 点分段)。
#[test]
fn identifier格式合法() {
    assert!(
        !APP_IDENTIFIER.is_empty(),
        "identifier 不能为空"
    );
    assert!(
        APP_IDENTIFIER
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-'),
        "identifier 只能含小写字母/数字/点/连字符: {APP_IDENTIFIER}"
    );
    assert!(
        APP_IDENTIFIER.contains('.'),
        "identifier 应为反向域名格式 (至少含一个点): {APP_IDENTIFIER}"
    );
}

/// 状态文件必须落在**用户配置目录**, 而不是 exe 旁边或当前工作目录。
///
/// 理由:
/// - exe 放 `C:\Program Files\` 等不可写目录 → 写不进去
/// - 从不同 cwd 运行同一个 exe → 状态位置必须一致
///
/// 按优先级: `%APPDATA%` → `%LOCALAPPDATA%` → `%TEMP%` → exe 同目录。
/// 前两个在受限环境 (只读 AppData) 会降级, 所以断言"在候选目录之一下",
/// 但**绝不能**是裸的当前工作目录相对路径。
#[test]
fn 状态文件落在用户配置目录() {
    let p = default_state_path();

    let is_candidate = |var: &str| {
        std::env::var_os(var)
            .map(|d| p.starts_with(PathBuf::from(d).join(APP_IDENTIFIER)))
            .unwrap_or(false)
    };
    let in_user_dir = is_candidate("APPDATA")
        || is_candidate("LOCALAPPDATA")
        || is_candidate("TEMP");

    if !in_user_dir {
        // 降级到了 exe 同目录 (所有候选都不可写时) —— 也比 cwd 好
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.to_path_buf()));
        assert!(
            exe_dir.map(|d| p.starts_with(d)).unwrap_or(false),
            "状态文件既不在用户目录也不在 exe 同目录: {}",
            p.display()
        );
    }

    assert_eq!(
        p.file_name().and_then(|n| n.to_str()),
        Some(".icon_arranger_state.json"),
        "文件名应为 .icon_arranger_state.json"
    );
}

/// 绝不能落在"当前工作目录"的裸相对路径上 —— 那会让历史跟着 cwd 跑。
#[test]
fn 状态路径不是当前工作目录() {
    let p = default_state_path();
    assert!(
        p.is_absolute(),
        "状态路径必须是绝对路径, 实际: {}",
        p.display()
    );
}

/// 同一进程内多次调用必须返回同一个路径 (不能有随机性)。
#[test]
fn 状态路径是稳定的() {
    assert_eq!(default_state_path(), default_state_path());
}

/// CLI 的 `--state` 参数必须能覆盖默认路径。
#[test]
fn state参数可以覆盖() {
    let args = vec![
        "--state".to_string(),
        r"D:\some\where\custom.json".to_string(),
    ];
    let cli = desktop_icon_arranger::cli::parse_cli(&args);
    assert_eq!(cli.state, PathBuf::from(r"D:\some\where\custom.json"));
}
