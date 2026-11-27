//! 构建脚本:
//! 1. 把前端 HTML 复制成 Tauri 前端资源, 并注入 QWebChannel 兼容层。
//! 2. 运行 `tauri_build::build()` 生成 Windows 资源 (图标/manifest/版本信息)。
//!
//! 为什么要有第 1 步:
//! - 前端 `web/index.html` 依赖 QWebChannel (`new QWebChannel(qt.webChannelTransport, ...)`)
//! - Tauri 用的是自己的 `invoke` API
//! - 本脚本在 `<head>` 里注入 `qwebchannel-shim.js`, 让前端代码**一行不改**就能工作
//!
//! 输出目录: `web-frontend/index.html` (Tauri 的 frontendDist)

use std::path::{Path, PathBuf};

/// 找到前端的 index.html。
fn find_frontend_html(root: &Path) -> Option<PathBuf> {
    let local = root.join("web").join("index.html");
    if local.exists() {
        Some(local)
    } else {
        None
    }
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let shim = root.join("src").join("gui").join("qwebchannel-shim.js");
    let out_dir = root.join("web-frontend");
    let out_html = out_dir.join("index.html");

    println!("cargo:rerun-if-changed={}", shim.display());

    let Some(src_html) = find_frontend_html(&root) else {
        panic!(
            "找不到前端 index.html —— 请确认 {} 存在",
            root.join("web").join("index.html").display()
        );
    };
    println!("cargo:rerun-if-changed={}", src_html.display());

    let html = std::fs::read_to_string(&src_html)
        .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", src_html.display()));
    let shim_js = std::fs::read_to_string(&shim)
        .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", shim.display()));

    // 注入到 </head> 之前, 保证在 index.html 的任何脚本之前执行
    let injected = if let Some(pos) = html.find("</head>") {
        format!(
            "{}\n<!-- QWebChannel 兼容层 (Rust/Tauri) -->\n<script>\n{}\n</script>\n{}",
            &html[..pos],
            shim_js,
            &html[pos..]
        )
    } else {
        format!(
            "<!-- QWebChannel 兼容层 (Rust/Tauri) -->\n<script>\n{}\n</script>\n{}",
            shim_js, html
        )
    };

    std::fs::create_dir_all(&out_dir).expect("创建前端资源目录失败");
    std::fs::write(&out_html, injected).expect("写入前端资源失败");
    println!("已生成前端资源: {}", out_html.display());

    // Tauri 资源生成 (图标 / manifest / 版本信息)
    #[cfg(feature = "gui")]
    tauri_build::build();
}
