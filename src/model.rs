//! Shared data types. This file is the contract between the data layer
//! (vd / enumerate / icons), the renderer (layout / render / theme) and the host.
//! Change it only in coordination with every module that uses it.

use std::sync::Arc;

use windows::Win32::Foundation::HWND;
use windows::core::GUID;

/// Raw window handle as an integer, so model types stay `Send`.
pub type Hwnd = isize;

pub fn to_hwnd(h: Hwnd) -> HWND {
    HWND(h as *mut _)
}

pub fn from_hwnd(h: HWND) -> Hwnd {
    h.0 as isize
}

/// Integer rectangle in physical screen pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RectI {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl RectI {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn intersects(&self, o: &RectI) -> bool {
        self.left < o.right && o.left < self.right && self.top < o.bottom && o.top < self.bottom
    }
}

/// Float rectangle in client pixels of the pager window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl RectF {
    pub fn width(&self) -> f32 {
        self.right - self.left
    }
    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopInfo {
    pub id: GUID,
    /// Zero-based position in the desktop list.
    pub index: usize,
    /// User-assigned name; empty when the user never named it.
    pub name: String,
}

impl DesktopInfo {
    /// Name to show in UI: the user's name, or "Desktop N" (1-based).
    pub fn display_name(&self) -> String {
        if self.name.is_empty() {
            format!("Desktop {}", self.index + 1)
        } else {
            self.name.clone()
        }
    }
}

/// Icon pixels: 32-bit BGRA, premultiplied alpha, top-down rows, stride = width * 4.
#[derive(Debug)]
pub struct IconImage {
    /// Unique per distinct image; renderers may cache GPU bitmaps by this id.
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub bgra_premul: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: Hwnd,
    /// Desktop the window lives on; `None` means pinned (visible on all desktops).
    pub desktop: Option<GUID>,
    /// Visible frame bounds (DWMWA_EXTENDED_FRAME_BOUNDS), physical screen pixels.
    pub rect: RectI,
    pub title: String,
    pub icon: Option<Arc<IconImage>>,
    /// True for the current foreground window.
    pub is_foreground: bool,
}

/// Everything the renderer needs for one frame.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub desktops: Vec<DesktopInfo>,
    /// Index into `desktops` of the current desktop.
    pub current: usize,
    /// Reference screen area (primary monitor work area, i.e. without the taskbar, physical
    /// pixels). Window rects are mapped from this area into each desktop cell.
    pub screen: RectI,
    /// Non-minimized, user-visible top-level windows that intersect `screen`,
    /// in z-order, TOPMOST FIRST.
    pub windows: Vec<WindowInfo>,
}

impl Snapshot {
    /// Windows shown in the cell of desktop `index`, topmost first (pinned windows included).
    pub fn windows_on(&self, index: usize) -> impl Iterator<Item = &WindowInfo> {
        let id = self.desktops.get(index).map(|d| d.id);
        self.windows
            .iter()
            .filter(move |w| w.desktop.is_none() || w.desktop == id)
    }
}

/// Visual layout options (sizes in DIPs; multiply by dpi/96 for pixels).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    /// Gap between cells.
    pub spacing: f32,
    /// Margin above and below the grid inside the taskbar.
    pub vertical_margin: f32,
    /// Draw app icons inside window outlines.
    pub show_icons: bool,
    /// Draw the desktop name/number inside each cell (behind the windows).
    pub show_names: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            spacing: 4.0,
            vertical_margin: 5.0,
            show_icons: true,
            show_names: false,
        }
    }
}

/// What lies under a point of the pager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    /// Empty area of a desktop cell.
    Desktop(usize),
    /// A window outline (topmost one under the point) inside a desktop cell.
    Window { desktop: usize, hwnd: Hwnd },
    /// An overflow marker (shown when desktops are hidden by the sliding window).
    /// `forward == false` is the left marker (hidden desktops before the visible range).
    Overflow { forward: bool },
}

/// Transient UI state the host passes to the renderer.
#[derive(Clone, Debug, Default)]
pub struct ViewState {
    pub hover: Option<Hit>,
    pub drag: Option<DragVisual>,
}

/// A window outline being dragged to another desktop.
#[derive(Clone, Copy, Debug)]
pub struct DragVisual {
    pub hwnd: Hwnd,
    /// Desktop cell the drag started from.
    pub source_desktop: usize,
    /// Cursor position in client pixels.
    pub cursor: (f32, f32),
    /// Cursor offset from the outline's top-left at drag start, client pixels.
    pub grab_offset: (f32, f32),
    /// Desktop cell currently under the cursor, if any.
    pub target_desktop: Option<usize>,
}
