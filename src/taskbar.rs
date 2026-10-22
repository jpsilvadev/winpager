//! OWNER: host agent. Locating the taskbar and placing the pager in/over it.

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::config::Anchor;
use crate::model::{RectI, from_hwnd};

/// Which monitor edge the taskbar sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Bottom,
    Top,
}

/// Snapshot of the taskbar geometry (screen coordinates, physical pixels).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskbarInfo {
    pub hwnd: isize,
    pub rect: RectI,
    pub notify: Option<RectI>,
    pub rebar: Option<RectI>,
    pub dpi: u32,
    pub edge: Edge,
}

impl TaskbarInfo {
    pub fn hwnd(&self) -> HWND {
        crate::model::to_hwnd(self.hwnd)
    }

    /// Taskbar looks auto-hidden (collapsed to a sliver at the screen edge).
    pub fn is_collapsed(&self) -> bool {
        self.rect.height() < (self.dpi as i32 * 20 / 96)
    }
}

fn to_rect(r: RECT) -> RectI {
    RectI { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

fn window_rect(hwnd: HWND) -> Option<RectI> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r).ok()?; }
    Some(to_rect(r))
}

/// Class name of a window (empty on failure).
pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Find the primary taskbar and its relevant children.
pub fn find() -> Option<TaskbarInfo> {
    unsafe {
        let tray = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok()?;
        let rect = window_rect(tray)?;
        let child = |class: PCWSTR| -> Option<RectI> {
            let h = FindWindowExW(tray, None, class, PCWSTR::null()).ok()?;
            if !IsWindowVisible(h).as_bool() {
                return None;
            }
            window_rect(h).filter(|r| r.width() > 0 && r.height() > 0)
        };
        let notify = child(w!("TrayNotifyWnd"));
        let rebar = child(w!("ReBarWindow32"));
        let mut dpi = GetDpiForWindow(tray);
        if dpi == 0 {
            dpi = 96;
        }
        let mon = MonitorFromWindow(tray, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let edge = if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let mid = (mi.rcMonitor.top + mi.rcMonitor.bottom) / 2;
            if rect.top + rect.height() / 2 < mid { Edge::Top } else { Edge::Bottom }
        } else {
            Edge::Bottom
        };
        Some(TaskbarInfo { hwnd: from_hwnd(tray), rect, notify, rebar, dpi, edge })
    }
}

/// Screen rectangle for a pager `width_px` wide: full taskbar height, anchored per config.
pub fn pager_rect(info: &TaskbarInfo, anchor: Anchor, offset_dip: i32, width_px: i32) -> RectI {
    let off = offset_dip * info.dpi as i32 / 96;
    let left = match anchor {
        Anchor::Tray => {
            let right = info.notify.map_or(info.rect.right, |n| n.left) - off;
            right - width_px
        }
        Anchor::Left => info.rect.left + off,
    };
    // Never leave the taskbar horizontally.
    let left = left.clamp(info.rect.left, (info.rect.right - width_px).max(info.rect.left));
    RectI { left, top: info.rect.top, right: left + width_px, bottom: info.rect.bottom }
}

/// True when `pager` covers part of the task-button area (informational only).
pub fn overlaps_task_buttons(info: &TaskbarInfo, pager: &RectI) -> bool {
    info.rebar.is_some_and(|r| r.intersects(pager))
}

/// Screen rect -> client rect of `parent`.
pub fn screen_to_client(parent: HWND, r: &RectI) -> RectI {
    let mut tl = POINT { x: r.left, y: r.top };
    unsafe {
        let _ = ScreenToClient(parent, &mut tl);
    }
    RectI { left: tl.x, top: tl.y, right: tl.x + r.width(), bottom: tl.y + r.height() }
}

/// Registered message Explorer broadcasts after (re)creating the taskbar.
pub fn taskbar_created_message() -> u32 {
    unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }
}
