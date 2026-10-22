//! OWNER: data agent. Builds a `Snapshot` of desktops and windows.

use std::collections::HashSet;

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DWM_CLOAKED_SHELL, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::icons::IconCache;
use crate::model::{Hwnd, RectI, Snapshot, WindowInfo, from_hwnd, to_hwnd};

/// Primary monitor rectangle in physical pixels.
pub fn primary_screen() -> RectI {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let r = mi.rcMonitor;
            return RectI { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
        }
        RectI { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) }
    }
}

fn rect_i(r: RECT) -> RectI {
    RectI { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let v = unsafe { &mut *(lparam.0 as *mut Vec<Hwnd>) };
    v.push(from_hwnd(hwnd));
    true.into()
}

/// Top-level windows, topmost first.
fn top_level_windows() -> Vec<Hwnd> {
    let mut v: Vec<Hwnd> = Vec::with_capacity(256);
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut v as *mut Vec<Hwnd> as isize));
    }
    v
}

fn window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// Visible frame bounds (without the invisible resize border), falling back to the window rect.
fn frame_rect(hwnd: HWND) -> Option<RectI> {
    unsafe {
        let mut r = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok()
        {
            return Some(rect_i(r));
        }
        GetWindowRect(hwnd, &mut r).ok().map(|_| rect_i(r))
    }
}

/// Cheap, winvd-free part of the Alt-Tab filter.
fn is_alt_tab_candidate(hwnd: HWND) -> Option<bool> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        let app = ex.contains(WS_EX_APPWINDOW);
        if ex.contains(WS_EX_TOOLWINDOW) && !app {
            return None;
        }
        if ex.contains(WS_EX_NOACTIVATE) {
            return None;
        }
        if !app && GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.0.is_null()) {
            return None;
        }
        // CoreWindows are the (top-level) content windows of packaged apps; the user-visible
        // window is the ApplicationFrameWindow that hosts them.
        let mut cls = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut cls).max(0) as usize;
        if String::from_utf16_lossy(&cls[..n]) == "Windows.UI.Core.CoreWindow" {
            return None;
        }
        // DWM cloaking: 0 = visible, SHELL (2) = on another virtual desktop (wanted);
        // APP (1) and INHERITED (4) are hidden by the app / its owner (skip).
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
            && cloaked != DWM_CLOAKED_SHELL
        {
            return None;
        }
        Some(cloaked == 0)
    }
}

/// Snapshot of all desktops and of the windows a user would see in Alt-Tab, skipping
/// minimized windows, windows not on the primary monitor, and windows in `exclude`.
///
/// Cost: dominated by winvd COM calls. The desktop list is cached by `vd::desktops` while a
/// `vd::Listener` is alive; pin queries (the expensive per-window calls) are only made for
/// windows visible on the current desktop. Per-window desktop lookups are cheap (~30 us)
/// and not cached, so membership is always current.
pub fn build_snapshot(icons: &mut IconCache, exclude: &[Hwnd]) -> anyhow::Result<Snapshot> {
    let (desktops, current) = crate::vd::desktops()?;
    let screen = primary_screen();

    let all = top_level_windows();
    let alive: HashSet<Hwnd> = all.iter().copied().collect();

    let fg_root = unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() { HWND::default() } else { GetAncestor(fg, GA_ROOTOWNER) }
    };

    let mut windows = Vec::new();
    for &h in &all {
        if exclude.contains(&h) {
            continue;
        }
        let hwnd = to_hwnd(h);
        let Some(visible_here) = is_alt_tab_candidate(hwnd) else { continue };
        let Some(rect) = frame_rect(hwnd) else { continue };
        if rect.width() <= 0 || rect.height() <= 0 || !rect.intersects(&screen) {
            continue;
        }

        // Windows the desktop manager doesn't know (Program Manager, overlays, ...) fail here.
        let Ok(desk) = winvd::get_desktop_by_window(hwnd) else { continue };
        let Ok(desk_id) = desk.get_id() else { continue };
        // A pinned window is shown on every desktop, so it is never shell-cloaked. Only
        // windows visible right now (cloak == 0) can be pinned; skipping the two pin
        // queries (~0.15 ms each) for everything else saves most of the per-window cost.
        let pinned = visible_here
            && (winvd::is_pinned_window(hwnd).unwrap_or(false)
                || winvd::is_pinned_app(hwnd).unwrap_or(false));

        let is_foreground = !fg_root.0.is_null() && unsafe { GetAncestor(hwnd, GA_ROOTOWNER) } == fg_root;

        windows.push(WindowInfo {
            hwnd: h,
            desktop: if pinned { None } else { Some(desk_id) },
            rect,
            title: window_title(hwnd),
            icon: icons.get(h),
            is_foreground,
        });
    }
    icons.retain(&alive);

    Ok(Snapshot { desktops, current, screen, windows })
}
