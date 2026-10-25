//! OWNER: render agent. Grid geometry and hit testing (pure, no Win32 calls).

use crate::model::{Hit, LayoutConfig, RectF, RectI, Snapshot};

/// Bounds for `LayoutConfig::min_cell_width` (the smallest cell width as a fraction of the
/// natural width before desktops start to be
/// hidden behind a sliding window). Cells only ever get narrower, never shorter.
pub const MIN_CELL_WIDTH_RANGE: (f32, f32) = (0.3, 1.0);
const DEFAULT_MIN_CELL_WIDTH: f32 = 0.8;
/// Horizontal padding between the floating widget's backplate edge and the cells, in DIPs
/// (apply with `Layout::pad_x`).
pub const PLATE_PAD_DIP: f32 = 8.0;
/// Width of an overflow marker region in DIPs (the chevron drawn when desktops are hidden).
const OVERFLOW_MARKER_DIP: f32 = 9.0;

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
    /// Slim region left of the first visible cell when desktops before `visible` are hidden.
    /// Hit tests return `Hit::Overflow { forward: .. }` there.
    pub overflow_left: Option<RectF>,
    /// Slim region right of the last visible cell when desktops after `visible` are hidden.
    /// Hit tests return `Hit::Overflow { forward: .. }` there.
    pub overflow_right: Option<RectF>,
    /// Copy of `Snapshot::screen` used for mapping.
    pub screen: RectI,
}

impl Layout {
    /// Compute a single-row strip for `height_px` of available height. Width follows from
    /// the desktop count and the screen aspect ratio, but never exceeds `max_width_px`
    /// (pass `i32::MAX` for no limit).
    ///
    /// 1. Natural size: cell height = `height_px` minus margins (or `cell_height`), cell
    ///    width follows the screen aspect ratio (or `cell_width`).
    /// 2. If all cells do not fit, narrow every cell (width only: the height and top stay)
    ///    down to `LayoutConfig::min_cell_width` of the natural width.
    /// 3. If they still do not fit, show a sliding window of `k` desktops at full height and
    ///    minimum width, containing `snapshot.current` (centered where possible, clamped to
    ///    the ends), with an overflow marker on each side that has hidden desktops.
    /// 4. If not even one minimum-width cell (plus markers) fits, only the current desktop
    ///    is shown, without markers: at full height as wide as fits, or, below the minimum
    ///    width, scaled down uniformly to the available width.
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
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let height = height_px.max(1);
        let n = snapshot.desktops.len();
        let mut layout = Layout {
            width: 0,
            height,
            scale,
            cells: vec![RectF::default(); n],
            visible: 0..0,
            overflow_left: None,
            overflow_right: None,
            screen: snapshot.screen,
        };
        if n == 0 {
            return layout;
        }
        let max_w = max_width_px.max(0);
        let current = snapshot.current.min(n - 1);

        let margin = (cfg.vertical_margin.max(0.0) * scale).round() as i32;
        let gap = (cfg.spacing.max(0.0) * scale).round() as i32;
        let marker_w = ((OVERFLOW_MARKER_DIP * scale).round() as i32).max(4);

        // A configured size is used when it is a positive number; anything else means "auto".
        let fixed = |v: Option<f32>| v.filter(|v| v.is_finite() && *v > 0.0).map(|v| ((v * scale).round() as i32).max(1));
        let n_i = n as i32;
        // Width of `k` cells of width `cw` plus `markers` marker regions.
        let strip_w = |k: i32, cw: i32, markers: i32| {
            k.saturating_mul(cw)
                .saturating_add((k - 1).max(0).saturating_mul(gap))
                .saturating_add(markers * (marker_w + gap))
        };

        let ch_nat = match fixed(cfg.cell_height) {
            Some(h) => h.min(height),
            None => (height - 2 * margin).max(1),
        };
        let cw_nat = fixed(cfg.cell_width).unwrap_or_else(|| {
            let screen_aspect = snapshot.screen.width().max(1) as f32 / snapshot.screen.height().max(1) as f32;
            ((ch_nat as f32 * screen_aspect).round() as i32).max(1)
        });
        // Natural cell shape, kept when the cell has to be scaled down as a whole (step 4).
        let aspect = cw_nat as f32 / ch_nat as f32;
        // Tolerate out-of-range or non-finite config values.
        let frac = if cfg.min_cell_width.is_finite() { cfg.min_cell_width } else { DEFAULT_MIN_CELL_WIDTH }
            .clamp(MIN_CELL_WIDTH_RANGE.0, MIN_CELL_WIDTH_RANGE.1);
        let cw_min = (((cw_nat as f32) * frac).round() as i32).clamp(1, cw_nat);
        let p = Placement { gap, marker_w };

        // (1) natural size, (2) width-only shrink: the widest cell in [cw_min, cw_nat]
        // such that all desktops fit.
        let fit_cw = (max_w.saturating_sub((n_i - 1).saturating_mul(gap)) / n_i).min(cw_nat);
        if fit_cw >= cw_min {
            layout.place(&p, ch_nat, fit_cw, 0..n, false, false);
            return layout;
        }

        // (3) sliding window at full height and minimum width.
        for k in (1..n_i).rev() {
            let start = (current as i32 - (k - 1) / 2).clamp(0, n_i - k);
            let (left, right) = (start > 0, start + k < n_i);
            if strip_w(k, cw_min, left as i32 + right as i32) <= max_w {
                layout.place(&p, ch_nat, cw_min, start as usize..(start + k) as usize, left, right);
                return layout;
            }
        }

        // (4) only the current desktop.
        let (ch, cw) = if cw_min <= max_w {
            (ch_nat, max_w.min(cw_nat))
        } else if max_w >= 1 {
            let ch = ((max_w as f32 / aspect).floor() as i32).clamp(1, ch_nat);
            (ch, ((ch as f32 * aspect).round() as i32).clamp(1, max_w))
        } else {
            return layout; // nothing fits at all: empty strip, nothing visible
        };
        layout.place(&p, ch, cw, current..current + 1, false, false);
        layout
    }

    /// Fill in cells for the desktops in `shown` (whole-pixel rects, vertically centered)
    /// and the optional overflow markers, and set `width`/`visible`.
    fn place(
        &mut self,
        p: &Placement,
        ch: i32,
        cw: i32,
        shown: std::ops::Range<usize>,
        left_marker: bool,
        right_marker: bool,
    ) {
        let top = ((self.height - ch) / 2).max(0);
        let region = |x: i32, w: i32| RectF {
            left: x as f32,
            top: top as f32,
            right: (x + w) as f32,
            bottom: (top + ch) as f32,
        };
        let mut x = 0;
        if left_marker {
            self.overflow_left = Some(region(x, p.marker_w));
            x += p.marker_w + p.gap;
        }
        for i in shown.clone() {
            self.cells[i] = region(x, cw);
            x += cw;
            if i + 1 < shown.end {
                x += p.gap;
            }
        }
        if right_marker {
            x += p.gap;
            self.overflow_right = Some(region(x, p.marker_w));
            x += p.marker_w;
        }
        self.width = x;
        self.visible = shown;
    }

    /// Add `pad` pixels of empty space left and right of the content (cells and markers are
    /// shifted, `width` grows by `2 * pad`). Used for the floating widget's backplate; the
    /// caller subtracts `2 * pad` from the `max_width_px` it passes to `compute`.
    pub fn pad_x(&mut self, pad: i32) {
        let pad = pad.max(0);
        if pad == 0 || self.cells.is_empty() {
            return;
        }
        let dx = pad as f32;
        let shift = |r: &mut RectF| {
            r.left += dx;
            r.right += dx;
        };
        for i in self.visible.clone() {
            if let Some(c) = self.cells.get_mut(i) {
                shift(c);
            }
        }
        self.overflow_left.iter_mut().for_each(shift);
        self.overflow_right.iter_mut().for_each(shift);
        self.width += 2 * pad;
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

    /// Visible desktop cell under a client point (never a hidden desktop).
    pub fn desktop_at(&self, x: f32, y: f32) -> Option<usize> {
        self.visible.clone().find(|&i| self.cells[i].contains(x, y))
    }

    /// What is under a client point: topmost window outline (clipped to its cell) first,
    /// else the cell. Overflow markers give `Hit::Overflow`; gaps are `Hit::None`.
    pub fn hit_test(&self, snapshot: &Snapshot, x: f32, y: f32) -> Hit {
        if self.overflow_left.is_some_and(|r| r.contains(x, y)) {
            return Hit::Overflow { forward: false };
        }
        if self.overflow_right.is_some_and(|r| r.contains(x, y)) {
            return Hit::Overflow { forward: true };
        }
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

struct Placement {
    gap: i32,
    marker_w: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DesktopInfo, WindowInfo};
    use windows::core::GUID;

    fn guid(i: u32) -> GUID {
        GUID::from_u128(i as u128 + 1)
    }

    fn snap_with(n: usize, current: usize, h: i32, windows: Vec<WindowInfo>) -> Snapshot {
        Snapshot {
            desktops: (0..n)
                .map(|i| DesktopInfo { id: guid(i as u32), index: i, name: String::new() })
                .collect(),
            current,
            screen: RectI { left: 0, top: 0, right: 3440, bottom: h },
            windows,
        }
    }

    /// 3440x1440 screen (test geometry that is easy to reason about).
    fn snap(n: usize, windows: Vec<WindowInfo>) -> Snapshot {
        snap_with(n, 0, 1440, windows)
    }

    /// The user's work area: 3440x1392.
    fn wa(n: usize, current: usize) -> Snapshot {
        snap_with(n, current, 1392, vec![])
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

    /// Test geometry, independent of the shipped defaults: 4 px gaps and 10 px margins
    /// (28 px cells in a 48 px taskbar), auto cell size.
    fn cfg() -> LayoutConfig {
        LayoutConfig { spacing: 4.0, vertical_margin: 10.0, ..LayoutConfig::default() }
    }

    /// Natural geometry for a 48 px / 100% DPI taskbar on the 3440x1392 work area, derived
    /// from the test config: (cell height, top, cell width, minimum cell width).
    fn nat() -> (f32, f32, f32, f32) {
        let m = cfg().vertical_margin.round();
        let ch = 48.0 - 2.0 * m;
        let cw = (ch * 3440.0 / 1392.0).round();
        (ch, m, cw, (cw * cfg().min_cell_width).round())
    }

    #[test]
    fn one_row_48px() {
        let s = snap(7, vec![]);
        let l = Layout::compute(&s, &cfg(), 48, 96, i32::MAX);
        assert_eq!(l.cells.len(), 7);
        assert_eq!(l.visible, 0..7);
        assert!(l.overflow_left.is_none() && l.overflow_right.is_none());
        assert_eq!(l.height, 48);
        let c0 = l.cells[0];
        let (ch, top, _, _) = nat();
        assert_eq!(c0.height(), ch);
        assert_eq!(c0.top, top);
        assert_eq!(c0.width(), (ch * 3440.0 / 1440.0).round());
        // Exact content width, no trailing gap.
        assert_eq!(l.width as f32, l.cells[6].right);
        for c in &l.cells {
            assert!(is_int(c.left) && is_int(c.top) && is_int(c.right) && is_int(c.bottom));
            assert_eq!(c.width(), c0.width());
            assert_eq!(c.top, top);
        }
        assert_eq!(l.cells[1].left - l.cells[0].right, 4.0);
    }

    #[test]
    fn scaled_72px() {
        let s = snap(7, vec![]);
        let l = Layout::compute(&s, &cfg(), 72, 144, i32::MAX);
        assert_eq!(l.scale, 1.5);
        let m = (cfg().vertical_margin * 1.5).round();
        assert_eq!(l.cells[0].height(), 72.0 - 2.0 * m);
        assert_eq!(l.cells[0].top, m);
    }

    #[test]
    fn no_limit_work_area() {
        let s = wa(12, 3);
        let l = Layout::compute(&s, &cfg(), 48, 96, i32::MAX);
        assert_eq!(l.visible, 0..12);
        let (ch, _, cw, _) = nat();
        assert_eq!(l.cells[0].height(), ch);
        assert_eq!(l.cells[0].width(), cw);
        assert_eq!(l.width, 12 * cw as i32 + 11 * 4);
        // A limit that is exactly enough changes nothing.
        let l2 = Layout::compute(&s, &cfg(), 48, 96, l.width);
        assert_eq!(l2.cells, l.cells);
        assert_eq!(l2.width, l.width);
    }

    #[test]
    fn shrink_only() {
        // 11 desktops do not fit at the natural width in 680 px -> width-only shrink.
        let (ch, top, cw_nat, cw_min) = nat();
        let s = wa(11, 4);
        assert!(11 * cw_nat as i32 + 10 * 4 > 680);
        let l = Layout::compute(&s, &cfg(), 48, 96, 680);
        assert_eq!(l.visible, 0..11);
        assert!(l.overflow_left.is_none() && l.overflow_right.is_none());
        assert!(l.width <= 680);
        // Height and top are the natural ones; only the width shrank, and not below the configured floor.
        let w = l.cells[0].width();
        assert!((cw_min..cw_nat).contains(&w), "width {w}");
        for c in &l.cells {
            assert_eq!((c.width(), c.height(), c.top), (w, ch, top));
            assert!(is_int(c.left) && is_int(c.right));
        }
        // Widest that fits: one pixel wider would not.
        assert!(11 * (w as i32 + 1) + 10 * 4 > 680);
        assert_eq!(l.width as f32, l.cells[10].right);
        // Windows follow the narrower cell: a half-width tile is half the cell wide.
        let r = l.map_window(0, &RectI { left: 0, top: 0, right: 1720, bottom: 1392 });
        assert!((r.width() - w / 2.0).abs() < 0.01 && r.height() == ch);
    }

    #[test]
    fn shrink_floor_follows_config() {
        // Exactly enough room for 16 cells at the configured floor: shrink, no sliding.
        let (ch, _, _, cw_min) = nat();
        let cwm = cw_min as i32;
        let s = wa(16, 3);
        let l = Layout::compute(&s, &cfg(), 48, 96, 16 * cwm + 15 * 4);
        assert_eq!(l.visible, 0..16);
        assert_eq!(l.cells[0].width(), cw_min);
        assert_eq!(l.cells[0].height(), ch);
        // One pixel less and the strip starts sliding, still at full height.
        let l = Layout::compute(&s, &cfg(), 48, 96, 16 * cwm + 15 * 4 - 1);
        assert!(l.visible.len() < 16);
        let c = l.cells[l.visible.start];
        assert_eq!((c.width(), c.height()), (cw_min, ch));
    }

    #[test]
    fn pad_x_shifts_content_and_hit_tests_follow() {
        let s = wa(12, 6);
        let mut l = Layout::compute(&s, &cfg(), 48, 96, 400);
        let (w0, cells0) = (l.width, l.cells.clone());
        let (a, b) = (l.overflow_left.unwrap(), l.overflow_right.unwrap());
        l.pad_x(8);
        assert_eq!(l.width, w0 + 16);
        for i in l.visible.clone() {
            assert_eq!(l.cells[i].left, cells0[i].left + 8.0);
            assert_eq!(l.cells[i].width(), cells0[i].width());
        }
        for i in (0..12).filter(|i| !l.visible.contains(i)) {
            assert_eq!(l.cells[i], RectF::default());
        }
        assert_eq!(l.overflow_left.unwrap().left, a.left + 8.0);
        assert_eq!(l.overflow_right.unwrap().right, b.right + 8.0);
        // The padding itself is empty; shifted content is hit.
        assert_eq!(l.hit_test(&s, 2.0, 24.0), Hit::None);
        assert_eq!(l.hit_test(&s, l.overflow_left.unwrap().left + 1.0, 24.0), Hit::Overflow { forward: false });
        assert_eq!(l.hit_test(&s, l.width as f32 - 2.0, 24.0), Hit::None);
    }

    #[test]
    fn default_floor_slides_eleven_desktops_in_634px() {
        // Default floor 0.8: the real-taskbar span of 634 px fits only 10 shrunken cells.
        let (ch, _, _, cw_min) = nat();
        let s = wa(11, 5);
        let l = Layout::compute(&s, &cfg(), 48, 96, 634);
        assert!(l.visible.len() < 11, "visible {:?}", l.visible);
        assert!(l.visible.contains(&5));
        assert!(l.overflow_left.is_some() || l.overflow_right.is_some());
        assert!(l.width <= 634);
        let c = l.cells[5];
        assert_eq!((c.width(), c.height()), (cw_min, ch));
        // 10 desktops still fit by shrinking only.
        let l = Layout::compute(&wa(10, 5), &cfg(), 48, 96, 634);
        assert_eq!(l.visible, 0..10);
    }

    #[test]
    fn floor_of_one_never_shrinks() {
        let cfg = LayoutConfig { min_cell_width: 1.0, ..cfg() };
        let (_, _, cw_nat, _) = nat();
        let s = wa(11, 5);
        // 11 natural cells need 11*cw + 40 px; one pixel less and it slides immediately.
        let need = 11 * cw_nat as i32 + 10 * 4;
        let l = Layout::compute(&s, &cfg, 48, 96, need);
        assert_eq!(l.visible, 0..11);
        let l = Layout::compute(&s, &cfg, 48, 96, need - 1);
        assert!(l.visible.len() < 11 && l.visible.contains(&5));
        assert_eq!(l.cells[5].width(), cw_nat);
    }

    #[test]
    fn out_of_range_floor_is_tolerated() {
        let s = wa(11, 5);
        for v in [-1.0f32, 0.0, 0.1, 5.0, f32::NAN, f32::INFINITY] {
            let cfg = LayoutConfig { min_cell_width: v, ..cfg() };
            let l = Layout::compute(&s, &cfg, 48, 96, 634);
            assert!(l.width <= 634, "floor {v}");
            assert!(l.visible.contains(&5), "floor {v}");
        }
        // Below the range behaves like 0.3, above like 1.0.
        let at = |v| Layout::compute(&s, &LayoutConfig { min_cell_width: v, ..cfg() }, 48, 96, 634);
        assert_eq!(at(0.0).cells, at(0.3).cells);
        assert_eq!(at(5.0).cells, at(1.0).cells);
    }

    #[test]
    fn sliding_window_current_at_start() {
        let (ch, _, _, cw_min) = nat();
        let s = wa(20, 0);
        let l = Layout::compute(&s, &cfg(), 48, 96, 680);
        assert_eq!(l.visible.start, 0);
        assert!(l.visible.end < 20 && l.visible.end >= 2);
        assert!(l.overflow_left.is_none());
        let r = l.overflow_right.expect("right marker");
        assert!(l.width <= 680);
        assert_eq!(l.width as f32, r.right);
        // Full height, minimum width (from the config).
        assert_eq!(l.cells[0].height(), ch);
        assert_eq!(l.cells[0].width(), cw_min);
        // Hidden desktops are empty and never hit.
        for i in l.visible.end..20 {
            assert_eq!(l.cells[i], RectF::default());
        }
        assert_eq!(l.desktop_at(r.left + 1.0, r.top + 3.0), None);
        assert_eq!(l.hit_test(&s, r.left + 1.0, r.top + 3.0), Hit::Overflow { forward: true });
        // Just outside the marker (in the gap before it) is nothing.
        assert_eq!(l.hit_test(&s, r.left - 0.5, r.top + 3.0), Hit::None);
        assert_eq!(l.hit_test(&s, r.right, r.top + 3.0), Hit::None);
    }

    #[test]
    fn sliding_window_current_in_middle() {
        let s = wa(20, 10);
        let l = Layout::compute(&s, &cfg(), 48, 96, 680);
        assert!(l.visible.contains(&10));
        assert!(l.visible.start > 0 && l.visible.end < 20);
        let (a, b) = (l.overflow_left.expect("left"), l.overflow_right.expect("right"));
        assert!(a.right < l.cells[l.visible.start].left);
        assert!(b.left > l.cells[l.visible.end - 1].right);
        assert_eq!(a.left, 0.0);
        assert_eq!(l.width as f32, b.right);
        assert!(l.width <= 680);
        assert_eq!(l.cells[10].height(), nat().0);
        // Roughly centered on the current desktop.
        let (before, after) = (10 - l.visible.start, l.visible.end - 1 - 10);
        assert!(before.abs_diff(after) <= 1);
        assert_eq!(l.desktop_at(a.left + 1.0, a.top + 3.0), None);
        assert_eq!(l.hit_test(&s, b.left + 1.0, b.top + 3.0), Hit::Overflow { forward: true });
        assert_eq!(l.hit_test(&s, a.left + 1.0, a.top + 3.0), Hit::Overflow { forward: false });
    }

    #[test]
    fn sliding_window_current_at_end() {
        let s = wa(20, 19);
        let l = Layout::compute(&s, &cfg(), 48, 96, 680);
        assert_eq!(l.visible.end, 20);
        assert!(l.visible.start > 0);
        assert!(l.overflow_right.is_none());
        let a = l.overflow_left.expect("left marker");
        assert_eq!(a.left, 0.0);
        assert_eq!(l.width as f32, l.cells[19].right);
        assert!(l.width <= 680);
        // Hit-testing a hidden desktop's old position finds nothing hidden.
        for x in 0..l.width {
            if let Some(d) = l.desktop_at(x as f32 + 0.5, 24.0) {
                assert!(l.visible.contains(&d));
            }
        }
    }

    #[test]
    fn never_exceeds_max_width() {
        for n in 0..=30usize {
            for current in [0, n / 2, n.saturating_sub(1)] {
                for (h, dpi) in [(48, 96), (72, 144), (60, 120), (10, 96)] {
                    for max in (0..1300).step_by(7).chain([i32::MAX]) {
                        let s = wa(n, current);
                        let l = Layout::compute(&s, &cfg(), h, dpi, max);
                        assert!(l.width <= max, "n={n} cur={current} h={h} max={max} w={}", l.width);
                        assert_eq!(l.cells.len(), n);
                        if n == 0 {
                            continue;
                        }
                        if max >= 1 {
                            assert!(l.visible.contains(&current), "n={n} cur={current} max={max}");
                        }
                        for (i, c) in l.cells.iter().enumerate() {
                            if l.visible.contains(&i) {
                                assert!(c.width() >= 1.0 && c.height() >= 1.0);
                                assert!(c.left >= 0.0 && c.right <= l.width as f32);
                                assert!(c.top >= 0.0 && c.bottom <= l.height as f32);
                                assert!(is_int(c.left) && is_int(c.right));
                            } else {
                                assert_eq!(*c, RectF::default());
                            }
                        }
                        // Markers exist exactly when desktops are hidden on that side
                        // (except in the "current only, no room" fallback).
                        if l.overflow_left.is_some() {
                            assert!(l.visible.start > 0);
                        }
                        if l.overflow_right.is_some() {
                            assert!(l.visible.end < n);
                        }
                        for r in [l.overflow_left, l.overflow_right].into_iter().flatten() {
                            assert!(r.left >= 0.0 && r.right <= l.width as f32);
                        }
                        // Never a hidden desktop under any x.
                        for x in 0..l.width.max(1) {
                            if let Some(d) = l.desktop_at(x as f32 + 0.5, h as f32 / 2.0) {
                                assert!(l.visible.contains(&d));
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn one_desktop_edge_cases() {
        // Plenty of room.
        let l = Layout::compute(&wa(1, 0), &cfg(), 48, 96, i32::MAX);
        assert_eq!((l.visible.clone(), l.width), (0..1, nat().2 as i32));
        // Too narrow for the natural cell but wide enough for the minimum: narrower, same height.
        let l = Layout::compute(&wa(1, 0), &cfg(), 48, 96, 55);
        assert_eq!(l.visible, 0..1);
        assert!(l.width == 55 && l.cells[0].height() == nat().0);
        // Narrower than the minimum cell: as large as fits.
        let l = Layout::compute(&wa(1, 0), &cfg(), 48, 96, 40);
        assert_eq!(l.visible, 0..1);
        assert!(l.width <= 40 && l.width >= 30, "w={}", l.width);
        assert!(l.cells[0].height() >= 1.0);
        // Nothing fits.
        let l = Layout::compute(&wa(1, 0), &cfg(), 48, 96, 0);
        assert_eq!(l.width, 0);
        assert_eq!(l.hit_test(&wa(1, 0), 0.5, 10.0), Hit::None);
        // Many desktops, only the current fits at a reduced size.
        let s = wa(12, 5);
        let l = Layout::compute(&s, &cfg(), 48, 96, 50);
        assert_eq!(l.visible, 5..6);
        assert!(l.width <= 50 && l.overflow_left.is_none() && l.overflow_right.is_none());
        let c = l.cells[5];
        assert!(matches!(l.hit_test(&s, c.left + 2.0, c.top + 2.0), Hit::Desktop(5)));
        // Room for the minimum cell but not for markers: current only, no markers.
        let l = Layout::compute(&s, &cfg(), 48, 96, 60);
        assert_eq!(l.visible, 5..6);
        assert!(l.overflow_left.is_none() && l.overflow_right.is_none());
        assert_eq!(l.width, 60);
        assert_eq!(l.cells[5].height(), nat().0); // full height, as wide as fits
    }

    #[test]
    fn degenerate() {
        let s0 = snap(0, vec![]);
        let l = Layout::compute(&s0, &cfg(), 48, 96, i32::MAX);
        assert!(l.cells.is_empty());
        assert_eq!(l.width, 0);
        assert_eq!(l.hit_test(&s0, 1.0, 1.0), Hit::None);

        for h in [-5, 0, 1, 3, 10] {
            let s = snap(5, vec![]);
            let l = Layout::compute(&s, &cfg(), h, 96, i32::MAX);
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
        let l = Layout::compute(&s, &cfg(), 48, 0, i32::MAX);
        assert!(l.cells.iter().all(|c| c.width().is_finite()));
        let r = l.map_window(0, &RectI { left: 0, top: 0, right: 10, bottom: 10 });
        assert!(r.right.is_finite());
        assert_eq!(l.map_window(99, &RectI::default()), RectF::default());
        // Current out of range is clamped.
        let mut s = snap(3, vec![]);
        s.current = 99;
        let l = Layout::compute(&s, &cfg(), 48, 96, 100);
        assert!(l.visible.contains(&2));
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
        let l = Layout::compute(&s, &cfg(), 48, 96, i32::MAX);
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
        let l = Layout::compute(&s, &cfg(), 48, 96, i32::MAX);
        let c1 = l.cells[1];
        assert_eq!(l.hit_test(&s, c1.left + 3.0, c1.top + 3.0), Hit::Desktop(1));
    }

    #[test]
    fn fixed_cell_height_overrides_margin_and_is_centered() {
        let s = wa(7, 0);
        let c = LayoutConfig { cell_height: Some(36.0), ..cfg() };
        let l = Layout::compute(&s, &c, 48, 96, i32::MAX);
        let c0 = l.cells[0];
        assert_eq!((c0.height(), c0.top), (36.0, 6.0));
        assert_eq!(c0.width(), (36.0f32 * 3440.0 / 1392.0).round());
        // Scales with DPI, and never exceeds the taskbar height.
        let l = Layout::compute(&s, &c, 72, 144, i32::MAX);
        assert_eq!((l.cells[0].height(), l.cells[0].top), (54.0, 9.0));
        let c = LayoutConfig { cell_height: Some(200.0), ..cfg() };
        let l = Layout::compute(&s, &c, 48, 96, i32::MAX);
        assert_eq!((l.cells[0].height(), l.cells[0].top), (48.0, 0.0));
    }

    #[test]
    fn fixed_cell_width_and_windows_follow() {
        let s = wa(7, 0);
        let c = LayoutConfig { cell_width: Some(40.0), ..cfg() };
        let l = Layout::compute(&s, &c, 48, 96, i32::MAX);
        assert_eq!((l.cells[0].width(), l.cells[0].height()), (40.0, nat().0));
        assert_eq!(l.width, 7 * 40 + 6 * 4);
        // A full-screen window fills the cell whatever its shape.
        let r = l.map_window(0, &RectI { left: 0, top: 0, right: 3440, bottom: 1392 });
        assert_eq!((r.width(), r.height()), (40.0, nat().0));
        // Still narrows (down to the floor) when the strip does not fit.
        let l = Layout::compute(&s, &c, 48, 96, 7 * 36 + 6 * 4);
        assert_eq!((l.visible.clone(), l.cells[0].width()), (0..7, 36.0));
    }

    #[test]
    fn non_positive_cell_sizes_mean_auto() {
        let s = wa(7, 0);
        let auto = Layout::compute(&s, &cfg(), 48, 96, i32::MAX);
        for v in [0.0f32, -5.0, f32::NAN, f32::INFINITY] {
            let c = LayoutConfig { cell_height: Some(v), cell_width: Some(v), ..cfg() };
            assert_eq!(Layout::compute(&s, &c, 48, 96, i32::MAX).cells, auto.cells, "{v}");
        }
    }

    #[test]
    fn shipped_defaults_touching_cells() {
        // Defaults: no gap, 7 px margins -> 34 px cells in a 48 px taskbar.
        let l = Layout::compute(&wa(7, 0), &LayoutConfig::default(), 48, 96, i32::MAX);
        assert_eq!((l.cells[0].height(), l.cells[0].top), (34.0, 7.0));
        assert_eq!(l.cells[1].left, l.cells[0].right);
    }
}
