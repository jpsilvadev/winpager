//! OWNER: host agent. Measures the free space inside the Windows 11 taskbar with UI Automation.
//!
//! The taskbar is XAML, so `ReBarWindow32`/`TrayNotifyWnd` rects are stale. UI Automation
//! reports exact rects for the task buttons and the tray items. All UIA calls run on a
//! dedicated MTA thread (never on the UI thread: our pager is a cross-process child of the
//! taskbar and shares its input queue, so blocking cross-process calls there could deadlock).
//!
//! The thread polls about once a second (and immediately on `request_refresh`), publishes the
//! result in shared state and posts a message to a window only when the result changes.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
use windows::core::{PCWSTR, VARIANT, w};

const POLL: Duration = Duration::from_millis(1000);
/// Consecutive failed polls before the space is reported as unavailable.
const FAIL_LIMIT: u32 = 3;

pub const TASK_BUTTON_CLASS: &str = "Taskbar.TaskListButtonAutomationPeer";
pub const TRAY_CLASS_PREFIX: &str = "SystemTray.";
const FIXED_BUTTON_IDS: [&str; 3] = ["StartButton", "SearchButton", "TaskViewButton"];
const WIDGETS_ID: &str = "WidgetsButton";

/// Horizontal extents (screen pixels) of the taskbar's parts. `None` = not found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Space {
    /// (left, right) of the Start/Search/TaskView/task-list button group.
    pub buttons: Option<(i32, i32)>,
    /// (left, right) of the Widgets button.
    pub widgets: Option<(i32, i32)>,
    /// Left edge of the system tray (first `SystemTray.*` element).
    pub tray_left: Option<i32>,
}

/// One automation element, reduced to what we need.
#[derive(Clone, Debug)]
pub struct Elem {
    pub class: String,
    pub id: String,
    pub rect: (i32, i32, i32, i32),
}

/// Pure: classify elements into a [`Space`].
pub fn compute_space(elems: &[Elem]) -> Space {
    let mut s = Space::default();
    for e in elems {
        let (l, _t, r, _b) = e.rect;
        // Ignore empty or off-screen (hidden) elements.
        if r <= l || l <= -10000 {
            continue;
        }
        if e.class == TASK_BUTTON_CLASS || FIXED_BUTTON_IDS.contains(&e.id.as_str()) {
            s.buttons = Some(match s.buttons {
                Some((a, b)) => (a.min(l), b.max(r)),
                None => (l, r),
            });
        } else if e.id == WIDGETS_ID {
            s.widgets = Some((l, r));
        } else if e.class.starts_with(TRAY_CLASS_PREFIX) {
            s.tray_left = Some(s.tray_left.map_or(l, |t| t.min(l)));
        }
    }
    s
}

/// The free span (screen x range, `left..right`) available for the pager.
///
/// `gap` is the clearance kept to neighbouring elements and `offset` the user's distance from
/// the anchor, both in pixels. Falls back to the plain taskbar edges for missing parts.
pub fn free_span(
    anchor_tray: bool,
    space: &Space,
    taskbar: (i32, i32),
    gap: i32,
    offset: i32,
) -> (i32, i32) {
    if anchor_tray {
        let left = space.buttons.map_or(taskbar.0, |b| b.1 + gap);
        let right = space.tray_left.unwrap_or(taskbar.1) - offset;
        (left, right.max(left))
    } else {
        let left = match space.widgets {
            Some(w) => w.1 + gap,
            None => taskbar.0 + offset,
        };
        let right = space.buttons.map_or(taskbar.1, |b| b.0 - gap);
        (left, right.max(left))
    }
}

/// Pure: x range of the pager of `width` for `span`: right-aligned for the tray anchor,
/// left-aligned otherwise. The width is NOT clamped (the layout is responsible for staying
/// within the span, see `Layout::compute`'s `max_width_px`); if it is wider anyway, it
/// overflows away from the anchor.
pub fn place_in_span(anchor_tray: bool, span: (i32, i32), width: i32) -> (i32, i32) {
    let w = width.max(0);
    if anchor_tray { (span.1 - w, span.1) } else { (span.0, span.0 + w) }
}

// ---------------------------------------------------------------------------------------
// Background thread
// ---------------------------------------------------------------------------------------

struct Shared {
    state: Mutex<Option<Space>>,
    stop: AtomicBool,
    wake: Mutex<bool>,
    cv: Condvar,
    /// Duration of the last poll in microseconds.
    last_us: AtomicU64,
}

/// Handle to the UIA polling thread. Dropping it stops the thread.
pub struct SpaceMonitor {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl SpaceMonitor {
    /// Start polling. On a change of the measured space, `msg` is posted to `notify`.
    pub fn start(notify: isize, msg: u32) -> SpaceMonitor {
        let shared = Arc::new(Shared {
            state: Mutex::new(None),
            stop: AtomicBool::new(false),
            wake: Mutex::new(false),
            cv: Condvar::new(),
            last_us: AtomicU64::new(0),
        });
        let s2 = shared.clone();
        let thread = std::thread::Builder::new()
            .name("winpager-uia".into())
            .spawn(move || worker(s2, notify, msg))
            .map_err(|e| crate::wlog!("taskbar_space: cannot spawn thread: {e}"))
            .ok();
        SpaceMonitor { shared, thread }
    }

    /// Latest measurement; `None` while UIA is unavailable (or before the first poll).
    pub fn current(&self) -> Option<Space> {
        self.shared.state.lock().ok().and_then(|g| *g)
    }

    /// Poll again right now (e.g. after `TaskbarCreated`).
    pub fn request_refresh(&self) {
        if let Ok(mut g) = self.shared.wake.lock() {
            *g = true;
            self.shared.cv.notify_all();
        }
    }

    /// Microseconds the last poll took.
    pub fn last_poll_micros(&self) -> u64 {
        self.shared.last_us.load(Ordering::Relaxed)
    }
}

impl Drop for SpaceMonitor {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Ok(mut g) = self.shared.wake.lock() {
            *g = true;
            self.shared.cv.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Uia {
    automation: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    cond: IUIAutomationCondition,
    root: Option<IUIAutomationElement>,
    /// Containers whose direct children are the interesting elements (the `TaskbarFrame`
    /// holding Widgets/Start/Search/TaskView/task buttons, and the parent of the tray
    /// buttons). Scanning only their children is ~7x cheaper than walking the whole tree.
    /// Empty = fall back to a full descendant scan from the root.
    containers: Vec<IUIAutomationElement>,
}

impl Uia {
    unsafe fn new(cond_all: bool) -> windows::core::Result<Uia> {
        unsafe {
            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let cache = automation.CreateCacheRequest()?;
            cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
            cache.AddProperty(UIA_ClassNamePropertyId)?;
            cache.AddProperty(UIA_AutomationIdPropertyId)?;
            let cond = if cond_all {
                automation.CreateTrueCondition()?
            } else {
                // class in the known set OR one of the fixed button ids.
                let mut conds: Vec<IUIAutomationCondition> = Vec::new();
                for c in KNOWN_CLASSES {
                    conds.push(automation.CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from(*c))?);
                }
                for i in FIXED_BUTTON_IDS.iter().chain(std::iter::once(&WIDGETS_ID)) {
                    conds.push(automation.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(*i))?);
                }
                let mut it = conds.into_iter();
                let mut acc = it.next().ok_or_else(windows::core::Error::empty)?;
                for c in it {
                    acc = automation.CreateOrCondition(&acc, &c)?;
                }
                acc
            };
            Ok(Uia { automation, cache, cond, root: None, containers: Vec::new() })
        }
    }

    /// (Re)acquire the taskbar element and the containers to scan.
    unsafe fn acquire_root(&mut self) -> windows::core::Result<()> {
        unsafe {
            let hwnd = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null())?;
            let root = self.automation.ElementFromHandle(hwnd)?;
            self.containers.clear();
            let frame_cond = self
                .automation
                .CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from("TaskbarFrame"))?;
            let tray_cond = self
                .automation
                .CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from("SystemTray.NormalButton"))?;
            let frame = root.FindFirstBuildCache(TreeScope_Descendants, &frame_cond, &self.cache);
            let tray = root.FindFirstBuildCache(TreeScope_Descendants, &tray_cond, &self.cache);
            if let (Ok(frame), Ok(tray)) = (frame, tray)
                && let Ok(walker) = self.automation.RawViewWalker()
                && let Ok(parent) = walker.GetParentElement(&tray)
            {
                self.containers = vec![frame, parent];
            } else {
                crate::wlog!("taskbar_space: containers not found; using full descendant scans");
            }
            self.root = Some(root);
            Ok(())
        }
    }

    unsafe fn scan(&mut self) -> windows::core::Result<Vec<Elem>> {
        unsafe {
            if self.root.is_none() {
                self.acquire_root()?;
            }
            let mut out = Vec::new();
            let r = if self.containers.is_empty() {
                match &self.root {
                    Some(root) => self.collect(root, TreeScope_Descendants, &mut out),
                    None => Ok(()),
                }
            } else {
                self.containers
                    .iter()
                    .try_for_each(|c| self.collect(c, TreeScope_Children, &mut out))
            };
            if let Err(e) = r {
                self.root = None; // stale (Explorer restarted?): re-acquire next time
                return Err(e);
            }
            Ok(out)
        }
    }

    unsafe fn collect(
        &self,
        from: &IUIAutomationElement,
        scope: TreeScope,
        out: &mut Vec<Elem>,
    ) -> windows::core::Result<()> {
        unsafe {
            let arr = from.FindAllBuildCache(scope, &self.cond, &self.cache)?;
            for i in 0..arr.Length()? {
                let el = arr.GetElement(i)?;
                let r: RECT = el.CachedBoundingRectangle().unwrap_or_default();
                out.push(Elem {
                    class: el.CachedClassName().map(|b| b.to_string()).unwrap_or_default(),
                    id: el.CachedAutomationId().map(|b| b.to_string()).unwrap_or_default(),
                    rect: (r.left, r.top, r.right, r.bottom),
                });
            }
            Ok(())
        }
    }
}


/// Exact class names queried (UIA has no prefix match). Anything else with a
/// `SystemTray.` prefix would only matter if it is left of these; see `dump`.
const KNOWN_CLASSES: &[&str] = &[
    TASK_BUTTON_CLASS,
    "SystemTray.NormalButton",
    "SystemTray.AccentButton",
    "SystemTray.OmniButton",
    "SystemTray.OmniButtonRight",
    "SystemTray.ShowDesktopButton",
    "SystemTray.ChevronIconView",
    "SystemTray.IconView",
    "SystemTray.Item",
];

/// Debug aid: all descendants of the taskbar (class, id, rect), as text.
pub fn dump() -> String {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut out = String::new();
        match Uia::new(true).and_then(|mut u| u.scan()) {
            Ok(v) => {
                for e in v {
                    out.push_str(&format!("{:<60} {:<28} {:?}\n", e.class, e.id, e.rect));
                }
            }
            Err(e) => out.push_str(&format!("error: {e}\n")),
        }
        CoUninitialize();
        out
    }
}

/// Debug aid: average cost of one poll with the OR-condition vs. the true-condition query.
pub fn bench() -> String {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut out = String::new();
        for (name, all) in [("or-condition", false), ("true-condition", true)] {
            match Uia::new(all) {
                Ok(mut u) => {
                    let _ = u.scan();
                    let t = Instant::now();
                    let mut n = 0;
                    for _ in 0..20 {
                        n = u.scan().map(|v| v.len()).unwrap_or(0);
                    }
                    out.push_str(&format!("{name}: {} elements, {:?} per poll\n", n, t.elapsed() / 20));
                }
                Err(e) => out.push_str(&format!("{name}: error {e}\n")),
            }
        }
        CoUninitialize();
        out
    }
}

fn worker(shared: Arc<Shared>, notify: isize, msg: u32) {
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let mut uia: Option<Uia> = None;
        let mut fails = 0u32;
        let mut last_published: Option<Option<Space>> = None;
        let (mut polls, mut total_us, mut max_us) = (0u64, 0u64, 0u64);

        while !shared.stop.load(Ordering::SeqCst) {
            let t0 = Instant::now();
            if uia.is_none() {
                match Uia::new(false) {
                    Ok(u) => uia = Some(u),
                    Err(e) => crate::wlog!("taskbar_space: UIA init failed: {e}"),
                }
            }
            let result = match uia.as_mut() {
                Some(u) => u.scan().map(|e| compute_space(&e)),
                None => Err(windows::core::Error::empty()),
            };
            let us = t0.elapsed().as_micros() as u64;
            shared.last_us.store(us, Ordering::Relaxed);
            let publish = match result {
                Ok(space) => {
                    fails = 0;
                    polls += 1;
                    total_us += us;
                    max_us = max_us.max(us);
                    if polls == 1 || polls == 5 || polls % 60 == 0 {
                        crate::wlog!(
                            "taskbar_space: poll #{polls}: last {us} us, avg {} us, max {max_us} us",
                            total_us / polls
                        );
                    }
                    Some(Some(space))
                }
                Err(e) => {
                    fails += 1;
                    if fails == 1 {
                        crate::wlog!("taskbar_space: poll failed: {e}");
                    }
                    if fails >= FAIL_LIMIT { Some(None) } else { None }
                }
            };
            if let Some(p) = publish
                && last_published != Some(p)
            {
                last_published = Some(p);
                if let Ok(mut g) = shared.state.lock() {
                    *g = p;
                }
                crate::wlog!("taskbar_space: {:?}", p);
                let _ = PostMessageW(HWND(notify as *mut _), msg, WPARAM(0), LPARAM(0));
            }

            // Sleep until the next poll, a refresh request, or shutdown.
            let mut g = match shared.wake.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
            if !*g {
                g = match shared.cv.wait_timeout(g, POLL) {
                    Ok((g, _)) => g,
                    Err(_) => break,
                };
            }
            *g = false;
        }
        drop(uia);
        if com {
            CoUninitialize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(class: &str, id: &str, l: i32, r: i32) -> Elem {
        Elem { class: class.into(), id: id.into(), rect: (l, 1392, r, 1440) }
    }

    fn sample() -> Vec<Elem> {
        vec![
            e("", "WidgetsButton", 6, 158),
            e("ToggleButton", "StartButton", 1293, 1338),
            e("ToggleButton", "SearchButton", 1340, 1390),
            e("ToggleButton", "TaskViewButton", 1392, 1440),
            e(TASK_BUTTON_CLASS, "a", 1450, 1500),
            e(TASK_BUTTON_CLASS, "b", 2056, 2104),
            e(TASK_BUTTON_CLASS, "c", 2104, 2148),
            e(TASK_BUTTON_CLASS, "hidden", -32000, -31950),
            e("SystemTray.NormalButton", "", 2840, 2870),
            e("SystemTray.OmniButton", "", 2830, 2900),
            e("SystemTray.ShowDesktopButton", "", 3430, 3440),
        ]
    }

    #[test]
    fn classifies_elements() {
        let s = compute_space(&sample());
        assert_eq!(s.buttons, Some((1293, 2148)));
        assert_eq!(s.widgets, Some((6, 158)));
        assert_eq!(s.tray_left, Some(2830));
    }

    #[test]
    fn tray_span_and_placement() {
        let s = compute_space(&sample());
        let span = free_span(true, &s, (0, 3440), 8, 8);
        assert_eq!(span, (2156, 2822));
        assert_eq!(place_in_span(true, span, 660), (2162, 2822));
        // Wider than the span: overflows leftwards (the layout must prevent this).
        assert_eq!(place_in_span(true, span, 900), (1922, 2822));
    }

    #[test]
    fn left_span_and_placement() {
        let s = compute_space(&sample());
        let span = free_span(false, &s, (0, 3440), 8, 8);
        assert_eq!(span, (166, 1285));
        assert_eq!(place_in_span(false, span, 500), (166, 666));
        assert_eq!(place_in_span(false, span, 5000), (166, 5166));
    }

    #[test]
    fn missing_parts_fall_back_to_taskbar_edges() {
        let s = Space::default();
        assert_eq!(free_span(true, &s, (0, 3440), 8, 8), (0, 3432));
        assert_eq!(free_span(false, &s, (0, 3440), 8, 8), (8, 3440));
    }

    #[test]
    fn degenerate_span_never_negative() {
        let s = Space { buttons: Some((100, 3000)), widgets: None, tray_left: Some(2900) };
        let span = free_span(true, &s, (0, 3440), 8, 8);
        assert_eq!(span.0, span.1);
        assert_eq!(place_in_span(true, span, 0), (span.1, span.1));
    }
}
