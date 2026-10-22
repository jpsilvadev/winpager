//! OWNER: data agent. Virtual desktop operations (wraps `winvd`).
//! All functions are called from the UI thread.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};
use std::thread::JoinHandle;

use anyhow::{Context, bail};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::model::{DesktopInfo, Hwnd, to_hwnd};

fn err<E: std::fmt::Debug>(e: E) -> anyhow::Error {
    anyhow::anyhow!("{e:?}")
}

// ---- desktop list cache ---------------------------------------------------------------
//
// Reading the desktop list costs ~2 ms for 7 desktops (every `get_name` re-fetches the
// desktop array), and the host asks on every window event. While a `Listener` is alive,
// every desktop event bumps `GEN`, which invalidates the cache; entries also expire after
// `CACHE_TTL` as a safety net (e.g. events lost while explorer restarts). Without a
// listener nothing is cached.

const CACHE_TTL: Duration = Duration::from_secs(2);
static GEN: AtomicU64 = AtomicU64::new(0);
static LISTENERS: AtomicUsize = AtomicUsize::new(0);
type DesktopList = (Vec<DesktopInfo>, usize);
static CACHE: Mutex<Option<(u64, Instant, DesktopList)>> = Mutex::new(None);

/// All desktops in order, plus the index of the current one.
pub fn desktops() -> anyhow::Result<(Vec<DesktopInfo>, usize)> {
    if LISTENERS.load(Ordering::Acquire) == 0 {
        return read_desktops();
    }
    let gen_before = GEN.load(Ordering::Acquire);
    if let Ok(c) = CACHE.lock()
        && let Some((g, at, v)) = c.as_ref()
        && *g == gen_before
        && at.elapsed() < CACHE_TTL
    {
        return Ok(v.clone());
    }
    let v = read_desktops()?;
    if GEN.load(Ordering::Acquire) == gen_before
        && let Ok(mut c) = CACHE.lock()
    {
        *c = Some((gen_before, Instant::now(), v.clone()));
    }
    Ok(v)
}

fn read_desktops() -> anyhow::Result<(Vec<DesktopInfo>, usize)> {
    let list = winvd::get_desktops().map_err(err).context("listing virtual desktops")?;
    let cur = winvd::get_current_desktop()
        .and_then(|d| d.get_id())
        .map_err(err)
        .context("getting current virtual desktop")?;
    let mut out = Vec::with_capacity(list.len());
    let mut current = 0;
    for (i, d) in list.iter().enumerate() {
        let id = d.get_id().map_err(err).context("getting desktop id")?;
        if id == cur {
            current = i;
        }
        out.push(DesktopInfo { id, index: i, name: d.get_name().unwrap_or_default() });
    }
    Ok((out, current))
}

fn invalidate() {
    GEN.fetch_add(1, Ordering::AcqRel);
}

pub fn switch_to(index: usize) -> anyhow::Result<()> {
    let r = winvd::switch_desktop(index as u32)
        .map_err(err)
        .with_context(|| format!("switching to desktop {index}"));
    invalidate();
    r
}

/// Create a new desktop at the end of the list.
pub fn create() -> anyhow::Result<()> {
    let r = winvd::create_desktop().map(|_| ()).map_err(err).context("creating virtual desktop");
    invalidate();
    r
}

/// Remove desktop `index`; its windows go to a neighbouring desktop
/// (the previous one, or the next one when removing the first).
/// Refuses to remove the last remaining desktop.
pub fn remove(index: usize) -> anyhow::Result<()> {
    let count = winvd::get_desktop_count().map_err(err).context("counting desktops")? as usize;
    if count <= 1 {
        bail!("cannot remove the last remaining desktop");
    }
    if index >= count {
        bail!("desktop {index} does not exist ({count} desktops)");
    }
    let fallback = if index == 0 { 1 } else { index - 1 };
    let r = winvd::remove_desktop(index as u32, fallback as u32)
        .map_err(err)
        .with_context(|| format!("removing desktop {index}"));
    invalidate();
    r
}

/// Move a window (of any process) to desktop `index`.
pub fn move_window(hwnd: Hwnd, index: usize) -> anyhow::Result<()> {
    let h = to_hwnd(hwnd);
    winvd::move_window_to_desktop(index as u32, &h)
        .map_err(err)
        .with_context(|| format!("moving window {hwnd:#x} to desktop {index}"))
}

/// Keeps the desktop-event listener alive; dropping it stops listening.
pub struct Listener {
    events: Option<winvd::DesktopEventThread>,
    forwarder: Option<JoinHandle<()>>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        // Stops and joins the winvd thread; that drops its Sender, which ends the
        // forwarder's recv loop, so the join below cannot hang.
        drop(self.events.take());
        if let Some(h) = self.forwarder.take() {
            let _ = h.join();
        }
        LISTENERS.fetch_sub(1, Ordering::AcqRel);
        invalidate();
    }
}

/// Start listening for desktop events (switch, create, destroy, rename, reorder,
/// window moved between desktops). For every event, post `msg` (WPARAM/LPARAM 0)
/// to `notify`. Events may be coalesced.
pub fn listen(notify: Hwnd, msg: u32) -> anyhow::Result<Listener> {
    let (tx, rx) = mpsc::channel::<winvd::DesktopEvent>();
    let events = winvd::listen_desktop_events(tx)
        .map_err(err)
        .context("starting virtual desktop event listener")?;
    let forwarder = std::thread::Builder::new()
        .name("winpager-vd-events".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                // Collapse bursts into a single notification.
                while rx.try_recv().is_ok() {}
                invalidate();
                unsafe {
                    let _ = PostMessageW(to_hwnd(notify), msg, WPARAM(0), LPARAM(0));
                }
            }
        })
        .context("spawning event forwarder thread")?;
    LISTENERS.fetch_add(1, Ordering::AcqRel);
    invalidate();
    Ok(Listener { events: Some(events), forwarder: Some(forwarder) })
}
