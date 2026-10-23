#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
桌面图标排列器 (Desktop Icon Arranger)
======================================

**不修改任何文件名、不移动任何文件**, 只通过 Windows Shell 的 ListView 消息
重新摆放桌面图标在屏幕上的位置。

支持多种排序模式:
    - standard  : 系统图标 -> 软件 -> 文件夹 -> 文件 (标准四段分组, 默认)
    - name      : 纯名称中英文自然拼音混排 (系统图标置顶)
    - time_desc : 按修改时间倒序 (最新文件/程序排最前)
    - time_asc  : 按修改时间正序 (最早修改排最前)
    - type      : 详细类型聚合 (系统 -> 软件 -> 文件夹 -> 文档 -> 媒体 -> 压缩包 -> 其它)

摆放流向:
    - col : 列优先 (纵向 Windows 默认: 先填满一列再换到右边列)
    - row : 行优先 (横向: 先从左往右填满一行再换到下一行)

摆放方式: 把现有图标坐标聚类出的列线/行线**吸附到规整网格**, 再按流向铺满。

前置条件: 桌面必须关闭"自动排列图标"(LVS_AUTOARRANGE)。apply 与 undo 都会
检测并明确报错, 不会静默失败。

用法:
    python desktop_icon_arranger.py                    # 只打印方案 (默认 dry-run)
    python desktop_icon_arranger.py --apply            # 应用 (自动保存原始位置)
    python desktop_icon_arranger.py --mode time_desc   # 按最新时间排序
    python desktop_icon_arranger.py --direction row    # 按行优先横向排列
    python desktop_icon_arranger.py --undo             # 还原到上次应用前
    python desktop_icon_arranger.py --list-history     # 列出可回退的历史快照
    python desktop_icon_arranger.py --undo --depth 2   # 回退到更早的快照

图形界面见同目录 desktop_icon_arranger_gui.py (PySide6)。
依赖: ctypes(标准库) + **pypinyin(必备)**。仅支持 Windows。
"""

from __future__ import annotations

import argparse
import ctypes
import json
import os
import re
import statistics
import sys
import time
from ctypes import wintypes
from pathlib import Path

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")

# --------------------------------------------------------------------------
# Win32 绑定
# --------------------------------------------------------------------------

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
shell32 = ctypes.WinDLL("shell32", use_last_error=True)
ole32 = ctypes.WinDLL("ole32", use_last_error=True)

# ---- 名称排序键 ----
MISSING_PYPINYIN_MSG = (
    "缺少必备依赖 pypinyin, 已中止。\n"
    "\n"
    "本工具承诺\"中文按拼音、与英文自然混排\", 该口径由 pypinyin 提供。\n"
    "安装:\n"
    "    pip install pypinyin\n"
    "国内网络:\n"
    "    pip install -i https://pypi.tuna.tsinghua.edu.cn/simple pypinyin"
)

try:
    from pypinyin import lazy_pinyin as _lazy_pinyin
except ImportError as _import_err:
    print(MISSING_PYPINYIN_MSG, file=sys.stderr)
    raise SystemExit(1) from _import_err


def natural_sort_key(s: str) -> list:
    """自然拼音混排排序键:
    中文按拼音音节、英文按字符, 连续数字按整数数值比较(例如 '2' 排在 '10' 之前)。
    统一转为小写后进行逐段比较。
    """
    parts = re.split(r"(\d+)", s)
    key = []
    for part in parts:
        if not part:
            continue
        if part.isdigit():
            # 数字块以数值比较: 标记类型 1
            key.append((1, int(part), ""))
        else:
            # 文本块: 汉字转拼音音节, 英文/符号保留小写
            syllables = _lazy_pinyin(part)
            for syll in syllables:
                key.append((0, 0, syll.casefold()))
    return key


def name_key(name: str) -> list:
    """兼容旧接口的名称排序键, 采用自然拼音混排。"""
    return natural_sort_key(name)


LRESULT = ctypes.c_ssize_t

user32.SendMessageW.restype = LRESULT
user32.SendMessageW.argtypes = [wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t]
user32.FindWindowExW.restype = wintypes.HWND
user32.FindWindowExW.argtypes = [wintypes.HWND, wintypes.HWND, wintypes.LPCWSTR, wintypes.LPCWSTR]
user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.GetWindowLongPtrW.restype = ctypes.c_ssize_t
user32.GetWindowLongPtrW.argtypes = [wintypes.HWND, ctypes.c_int]
user32.EnumWindows.argtypes = [ctypes.c_void_p, wintypes.LPARAM]
user32.SystemParametersInfoW.argtypes = [wintypes.UINT, wintypes.UINT, ctypes.c_void_p, wintypes.UINT]
user32.OpenDesktopW.restype = wintypes.HANDLE
user32.OpenDesktopW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
user32.SetThreadDesktop.restype = wintypes.BOOL
user32.SetThreadDesktop.argtypes = [wintypes.HANDLE]
user32.RedrawWindow.restype = wintypes.BOOL
user32.RedrawWindow.argtypes = [wintypes.HWND, ctypes.c_void_p, ctypes.c_void_p, wintypes.UINT]

kernel32.OpenProcess.restype = wintypes.HANDLE
kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.VirtualAllocEx.restype = ctypes.c_void_p
kernel32.VirtualAllocEx.argtypes = [wintypes.HANDLE, ctypes.c_void_p, ctypes.c_size_t,
                                    wintypes.DWORD, wintypes.DWORD]
kernel32.VirtualFreeEx.argtypes = [wintypes.HANDLE, ctypes.c_void_p, ctypes.c_size_t, wintypes.DWORD]
kernel32.WriteProcessMemory.argtypes = [wintypes.HANDLE, ctypes.c_void_p, ctypes.c_void_p,
                                        ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
kernel32.ReadProcessMemory.argtypes = [wintypes.HANDLE, ctypes.c_void_p, ctypes.c_void_p,
                                       ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]

WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

LVM_FIRST = 0x1000
LVM_GETITEMCOUNT = LVM_FIRST + 4       # 0x1004
LVM_SETITEMPOSITION = LVM_FIRST + 15   # 0x100F (标准 Win32 消息, 传 MAKELPARAM(x, y))
LVM_GETITEMPOSITION = LVM_FIRST + 16   # 0x1010
LVM_REDRAWITEMS = LVM_FIRST + 21       # 0x1015
LVM_GETITEMTEXTW = LVM_FIRST + 115     # 0x1073

LVIF_TEXT = 0x0001
GWL_STYLE = -16
LVS_AUTOARRANGE = 0x0100

PROCESS_FLAGS = 0x0008 | 0x0010 | 0x0020 | 0x0400  # VM_OP | VM_READ | VM_WRITE | QUERY_INFO
MEM_COMMIT_RESERVE = 0x3000
MEM_RELEASE = 0x8000
PAGE_READWRITE = 0x04

SOFTWARE_EXTS = (".lnk", ".exe", ".url", ".bat", ".cmd", ".appref-ms")
DOC_EXTS = (".docx", ".doc", ".xlsx", ".xls", ".pptx", ".ppt", ".pdf", ".txt", ".md", ".csv", ".wps")
MEDIA_EXTS = (".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".svg", ".mp4", ".mkv", ".mp3", ".wav")
ARCHIVE_EXTS = (".zip", ".7z", ".rar", ".tar", ".gz", ".iso")

DEFAULT_CELL_W = 76
DEFAULT_CELL_H = 84

COORD_MIN, COORD_MAX = -(2 ** 31), 2 ** 31 - 1
LEGACY_COORD_LIMIT = 32767

HISTORY_KEEP = 20

REMOTE_BLOCK = 4096
LVITEM_OFF = 0
POS_OFF = 1024
TEXT_OFF = 2048

KIND_LABEL = {
    "SYS": "系统图标",
    "S": "软件应用",
    "D": "文件夹",
    "DOC": "办公文档",
    "MEDIA": "图片影音",
    "ARCH": "压缩安装",
    "F": "普通文件",
}


class LVITEMW(ctypes.Structure):
    _fields_ = [
        ("mask", wintypes.UINT), ("iItem", ctypes.c_int), ("iSubItem", ctypes.c_int),
        ("state", wintypes.UINT), ("stateMask", wintypes.UINT),
        ("pszText", ctypes.c_void_p), ("cchTextMax", ctypes.c_int), ("iImage", ctypes.c_int),
        ("lParam", ctypes.c_void_p), ("iIndent", ctypes.c_int), ("iGroupId", ctypes.c_int),
        ("cColumns", wintypes.UINT), ("puColumns", ctypes.c_void_p),
        ("piColFmt", ctypes.c_void_p), ("iGroup", ctypes.c_int),
    ]


class POINT(ctypes.Structure):
    _fields_ = [("x", ctypes.c_long), ("y", ctypes.c_long)]


class RECT(ctypes.Structure):
    _fields_ = [("left", ctypes.c_long), ("top", ctypes.c_long),
                ("right", ctypes.c_long), ("bottom", ctypes.c_long)]


class GUID(ctypes.Structure):
    _fields_ = [
        ("Data1", wintypes.DWORD),
        ("Data2", wintypes.WORD),
        ("Data3", wintypes.WORD),
        ("Data4", wintypes.BYTE * 8)
    ]

    @classmethod
    def from_str(cls, s: str):
        g = cls()
        ole32.CLSIDFromString(wintypes.LPCWSTR(s), ctypes.byref(g))
        return g


SPI_GETWORKAREA = 0x0030


# --------------------------------------------------------------------------
# 定位桌面 ListView
# --------------------------------------------------------------------------

def ensure_interactive_desktop() -> bool:
    """确保当前线程附加到 default 交互桌面。
    必须在创建任何窗口句柄或 GUI 事件循环前调用。
    """
    try:
        DESKTOP_ALL = 0x01FF
        hdesk = user32.OpenDesktopW("default", 0, False, DESKTOP_ALL)
        if hdesk:
            return bool(user32.SetThreadDesktop(hdesk))
    except Exception:
        pass
    return False

# 注意: 模块加载时不再自动调用 ensure_interactive_desktop()。
# 调用方（命令行 main() 或 GUI main()）负责在 QApplication 创建前调用。
# 原因: QtWebEngine 要求主线程桌面上下文在 QApplication 初始化前保持原始状态，
#       模块导入时提前调用 SetThreadDesktop 会导致 WebEngine 渲染进程崩溃。


def _probe_in_current_desktop() -> int | None:
    """在当前线程所属桌面下寻找 SysListView32。"""
    tops: list[tuple[int, str]] = []

    def _cb(hwnd, _lparam):
        buf = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, buf, 256)
        tops.append((hwnd, buf.value))
        return True

    user32.EnumWindows(WNDENUMPROC(_cb), 0)

    def _probe(top_hwnd: int) -> int | None:
        dv = user32.FindWindowExW(top_hwnd, None, "SHELLDLL_DefView", None)
        if dv:
            lv = user32.FindWindowExW(dv, None, "SysListView32", None)
            if lv:
                return lv
        return None

    for want in ("Progman", "WorkerW"):
        for hwnd, cls in tops:
            if cls == want:
                lv = _probe(hwnd)
                if lv:
                    return lv
    return None


def find_desktop_listview() -> int | None:
    """返回桌面 SysListView32 的 HWND。

    Win10/11 下桌面视图可能挂在 Progman 下, 也可能被搬到某个 WorkerW 下。
    若在当前线程桌面未找到(例如后台执行或代理沙箱环境), 自动尝试切换至交互式
    default 桌面重试。
    """
    lv = _probe_in_current_desktop()
    if lv:
        return lv

    # 尝试切换至 default 交互桌面
    try:
        DESKTOP_ALL = 0x01FF
        hdesk = user32.OpenDesktopW("default", 0, False, DESKTOP_ALL)
        if hdesk:
            user32.SetThreadDesktop(hdesk)
            lv = _probe_in_current_desktop()
            if lv:
                return lv
    except Exception:
        pass

    return None


def is_auto_arrange(lv: int) -> bool:
    style = user32.GetWindowLongPtrW(lv, GWL_STYLE)
    return bool(style & LVS_AUTOARRANGE)


def get_work_area() -> RECT:
    r = RECT()
    user32.SystemParametersInfoW(SPI_GETWORKAREA, 0, ctypes.byref(r), 0)
    return r


AUTO_ARRANGE_HINT = (
    "错误: 桌面当前开启了 [自动排列图标], 所有位置设置都会被 explorer 忽略。\n"
    "      请先在桌面空白处右键 -> 查看 -> 取消勾选 [自动排列图标] 后重试。"
)


# --------------------------------------------------------------------------
# 读写图标
# --------------------------------------------------------------------------

class DesktopIcons:
    """打开一次 explorer 进程, 读取/设置桌面图标位置。"""

    def __init__(self, lv: int):
        self.lv = lv
        self.n = self._count()
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(lv, ctypes.byref(pid))
        self.hproc = kernel32.OpenProcess(PROCESS_FLAGS, False, pid.value)
        if not self.hproc:
            raise RuntimeError(
                f"无法打开 explorer 进程 (pid={pid.value}, err={ctypes.get_last_error()})"
            )
        self.base = kernel32.VirtualAllocEx(self.hproc, None, REMOTE_BLOCK,
                                            MEM_COMMIT_RESERVE, PAGE_READWRITE)
        if not self.base:
            kernel32.CloseHandle(self.hproc)
            raise RuntimeError("VirtualAllocEx 失败")
        self._cache: dict[str, list[int]] | None = None
        self._issued: dict[str, set[int]] = {}

    def close(self):
        if getattr(self, "base", None):
            kernel32.VirtualFreeEx(self.hproc, ctypes.c_void_p(self.base), 0, MEM_RELEASE)
            self.base = None
        if getattr(self, "hproc", None):
            kernel32.CloseHandle(self.hproc)
            self.hproc = None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def _count(self) -> int:
        return user32.SendMessageW(self.lv, LVM_GETITEMCOUNT, 0, 0)

    def redraw(self, count: int) -> None:
        if count > 0:
            user32.SendMessageW(self.lv, LVM_REDRAWITEMS, 0, count - 1)
        # 强制整窗重绘
        user32.RedrawWindow(self.lv, None, None, 0x0085)

    def _read_name(self, i: int) -> str:
        buf_addr = self.base + TEXT_OFF
        item = LVITEMW(mask=LVIF_TEXT, iItem=i, iSubItem=0,
                       pszText=ctypes.c_void_p(buf_addr), cchTextMax=260)
        kernel32.WriteProcessMemory(self.hproc, ctypes.c_void_p(self.base + LVITEM_OFF),
                                    ctypes.byref(item), ctypes.sizeof(item), None)
        user32.SendMessageW(self.lv, LVM_GETITEMTEXTW, i, self.base + LVITEM_OFF)
        tb = ctypes.create_unicode_buffer(260)
        kernel32.ReadProcessMemory(self.hproc, ctypes.c_void_p(buf_addr), tb, 520, None)
        return tb.value

    def _read_pos(self, i: int) -> tuple[int, int]:
        addr = self.base + POS_OFF
        pt = POINT(0, 0)
        kernel32.WriteProcessMemory(self.hproc, ctypes.c_void_p(addr),
                                    ctypes.byref(pt), 8, None)
        user32.SendMessageW(self.lv, LVM_GETITEMPOSITION, i, addr)
        kernel32.ReadProcessMemory(self.hproc, ctypes.c_void_p(addr),
                                   ctypes.byref(pt), 8, None)
        return pt.x, pt.y

    def read_all(self) -> list[tuple[int, str, int, int]]:
        self.n = self._count()
        self._cache = {}
        out = []
        for i in range(self.n):
            name = self._read_name(i)
            x, y = self._read_pos(i)
            self._cache.setdefault(name, []).append(i)
            out.append((i, name, x, y))
        return out

    def duplicate_names(self) -> dict[str, int]:
        if self._cache is None:
            self._rescan()
        return {k: len(v) for k, v in self._cache.items() if len(v) > 1}

    def _rescan(self) -> None:
        self.n = self._count()
        self._cache = {}
        for i in range(self.n):
            self._cache.setdefault(self._read_name(i), []).append(i)

    def begin_stage(self) -> None:
        self._issued.clear()

    def index_of(self, name: str) -> int | None:
        issued = self._issued.setdefault(name, set())
        for attempt in range(2):
            if self._cache is None:
                self._rescan()
            candidates = [i for i in self._cache.get(name, ()) if i not in issued]
            for idx in candidates:
                if self._read_name(idx) == name:
                    issued.add(idx)
                    return idx
            if attempt == 0:
                self._rescan()
                continue
        return None

    def set_position(self, index: int, x: int, y: int) -> bool:
        """用 LVM_SETITEMPOSITION 设置图标位置。

        实测确认: Windows 桌面 SysListView32 必须使用 LVM_SETITEMPOSITION (0x100F),
        lParam 传 MAKELPARAM(x, y)。LVM_SETITEMPOSITION32 (0x1049) 会直接返回 0
        且不执行任何位移。
        """
        x, y = int(x), int(y)
        lparam = ((y & 0xFFFF) << 16) | (x & 0xFFFF)
        res = user32.SendMessageW(self.lv, LVM_SETITEMPOSITION, index, lparam)
        return bool(res)


# --------------------------------------------------------------------------
# 分类与路径解析
# --------------------------------------------------------------------------

def _get_known_folder(guid_str: str) -> Path | None:
    try:
        guid = GUID()
        ole32.CLSIDFromString.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(GUID)]
        if ole32.CLSIDFromString(guid_str, ctypes.byref(guid)) != 0:
            return None
        p_path = wintypes.LPWSTR()
        hr = shell32.SHGetKnownFolderPath(ctypes.byref(guid), 0, None, ctypes.byref(p_path))
        if hr == 0 and p_path.value:
            path_str = p_path.value
            ole32.CoTaskMemFree(p_path)
            return Path(path_str)
    except Exception:
        pass
    return None


def desktop_dirs() -> list[Path]:
    """获取用户桌面目录列表(含系统重定向桌面、公共桌面)。"""
    out: list[Path] = []
    # 1. Shell API 获取实际用户桌面(支持 OneDrive / D 盘重定向)
    sh_user = _get_known_folder("{B4BFCC3A-DB2C-424C-B029-7FE99A87C641}")
    if sh_user and sh_user.is_dir() and sh_user not in out:
        out.append(sh_user)

    # 2. 环境变量降级
    up = os.environ.get("USERPROFILE")
    if up:
        p = Path(up) / "Desktop"
        if p.is_dir() and p not in out:
            out.append(p)

    # 3. 公共桌面
    sh_pub = _get_known_folder("{C4AA340D-F20F-4863-AFEF-F87EF2E6527E}")
    if sh_pub and sh_pub.is_dir() and sh_pub not in out:
        out.append(sh_pub)
    pub = os.environ.get("PUBLIC") or r"C:\Users\Public"
    p = Path(pub) / "Desktop"
    if p.is_dir() and p not in out:
        out.append(p)

    return out


def _classify_entry(p: Path) -> tuple[str, str]:
    """返回 (lookup_key, kind)。"""
    name = p.name
    suf = p.suffix.lower()
    if p.is_dir():
        return name.casefold(), "D"
    if suf == ".lnk":
        return name[:-4].casefold(), "S"
    if suf in SOFTWARE_EXTS:
        return name.casefold(), "S"
    if suf in DOC_EXTS:
        return name.casefold(), "DOC"
    if suf in MEDIA_EXTS:
        return name.casefold(), "MEDIA"
    if suf in ARCHIVE_EXTS:
        return name.casefold(), "ARCH"
    return name.casefold(), "F"


def build_lookup(dirs: list[Path], conflicts: list[str] | None = None) -> dict[str, tuple[str, Path]]:
    """建立 显示名(casefold) -> (kind, path) 映射。"""
    lookup: dict[str, tuple[str, Path]] = {}
    for d in dirs:
        try:
            entries = sorted(d.iterdir(), key=lambda p: p.name.casefold())
        except OSError as e:
            if conflicts is not None:
                conflicts.append(f"无法读取目录 {d}: {e}")
            continue
        for p in entries:
            name = p.name
            if name.startswith(".") or name.casefold() == "desktop.ini":
                continue
            try:
                key, kind = _classify_entry(p)
            except OSError as e:
                if conflicts is not None:
                    conflicts.append(f"无法识别 {p}: {e}")
                continue
            if key in lookup:
                if conflicts is not None:
                    conflicts.append(
                        f"显示名冲突: {name!r} 与 {lookup[key][1].name!r} 都映射到 {key!r}, "
                        f"已保留先出现的 {lookup[key][1].name!r}"
                    )
                continue
            lookup[key] = (kind, p)
    return lookup


def _safe_mtime(p: Path | None) -> float:
    if p is None:
        return float("inf")
    try:
        return p.stat().st_mtime
    except OSError:
        pass
    try:
        return p.lstat().st_mtime
    except OSError:
        return float("inf")


# --------------------------------------------------------------------------
# 排序规划 (多种模式)
# --------------------------------------------------------------------------

def plan(
    icons: list[tuple[int, str, int, int]],
    lookup: dict[str, tuple[str, Path]],
    mode: str = "standard",
    reverse: bool = False,
) -> list[str]:
    """按指定模式生成排好序的图标显示名列表。

    模式:
      - standard  : 系统图标置顶 -> 软件应用 -> 文件夹 -> 普通文件
      - name      : 纯名称混排 (系统图标置顶, 随后所有项自然拼音混排)
      - time_desc : 按修改时间倒序 (最新文件/程序排最前)
      - time_asc  : 按修改时间正序 (最早修改排最前)
      - type      : 详细分类聚合 (系统 -> 软件 -> 文件夹 -> 文档 -> 媒体 -> 压缩包 -> 其它)
    """
    sys_icons: list[tuple[int, str]] = []
    others: list[tuple[str, str, Path | None]] = []

    for idx, name, _x, _y in icons:
        hit = lookup.get(name.casefold())
        if hit is None:
            sys_icons.append((idx, name))
        else:
            others.append((name, hit[0], hit[1]))

    # 系统图标始终保持相对物理顺序置顶
    sys_icons.sort(key=lambda t: t[0])
    sys_names = [n for _, n in sys_icons]

    if mode == "name":
        # 纯名称混排
        sorted_others = sorted(others, key=lambda t: natural_sort_key(t[0]), reverse=reverse)
        return sys_names + [n for n, _, _ in sorted_others]

    elif mode == "time_desc":
        # 最新修改排在最前
        sorted_others = sorted(
            others,
            key=lambda t: (-_safe_mtime(t[2]), natural_sort_key(t[0])),
            reverse=reverse,
        )
        return sys_names + [n for n, _, _ in sorted_others]

    elif mode == "time_asc":
        # 最早修改排在最前
        sorted_others = sorted(
            others,
            key=lambda t: (_safe_mtime(t[2]), natural_sort_key(t[0])),
            reverse=reverse,
        )
        return sys_names + [n for n, _, _ in sorted_others]

    elif mode == "type":
        # 详细类型聚合: S -> D -> DOC -> MEDIA -> ARCH -> F
        order_map = {"S": 1, "D": 2, "DOC": 3, "MEDIA": 4, "ARCH": 5, "F": 6}
        sorted_others = sorted(
            others,
            key=lambda t: (
                order_map.get(t[1], 99),
                natural_sort_key(t[0]),
            ),
            reverse=reverse,
        )
        return sys_names + [n for n, _, _ in sorted_others]

    else:
        # standard 推荐分组: 软件 -> 文件夹 -> 文件(DOC/MEDIA/ARCH/F 统一为文件组)
        software = [t for t in others if t[1] == "S"]
        dirs = [t for t in others if t[1] == "D"]
        files = [t for t in others if t[1] not in ("S", "D")]

        software.sort(key=lambda t: natural_sort_key(t[0]), reverse=reverse)
        dirs.sort(key=lambda t: natural_sort_key(t[0]), reverse=reverse)
        # 文件按修改时间倒序(新文件排在前面), 同时间按名称
        files.sort(key=lambda t: (-_safe_mtime(t[2]), natural_sort_key(t[0])), reverse=reverse)

        return sys_names + [n for n, _, _ in software] + [n for n, _, _ in dirs] + [n for n, _, _ in files]


# --------------------------------------------------------------------------
# 槽位生成 (支持列优先与行优先)
# --------------------------------------------------------------------------

def _cluster(vals: list[int], tol: int = 8) -> list[int]:
    out: list[int] = []
    for v in sorted(set(vals)):
        if out and v - out[-1] <= tol:
            continue
        out.append(v)
    return out


def _regular_grid(lines: list[int], default_step: int, min_step: int = 32) -> tuple[list[int], int]:
    if len(lines) < 2:
        return list(lines), default_step
    gaps = [b - a for a, b in zip(lines, lines[1:]) if b - a >= min_step]
    if not gaps:
        step = default_step
    else:
        step = max(int(statistics.median(gaps)), min_step)
    base = lines[0]
    out: list[int] = []
    for v in lines:
        snapped = base + round((v - base) / step) * step
        if not out or snapped > out[-1]:
            out.append(snapped)
    return out, step


def _validate_slots(slots: list[tuple[int, int]], work: RECT,
                    cell_w: int, cell_h: int) -> None:
    if len(set(slots)) != len(slots):
        raise RuntimeError("内部错误: 槽位存在重复坐标")
    for x, y in slots:
        if not (COORD_MIN <= x <= COORD_MAX and COORD_MIN <= y <= COORD_MAX):
            raise RuntimeError(f"槽位 ({x},{y}) 超出可表示坐标范围")
        if x < work.left or y < work.top or x + cell_w > work.right or y + cell_h > work.bottom:
            raise RuntimeError(
                f"槽位 ({x},{y}) 超出工作区 "
                f"({work.left},{work.top})-({work.right},{work.bottom})"
            )


def build_slots(icons: list[tuple[int, str, int, int]],
                work: RECT | None = None,
                direction: str = "col") -> list[tuple[int, int]]:
    """生成完整规整网格槽位。

    direction:
      - 'col': 列优先(先自上而下填满一列, 再换到右边一列, Windows 默认)
      - 'row': 行优先(先自左向右填满一行, 再换到下方一行)
    """
    n = len(icons)
    if n == 0:
        return []
    if work is None:
        work = get_work_area()

    xs, slot_w = _regular_grid(_cluster([x for _, _, x, _ in icons]), DEFAULT_CELL_W)
    ys, slot_h = _regular_grid(_cluster([y for _, _, _, y in icons]), DEFAULT_CELL_H)

    xs[0] = max(xs[0], work.left)
    ys[0] = max(ys[0], work.top)

    ys_fit = [y for y in ys if y + slot_h <= work.bottom]
    xs_fit = [x for x in xs if x + slot_w <= work.right]
    if not ys_fit or not xs_fit:
        slot_w = min(slot_w, max(1, work.right - work.left))
        slot_h = min(slot_h, max(1, work.bottom - work.top))
        xs_fit = xs_fit or [work.left]
        ys_fit = ys_fit or [work.top]
    xs, ys = xs_fit, ys_fit

    if direction == "row":
        # 行优先: 先横向铺满屏幕全部可用列（先填满第一行），若放不下再向下增加行
        while xs[-1] + 2 * slot_w <= work.right:
            xs.append(xs[-1] + slot_w)
        while len(xs) * len(ys) < n:
            ny = ys[-1] + slot_h
            if ny + slot_h <= work.bottom:
                ys.append(ny)
            else:
                raise RuntimeError(
                    f"工作区放不下 {n} 个图标 (当前 {len(xs)} 列 x {len(ys)} 行, "
                    f"单元格 {slot_w}x{slot_h})"
                )
        slots = [(x, y) for y in ys for x in xs][:n]
    else:
        # 列优先: 先纵向铺满屏幕全部可用行（先填满第一列），若放不下再向右增加列
        while ys[-1] + 2 * slot_h <= work.bottom:
            ys.append(ys[-1] + slot_h)
        while len(xs) * len(ys) < n:
            nx = xs[-1] + slot_w
            if nx + slot_w <= work.right:
                xs.append(nx)
            else:
                raise RuntimeError(
                    f"工作区放不下 {n} 个图标 (当前 {len(xs)} 列 x {len(ys)} 行, "
                    f"单元格 {slot_w}x{slot_h})"
                )
        slots = [(x, y) for x in xs for y in ys][:n]

    _validate_slots(slots, work, slot_w, slot_h)
    return slots


def _rect_of(slots: list[tuple[int, int]], cell_w: int, cell_h: int) -> tuple[int, int, int, int]:
    xs = [x for x, _ in slots]
    ys = [y for _, y in slots]
    return min(xs), min(ys), max(xs) + cell_w, max(ys) + cell_h


def _overlaps(a: tuple[int, int, int, int], b: tuple[int, int, int, int]) -> bool:
    return a[0] < b[2] and b[0] < a[2] and a[1] < b[3] and b[1] < a[3]


def _temp_slots(slots: list[tuple[int, int]], n: int, work: RECT) -> list[tuple[int, int]]:
    """生成与目标区域完全不重叠的中转区域坐标。"""
    if n <= 0:
        return []
    cell_w, cell_h = DEFAULT_CELL_W, DEFAULT_CELL_H
    t_left, t_top, t_right, t_bottom = _rect_of(slots, cell_w, cell_h)

    def generate(origin_x: int, origin_y: int) -> list[tuple[int, int]] | None:
        cols = (work.right - origin_x) // cell_w
        rows = (work.bottom - origin_y) // cell_h
        if cols < 1 or rows < 1 or cols * rows < n:
            return None
        out: list[tuple[int, int]] = []
        for c in range(cols):
            for r in range(rows):
                out.append((origin_x + c * cell_w, origin_y + r * cell_h))
                if len(out) == n:
                    return out
        return out

    candidates = [
        (t_right + cell_w, max(work.top, t_top)),
        (max(work.left, t_left), t_bottom + cell_h),
    ]
    for origin_x, origin_y in candidates:
        tmp = generate(origin_x, origin_y)
        if tmp is None:
            continue
        if _overlaps(_rect_of(tmp, cell_w, cell_h), (t_left, t_top, t_right, t_bottom)):
            continue
        return tmp

    raise RuntimeError(
        f"找不到与目标区不重叠的中转区域 (需要 {n} 个单元格), 无法执行两阶段重排"
    )


# --------------------------------------------------------------------------
# 应用 / 撤销
# --------------------------------------------------------------------------

def apply_positions(lv: int, names: list[str], slots: list[tuple[int, int]],
                    originals: dict[str, tuple[int, int]] | None = None,
                    ) -> tuple[int, list[str], list[tuple[str, int, int]]]:
    """两阶段重排并落地到桌面, 返回 (成功落位数量, 未找到的名字, 遗留在中转区的项)。"""
    n = len(names)
    if n != len(slots):
        raise RuntimeError(f"图标数 {n} 与槽位数 {len(slots)} 不一致")
    if n == 0:
        return 0, [], []
    if is_auto_arrange(lv):
        raise RuntimeError("桌面开启了 [自动排列图标], 位置设置会被忽略, 已中止")

    work = get_work_area()
    _validate_slots(slots, work, DEFAULT_CELL_W, DEFAULT_CELL_H)
    tmp = _temp_slots(slots, n, work)

    missing: list[str] = []
    stuck: list[tuple[str, int, int]] = []

    with DesktopIcons(lv) as di:
        di.read_all()
        di.begin_stage()
        stage1_ops = []
        stage2_ops = []
        for name, (tx, ty), (sx, sy) in zip(names, tmp, slots):
            idx = di.index_of(name)
            if idx is None:
                missing.append(name)
                continue
            stage1_ops.append((idx, tx, ty))
            stage2_ops.append((idx, sx, sy, name))

        # 阶段 1: 全体撤离到中转区 (避开目标区, 防止碰撞挤位)
        staged: dict[int, tuple[int, int]] = {}
        for idx, tx, ty in stage1_ops:
            staged[idx] = (tx, ty)
            di.set_position(idx, tx, ty)
        di.redraw(n)
        time.sleep(0.12)

        # 阶段 2: 落到目标槽位
        placed = 0
        for idx, sx, sy, name in stage2_ops:
            if not di.set_position(idx, sx, sy):
                # 阶段 2 失败 => 该图标仍停留在中转区, 如实上报
                tx, ty = staged.get(idx, (0, 0))
                stuck.append((name, tx, ty))
            else:
                placed += 1
        di.redraw(n)

    return placed, sorted(set(missing)), stuck


def save_state(path: Path, icons: list[tuple[int, str, int, int]]) -> int:
    """保存图标位置快照到 JSON。"""
    path.parent.mkdir(parents=True, exist_ok=True)
    history: list[dict] = []
    if path.exists():
        try:
            old = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            old = None
        if isinstance(old, dict) and old.get("positions"):
            history.append({"saved_at": old.get("saved_at"), "positions": old["positions"]})
            prev_hist = old.get("history")
            if isinstance(prev_hist, list):
                history.extend(h for h in prev_hist if isinstance(h, dict) and h.get("positions"))
    data = {
        "saved_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "positions": [[n, x, y] for _i, n, x, y in icons],
        "history": history[:HISTORY_KEEP],
    }
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")
    return len(data["history"])


def _pick_snapshot(data: dict, depth: int) -> tuple[list[tuple[str, int, int]], str]:
    layers = [{"saved_at": data.get("saved_at"), "positions": data.get("positions") or []}]
    layers.extend(h for h in (data.get("history") or []) if isinstance(h, dict))
    layers = [L for L in layers if L.get("positions")]
    if not layers:
        raise ValueError("位置记录为空")
    if not 0 <= depth < len(layers):
        raise ValueError(f"--depth 超出范围: 可回退 0..{len(layers) - 1} (共 {len(layers)} 层)")
    layer = layers[depth]
    recs = layer["positions"]
    records = ([(n, x, y) for _i, n, x, y in recs] if len(recs[0]) == 4
               else [(n, x, y) for n, x, y in recs])
    return records, layer.get("saved_at") or "未知时间"


def do_undo(lv: int, path: Path, depth: int = 0) -> int:
    """还原到 depth 层之前的布局。"""
    if is_auto_arrange(lv):
        print(AUTO_ARRANGE_HINT)
        return 2
    if not path.exists():
        print(f"找不到位置记录: {path}")
        return 1
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as e:
        print(f"位置记录无法解析: {e}")
        return 1
    if not isinstance(data, dict):
        print("位置记录格式不正确")
        return 1
    try:
        records, saved_at = _pick_snapshot(data, depth)
    except ValueError as e:
        print(e)
        return 1

    slots = [(x, y) for _n, x, y in records]
    work = get_work_area()
    try:
        _validate_slots(slots, work, DEFAULT_CELL_W, DEFAULT_CELL_H)
        tmp = _temp_slots(slots, len(records), work)
    except RuntimeError as e:
        print(f"无法还原到该快照: {e}")
        return 1

    missing: list[str] = []
    stuck: list[tuple[str, int, int]] = []

    with DesktopIcons(lv) as di:
        di.read_all()
        di.begin_stage()
        # 索引只解析一次: index_of 会消耗 begin_stage() 的发放配额, 不能查两遍
        stage1_ops = []
        stage2_ops = []
        for (name, x, y), (tx, ty) in zip(records, tmp):
            idx = di.index_of(name)
            if idx is None:
                missing.append(name)
                continue
            stage1_ops.append((idx, tx, ty))
            stage2_ops.append((idx, x, y))

        # 阶段 1: 全体撤离到中转区
        for idx, tx, ty in stage1_ops:
            di.set_position(idx, tx, ty)
        di.redraw(len(records))
        time.sleep(0.12)

        # 阶段 2: 落到目标槽位
        placed = 0
        for idx, x, y in stage2_ops:
            if di.set_position(idx, x, y):
                placed += 1
        di.redraw(len(records))

    print(f"已还原 {placed} 个图标位置 (快照时间 {saved_at}, depth={depth})")
    gone = sorted(set(missing))
    if gone:
        print(f"桌面上已不存在 {len(gone)} 个图标, 跳过: {gone}")
    if stuck:
        print(f"警告: {len(stuck)} 个图标遗留在中转区:")
        for name, x, y in stuck:
            print(f"        {name!r} 当前约在 ({x},{y})")
    return 0


# --------------------------------------------------------------------------
# 终端打印
# --------------------------------------------------------------------------

def print_plan(icons, names, lookup, slots):
    pos_of: dict[str, list[tuple[int, int]]] = {}
    for _i, n, x, y in icons:
        pos_of.setdefault(n, []).append((x, y))

    print(f"{'新序':>4}  {'槽位':>13}  {'原名':<28} {'类别':<8} {'原位置'}")
    print("-" * 78)
    for pos, (name, (sx, sy)) in enumerate(zip(names, slots), 1):
        hit = lookup.get(name.casefold())
        kind = "SYS" if hit is None else hit[0]
        queue = pos_of.get(name) or []
        ox, oy = queue.pop(0) if queue else ("?", "?")
        label = KIND_LABEL.get(kind, "文件")
        print(f"{pos:>4}  ({sx:>4},{sy:>4})  {name:<28} {label:<8} ({ox},{oy})")


def _list_history(path: Path) -> int:
    if not path.exists():
        print(f"找不到位置记录: {path}")
        return 1
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as e:
        print(f"位置记录无法解析: {e}")
        return 1
    layers = [{"saved_at": data.get("saved_at"), "positions": data.get("positions") or []}]
    layers.extend(h for h in (data.get("history") or []) if isinstance(h, dict))
    layers = [L for L in layers if L.get("positions")]
    if not layers:
        print("位置记录为空")
        return 1
    print(f"共 {len(layers)} 层可回退 (--undo --depth N):")
    for i, L in enumerate(layers):
        tag = "当前快照" if i == 0 else f"历史 -{i}"
        print(f"  depth={i}  {tag}  {L.get('saved_at')}  {len(L['positions'])} 个图标")
    return 0


# --------------------------------------------------------------------------
# main CLI
# --------------------------------------------------------------------------

def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="桌面图标排列器(不改文件名, 真实物理重排)")
    ap.add_argument("--apply", action="store_true", help="应用排序")
    ap.add_argument("--undo", action="store_true", help="从位置记录还原")
    ap.add_argument("--depth", type=int, default=0,
                    help="配合 --undo: 回退到第几层快照 (0=最近一次)")
    ap.add_argument("--list-history", action="store_true", help="列出可回退的历史快照")
    ap.add_argument("--mode", choices=["standard", "name", "time_desc", "time_asc", "type"],
                    default="standard", help="排序模式: standard(四段分组), name(名称混排), time_desc(最新优先), time_asc(最早优先), type(详细类型)")
    ap.add_argument("--direction", choices=["col", "row"], default="col",
                    help="排列流向: col(列优先/纵向Windows默认), row(行优先/横向)")
    ap.add_argument("--reverse", action="store_true", help="反转排序顺序")
    ap.add_argument("--state", default=str(Path(__file__).with_name(".icon_arranger_state.json")),
                    help="位置记录文件路径")
    args = ap.parse_args(argv)

    lv = find_desktop_listview()
    if not lv:
        print("未找到桌面 ListView (SHELLDLL_DefView / SysListView32)")
        return 1
    print(f"桌面 ListView hwnd = {lv}")

    state = Path(args.state)

    if args.list_history:
        return _list_history(state)

    if is_auto_arrange(lv):
        print(AUTO_ARRANGE_HINT)
        return 2

    if args.undo:
        return do_undo(lv, state, args.depth)

    dirs = desktop_dirs()
    conflicts: list[str] = []
    lookup = build_lookup(dirs, conflicts)
    print(f"桌面来源: {', '.join(str(d) for d in dirs)}")

    with DesktopIcons(lv) as di:
        icons = di.read_all()
        dups = di.duplicate_names()

    names = plan(icons, lookup, mode=args.mode, reverse=args.reverse)
    slots = build_slots(icons, direction=args.direction)

    kinds = {"SYS": 0, "S": 0, "D": 0, "DOC": 0, "MEDIA": 0, "ARCH": 0, "F": 0}
    for _i, n, _x, _y in icons:
        hit = lookup.get(n.casefold())
        k = "SYS" if hit is None else hit[0]
        kinds[k] = kinds.get(k, 0) + 1
    print(f"图标共 {len(icons)} 个: 系统 {kinds['SYS']} / 软件 {kinds['S']} "
          f"/ 文件夹 {kinds['D']} / 文档 {kinds['DOC']} / 媒体 {kinds['MEDIA']} "
          f"/ 压缩包 {kinds['ARCH']} / 其它文件 {kinds['F']}")

    xs = sorted({x for x, _ in slots})
    ys = sorted({y for _, y in slots})
    flow_desc = "纵向·列优先" if args.direction == "col" else "横向·行优先"
    print(f"目标网格({flow_desc}): {len(xs)} 列 x {len(ys)} 行, 实填 {len(slots)} 格")
    if dups:
        print(f"警告: 存在同名图标 {dups}")
    for c in conflicts:
        print(f"警告: {c}")
    print()

    print_plan(icons, names, lookup, slots)
    print()

    if args.apply:
        originals = {}
        for _i, n, x, y in icons:
            originals.setdefault(n, (x, y))
        depth = save_state(state, icons)
        print(f"原始位置已记录: {state} (可回退 {depth + 1} 层)")
        done, missing, stuck = apply_positions(lv, names, slots, originals)
        print(f"已应用落地: {done} 个图标重新排列(未改动任何文件名)")
        if missing:
            print(f"未找到 {len(missing)} 个图标: {missing}")
        if stuck:
            print(f"警告: {len(stuck)} 个图标遗留在中转区, 请手动拖回:")
            for name, x, y in stuck:
                ox, oy = originals.get(name, ("?", "?"))
                print(f"        {name!r} 当前约在 ({x},{y}), 原位置 ({ox},{oy})")
    else:
        print("(dry-run, 未做任何改动; 加 --apply 才会实际应用到桌面)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
