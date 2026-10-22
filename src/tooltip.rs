//! OWNER: host agent. A standard Win32 tooltip control used in tracking mode, so we can
//! position it above the pager ourselves.

use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PWSTR;

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

    /// Show (or update) the tooltip, horizontally centered on `center_x`. With `above`
    /// its bottom edge is at `anchor_y`, otherwise its top edge (screen coordinates).
    pub fn show(&mut self, text: &str, center_x: i32, anchor_y: i32, above: bool) {
        unsafe {
            let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let mut ti = Self::info(self.owner, wide.as_mut_ptr());
            let pti = LPARAM(&mut ti as *mut _ as isize);
            if self.text != text {
                SendMessageW(self.hwnd, TTM_SETTOOLINFOW, WPARAM(0), pti);
                self.text = text.to_string();
            }
            let size = SendMessageW(self.hwnd, TTM_GETBUBBLESIZE, WPARAM(0), pti).0 as u32;
            let (w, h) = ((size & 0xFFFF) as i32, (size >> 16) as i32);
            let x = center_x - w / 2;
            let y = if above { anchor_y - h } else { anchor_y };
            let pos = ((y as i16 as u16 as u32) << 16) | (x as i16 as u16 as u32);
            SendMessageW(self.hwnd, TTM_TRACKPOSITION, WPARAM(0), LPARAM(pos as isize));
            if !self.visible {
                SendMessageW(self.hwnd, TTM_TRACKACTIVATE, WPARAM(1), pti);
                self.visible = true;
            }
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
