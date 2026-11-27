//! 桌面图标排列器 —— 命令行入口。
//!
//! console 子系统, 有完整的 stdout/stderr。
//!
//! ```text
//! desktop-icon-arranger-cli.exe                      # dry-run, 只打印方案
//! desktop-icon-arranger-cli.exe --apply             # 真实应用到桌面
//! desktop-icon-arranger-cli.exe --mode name --direction row
//! desktop-icon-arranger-cli.exe --undo --depth 2     # 回退到第 2 层快照
//! desktop-icon-arranger-cli.exe --list-history
//! desktop-icon-arranger-cli.exe --json              # 机器可读输出
//! ```

use desktop_icon_arranger::cli;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = cli::parse_cli(&args);

    if let Err(e) = cli::run(cli) {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}
