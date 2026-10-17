//! Displays: monitors from `EnumDisplayMonitors`, refined with the display-configuration API
//! (friendly name, exact refresh rate, HDR state, SDR white level).

use core::mem;
use core::ptr;

use alloc::vec::Vec;

use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LPARAM, LUID, RECT,
};
use windows_sys::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW,
    HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows_sys::core::BOOL;

use super::wide_chars;
use crate::{Display, DisplayId, Hdr, Rect};

pub(super) fn enumerate(out: &mut Vec<Display>) {
    let mut monitors: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only pushes to `monitors`, which outlives the call.
    unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(collect),
            (&raw mut monitors) as LPARAM,
        )
    };
    let paths = active_paths();
    let start = out.len();
    out.extend(monitors.into_iter().filter_map(|m| describe(m, &paths)));
    // Primary first.
    if let Some(i) = out[start..].iter().position(|d| d.primary) {
        out[start..].swap(0, i);
    }
}

unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `Vec` passed by `enumerate`, borrowed for the enumeration.
    let monitors = unsafe { &mut *(data as *mut Vec<HMONITOR>) };
    monitors.push(monitor);
    1
}

/// The bounds and work area of `monitor`, or `None` if it is gone.
pub(super) fn monitor_info(monitor: HMONITOR) -> Option<MONITORINFOEXW> {
    // SAFETY: all-zero is a valid MONITORINFOEXW.
    let mut info: MONITORINFOEXW = unsafe { mem::zeroed() };
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `cbSize` says the buffer is a MONITORINFOEXW; invalid handles just fail.
    let ok = unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) };
    (ok != 0).then_some(info)
}

pub(super) fn describe_monitor(monitor: HMONITOR) -> Option<Display> {
    describe(monitor, &active_paths())
}

fn describe(monitor: HMONITOR, paths: &[DISPLAYCONFIG_PATH_INFO]) -> Option<Display> {
    let info = monitor_info(monitor)?;
    let mut display = Display::new(DisplayId(monitor as u64), rect(info.monitorInfo.rcMonitor));
    display.work_area = rect(info.monitorInfo.rcWork);
    display.primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
    display.set_device_name_chars(wide_chars(&info.szDevice));
    let (mut x, mut y) = (0, 0);
    // SAFETY: valid monitor and out pointers.
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) } >= 0 && x > 0 {
        display.dpi = x;
    }

    match paths
        .iter()
        .find(|p| source_name(p).is_some_and(|name| same_name(&name, &info.szDevice)))
    {
        Some(path) => refine(&mut display, path),
        None => {
            // No display-configuration path (some virtual displays): integer refresh rate only.
            // SAFETY: all-zero is a valid DEVMODEW.
            let mut mode: DEVMODEW = unsafe { mem::zeroed() };
            mode.dmSize = size_of::<DEVMODEW>() as u16;
            // SAFETY: NUL-terminated device name; `dmSize` is set.
            if unsafe {
                EnumDisplaySettingsW(info.szDevice.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode)
            } != 0
                && mode.dmDisplayFrequency > 1
            {
                display.refresh_rate_mhz = mode.dmDisplayFrequency * 1000;
            }
        }
    }
    Some(display)
}

fn refine(display: &mut Display, path: &DISPLAYCONFIG_PATH_INFO) {
    let target = &path.targetInfo;
    let rate = target.refreshRate;
    if rate.Denominator != 0 {
        display.refresh_rate_mhz =
            (rate.Numerator as u64 * 1000 / rate.Denominator as u64).min(u32::MAX as u64) as u32;
    }

    let mut name: DISPLAYCONFIG_TARGET_DEVICE_NAME = request(
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        target.adapterId,
        target.id,
    );
    if get(&mut name.header) {
        display.set_name_chars(wide_chars(&name.monitorFriendlyDeviceName));
    }

    let mut color: DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO = request(
        DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
        target.adapterId,
        target.id,
    );
    if get(&mut color.header) {
        // SAFETY: every bit pattern is a valid u32.
        let bits = unsafe { color.Anonymous.value };
        let (supported, enabled, wide_color_enforced) = (bits & 1, bits & 2, bits & 4);
        // `wideColorEnforced` marks an SDR display under automatic color management, not HDR.
        display.hdr = match (supported != 0 && wide_color_enforced == 0, enabled != 0) {
            (false, _) => Hdr::Unsupported,
            (true, false) => Hdr::Supported,
            (true, true) => Hdr::Enabled,
        };
    }

    let mut white: DISPLAYCONFIG_SDR_WHITE_LEVEL = request(
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
        target.adapterId,
        target.id,
    );
    // In thousandths of the 80-nit sRGB reference.
    if get(&mut white.header) && white.SDRWhiteLevel > 0 {
        display.sdr_white_nits = white.SDRWhiteLevel as f32 * 80.0 / 1000.0;
    }
}

/// Every active path (source to monitor) of the current display configuration.
fn active_paths() -> Vec<DISPLAYCONFIG_PATH_INFO> {
    let mut paths = Vec::new();
    let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = Vec::new();
    // The configuration can change between the size query and the query; retry a few times.
    for _ in 0..4 {
        let (mut path_count, mut mode_count) = (0, 0);
        // SAFETY: valid out pointers.
        if unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        } != ERROR_SUCCESS
        {
            break;
        }
        paths.clear();
        paths.reserve(path_count as usize);
        modes.clear();
        modes.reserve(mode_count as usize);
        // SAFETY: the arrays have room for the counts passed; QDC_ONLY_ACTIVE_PATHS takes no
        // topology out pointer.
        let result = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        if result == ERROR_SUCCESS {
            // SAFETY: the call initialized the first `path_count` elements (at most the capacity).
            unsafe { paths.set_len(path_count as usize) };
            return paths;
        }
        if result != ERROR_INSUFFICIENT_BUFFER {
            break;
        }
    }
    Vec::new()
}

fn source_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<[u16; 32]> {
    let mut name: DISPLAYCONFIG_SOURCE_DEVICE_NAME = request(
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        path.sourceInfo.adapterId,
        path.sourceInfo.id,
    );
    get(&mut name.header).then_some(name.viewGdiDeviceName)
}

/// A zeroed request packet of type `T` whose header asks for `kind` about one source or target.
fn request<T>(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, adapter: LUID, id: u32) -> T {
    const { assert!(size_of::<T>() >= size_of::<DISPLAYCONFIG_DEVICE_INFO_HEADER>()) };
    // SAFETY: the packets are plain C structs, valid when zeroed.
    let mut packet: T = unsafe { mem::zeroed() };
    let header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size_of::<T>() as u32,
        adapterId: adapter,
        id,
    };
    // SAFETY: every packet starts with its header, and `T` is at least that large.
    unsafe { ptr::write((&raw mut packet).cast(), header) };
    packet
}

fn get(header: &mut DISPLAYCONFIG_DEVICE_INFO_HEADER) -> bool {
    // SAFETY: `header` heads a packet of `header.size` bytes (see `request`).
    unsafe { DisplayConfigGetDeviceInfo(header) == ERROR_SUCCESS as i32 }
}

fn same_name(a: &[u16], b: &[u16]) -> bool {
    let a = &a[..a.iter().position(|&c| c == 0).unwrap_or(a.len())];
    let b = &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())];
    a == b
}

fn rect(r: RECT) -> Rect {
    Rect::new(
        r.left,
        r.top,
        (r.right - r.left).max(0) as u32,
        (r.bottom - r.top).max(0) as u32,
    )
}
