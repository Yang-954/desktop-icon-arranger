#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
桌面图标排列器 · 一键打包脚本 (PyInstaller)
==============================================

将 desktop_icon_arranger_gui.py (QWebEngineView 版) 打包为独立的 Windows 单文件可执行程序
(dist/桌面图标排序器.exe)。

依赖(在 agent conda 环境中):
    pip install PySide6 pypinyin PyInstaller
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")
if hasattr(sys.stderr, "reconfigure"):
    sys.stderr.reconfigure(encoding="utf-8")

BASE_DIR = Path(__file__).resolve().parent
DIST_DIR = BASE_DIR / "dist"
BUILD_DIR = BASE_DIR / "build"
SPEC_FILE = BASE_DIR / "桌面图标排序器.spec"
MAIN_SCRIPT = BASE_DIR / "desktop_icon_arranger_gui.py"
ICON_FILE = BASE_DIR / "app_icon.ico"
WEB_QRC_PY = BASE_DIR / "web" / "index_rc.py"


def find_python_with_pyside6() -> str:
    """优先使用 agent conda 环境 (有 PySide6 + QtWebEngine + PyInstaller)。"""
    candidates = [
        Path("C:/Users/Admin/miniconda3/envs/agent/python.exe"),
        Path("C:/Users/Admin/miniconda3/envs/jkai/python.exe"),
        Path("C:/Users/Admin/miniconda3/python.exe"),
    ]
    for c in candidates:
        if c.exists():
            try:
                r = subprocess.run(
                    [str(c), "-c", "import PySide6, PySide6.QtWebEngineWidgets, PyInstaller"],
                    capture_output=True, text=True, timeout=10,
                )
                if r.returncode == 0:
                    return str(c)
            except Exception:
                continue
    # 退而求其次用当前 python
    return sys.executable


def build():
    print("=" * 60)
    print("[BUILD] 开始打包: 桌面图标排序器 (QWebEngineView 单文件绿色便携版)")
    print(f"工作目录: {BASE_DIR}")
    print("=" * 60)

    py_exe = find_python_with_pyside6()
    print(f"使用 Python: {py_exe}")

    # 0) 重新编译 QRC (用户改了 web/index.html 必须重新生 index_rc.py)
    print("\n[0/3] 重新编译 QRC -> web/index_rc.py")
    rcc = Path(py_exe).parent / "Scripts" / "pyside6-rcc.exe"
    if not rcc.exists():
        print(f"[WARN] 未找到 pyside6-rcc,跳过;请手动确认 web/index_rc.py 是最新的")
    else:
        rcc_cmd = [str(rcc), "-o", str(WEB_QRC_PY), str(BASE_DIR / "web" / "index.qrc")]
        rr = subprocess.run(rcc_cmd, cwd=str(BASE_DIR / "web"))
        if rr.returncode != 0:
            print("[ERROR] QRC 编译失败")
            return rr.returncode

    # 1) 清空 build/ dist/
    if DIST_DIR.exists():
        shutil.rmtree(DIST_DIR)
    if BUILD_DIR.exists():
        shutil.rmtree(BUILD_DIR)

    # 2) PyInstaller 命令
    cmd = [
        py_exe,
        "-m", "PyInstaller",
        "--name=桌面图标排序器",
        "--noconsole",
        "--onefile",
        "--clean",
        "--collect-data=pypinyin",

        # QtWebEngine 需要这些 hidden imports / data
        "--collect-submodules=PySide6.QtWebEngineWidgets",
        "--collect-submodules=PySide6.QtWebEngineCore",
        "--collect-submodules=PySide6.QtWebChannel",
        "--hidden-import=PySide6.QtWebEngineQuick",
        "--collect-data=PySide6",

        # 强制把 web.index_rc 模块打进去 (Qt 资源)
        f"--add-data={WEB_QRC_PY};web",

        f"--distpath={DIST_DIR}",
        f"--workpath={BUILD_DIR}",
        f"--specpath={BASE_DIR}",
    ]

    if ICON_FILE.exists():
        cmd.append(f"--icon={ICON_FILE}")
        cmd.append(f"--add-data={ICON_FILE};.")

    cmd.append(str(MAIN_SCRIPT))

    print("\n[1/3] 执行 PyInstaller")
    print("命令:\n", " ".join(cmd), "\n")
    ret = subprocess.run(cmd, cwd=str(BASE_DIR))

    if ret.returncode != 0:
        print(f"\n[ERROR] PyInstaller 失败, 退出码: {ret.returncode}")
        return ret.returncode

    exe_path = DIST_DIR / "桌面图标排序器.exe"
    print("\n" + "=" * 60)
    if exe_path.exists():
        sz_mb = exe_path.stat().st_size / (1024 * 1024)
        print("[SUCCESS] 打包成功!")
        print(f"产物: {exe_path} ({sz_mb:.1f} MB)")
        print("说明: 单文件绿色便携程序,可放置任意目录双击运行。")
        print("      单实例已开启,再次启动会通知原窗口前台。")
        print("      主题 (浅色/暗色) 通过 QSettings 持久化。")
    else:
        print("[ERROR] 报告成功但未找到 exe 文件")
    print("=" * 60)
    return 0


if __name__ == "__main__":
    raise SystemExit(build())