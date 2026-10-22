//! OWNER: data agent. Virtual desktop operations (wraps `winvd`).
//! All functions are called from the UI thread.

use crate::model::{DesktopInfo, Hwnd};

/// All desktops in order, plus the index of the current one.
pub fn desktops() -> anyhow::Result<(Vec<DesktopInfo>, usize)> {
    // STUB: minimal real implementation so other modules can test.
    let list = winvd::get_desktops().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let cur = winvd::get_current_desktop()
        .and_then(|d| d.get_id())
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mut out = Vec::new();
    let mut current = 0;
    for (i, d) in list.iter().enumerate() {
        let id = d.get_id().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        if id == cur {
            current = i;
        }
        out.push(DesktopInfo { id, index: i, name: d.get_name().unwrap_or_default() });
    }
    Ok((out, current))
}

pub fn switch_to(index: usize) -> anyhow::Result<()> {
    winvd::switch_desktop(index as u32).map_err(|e| anyhow::anyhow!("{e:?}"))
}

/// Create a new desktop at the end of the list.
pub fn create() -> anyhow::Result<()> {
    todo!()
}

/// Remove desktop `index`; its windows go to a neighbouring desktop.
pub fn remove(index: usize) -> anyhow::Result<()> {
    let _ = index;
    todo!()
}

/// Move a window (of any process) to desktop `index`.
pub fn move_window(hwnd: Hwnd, index: usize) -> anyhow::Result<()> {
    let _ = (hwnd, index);
    todo!()
}

/// Keeps the desktop-event listener alive; dropping it stops listening.
pub struct Listener {
    _private: (),
}

/// Start listening for desktop events (switch, create, destroy, rename, reorder,
/// window moved between desktops). For every event, post `msg` (WPARAM/LPARAM 0)
/// to `notify`. Events may be coalesced.
pub fn listen(notify: Hwnd, msg: u32) -> anyhow::Result<Listener> {
    let _ = (notify, msg);
    Ok(Listener { _private: () })
}
