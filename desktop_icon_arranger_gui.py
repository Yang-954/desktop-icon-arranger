#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
桌面图标排列器 · QWebEngineView 版
====================================

整个 UI 由 web/index.html (毛玻璃主题、QWebChannel 桥接) 渲染, Python 仅负责:

  - 真实物理落地: 通过 Windows Shell ListView + LVM_SETITEMPOSITION 重排桌面图标
  - 亚克力毛玻璃 (Acrylic) 窗口背景: Windows 11/10 原生 DWM API
  - 桌面扫描与排序: 调用 desktop_icon_arranger 核心引擎
  - 单实例运行: QLocalServer 命名管道守卫
  - 主题切换持久化: QSettings + DWMWA_USE_IMMERSIVE_DARK_MODE

UI 由 HTML/CSS/JS 全权负责, 通过 QWebChannel 调用 Backend 接口。

依赖:
    pip install PySide6 pypinyin
"""

from __future__ import annotations

import base64
import json
import os
import sys

# 关键抗频闪配置：在导入 QtWebEngine 之前彻底禁用 DirectComposition 交换链与 GPU 光栅化冲突
# Windows 下 Chromium 与 Qt/DWM 混部渲染时，DirectComposition 极易产生 VSync 帧率脱节导致高频闪烁抖动
os.environ["QTWEBENGINE_CHROMIUM_FLAGS"] = (
    "--disable-features=DirectComposition "
    "--disable-gpu-rasterization "
    "--disable-zero-copy"
)
import threading
from ctypes import byref, c_int, sizeof
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from PySide6.QtCore import (
    QBuffer,
    QFileInfo,
    QObject,
    QSettings,
    Qt,
    QTimer,
    QUrl,
    Signal,
    Slot,
)
from PySide6.QtGui import QColor, QFont, QIcon, QImage, QPixmap
from PySide6.QtNetwork import QLocalServer, QLocalSocket
from PySide6.QtWidgets import QApplication, QFileIconProvider, QMainWindow
from PySide6.QtWebChannel import QWebChannel
from PySide6.QtWebEngineWidgets import QWebEngineView

import desktop_icon_arranger as core
import web.index_rc  # noqa: F401  # 触发 QRC 资源注册 (init Qt resources)

# --------------------------------------------------------------------------
# Windows DWM 绑定 (Acrylic + 标题栏暗色)
# --------------------------------------------------------------------------
import ctypes

dwmapi = ctypes.WinDLL("dwmapi", use_last_error=True)

DWMWA_USE_IMMERSIVE_DARK_MODE = 20
DWMWA_SYSTEMBACKDROP_TYPE = 38

ACCENT_STATE_DISABLED = 0
ACCENT_STATE_ENABLE_GRADIENT = 1
ACCENT_STATE_ENABLE_TRANSPARENTGRADIENT = 2
ACCENT_STATE_ENABLE_BLURBEHIND = 3  # Win10
ACCENT_STATE_ENABLE_ACRYLICBLURBEHIND = 4  # Win10
ACCENT_STATE_INVALID = 5


class ACCENT_POLICY(ctypes.Structure):
    _fields_ = [
        ("AccentState", ctypes.c_uint),
        ("AccentFlags", ctypes.c_uint),
        ("GradientColor", ctypes.c_uint),
        ("AnimationId", ctypes.c_uint),
    ]


class WINDOWCOMPOSITIONATTRIBDATA(ctypes.Structure):
    _fields_ = [
        ("Attribute", ctypes.c_int),
        ("Data", ctypes.POINTER(ctypes.c_int)),
        ("SizeOfData", ctypes.c_size_t),
    ]


user32 = ctypes.WinDLL("user32", use_last_error=True)
SetWindowCompositionAttribute = user32.SetWindowCompositionAttribute
SetWindowCompositionAttribute.restype = ctypes.c_bool
SetWindowCompositionAttribute.argtypes = [
    ctypes.c_void_p,
    ctypes.POINTER(WINDOWCOMPOSITIONATTRIBDATA),
]

# --------------------------------------------------------------------------
# Win32 SHGetFileInfo 用于提取系统虚拟目录的原生图标
# (此电脑 / 回收站 / 控制面板 / 网络 — QFileIconProvider 无法识别)
# --------------------------------------------------------------------------
shell32_icon = ctypes.WinDLL("shell32", use_last_error=True)
ole32_icon = ctypes.WinDLL("ole32", use_last_error=True)

SHGFI_PIDL = 0x00000008
SHGFI_ICON = 0x00000100
SHGFI_LARGEICON = 0x00000000
SHGFI_SMALLICON = 0x00000001


class SHFILEINFOW(ctypes.Structure):
    _fields_ = [
        ("hIcon", ctypes.c_void_p),
        ("iIcon", ctypes.c_int),
        ("dwAttributes", ctypes.c_uint),
        ("szDisplayName", ctypes.c_wchar * 260),
        ("szTypeName", ctypes.c_wchar * 80),
    ]


SHGetFileInfoW = shell32_icon.SHGetFileInfoW
SHGetFileInfoW.argtypes = [
    ctypes.c_void_p,             # pszPath (传 PIDL 地址)
    ctypes.c_uint,               # dwFileAttributes
    ctypes.POINTER(SHFILEINFOW), # psfi
    ctypes.c_uint,               # cbFileInfo (sizeof(SHFILEINFOW))
    ctypes.c_uint,               # uFlags
]
SHGetFileInfoW.restype = ctypes.c_size_t

SHGetSpecialFolderLocation = shell32_icon.SHGetSpecialFolderLocation
SHGetSpecialFolderLocation.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.POINTER(ctypes.c_void_p)]
SHGetSpecialFolderLocation.restype = ctypes.c_int


def _extract_system_icon(name: str, size: int = 48) -> QPixmap | None:
    """从 Windows 虚拟目录获取原生真实系统图标（此电脑/回收站/控制面板/网络等）。"""
    csidl_map = {
        "此电脑": 0x0011,    # CSIDL_DRIVES
        "这台电脑": 0x0011,
        "计算机": 0x0011,
        "我的电脑": 0x0011,
        "回收站": 0x000A,    # CSIDL_BITBUCKET
        "控制面板": 0x0003,  # CSIDL_CONTROLS
        "网络": 0x0012,      # CSIDL_NETWORK
        "网上邻居": 0x0012,
        "用户文件": 0x0028,  # CSIDL_PROFILE
        "我的文档": 0x0005,  # CSIDL_PERSONAL
        "文档": 0x0005,
    }
    csidl = csidl_map.get(name.strip())
    if csidl is None:
        return None
    try:
        pidl = ctypes.c_void_p()
        hr = SHGetSpecialFolderLocation(None, csidl, ctypes.byref(pidl))
        if hr != 0 or not pidl.value:
            return None

        info = SHFILEINFOW()
        flags = SHGFI_PIDL | SHGFI_ICON | SHGFI_LARGEICON
        res = SHGetFileInfoW(
            pidl,
            0,
            ctypes.byref(info),
            ctypes.sizeof(SHFILEINFOW),
            flags,
        )
        if not info.hIcon:
            if pidl.value:
                try:
                    ole32_icon.CoTaskMemFree(pidl)
                except Exception:
                    pass
            return None

        hicon = info.hIcon
        qimg = None
        try:
            qimg = QImage.fromHICON(hicon)
        except Exception:
            pass

        try:
            user32.DestroyIcon(hicon)
        except Exception:
            pass
        try:
            ole32_icon.CoTaskMemFree(pidl)
        except Exception:
            pass

        if qimg is None or qimg.isNull():
            return None
        return QPixmap.fromImage(qimg).scaled(size, size, Qt.KeepAspectRatio, Qt.SmoothTransformation)
    except Exception as e:
        print(f"[_extract_system_icon] error: {e}", file=sys.stderr)
        return None


def _pixmap_to_data_url(pixmap: QPixmap) -> str:
    """QPixmap → base64 PNG data URL。"""
    if pixmap is None or pixmap.isNull():
        return ""
    img = pixmap.toImage()
    buf = QBuffer()
    buf.open(QBuffer.OpenModeFlag.WriteOnly)
    img.save(buf, "PNG")
    return "data:image/png;base64," + base64.b64encode(buf.data().data()).decode("ascii")


def apply_acrylic_effect(hwnd: int) -> bool:
    """尝试在 Win11 上启用 Acrylic; 失败则回退到 Win10 Composition blur。"""
    if hwnd == 0:
        return False

    # 优先尝试 Win11 DWMWA_SYSTEMBACKDROP_TYPE = 3 (Acrylic)
    try:
        backdrop_type = c_int(3)  # DWMSBT_ACRYLIC
        dwmapi.DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            ctypes.byref(backdrop_type),
            ctypes.sizeof(backdrop_type),
        )
        return True
    except Exception:
        pass

    # 回退: Win10 SetWindowCompositionAttribute (Accent = AcrylicBlurBehind)
    try:
        accent = ACCENT_POLICY()
        accent.AccentState = ACCENT_STATE_ENABLE_ACRYLICBLURBEHIND
        accent.AccentFlags = 0
        accent.GradientColor = 0x99CCFF  # ARGB: 浅蓝灰, alpha 60%
        accent.AnimationId = 0

        data = WINDOWCOMPOSITIONATTRIBDATA()
        data.Attribute = 19  # WCA_ACCENT_POLICY
        data.Data = ctypes.cast(ctypes.pointer(accent), ctypes.POINTER(ctypes.c_int))
        data.SizeOfData = ctypes.sizeof(accent)

        return bool(
            SetWindowCompositionAttribute(ctypes.c_void_p(hwnd), ctypes.byref(data))
        )
    except Exception:
        return False


# --------------------------------------------------------------------------
# 单实例守卫
# --------------------------------------------------------------------------
SINGLE_INSTANCE_KEY = "DesktopIconArranger-WebView-SingleInstance-v1"


class SingleInstanceGuard:
    """QLocalServer 单实例守卫。 已运行 → 通知前台显示后退出。"""

    def __init__(self):
        QLocalServer.removeServer(SINGLE_INSTANCE_KEY)
        self._server = QLocalServer()
        self.is_primary = self._server.listen(SINGLE_INSTANCE_KEY)

    @property
    def server(self):
        return self._server

    def signal_primary(self) -> bool:
        sock = QLocalSocket()
        sock.connectToServer(SINGLE_INSTANCE_KEY)
        if not sock.waitForConnected(400):
            return False
        sock.write(b"show\n")
        sock.flush()
        sock.waitForBytesWritten(400)
        sock.disconnectFromServer()
        return True


def _bring_main_to_front(window: "MainWindow"):
    if window.isMinimized():
        window.showNormal()
    if not window.isVisible():
        window.show()
    window.raise_()
    window.activateWindow()


# --------------------------------------------------------------------------
# Backend: 暴露给 JS 的 Python 接口
# --------------------------------------------------------------------------
class Backend(QObject):
    """所有 JS → Python 调用经由此 QObject 中转。
    每个 @Slot 方法必须用 @Slot(type, result=...) 声明签名。"""

    iconScanned = Signal(str)  # JSON string
    iconThumbnailsLoaded = Signal(str)  # JSON string {id: base64, ...}
    iconsLoadFinished = Signal()  # 图标全部提取加载完成信号
    sortApplied = Signal(bool)
    snapshotRestored = Signal(bool)
    errorOccurred = Signal(str)

    # core 内部 kind 编码 → JS / HTML 友好分类
    KIND_MAP = {
        "SYS": "system",
        "S": "app",
        "D": "folder",
        "DOC": "doc",
        "MEDIA": "media",
        "ARCH": "archive",
        "F": "file",
    }

    def __init__(self, settings: QSettings, parent=None):
        super().__init__(parent)
        self._settings = settings
        self._core = core
        self._state_path = (
            Path(__file__).with_name(".icon_arranger_state.json")
            if hasattr(Path, "with_name")
            else Path(".icon_arranger_state.json")
        )
        # 文件/文件夹/应用的图标提供者 (QFileIconProvider 处理常见类型; 系统虚拟目录不行)
        self._file_icon_provider = QFileIconProvider()
        # 自定义 override 图标 (来自应用图标)
        ico_file = Path(__file__).with_name("app_icon.ico")
        self._app_fallback_icon = QIcon(str(ico_file)) if ico_file.exists() else QIcon()
        # 图标 Base64 内存缓存 (加速二次扫描与即时启动)
        self._icon_cache: dict[str, str] = {}
        # 缓存当前扫描结果，用于 preview_sort 零延迟计算规划顺序
        self._cached_icons: list[tuple[int, str, int, int]] = []
        self._cached_lookup: dict = {}
        self._cached_items: list[dict] = []

    def _run_win32(self, func, *args, **kwargs):
        """在独立工作线程中附着交互式桌面并执行底层 Win32 API, 彻底与 Qt 主线程隔离。"""
        out = []
        err = []
        def _worker():
            try:
                self._core.ensure_interactive_desktop()
                res = func(*args, **kwargs)
                out.append(res)
            except Exception as e:
                err.append(e)
        t = threading.Thread(target=_worker)
        t.start()
        t.join()
        if err:
            raise err[0]
        return out[0] if out else None

    # ---------- 真实图标提取 ----------

    def _icon_to_data_url(self, qicon: QIcon, size: int = 48) -> str:
        """QIcon → base64 PNG data URL。"""
        try:
            if qicon is None or qicon.isNull():
                return ""
            pm = qicon.pixmap(size, size)
            if pm.isNull():
                return ""
            return _pixmap_to_data_url(pm)
        except Exception:
            return ""

    def _extract_icon_b64(self, kind_raw: str, path_str: str, name: str) -> str:
        """根据分类提取 48x48 PNG 缩略图, 返回 data URL; 失败返回空串。"""
        try:
            if kind_raw == "SYS":
                pm = _extract_system_icon(name, 48)
                if pm is not None and not pm.isNull():
                    return _pixmap_to_data_url(pm)
                return self._icon_to_data_url(self._app_fallback_icon, 48)

            if path_str:
                p = Path(path_str)
                if p.exists():
                    qicon = self._file_icon_provider.icon(QFileInfo(str(p)))
                    if qicon is not None and not qicon.isNull():
                        return self._icon_to_data_url(qicon, 48)
            return self._icon_to_data_url(self._app_fallback_icon, 48)
        except Exception as e:
            print(f"[icon extract] {e}", file=sys.stderr)
            return ""

    # ---------- 桌面扫描 ----------

    @Slot(result=str)
    def scan_icons(self) -> str:
        """扫描当前桌面, 返回 JSON 字符串列表 [{id,name,cat,sys,path,modified}, ...]。"""
        def _do_scan():
            lv = self._core.find_desktop_listview()
            if not lv:
                return None, "未找到桌面 ListView"
            dirs = self._core.desktop_dirs()
            conflicts: list[str] = []
            lookup = self._core.build_lookup(dirs, conflicts)
            with self._core.DesktopIcons(lv) as di:
                icons = di.read_all()
            return (icons, lookup), None

        try:
            res, err_msg = self._run_win32(_do_scan)
            if res is None:
                self.errorOccurred.emit(err_msg or "扫描桌面失败")
                return "[]"

            icons, lookup = res
            import datetime

            items = []
            uncached = []
            for idx, (_hwnd, name, _x, _y) in enumerate(icons):
                hit = lookup.get(name.casefold())
                if hit is None:
                    kind_raw = "SYS"
                    path_str = ""
                else:
                    kind_raw, path_obj = hit
                    path_str = str(path_obj) if path_obj else ""

                cat = self.KIND_MAP.get(kind_raw, "file")
                mtime = "-"
                if path_str:
                    try:
                        p = Path(path_str)
                        if p.exists():
                            ts = p.stat().st_mtime
                            mtime = datetime.datetime.fromtimestamp(ts).strftime("%Y-%m-%d")
                    except Exception:
                        pass

                cache_key = f"{kind_raw}|{path_str}|{name}"
                icon_b64 = self._icon_cache.get(cache_key, "")
                if not icon_b64:
                    uncached.append((idx + 1, kind_raw, path_str, name, cache_key))

                items.append(
                    {
                        "id": idx + 1,
                        "name": name,
                        "cat": cat,
                        "sys": cat == "system",
                        "path": path_str,
                        "modified": mtime,
                        "icon": icon_b64,
                    }
                )

            self._cached_icons = icons
            self._cached_lookup = lookup
            self._cached_items = items

            # 极速第 1 阶段: 30ms 立即将桌面骨架与项目数据返回给前端, 页面瞬间显示
            payload = json.dumps(items, ensure_ascii=False)
            self.iconScanned.emit(payload)

            # 渐进第 2 阶段: 后台线程异步提取高清图标并通过信号平滑注入, 彻底消除启动等待
            if uncached:
                def _extract_worker():
                    updates = {}
                    for item_id, k_raw, p_str, nm, c_key in uncached:
                        b64 = self._extract_icon_b64(k_raw, p_str, nm)
                        if b64:
                            self._icon_cache[c_key] = b64
                            updates[str(item_id)] = b64
                            for ci in self._cached_items:
                                if ci["id"] == item_id:
                                    ci["icon"] = b64
                                    break
                    if updates:
                        self.iconThumbnailsLoaded.emit(json.dumps(updates))
                    # 提取完成后直接发射信号驱动前端自动刷新预览窗口
                    self.iconsLoadFinished.emit()

                th = threading.Thread(target=_extract_worker, daemon=True)
                th.start()
            else:
                # 缓存已命中全部图标, 立即通知前端刷新
                self.iconsLoadFinished.emit()

            return payload
        except Exception as e:
            err = f"scan_icons 失败: {e}"
            print(f"[Backend] {err}", file=sys.stderr)
            self.errorOccurred.emit(err)
            return "[]"

    # ---------- 排序预览计算 (与核心 plan() 保持 100% 一致) ----------

    def _compute_desktop_metrics(self) -> dict:
        """分析当前屏幕工作区与网格容量指标: 一列能排多少个 (max_rows), 一行能排多少个 (max_cols)。"""
        try:
            work = self._core.get_work_area()
            icons = self._cached_icons or []
            if not icons:
                lv = self._core.find_desktop_listview()
                if lv:
                    with self._core.DesktopIcons(lv) as di:
                        icons = di.read_all()
            slot_w, slot_h = self._core.DEFAULT_CELL_W, self._core.DEFAULT_CELL_H
            raw_x0, raw_y0 = work.left, work.top
            if icons:
                raw_xs, slot_w = self._core._regular_grid(
                    self._core._cluster([x for _, _, x, _ in icons]),
                    self._core.DEFAULT_CELL_W,
                )
                raw_ys, slot_h = self._core._regular_grid(
                    self._core._cluster([y for _, _, _, y in icons]),
                    self._core.DEFAULT_CELL_H,
                )
                raw_x0, raw_y0 = raw_xs[0], raw_ys[0]

            raw_x0 = max(raw_x0, work.left)
            raw_y0 = max(raw_y0, work.top)

            # 计算一列能排多少个 (垂直最大可用行数)
            full_ys = [raw_y0]
            while full_ys[-1] + 2 * slot_h <= work.bottom:
                full_ys.append(full_ys[-1] + slot_h)
            max_rows_per_col = len(full_ys)

            # 计算一行能排多少个 (水平最大可用列数)
            full_xs = [raw_x0]
            while full_xs[-1] + 2 * slot_w <= work.right:
                full_xs.append(full_xs[-1] + slot_w)
            max_cols_per_row = len(full_xs)

            return {
                "screen_w": work.right - work.left,
                "screen_h": work.bottom - work.top,
                "slot_w": slot_w,
                "slot_h": slot_h,
                "max_rows": max_rows_per_col,
                "max_cols": max_cols_per_row,
            }
        except Exception as e:
            print(f"[Backend] _compute_desktop_metrics 异常: {e}", file=sys.stderr)
            return {
                "screen_w": 1920,
                "screen_h": 1032,
                "slot_w": 76,
                "slot_h": 84,
                "max_rows": 12,
                "max_cols": 24,
            }

    @Slot(result=str)
    def get_desktop_info(self) -> str:
        """提供给前端单独获取桌面规格与行列容量信息。"""
        return json.dumps(self._compute_desktop_metrics(), ensure_ascii=False)

    @Slot(str, str, bool, result=str)
    def preview_sort(self, mode: str, direction: str, reverse: bool) -> str:
        """根据指定的 mode, direction, reverse 计算排序后的图标列表并返回 JSON。
        与底层核心 apply_sort 的 plan() 算法完全同源，确保预览与实际落地 100% 一致。"""
        try:
            if not self._cached_icons or not self._cached_items:
                self.scan_icons()

            if not self._cached_icons or not self._cached_items:
                return json.dumps({"items": [], "desktop": None}, ensure_ascii=False)

            sorted_names = self._core.plan(
                self._cached_icons,
                self._cached_lookup,
                mode=mode,
                reverse=reverse,
            )

            # 获取对应流向的物理槽位并绑定坐标与网格行列信息
            slots = self._core.build_slots(self._cached_icons, direction=direction)
            xs = sorted(list(set(x for x, _ in slots)))
            ys = sorted(list(set(y for _, y in slots)))

            item_map: dict[str, list[dict]] = {}
            for it in self._cached_items:
                item_map.setdefault(it["name"].casefold(), []).append(it)

            sorted_items = []
            used_ids = set()
            for idx, nm in enumerate(sorted_names):
                bucket = item_map.get(nm.casefold(), [])
                for it in bucket:
                    if it["id"] not in used_ids:
                        it_copy = dict(it)
                        if idx < len(slots):
                            sx, sy = slots[idx]
                            c_idx = xs.index(sx) + 1 if sx in xs else 1
                            r_idx = ys.index(sy) + 1 if sy in ys else 1
                            if direction == "col":
                                it_copy["slot_label"] = f"第{c_idx}列·行{r_idx}"
                            else:
                                it_copy["slot_label"] = f"第{r_idx}行·列{c_idx}"
                            it_copy["slot_coord"] = f"({sx}, {sy})"
                            it_copy["slot_col"] = c_idx
                            it_copy["slot_row"] = r_idx
                        sorted_items.append(it_copy)
                        used_ids.add(it["id"])
                        break

            for it in self._cached_items:
                if it["id"] not in used_ids:
                    sorted_items.append(dict(it))
                    used_ids.add(it["id"])

            desktop_info = self._compute_desktop_metrics()
            desktop_info["active_cols"] = len(xs)
            desktop_info["active_rows"] = len(ys)
            desktop_info["direction"] = direction
            desktop_info["icon_count"] = len(sorted_items)

            payload = {
                "items": sorted_items,
                "desktop": desktop_info,
            }
            return json.dumps(payload, ensure_ascii=False)
        except Exception as e:
            err = f"preview_sort 失败: {e}"
            print(f"[Backend] {err}", file=sys.stderr)
            self.errorOccurred.emit(err)
            return json.dumps({"items": self._cached_items, "desktop": None}, ensure_ascii=False)

    # ---------- 应用排序 ----------

    @Slot(str, str, bool, result=bool)
    def apply_sort(self, mode: str, direction: str, reverse: bool) -> bool:
        """真实重排桌面图标坐标。 捕获 auto_arrange 等错误。"""
        def _do_apply():
            lv = self._core.find_desktop_listview()
            if not lv:
                return False, "未找到桌面 ListView"
            if self._core.is_auto_arrange(lv):
                return False, "桌面开启了「自动排列图标」,请先在桌面右键 → 查看 → 取消勾选。"

            dirs = self._core.desktop_dirs()
            lookup = self._core.build_lookup(dirs, [])
            with self._core.DesktopIcons(lv) as di:
                icons = di.read_all()

            names = self._core.plan(icons, lookup, mode=mode, reverse=reverse)
            slots = self._core.build_slots(icons, direction=direction)

            # 保存快照
            self._core.save_state(self._state_path, icons)

            # 真正落地
            placed, missing, stuck = self._core.apply_positions(lv, names, slots)
            if stuck:
                return False, f"应用未完全成功: {len(stuck)} 个图标卡在中转区"

            # 预期位置序列与桌面实际坐标严格对比
            import time
            time.sleep(0.15)
            with self._core.DesktopIcons(lv) as di:
                actual_icons = di.read_all()
            # 重名图标必须按出现次序配对, 不能只按名字建单值映射
            actual_pos_map: dict[str, list[tuple[int, int]]] = {}
            for _, nm, ax, ay in actual_icons:
                actual_pos_map.setdefault(nm.casefold(), []).append((ax, ay))

            mismatches = []
            seen: dict[str, int] = {}
            for nm, (exp_x, exp_y) in zip(names, slots):
                k = nm.casefold()
                seq = seen.get(k, 0)
                seen[k] = seq + 1
                bucket = actual_pos_map.get(k, [])
                if seq >= len(bucket):
                    mismatches.append(f"{nm}(未在桌面找到)")
                    continue
                act_x, act_y = bucket[seq]
                # 桌面「对齐到网格」会把图标吸附到最近的网格线, 按单元格步长容差
                if (abs(act_x - exp_x) > self._core.DEFAULT_CELL_W // 2
                        or abs(act_y - exp_y) > self._core.DEFAULT_CELL_H // 2):
                    mismatches.append(f"{nm}(预期: {exp_x},{exp_y} 实际: {act_x},{act_y})")

            if mismatches:
                return False, f"位置校验未通过: {len(mismatches)} 个图标实际坐标与预期不符 (如: {mismatches[0]})"

            ok = True
            if ok:
                try:
                    item_map = {}
                    for it in self._cached_items:
                        item_map.setdefault(it["name"].casefold(), []).append(it)
                    sorted_items = []
                    used_ids = set()
                    for nm in names:
                        bucket = item_map.get(nm.casefold(), [])
                        for it in bucket:
                            if it["id"] not in used_ids:
                                sorted_items.append(it)
                                used_ids.add(it["id"])
                                break
                    for it in self._cached_items:
                        if it["id"] not in used_ids:
                            sorted_items.append(it)
                            used_ids.add(it["id"])
                    self._cached_items = sorted_items
                except Exception:
                    pass
            return True, None

        try:
            ok, err_msg = self._run_win32(_do_apply)
            if not ok and err_msg:
                self.errorOccurred.emit(err_msg)
            self.sortApplied.emit(ok)
            return ok
        except Exception as e:
            err = f"apply_sort 失败: {e}"
            print(f"[Backend] {err}", file=sys.stderr)
            self.errorOccurred.emit(err)
            return False

    # ---------- 撤销 ----------

    @Slot(result=bool)
    def undo_snapshot(self) -> bool:
        """还原到上一次应用前的快照。"""
        def _do_undo():
            lv = self._core.find_desktop_listview()
            if not lv:
                return False, "未找到桌面 ListView"
            rc = self._core.do_undo(lv, self._state_path, 0)
            return rc == 0, None

        try:
            ok, err_msg = self._run_win32(_do_undo)
            if not ok and err_msg:
                self.errorOccurred.emit(err_msg)
            self.snapshotRestored.emit(ok)
            return ok
        except Exception as e:
            err = f"undo_snapshot 失败: {e}"
            print(f"[Backend] {err}", file=sys.stderr)
            self.errorOccurred.emit(err)
            return False

    @Slot(int, result=bool)
    def restore_snapshot(self, depth: int) -> bool:
        """还原到往前第 N 层快照。"""
        def _do_restore():
            lv = self._core.find_desktop_listview()
            if not lv:
                return False, "未找到桌面 ListView"
            rc = self._core.do_undo(lv, self._state_path, depth)
            return rc == 0, None

        try:
            ok, err_msg = self._run_win32(_do_restore)
            if not ok and err_msg:
                self.errorOccurred.emit(err_msg)
            self.snapshotRestored.emit(ok)
            return ok
        except Exception as e:
            err = f"restore_snapshot({depth}) 失败: {e}"
            print(f"[Backend] {err}", file=sys.stderr)
            self.errorOccurred.emit(err)
            return False

    # ---------- 持久化顺序调整 (来自 UI 的置顶/上移/下移/置底/重置) ----------

    @Slot(str, result=bool)
    def save_order(self, order_json: str) -> bool:
        """兼容保留: 将自定义顺序按默认纵向排列物理写入桌面。"""
        return self.apply_custom_order(order_json, "col")

    @Slot(str, str, result=bool)
    def apply_custom_order(self, order_json: str, direction: str) -> bool:
        """根据前端传入的自定义微调顺序与流向，真实物理排布桌面并保存快照。"""
        def _do_apply_custom():
            try:
                order = json.loads(order_json)
                if not isinstance(order, list):
                    return False, "顺序数据无效"
                lv = self._core.find_desktop_listview()
                if not lv:
                    return False, "未找到桌面 ListView"
                if self._core.is_auto_arrange(lv):
                    return False, "桌面开启了「自动排列图标」,无法应用顺序调整。"

                with self._core.DesktopIcons(lv) as di:
                    icons = di.read_all()

                names_in_order = []
                for item_id in order:
                    found = False
                    for it in self._cached_items:
                        if it["id"] == item_id:
                            names_in_order.append(it["name"])
                            found = True
                            break
                    if not found and 1 <= item_id <= len(icons):
                        _, nm, _, _ = icons[item_id - 1]
                        names_in_order.append(nm)

                seen = set(names_in_order)
                for _, nm, _, _ in icons:
                    if nm not in seen:
                        names_in_order.append(nm)
                        seen.add(nm)

                slots = self._core.build_slots(icons, direction=direction or "col")
                self._core.save_state(self._state_path, icons)
                placed, missing, stuck = self._core.apply_positions(lv, names_in_order, slots)
                if stuck:
                    return False, f"应用微调未完全成功: {len(stuck)} 个图标卡在中转区"

                # 预期位置序列与桌面实际坐标严格对比
                import time
                time.sleep(0.15)
                with self._core.DesktopIcons(lv) as di:
                    actual_icons = di.read_all()
                # 重名图标按出现次序配对
                actual_pos_map: dict[str, list[tuple[int, int]]] = {}
                for _, nm, ax, ay in actual_icons:
                    actual_pos_map.setdefault(nm.casefold(), []).append((ax, ay))

                mismatches = []
                seen: dict[str, int] = {}
                for nm, (exp_x, exp_y) in zip(names_in_order, slots):
                    k = nm.casefold()
                    seq = seen.get(k, 0)
                    seen[k] = seq + 1
                    bucket = actual_pos_map.get(k, [])
                    if seq >= len(bucket):
                        mismatches.append(f"{nm}(未在桌面找到)")
                        continue
                    act_x, act_y = bucket[seq]
                    if (abs(act_x - exp_x) > self._core.DEFAULT_CELL_W // 2
                            or abs(act_y - exp_y) > self._core.DEFAULT_CELL_H // 2):
                        mismatches.append(f"{nm}(预期: {exp_x},{exp_y} 实际: {act_x},{act_y})")

                if mismatches:
                    return False, f"位置校验未通过: {len(mismatches)} 个图标实际坐标与预期不符 (如: {mismatches[0]})"

                return True, None
            except Exception as ex:
                return False, f"应用微调顺序失败: {ex}"

        try:
            ok, err_msg = self._run_win32(_do_apply_custom)
            if not ok and err_msg:
                self.errorOccurred.emit(err_msg)
            self.sortApplied.emit(ok)
            return ok
        except Exception as e:
            err = f"apply_custom_order 失败: {e}"
            print(f"[Backend] {err}", file=sys.stderr)
            self.errorOccurred.emit(err)
            return False

    @Slot(int, result=str)
    def preview_snapshot(self, depth: int) -> str:
        """预览指定深度的历史快照布局，返回带 slot_coord 的图标列表，不影响物理桌面。"""
        try:
            if not self._state_path.exists():
                return json.dumps({"items": [], "desktop": None, "error": "无快照文件"}, ensure_ascii=False)
            data = json.loads(self._state_path.read_text(encoding="utf-8"))
            positions, saved_at = self._core._pick_snapshot(data, depth)
            if not positions:
                return json.dumps({"items": [], "desktop": None, "error": "快照层级无效"}, ensure_ascii=False)

            if not self._cached_items:
                self.scan_icons()

            item_map: dict[str, list[dict]] = {}
            for it in self._cached_items:
                item_map.setdefault(it["name"].casefold(), []).append(it)

            xs = sorted(list(set(x for _, x, _ in positions)))
            ys = sorted(list(set(y for _, _, y in positions)))

            sorted_items = []
            used_ids = set()
            for idx, (nm, x, y) in enumerate(positions):
                bucket = item_map.get(nm.casefold(), [])
                for it in bucket:
                    if it["id"] not in used_ids:
                        it_copy = dict(it)
                        c_idx = xs.index(x) + 1 if x in xs else 1
                        r_idx = ys.index(y) + 1 if y in ys else 1
                        it_copy["slot_label"] = f"第{c_idx}列·行{r_idx}"
                        it_copy["slot_coord"] = f"({x}, {y})"
                        it_copy["slot_col"] = c_idx
                        it_copy["slot_row"] = r_idx
                        sorted_items.append(it_copy)
                        used_ids.add(it["id"])
                        break

            for it in self._cached_items:
                if it["id"] not in used_ids:
                    sorted_items.append(dict(it))
                    used_ids.add(it["id"])

            desktop_info = self._compute_desktop_metrics()
            desktop_info["active_cols"] = len(xs) if xs else 4
            desktop_info["active_rows"] = len(ys) if ys else 10
            desktop_info["direction"] = "col"
            desktop_info["icon_count"] = len(sorted_items)

            payload = {
                "items": sorted_items,
                "desktop": desktop_info,
                "is_snapshot": True,
                "snapshot_depth": depth,
                "snapshot_time": saved_at or "未知时间",
            }
            return json.dumps(payload, ensure_ascii=False)
        except Exception as e:
            err = f"preview_snapshot({depth}) 失败: {e}"
            print(f"[Backend] {err}", file=sys.stderr)
            return json.dumps({"items": [], "desktop": None, "error": str(e)}, ensure_ascii=False)

    @Slot(result=bool)
    def clear_history(self) -> bool:
        """清空本地历史快照文件。"""
        try:
            if self._state_path.exists():
                self._state_path.unlink()
            return True
        except Exception as e:
            print(f"[Backend] clear_history failed: {e}", file=sys.stderr)
            return False

    @Slot(result=str)
    def get_history_count(self) -> str:
        """返回快照数量 (JSON int 字符串)。"""
        try:
            if not self._state_path.exists():
                return "0"
            data = json.loads(self._state_path.read_text(encoding="utf-8"))
            layers = [{"saved_at": data.get("saved_at"), "positions": data.get("positions") or []}]
            layers.extend(h for h in (data.get("history") or []) if isinstance(h, dict))
            layers = [L for L in layers if L.get("positions")]
            return str(len(layers))
        except Exception:
            return "0"

    @Slot(result=str)
    def get_snapshot_list(self) -> str:
        """返回所有历史快照列表 (JSON 字符串): [{'depth': 0, 'time': '...', 'count': N}, ...]"""
        try:
            if not self._state_path.exists():
                return "[]"
            data = json.loads(self._state_path.read_text(encoding="utf-8"))
            layers = [{"saved_at": data.get("saved_at"), "positions": data.get("positions") or []}]
            layers.extend(h for h in (data.get("history") or []) if isinstance(h, dict))
            layers = [L for L in layers if L.get("positions")]
            res = []
            for depth, L in enumerate(layers):
                res.append({
                    "depth": depth,
                    "time": L.get("saved_at") or "未知时间",
                    "count": len(L.get("positions") or [])
                })
            return json.dumps(res, ensure_ascii=False)
        except Exception:
            return "[]"

    # ---------- 扫描源 (用户配置) ----------

    @Slot(result=str)
    def get_scan_sources(self) -> str:
        """返回用户配置的扫描源路径列表 (JSON)。"""
        try:
            dirs = self._core.desktop_dirs()
            sources = [{"path": str(d), "exists": d.exists()} for d in dirs]
            return json.dumps(sources, ensure_ascii=False)
        except Exception:
            return "[]"

    # ---------- 设置持久化 ----------

    @Slot(str, str, result=bool)
    def save_setting(self, key: str, value: str) -> bool:
        try:
            self._settings.setValue(f"ui/{key}", value)
            if key == "theme":
                parent = self.parent()
                if parent is not None and hasattr(parent, "_update_dwm_dark_mode"):
                    parent._update_dwm_dark_mode(value == "dark")
            return True
        except Exception as e:
            print(f"[Backend] save_setting failed: {e}", file=sys.stderr)
            return False

    @Slot(str, str, result=str)
    def get_setting(self, key: str, default: str) -> str:
        v = self._settings.value(f"ui/{key}", default, type=str)
        return v or default

    @Slot(result=str)
    def get_all_settings(self) -> str:
        """返回所有 UI 相关设置的 JSON 字符串, 供 JS 启动时一次性读取。"""
        try:
            settings = {
                "theme": self._settings.value("ui/theme", "light", type=str),
                "auto_apply": self._settings.value("ui/auto_apply", "0", type=str),
            }
            return json.dumps(settings, ensure_ascii=False)
        except Exception:
            return '{"theme": "light", "auto_apply": "0"}'

    @Slot(result=str)
    def get_app_version(self) -> str:
        return "桌面图标排列器 v2.0 (WebView)"

    @Slot(result=str)
    def ping(self) -> str:
        """健康检查, 用于 JS 测试 bridge 是否通。"""
        return "pong"


# --------------------------------------------------------------------------
# 主窗口
# --------------------------------------------------------------------------
class MainWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("桌面图标排列器 · WebView 版")
        self.resize(1260, 800)
        # 注意: 绝不开启 WA_TranslucentBackground 与 Acrylic 注入。
        # QWebEngineView(Chromium) 在开启透明背景分层窗口时，会与 Windows DWM 合成器
        # 产生高频重绘冲突，引发剧烈的界面高频闪烁抖动。毛玻璃效果由 HTML/CSS 原生高性能渲染。

        self._settings = QSettings("DesktopIconArranger", "WebViewPreview")
        self._theme = self._settings.value("ui/theme", "light", type=str)
        if self._theme not in ("light", "dark"):
            self._theme = "light"

        ico_file = Path(__file__).with_name("app_icon.ico")
        if ico_file.exists():
            self.setWindowIcon(QIcon(str(ico_file)))

        # 防闪屏: 把 webview 的 page 背景设成深色 (避免 Chromium 默认白底闪一下)
        # 主题色与 HTML <head> 内联脚本保持一致
        page_bg = "#eef0f9" if self._theme == "light" else "#0a0a14"

        # Backend + WebView + Channel
        self._backend = Backend(self._settings, self)
        self._webview = QWebEngineView(self)
        self._webview.page().setBackgroundColor(QColor(page_bg))

        self._channel = QWebChannel(self)
        self._channel.registerObject("backend", self._backend)
        # 先 setUrl 触发 page 创建, 再注册 channel
        self._webview.setUrl(QUrl(f"qrc:///index.html?theme={self._theme}"))
        self._webview.page().setWebChannel(self._channel)
        self.setCentralWidget(self._webview)

        self._dwm_dark_mode_applied = False

        # loadFinished 后关闭「准备中」标识, 避免页面在加载完成前一直显示 flash
        self._webview.loadFinished.connect(self._on_load_finished)

    def _on_load_finished(self, ok: bool):
        """页面加载完成后, 通知前端解禁动画（避免重复 setBackgroundColor 引发视口清屏闪烁）。"""
        try:
            self._webview.page().runJavaScript(
                "window.__APP_LOADED__ = true;"
                "document.documentElement.dataset.loaded = 'true';"
            )
        except Exception:
            pass

    def showEvent(self, event):
        super().showEvent(event)
        if not self._dwm_dark_mode_applied:
            self._update_dwm_dark_mode(self._theme == "dark")
            self._dwm_dark_mode_applied = True

    def _update_dwm_dark_mode(self, dark: bool):
        try:
            hwnd = int(self.winId())
            if hwnd == 0:
                return
            value = c_int(1 if dark else 0)
            dwmapi.DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                byref(value),
                sizeof(value),
            )
        except Exception:
            pass


# --------------------------------------------------------------------------
# Entry
# --------------------------------------------------------------------------

def main():
    # 主线程专心负责 Qt GUI 与 WebEngine 渲染，所有 Win32 Shell 交互均在独立工作线程中隔离执行。
    from PySide6.QtGui import QGuiApplication
    QGuiApplication.setAttribute(Qt.AA_ShareOpenGLContexts)

    app = QApplication(sys.argv)
    app.setApplicationName("桌面图标排列器")
    app.setOrganizationName("DesktopIconArranger")
    app.setFont(QFont("Microsoft YaHei", 9))

    guard = SingleInstanceGuard()
    if not guard.is_primary:
        ok = guard.signal_primary()
        msg = "[INFO] 已通知运行中的窗口,本进程退出。" if ok else "[WARN] 已检测到运行中的实例,但通知失败。"
        print(msg, file=sys.stderr, flush=True)
        sys.exit(0)

    w = MainWindow()
    w._single_instance_guard = guard

    def _on_another_launched():
        sock = guard.server.nextPendingConnection()
        if sock is None:
            return
        _bring_main_to_front(w)
        try:
            sock.disconnectFromServer()
        except Exception:
            pass

    guard.server.newConnection.connect(_on_another_launched)

    w.show()
    # 拉回主屏中心, 防止双屏/最小化导致看不到
    screen = app.primaryScreen()
    if screen:
        geo = screen.availableGeometry()
        x = geo.x() + (geo.width() - w.width()) // 2
        y = geo.y() + (geo.height() - w.height()) // 2
        w.move(x, y)

    sys.exit(app.exec())


if __name__ == "__main__":
    main()