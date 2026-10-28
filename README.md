# 桌面图标排列器

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)

**不改任何文件名、不移动任何文件** —— 只通过 Windows Shell 消息重新摆放桌面图标在屏幕上的位置，真实落地生效。

---

## 文件结构

| 文件 / 目录 | 说明 |
| :--- | :--- |
| [`dist/桌面图标排序器.exe`](dist/桌面图标排序器.exe) | **独立单文件可执行程序**(QWebEngineView + Chromium 内核,绿色便携,双击运行,体积 ~243 MB) |
| [`desktop_icon_arranger_gui.py`](desktop_icon_arranger_gui.py) | **主程序**:QMainWindow + QWebEngineView 加载 web/index.html,Python 经 QWebChannel 暴露 scan/apply/undo 接口 |
| [`desktop_icon_arranger.py`](desktop_icon_arranger.py) | 核心底层引擎 + 命令行 CLI(5 种排序模式,双向流向,dry-run / apply / undo) |
| [`web/index.html`](web/index.html) | 毛玻璃主题 UI(纯 HTML/CSS/JS,经 `pyside6-rcc` 编译进 exe 资源系统) |
| [`web/index.qrc`](web/index.qrc) + [`web/index_rc.py`](web/index_rc.py) | Qt 资源描述文件及其 Python 编译产物 |
| [`build_exe.py`](build_exe.py) | 一键打包脚本(自动选 agent 环境 python,PyInstaller --onefile) |
| [`app_icon.ico`](app_icon.ico) | 应用程序图标文件 |
| `_preview-frosted/index.html` | 浏览器内单独预览版(无需 python,本地直接打开) |

---

## 技术架构 (v2 — QWebEngineView 版)

```
   ┌─────────────── Python 进程 ───────────────┐
   │                                           │
   │  QMainWindow                              │
   │    ├ DWM Acrylic 窗口背景 (Win11/10)       │
   │    └ QWebEngineView (Chromium 内核)        │
   │         │                                 │
   │         └─ QWebChannel ──> Backend ──> core│
   │             (JS ↔ Python)             engine │
   │                                           │
   │  单实例守卫: QLocalServer                  │
   │  主题持久化: QSettings                     │
   └───────────────────────────────────────────┘
            ▲                       ▲
            │                       │
       scan_icons()            apply_sort()
       undo_snapshot()         restore_snapshot()
       save_setting()          get_all_settings()
            │                       │
       ┌────┴────── JS / HTML ──────┴────┐
       │  web/index.html (CSS 毛玻璃)    │
       │  - backdrop-filter:blur(24px)  │
       │  - 主题:浅色/暗色 (CSS 变量)   │
       │  - 多视图切换 + 完整 UI 交互     │
       └────────────────────────────────┘
```

---

## 核心功能与亮点

### 1. 真实物理落地（解决此前“排序未生效”问题）
- **实测生效消息**：Windows 桌面 `SysListView32` 容器实际响应 `LVM_SETITEMPOSITION`（0x100F），采用 `MAKELPARAM(x, y)` 发送坐标。修复了此前误用未受支持的 `LVM_SETITEMPOSITION32` 导致静默失败的问题，并在落位后调用 `RedrawWindow` 触发系统级重绘。
- **两阶段防避让重排**：Explorer 在槽位冲突时会自动挤压避让原图标。本工具先将全体图标撤离至目标区之外的“中转区”，再精准落位目标槽位，绝不触发避让错位。
- **持久化（解决“重启 Explorer 后恢复原样”问题）**：实测确认 `HKCU\Software\Microsoft\Windows\Shell\Bags\1\Desktop\IconLayouts` 注册表二进制值才是桌面布局的权威持久化源。`LVM_SETITEMPOSITION` 移动单个或批量图标时 Explorer 都会同步改写该值，因此布局可跨 Explorer 重启 / 注销重登 / 开机保持。
  - **已废弃的 COM 方案**：曾尝试用 Shell `IFolderView::SelectAndPositionItems` + `IShellView::SaveViewState` 走官方接口，但 `IFolderView::Item(i)` 的索引体系与 `SysListView32` 的 ListView 索引**不一致**，会导致绝大多数图标取不到 PIDL 而静默失败（实测 36 个图标只移动 3 个），且该失败被 `return True` 掩盖，用户看到的就是“排序了但跟没排一样”。已整体移除相关 COM 代码。
  - **网格吸附容差**：桌面「对齐到网格」会把图标吸附到最近网格线，校验时按单元格步长的一半（`DEFAULT_CELL_W/H // 2`）作为容差，而非固定像素值。

### 2. 现代 Fluent 亚克力毛玻璃风格（Acrylic Frosted Glass）
- **系统级亚克力毛玻璃**：融合 Windows 11 DWM 系统级 `DWMWA_SYSTEMBACKDROP_TYPE`（Acrylic）与 Windows 10 模糊合成，窗口半透明透射桌面壁纸模糊效果。
- **规整统一卡片（IconCardDelegate）**：使用自定义委托接管绘制，每个卡片严格恒定为 `116 × 96` 像素，彻底根治此前网格高矮胖瘦参差不齐的问题。
- **原生真实系统图标**：通过 Win32 Shell API（`SHGetSpecialFolderLocation` + `SHGetFileInfoW`）从系统底层提取「此电脑」、「回收站」、「控制面板」、「网络」等真实原生图标，绝不用通用白文件图标。

### 3. 丰富排序规则（支持 5 种模式）
1. **🌟 标准推荐**（默认）：系统图标（此电脑/回收站/控制面板）置顶 -> 软件应用（自然拼音混排）-> 文件夹（自然拼音混排）-> 文件（修改时间由新到旧）
2. **🔤 纯名称混排**：系统图标置顶，随后所有软件、文件夹、文件统一按中英文拼音自然混排（A–Z）
3. **🕒 最新修改优先**：系统图标置顶，其余所有项按修改时间从新到旧倒序排序（最新生成或下载的文件/应用在最前）
4. **⏳ 最早创建优先**：系统图标置顶，其余所有项按修改时间从旧到新正序排序
5. **📁 详细分类聚合**：系统图标 -> 软件应用 -> 文件夹 -> 办公文档 -> 图片影音 -> 压缩安装包 -> 其它

### 4. 自然数字混排（Natural Numeric Sorting）
- 汉字通过 `pypinyin` 转拼音音节与英文字母同台混排（如 `千问(Q)` 排在 `PixPin(P)` 之后、`ToDesk(T)` 之前）。
- 连续数字按**数值大小**比较，解决 `10` 排在 `2` 前面的反直觉问题（如 `项目2` 排在 `项目10` 前，`Navicat 2` 排在 `Navicat 17` 前）。

### 5. 灵活排列流向（双向填充）
- **⬇️ 纵向·列优先**（Windows 经典默认）：先从上往下排满一列，再换到右边一列。
- **➡️ 横向·行优先**（符合阅读直觉）：先从左往右排满一行，再换到下一行。

---

## 使用指南

### 1. 直接运行 EXE（最简单，无需任何配置）

直接双击运行：
```
code/桌面排序器/dist/桌面图标排序器.exe
```
无需 Python、无需 pip 安装任何依赖，即开即用。

### 2. 源码启动图形界面（开发调试）

```bash
python desktop_icon_arranger_gui.py
```

- **顶部控制栏**：
  - 下拉切换 5 种排序规则
  - 切换 纵向·列优先 / 横向·行优先 流向
  - 一键切换 升序 / 降序
  - **搜索过滤框**：输入拼音或文字（如 `wx` 或 `微信`），实时聚焦高亮匹配图标
  - **扫描源面板**（可折叠）：自动识别用户桌面（支持 OneDrive / D 盘重定向）与公共桌面，支持自定义添加
- **主工作区网格**：
  - 真实复刻桌面列优先/行优先网格排布，带彩色分类角标与顺位序号
  - 鼠标直接拖拽微调顺位
  - **快捷微调按钮**：选中图标后可一键「⏫ 置顶」、「▲ 上移」、「▼ 下移」、「⏬ 置底」、「↺ 重置当前推荐」
- **侧边控制台**：
  - 分类统计卡片（各类数量明细与色彩图例）
  - 实时顺序清单（与网格联动高亮）
  - 选中图标属性卡片（名称、类别、原坐标、目标槽位、修改日期、绝对路径）
  - **「🚀 立即应用排序到桌面」**（真实重排屏幕图标坐标）
  - **「⏱️ 撤销 / 历史快照管理…」**（弹出快照列表，支持任意历史层级一键回退）

### 3. 一键重新打包 EXE

```bash
python build_exe.py
```
打包产物将自动输出到 `dist/桌面图标排序器.exe`。

### 4. 命令行模式（CLI）

```bash
# 默认 dry-run，仅打印方案与坐标对照，不修改桌面
python desktop_icon_arranger.py

# 真实应用标准推荐排序（列优先）
python desktop_icon_arranger.py --apply

# 按最新修改时间排序
python desktop_icon_arranger.py --mode time_desc --apply

# 按横向·行优先排列并应用
python desktop_icon_arranger.py --direction row --apply

# 纯名称 A-Z 混排并降序排列
python desktop_icon_arranger.py --mode name --reverse --apply

# 查看历史快照列表
python desktop_icon_arranger.py --list-history

# 还原到上一次应用前
python desktop_icon_arranger.py --undo

# 回退到往前第 N 层的历史快照
python desktop_icon_arranger.py --undo --depth 2
```

---

## 安全性与前置要求

1. **必须关闭「自动排列图标」**：
   - 桌面空白处 `右键 -> 查看 -> 取消勾选【自动排列图标】`。若开启该项，Windows 系统会锁定图标禁止外部自由排列。
   - 工具在应用与撤销前均会自动检测该状态，若开启会弹出清晰拦截提示。
2. **零破坏性**：
   - 绝不改动任何文件名、文件路径或文件内容。
   - 每次应用前均会自动捕获当前桌面所有图标坐标并压入历史栈（最多保留 20 份），任何时刻均可轻松撤销还原。
3. **环境隔离自适应**：
   - 内置 `ensure_interactive_desktop` 机制，自动处理 Windows 会话、终端、沙箱或后台进程所属桌面的附着，无论是在双击直接运行、CMD/PowerShell 还是 IDE 内嵌终端中，均可稳定定位桌面视图句柄。

---

## 许可

[MIT](./LICENSE) — Copyright (c) 2026 凡尘相守 (Yang-954)
