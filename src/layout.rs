//! OWNER: render agent. Grid geometry and hit testing (pure, no Win32 calls).

use crate::model::{Hit, LayoutConfig, RectF, RectI, Snapshot};

#[derive(Clone, Debug, Default)]
pub struct Layout {
    /// Total pager size in physical pixels.
    pub width: i32,
    pub height: i32,
    /// dpi / 96.
    pub scale: f32,
    /// One rect per desktop, same order as `Snapshot::desktops`. Edges are whole pixels.
    /// Desktops outside `visible` have an empty rect (never drawn, never hit).
    pub cells: Vec<RectF>,
    /// Desktop indices that are shown; all of them unless `max_width_px` is too small.
    pub visible: std::ops::Range<usize>,
    /// Copy of `Snapshot::screen` used for mapping.
    pub screen: RectI,
}

impl Layout {
    /// Compute a single-row strip for `height_px` of available height. Width follows from
    /// the desktop count and the screen aspect ratio, but never exceeds `max_width_px`
    /// (pass `i32::MAX` for no limit).
    ///
    /// Cells are snapped to whole pixels, all the same size, and vertically centered.
    /// `width` is the exact content width (no trailing gap).
    pub fn compute(
        snapshot: &Snapshot,
        cfg: &LayoutConfig,
        height_px: i32,
        dpi: u32,
        max_width_px: i32,
    ) -> Layout {
        // STUB: `max_width_px` is not honoured yet.
        let _ = max_width_px;
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let height = height_px.max(1);
        let n = snapshot.desktops.len();
        if n == 0 {
            return Layout {
                width: 0,
                height,
                scale,
                cells: Vec::new(),
                visible: 0..0,
                screen: snapshot.screen,
            };
        }

        let rows = 1usize;
        let cols = n.div_ceil(rows);

        let margin = (cfg.vertical_margin.max(0.0) * scale).round() as i32;
        let gap = (cfg.spacing.max(0.0) * scale).round() as i32;

        let sw = snapshot.screen.width().max(1) as f32;
        let sh = snapshot.screen.height().max(1) as f32;
        let aspect = sw / sh;

        let avail = height - 2 * margin - (rows as i32 - 1) * gap;
        let ch = (avail / rows as i32).max(1);
        let cw = ((ch as f32 * aspect).round() as i32).max(1);

        let grid_h = rows as i32 * ch + (rows as i32 - 1) * gap;
        let top0 = ((height - grid_h) / 2).max(0);

        let cells = (0..n)
            .map(|i| {
                let (r, c) = ((i / cols) as i32, (i % cols) as i32);
                let left = c * (cw + gap);
                let top = top0 + r * (ch + gap);
                RectF {
                    left: left as f32,
                    top: top as f32,
                    right: (left + cw) as f32,
                    bottom: (top + ch) as f32,
                }
            })
            .collect();

        Layout {
            width: cols as i32 * cw + (cols as i32 - 1) * gap,
            height,
            scale,
            cells,
            visible: 0..n,
            screen: snapshot.screen,
        }
    }

    /// Map a window's screen rect into the cell of desktop `desktop` (not clipped).
    /// Returns an empty rect if `desktop` is out of range.
    pub fn map_window(&self, desktop: usize, rect: &RectI) -> RectF {
        let Some(c) = self.cells.get(desktop).copied() else {
            return RectF::default();
        };
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

    /// What is under a client point: topmost window outline (clipped to its cell) first,
    /// else the cell.
    pub fn hit_test(&self, snapshot: &Snapshot, x: f32, y: f32) -> Hit {
        let Some(d) = self.desktop_at(x, y) else { return Hit::None };
        // The point is already inside cell `d`, so the unclipped outline test is equivalent
        // to testing the outline clipped to the cell.
        for w in snapshot.windows_on(d) {
            if self.map_window(d, &w.rect).contains(x, y) {
                return Hit::Window { desktop: d, hwnd: w.hwnd };
            }
        }
        Hit::Desktop(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DesktopInfo, WindowInfo};
    use windows::core::GUID;

    fn guid(i: u32) -> GUID {
        GUID::from_u128(i as u128 + 1)
    }

    fn snap(n: usize, windows: Vec<WindowInfo>) -> Snapshot {
        Snapshot {
            desktops: (0..n)
                .map(|i| DesktopInfo { id: guid(i as u32), index: i, name: String::new() })
                .collect(),
            current: 0,
            screen: RectI { left: 0, top: 0, right: 3440, bottom: 1440 },
            windows,
        }
    }

    fn win(hwnd: isize, desktop: Option<usize>, r: [i32; 4]) -> WindowInfo {
        WindowInfo {
            hwnd,
            desktop: desktop.map(|d| guid(d as u32)),
            rect: RectI { left: r[0], top: r[1], right: r[2], bottom: r[3] },
            title: String::new(),
            icon: None,
            is_foreground: false,
        }
    }

    fn is_int(v: f32) -> bool {
        v.fract() == 0.0
    }

    #[test]
    fn one_row_48px() {
        let s = snap(7, vec![]);
        let cfg = LayoutConfig::default();
        let l = Layout::compute(&s, &cfg, 48, 96, i32::MAX);
        assert_eq!(l.cells.len(), 7);
        assert_eq!(l.height, 48);
        let c0 = l.cells[0];
        assert_eq!(c0.height(), 38.0);
        assert_eq!(c0.top, 5.0);
        assert_eq!(c0.width(), (38.0f32 * 3440.0 / 1440.0).round());
        // Exact content width, no trailing gap.
        assert_eq!(l.width as f32, l.cells[6].right);
        for c in &l.cells {
            assert!(is_int(c.left) && is_int(c.top) && is_int(c.right) && is_int(c.bottom));
            assert_eq!(c.width(), c0.width());
            assert_eq!(c.top, 5.0);
        }
        assert_eq!(l.cells[1].left - l.cells[0].right, 4.0);
    }

    #[test]
    fn scaled_72px() {
        let s = snap(7, vec![]);
        let l = Layout::compute(&s, &LayoutConfig::default(), 72, 144, i32::MAX);
        assert_eq!(l.scale, 1.5);
        // margin 7.5 -> 8, ch = 72 - 16 = 56
        assert_eq!(l.cells[0].height(), 56.0);
        assert_eq!(l.cells[0].top, 8.0);
    }

    #[test]
    fn degenerate() {
        let s0 = snap(0, vec![]);
        let l = Layout::compute(&s0, &LayoutConfig::default(), 48, 96, i32::MAX);
        assert!(l.cells.is_empty());
        assert_eq!(l.width, 0);
        assert_eq!(l.hit_test(&s0, 1.0, 1.0), Hit::None);

        for h in [-5, 0, 1, 3, 10] {
            let s = snap(5, vec![]);
            let l = Layout::compute(&s, &LayoutConfig::default(), h, 96, i32::MAX);
            assert_eq!(l.cells.len(), 5);
            assert!(l.width > 0 && l.height > 0);
            for c in &l.cells {
                assert!(c.width() >= 1.0 && c.height() >= 1.0);
                assert!(c.left.is_finite() && c.top.is_finite());
            }
        }
        // Zero-sized screen and zero dpi must not produce NaN.
        let mut s = snap(2, vec![]);
        s.screen = RectI::default();
        let l = Layout::compute(&s, &LayoutConfig::default(), 48, 0, i32::MAX);
        assert!(l.cells.iter().all(|c| c.width().is_finite()));
        let r = l.map_window(0, &RectI { left: 0, top: 0, right: 10, bottom: 10 });
        assert!(r.right.is_finite());
        assert_eq!(l.map_window(99, &RectI::default()), RectF::default());
    }

    #[test]
    fn hit_test_z_order_and_pinned() {
        // Topmost first: 2 overlaps 1; 3 is pinned (all desktops) and lowest.
        let ws = vec![
            win(2, Some(0), [0, 0, 1720, 1440]),
            win(1, Some(0), [0, 0, 3440, 1440]),
            win(3, None, [1720, 0, 3440, 1440]),
            win(4, Some(1), [0, 0, 3440, 1440]),
        ];
        let s = snap(3, ws);
        let l = Layout::compute(&s, &LayoutConfig::default(), 48, 96, i32::MAX);
        let c0 = l.cells[0];
        let left = (c0.left + 2.0, c0.top + 10.0);
        let right = (c0.right - 2.0, c0.top + 10.0);
        assert_eq!(l.hit_test(&s, left.0, left.1), Hit::Window { desktop: 0, hwnd: 2 });
        assert_eq!(l.hit_test(&s, right.0, right.1), Hit::Window { desktop: 0, hwnd: 1 });

        // Desktop 1: the pinned window (3) is listed before 4 so it wins by z-order.
        let c1 = l.cells[1];
        assert_eq!(
            l.hit_test(&s, c1.right - 2.0, c1.top + 5.0),
            Hit::Window { desktop: 1, hwnd: 3 }
        );
        // Desktop 2: only the pinned window is visible on the right half.
        let c2 = l.cells[2];
        assert_eq!(
            l.hit_test(&s, c2.right - 2.0, c2.top + 5.0),
            Hit::Window { desktop: 2, hwnd: 3 }
        );
        assert_eq!(l.hit_test(&s, c2.left + 2.0, c2.top + 5.0), Hit::Desktop(2));
        // Gap between cells and outside.
        assert_eq!(l.hit_test(&s, c0.right + 1.0, c0.top + 5.0), Hit::None);
        assert_eq!(l.hit_test(&s, -1.0, 0.0), Hit::None);
        assert_eq!(l.desktop_at(c1.left, c1.top), Some(1));
    }

    #[test]
    fn offscreen_window_is_clipped_for_hits() {
        // A window hanging far to the right of desktop 0 must not capture clicks in cell 1.
        let s = snap(2, vec![win(9, Some(0), [3000, 0, 6000, 1440])]);
        let l = Layout::compute(&s, &LayoutConfig::default(), 48, 96, i32::MAX);
        let c1 = l.cells[1];
        assert_eq!(l.hit_test(&s, c1.left + 3.0, c1.top + 3.0), Hit::Desktop(1));
    }
}
