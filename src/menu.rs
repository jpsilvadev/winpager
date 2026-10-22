//! OWNER: host agent. Right-click context menu and the "Start with Windows" registry value.

use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCSTR, PCWSTR, w};

use crate::config::Placement;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    AddDesktop,
    RemoveDesktop,
    ShowIcons,
    ShowNames,
    Place(Placement),
    Autostart,
    OpenConfig,
    Exit,
}

const ID_ADD: usize = 1;
const ID_REMOVE: usize = 2;
const ID_ICONS: usize = 20;
const ID_NAMES: usize = 21;
const ID_PLACE_TASKBAR: usize = 30;
const ID_PLACE_FLOATING: usize = 31;
const ID_AUTOSTART: usize = 40;
const ID_CONFIG: usize = 41;
const ID_EXIT: usize = 99;

/// What the menu needs to know to label and check its items.
pub struct MenuState {
    /// Desktop that "Remove this desktop" would remove, if any.
    pub removable: Option<usize>,
    pub show_icons: bool,
    pub show_names: bool,
    pub placement: Placement,
    pub autostart: bool,
}

fn flag(on: bool) -> MENU_ITEM_FLAGS {
    if on { MF_CHECKED } else { MF_UNCHECKED }
}

/// Show the menu at screen point `pt`; returns the chosen command. `owner` must be a
/// top-level window of this process (it is made foreground so the menu dismisses properly).
pub fn show(owner: HWND, pt: (i32, i32), st: &MenuState) -> Option<Cmd> {
    unsafe {
        let menu = match CreatePopupMenu() {
            Ok(m) => m,
            Err(e) => {
                crate::wlog!("menu: CreatePopupMenu failed: {e}");
                return None;
            }
        };
        let place_menu = CreatePopupMenu().ok();

        let mut ok = true;
        let mut add = |m: HMENU, fl: MENU_ITEM_FLAGS, id: usize, text: PCWSTR| {
            if let Err(e) = AppendMenuW(m, fl, id, text) {
                ok = false;
                crate::wlog!("menu: AppendMenuW failed: {e}");
            }
        };

        add(menu, MF_STRING, ID_ADD, w!("Add desktop"));
        let remove_fl = if st.removable.is_some() { MF_STRING } else { MF_STRING | MF_GRAYED };
        add(menu, remove_fl, ID_REMOVE, w!("Remove this desktop"));
        add(menu, MF_SEPARATOR, 0, PCWSTR::null());

        add(menu, MF_STRING | flag(st.show_icons), ID_ICONS, w!("Show icons"));
        add(menu, MF_STRING | flag(st.show_names), ID_NAMES, w!("Show names"));
        if let Some(pm) = place_menu {
            add(pm, MF_STRING, ID_PLACE_TASKBAR, w!("Taskbar"));
            add(pm, MF_STRING, ID_PLACE_FLOATING, w!("Floating"));
            let sel = match st.placement {
                Placement::Taskbar => ID_PLACE_TASKBAR,
                Placement::Floating => ID_PLACE_FLOATING,
            };
            let _ = CheckMenuRadioItem(
                pm,
                ID_PLACE_TASKBAR as u32,
                ID_PLACE_FLOATING as u32,
                sel as u32,
                MF_BYCOMMAND.0,
            );
            add(menu, MF_STRING | MF_POPUP, pm.0 as usize, w!("Placement"));
        }
        add(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(menu, MF_STRING | flag(st.autostart), ID_AUTOSTART, w!("Start with Windows"));
        add(menu, MF_STRING, ID_CONFIG, w!("Open config file"));
        add(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(menu, MF_STRING, ID_EXIT, w!("Exit"));
        let _ = ok;

        // TrackPopupMenu only dismisses on an outside click if the owner is foreground.
        let _ = SetForegroundWindow(owner);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
            pt.0,
            pt.1,
            0,
            owner,
            None,
        );
        let _ = PostMessageW(owner, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu); // also destroys the submenus
        match chosen.0 as usize {
            0 => None,
            ID_ADD => Some(Cmd::AddDesktop),
            ID_REMOVE => Some(Cmd::RemoveDesktop),
            ID_ICONS => Some(Cmd::ShowIcons),
            ID_NAMES => Some(Cmd::ShowNames),
            ID_PLACE_TASKBAR => Some(Cmd::Place(Placement::Taskbar)),
            ID_PLACE_FLOATING => Some(Cmd::Place(Placement::Floating)),
            ID_AUTOSTART => Some(Cmd::Autostart),
            ID_CONFIG => Some(Cmd::OpenConfig),
            ID_EXIT => Some(Cmd::Exit),
            _ => None,
        }
    }
}

/// Opt in to dark popup menus (undocumented uxtheme ordinal 135, `SetPreferredAppMode`).
pub fn enable_dark_menus() {
    unsafe {
        let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        // Ordinal 135 = SetPreferredAppMode(int mode); 1 = AllowDark.
        if let Some(f) = GetProcAddress(lib, PCSTR(135usize as *const u8)) {
            let f: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            f(1);
        }
        // Ordinal 136 = FlushMenuThemes.
        if let Some(f) = GetProcAddress(lib, PCSTR(136usize as *const u8)) {
            let f: unsafe extern "system" fn() = std::mem::transmute(f);
            f();
        }
    }
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("WinPager");

pub fn autostart_enabled() -> bool {
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None)
            == ERROR_SUCCESS
    }
}

pub fn set_autostart(on: bool) -> anyhow::Result<()> {
    unsafe {
        if on {
            let exe = std::env::current_exe()?;
            let value = format!("\"{}\"", exe.display());
            let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
            let err = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(wide.as_ptr() as *const _),
                (wide.len() * 2) as u32,
            );
            if err != ERROR_SUCCESS {
                anyhow::bail!("RegSetKeyValueW failed: {err:?}");
            }
        } else {
            let err = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
            if err != ERROR_SUCCESS && err != ERROR_FILE_NOT_FOUND {
                anyhow::bail!("RegDeleteKeyValueW failed: {err:?}");
            }
        }
    }
    Ok(())
}
