//! OWNER: render agent. Colors derived from the Windows theme (light/dark, accent).
//! The host only calls `Theme::from_system()`; fields are the renderer's business.

use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::{PCWSTR, w};

/// Straight (non-premultiplied) RGBA color, components 0..=1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_rgb8(r: u8, g: u8, b: u8) -> Self {
        Self::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// Multiply the alpha by `k`.
    pub fn faded(self, k: f32) -> Self {
        Self { a: self.a * k, ..self }
    }

    /// Linear blend towards `other` by `t` (alpha included).
    pub fn mix(self, other: Rgba, t: f32) -> Self {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Self::new(l(self.r, other.r), l(self.g, other.g), l(self.b, other.b), l(self.a, other.a))
    }

    pub fn luma(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }
}

#[derive(Clone, Debug)]
pub struct Theme {
    /// True when the taskbar is dark (system mode, not app mode).
    pub dark: bool,
    pub accent: Rgba,
    /// Near-transparent fill over the whole pager so every pixel has alpha >= 1.
    pub background: Rgba,
    pub cell_fill: Rgba,
    pub cell_border: Rgba,
    pub cell_hover_fill: Rgba,
    pub cell_hover_border: Rgba,
    pub current_fill: Rgba,
    pub current_border: Rgba,
    pub window_fill: Rgba,
    pub window_border: Rgba,
    pub fg_window_fill: Rgba,
    pub fg_window_border: Rgba,
    pub window_hover_fill: Rgba,
    pub label: Rgba,
    /// Chevron drawn in the overflow markers when desktops are hidden.
    pub overflow_marker: Rgba,
    pub ghost_fill: Rgba,
    pub ghost_border: Rgba,
    pub drop_fill: Rgba,
    pub drop_border: Rgba,
    /// Backplate of the floating widget (a Fluent-like surface) and its 1 px border.
    pub plate_fill: Rgba,
    pub plate_border: Rgba,
}

impl Theme {
    pub fn from_system() -> Self {
        let dark = !system_uses_light_theme().unwrap_or(false);
        Self::from_parts(dark, system_accent().unwrap_or(Rgba::from_rgb8(0x00, 0x78, 0xD4)))
    }

    pub fn from_parts(dark: bool, accent: Rgba) -> Self {
        let fg = if dark { Rgba::new(1.0, 1.0, 1.0, 1.0) } else { Rgba::new(0.0, 0.0, 0.0, 1.0) };
        let bg = if dark { Rgba::new(0.0, 0.0, 0.0, 1.0) } else { Rgba::new(1.0, 1.0, 1.0, 1.0) };
        // Fluent uses lighter accent shades on dark surfaces and darker ones on light ones.
        let acc = if dark {
            if accent.luma() < 0.45 { accent.mix(Rgba::new(1.0, 1.0, 1.0, 1.0), 0.35) } else { accent }
        } else if accent.luma() > 0.55 {
            accent.mix(Rgba::new(0.0, 0.0, 0.0, 1.0), 0.25)
        } else {
            accent
        };
        let acc = acc.with_alpha(1.0);

        if dark {
            Theme {
                dark,
                accent: acc,
                background: bg.with_alpha(1.0 / 255.0),
                cell_fill: fg.with_alpha(0.07),
                cell_border: fg.with_alpha(0.50),
                cell_hover_fill: fg.with_alpha(0.14),
                cell_hover_border: fg.with_alpha(0.72),
                current_fill: acc.with_alpha(0.26),
                current_border: acc.with_alpha(0.95),
                // Opaque so the desktop label behind them never shows through.
                window_fill: Rgba::from_rgb8(0x4c, 0x4c, 0x4c),
                window_border: fg.with_alpha(0.62),
                fg_window_fill: Rgba::from_rgb8(0x6e, 0x6e, 0x6e),
                fg_window_border: fg.with_alpha(0.88),
                window_hover_fill: Rgba::from_rgb8(0x5c, 0x5c, 0x5c),
                label: fg.with_alpha(0.34),
                overflow_marker: fg.with_alpha(0.70),
                ghost_fill: acc.mix(fg, 0.15).with_alpha(0.55),
                ghost_border: acc.mix(fg, 0.35).with_alpha(1.0),
                drop_fill: acc.with_alpha(0.38),
                drop_border: acc.mix(fg, 0.25).with_alpha(1.0),
                plate_fill: Rgba::from_rgb8(0x2c, 0x2c, 0x2c).with_alpha(0.95),
                plate_border: fg.with_alpha(0.16),
            }
        } else {
            Theme {
                dark,
                accent: acc,
                background: bg.with_alpha(1.0 / 255.0),
                cell_fill: fg.with_alpha(0.07),
                cell_border: fg.with_alpha(0.48),
                cell_hover_fill: fg.with_alpha(0.06),
                cell_hover_border: fg.with_alpha(0.70),
                current_fill: acc.with_alpha(0.22),
                current_border: acc.with_alpha(0.95),
                window_fill: Rgba::from_rgb8(0xfc, 0xfc, 0xfc),
                window_border: fg.with_alpha(0.50),
                fg_window_fill: Rgba::from_rgb8(0xff, 0xff, 0xff),
                fg_window_border: fg.with_alpha(0.78),
                window_hover_fill: Rgba::from_rgb8(0xff, 0xff, 0xff),
                label: fg.with_alpha(0.30),
                overflow_marker: fg.with_alpha(0.62),
                ghost_fill: acc.mix(bg, 0.55).with_alpha(0.75),
                ghost_border: acc.with_alpha(1.0),
                drop_fill: acc.with_alpha(0.32),
                drop_border: acc.with_alpha(1.0),
                plate_fill: Rgba::from_rgb8(0xf3, 0xf3, 0xf3).with_alpha(0.95),
                plate_border: fg.with_alpha(0.14),
            }
        }
    }
}

fn reg_dword(subkey: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `data`/`size` outlive the call and `size` matches the buffer.
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    err.is_ok().then_some(data)
}

fn system_uses_light_theme() -> Option<bool> {
    reg_dword(
        w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
        w!("SystemUsesLightTheme"),
    )
    .map(|v| v != 0)
}

fn system_accent() -> Option<Rgba> {
    if let Some(v) = reg_dword(w!(r"Software\Microsoft\Windows\DWM"), w!("AccentColor")) {
        // ABGR
        let (r, g, b) = ((v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, ((v >> 16) & 0xFF) as u8);
        return Some(Rgba::from_rgb8(r, g, b));
    }
    let mut c = 0u32;
    let mut opaque = BOOL(0);
    // SAFETY: valid out pointers.
    unsafe { DwmGetColorizationColor(&mut c, &mut opaque).ok()? };
    // ARGB
    Some(Rgba::from_rgb8((c >> 16) as u8, (c >> 8) as u8, c as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_theme_does_not_panic() {
        let t = Theme::from_system();
        assert!(t.accent.a == 1.0);
        assert!(t.background.a > 0.0);
    }

    #[test]
    fn both_palettes_have_visible_background() {
        for dark in [true, false] {
            let t = Theme::from_parts(dark, Rgba::from_rgb8(0xE7, 0x48, 0x56));
            assert!(t.background.a >= 1.0 / 255.0);
            assert_eq!(t.dark, dark);
        }
    }
}
