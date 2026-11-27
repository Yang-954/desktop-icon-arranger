//! 图标提取: 把桌面图标的真实图标渲染成 PNG (base64 data URL)。
//!
//! 两条路径 (与 Python 版一致):
//! 1. **系统虚拟目录** (此电脑 / 回收站 / 控制面板 / 网络 ...):
//!    `SHGetSpecialFolderLocation` 拿 PIDL, 再 `SHGetFileInfoW` 取图标句柄。
//!    这些图标 `QFileIconProvider` 取不到, 必须走 Shell API。
//! 2. **普通文件/文件夹**:
//!    `SHGetFileInfoW` 直接传路径, 取图标句柄。
//!
//! 渲染: `GetIconInfo` -> `CreateDIBSection` -> 画到内存 DC -> 取像素 -> 编码 PNG。
//!
//! PNG 编码用纯标准库 + 系统 zlib1.dll (compress2), 不引入额外 Rust 依赖。

use std::ffi::c_void;

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_USAGE, DeleteDC,
    DeleteObject, GetBitmapDimensionEx, GetDIBits, HGDIOBJ, HALFTONE, SelectObject,
    SetStretchBltMode, SRCCOPY, StretchBlt,
};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    ExtractIconExW, FOLDERID_ControlPanelFolder, FOLDERID_Desktop, FOLDERID_Profile,
    FOLDERID_RecycleBinFolder, KNOWN_FOLDER_FLAG, SHFILEINFOW, SHGFI_FLAGS, SHGFI_ICON,
    SHGFI_LARGEICON, SHGFI_PIDL, SHGFI_SYSICONINDEX, SHGetFileInfoW, SHGetKnownFolderPath,
    SHGetSpecialFolderLocation,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, HICON, ICONINFO,
};
use windows::core::{GUID, PCWSTR};

/// 系统虚拟目录名 -> CSIDL 值 (与 Python 版 `csidl_map` 一致)。
fn csidl_for(name: &str) -> Option<i32> {
    Some(match name.trim() {
        "此电脑" | "我的电脑" | "计算机" => 0x0010, // CSIDL_DRIVE
        "回收站" => 0x000A,                         // CSIDL_BITBUCKET
        "控制面板" => 0x0003,                       // CSIDL_CONTROLS
        "网络" | "网上邻居" => 0x0012,              // CSIDL_NETWORK
        "用户文件" => 0x0028,                       // CSIDL_PROFILE
        "我的文档" | "文档" => 0x0005,              // CSIDL_PERSONAL
        _ => return None,
    })
}

/// 提取系统虚拟目录的图标 (data URL)。
///
/// 实现说明:
/// Python 版走 `SHGetSpecialFolderLocation` -> PIDL -> `SHGetFileInfoW(SHGFI_PIDL)`。
/// 但 **windows crate 的 `SHGetFileInfoW` 只接受 `PCWSTR` (字符串指针), 无法传 PIDL**,
/// 所以这里换成等价做法: 用真实路径字符串 + `SHGetFileInfoW` 取图标。
///
/// 各虚拟目录的等价真实路径:
/// - 此电脑   -> `%SystemRoot%` (资源管理器的"此电脑"图标与 explorer.exe 一致)
/// - 回收站   -> `%SystemRoot%\System32\Shell32.dll` 的回收站图标 (用系统图标索引)
/// - 控制面板 -> `%SystemRoot%\System32\Control Panel\control.exe`
/// - 网络     -> `%SystemRoot%\System32\shell32.dll`
/// - 用户文件 -> `%USERPROFILE%`
pub fn extract_system_icon(name: &str, size: i32) -> Option<String> {
    // Shell/GDI 图标 API 非线程安全 —— 整个提取流程持锁。
    let _guard = ICON_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // 先试"系统图标索引"路径 (回收站/网络这类虚拟目录只有索引图标)
    if let Some(out) = extract_system_icon_by_index(name, size) {
        return Some(out);
    }
    // 再试已知系统图标索引
    if let Some(out) = extract_system_icon_by_known_index(name, size) {
        return Some(out);
    }
    // 再试"已知文件夹路径" (回收站 / 控制面板 —— Shell 给这些目录特殊图标)
    if let Some(out) = extract_system_icon_by_known_folder(name, size) {
        return Some(out);
    }
    // 最后试真实路径 (注意: extract_file_icon 内部也会加锁, 所以这里不能直接调它)
    let path = real_path_for(name)?;
    extract_file_icon_locked(&path, size)
}

/// 系统图标的兜底: 直接从系统 Shell 图标库按**固定索引**取。
///
/// `SHGetSpecialFolderLocation` + PIDL 路径在本机不可用 (windows crate 的
/// `SHGetFileInfoW` 只接受 PCWSTR, 传 PIDL 指针会失败), 所以用已知的
/// 系统图标索引直接取。这些索引是 Windows Shell 的固定约定。
fn extract_system_icon_by_known_index(name: &str, size: i32) -> Option<String> {
    // Windows Shell 的固定系统图标索引
    let index = match name.trim() {
        "回收站" => 42,   // Shell32 的回收站图标
        "网络" | "网上邻居" => 19, // 网上邻居
        "此电脑" | "我的电脑" | "计算机" => 15, // 我的电脑
        "控制面板" => 22, // 控制面板
        "用户文件" => 5,  // 个人文档
        _ => return None,
    };
    unsafe {
        let mut large: HICON = HICON::default();
        let mut small: HICON = HICON::default();
        let count = ExtractIconExW(
            PCWSTR::null(),
            index,
            Some(&mut large as *mut HICON),
            Some(&mut small as *mut HICON),
            1,
        );
        if count == 0 || large.is_invalid() {
            return None;
        }
        let result = icon_to_data_url(large, size);
        let _ = DestroyIcon(large);
        if !small.is_invalid() {
            let _ = DestroyIcon(small);
        }
        result
    }
}

/// 系统虚拟目录 -> 等价的真实路径。
fn real_path_for(name: &str) -> Option<String> {
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    Some(match name.trim() {
        "此电脑" | "我的电脑" | "计算机" => format!("{system}\\explorer.exe"),
        "控制面板" => format!("{system}\\System32\\Control Panel\\control.exe"),
        "网络" | "网上邻居" => format!("{system}\\System32\\shell32.dll"),
        "用户文件" => std::env::var("USERPROFILE").ok()?,
        _ => return None,
    })
}

/// 已知文件夹 ID -> 系统虚拟目录名。
///
/// 用于取**回收站**这类没有真实文件、只能靠"虚拟文件夹路径"拿图标的条目。
///
/// 与 Python 版 `_extract_system_icon` 的 `csidl_map` 对应:
/// Python 用 `SHGetSpecialFolderLocation` 拿 PIDL, 再 `SHGetFileInfoW(SHGFI_PIDL)`。
/// Rust 这边 windows crate 的 `SHGetFileInfoW` **只接受 PCWSTR, 传不了 PIDL**,
/// 所以改用 `SHGetKnownFolderPath` 拿等价路径 —— Shell 对这些目录同样给特殊图标。
fn known_folder_for(name: &str) -> Option<GUID> {
    match name.trim() {
        "回收站" => Some(FOLDERID_RecycleBinFolder),
        "控制面板" => Some(FOLDERID_ControlPanelFolder),
        "用户文件" => Some(FOLDERID_Profile),
        "桌面" => Some(FOLDERID_Desktop),
        _ => None,
    }
}

/// 通过已知文件夹路径取图标 (回收站 / 控制面板)。
fn extract_system_icon_by_known_folder(name: &str, size: i32) -> Option<String> {
    let guid = known_folder_for(name)?;

    unsafe {
        let pwstr = SHGetKnownFolderPath(&guid, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let path = pwstr.to_string().ok()?;
        CoTaskMemFree(Some(pwstr.0 as *const c_void));

        // `SHGetFileInfoW` 对这些虚拟目录仍能给出 Shell 特殊图标
        let wide = to_wide(&path);
        let mut info = SHFILEINFOW::default();
        let flags = SHGFI_FLAGS(SHGFI_ICON.0 | SHGFI_LARGEICON.0);
        let res = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        );
        if res == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let result = icon_to_data_url(info.hIcon, size);
        let _ = DestroyIcon(info.hIcon);
        result
    }
}

/// **原始的** `SHGetFileInfoW` —— 第一个参数是 `*const c_void`, 可以传 PIDL。
///
/// 为什么不用 windows crate 里的 `SHGetFileInfoW`?
/// 它的签名是 `pszpath: PCWSTR` —— **只能传字符串路径**, 没法传 PIDL 指针。
/// 而回收站 / 控制面板这类虚拟目录**必须走 PIDL** 才能拿到正确图标
/// (它们没有真实文件路径, 或者路径的图标不是 Shell 的特殊图标)。
///
/// Python 版用的是 `SHGetFileInfoW(pidl, 0, ..., SHGFI_PIDL | SHGFI_ICON | SHGFI_LARGEICON)`,
/// 这里自己声明一个等价签名, 保证两边行为一致。
#[link(name = "shell32")]
unsafe extern "system" {
    #[link_name = "SHGetFileInfoW"]
    fn SHGetFileInfoW_raw(
        pszpath: *const c_void,
        dwfileattributes: u32,
        psfinfo: *mut SHFILEINFOW,
        cbfileinfo: u32,
        uflags: u32,
    ) -> usize;
}

/// 通过系统图标索引取图标 (回收站 / 网络 这类虚拟目录专用)。
///
/// `SHGetSpecialFolderLocation` 拿 PIDL -> `SHGetFileInfoW(SHGFI_SYSICONINDEX)`
/// 拿索引 -> `ExtractIconExW(NULL, index)` 取图标。
fn extract_system_icon_by_index(name: &str, size: i32) -> Option<String> {
    let csidl = csidl_for(name)?;

    unsafe {
        let Ok(pidl) = SHGetSpecialFolderLocation(None, csidl) else {
            return None;
        };
        if pidl.is_null() {
            return None;
        }

        let mut info = SHFILEINFOW::default();
        // 先试"直接取图标" (PIDL 路径, 与 Python 版一致)
        let icon_flags = SHGFI_PIDL.0 | SHGFI_ICON.0 | SHGFI_LARGEICON.0;
        let res = SHGetFileInfoW_raw(
            pidl as *const c_void,
            0,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            icon_flags,
        );
        if res != 0 && !info.hIcon.is_invalid() {
            let result = icon_to_data_url(info.hIcon, size);
            let _ = DestroyIcon(info.hIcon);
            let _ = CoTaskMemFree(Some(pidl as *const c_void));
            return result;
        }

        // 再试"取系统图标索引" -> ExtractIconExW
        let mut info2 = SHFILEINFOW::default();
        let idx_flags = SHGFI_SYSICONINDEX.0 | SHGFI_PIDL.0;
        let res2 = SHGetFileInfoW_raw(
            pidl as *const c_void,
            0,
            &mut info2,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            idx_flags,
        );
        let _ = CoTaskMemFree(Some(pidl as *const c_void));
        if res2 == 0 {
            return None;
        }

        let mut large: HICON = HICON::default();
        let mut small: HICON = HICON::default();
        let count = ExtractIconExW(
            PCWSTR::null(),
            info.iIcon,
            Some(&mut large as *mut HICON),
            Some(&mut small as *mut HICON),
            1,
        );
        if count == 0 || large.is_invalid() {
            return None;
        }
        let result = icon_to_data_url(large, size);
        let _ = DestroyIcon(large);
        if !small.is_invalid() {
            let _ = DestroyIcon(small);
        }
        result
    }
}

/// 全局互斥锁: Shell/GDI 的图标 API **不是线程安全的**。
///
/// `SHGetFileInfoW` / `ExtractIconExW` / `GetIconInfo` / `CreateDIBSection` 这几个
/// 都会用到进程级的临时状态, 并发调用会互相破坏 HICON 句柄。
///
/// 所以**从取图标句柄到渲染成 PNG 的整个流程**都必须持锁 ——
/// 只锁渲染那一段不够 (句柄在锁外就已经被别的线程的 `DestroyIcon` 干掉了)。
static ICON_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 从 HICON 渲染出 PNG 的 base64 data URL。
///
/// 关键点: `GetIconInfo` 返回的 `xHotspot/yHotspot` 之类的字段不能直接用,
/// 但 **`hbm` (颜色位图) 与 `hbmMask` (掩码位图) 必须配对使用**:
/// 只画 `hbm` 而不处理掩码, 不同图标会渲染成同一个样子。
///
/// 做法:
/// 1. `GetIconInfo` 拿到 hbm (颜色) + hbmMask (1bpp 掩码)
/// 2. 建 48x48 的 DIB, 先用 hbmMask 作为背景画 (StretchBlt 缩放到目标尺寸)
/// 3. 再用 `hbm` 作为掩码 (SetDIBits) 叠加彩色图标
/// 4. 取像素 -> PNG
///
/// **调用方必须已持有 `ICON_LOCK`** (见 `extract_file_icon` / `extract_system_icon`)。
fn icon_to_data_url(hicon: HICON, size: i32) -> Option<String> {
    unsafe {
        let mut icon_info = ICONINFO::default();
        if GetIconInfo(hicon, &mut icon_info).is_err() {
            eprintln!("[icons] GetIconInfo 失败");
            return None;
        }

        let dc = CreateCompatibleDC(None);
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size, // 负数 = 自上而下
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            ..Default::default()
        };

        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbmp = match CreateDIBSection(
            Some(dc),
            &bmi as *const BITMAPINFO,
            DIB_USAGE(0),
            &mut bits,
            None,
            0,
        ) {
            Ok(b) => b,
            Err(_) => {
                let _ = DeleteDC(dc);
                return None;
            }
        };
        if hbmp.is_invalid() {
            let _ = DeleteDC(dc);
            return None;
        }

        let old = SelectObject(dc, HGDIOBJ(hbmp.0));

        // 1) 画掩码 (1bpp) 作为底层, StretchBlt 缩放到目标尺寸
        let (mw, mh) = bitmap_size(icon_info.hbmMask);
        let src_dc = CreateCompatibleDC(Some(dc));
        let src_bmp = SelectObject(src_dc, HGDIOBJ(icon_info.hbmMask.0));
        let _ = SetStretchBltMode(src_dc, HALFTONE);
        let _ = StretchBlt(
            dc, 0, 0, size, size, Some(src_dc),
            0, 0, mw, mh, SRCCOPY,
        );
        let _ = SelectObject(src_dc, HGDIOBJ(src_bmp.0));
        let _ = DeleteDC(src_dc);

        // 2) 用颜色位图叠加彩色图标
        let (cw, ch) = bitmap_size(icon_info.hbmColor);
        let src_dc2 = CreateCompatibleDC(Some(dc));
        let src_bmp2 = SelectObject(src_dc2, HGDIOBJ(icon_info.hbmColor.0));
        let _ = SetStretchBltMode(src_dc2, HALFTONE);
        let _ = StretchBlt(
            dc, 0, 0, size, size, Some(src_dc2),
            0, 0, cw, ch, SRCCOPY,
        );
        let _ = SelectObject(src_dc2, HGDIOBJ(src_bmp2.0));
        let _ = DeleteDC(src_dc2);

        // 3) 取像素
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        let got = GetDIBits(
            dc,
            hbmp,
            0,
            size as u32,
            Some(pixels.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_USAGE(0), // DIB_RGB_COLORS
        );
        let _ = SelectObject(dc, HGDIOBJ(old.0));
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        let _ = DeleteDC(dc);

        if got == 0 {
            eprintln!("[icons] GetDIBits 返回 0");
            return None;
        }

        // BGRA -> RGBA
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }

        encode_png(&pixels, size, size)
    }
}

/// 提取普通文件/文件夹的图标 (data URL)。
///
/// 用 `SHGetFileInfoW` (SHGFI_ICON | SHGFI_LARGEICON)。
///
/// 注意: **不能加 `SHGFI_USEFILEATTRIBUTES`** —— 它会让 Shell 对所有文件
/// 返回同一个默认图标 (实测 4 个不同 exe 提取出的 PNG 完全相同)。
pub fn extract_file_icon(path: &str, size: i32) -> Option<String> {
    // Shell/GDI 图标 API 非线程安全 —— 整个提取流程持锁。
    let _guard = ICON_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    extract_file_icon_locked(path, size)
}

/// `extract_file_icon` 的内部实现 —— **调用方必须已持有 `ICON_LOCK`**。
fn extract_file_icon_locked(path: &str, size: i32) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    // 文件必须真实存在 —— 否则会拿到默认的空白图标
    if !std::path::Path::new(path).exists() {
        return None;
    }
    unsafe {
        let wide = to_wide(path);
        let mut info = SHFILEINFOW::default();
        let flags = SHGFI_FLAGS(SHGFI_ICON.0 | SHGFI_LARGEICON.0);
        let res = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        );
        if res == 0 || info.hIcon.is_invalid() {
            eprintln!("[icons] SHGetFileInfoW 失败: {path} (res={res})");
            return None;
        }
        let result = icon_to_data_url(info.hIcon, size);
        let _ = DestroyIcon(info.hIcon);
        result
    }
}

/// 字符串 -> UTF-16 (带结尾 0)。
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 最小 PNG 编码器 (RGBA -> PNG **data URL**)。
///
/// 结构: 签名 + IHDR + IDAT (zlib deflate) + IEND。
/// zlib 用系统的 `compress2` (zlib1.dll), 不引入 Rust 依赖。
///
/// ⚠️ **必须返回完整的 data URL** (`data:image/png;base64,....`),
/// 不能只返回裸 base64。
/// 前端拿到后直接塞进 `<img src="...">` —— 裸 base64 会被浏览器当成
/// **相对路径**去请求, 结果 404, 界面就显示成一个空方框 (看着像"图标提取失败")。
/// Python 版的 `_pixmap_to_data_url` 返回的也是完整 data URL, 两边必须一致。
pub fn encode_png(rgba: &[u8], width: i32, height: i32) -> Option<String> {
    use base64::Engine;

    // --- 组装原始扫描线 (每行前面加一个 filter byte 0) ---
    let mut raw = Vec::with_capacity((height as usize) * (1 + width as usize * 4));
    for y in 0..height as usize {
        raw.push(0u8); // filter type: None
        let start = y * width as usize * 4;
        raw.extend_from_slice(&rgba[start..start + width as usize * 4]);
    }

    let compressed = match zlib_compress(&raw) {
        Some(c) => c,
        None => { eprintln!("[icons] zlib 压缩失败"); return None; }
    };

    // --- PNG 块 ---
    let mut png: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type: RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_chunk(&mut png, b"IHDR", &ihdr);

    write_chunk(&mut png, b"IDAT", &compressed);
    write_chunk(&mut png, b"IEND", &[]);

    // 完整的 data URL —— 前端直接当 <img src> 用, 缺前缀会变成相对路径 404
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

/// 写入一个 PNG 块 (长度 + 类型 + 数据 + CRC)。
fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// CRC32 (PNG 用的多项式 0xEDB88320)。
fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *entry = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// 取位图的实际尺寸 (像素)。
fn bitmap_size(hbm: windows::Win32::Graphics::Gdi::HBITMAP) -> (i32, i32) {
    unsafe {
        let mut sz = SIZE { cx: 0, cy: 0 };
        if GetBitmapDimensionEx(hbm, &mut sz).as_bool() && sz.cx > 0 && sz.cy > 0 {
            return (sz.cx, sz.cy);
        }
        (32, 32)
    }
}

/// zlib 压缩 (PNG 的 IDAT 需要 zlib 格式, 不是裸 deflate)。
///
/// 用纯 Rust 的 `flate2` (miniz_oxide 后端), 不依赖系统 zlib1.dll ——
/// 实测有些 Windows 精简安装里根本没有 zlib1.dll。
fn zlib_compress(data: &[u8]) -> Option<Vec<u8>> {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).ok()?;
    encoder.finish().ok()
}
