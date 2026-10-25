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
    /// Floating mode: keep the widget above other windows.
    pub floating_topmost: bool,
    /// Floating mode: top-left corner in physical screen pixels; `None` = the default spot
    /// just above the taskbar, right-aligned with the tray.
    pub floating_pos: Option<(i32, i32)>,
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
            floating_topmost: true,
            floating_pos: None,
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
        // Sizes: a positive number is used (clamped), anything else means "auto".
        let size = |v: Option<f32>| v.filter(|v| v.is_finite() && *v > 0.0).map(|v| v.clamp(1.0, 1024.0));
        self.layout.cell_height = size(self.layout.cell_height);
        self.layout.cell_width = size(self.layout.cell_width);
        self.layout.corner_radius =
            if self.layout.corner_radius.is_finite() { self.layout.corner_radius.clamp(0.0, 64.0) } else { 0.0 };
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
    fn floating_options_roundtrip_and_old_files_still_load() {
        // An older file without the floating keys keeps working.
        let c: Config = toml::from_str("placement = \"floating\"\n[layout]\nspacing = 3.0\n").unwrap();
        assert!(c.floating_topmost);
        assert_eq!(c.floating_pos, None);
        let c = Config { floating_pos: Some((-120, 800)), floating_topmost: false, ..Config::default() };
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.floating_pos, Some((-120, 800)));
        assert!(!back.floating_topmost);
        // `None` is simply omitted.
        assert!(!toml::to_string_pretty(&Config::default()).unwrap().contains("floating_pos"));
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

    #[test]
    fn cell_size_and_corner_radius() {
        // Defaults: auto size, square corners; `None` sizes are omitted from the file.
        let d = Config::default();
        assert_eq!((d.layout.cell_height, d.layout.cell_width, d.layout.corner_radius), (None, None, 0.0));
        let text = toml::to_string_pretty(&d).unwrap();
        // (Line starts: `min_cell_width` also contains "cell_width".)
        assert!(!text.contains("\ncell_height") && !text.contains("\ncell_width"));
        let c: Config = toml::from_str("[layout]\ncell_height = 32.0\ncell_width = 60\ncorner_radius = 4\n").unwrap();
        let c = c.sanitized();
        assert_eq!((c.layout.cell_height, c.layout.cell_width, c.layout.corner_radius), (Some(32.0), Some(60.0), 4.0));
        // Zero, negative and non-finite sizes mean auto; the radius is clamped.
        let mut c = Config::default();
        c.layout.cell_height = Some(0.0);
        c.layout.cell_width = Some(f32::NAN);
        c.layout.corner_radius = -3.0;
        let c = c.sanitized();
        assert_eq!((c.layout.cell_height, c.layout.cell_width, c.layout.corner_radius), (None, None, 0.0));
    }
}
