//! OWNER: render agent. Grid geometry and hit testing (pure, no Win32 calls).

use crate::model::{Hit, LayoutConfig, RectF, RectI, Snapshot};

#[derive(Clone, Debug, Default)]
pub struct Layout {
    /// Total pager size in physical pixels.
    pub width: i32,
    pub height: i32,
    /// dpi / 96.
    pub scale: f32,
    /// One rect per desktop, same order as `Snapshot::desktops`.
    pub cells: Vec<RectF>,
    /// Copy of `Snapshot::screen` used for mapping.
    pub screen: RectI,
}

impl Layout {
    /// Compute the grid for `height_px` of available height. Width follows from the
    /// desktop count, `cfg.rows`, and the screen aspect ratio.
    pub fn compute(snapshot: &Snapshot, cfg: &LayoutConfig, height_px: i32, dpi: u32) -> Layout {
        // STUB: single row only.
        let scale = dpi as f32 / 96.0;
        let n = snapshot.desktops.len().max(1);
        let m = cfg.vertical_margin * scale;
        let gap = cfg.spacing * scale;
        let ch = (height_px as f32 - 2.0 * m).max(4.0);
        let aspect = snapshot.screen.width().max(1) as f32 / snapshot.screen.height().max(1) as f32;
        let cw = ch * aspect;
        let cells = (0..n)
            .map(|i| {
                let x = i as f32 * (cw + gap);
                RectF { left: x, top: m, right: x + cw, bottom: m + ch }
            })
            .collect();
        Layout {
            width: (n as f32 * (cw + gap) - gap).ceil() as i32,
            height: height_px,
            scale,
            cells,
            screen: snapshot.screen,
        }
    }

    /// Map a window's screen rect into the cell of desktop `desktop` (not clipped).
    pub fn map_window(&self, desktop: usize, rect: &RectI) -> RectF {
        let c = self.cells[desktop];
        let sx = c.width() / self.screen.width().max(1) as f32;
        let sy = c.height() / self.screen.height().max(1) as f32;
        RectF {
            left: c.left + (rect.left - self.screen.left) as f32 * sx,
            top: c.top + (rect.top - self.screen.top) as f32 * sy,
            right: c.left + (rect.right - self.screen.left) as f32 * sx,
            bottom: c.top + (rect.bottom - self.screen.top) as f32 * sy,
        }
    }

    /// Desktop cell under a client point.
    pub fn desktop_at(&self, x: f32, y: f32) -> Option<usize> {
        self.cells.iter().position(|c| c.contains(x, y))
    }

    /// What is under a client point: topmost window outline first, else the cell.
    pub fn hit_test(&self, snapshot: &Snapshot, x: f32, y: f32) -> Hit {
        let Some(d) = self.desktop_at(x, y) else { return Hit::None };
        for w in snapshot.windows_on(d) {
            if self.map_window(d, &w.rect).contains(x, y) {
                return Hit::Window { desktop: d, hwnd: w.hwnd };
            }
        }
        Hit::Desktop(d)
    }
}
