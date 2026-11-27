# 桌面图标排列器 (v3 · Rust)

**不改任何文件名、不移动任何文件** —— 只通过 Windows Shell 消息重新摆放桌面图标在屏幕上的位置, 真实落地生效。

> **v3 已用 Rust + Tauri 2 全面重写。** 早期的 Python 版 (PySide6 + QtWebEngine) 已被本实现取代并从仓库移除。
> 仓库历史里保留着 Python 版的全部提交, 可随时 `git log` 查看。

---

## 相比 v2 (Python 版)

| | v2 (Python) | **v3 (Rust)** |
|---|---|---|
| 产物体积 | 310 MB (onedir) | **3.6 MB** 单 exe |
| 运行时依赖 | 打包整个 PySide6 + Chromium | **无** —— 走系统 WebView2 |
| 窗口出现耗时 | 1.3–3 s | **0.11–0.33 s** |
| 内存占用 | 300–500 MB | **19 MB** |
| 排序结果 | 基准 | **与 Python 版逐项一致** (20/20 组合验证) |

关键收益不是"换语言", 而是**去掉打包的 Chromium 内核** (194 MB) 与**去掉 onefile 解压**。

> 双实现对照测试 (`tools/compare_impl.py`) 在 v3 开发期用于验证"两版排序结果逐项一致",
> v2 删除后该脚本随之移除 —— 它的结论已固化到下面的单元测试里。

---

## 快速开始

### 构建

```bash
cargo build --release                          # 两个 exe 都出
cargo build --release --no-default-features     # 只出 CLI 版 (不拉 Tauri 依赖)
```

产物 (`target/release/`):

| 文件 | 大小 | 子系统 | 用途 |
|---|---|---|---|
| `desktop-icon-arranger.exe` | **3.6 MB** | **GUI** | 双击运行, **完全不弹控制台** |
| `desktop-icon-arranger-cli.exe` | 1.1 MB | Console | 命令行, 有 stdout/stderr |

> **为什么拆成两个 exe？**
> Rust 的 `windows_subsystem` 是**编译期**属性, 没法按运行参数切换:
> - 用 GUI 子系统 → CLI 没有 stdout
> - 用 console 子系统 → 双击弹黑框; 而且在 Windows Terminal (ConPTY) 下
>   `GetConsoleWindow()` 返回的是**伪句柄**, `ShowWindow` 根本无效, 藏不掉
>
> 所以只能拆。这也逼出 `required-features = ["gui"]` ——
> 否则跑测试用的 `--no-default-features` 会把 GUI 版覆盖成 0.1 MB 的空壳。

### 运行

```bash
# GUI (双击 exe 也是这个) —— 不会弹控制台黑框
desktop-icon-arranger.exe

# CLI (dry-run, 只打印方案)
desktop-icon-arranger-cli.exe
desktop-icon-arranger-cli.exe --mode name --direction row
desktop-icon-arranger-cli.exe --apply           # 真实应用到桌面
desktop-icon-arranger-cli.exe --undo --depth 2  # 回退到第 2 层快照
desktop-icon-arranger-cli.exe --list-history
desktop-icon-arranger-cli.exe --json            # 机器可读输出 (供脚本消费)
```

---

## 架构

```
┌──────────────── Rust 进程 (3.6 MB) ─────────────────┐
│                                                     │
│  src/main.rs        GUI 入口 (GUI 子系统, 无控制台)   │
│  src/bin/dia-cli.rs CLI 入口 (Console 子系统)         │
│  src/lib.rs         核心引擎 (库, 不依赖 Tauri)       │
│    ├── cli.rs        CLI 逻辑 (参数/排序应用/撤销)      │
│    ├── win32.rs      Explorer 的 SysListView32 读写   │
│    ├── sorting.rs    5 种排序模式 + 槽位网格          │
│    ├── classify.rs   图标分类 (系统/软件/文件夹/...)   │
│    ├── state.rs      撤销快照 (与 Python 版同格式)    │
│    └── icons.rs      图标提取 → PNG (data URL)       │
│                                                     │
│  src/gui/           Tauri 2 壳                       │
│    ├── mod.rs        窗口 + 单实例插件                 │
│    ├── commands.rs   15 个命令 (= Python 的 15 个 Slot)│
│    ├── backend.rs    扫描/缓存/桌面线程封装             │
│    └── qwebchannel-shim.js   QWebChannel 兼容层        │
└─────────────────────────────────────────────────────┘
                    ▲
                    │ 复用 web/index.html (零改动)
                    │
              系统 WebView2 (Edge 内核)
```

### 关键设计 1: 前端一行不改

前端 `web/index.html` (2215 行) 通过 QWebChannel 调用 `backend.xxx(参数, 回调)`。
Tauri 用的是自己的 `invoke` API。

本项目注入一个 **QWebChannel 兼容层** (`src/gui/qwebchannel-shim.js`):

- 提供假的 `qt.webChannelTransport` 与 `QWebChannel` 构造器
- `backend.xxx(args..., callback)` → 翻译成 `window.__TAURI_INTERNALS__.invoke('xxx', args)`
- 信号 (`iconScanned` / `iconThumbnailsLoaded` / `iconsLoadFinished` / `errorOccurred`)
  用 `connect(fn)` 订阅, 底层是 `plugin:event|listen` + `transformCallback`

> **为什么用 `__TAURI_INTERNALS__` 而不是 `window.__TAURI__`?**
> `window.__TAURI__` (全局 API) 由 `app.withGlobalTauri = true` 控制, 但在 Tauri 2.12.7 上
> 该开关**不生效** —— 全局 API 脚本清单由 `tauri-build` 写进 app 包的 `OUT_DIR`,
> 而 `tauri-codegen` (proc-macro) 从**它自己的** `OUT_DIR` 读, 两者不是同一个目录。
> 而 `window.__TAURI_INTERNALS__` 由 `core.js` (IPC 初始化脚本) **总是**注入 ——
> 所以兼容层基于它自己实现 `invoke` 与事件订阅, 更稳。详见下面第 11 条。

注入由 `build.rs` 完成 —— 它读 `web/index.html`, 在 `</head>` 前插入 shim,
输出到 `web-frontend/index.html` (Tauri 的 `frontendDist`)。

**结果: `web/index.html` 一个字都没改。**

### 关键设计 2: 桌面切换只在工作线程

Windows 的 `SetThreadDesktop` 会把线程切到 default 桌面, 但该桌面句柄是**按权限打开**的。

如果打开时缺少 `DESKTOP_CREATEWINDOW (0x0002)`, 切换后该线程就**无权创建窗口** ——
而 Tauri 的主线程恰恰要创建窗口。

所以本项目:

| 线程 | 是否切桌面 | 理由 |
|---|---|---|
| **主线程** (Tauri 事件循环) | **否** | 它要创建窗口, 切桌面会破坏权限 |
| **工作线程** (访问 Explorer) | 是 | 干完就退出, 不影响主线程 |

对应 Python 版 `Backend._run_win32` 的做法。

---

## 与 Python 版 (v2) 的一致性

v3 重写时, 两个实现曾对**同一批真实桌面图标**各自生成排序方案, 逐项 diff
(顺序 + 每个图标的落位坐标 + 冲突提示), 覆盖
**5 种排序模式 × 2 种流向 × 2 种升降序 = 20 种组合**, 结果全部一致:

```
[OK]   mode=standard dir=col reverse=False  (43 个图标, 顺序与坐标完全一致)
...
================================================================
[PASS] 双实现完全一致 —— 20/20 种组合全部通过
================================================================
```

v2 删除后, 对照脚本 (`tools/compare_impl.py`) 随之移除 ——
**这次验证的结论已固化为本项目的单元测试**:

```bash
cargo test --release --no-default-features
```

- `tests/sort_key.rs` — 自然拼音混排排序键 (10 个用例)
- `tests/icons.rs` — 图标提取 (5 个用例, 含"不同文件必须产出不同图标"与"必须是完整 data URL")

另有 `tools/probe_gui.py` —— 通过 WebView2 的 DevTools 协议在真实页面里执行 JS / 查 DOM / 截图,
用来做**图形界面的端到端验证** (纯标准库实现, 不引第三方依赖)。

---

## 实现要点（踩坑速查）

从 Python 版迁到 Rust 版时踩过的坑, 每条都已写进对应代码的注释。
完整分析见 **[docs/移植笔记.md](docs/移植笔记.md)**。

| # | 坑 | 解法 |
|---|---|---|
| 1 | `DESKTOP_ALL` 漏 `DESKTOP_CREATEWINDOW` → 窗口建不出来 | 用字面量 `0x01FF`, 别手工拼 flag |
| 2 | `pypinyin` 把连续非汉字当**一个**元素 | 按 run 切分, 不是逐字符 |
| 3 | `sorted(reverse=True)` 是**整体反转**, 不是逐项倒排 | 排序键要自己带上负号 |
| 4 | `Path.suffix` 带点号, `Path::extension()` 不带 | 手动补点号 |
| 5 | 数字段解析漏掉第一个字符 (`"10"` → `"0"`) | 循环边界重算 |
| 6 | crate 的 `SHGetFileInfoW` 传不了 PIDL | 自己声明 `*const c_void` 签名的原始函数 |
| 7 | `SHGFI_USEFILEATTRIBUTES` 让所有文件返回同一个图标 | 去掉这个 flag |
| 8 | 图标渲染必须 `hbmMask` + `hbmColor` 配对 | `GetIconInfo` 两个位图都要用 |
| 9 | 本机没有 `zlib1.dll` | 用纯 Rust 的 `flate2` |
| 9b | 只返回裸 base64 → 前端 `<img>` 404 显示空方框 | 必须带 `data:image/png;base64,` 前缀 |
| 10 | `windows_subsystem` 是编译期的, 没法按参数切换 | 拆 GUI / CLI 两个二进制 |
| 11 | `withGlobalTauri` 在 Tauri 2.12 上不生效 | 只依赖 `window.__TAURI_INTERNALS__` |
| 12 | 缓存 key 在两处各算一半 → 刷新丢图标 | 让构造函数一并产出 key |
| 13 | Shell / GDI 图标 API 非线程安全 | 锁覆盖"取句柄 → 渲染 → 销毁"全程 |


## 运行时写什么 / 写哪里

**没有任何硬编码绝对路径** —— exe 放到任何盘符、任何目录（甚至 `C:\Program Files\`）
都能直接跑，不依赖它旁边的任何文件。

| 写什么 | 写到哪 | 说明 |
|---|---|---|
| **WebView2 用户数据** | `%LOCALAPPDATA%\com.desktopiconarranger.app\EBWebView\` | Chromium 的 Cookie / GPU 缓存 / 字体缓存，约 **11 MB**。跟着**用户**走，不跟 exe 走 |
| **位置记录（撤销历史）** | `%APPDATA%\com.desktopiconarranger.app\.icon_arranger_state.json` | 跟着**用户**走。换目录、换盘符运行同一个 exe，看到的都是同一份历史 |
| **UI 设置** | 不落盘 | 只在内存里 |

两处目录名都用同一个标识符 `com.desktopiconarranger.app`（即 `tauri.conf.json` 的 `identifier`），
全局只有一套命名。位置记录的路径按可写性依次尝试：
`%APPDATA%` → `%LOCALAPPDATA%` → `%TEMP%` → exe 同目录（兜底）。

若旧版本留下过位置记录（exe 同目录，或早期目录名 `desktop-icon-arranger`），
**首次运行会自动迁移**到新位置，历史不丢。
CLI 也可以用 `--state <路径>` 显式指定。

### 缓存会涨多大 / 会被自动清理吗？

实测（连跑 5 次，每次启动 9 秒）：

| | 大小 |
|---|---|
| 从零开始首次运行 | **10.7 MB** |
| 第 2 ~ 5 次运行 | 每次只增 **0.01 MB**（10 KB） |

**基本收敛，不会持续占用 C 盘。** 原因是这个应用只有一个固定页面，
Chromium 能缓存的东西就那么多（`Default` 5.5 MB + `GrShaderCache` 4.7 MB + `ShaderCache` 0.5 MB）。

Chromium 自身对磁盘缓存有配额上限，超过会自动淘汰旧条目；
即使真涨到上限也不会无限增长。想彻底清掉，删掉那个 `EBWebView` 目录即可，下次运行会自动重建。

---

## 已知限制

- 暂不支持"自定义扫描目录" (前端的"添加自定义源"按钮还只显示用户/公共桌面)。
- 图标缩略图尺寸固定 48×48。
- 需要系统已安装 **WebView2 运行时** (Win11 自带; Win10 需单独装)。

---

## 许可

[MIT](./LICENSE)
