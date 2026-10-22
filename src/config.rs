//! OWNER: host agent. User configuration, stored as TOML in %APPDATA%\winpager\config.toml.

use crate::model::LayoutConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    /// Child window embedded in the taskbar (Shell_TrayWnd).
    Taskbar,
    /// Top-most floating tool window over the taskbar.
    Floating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Anchor {
    /// Right-aligned, just left of the notification area.
    Tray,
    /// Left-aligned, from the left edge of the taskbar.
    Left,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub layout: LayoutConfig,
    pub placement: Placement,
    pub anchor: Anchor,
    /// Distance from the anchor, DIPs.
    pub offset: i32,
    /// Scrolling past the last desktop wraps to the first (and vice versa).
    pub wrap_scroll: bool,
    /// Reverse the scroll direction.
    pub invert_scroll: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            layout: LayoutConfig::default(),
            placement: Placement::Taskbar,
            anchor: Anchor::Tray,
            offset: 8,
            wrap_scroll: true,
            invert_scroll: false,
        }
    }
}

impl Config {
    /// Load from disk; defaults on missing or invalid file.
    pub fn load() -> Config {
        Config::default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        todo!()
    }
}
