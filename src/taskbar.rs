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
        Some(TaskbarInfo { hwnd: from_hwnd(tray), rect, notify, dpi, edge })
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

/// Pure: default top-left corner of the floating widget, just above the taskbar (below it
/// for a top taskbar). `Tray` anchor right-aligns the widget `offset` px left of `tray_left`;
/// `Left` puts it `offset` px from the taskbar's left edge. `gap` is the clearance to the
/// taskbar.
#[allow(clippy::too_many_arguments)]
pub fn default_floating_pos(
    taskbar: &RectI,
    edge: Edge,
    anchor: Anchor,
    tray_left: i32,
    offset: i32,
    gap: i32,
    width: i32,
    height: i32,
) -> (i32, i32) {
    let x = match anchor {
        Anchor::Tray => tray_left - offset - width,
        Anchor::Left => taskbar.left + offset,
    };
    let y = match edge {
        Edge::Bottom => taskbar.top - gap - height,
        Edge::Top => taskbar.bottom + gap,
    };
    (x, y)
}

/// Pure: move a `w` x `h` rect at (`x`, `y`) so it lies inside `work` (top-left wins when the
/// rect is larger than the work area). Returns the new top-left corner.
pub fn clamp_into(x: i32, y: i32, w: i32, h: i32, work: &RectI) -> (i32, i32) {
    let cx = x.min(work.right - w).max(work.left);
    let cy = y.min(work.bottom - h).max(work.top);
    (cx, cy)
}

/// Work area of the monitor nearest to `rect` (screen coordinates).
pub fn work_area_near(rect: &RectI) -> Option<RectI> {
    unsafe {
        let r = RECT { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
        let mon = MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() { Some(to_rect(mi.rcWork)) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb() -> RectI {
        RectI { left: 0, top: 1392, right: 3440, bottom: 1440 }
    }

    #[test]
    fn default_position_is_above_the_taskbar_right_aligned_with_the_tray() {
        let p = default_floating_pos(&tb(), Edge::Bottom, Anchor::Tray, 2830, 8, 6, 600, 48);
        assert_eq!(p, (2830 - 8 - 600, 1392 - 6 - 48));
        let p = default_floating_pos(&tb(), Edge::Bottom, Anchor::Left, 2830, 8, 6, 600, 48);
        assert_eq!(p, (8, 1338));
        // Top taskbar: below it.
        let top = RectI { left: 0, top: 0, right: 3440, bottom: 48 };
        let p = default_floating_pos(&top, Edge::Top, Anchor::Tray, 2830, 8, 6, 600, 48);
        assert_eq!(p, (2222, 54));
    }

    #[test]
    fn clamping_keeps_the_widget_on_screen() {
        let work = RectI { left: 0, top: 0, right: 1920, bottom: 1040 };
        // Already inside: unchanged.
        assert_eq!(clamp_into(100, 200, 600, 48, &work), (100, 200));
        // Off to the right/bottom, e.g. a monitor was unplugged.
        assert_eq!(clamp_into(3000, 1500, 600, 48, &work), (1320, 992));
        // Off to the left/top (also negative secondary monitor coordinates).
        assert_eq!(clamp_into(-500, -60, 600, 48, &work), (0, 0));
        let left_mon = RectI { left: -1920, top: 0, right: 0, bottom: 1080 };
        assert_eq!(clamp_into(-100, 1070, 600, 48, &left_mon), (-600, 1032));
        // Larger than the work area: top-left wins.
        assert_eq!(clamp_into(50, 50, 2500, 48, &work), (0, 50));
    }
}
