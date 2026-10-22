//! OWNER: host agent. A standard Win32 tooltip control used in tracking mode, so we can
//! position it above the pager ourselves.

use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PWSTR;

use crate::model::RectI;

pub struct Tooltip {
    hwnd: HWND,
    /// Window the tool is registered for (a stable, top-level, hidden window).
    owner: HWND,
    visible: bool,
    text: String,
}

impl Tooltip {
    pub fn new(owner: HWND) -> Option<Tooltip> {
        unsafe {
            let icc = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_WIN95_CLASSES,
            };
            let _ = InitCommonControlsEx(&icc);
            let hinst = GetModuleHandleW(None).ok()?;
            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                TOOLTIPS_CLASS,
                None,
                WINDOW_STYLE(WS_POPUP.0 | TTS_NOPREFIX | TTS_ALWAYSTIP),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                hinst,
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    crate::wlog!("tooltip: CreateWindowExW failed: {e}");
                    return None;
                }
            };
            let mut ti = Self::info(owner, std::ptr::null_mut());
            let r = SendMessageW(hwnd, TTM_ADDTOOLW, WPARAM(0), LPARAM(&mut ti as *mut _ as isize));
            if r.0 == 0 {
                crate::wlog!("tooltip: TTM_ADDTOOLW failed");
            }
            // Any max width enables multi-line text.
            SendMessageW(hwnd, TTM_SETMAXTIPWIDTH, WPARAM(0), LPARAM(480));
            Some(Tooltip { hwnd, owner, visible: false, text: String::new() })
        }
    }

    fn info(owner: HWND, text: *mut u16) -> TTTOOLINFOW {
        TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_TRACK | TTF_ABSOLUTE,
            hwnd: owner,
            uId: 1,
            lpszText: PWSTR(text),
            ..Default::default()
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Show (or update) the tooltip for a hover target centred (where possible) on screen x
    /// `anchor_x` of the pager at `pager` (screen rect). Placement: see [`place_tooltip`].
    pub fn show(&mut self, text: &str, anchor_x: i32, pager: &RectI, gap: i32) {
        unsafe {
            let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let mut ti = Self::info(self.owner, wide.as_mut_ptr());
            let pti = LPARAM(&mut ti as *mut _ as isize);
            if self.text != text {
                SendMessageW(self.hwnd, TTM_SETTOOLINFOW, WPARAM(0), pti);
                self.text = text.to_string();
            }
            let work = crate::taskbar::work_area_near(pager)
                .unwrap_or(RectI { left: -32000, top: -32000, right: 32000, bottom: 32000 });
            let size = SendMessageW(self.hwnd, TTM_GETBUBBLESIZE, WPARAM(0), pti).0 as u32;
            let mut dims = ((size & 0xFFFF) as i32, (size >> 16) as i32);
            self.place(pager, dims, anchor_x, &work, gap);
            if !self.visible {
                SendMessageW(self.hwnd, TTM_TRACKACTIVATE, WPARAM(1), pti);
                self.visible = true;
            }
            // The real window size is only certain once it is shown: correct once if it differs.
            let mut r = RECT::default();
            if GetWindowRect(self.hwnd, &mut r).is_ok() {
                let real = (r.right - r.left, r.bottom - r.top);
                if real.0 > 0 && real.1 > 0 && real != dims {
                    dims = real;
                    self.place(pager, dims, anchor_x, &work, gap);
                }
            }
        }
    }

    unsafe fn place(&self, pager: &RectI, size: (i32, i32), anchor_x: i32, work: &RectI, gap: i32) {
        let (x, y) = place_tooltip(pager, size, anchor_x, work, gap);
        let pos = ((y as i16 as u16 as u32) << 16) | (x as i16 as u16 as u32);
        unsafe {
            SendMessageW(self.hwnd, TTM_TRACKPOSITION, WPARAM(0), LPARAM(pos as isize));
        }
    }
    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        unsafe {
            let mut ti = Self::info(self.owner, std::ptr::null_mut());
            SendMessageW(self.hwnd, TTM_TRACKACTIVATE, WPARAM(0), LPARAM(&mut ti as *mut _ as isize));
        }
        self.visible = false;
    }
}

impl Drop for Tooltip {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Pure: top-left screen position of a tooltip of `size` for a pager at `pager`, hovered at
/// screen x `anchor_x`, inside the monitor work area `work`.
///
/// Above the pager is preferred (tooltip bottom `gap` above the pager top). If that would
/// cross the top of the work area it goes below (top `gap` below the pager bottom). If
/// neither side fits it takes the side with more room and is clamped into the work area.
/// Horizontally it is centred on `anchor_x` and clamped fully inside the work area (it hugs
/// the left edge if it is wider than the work area).
pub fn place_tooltip(pager: &RectI, size: (i32, i32), anchor_x: i32, work: &RectI, gap: i32) -> (i32, i32) {
    let (w, h) = (size.0.max(0), size.1.max(0));
    let x = (anchor_x - w / 2).min(work.right - w).max(work.left);
    let above_y = pager.top - gap - h;
    let below_y = pager.bottom + gap;
    let y = if above_y >= work.top {
        above_y
    } else if below_y + h <= work.bottom {
        below_y
    } else {
        let room_above = pager.top - gap - work.top;
        let room_below = work.bottom - below_y;
        let y = if room_above >= room_below { above_y } else { below_y };
        y.min(work.bottom - h).max(work.top)
    };
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: RectI = RectI { left: 0, top: 0, right: 3440, bottom: 1392 };

    fn pager(l: i32, t: i32) -> RectI {
        RectI { left: l, top: t, right: l + 500, bottom: t + 48 }
    }

    #[test]
    fn normal_placement_is_above_and_centred() {
        let p = pager(2000, 1344);
        assert_eq!(place_tooltip(&p, (100, 24), 2250, &WORK, 6), (2200, 1344 - 6 - 24));
        // Two lines are taller; the bottom edge stays at the same gap.
        let (_, y) = place_tooltip(&p, (180, 44), 2250, &WORK, 6);
        assert_eq!(y + 44, 1344 - 6);
    }

    #[test]
    fn top_of_screen_flips_below() {
        let p = pager(2000, 0);
        let (x, y) = place_tooltip(&p, (100, 44), 2250, &WORK, 6);
        assert_eq!((x, y), (2200, 48 + 6));
        // Just barely enough room above: stays above; one pixel less flips.
        let p = pager(2000, 50);
        assert_eq!(place_tooltip(&p, (100, 44), 2250, &WORK, 6).1, 0);
        let p = pager(2000, 49);
        assert_eq!(place_tooltip(&p, (100, 44), 2250, &WORK, 6).1, 49 + 48 + 6);
    }

    #[test]
    fn top_taskbar_work_area_flips_below() {
        // A top taskbar: work area starts below it, the pager is inside the taskbar.
        let work = RectI { left: 0, top: 48, right: 3440, bottom: 1440 };
        let p = pager(2000, 0);
        assert_eq!(place_tooltip(&p, (100, 24), 2250, &work, 6).1, 54);
    }

    #[test]
    fn horizontal_clamping() {
        let p = pager(0, 600);
        assert_eq!(place_tooltip(&p, (200, 24), 10, &WORK, 6).0, 0);
        let p = pager(2940, 600);
        assert_eq!(place_tooltip(&p, (200, 24), 3430, &WORK, 6).0, 3440 - 200);
        // A secondary monitor to the left (negative coordinates).
        let work = RectI { left: -1920, top: 0, right: 0, bottom: 1080 };
        let p = pager(-500, 500);
        assert_eq!(place_tooltip(&p, (200, 24), -5, &work, 6).0, -200);
        assert_eq!(place_tooltip(&p, (200, 24), -1915, &work, 6).0, -1920);
    }

    #[test]
    fn tiny_work_area_is_clamped_and_never_panics() {
        // Neither side fits: the side with more room wins and the result stays inside.
        let work = RectI { left: 0, top: 0, right: 150, bottom: 60 };
        let p = RectI { left: 10, top: 20, right: 140, bottom: 50 };
        let (x, y) = place_tooltip(&p, (100, 44), 75, &work, 6);
        assert!(x >= 0 && x + 100 <= 150, "x {x}");
        assert!(y >= 0 && y + 44 <= 60, "y {y}");
        // Tooltip larger than the work area: top-left wins.
        let (x, y) = place_tooltip(&p, (400, 200), 75, &work, 6);
        assert_eq!((x, y), (0, 0));
    }
}
