//! OWNER: data agent. Window icon extraction to premultiplied BGRA pixels, with caching.
//!
//! Resolution order for a window (first acceptable result wins):
//! 1. Packaged (UWP) apps hosted by `ApplicationFrameHost`: the app's AppUserModelID
//!    -> `shell:AppsFolder\<AUMID>` via `IShellItemImageFactory`.
//! 2. The icon the window itself provides (`WM_GETICON` big / small2 / small, then the
//!    window class icons), if it is at least 32 px wide.
//! 3. Otherwise the AppUserModelID (packaged apps) or the process executable through
//!    `IShellItemImageFactory`, if that gives at least 32 px; otherwise the small window
//!    icon, otherwise whatever the shell gave us.
//!
//! Caches: per hwnd (with back-off retries for failures and for results that came from
//! a fallback, since apps often set their icon after the window appears), per exe/AUMID
//! (so many windows of one app share one image), and pixel-identical images are shared
//! through a content-hash table so renderers can cache GPU bitmaps by `IconImage::id`.
//!
//! Must be used from a COM-initialized (STA) thread for the shell paths to work; without
//! COM they simply fail and the window icon path is used.

#![allow(clippy::chunks_exact_to_as_chunks, clippy::needless_range_loop)]

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyStore, PROPERTYKEY, SHGetPropertyStoreForWindow,
};
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GCLP_HICON, GCLP_HICONSM, GetClassLongPtrW, GetClassNameW, GetIconInfo,
    GetWindowThreadProcessId, HICON, ICONINFO, IsHungAppWindow, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    SendMessageTimeoutW, WM_GETICON,
};
use windows::core::{GUID, HSTRING, PCWSTR, PWSTR};

use crate::model::{Hwnd, IconImage, to_hwnd};

/// Preferred minimum width; windows icons smaller than this trigger the exe/AUMID lookup.
const WANT_PX: u32 = 32;
/// Images are downscaled to at most this size.
const MAX_PX: u32 = 64;
/// Size requested from `IShellItemImageFactory`.
const SHELL_PX: i32 = 48;
/// Max time spent in one `WM_GETICON` before giving up on a window.
const MSG_TIMEOUT_MS: u32 = 50;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// `PKEY_AppUserModel_ID` (defined here to avoid pulling the EnhancedStorage feature).
const PKEY_APPUSERMODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};

/// Premultiplied BGRA pixels, top-down.
struct Pixels {
    w: u32,
    h: u32,
    data: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The window's own icon; considered final.
    Window,
    /// Shell/exe fallback or nothing; re-checked with back-off.
    Fallback,
}

struct Entry {
    icon: Option<Arc<IconImage>>,
    source: Source,
    pid: u32,
    attempts: u32,
    next_check: Instant,
}

struct KeyEntry {
    icon: Option<Arc<IconImage>>,
    retry_at: Instant,
}

pub struct IconCache {
    by_hwnd: HashMap<Hwnd, Entry>,
    by_key: HashMap<String, KeyEntry>,
    by_hash: HashMap<(u64, u32, u32), Weak<IconImage>>,
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new()
    }
}

impl IconCache {
    pub fn new() -> Self {
        Self { by_hwnd: HashMap::new(), by_key: HashMap::new(), by_hash: HashMap::new() }
    }

    /// Icon for a top-level window, ideally >= 32x32. Cached; must be cheap on cache hit.
    pub fn get(&mut self, hwnd: Hwnd) -> Option<Arc<IconImage>> {
        let now = Instant::now();
        let pid = pid_of(hwnd);
        if let Some(e) = self.by_hwnd.get(&hwnd)
            && e.pid == pid
            && (e.source == Source::Window || now < e.next_check)
        {
            return e.icon.clone();
        }

        let (icon, source) = self.resolve(hwnd, pid);
        let (old_icon, attempts) = match self.by_hwnd.get(&hwnd) {
            Some(e) if e.pid == pid => (e.icon.clone(), e.attempts),
            _ => (None, 0),
        };
        // Never lose a working icon to a transient failure.
        let icon = icon.or(old_icon);
        let attempts = if source == Source::Window { 0 } else { attempts + 1 };
        let delay = Duration::from_secs(1u64 << attempts.min(6)).min(Duration::from_secs(60));
        self.by_hwnd
            .insert(hwnd, Entry { icon: icon.clone(), source, pid, attempts, next_check: now + delay });
        icon
    }

    /// Drop cache entries for windows that no longer exist.
    pub fn retain(&mut self, alive: &HashSet<Hwnd>) {
        self.by_hwnd.retain(|h, _| alive.contains(h));
        self.by_hash.retain(|_, w| w.strong_count() > 0);
    }

    // ---- resolution -------------------------------------------------------------------

    fn resolve(&mut self, hwnd: Hwnd, pid: u32) -> (Option<Arc<IconImage>>, Source) {
        let class = class_name(hwnd);
        let is_uwp_frame = class == "ApplicationFrameWindow";

        // 1. Packaged app behind ApplicationFrameHost.
        if is_uwp_frame && let Some(aumid) = uwp_aumid(hwnd, pid) {
            let icon = self.key_icon(&format!("aumid:{aumid}"), || {
                shell_image(&format!("shell:AppsFolder\\{aumid}"))
            });
            if icon.is_some() {
                return (icon, Source::Window);
            }
        }

        // 2. Window-provided icon.
        let win = window_icon(hwnd);
        if let Some(p) = &win
            && p.w >= WANT_PX
        {
            return (self.share(win.unwrap()), Source::Window);
        }

        // 3. AUMID / exe via the shell.
        let fallback = if is_uwp_frame { None } else { self.shell_fallback(hwnd, pid) };
        match (win, fallback) {
            (Some(w), Some(f)) => {
                // Prefer the bigger one; ties go to the window's own icon.
                if f.width >= w.w {
                    (Some(f), Source::Fallback)
                } else {
                    (self.share(w), Source::Fallback)
                }
            }
            (Some(w), None) => (self.share(w), Source::Fallback),
            (None, f) => (f, Source::Fallback),
        }
    }

    fn shell_fallback(&mut self, hwnd: Hwnd, pid: u32) -> Option<Arc<IconImage>> {
        let aumid = window_aumid(hwnd);
        let exe = exe_path(pid);
        if let Some(a) = aumid.as_deref()
            && a.contains('!')
            && let Some(i) = self.aumid_icon(a)
        {
            return Some(i);
        }
        if let Some(exe) = exe.as_deref()
            && let Some(i) = self.key_icon(&format!("exe:{}", exe.to_lowercase()), || shell_image(exe))
        {
            return Some(i);
        }
        if let Some(a) = aumid.as_deref()
            && !a.contains('!')
        {
            return self.aumid_icon(a);
        }
        None
    }

    fn aumid_icon(&mut self, aumid: &str) -> Option<Arc<IconImage>> {
        self.key_icon(&format!("aumid:{aumid}"), || shell_image(&format!("shell:AppsFolder\\{aumid}")))
    }

    /// Cached lookup by exe / AUMID; failures are remembered for a while.
    fn key_icon(&mut self, key: &str, f: impl FnOnce() -> Option<Pixels>) -> Option<Arc<IconImage>> {
        let now = Instant::now();
        if let Some(e) = self.by_key.get(key)
            && (e.icon.is_some() || now < e.retry_at)
        {
            return e.icon.clone();
        }
        let icon = f().and_then(|p| self.share(p));
        self.by_key.insert(
            key.to_string(),
            KeyEntry { icon: icon.clone(), retry_at: now + Duration::from_secs(30) },
        );
        icon
    }

    /// Turn pixels into a shared `IconImage` (identical pixels reuse one image/id).
    fn share(&mut self, p: Pixels) -> Option<Arc<IconImage>> {
        if p.w == 0 || p.h == 0 {
            return None;
        }
        let mut hasher = DefaultHasher::new();
        p.data.hash(&mut hasher);
        let key = (hasher.finish(), p.w, p.h);
        if let Some(a) = self.by_hash.get(&key).and_then(Weak::upgrade) {
            return Some(a);
        }
        let img = Arc::new(IconImage {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            width: p.w,
            height: p.h,
            bgra_premul: p.data,
        });
        self.by_hash.insert(key, Arc::downgrade(&img));
        Some(img)
    }
}

// ---- window / process helpers ---------------------------------------------------------

fn pid_of(hwnd: Hwnd) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(to_hwnd(hwnd), Some(&mut pid)) };
    pid
}

fn class_name(hwnd: Hwnd) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(to_hwnd(hwnd), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn exe_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Explicit AppUserModelID of a window (from its property store), if any.
fn window_aumid(hwnd: Hwnd) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(to_hwnd(hwnd)).ok()?;
        let mut pv = store.GetValue(&PKEY_APPUSERMODEL_ID).ok()?;
        let s = PropVariantToStringAlloc(&pv).ok();
        let out = s.and_then(|p| {
            let st = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const c_void));
            st
        });
        let _ = PropVariantClear(&mut pv);
        out.filter(|s| !s.is_empty())
    }
}

/// AUMID of the app hosted in an `ApplicationFrameWindow`.
fn uwp_aumid(frame: Hwnd, frame_pid: u32) -> Option<String> {
    if let Some(a) = window_aumid(frame) {
        return Some(a);
    }
    // Find the child CoreWindow that belongs to the real app process.
    struct Ctx {
        frame_pid: u32,
        found: Option<(Hwnd, u32)>,
    }
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> windows::Win32::Foundation::BOOL {
        let ctx = unsafe { &mut *(lp.0 as *mut Ctx) };
        let hw = h.0 as isize;
        let pid = pid_of(hw);
        if pid != ctx.frame_pid && class_name(hw) == "Windows.UI.Core.CoreWindow" {
            ctx.found = Some((hw, pid));
            return false.into();
        }
        true.into()
    }
    let mut ctx = Ctx { frame_pid, found: None };
    unsafe {
        let _ = EnumChildWindows(to_hwnd(frame), Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    let (core, pid) = ctx.found?;
    window_aumid(core).or_else(|| process_aumid(pid))
}

fn process_aumid(pid: u32) -> Option<String> {
    use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut len = 0u32;
        let _ = GetApplicationUserModelId(h, &mut len, PWSTR::null());
        let out = if len > 1 {
            let mut buf = vec![0u16; len as usize];
            let r = GetApplicationUserModelId(h, &mut len, PWSTR(buf.as_mut_ptr()));
            (r.0 == 0).then(|| String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]))
        } else {
            None
        };
        let _ = CloseHandle(h);
        out
    }
}

// ---- window icon ----------------------------------------------------------------------

/// Best icon the window itself offers. Blocks at most `MSG_TIMEOUT_MS` once, and not at
/// all for windows already known to be hung.
fn window_icon(hwnd: Hwnd) -> Option<Pixels> {
    let h = to_hwnd(hwnd);
    let mut best: Option<Pixels> = None;
    let consider = |hicon: usize, best: &mut Option<Pixels>| {
        if hicon == 0 {
            return;
        }
        if let Some(p) = hicon_pixels(HICON(hicon as *mut c_void))
            && best.as_ref().is_none_or(|b| p.w > b.w)
        {
            *best = Some(p);
        }
    };

    if !unsafe { IsHungAppWindow(h) }.as_bool() {
        // ICON_BIG = 1, ICON_SMALL2 = 2, ICON_SMALL = 0
        for which in [1usize, 2, 0] {
            let mut res = 0usize;
            let r = unsafe {
                SendMessageTimeoutW(
                    h,
                    WM_GETICON,
                    WPARAM(which),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    MSG_TIMEOUT_MS,
                    Some(&mut res),
                )
            };
            if r.0 == 0 {
                // Timed out / hung: don't wait again for this window.
                break;
            }
            consider(res, &mut best);
            if best.as_ref().is_some_and(|b| b.w >= WANT_PX) {
                return best;
            }
        }
    }
    for idx in [GCLP_HICON, GCLP_HICONSM] {
        let v = unsafe { GetClassLongPtrW(h, idx) };
        consider(v, &mut best);
        if best.as_ref().is_some_and(|b| b.w >= WANT_PX) {
            break;
        }
    }
    best
}

// ---- pixel conversion -----------------------------------------------------------------

/// Read a bitmap as raw 32bpp top-down BGRA (whatever alpha the bitmap has).
unsafe fn bitmap_bgra(hbm: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut bm = BITMAP::default();
        let n = GetObjectW(hbm, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void));
        if n == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(dc, hbm, 0, h as u32, Some(buf.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        (lines != 0).then_some((w as u32, h as u32, buf))
    }
}

fn premultiply(data: &mut [u8]) {
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 255 {
            px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
            px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
            px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
        }
    }
}

/// Convert an HICON (not consumed, not destroyed) to premultiplied pixels.
fn hicon_pixels(icon: HICON) -> Option<Pixels> {
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(icon, &mut ii).ok()?;
        let result = (|| {
            if ii.hbmColor.is_invalid() {
                return None; // monochrome icons: not worth supporting
            }
            let (w, h, mut data) = bitmap_bgra(ii.hbmColor)?;
            if data.chunks_exact(4).all(|p| p[3] == 0) {
                // No alpha channel: build it from the AND mask (0 = opaque).
                let mask = if ii.hbmMask.is_invalid() { None } else { bitmap_bgra(ii.hbmMask) };
                match mask {
                    Some((mw, mh, m)) if mw == w && mh == h => {
                        for (px, mp) in data.chunks_exact_mut(4).zip(m.chunks_exact(4)) {
                            px[3] = if mp[0] == 0 && mp[1] == 0 && mp[2] == 0 { 255 } else { 0 };
                        }
                    }
                    _ => data.chunks_exact_mut(4).for_each(|px| px[3] = 255),
                }
            }
            premultiply(&mut data);
            Some(downscale(Pixels { w, h, data }))
        })();
        let _ = DeleteObject(ii.hbmColor);
        let _ = DeleteObject(ii.hbmMask);
        result
    }
}

/// Icon/thumbnail from the shell for a file path or `shell:AppsFolder\...` name.
fn shell_image(parsing_name: &str) -> Option<Pixels> {
    unsafe {
        let name = HSTRING::from(parsing_name);
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None::<&windows::Win32::System::Com::IBindCtx>).ok()?;
        let hbm = factory.GetImage(SIZE { cx: SHELL_PX, cy: SHELL_PX }, SIIGBF_ICONONLY).ok()?;
        let raw = bitmap_bgra(hbm);
        let _ = DeleteObject(hbm);
        let (w, h, mut data) = raw?;
        if data.chunks_exact(4).all(|p| p[3] == 0) {
            data.chunks_exact_mut(4).for_each(|px| px[3] = 255);
        } else if data.chunks_exact(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]) {
            // Some colour channel exceeds alpha, so it cannot be premultiplied already.
            premultiply(&mut data);
        }
        Some(downscale(Pixels { w, h, data }))
    }
}

/// Box-filter downscale (premultiplied data) so the longest side is at most `MAX_PX`.
fn downscale(p: Pixels) -> Pixels {
    let m = p.w.max(p.h);
    if m <= MAX_PX {
        return p;
    }
    let nw = (p.w * MAX_PX / m).max(1);
    let nh = (p.h * MAX_PX / m).max(1);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        let y0 = y * p.h / nh;
        let y1 = ((y + 1) * p.h / nh).max(y0 + 1).min(p.h);
        for x in 0..nw {
            let x0 = x * p.w / nw;
            let x1 = ((x + 1) * p.w / nw).max(x0 + 1).min(p.w);
            let mut acc = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy * p.w + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += p.data[i + c] as u32;
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let o = ((y * nw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = ((acc[c] + n / 2) / n) as u8;
            }
        }
    }
    Pixels { w: nw, h: nh, data: out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

    #[test]
    fn shell_image_for_exe_and_aumid() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let p = shell_image("C:\\Windows\\System32\\notepad.exe").expect("exe icon");
        println!("notepad: {}x{}", p.w, p.h);
        assert!(p.w >= 32);
        assert!(p.data.chunks_exact(4).any(|px| px[3] > 0));
        // premultiplied invariant
        assert!(p.data.chunks_exact(4).all(|px| px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]));
        assert!(shell_image("C:\\does\\not\\exist.exe").is_none());
    }

    #[test]
    fn downscale_limits_size() {
        let p = Pixels { w: 256, h: 128, data: vec![255; 256 * 128 * 4] };
        let d = downscale(p);
        assert_eq!((d.w, d.h), (64, 32));
        assert!(d.data.iter().all(|&b| b == 255));
    }
}
