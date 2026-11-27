//! Win32 桌面图标读写。
//!
//! 关键实现要点 (全部来自 Python 版的实测结论, 重构时必须保持一致):
//!
//! 1. 桌面图标容器是 Explorer 的 `SysListView32`, 位置读写靠 LVM_* 消息。
//! 2. **必须用 `LVM_SETITEMPOSITION` (0x100F)**, `lParam` 传 `MAKELPARAM(x, y)`。
//!    `LVM_SETITEMPOSITION32` (0x1049) 会直接返回 0 且不执行任何位移 (实测)。
//! 3. 读取图标名/坐标需要跨进程内存: 对 explorer 进程
//!    `VirtualAllocEx` 一块区域, `WriteProcessMemory` 写入 `LVITEMW`,
//!    `SendMessage` 后再 `ReadProcessMemory` 取回。
//! 4. 桌面视图可能挂在 `Progman` 下, 也可能被搬到某个 `WorkerW` 下, 两者都要找。
//! 5. 后台进程/沙箱环境下当前线程桌面可能不是交互桌面, 需要
//!    `OpenDesktopW("default")` + `SetThreadDesktop` 后重试。

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW, RedrawWindow};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
};
use windows::Win32::System::StationsAndDesktops::{
    DESKTOP_CONTROL_FLAGS, OpenDesktopW, SetThreadDesktop,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_PublicDesktop, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

// ---- LVM 消息常量 ----
const LVM_FIRST: u32 = 0x1000;
const LVM_GETITEMCOUNT: u32 = LVM_FIRST + 4; // 0x1004
const LVM_SETITEMPOSITION: u32 = LVM_FIRST + 15; // 0x100F
const LVM_GETITEMPOSITION: u32 = LVM_FIRST + 16; // 0x1010
const LVM_REDRAWITEMS: u32 = LVM_FIRST + 21; // 0x1015
const LVM_GETITEMTEXTW: u32 = LVM_FIRST + 115; // 0x1073

const LVIF_TEXT: u32 = 0x0001;
const LVS_AUTOARRANGE: i32 = 0x0100;

/// 桌面访问权限。
///
/// **必须是完整的 `0x01FF`** (与 Python 版 `DESKTOP_ALL = 0x01FF` 一致)。
///
/// ⚠️ 坑: 早先的 Rust 版手工挑了
/// `SWITCHDESKTOP | READOBJECTS | WRITEOBJECTS | HOOKCONTROL | ENUMERATE`
/// 拼成 `0x014D`, **漏掉了 `DESKTOP_CREATEWINDOW (0x0002)`**。
/// 后果: `SetThreadDesktop` 切到 default 桌面后, 该线程没有创建窗口的权限,
/// 后续 `CreateWindowExW` 直接返回 `E_ACCESSDENIED (0x80070005)`,
/// 表现为 Tauri 报 `failed to create window`。
///
/// 所以这里直接用字面量 `0x01FF`, 不再手工拼装 —— 拼装必然漏位。
const DESKTOP_ALL: u32 = 0x01FF;

/// 跨进程内存块布局 (与 Python 版完全一致)
const REMOTE_BLOCK: usize = 4096;
const LVITEM_OFF: usize = 0;
const POS_OFF: usize = 1024;
const TEXT_OFF: usize = 2048;

/// `LVITEMW` 结构 (64 位)
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct LvItemW {
    mask: u32,
    i_item: i32,
    i_sub_item: i32,
    state: u32,
    state_mask: u32,
    psz_text: *mut u16,
    cch_text_max: i32,
    i_image: i32,
    l_param: isize,
    i_indent: i32,
    i_group_id: i32,
    c_columns: u32,
    pu_columns: *mut u32,
    pi_col_fmt: *mut i32,
    i_group: i32,
}

// ---------------------------------------------------------------------------
// 桌面查找
// ---------------------------------------------------------------------------

/// 在当前线程所属桌面下寻找 `SysListView32`。
fn probe_in_current_desktop() -> Option<isize> {
    // EnumWindows 的回调不能捕获环境变量, 用 thread_local 传递结果。
    thread_local! {
        static TOPS: std::cell::RefCell<Vec<(isize, String)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    TOPS.with(|t| t.borrow_mut().clear());

    unsafe extern "system" fn cb(hwnd: HWND, _lparam: LPARAM) -> BOOL {
        let mut buf = [0u16; 256];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        let name = String::from_utf16_lossy(&buf[..len as usize]);
        TOPS.with(|t| t.borrow_mut().push((hwnd.0 as isize, name)));
        TRUE
    }

    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(0));
    }

    let tops: Vec<(isize, String)> = TOPS.with(|t| t.borrow().clone());

    let probe = |top: isize| -> Option<isize> {
        unsafe {
            let Ok(dv) = FindWindowExW(Some(HWND(top as *mut c_void)), None, w!("SHELLDLL_DefView"), None)
            else {
                return None;
            };
            let Ok(lv) = FindWindowExW(Some(dv), None, w!("SysListView32"), None) else {
                return None;
            };
            Some(lv.0 as isize)
        }
    };

    for want in ["Progman", "WorkerW"] {
        for (hwnd, cls) in &tops {
            if cls == want {
                if let Some(lv) = probe(*hwnd) {
                    return Some(lv);
                }
            }
        }
    }
    None
}

/// 确保当前线程附加到 default 交互桌面。
///
/// 必须在创建任何窗口句柄或 GUI 事件循环前调用。
pub fn ensure_interactive_desktop() -> bool {
    unsafe {
        let Ok(hdesk) = OpenDesktopW(w!("default"), DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ALL) else {
            return false;
        };
        if hdesk.is_invalid() {
            return false;
        }
        SetThreadDesktop(hdesk).is_ok()
    }
}

/// 返回桌面 `SysListView32` 的句柄。
///
/// 若当前线程桌面下找不到 (例如后台执行或代理沙箱环境), 自动尝试切换至
/// 交互式 default 桌面重试。
pub fn find_desktop_listview() -> Option<isize> {
    if let Some(lv) = probe_in_current_desktop() {
        return Some(lv);
    }

    unsafe {
        if let Ok(hdesk) = OpenDesktopW(w!("default"), DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ALL) {
            if !hdesk.is_invalid() {
                let _ = SetThreadDesktop(hdesk);
                return probe_in_current_desktop();
            }
        }
    }
    None
}

/// 检测桌面是否开启了「自动排列图标」。
///
/// 开启后系统会锁定图标位置, 禁止外部自由排列 —— 此时应用必须拦截。
pub fn is_auto_arrange(lv: isize) -> bool {
    unsafe {
        let style = GetWindowLongPtrW(HWND(lv as *mut c_void), GWL_STYLE);
        (style as i32 & LVS_AUTOARRANGE) != 0
    }
}

/// 返回当前屏幕工作区 (排除任务栏)。
pub fn get_work_area() -> RECT {
    unsafe {
        let mut r = RECT::default();
        if SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok() {
            return r;
        }
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }
}

// ---------------------------------------------------------------------------
// 桌面目录
// ---------------------------------------------------------------------------

/// 获取用户桌面目录 (支持 OneDrive / D 盘重定向)。
fn known_folder(guid: &windows::core::GUID) -> Option<std::path::PathBuf> {
    unsafe {
        let Ok(p_path) = SHGetKnownFolderPath(guid, KF_FLAG_DEFAULT, None) else {
            return None;
        };
        if !p_path.is_null() {
            let s = p_path.to_string().ok()?;
            let _ = CoTaskMemFree(Some(p_path.0 as *const c_void));
            return Some(std::path::PathBuf::from(s));
        }
    }
    None
}

/// 获取桌面目录列表 (用户桌面 + 公共桌面)。
pub fn desktop_dirs() -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();

    if let Some(p) = known_folder(&FOLDERID_Desktop) {
        if p.is_dir() && !out.contains(&p) {
            out.push(p);
        }
    }

    // 环境变量降级
    if let Ok(up) = std::env::var("USERPROFILE") {
        let p = std::path::PathBuf::from(up).join("Desktop");
        if p.is_dir() && !out.contains(&p) {
            out.push(p);
        }
    }

    if let Some(p) = known_folder(&FOLDERID_PublicDesktop) {
        if p.is_dir() && !out.contains(&p) {
            out.push(p);
        }
    }
    if let Ok(pubdir) = std::env::var("PUBLIC") {
        let p = std::path::PathBuf::from(pubdir).join("Desktop");
        if p.is_dir() && !out.contains(&p) {
            out.push(p);
        }
    }

    out
}

// ---------------------------------------------------------------------------
// 图标读写
// ---------------------------------------------------------------------------

/// 一个桌面图标: (ListView 索引, 显示名, x, y)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconEntry {
    pub index: i32,
    pub name: String,
    pub x: i32,
    pub y: i32,
}

/// 打开一次 explorer 进程, 读取/设置桌面图标位置。
pub struct DesktopIcons {
    lv: isize,
    hproc: HANDLE,
    base: *mut c_void,
    cache: Option<Vec<(String, Vec<i32>)>>,
    issued: Vec<(String, Vec<i32>)>,
}

impl DesktopIcons {
    /// 打开桌面 ListView 并分配跨进程内存块。
    pub fn new(lv: isize) -> anyhow::Result<Self> {
        let hwnd = HWND(lv as *mut c_void);
        let mut pid = 0u32;
        unsafe {
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }

        let access =
            PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_QUERY_INFORMATION;
        let hproc = unsafe { OpenProcess(access, false, pid) }?;
        if hproc.is_invalid() {
            anyhow::bail!("无法打开 explorer 进程 (pid={pid})");
        }

        let base = unsafe {
            VirtualAllocEx(
                hproc,
                None,
                REMOTE_BLOCK,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if base.is_null() {
            unsafe {
                let _ = CloseHandle(hproc);
            }
            anyhow::bail!("VirtualAllocEx 失败");
        }

        Ok(Self {
            lv,
            hproc,
            base: base as *mut c_void,
            cache: None,
            issued: Vec::new(),
        })
    }

    fn hwnd(&self) -> HWND {
        HWND(self.lv as *mut c_void)
    }

    /// 图标总数。
    pub fn count(&self) -> i32 {
        unsafe { SendMessageW(self.hwnd(), LVM_GETITEMCOUNT, None, None).0 as i32 }
    }

    /// 触发重绘。
    pub fn redraw(&self, count: i32) {
        unsafe {
            if count > 0 {
                SendMessageW(
                    self.hwnd(),
                    LVM_REDRAWITEMS,
                    Some(WPARAM(0)),
                    Some(LPARAM((count - 1) as isize)),
                );
            }
            // 强制整窗重绘
            let _ = RedrawWindow(
                Some(self.hwnd()),
                None,
                None,
                RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
            );
        }
    }

    /// 读取第 `i` 个图标的显示名。
    fn read_name(&self, i: i32) -> String {
        unsafe {
            let buf_addr = (self.base as usize + TEXT_OFF) as *mut u16;
            let item = LvItemW {
                mask: LVIF_TEXT,
                i_item: i,
                i_sub_item: 0,
                psz_text: buf_addr,
                cch_text_max: 260,
                ..Default::default()
            };
            let _ = WriteProcessMemory(
                self.hproc,
                (self.base as usize + LVITEM_OFF) as *const c_void,
                &item as *const LvItemW as *const c_void,
                size_of::<LvItemW>(),
                None,
            );
            SendMessageW(
                self.hwnd(),
                LVM_GETITEMTEXTW,
                Some(WPARAM(i as usize)),
                Some(LPARAM(self.base as usize as isize + LVITEM_OFF as isize)),
            );
            let mut buf = [0u16; 260];
            let _ = ReadProcessMemory(
                self.hproc,
                buf_addr as *const c_void,
                buf.as_mut_ptr() as *mut c_void,
                size_of_val(&buf),
                None,
            );
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..len])
        }
    }

    /// 读取第 `i` 个图标的坐标。
    fn read_pos(&self, i: i32) -> (i32, i32) {
        unsafe {
            let addr = self.base as usize + POS_OFF;
            let mut pt = POINT { x: 0, y: 0 };
            let _ = WriteProcessMemory(
                self.hproc,
                addr as *const c_void,
                &pt as *const POINT as *const c_void,
                size_of::<POINT>(),
                None,
            );
            SendMessageW(
                self.hwnd(),
                LVM_GETITEMPOSITION,
                Some(WPARAM(i as usize)),
                Some(LPARAM(addr as isize)),
            );
            let _ = ReadProcessMemory(
                self.hproc,
                addr as *const c_void,
                &mut pt as *mut POINT as *mut c_void,
                size_of::<POINT>(),
                None,
            );
            (pt.x, pt.y)
        }
    }

    /// 读取全部桌面图标。
    pub fn read_all(&mut self) -> Vec<IconEntry> {
        let n = self.count();
        let mut out = Vec::with_capacity(n.max(0) as usize);
        let mut cache: Vec<(String, Vec<i32>)> = Vec::new();
        for i in 0..n {
            let name = self.read_name(i);
            let (x, y) = self.read_pos(i);
            match cache.iter_mut().find(|(k, _)| *k == name) {
                Some((_, v)) => v.push(i),
                None => cache.push((name.clone(), vec![i])),
            }
            out.push(IconEntry { index: i, name, x, y });
        }
        self.cache = Some(cache);
        out
    }

    /// 返回重名的图标 (显示名 -> 数量)。
    pub fn duplicate_names(&mut self) -> Vec<(String, usize)> {
        if self.cache.is_none() {
            self.rescan();
        }
        self.cache
            .as_ref()
            .map(|c| {
                c.iter()
                    .filter(|(_, v)| v.len() > 1)
                    .map(|(k, v)| (k.clone(), v.len()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn rescan(&mut self) {
        let n = self.count();
        let mut cache: Vec<(String, Vec<i32>)> = Vec::new();
        for i in 0..n {
            let name = self.read_name(i);
            match cache.iter_mut().find(|(k, _)| *k == name) {
                Some((_, v)) => v.push(i),
                None => cache.push((name, vec![i])),
            }
        }
        self.cache = Some(cache);
    }

    /// 开始一次新的落位批次 (清空"已占用"记录)。
    pub fn begin_stage(&mut self) {
        self.issued.clear();
    }

    /// 按显示名查找图标索引 (跳过本批次已占用的同名项)。
    pub fn index_of(&mut self, name: &str) -> Option<i32> {
        for attempt in 0..2 {
            if self.cache.is_none() {
                self.rescan();
            }
            let candidates: Vec<i32> = self
                .cache
                .as_ref()
                .and_then(|c| c.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
                .unwrap_or_default();

            for idx in candidates {
                if self.issued.iter().any(|(k, v)| k == name && v.contains(&idx)) {
                    continue;
                }
                if self.read_name(idx) == name {
                    self.issued.push((name.to_string(), vec![idx]));
                    return Some(idx);
                }
            }
            if attempt == 0 {
                self.rescan();
            }
        }
        None
    }

    /// 设置图标位置。
    ///
    /// 实测确认: Windows 桌面 `SysListView32` 必须使用 `LVM_SETITEMPOSITION` (0x100F),
    /// `lParam` 传 `MAKELPARAM(x, y)`。`LVM_SETITEMPOSITION32` (0x1049) 会直接返回 0
    /// 且不执行任何位移。
    pub fn set_position(&mut self, index: i32, x: i32, y: i32) -> bool {
        let lparam = ((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF);
        unsafe {
            let res: LRESULT = SendMessageW(
                self.hwnd(),
                LVM_SETITEMPOSITION,
                Some(WPARAM(index as usize)),
                Some(LPARAM(lparam as isize)),
            );
            res.0 != 0
        }
    }
}

impl Drop for DesktopIcons {
    fn drop(&mut self) {
        unsafe {
            if !self.base.is_null() {
                let _ = VirtualFreeEx(self.hproc, self.base, 0, MEM_RELEASE);
                self.base = std::ptr::null_mut();
            }
            if !self.hproc.is_invalid() {
                let _ = CloseHandle(self.hproc);
            }
        }
    }
}

/// 获取当前模块句柄 (供图标提取等需要资源句柄的场景使用)。
pub fn module_handle() -> windows::Win32::Foundation::HMODULE {
    unsafe { GetModuleHandleW(PCWSTR::null()).unwrap_or_default() }
}
