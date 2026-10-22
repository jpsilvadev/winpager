//! Render test harness: draws fake (or, with `--real`, live) snapshots to PNG files so the
//! look of the pager can be reviewed without a window.
//!
//! Usage:
//!   cargo run --example render_demo [-- OUT_DIR]          fake snapshot, all variants
//!   cargo run --example render_demo -- --real [OUT_DIR]   live snapshot via enumerate
//!   cargo run --example render_demo -- --present [MS]     show a layered test window for MS ms
//!
//! Every variant is written composited onto a taskbar-like background, at 1x and 3x
//! (nearest-neighbour upscale).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::core::GUID;
use winpager::layout::Layout;
use winpager::model::*;
use winpager::render::Renderer;
use winpager::theme::{Rgba, Theme};

const SCREEN: RectI = RectI { left: 0, top: 0, right: 3440, bottom: 1392 }; // primary work area (3440x1440 minus the 48 px taskbar)

// ---------------------------------------------------------------------------------------
// Synthetic icons
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Shape {
    RoundedSquare,
    Circle,
    Diamond,
}

/// 32x32 premultiplied BGRA icon: a colored shape with anti-aliased (alpha) edges and a
/// lighter inner mark so down-scaling quality is visible.
fn make_icon(id: u64, shape: Shape, rgb: (u8, u8, u8)) -> Arc<IconImage> {
    const N: u32 = 32;
    let mut px = vec![0u8; (N * N * 4) as usize];
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            // Signed distance (negative inside).
            let d = match shape {
                Shape::RoundedSquare => {
                    let (qx, qy) = (dx.abs() - 11.0, dy.abs() - 11.0);
                    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - 4.0
                }
                Shape::Circle => dx.hypot(dy) - 14.5,
                Shape::Diamond => (dx.abs() + dy.abs()) / std::f32::consts::SQRT_2 - 13.0,
            };
            let cov = (0.5 - d).clamp(0.0, 1.0);
            // Inner lighter mark.
            let inner = ((0.5 - (dx.hypot(dy) - 5.5)).clamp(0.0, 1.0)) * 0.55;
            let (r, g, b) = (rgb.0 as f32, rgb.1 as f32, rgb.2 as f32);
            let (r, g, b) = (r + (255.0 - r) * inner, g + (255.0 - g) * inner, b + (255.0 - b) * inner);
            let o = ((y * N + x) * 4) as usize;
            px[o] = (b * cov).round() as u8;
            px[o + 1] = (g * cov).round() as u8;
            px[o + 2] = (r * cov).round() as u8;
            px[o + 3] = (cov * 255.0).round() as u8;
        }
    }
    Arc::new(IconImage { id, width: N, height: N, bgra_premul: px })
}

fn icons() -> Vec<Arc<IconImage>> {
    use Shape::*;
    vec![
        make_icon(1, RoundedSquare, (0x2b, 0x7c, 0xe6)), // browser-ish blue
        make_icon(2, RoundedSquare, (0x6a, 0x3d, 0xd0)), // editor purple
        make_icon(3, Circle, (0xe8, 0x5d, 0x2a)),        // orange
        make_icon(4, RoundedSquare, (0x22, 0x9e, 0x5a)), // green
        make_icon(5, Diamond, (0xd8, 0xa9, 0x12)),       // yellow
        make_icon(6, Circle, (0xd1, 0x34, 0x4e)),        // red
        make_icon(7, RoundedSquare, (0x30, 0x30, 0x30)), // terminal dark
    ]
}

// ---------------------------------------------------------------------------------------
// Fake snapshot
// ---------------------------------------------------------------------------------------

fn guid(i: usize) -> GUID {
    GUID::from_u128(0x1000 + i as u128)
}

const H: i32 = 1392; // screen height above a 48 px taskbar

fn fake_snapshot() -> Snapshot {
    fake_snapshot_n(7, 4)
}

/// The 7-desktop scene, extended to `n` desktops by repeating the per-desktop layouts.
fn fake_snapshot_n(n: usize, current: usize) -> Snapshot {
    let ic = icons();
    let desktops: Vec<DesktopInfo> = (0..n)
        .map(|i| DesktopInfo {
            id: guid(i),
            index: i,
            name: if i == 2 { "Work".to_string() } else { String::new() },
        })
        .collect();
    let mut next = 100isize;
    let mut win = |desk: Option<usize>, r: [i32; 4], icon: usize, fg: bool| {
        next += 1;
        WindowInfo {
            hwnd: next,
            desktop: desk.map(guid),
            rect: RectI { left: r[0], top: r[1], right: r[2], bottom: r[3] },
            title: format!("Window {next}"),
            icon: Some(ic[icon].clone()),
            is_foreground: fg,
        }
    };
    // Topmost first.
    let windows = vec![
        // Desktop 4 (current): small centered foreground window above two half tiles, plus
        // a pinned window.
        win(Some(4), [1000, 300, 2440, 1000], 2, true),
        win(None, [2700, 900, 3340, 1340], 4, false),
        win(Some(4), [0, 0, 1720, H], 0, false),
        win(Some(4), [1720, 0, 3440, H], 1, false),
        // Desktop 0: two half tiles.
        win(Some(0), [0, 0, 1720, H], 0, false),
        win(Some(0), [1720, 0, 3440, H], 1, false),
        // Desktop 1: maximized terminal.
        win(Some(1), [0, 0, 3440, H], 6, false),
        // Desktop 2 "Work": three overlapping windows.
        win(Some(2), [1400, 500, 3000, 1300], 3, false),
        win(Some(2), [700, 250, 2300, 1100], 5, false),
        win(Some(2), [150, 80, 1750, 900], 1, false),
        // Desktop 5: maximized plus small dialog on top.
        win(Some(5), [1300, 500, 2100, 900], 5, false),
        win(Some(5), [0, 0, 3440, H], 0, false),
        // Desktop 6: single centered window.
        win(Some(6), [860, 200, 2580, 1200], 3, false),
    ];
    // Desktops beyond the 7 hand-made ones reuse the layouts of desktop `i % 7`.
    let mut extra = Vec::new();
    for i in 7..n {
        for w in windows.iter().filter(|w| w.desktop == Some(guid(i % 7))) {
            next += 1;
            extra.push(WindowInfo { hwnd: next, desktop: Some(guid(i)), ..w.clone() });
        }
    }
    let mut windows = windows;
    windows.extend(extra);
    windows.retain(|w| w.desktop.is_none_or(|d| desktops.iter().any(|x| x.id == d)));
    Snapshot { desktops, current: current.min(n.saturating_sub(1)), screen: SCREEN, windows }
}

// ---------------------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------------------

/// Composite premultiplied BGRA over an opaque background (padding included) and upscale.
fn compose(px: &[u8], w: u32, h: u32, bg: [u8; 3], pad: u32, k: u32) -> (u32, u32, Vec<u8>) {
    let (cw, ch) = ((w + 2 * pad) * k, (h + 2 * pad) * k);
    let mut out = vec![0u8; (cw * ch * 4) as usize];
    for y in 0..ch {
        for x in 0..cw {
            let (sx, sy) = (x / k, y / k);
            let o = ((y * cw + x) * 4) as usize;
            let (mut b, mut g, mut r) = (bg[2] as u32, bg[1] as u32, bg[0] as u32);
            if sx >= pad && sy >= pad && sx < pad + w && sy < pad + h {
                let i = (((sy - pad) * w + (sx - pad)) * 4) as usize;
                let inv = 255 - px[i + 3] as u32;
                b = px[i] as u32 + (b * inv + 127) / 255;
                g = px[i + 1] as u32 + (g * inv + 127) / 255;
                r = px[i + 2] as u32 + (r * inv + 127) / 255;
            }
            out[o] = b.min(255) as u8;
            out[o + 1] = g.min(255) as u8;
            out[o + 2] = r.min(255) as u8;
            out[o + 3] = 255;
        }
    }
    (cw, ch, out)
}

struct Case<'a> {
    name: &'a str,
    height: i32,
    dpi: u32,
    dark: bool,
    /// Width budget passed to `Layout::compute`.
    max_w: i32,
    view: fn(&Layout, &Snapshot) -> ViewState,
}

fn no_view(_: &Layout, _: &Snapshot) -> ViewState {
    ViewState::default()
}

fn hover_cell(_: &Layout, _: &Snapshot) -> ViewState {
    ViewState { hover: Some(Hit::Desktop(1)), drag: None }
}

fn hover_window(l: &Layout, s: &Snapshot) -> ViewState {
    // Hover the left tile on desktop 0.
    let c = l.cells[0];
    let hit = l.hit_test(s, c.left + c.width() * 0.2, c.top + c.height() * 0.5);
    ViewState { hover: Some(hit), drag: None }
}

fn hover_marker_left(l: &Layout, s: &Snapshot) -> ViewState {
    let m = l.overflow_left.expect("left marker");
    let hit = l.hit_test(s, m.left + m.width() * 0.5, m.top + m.height() * 0.5);
    assert_eq!(hit, Hit::Overflow { forward: false });
    ViewState { hover: Some(hit), drag: None }
}

fn hover_marker_right(l: &Layout, s: &Snapshot) -> ViewState {
    let m = l.overflow_right.expect("right marker");
    let hit = l.hit_test(s, m.left + m.width() * 0.5, m.top + m.height() * 0.5);
    assert_eq!(hit, Hit::Overflow { forward: true });
    ViewState { hover: Some(hit), drag: None }
}

fn drag_view(l: &Layout, s: &Snapshot) -> ViewState {
    // Dragging the left half tile of the current desktop (index 4) towards desktop 6.
    let src = l.cells[4];
    let hit = l.hit_test(s, src.left + 4.0, src.top + 4.0);
    let Hit::Window { hwnd, .. } = hit else { return ViewState::default() };
    let r = l.map_window(4, &s.windows.iter().find(|w| w.hwnd == hwnd).unwrap().rect);
    let t = l.cells[6];
    let cursor = (t.left + t.width() * 0.55, t.top + t.height() * 0.45);
    ViewState {
        hover: Some(Hit::Desktop(6)),
        drag: Some(DragVisual {
            hwnd,
            source_desktop: 4,
            cursor,
            grab_offset: ((src.left + 4.0 - r.left).round(), (src.top + 4.0 - r.top).round() + 1.0),
            target_desktop: Some(6),
        }),
    }
}

fn write_case(
    r: &mut Renderer,
    out: &Path,
    snap: &Snapshot,
    cfg: &LayoutConfig,
    c: &Case,
) -> Result<()> {
    let layout = Layout::compute(snap, cfg, c.height, c.dpi, c.max_w);
    let theme = Theme::from_parts(c.dark, Rgba::from_rgb8(0x00, 0x78, 0xD4));
    let view = (c.view)(&layout, snap);
    let (w, h, px) = r.render_to_bgra(snap, &layout, &view, &theme, cfg)?;
    let min_alpha = px.chunks(4).map(|p| p[3]).min().unwrap_or(0);
    anyhow::ensure!(min_alpha >= 1, "{}: found a fully transparent pixel", c.name);
    let bg = if c.dark { [0x1f, 0x1f, 0x1f] } else { [0xee, 0xee, 0xee] };
    // Raw (transparent) render, then composites at 1x and 3x.
    r.render_to_png(&out.join(format!("{}_raw.png", c.name)), snap, &layout, &view, &theme, cfg)?;
    for k in [1u32, 3] {
        let (cw, ch, cp) = compose(&px, w, h, bg, 10, k);
        let name = if k == 1 { format!("{}.png", c.name) } else { format!("{}_x3.png", c.name) };
        r.write_png_bgra(&out.join(&name), cw, ch, &cp)?;
    }
    println!("{:<28} {}x{}  min alpha {}", c.name, w, h, min_alpha);
    Ok(())
}

fn main() -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--present") {
        let ms = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1500);
        return present_test(ms);
    }
    let real = args.iter().any(|a| a == "--real");
    let out: PathBuf = args
        .iter()
        .find(|a| !a.starts_with("--") && a.parse::<u32>().is_err())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/render_demo"));
    std::fs::create_dir_all(&out)?;

    let mut r = Renderer::new()?;

    if real {
        let mut cache = winpager::icons::IconCache::new();
        let snap = winpager::enumerate::build_snapshot(&mut cache, &[])?;
        println!("real snapshot: {} desktops, {} windows", snap.desktops.len(), snap.windows.len());
        let sys = Theme::from_system();
        let cfg = LayoutConfig::default();
        let layout = Layout::compute(&snap, &cfg, 48, 96, i32::MAX);
        let (w, h, px) = r.render_to_bgra(&snap, &layout, &ViewState::default(), &sys, &cfg)?;
        let bg = if sys.dark { [0x1f, 0x1f, 0x1f] } else { [0xee, 0xee, 0xee] };
        for k in [1u32, 3] {
            let (cw, ch, cp) = compose(&px, w, h, bg, 10, k);
            let name = if k == 1 { "real.png" } else { "real_x3.png" };
            r.write_png_bgra(&out.join(name), cw, ch, &cp)?;
        }
        println!("wrote {}", out.join("real_x3.png").display());
        return Ok(());
    }

    let snap = fake_snapshot();
    let cases = [
        Case { name: "dark_1row", height: 48, dpi: 96, dark: true, max_w: i32::MAX, view: no_view },
        Case { name: "light_1row", height: 48, dpi: 96, dark: false, max_w: i32::MAX, view: no_view },
        Case { name: "dark_1row_150", height: 72, dpi: 144, dark: true, max_w: i32::MAX, view: no_view },
        Case { name: "light_1row_150", height: 72, dpi: 144, dark: false, max_w: i32::MAX, view: no_view },
        Case { name: "dark_hover_cell", height: 48, dpi: 96, dark: true, max_w: i32::MAX, view: hover_cell },
        Case { name: "dark_hover_window", height: 48, dpi: 96, dark: true, max_w: i32::MAX, view: hover_window },
        Case { name: "light_hover_cell", height: 48, dpi: 96, dark: false, max_w: i32::MAX, view: hover_cell },
        Case { name: "dark_drag", height: 48, dpi: 96, dark: true, max_w: i32::MAX, view: drag_view },
        Case { name: "light_drag", height: 48, dpi: 96, dark: false, max_w: i32::MAX, view: drag_view },
        Case { name: "dark_drag_150", height: 72, dpi: 144, dark: true, max_w: i32::MAX, view: drag_view },
    ];
    for c in &cases {
        let cfg = LayoutConfig::default();
        write_case(&mut r, &out, &snap, &cfg, c)?;
    }
    // Desktop names (off by default now).
    let cfg = LayoutConfig { show_names: true, ..Default::default() };
    let c = Case { name: "dark_names", height: 48, dpi: 96, dark: true, max_w: i32::MAX, view: no_view };
    write_case(&mut r, &out, &snap, &cfg, &c)?;
    // Overflow: a 680 px budget (the free gap on the dev machine) with more desktops.
    let cfg = LayoutConfig::default();
    for dark in [true, false] {
        let t = if dark { "dark" } else { "light" };
        let case = |name: &'static str, h, dpi, max_w| Case { name, height: h, dpi, dark, max_w, view: no_view };
        // 12 desktops: too wide at the natural size, so cells only get narrower.
        for cur in [0usize, 6, 11] {
            let name: &'static str = Box::leak(format!("{t}_overflow12_cur{cur}").into_boxed_str());
            write_case(&mut r, &out, &fake_snapshot_n(12, cur), &cfg, &case(name, 48, 96, 680))?;
        }
        // 20 desktops: even the narrowest cells do not fit, so a sliding window + markers.
        for cur in [0usize, 10, 19] {
            let name: &'static str = Box::leak(format!("{t}_overflow20_cur{cur}").into_boxed_str());
            write_case(&mut r, &out, &fake_snapshot_n(20, cur), &cfg, &case(name, 48, 96, 680))?;
        }
        // Hovered overflow markers (current in the middle: both markers exist).
        for (suffix, view) in [("left", hover_marker_left as fn(&Layout, &Snapshot) -> ViewState), ("right", hover_marker_right)] {
            let name: &'static str = Box::leak(format!("{t}_overflow20_hover_{suffix}").into_boxed_str());
            let c = Case { name, height: 48, dpi: 96, dark, max_w: 680, view };
            write_case(&mut r, &out, &fake_snapshot_n(20, 10), &cfg, &c)?;
        }
        let name: &'static str = Box::leak(format!("{t}_overflow9_shrink").into_boxed_str());
        write_case(&mut r, &out, &fake_snapshot_n(9, 4), &cfg, &case(name, 48, 96, 520))?;
        let name: &'static str = Box::leak(format!("{t}_overflow12_unlimited").into_boxed_str());
        write_case(&mut r, &out, &fake_snapshot_n(12, 6), &cfg, &case(name, 48, 96, i32::MAX))?;
        // 150% DPI: the gap scales too (1020 px).
        let name: &'static str = Box::leak(format!("{t}_overflow20_cur10_150").into_boxed_str());
        write_case(&mut r, &out, &fake_snapshot_n(20, 10), &cfg, &case(name, 72, 144, 1020))?;
    }
    // Option variants: no icons / no names.
    let cfg = LayoutConfig { show_icons: false, show_names: false, ..Default::default() };
    let c = Case { name: "dark_plain", height: 48, dpi: 96, dark: true, max_w: i32::MAX, view: no_view };
    write_case(&mut r, &out, &snap, &cfg, &c)?;
    println!("output in {}", std::fs::canonicalize(&out).unwrap_or(out).display());
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Optional sanity check of `present` on a real layered window of our own
// ---------------------------------------------------------------------------------------

unsafe extern "system" fn wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wp: windows::Win32::Foundation::WPARAM,
    lp: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wp, lp) }
}

fn present_test(ms: u32) -> Result<()> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("WinPagerRenderDemo");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let snap = fake_snapshot();
        let cfg = LayoutConfig::default();
        let layout = Layout::compute(&snap, &cfg, 48, 96, i32::MAX);
        let (x, y) = (300, 300);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            w!("WinPager render demo"),
            WS_POPUP,
            x,
            y,
            layout.width,
            layout.height,
            None,
            None,
            hinst,
            None,
        )?;
        let mut r = Renderer::new()?;
        let theme = Theme::from_system();
        r.present(hwnd, &snap, &layout, &ViewState::default(), &theme, &cfg)
            .context("present")?;
        // Present a second time (reuse path) and with a different size (DIB recreation).
        r.present(hwnd, &snap, &layout, &ViewState::default(), &theme, &cfg)?;
        let l2 = Layout::compute(&snap, &LayoutConfig { ..cfg.clone() }, 48, 96, i32::MAX);
        r.present(hwnd, &snap, &l2, &ViewState::default(), &theme, &cfg)?;
        r.present(hwnd, &snap, &layout, &ViewState::default(), &theme, &cfg)?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, layout.width, layout.height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        println!("layered window at {x},{y} size {}x{} for {ms} ms", layout.width, layout.height);
        let end = std::time::Instant::now() + std::time::Duration::from_millis(ms as u64);
        let mut msg = MSG::default();
        while std::time::Instant::now() < end {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}
