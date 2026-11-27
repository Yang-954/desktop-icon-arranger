//! 运行时路径策略的回归测试。
//!
//! 这些路径决定了 **exe 能不能放到任何地方都能用**, 一旦改错会静默出问题
//! (比如写到不可写目录 → 保存快照失败; 或跟着 cwd 跑 → 历史分裂)。

use desktop_icon_arranger::cli::default_state_path;
use std::path::PathBuf;

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
            .map(|d| p.starts_with(PathBuf::from(d).join("desktop-icon-arranger")))
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
