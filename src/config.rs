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
pub enum ChevronMode {
    /// Switch one desktop towards the clicked side (the strip slides one desktop per click).
    Step,
    /// Switch to the nearest hidden desktop on the clicked side.
    Jump,
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
    /// What clicking an overflow chevron does.
    pub chevron: ChevronMode,
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
            chevron: ChevronMode::Step,
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
        self.layout.min_cell_width = if self.layout.min_cell_width.is_finite() {
            self.layout.min_cell_width.clamp(0.3, 1.0)
        } else {
            LayoutConfig::default().min_cell_width
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chevron_is_lowercase_and_defaults_to_step() {
        assert_eq!(Config::default().chevron, ChevronMode::Step);
        let c: Config = toml::from_str("chevron = \"jump\"").unwrap();
        assert_eq!(c.chevron, ChevronMode::Jump);
        assert!(toml::to_string(&Config::default()).unwrap().contains("chevron = \"step\""));
    }

    #[test]
    fn min_cell_width_is_clamped() {
        let mut c = Config::default();
        assert_eq!(c.layout.min_cell_width, 0.8);
        for (v, want) in [(0.0, 0.3), (9.0, 1.0), (0.5, 0.5)] {
            c.layout.min_cell_width = v;
            assert_eq!(c.clone().sanitized().layout.min_cell_width, want);
        }
        c.layout.min_cell_width = f32::NAN;
        assert_eq!(c.sanitized().layout.min_cell_width, 0.8);
    }
}
