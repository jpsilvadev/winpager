//! OWNER: host agent. Pager window, message loop, input, hooks, menu.
//!
//! Window architecture
//! -------------------
//! * `control` window: hidden top-level popup. It owns the timers, receives the vd listener
//!   message, the `TaskbarCreated` broadcast, `WM_SETTINGCHANGE`, `WM_DISPLAYCHANGE`, ..., and
//!   is the owner of the context menu / message boxes / tooltip tool. Child windows of another
//!   process's taskbar would not receive any of those broadcasts, and the control window is
//!   stable across pager re-creation.
//! * `pager` window: the visible layered window (child of `Shell_TrayWnd` in
//!   `Placement::Taskbar`, a top-most popup in `Placement::Floating`). It only handles mouse
//!   input and is re-created whenever the placement or the taskbar changes.
//!
//! Wheel delivery: a child of another process's window receives `WM_MOUSEWHEEL` only if the
//! system routes it there (it normally goes to the focus window), so a `WH_MOUSE_LL` hook is
//! installed while the cursor is over the pager (armed by the first `WM_MOUSEMOVE`, removed on
//! `WM_MOUSELEAVE`). The hook swallows wheel messages whose point is inside the pager and
//! posts `WM_APP_WHEEL` to the pager window. The plain `WM_MOUSEWHEEL` handler is kept as a
//! fallback for when the hook could not be installed.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::config::{Config, Placement};
use crate::icons::IconCache;
use crate::layout::Layout;
use crate::menu::{self, Cmd, MenuState};
use crate::model::*;
use crate::render::Renderer;
use crate::taskbar::{self, TaskbarInfo};
use crate::theme::Theme;
use crate::tooltip::Tooltip;
use crate::vd;

const PAGER_CLASS: PCWSTR = w!("WinPagerWindow");
const CONTROL_CLASS: PCWSTR = w!("WinPagerControl");
const MUTEX_NAME: PCWSTR = w!("Local\\WinPager.SingleInstance");

/// Posted by the vd listener to the control window.
const WM_APP_VD: u32 = WM_APP + 1;
/// Posted by the low-level mouse hook to the pager. wParam = wheel delta (i16 as isize),
/// lParam = 1 for the horizontal wheel.
const WM_APP_WHEEL: u32 = WM_APP + 2;

const TIMER_REFRESH: usize = 1;
const TIMER_POLL: usize = 2;
const TIMER_DRAG: usize = 3;
const TIMER_VERIFY: usize = 4;

const REFRESH_DELAY_MS: u32 = 30;
const POLL_MS: u32 = 1000;
const VERIFY_MS: u32 = 600;

const TASKBAR_BRIDGE_CLASS: &str = "Windows.UI.Composition.DesktopWindowContentBridge";

static CONTROL_HWND: AtomicIsize = AtomicIsize::new(0);
static PAGER_HWND: AtomicIsize = AtomicIsize::new(0);
/// Pager rectangle in screen coordinates (left, top, right, bottom), read by the mouse hook.
static PAGER_RECT: [AtomicI32; 4] =
    [AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)];
static REFRESH_ARMED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` on the application state. Returns `None` if the state does not exist yet or is
/// already borrowed (a re-entrant message sent while we were inside a Win32 call).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => None,
    })
}

struct Press {
    x: f32,
    y: f32,
    hit: Hit,
    dragging: bool,
}

struct App {
    cfg: Config,
    control: HWND,
    pager: HWND,
    /// Placement actually in use (differs from `cfg.placement` after an automatic fallback).
    effective: Placement,
    renderer: Renderer,
    icons: IconCache,
    theme: Theme,
    snapshot: Snapshot,
    layout: Layout,
    view: ViewState,
    taskbar: Option<TaskbarInfo>,
    /// Pager rectangle in screen coordinates.
    pager_rect: RectI,
    tooltip: Option<Tooltip>,
    tooltip_key: Option<Hit>,
    press: Option<Press>,
    wheel_accum: i32,
    tracking: bool,
    wheel_hook: Option<HHOOK>,
    win_hooks: Vec<HWINEVENTHOOK>,
    listener: Option<vd::Listener>,
    quitting: bool,
    last_err: String,
    config_mtime: Option<std::time::SystemTime>,
    verified_blocked_once: bool,
    /// Test aid (`WINPAGER_TEST_NO_LEAVE=1`): ignore WM_MOUSELEAVE, so hover state produced by
    /// posted (synthetic) mouse messages is not immediately cleared by the system's leave.
    ignore_leave: bool,
}

// ---------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------

/// Holds the single-instance mutex for the lifetime of the process.
pub struct SingleInstance(Option<HANDLE>);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }
}

/// Take the named single-instance mutex. `None` means another instance already runs (the
/// caller should exit quietly). If the mutex cannot be created at all we run anyway.
pub fn single_instance() -> Option<SingleInstance> {
    unsafe {
        match CreateMutexW(None, false, MUTEX_NAME) {
            Ok(h) if GetLastError() == ERROR_ALREADY_EXISTS => {
                let _ = CloseHandle(h);
                None
            }
            Ok(h) => Some(SingleInstance(Some(h))),
            Err(_) => Some(SingleInstance(None)),
        }
    }
}

pub fn run(cfg: Config) -> anyhow::Result<()> {
    unsafe {
        let com_ok = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        if !com_ok {
            crate::wlog!("CoInitializeEx failed");
        }
        let result = run_inner(cfg);
        if com_ok {
            CoUninitialize();
        }
        if let Err(e) = &result {
            crate::wlog!("fatal: {e:#}");
        }
        result
    }
}

fn run_inner(cfg: Config) -> anyhow::Result<()> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        register_class(CONTROL_CLASS, Some(control_proc), hinst)?;
        register_class(PAGER_CLASS, Some(pager_proc), hinst)?;
        menu::enable_dark_menus();

        let control = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CONTROL_CLASS,
            w!("WinPager control"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            hinst,
            None,
        )?;
        CONTROL_HWND.store(control.0 as isize, Ordering::SeqCst);

        let renderer = Renderer::new()?;
        let app = App {
            effective: cfg.placement,
            cfg,
            control,
            pager: HWND::default(),
            renderer,
            icons: IconCache::new(),
            theme: Theme::from_system(),
            snapshot: Snapshot::default(),
            layout: Layout::default(),
            view: ViewState::default(),
            taskbar: None,
            pager_rect: RectI::default(),
            tooltip: None,
            tooltip_key: None,
            press: None,
            wheel_accum: 0,
            tracking: false,
            wheel_hook: None,
            win_hooks: Vec::new(),
            listener: None,
            quitting: false,
            last_err: String::new(),
            config_mtime: config_mtime(),
            verified_blocked_once: false,
            ignore_leave: std::env::var_os("WINPAGER_TEST_NO_LEAVE").is_some(),
        };
        APP.with(|c| *c.borrow_mut() = Some(app));

        with_app(|app| {
            app.tooltip = Tooltip::new(app.control);
            app.listener = match vd::listen(from_hwnd(app.control), WM_APP_VD) {
                Ok(l) => Some(l),
                Err(e) => {
                    crate::wlog!("vd::listen failed: {e:#}");
                    None
                }
            };
            install_win_event_hooks(app);
            rebuild_pager(app);
            SetTimer(app.control, TIMER_POLL, POLL_MS, None);
        });

        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Clean shutdown (also reached after an unexpected loop exit).
        let app = APP.with(|c| c.borrow_mut().take());
        if let Some(mut app) = app {
            shutdown(&mut app);
        }
        Ok(())
    }
}

fn register_class(name: PCWSTR, proc: WNDPROC, hinst: HINSTANCE) -> anyhow::Result<()> {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: proc,
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: name,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            anyhow::bail!("RegisterClassExW failed: {}", windows::core::Error::from_win32());
        }
        Ok(())
    }
}

fn shutdown(app: &mut App) {
    unsafe {
        app.quitting = true;
        remove_wheel_hook(app);
        for h in app.win_hooks.drain(..) {
            let _ = UnhookWinEvent(h);
        }
        app.listener = None;
        app.tooltip = None;
        let _ = KillTimer(app.control, TIMER_POLL);
        let _ = KillTimer(app.control, TIMER_REFRESH);
        if !app.pager.is_invalid() {
            let _ = DestroyWindow(app.pager);
            app.pager = HWND::default();
        }
        PAGER_HWND.store(0, Ordering::SeqCst);
        if IsWindow(app.control).as_bool() {
            let _ = DestroyWindow(app.control);
        }
        CONTROL_HWND.store(0, Ordering::SeqCst);
    }
}

fn config_mtime() -> Option<std::time::SystemTime> {
    let p = Config::path()?;
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

// ---------------------------------------------------------------------------------------
// Pager window creation and geometry
// ---------------------------------------------------------------------------------------

fn embedded(app: &App) -> bool {
    app.effective == Placement::Taskbar
}

/// Destroy the pager window (if any) and reset the input state tied to it.
fn destroy_pager(app: &mut App) {
    unsafe {
        remove_wheel_hook(app);
        app.press = None;
        app.tracking = false;
        app.view = ViewState::default();
        app.tooltip_key = None;
        if let Some(t) = app.tooltip.as_mut() {
            t.hide();
        }
        let _ = KillTimer(app.control, TIMER_DRAG);
        if !app.pager.is_invalid() {
            let old = app.pager;
            app.pager = HWND::default();
            PAGER_HWND.store(0, Ordering::SeqCst);
            if IsWindow(old).as_bool() {
                let _ = DestroyWindow(old);
            }
        }
    }
}

/// (Re)create the pager window for the current placement, then lay out and draw it.
fn rebuild_pager(app: &mut App) {
    destroy_pager(app);
    if let Some(tb) = taskbar::find() {
        app.taskbar = Some(tb);
    }
    if app.taskbar.is_none() {
        crate::wlog!("rebuild: no taskbar found; waiting");
        return;
    }
    collect_snapshot(app);
    relayout(app);
    let Some(tb) = app.taskbar else { return };

    let mut created = None;
    if embedded(app) {
        let rect = taskbar::pager_rect(&tb, app.cfg.anchor, app.cfg.offset, app.layout.width.max(1));
        match create_pager_window(&tb, rect, true) {
            Ok(h) => {
                created = Some((h, rect));
                crate::wlog!("created embedded pager {:?} client rect {:?}", h.0, taskbar::screen_to_client(tb.hwnd(), &rect));
            }
            Err(e) => {
                crate::wlog!("embedded pager creation failed: {e:#}; falling back to floating");
                app.effective = Placement::Floating;
            }
        }
    }
    if created.is_none() {
        let rect = taskbar::pager_rect(&tb, app.cfg.anchor, app.cfg.offset, app.layout.width.max(1));
        match create_pager_window(&tb, rect, false) {
            Ok(h) => created = Some((h, rect)),
            Err(e) => crate::wlog!("floating pager creation failed: {e:#}"),
        }
    }
    let Some((hwnd, rect)) = created else { return };
    app.pager = hwnd;
    PAGER_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    app.pager_rect = rect;
    store_pager_rect(&rect);
    log_overlap(app, &rect);
    apply_geometry(app, true);
    present(app);
    if embedded(app) {
        unsafe {
            SetTimer(app.control, TIMER_VERIFY, VERIFY_MS, None);
        }
    }
}

fn create_pager_window(tb: &TaskbarInfo, rect: RectI, embedded: bool) -> anyhow::Result<HWND> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        if embedded {
            let c = taskbar::screen_to_client(tb.hwnd(), &rect);
            let h = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE,
                PAGER_CLASS,
                w!("WinPager"),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                c.left,
                c.top,
                c.width(),
                c.height(),
                tb.hwnd(),
                None,
                hinst,
                None,
            )?;
            Ok(h)
        } else {
            let h = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                PAGER_CLASS,
                w!("WinPager"),
                WS_POPUP,
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
                None,
                None,
                hinst,
                None,
            )?;
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            Ok(h)
        }
    }
}

fn store_pager_rect(r: &RectI) {
    PAGER_RECT[0].store(r.left, Ordering::Relaxed);
    PAGER_RECT[1].store(r.top, Ordering::Relaxed);
    PAGER_RECT[2].store(r.right, Ordering::Relaxed);
    PAGER_RECT[3].store(r.bottom, Ordering::Relaxed);
}

fn log_overlap(app: &App, rect: &RectI) {
    if let Some(tb) = &app.taskbar
        && taskbar::overlaps_task_buttons(tb, rect)
    {
        crate::wlog!("warning: pager {:?} overlaps the task button area {:?}", rect, tb.rebar);
    }
}

/// Move/resize the pager window to match the layout width and the anchor.
fn apply_geometry(app: &mut App, force: bool) {
    let Some(tb) = app.taskbar else { return };
    if app.pager.is_invalid() {
        return;
    }
    let rect = taskbar::pager_rect(&tb, app.cfg.anchor, app.cfg.offset, app.layout.width.max(1));
    if !force && rect == app.pager_rect {
        return;
    }
    if rect != app.pager_rect {
        log_overlap(app, &rect);
    }
    app.pager_rect = rect;
    store_pager_rect(&rect);
    unsafe {
        let r = if embedded(app) {
            let c = taskbar::screen_to_client(tb.hwnd(), &rect);
            SetWindowPos(
                app.pager,
                HWND_TOP,
                c.left,
                c.top,
                c.width(),
                c.height(),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        } else if tb.is_collapsed() {
            let _ = ShowWindow(app.pager, SW_HIDE);
            Ok(())
        } else {
            SetWindowPos(
                app.pager,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        };
        if let Err(e) = r {
            crate::wlog!("SetWindowPos failed: {e}");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Refresh pipeline
// ---------------------------------------------------------------------------------------

fn log_once(app: &mut App, msg: String) {
    if app.last_err != msg {
        crate::wlog!("{msg}");
        app.last_err = msg;
    }
}

fn exclude_list(app: &App) -> Vec<Hwnd> {
    let mut v = vec![from_hwnd(app.control)];
    if !app.pager.is_invalid() {
        v.push(from_hwnd(app.pager));
    }
    if let Some(t) = &app.tooltip {
        v.push(from_hwnd(t.hwnd()));
    }
    v
}

fn collect_snapshot(app: &mut App) -> bool {
    let exclude = exclude_list(app);
    match crate::enumerate::build_snapshot(&mut app.icons, &exclude) {
        Ok(s) => {
            app.snapshot = s;
            app.last_err.clear();
            true
        }
        Err(e) => {
            log_once(app, format!("build_snapshot failed: {e:#}"));
            false
        }
    }
}

/// Recompute the layout from the cached snapshot and fix up everything that depends on it.
fn relayout(app: &mut App) {
    let (height, dpi) = match &app.taskbar {
        Some(tb) => (tb.rect.height().max(16), tb.dpi),
        None => (48, 96),
    };
    app.layout = Layout::compute(&app.snapshot, &app.cfg.layout, height, dpi);
    let n = app.snapshot.desktops.len();
    // Drop view state that refers to desktops/windows that no longer exist.
    if let Some(Hit::Desktop(d) | Hit::Window { desktop: d, .. }) = app.view.hover
        && d >= n
    {
        app.view.hover = None;
    }
    if let Some(drag) = app.view.drag {
        let alive = drag.source_desktop < n && app.snapshot.windows.iter().any(|w| w.hwnd == drag.hwnd);
        if !alive {
            cancel_drag(app);
        }
    }
    apply_geometry(app, false);
}

/// Full refresh: enumerate, lay out, draw.
fn refresh(app: &mut App) {
    if app.quitting {
        return;
    }
    if collect_snapshot(app) {
        relayout(app);
        present(app);
    }
    if !embedded(app) && !app.pager.is_invalid() {
        assert_topmost(app);
    }
}

fn present(app: &mut App) {
    if app.pager.is_invalid() || app.layout.cells.is_empty() {
        return;
    }
    let r = app.renderer.present(
        app.pager,
        &app.snapshot,
        &app.layout,
        &app.view,
        &app.theme,
        &app.cfg.layout,
    );
    if let Err(e) = r {
        log_once(app, format!("present failed: {e:#}"));
    }
}

fn assert_topmost(app: &App) {
    unsafe {
        let _ = SetWindowPos(
            app.pager,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

fn arm_refresh() {
    if !REFRESH_ARMED.swap(true, Ordering::SeqCst) {
        let c = CONTROL_HWND.load(Ordering::SeqCst);
        if c != 0 {
            let id = unsafe { SetTimer(to_hwnd(c), TIMER_REFRESH, REFRESH_DELAY_MS, None) };
            if id == 0 {
                REFRESH_ARMED.store(false, Ordering::SeqCst);
            }
        } else {
            REFRESH_ARMED.store(false, Ordering::SeqCst);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Periodic poll: taskbar geometry, z-order, config file, stray hooks
// ---------------------------------------------------------------------------------------

fn poll(app: &mut App) {
    if app.quitting {
        return;
    }
    let tb = taskbar::find();
    if app.pager.is_invalid() || (embedded(app) && !pager_still_embedded(app, tb.as_ref())) {
        if tb.is_some() {
            crate::wlog!("poll: pager missing or detached; rebuilding");
            rebuild_pager(app);
        }
        return;
    }
    if let Some(tb) = tb
        && app.taskbar != Some(tb)
    {
        crate::wlog!("poll: taskbar geometry changed: {:?}", tb);
        app.taskbar = Some(tb);
        relayout(app);
        present(app);
    }
    if embedded(app) {
        ensure_above_bridge(app);
    } else {
        // A floating top-most pager must not cover full-screen apps.
        let fullscreen = unsafe {
            use windows::Win32::UI::Shell::{
                QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
            };
            matches!(
                SHQueryUserNotificationState(),
                Ok(s) if s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE
            )
        };
        if fullscreen {
            unsafe {
                let _ = ShowWindow(app.pager, SW_HIDE);
            }
        } else {
            let tb_hidden = app.taskbar.is_some_and(|t| t.is_collapsed());
            unsafe {
                let _ = ShowWindow(app.pager, if tb_hidden { SW_HIDE } else { SW_SHOWNOACTIVATE });
            }
            assert_topmost(app);
        }
    }
    // Remove a stray wheel hook when the cursor is gone.
    if app.wheel_hook.is_some() && app.press.is_none() {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        if !point_in_pager(p.x, p.y) {
            remove_wheel_hook(app);
            app.tracking = false;
            if app.view.hover.take().is_some() {
                present(app);
            }
            hide_tooltip(app);
        }
    }
    // Live reload of the config file.
    let m = config_mtime();
    if m != app.config_mtime {
        app.config_mtime = m;
        if m.is_some() {
            crate::wlog!("config file changed; reloading");
            let new = Config::load();
            apply_config(app, new);
        }
    }
}

fn pager_still_embedded(app: &App, tb: Option<&TaskbarInfo>) -> bool {
    unsafe {
        if !IsWindow(app.pager).as_bool() {
            return false;
        }
        match tb {
            Some(tb) => GetParent(app.pager).map(|p| p == tb.hwnd()).unwrap_or(false),
            None => true,
        }
    }
}

/// Keep the embedded pager above the taskbar's XAML bridge window.
fn ensure_above_bridge(app: &App) {
    unsafe {
        let mut cur = GetWindow(app.pager, GW_HWNDPREV).unwrap_or_default();
        let mut guard = 0;
        while !cur.is_invalid() && guard < 32 {
            if taskbar::class_name(cur) == TASKBAR_BRIDGE_CLASS {
                crate::wlog!("pager fell below the XAML bridge; raising");
                let _ = SetWindowPos(app.pager, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                return;
            }
            cur = GetWindow(cur, GW_HWNDPREV).unwrap_or_default();
            guard += 1;
        }
    }
}

enum Verdict {
    Ok,
    /// Something outside the taskbar covers the pager (full-screen app, flyout...).
    Inconclusive,
    /// A taskbar-internal window receives the hit test instead of the pager.
    Blocked(String),
}

fn verify_embedded(app: &App) -> Verdict {
    unsafe {
        let r = app.pager_rect;
        let pt = POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 };
        let hit = WindowFromPoint(pt);
        if hit == app.pager {
            return Verdict::Ok;
        }
        let Some(tb) = app.taskbar else { return Verdict::Inconclusive };
        if GetAncestor(hit, GA_ROOT) == tb.hwnd() {
            Verdict::Blocked(format!("{} ({:?})", taskbar::class_name(hit), hit.0))
        } else {
            Verdict::Inconclusive
        }
    }
}

fn on_verify_timer(app: &mut App) {
    if !embedded(app) || app.pager.is_invalid() {
        return;
    }
    match verify_embedded(app) {
        Verdict::Ok => crate::wlog!("verify: embedded pager receives hit tests at its center"),
        Verdict::Inconclusive => crate::wlog!("verify: inconclusive (a window outside the taskbar covers the pager)"),
        Verdict::Blocked(by) if !app.verified_blocked_once => {
            crate::wlog!("verify: pager is blocked by {by}; raising and retrying once");
            app.verified_blocked_once = true;
            unsafe {
                let _ = SetWindowPos(app.pager, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                SetTimer(app.control, TIMER_VERIFY, VERIFY_MS, None);
            }
        }
        Verdict::Blocked(by) => {
            crate::wlog!("verify: embedding does not work (blocked by {by}); falling back to floating");
            app.effective = Placement::Floating;
            rebuild_pager(app);
        }
    }
}

fn apply_config(app: &mut App, new: Config) {
    let placement_changed = new.placement != app.cfg.placement;
    app.cfg = new;
    if placement_changed {
        app.effective = app.cfg.placement;
        app.verified_blocked_once = false;
        rebuild_pager(app);
    } else {
        relayout(app);
        present(app);
    }
}

fn save_config(app: &mut App) {
    if let Err(e) = app.cfg.save() {
        crate::wlog!("saving config failed: {e:#}");
    }
    app.config_mtime = config_mtime();
}

// ---------------------------------------------------------------------------------------
// WinEvent hooks and the wheel hook
// ---------------------------------------------------------------------------------------

fn install_win_event_hooks(app: &mut App) {
    let ranges = [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
        (EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ];
    for (lo, hi) in ranges {
        let h = unsafe {
            SetWinEventHook(
                lo,
                hi,
                None,
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if h.is_invalid() {
            crate::wlog!("SetWinEventHook({lo:#x}..{hi:#x}) failed");
        } else {
            app.win_hooks.push(h);
        }
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }
    // Top-level windows only.
    unsafe {
        if GetAncestor(hwnd, GA_PARENT) != GetDesktopWindow() {
            return;
        }
    }
    arm_refresh();
}

fn point_in_pager(x: i32, y: i32) -> bool {
    x >= PAGER_RECT[0].load(Ordering::Relaxed)
        && y >= PAGER_RECT[1].load(Ordering::Relaxed)
        && x < PAGER_RECT[2].load(Ordering::Relaxed)
        && y < PAGER_RECT[3].load(Ordering::Relaxed)
}

unsafe extern "system" fn mouse_ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code >= 0 {
            let m = wparam.0 as u32;
            if m == WM_MOUSEWHEEL || m == WM_MOUSEHWHEEL {
                let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
                let pager = PAGER_HWND.load(Ordering::SeqCst);
                if pager != 0 && point_in_pager(ms.pt.x, ms.pt.y) {
                    let delta = (ms.mouseData >> 16) as u16 as i16;
                    let _ = PostMessageW(
                        to_hwnd(pager),
                        WM_APP_WHEEL,
                        WPARAM(delta as isize as usize),
                        LPARAM((m == WM_MOUSEHWHEEL) as isize),
                    );
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

fn install_wheel_hook(app: &mut App) {
    if app.wheel_hook.is_some() {
        return;
    }
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).map(Into::into).unwrap_or_default();
        match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_ll_proc), hinst, 0) {
            Ok(h) => app.wheel_hook = Some(h),
            Err(e) => crate::wlog!("SetWindowsHookExW(WH_MOUSE_LL) failed: {e}"),
        }
    }
}

fn remove_wheel_hook(app: &mut App) {
    if let Some(h) = app.wheel_hook.take() {
        unsafe {
            let _ = UnhookWindowsHookEx(h);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Control window
// ---------------------------------------------------------------------------------------

unsafe extern "system" fn control_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                match wparam.0 {
                    TIMER_REFRESH => {
                        let _ = KillTimer(hwnd, TIMER_REFRESH);
                        REFRESH_ARMED.store(false, Ordering::SeqCst);
                        if with_app(refresh).is_none() {
                            arm_refresh();
                        }
                    }
                    TIMER_POLL => {
                        with_app(poll);
                    }
                    TIMER_DRAG => {
                        with_app(|app| {
                            if app.view.drag.is_some() && GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0 {
                                crate::wlog!("drag cancelled with Esc");
                                cancel_press(app);
                            }
                        });
                    }
                    TIMER_VERIFY => {
                        let _ = KillTimer(hwnd, TIMER_VERIFY);
                        with_app(on_verify_timer);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_APP_VD => {
                arm_refresh();
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                let what = if lparam.0 != 0 {
                    PCWSTR(lparam.0 as *const u16).to_string().unwrap_or_default()
                } else {
                    String::new()
                };
                if what == "ImmersiveColorSet" {
                    with_app(|app| {
                        app.theme = Theme::from_system();
                        present(app);
                    });
                } else if wparam.0 as u32 == SPI_SETWORKAREA.0 {
                    with_app(poll);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DWMCOLORIZATIONCOLORCHANGED | WM_THEMECHANGED => {
                with_app(|app| {
                    app.theme = Theme::from_system();
                    present(app);
                });
                LRESULT(0)
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                with_app(|app| {
                    poll(app);
                    refresh(app);
                });
                LRESULT(0)
            }
            WM_QUERYENDSESSION => LRESULT(1),
            WM_ENDSESSION | WM_CLOSE => {
                begin_quit();
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => {
                if msg == taskbar_created_msg() {
                    crate::wlog!("TaskbarCreated: re-finding taskbar and rebuilding pager");
                    if with_app(|app| {
                        app.taskbar = None;
                        app.verified_blocked_once = false;
                        rebuild_pager(app);
                    })
                    .is_none()
                    {
                        let _ = PostMessageW(hwnd, msg, wparam, lparam);
                    }
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
    }
}

fn taskbar_created_msg() -> u32 {
    use std::sync::OnceLock;
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(taskbar::taskbar_created_message)
}

fn begin_quit() {
    let ctl = CONTROL_HWND.load(Ordering::SeqCst);
    // Not inside a borrow: tear everything down. If we are (re-entrant), post to ourselves.
    if with_app(shutdown).is_none() && ctl != 0 {
        unsafe {
            let _ = PostMessageW(to_hwnd(ctl), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

// ---------------------------------------------------------------------------------------
// Pager window: input
// ---------------------------------------------------------------------------------------

fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    (lp.0 as i16 as i32, (lp.0 >> 16) as i16 as i32)
}

fn hit_desktop(h: Hit) -> Option<usize> {
    match h {
        Hit::None => None,
        Hit::Desktop(d) | Hit::Window { desktop: d, .. } => Some(d),
    }
}

unsafe extern "system" fn pager_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_NCHITTEST => LRESULT(HTCLIENT as isize),
            WM_ERASEBKGND => LRESULT(1),
            WM_SETCURSOR => {
                // Keep the arrow cursor; do not let the parent decide.
                if let Ok(c) = LoadCursorW(None, IDC_ARROW) {
                    SetCursor(c);
                }
                LRESULT(1)
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_xy(lparam);
                with_app(|app| on_mouse_move(app, hwnd, x, y));
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_app(|app| {
                    if app.ignore_leave {
                        return;
                    }
                    app.tracking = false;
                    if app.press.is_none() {
                        remove_wheel_hook(app);
                        if app.view.hover.take().is_some() {
                            present(app);
                        }
                        hide_tooltip(app);
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = lparam_xy(lparam);
                with_app(|app| {
                    hide_tooltip(app);
                    let hit = app.layout.hit_test(&app.snapshot, x as f32, y as f32);
                    app.press = Some(Press { x: x as f32, y: y as f32, hit, dragging: false });
                    SetCapture(hwnd);
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = lparam_xy(lparam);
                let (press, drag) = with_app(|app| (app.press.take(), app.view.drag.take()))
                    .unwrap_or((None, None));
                if press.is_some() {
                    let _ = ReleaseCapture();
                    let _ = KillTimer(to_hwnd(CONTROL_HWND.load(Ordering::SeqCst)), TIMER_DRAG);
                    with_app(|app| on_button_up(app, x, y, press, drag));
                }
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                // Capture lost while a press/drag is active (not a normal button release).
                with_app(|app| {
                    if app.press.is_some() || app.view.drag.is_some() {
                        crate::wlog!("capture lost; cancelling press/drag");
                        cancel_press(app);
                    }
                });
                LRESULT(0)
            }
            WM_CANCELMODE => {
                with_app(cancel_press);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_RBUTTONUP => {
                let (x, y) = lparam_xy(lparam);
                show_context_menu(hwnd, x, y);
                LRESULT(0)
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                // Fallback path (only reached when the low-level hook is not installed).
                let delta = (wparam.0 >> 16) as u16 as i16;
                let horizontal = msg == WM_MOUSEHWHEEL;
                with_app(|app| on_wheel(app, delta as i32, horizontal));
                LRESULT(0)
            }
            WM_APP_WHEEL => {
                let delta = wparam.0 as isize as i16;
                with_app(|app| on_wheel(app, delta as i32, lparam.0 != 0));
                LRESULT(0)
            }
            WM_DESTROY => {
                with_app(|app| {
                    if app.pager == hwnd {
                        crate::wlog!("pager window destroyed externally");
                        app.pager = HWND::default();
                        PAGER_HWND.store(0, Ordering::SeqCst);
                        remove_wheel_hook(app);
                        app.press = None;
                    }
                });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn on_mouse_move(app: &mut App, hwnd: HWND, x: i32, y: i32) {
    unsafe {
        if !app.tracking {
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            match TrackMouseEvent(&mut t) {
                Ok(()) => app.tracking = true,
                Err(e) => crate::wlog!("TrackMouseEvent failed: {e}"),
            }
            install_wheel_hook(app);
        }
    }
    let (fx, fy) = (x as f32, y as f32);
    if let Some(press) = &mut app.press {
        if !press.dragging {
            let (dx, dy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
            if (fx - press.x).abs() > dx as f32 || (fy - press.y).abs() > dy as f32 {
                press.dragging = true;
                if let Hit::Window { desktop, hwnd: wh } = press.hit {
                    let grab = app
                        .snapshot
                        .windows
                        .iter()
                        .find(|w| w.hwnd == wh)
                        .map(|w| app.layout.map_window(desktop, &w.rect))
                        .map(|r| (press.x - r.left, press.y - r.top))
                        .unwrap_or((0.0, 0.0));
                    app.view.drag = Some(DragVisual {
                        hwnd: wh,
                        source_desktop: desktop,
                        cursor: (fx, fy),
                        grab_offset: grab,
                        target_desktop: app.layout.desktop_at(fx, fy),
                    });
                    app.view.hover = None;
                    unsafe {
                        SetTimer(app.control, TIMER_DRAG, 50, None);
                    }
                }
            }
        }
        if let Some(d) = &mut app.view.drag {
            d.cursor = (fx, fy);
            d.target_desktop = app.layout.desktop_at(fx, fy);
            present(app);
        }
        return;
    }
    let hit = app.layout.hit_test(&app.snapshot, fx, fy);
    let new_hover = if hit == Hit::None { None } else { Some(hit) };
    if new_hover != app.view.hover {
        app.view.hover = new_hover;
        present(app);
    }
    update_tooltip(app, new_hover);
}

fn cancel_drag(app: &mut App) {
    app.view.drag = None;
    unsafe {
        let _ = KillTimer(app.control, TIMER_DRAG);
    }
}

/// Abort the current press/drag and redraw.
fn cancel_press(app: &mut App) {
    let had = app.press.take().is_some() || app.view.drag.is_some();
    cancel_drag(app);
    if had {
        unsafe {
            if GetCapture() == app.pager {
                // Re-entrancy: WM_CAPTURECHANGED will find press/drag already cleared.
                let _ = ReleaseCapture();
            }
        }
        present(app);
    }
}

fn on_button_up(app: &mut App, x: i32, y: i32, press: Option<Press>, drag: Option<DragVisual>) {
    let Some(press) = press else { return };
    let (fx, fy) = (x as f32, y as f32);
    let released_on = app.layout.desktop_at(fx, fy);
    if let Some(d) = drag {
        // Drag finished: move the window if dropped on another desktop.
        present(app);
        if let Some(target) = released_on
            && target != d.source_desktop
        {
            crate::wlog!("moving window {:#x} to desktop {}", d.hwnd, target);
            if let Err(e) = vd::move_window(d.hwnd, target) {
                crate::wlog!("vd::move_window failed: {e:#}");
            }
            refresh(app);
        }
        return;
    }
    if press.dragging {
        // Dragged beyond the threshold from an empty cell: neither a click nor a drag.
        present(app);
        return;
    }
    if let (Some(pd), Some(rd)) = (hit_desktop(press.hit), released_on)
        && pd == rd
    {
        switch_to(app, rd);
    }
}

fn switch_to(app: &mut App, index: usize) {
    if index >= app.snapshot.desktops.len() {
        return;
    }
    if let Err(e) = vd::switch_to(index) {
        crate::wlog!("vd::switch_to({index}) failed: {e:#}");
        return;
    }
    // Immediate feedback; the real state arrives with the next refresh.
    app.snapshot.current = index;
    present(app);
}

fn on_wheel(app: &mut App, delta: i32, horizontal: bool) {
    if app.press.is_some() {
        return;
    }
    // Vertical wheel up = previous desktop; horizontal wheel right = next desktop.
    app.wheel_accum += if horizontal { delta } else { -delta };
    let mut steps = app.wheel_accum / 120;
    app.wheel_accum -= steps * 120;
    if steps == 0 {
        return;
    }
    if app.cfg.invert_scroll {
        steps = -steps;
    }
    let n = app.snapshot.desktops.len() as i32;
    if n < 2 {
        return;
    }
    let cur = app.snapshot.current as i32;
    let target = wheel_target(cur, n, steps, app.cfg.wrap_scroll);
    if target != cur {
        switch_to(app, target as usize);
    }
}

/// Desktop reached by moving `steps` from `cur` among `n` desktops.
fn wheel_target(cur: i32, n: i32, steps: i32, wrap: bool) -> i32 {
    if wrap { (cur + steps).rem_euclid(n) } else { (cur + steps).clamp(0, n - 1) }
}

#[cfg(test)]
mod tests {
    use super::wheel_target;

    #[test]
    fn wheel_wraps_or_clamps() {
        assert_eq!(wheel_target(6, 7, 1, true), 0);
        assert_eq!(wheel_target(0, 7, -1, true), 6);
        assert_eq!(wheel_target(6, 7, 1, false), 6);
        assert_eq!(wheel_target(0, 7, -1, false), 0);
        assert_eq!(wheel_target(2, 7, 3, false), 5);
        assert_eq!(wheel_target(2, 7, -9, true), 0);
    }
}

// ---------------------------------------------------------------------------------------
// Tooltip
// ---------------------------------------------------------------------------------------

fn update_tooltip(app: &mut App, hover: Option<Hit>) {
    if app.tooltip_key == hover {
        return;
    }
    app.tooltip_key = hover;
    let Some(hit) = hover else {
        hide_tooltip(app);
        return;
    };
    let (d, wh) = match hit {
        Hit::Desktop(d) => (d, None),
        Hit::Window { desktop, hwnd } => (desktop, Some(hwnd)),
        Hit::None => {
            hide_tooltip(app);
            return;
        }
    };
    let Some(info) = app.snapshot.desktops.get(d) else { return };
    let mut text = info.display_name();
    if let Some(wh) = wh
        && let Some(w) = app.snapshot.windows.iter().find(|w| w.hwnd == wh)
    {
        let title: String = w.title.chars().take(120).collect();
        if !title.is_empty() {
            text.push_str("\r\n");
            text.push_str(&title);
        }
    }
    let cell = app.layout.cells[d];
    let cx = app.pager_rect.left + ((cell.left + cell.right) / 2.0) as i32;
    let gap = (6.0 * app.layout.scale) as i32;
    let above = app.taskbar.map(|t| t.edge) != Some(taskbar::Edge::Top);
    let anchor = if above { app.pager_rect.top - gap } else { app.pager_rect.bottom + gap };
    if let Some(t) = app.tooltip.as_mut() {
        t.show(&text, cx, anchor, above);
    }
}

fn hide_tooltip(app: &mut App) {
    app.tooltip_key = None;
    if let Some(t) = app.tooltip.as_mut() {
        t.hide();
    }
}

// ---------------------------------------------------------------------------------------
// Context menu
// ---------------------------------------------------------------------------------------

fn show_context_menu(pager: HWND, x: i32, y: i32) {
    let prepared = with_app(|app| {
        if app.view.drag.is_some() {
            return None;
        }
        hide_tooltip(app);
        let hit = app.layout.hit_test(&app.snapshot, x as f32, y as f32);
        let target = hit_desktop(hit);
        let removable = if app.snapshot.desktops.len() > 1 { target } else { None };
        let st = MenuState {
            removable,
            rows: app.cfg.layout.rows,
            show_icons: app.cfg.layout.show_icons,
            show_names: app.cfg.layout.show_names,
            placement: app.cfg.placement,
            autostart: menu::autostart_enabled(),
        };
        Some((app.control, st))
    })
    .flatten();
    let Some((owner, st)) = prepared else { return };

    let mut pt = POINT { x, y };
    unsafe {
        let _ = ClientToScreen(pager, &mut pt);
    }
    let removable = st.removable;
    let Some(cmd) = menu::show(owner, (pt.x, pt.y), &st) else { return };
    crate::wlog!("menu command: {cmd:?}");
    handle_command(owner, cmd, removable);
}

fn handle_command(owner: HWND, cmd: Cmd, target: Option<usize>) {
    match cmd {
        Cmd::AddDesktop => {
            if let Err(e) = vd::create() {
                crate::wlog!("vd::create failed: {e:#}");
            }
            with_app(refresh);
        }
        Cmd::RemoveDesktop => {
            let Some(idx) = target else { return };
            let (count, name) = with_app(|app| {
                let id = app.snapshot.desktops.get(idx).map(|d| d.id);
                let n = app.snapshot.windows.iter().filter(|w| w.desktop.is_some() && w.desktop == id).count();
                (n, app.snapshot.desktops.get(idx).map(|d| d.display_name()).unwrap_or_default())
            })
            .unwrap_or((0, String::new()));
            if count > 0 {
                let text = format!(
                    "\"{name}\" has {count} open window{}. They will be moved to a neighbouring desktop.\n\nRemove it?",
                    if count == 1 { "" } else { "s" }
                );
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                let r = unsafe {
                    MessageBoxW(
                        owner,
                        PCWSTR(wide.as_ptr()),
                        w!("Remove desktop"),
                        MB_YESNO | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND,
                    )
                };
                if r != IDYES {
                    return;
                }
            }
            if let Err(e) = vd::remove(idx) {
                crate::wlog!("vd::remove({idx}) failed: {e:#}");
            }
            with_app(refresh);
        }
        Cmd::Rows(n) => {
            with_app(|app| {
                app.cfg.layout.rows = n.clamp(1, 3);
                save_config(app);
                relayout(app);
                present(app);
            });
        }
        Cmd::ShowIcons => {
            with_app(|app| {
                app.cfg.layout.show_icons = !app.cfg.layout.show_icons;
                save_config(app);
                present(app);
            });
        }
        Cmd::ShowNames => {
            with_app(|app| {
                app.cfg.layout.show_names = !app.cfg.layout.show_names;
                save_config(app);
                present(app);
            });
        }
        Cmd::Place(p) => {
            with_app(|app| {
                if app.cfg.placement != p || app.effective != p {
                    app.cfg.placement = p;
                    app.effective = p;
                    app.verified_blocked_once = false;
                    save_config(app);
                    rebuild_pager(app);
                }
            });
        }
        Cmd::Autostart => {
            let on = !menu::autostart_enabled();
            if let Err(e) = menu::set_autostart(on) {
                crate::wlog!("autostart change failed: {e:#}");
            }
        }
        Cmd::OpenConfig => {
            with_app(|app| {
                if Config::path().is_some_and(|p| !p.exists()) {
                    save_config(app);
                }
            });
            open_config();
        }
        Cmd::Exit => begin_quit(),
    }
}

fn open_config() {
    let Some(path) = Config::path() else { return };
    let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let r = ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), None, None, SW_SHOWNORMAL);
        if (r.0 as isize) <= 32 {
            crate::wlog!("ShellExecuteW(open) returned {}; trying notepad", r.0 as isize);
            let quoted: Vec<u16> = format!("\"{}\"", path.display()).encode_utf16().chain(std::iter::once(0)).collect();
            let r2 = ShellExecuteW(None, w!("open"), w!("notepad.exe"), PCWSTR(quoted.as_ptr()), None, SW_SHOWNORMAL);
            if (r2.0 as isize) <= 32 {
                crate::wlog!("ShellExecuteW(notepad) returned {}", r2.0 as isize);
            }
        }
    }
}
