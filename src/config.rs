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
            wrap_scroll: false,
            invert_scroll: false,
        }
    }
}

impl Config {
    /// `%APPDATA%\winpager\config.toml` (override with the `WINPAGER_CONFIG` env var, for tests).
    pub fn path() -> Option<std::path::PathBuf> {
        if let Some(p) = std::env::var_os("WINPAGER_CONFIG") {
            return Some(std::path::PathBuf::from(p));
        }
        let base = std::env::var_os("APPDATA")?;
        Some(std::path::PathBuf::from(base).join("winpager").join("config.toml"))
    }

    /// Load from disk; defaults on missing or invalid file (errors are logged).
    pub fn load() -> Config {
        let Some(path) = Config::path() else {
            crate::wlog!("config: %APPDATA% not set, using defaults");
            return Config::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => c.sanitized(),
                Err(e) => {
                    crate::wlog!("config: invalid {}: {e}; using defaults", path.display());
                    Config::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => {
                crate::wlog!("config: cannot read {}: {e}; using defaults", path.display());
                Config::default()
            }
        }
    }

    /// Clamp values to sane ranges.
    fn sanitized(mut self) -> Config {
        self.layout.spacing = self.layout.spacing.clamp(0.0, 64.0);
        self.layout.vertical_margin = self.layout.vertical_margin.clamp(0.0, 64.0);
        self.offset = self.offset.clamp(-4000, 4000);
        self
    }

    pub fn save(&self) -> anyhow::Result<()> {
        use anyhow::Context;
        let path = Config::path().context("%APPDATA% is not set")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}
