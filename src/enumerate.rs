//! OWNER: data agent. Builds a `Snapshot` of desktops and windows.

use crate::icons::IconCache;
use crate::model::{Hwnd, RectI, Snapshot};

/// Primary monitor rectangle in physical pixels.
pub fn primary_screen() -> RectI {
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        RectI {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXSCREEN),
            bottom: GetSystemMetrics(SM_CYSCREEN),
        }
    }
}

/// Snapshot of all desktops and of the windows a user would see in Alt-Tab, skipping
/// minimized windows, windows not on the primary monitor, and windows in `exclude`.
pub fn build_snapshot(icons: &mut IconCache, exclude: &[Hwnd]) -> anyhow::Result<Snapshot> {
    // STUB: desktops only, no windows.
    let _ = (icons, exclude);
    let (desktops, current) = crate::vd::desktops()?;
    Ok(Snapshot { desktops, current, screen: primary_screen(), windows: Vec::new() })
}
