//! OWNER: render agent. Draws the pager with Direct2D into a premultiplied 32-bit DIB
//! and pushes it to a WS_EX_LAYERED window with UpdateLayeredWindow.
//!
//! All drawing goes through [`draw_frame`], which only needs an `ID2D1RenderTarget`, so the
//! same code renders to the layered window (DC render target on a DIB section) and to a
//! WIC bitmap (`render_to_png` / `render_to_bgra`). Resources that belong to a render
//! target (brush, icon bitmaps) live in [`TargetResources`] and are recreated with it.
//!
//! Coordinates: the render target runs at 96 DPI so 1 unit = 1 physical pixel; sizes in
//! DIPs are multiplied by `Layout::scale` here.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, anyhow};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HRESULT, Interface, PCWSTR, w};

use crate::layout::Layout;
use crate::model::{DragVisual, Hit, IconImage, LayoutConfig, RectF, Snapshot, ViewState};
use crate::theme::{Rgba, Theme};

/// `D2DERR_RECREATE_TARGET`: the render target must be recreated.
const D2DERR_RECREATE_TARGET: HRESULT = HRESULT(0x8899000Cu32 as i32);

const PIXEL_FORMAT: D2D1_PIXEL_FORMAT = D2D1_PIXEL_FORMAT {
    format: DXGI_FORMAT_B8G8R8A8_UNORM,
    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
};

// ---------------------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------------------

pub struct Renderer {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    wic: IWICImagingFactory,
    font_family: Vec<u16>,
    /// Text format cached for one font size (bit pattern of the f32 size).
    text_format: Option<(u32, IDWriteTextFormat)>,
    dib: Option<Dib>,
    target: Option<DcTarget>,
}

struct DcTarget {
    rt: ID2D1DCRenderTarget,
    res: TargetResources,
}

/// 32-bpp top-down DIB section selected into a memory DC.
struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    width: i32,
    height: i32,
}

impl Dib {
    fn new(width: i32, height: i32) -> anyhow::Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(anyhow!("CreateCompatibleDC failed"));
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = match CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) => b,
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(e).context("CreateDIBSection");
                }
            };
            let old = SelectObject(dc, bmp);
            Ok(Self { dc, bmp, old, width, height })
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.dc);
        }
    }
}

impl Renderer {
    /// Creates the factories. COM must already be initialized on this thread.
    pub fn new() -> anyhow::Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .context("D2D1CreateFactory")?;
            let dwrite: IDWriteFactory =
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).context("DWriteCreateFactory")?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                    .context("CoCreateInstance(WICImagingFactory); is COM initialized?")?;
            let font_family = pick_font_family(&dwrite);
            Ok(Self { d2d, dwrite, wic, font_family, text_format: None, dib: None, target: None })
        }
    }

    /// Render a frame of size `layout.width` x `layout.height` and apply it to the layered
    /// window `hwnd` (UpdateLayeredWindow with pptDst = None; the host positions the window).
    /// Every pixel has alpha >= 1 so the whole pager receives mouse input.
    pub fn present(
        &mut self,
        hwnd: HWND,
        snapshot: &Snapshot,
        layout: &Layout,
        view: &ViewState,
        theme: &Theme,
        cfg: &LayoutConfig,
    ) -> anyhow::Result<()> {
        let (w, h) = (layout.width.max(1), layout.height.max(1));
        let recreate_dib = self.dib.as_ref().is_none_or(|d| d.width != w || d.height != h);
        if recreate_dib {
            // The render target is bound to the old DC; release it first.
            self.target = None;
            self.dib = None;
            self.dib = Some(Dib::new(w, h)?);
        }

        let mut last_err = None;
        for _ in 0..2 {
            match self.draw_to_dib(w, h, snapshot, layout, view, theme, cfg) {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                    self.target = None;
                    last_err = Some(e);
                }
                Err(e) => return Err(e).context("Direct2D drawing failed"),
            }
        }
        if let Some(e) = last_err {
            return Err(e).context("Direct2D target could not be recreated");
        }

        let dib = self.dib.as_ref().expect("dib exists");
        unsafe {
            let size = SIZE { cx: w, cy: h };
            let src = POINT::default();
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
                ..Default::default()
            };
            UpdateLayeredWindow(
                hwnd,
                None,
                None,
                Some(&size),
                dib.dc,
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
            .context("UpdateLayeredWindow")?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_to_dib(
        &mut self,
        w: i32,
        h: i32,
        snapshot: &Snapshot,
        layout: &Layout,
        view: &ViewState,
        theme: &Theme,
        cfg: &LayoutConfig,
    ) -> windows::core::Result<()> {
        let dc = self.dib.as_ref().expect("dib exists").dc;
        if self.target.is_none() {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: PIXEL_FORMAT,
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = unsafe { self.d2d.CreateDCRenderTarget(&props)? };
            let res = TargetResources::new(&rt.cast::<ID2D1RenderTarget>()?)?;
            self.target = Some(DcTarget { rt, res });
        }
        let font_px = label_font_px(layout);
        let format = self.format_for(font_px);
        let target = self.target.as_mut().expect("target exists");
        let rect = RECT { left: 0, top: 0, right: w, bottom: h };
        unsafe { target.rt.BindDC(dc, &rect)? };
        let rt: ID2D1RenderTarget = target.rt.cast()?;
        let text = TextContext { factory: &self.dwrite, format: format.as_ref() };
        draw_frame(&rt, &mut target.res, &text, snapshot, layout, view, theme, cfg)
    }

    fn format_for(&mut self, size: f32) -> Option<IDWriteTextFormat> {
        if size < MIN_LABEL_PX {
            return None;
        }
        let key = size.to_bits();
        if let Some((k, f)) = &self.text_format
            && *k == key {
                return Some(f.clone());
            }
        let f = unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(
                    PCWSTR(self.font_family.as_ptr()),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )
                .ok()?;
            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            f
        };
        self.text_format = Some((key, f.clone()));
        Some(f)
    }

    /// Same drawing as `present`, written to a PNG (for tests and visual review).
    /// The PNG has straight alpha, i.e. what the layered window composites.
    pub fn render_to_png(
        &mut self,
        path: &Path,
        snapshot: &Snapshot,
        layout: &Layout,
        view: &ViewState,
        theme: &Theme,
        cfg: &LayoutConfig,
    ) -> anyhow::Result<()> {
        let (w, h, px) = self.render_to_bgra(snapshot, layout, view, theme, cfg)?;
        self.write_png_bgra(path, w, h, &px)
    }

    /// Renders a frame offscreen. Returns `(width, height, premultiplied BGRA, stride = w*4)`.
    pub fn render_to_bgra(
        &mut self,
        snapshot: &Snapshot,
        layout: &Layout,
        view: &ViewState,
        theme: &Theme,
        cfg: &LayoutConfig,
    ) -> anyhow::Result<(u32, u32, Vec<u8>)> {
        let (w, h) = (layout.width.max(1) as u32, layout.height.max(1) as u32);
        unsafe {
            let bitmap = self
                .wic
                .CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: PIXEL_FORMAT,
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = self.d2d.CreateWicBitmapRenderTarget(&bitmap, &props)?;
            let mut res = TargetResources::new(&rt)?;
            let format = self.format_for(label_font_px(layout));
            let text = TextContext { factory: &self.dwrite, format: format.as_ref() };
            draw_frame(&rt, &mut res, &text, snapshot, layout, view, theme, cfg)
                .context("drawing offscreen")?;
            drop(res);
            drop(rt);

            let lock = bitmap.Lock(
                &WICRect { X: 0, Y: 0, Width: w as i32, Height: h as i32 },
                WICBitmapLockRead.0 as u32,
            )?;
            let stride = lock.GetStride()?;
            let mut size = 0u32;
            let mut ptr = std::ptr::null_mut();
            lock.GetDataPointer(&mut size, &mut ptr)?;
            let src = std::slice::from_raw_parts(ptr, size as usize);
            let row = (w * 4) as usize;
            let mut out = Vec::with_capacity(row * h as usize);
            for y in 0..h as usize {
                out.extend_from_slice(&src[y * stride as usize..y * stride as usize + row]);
            }
            Ok((w, h, out))
        }
    }

    /// Writes premultiplied BGRA pixels as a PNG (straight alpha) through WIC.
    pub fn write_png_bgra(&self, path: &Path, w: u32, h: u32, bgra_premul: &[u8]) -> anyhow::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        if bgra_premul.len() < (w * h * 4) as usize {
            return Err(anyhow!("pixel buffer too small"));
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let bitmap = self.wic.CreateBitmapFromMemory(
                w,
                h,
                &GUID_WICPixelFormat32bppPBGRA,
                w * 4,
                &bgra_premul[..(w * h * 4) as usize],
            )?;
            let stream = self.wic.CreateStream()?;
            stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0)?;
            let encoder = self.wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame: Option<IWICBitmapFrameEncode> = None;
            encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
            let frame = frame.ok_or_else(|| anyhow!("no PNG frame"))?;
            frame.Initialize(None)?;
            frame.SetSize(w, h)?;
            let mut fmt = GUID_WICPixelFormat32bppBGRA;
            frame.SetPixelFormat(&mut fmt)?;
            frame.WriteSource(&bitmap, std::ptr::null())?;
            frame.Commit()?;
            encoder.Commit()?;
        }
        Ok(())
    }
}

fn pick_font_family(dwrite: &IDWriteFactory) -> Vec<u16> {
    let candidates = [w!("Segoe UI Variable Text"), w!("Segoe UI Variable"), w!("Segoe UI")];
    unsafe {
        let mut coll = None;
        if dwrite.GetSystemFontCollection(&mut coll, false).is_ok()
            && let Some(coll) = coll {
                for c in candidates {
                    let (mut idx, mut exists) = (0u32, BOOL(0));
                    if coll.FindFamilyName(c, &mut idx, &mut exists).is_ok() && exists.as_bool() {
                        return c.as_wide().iter().copied().chain(Some(0)).collect();
                    }
                }
            }
        w!("Segoe UI").as_wide().iter().copied().chain(Some(0)).collect()
    }
}

// ---------------------------------------------------------------------------------------
// Per-target resources
// ---------------------------------------------------------------------------------------

/// Icon bitmap cache key: icon id and the pixel size it was prescaled to.
type IconKey = (u64, u32, u32);

struct TargetResources {
    brush: ID2D1SolidColorBrush,
    icons: HashMap<IconKey, ID2D1Bitmap>,
    used: HashSet<IconKey>,
}

impl TargetResources {
    fn new(rt: &ID2D1RenderTarget) -> windows::core::Result<Self> {
        let brush = unsafe { rt.CreateSolidColorBrush(&d2d_color(Rgba::new(0.0, 0.0, 0.0, 1.0)), None)? };
        Ok(Self { brush, icons: HashMap::new(), used: HashSet::new() })
    }

    /// Bitmap of `icon` prescaled (area/tri-linear filtered) to fit `size` x `size`, so it
    /// can be drawn 1:1 on whole pixels. Returns the bitmap and its pixel size.
    fn icon_bitmap(
        &mut self,
        rt: &ID2D1RenderTarget,
        icon: &IconImage,
        size: u32,
    ) -> Option<(ID2D1Bitmap, u32, u32)> {
        if icon.width == 0 || icon.height == 0 || size == 0 {
            return None;
        }
        if icon.bgra_premul.len() < (icon.width * icon.height * 4) as usize {
            return None;
        }
        let (dw, dh) = if icon.width >= icon.height {
            (size, (size * icon.height / icon.width).max(1))
        } else {
            ((size * icon.width / icon.height).max(1), size)
        };
        let key = (icon.id, dw, dh);
        self.used.insert(key);
        if let Some(b) = self.icons.get(&key) {
            return Some((b.clone(), dw, dh));
        }
        let px = resample_premul(&icon.bgra_premul, icon.width, icon.height, dw, dh);
        let props = D2D1_BITMAP_PROPERTIES { pixelFormat: PIXEL_FORMAT, dpiX: 96.0, dpiY: 96.0 };
        let bmp = unsafe {
            rt.CreateBitmap(
                D2D_SIZE_U { width: dw, height: dh },
                Some(px.as_ptr() as *const _),
                dw * 4,
                &props,
            )
            .ok()?
        };
        self.icons.insert(key, bmp.clone());
        Some((bmp, dw, dh))
    }

    /// Drop cached bitmaps that were not requested during the last frame.
    fn end_frame(&mut self) {
        let used = std::mem::take(&mut self.used);
        self.icons.retain(|k, _| used.contains(k));
    }
}

struct TextContext<'a> {
    factory: &'a IDWriteFactory,
    format: Option<&'a IDWriteTextFormat>,
}

// ---------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------

const MIN_LABEL_PX: f32 = 7.5;

fn cell_height(layout: &Layout) -> f32 {
    layout.cells.first().map_or(0.0, |c| c.height())
}

/// Desktop label font size in pixels for this layout (rounded to half pixels).
fn label_font_px(layout: &Layout) -> f32 {
    (cell_height(layout) * 0.30 * 2.0).round() / 2.0
}

fn d2d_color(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a.clamp(0.0, 1.0) }
}

fn d2d_rect(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

fn deflate(r: RectF, d: f32) -> RectF {
    RectF { left: r.left + d, top: r.top + d, right: r.right - d, bottom: r.bottom - d }
}

fn snap_rect(r: RectF) -> RectF {
    RectF { left: r.left.round(), top: r.top.round(), right: r.right.round(), bottom: r.bottom.round() }
}

fn intersect(a: RectF, b: RectF) -> Option<RectF> {
    let r = RectF {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    };
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

/// Thin wrapper bundling the target, the brush and the per-frame scale.
struct Pen<'a> {
    rt: &'a ID2D1RenderTarget,
    brush: &'a ID2D1SolidColorBrush,
}

impl Pen<'_> {
    fn set(&self, c: Rgba) {
        unsafe { self.brush.SetColor(&d2d_color(c)) };
    }

    fn fill(&self, r: RectF, radius: f32, c: Rgba) {
        if c.a <= 0.0 || r.width() <= 0.0 || r.height() <= 0.0 {
            return;
        }
        self.set(c);
        unsafe {
            if radius > 0.25 {
                let rr = D2D1_ROUNDED_RECT { rect: d2d_rect(r), radiusX: radius, radiusY: radius };
                self.rt.FillRoundedRectangle(&rr, self.brush);
            } else {
                self.rt.FillRectangle(&d2d_rect(r), self.brush);
            }
        }
    }

    /// Stroke of `width` px drawn entirely inside `r` (r has whole-pixel edges).
    fn border(&self, r: RectF, radius: f32, width: f32, c: Rgba) {
        if c.a <= 0.0 || r.width() <= width || r.height() <= width {
            return;
        }
        self.set(c);
        let inset = deflate(r, width * 0.5);
        unsafe {
            if radius > 0.25 {
                let rr = D2D1_ROUNDED_RECT {
                    rect: d2d_rect(inset),
                    radiusX: (radius - width * 0.5).max(0.0),
                    radiusY: (radius - width * 0.5).max(0.0),
                };
                self.rt.DrawRoundedRectangle(&rr, self.brush, width, None);
            } else {
                self.rt.DrawRectangle(&d2d_rect(inset), self.brush, width, None);
            }
        }
    }
}

struct Frame<'a> {
    pen: Pen<'a>,
    rt: &'a ID2D1RenderTarget,
    snapshot: &'a Snapshot,
    layout: &'a Layout,
    view: &'a ViewState,
    theme: &'a Theme,
    cfg: &'a LayoutConfig,
    text: &'a TextContext<'a>,
    scale: f32,
}

#[allow(clippy::too_many_arguments)]
fn draw_frame(
    rt: &ID2D1RenderTarget,
    res: &mut TargetResources,
    text: &TextContext,
    snapshot: &Snapshot,
    layout: &Layout,
    view: &ViewState,
    theme: &Theme,
    cfg: &LayoutConfig,
) -> windows::core::Result<()> {
    let brush = res.brush.clone();
    let f = Frame {
        pen: Pen { rt, brush: &brush },
        rt,
        snapshot,
        layout,
        view,
        theme,
        cfg,
        text,
        scale: if layout.scale > 0.0 { layout.scale } else { 1.0 },
    };
    unsafe {
        rt.BeginDraw();
        rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
    }
    // Near-invisible full-surface fill so no pixel has alpha 0 (clicks must not fall through).
    f.pen.fill(
        RectF { left: 0.0, top: 0.0, right: layout.width.max(1) as f32, bottom: layout.height.max(1) as f32 },
        0.0,
        theme.background,
    );

    let hover_desktop = match f.view.hover {
        Some(Hit::Desktop(d)) | Some(Hit::Window { desktop: d, .. }) => Some(d),
        _ => None,
    };
    let drag = f.view.drag;

    for d in 0..layout.cells.len() {
        f.draw_cell(res, rt, d, hover_desktop == Some(d), drag);
    }
    if let Some(drag) = drag {
        f.draw_ghost(res, rt, &drag);
    }
    res.end_frame();
    unsafe { rt.EndDraw(None, None) }
}

impl Frame<'_> {
    fn draw_cell(
        &self,
        res: &mut TargetResources,
        rt: &ID2D1RenderTarget,
        d: usize,
        hovered: bool,
        drag: Option<DragVisual>,
    ) {
        let (s, th) = (self.scale, self.theme);
        let cell = self.layout.cells[d];
        let radius = (4.0 * s).round();
        let is_current = d == self.snapshot.current;

        // Background.
        self.pen.fill(cell, radius, th.cell_fill);
        if hovered {
            self.pen.fill(cell, radius, th.cell_hover_fill);
        }
        if is_current {
            self.pen.fill(cell, radius, th.current_fill);
        }

        let inner = deflate(cell, 1.0);

        // Desktop label, behind the windows.
        if self.cfg.show_names {
            self.draw_label(d, inner);
        }

        // Windows, bottom to top, clipped to the cell interior.
        unsafe { rt.PushAxisAlignedClip(&d2d_rect(inner), D2D1_ANTIALIAS_MODE_ALIASED) };
        let list: Vec<_> = self.snapshot.windows_on(d).collect();
        for w in list.iter().rev() {
            let r = self.window_rect(d, &w.rect);
            let Some(vis) = intersect(r, inner) else { continue };
            let dragged_here =
                matches!(drag, Some(dr) if dr.hwnd == w.hwnd && dr.source_desktop == d);
            let fade = if dragged_here { 0.35 } else { 1.0 };
            let fg = w.is_foreground && d == self.snapshot.current;
            let win_hover =
                matches!(self.view.hover, Some(Hit::Window { desktop, hwnd }) if desktop == d && hwnd == w.hwnd);
            let (fill, border) = if fg {
                (th.fg_window_fill, th.fg_window_border)
            } else if win_hover {
                (th.window_hover_fill, th.fg_window_border.faded(0.8))
            } else {
                (th.window_fill, th.window_border)
            };
            // Windows on the current desktop pick up a hint of the accent color.
            let fill = if is_current && !fg { fill.mix(th.accent.with_alpha(1.0), 0.14) } else { fill };
            let wr = (1.5 * s).min(r.width().min(r.height()) * 0.25).floor();
            self.pen.fill(r, wr, fill.faded(fade));
            self.pen.border(r, wr, 1.0, border.faded(fade));
            if self.cfg.show_icons
                && let Some(icon) = &w.icon {
                    self.draw_icon(res, rt, icon, vis, fade);
                }
        }
        unsafe { rt.PopAxisAlignedClip() };

        // Drop target highlight.
        if let Some(drag) = drag
            && drag.target_desktop == Some(d) && drag.source_desktop != d {
                self.pen.fill(cell, radius, th.drop_fill);
                self.pen.border(cell, radius, 2.0, th.drop_border);
                return;
            }

        // Cell border on top so window fills never overdraw it.
        if is_current {
            self.pen.border(cell, radius, 1.0, th.current_border);
        } else if hovered {
            self.pen.border(cell, radius, 1.0, th.cell_hover_border);
        } else {
            self.pen.border(cell, radius, 1.0, th.cell_border);
        }
    }

    /// Window outline in pixel-snapped cell coordinates (not clipped), at least 3x3.
    fn window_rect(&self, d: usize, rect: &crate::model::RectI) -> RectF {
        let mut r = snap_rect(self.layout.map_window(d, rect));
        if r.right - r.left < 3.0 {
            r.right = r.left + 3.0;
        }
        if r.bottom - r.top < 3.0 {
            r.bottom = r.top + 3.0;
        }
        r
    }

    fn draw_label(&self, d: usize, inner: RectF) {
        let Some(format) = self.text.format else { return };
        let info = &self.snapshot.desktops[d];
        let max_w = inner.width() - 6.0 * self.scale;
        let candidates = [info.display_name(), (info.index + 1).to_string()];
        for text in candidates {
            let wide: Vec<u16> = text.encode_utf16().collect();
            if wide.is_empty() {
                continue;
            }
            unsafe {
                let Ok(layout) = self.text.factory.CreateTextLayout(
                    &wide,
                    format,
                    inner.width().max(1.0),
                    inner.height().max(1.0),
                ) else {
                    continue;
                };
                let mut m = DWRITE_TEXT_METRICS::default();
                if layout.GetMetrics(&mut m).is_err() || m.width > max_w {
                    continue;
                }
                self.pen.set(self.theme.label);
                self.rt.DrawTextLayout(
                    D2D_POINT_2F { x: inner.left, y: inner.top },
                    &layout,
                    self.pen.brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            return;
        }
    }

    /// Icon size in px for an outline of `ow` x `oh` px, or `None` when too small.
    fn icon_size(&self, ow: f32, oh: f32) -> Option<u32> {
        let s = self.scale;
        let base = (cell_height(self.layout) * 0.42).round().clamp(8.0, 32.0 * s);
        let avail = ow.min(oh) - 4.0 * s;
        let size = base.min(avail).floor();
        (size >= 10.0).then_some(size as u32)
    }

    /// Draw `icon` centered in `area` (whole-pixel position, no scaling at draw time).
    fn draw_icon(
        &self,
        res: &mut TargetResources,
        rt: &ID2D1RenderTarget,
        icon: &IconImage,
        area: RectF,
        opacity: f32,
    ) {
        let Some(size) = self.icon_size(area.width(), area.height()) else { return };
        let Some((bmp, dw, dh)) = res.icon_bitmap(rt, icon, size) else { return };
        let left = (area.left + (area.width() - dw as f32) * 0.5).round();
        let top = (area.top + (area.height() - dh as f32) * 0.5).round();
        let dest = D2D_RECT_F { left, top, right: left + dw as f32, bottom: top + dh as f32 };
        unsafe {
            rt.DrawBitmap(
                &bmp,
                Some(&dest),
                opacity,
                D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                None,
            );
        }
    }

    fn draw_ghost(&self, res: &mut TargetResources, rt: &ID2D1RenderTarget, drag: &DragVisual) {
        let Some(w) = self.snapshot.windows.iter().find(|w| w.hwnd == drag.hwnd) else { return };
        if drag.source_desktop >= self.layout.cells.len() {
            return;
        }
        let src = self.window_rect(drag.source_desktop, &w.rect);
        let (ow, oh) = (src.width(), src.height());
        let left = (drag.cursor.0 - drag.grab_offset.0).round();
        let top = (drag.cursor.1 - drag.grab_offset.1).round();
        let r = RectF { left, top, right: left + ow, bottom: top + oh };
        let wr = (1.5 * self.scale).min(ow.min(oh) * 0.25).floor();
        self.pen.fill(r, wr, self.theme.ghost_fill);
        self.pen.border(r, wr, 1.0, self.theme.ghost_border);
        if self.cfg.show_icons
            && let Some(icon) = &w.icon {
                self.draw_icon(res, rt, icon, r, 1.0);
            }
    }
}

// ---------------------------------------------------------------------------------------
// Icon resampling
// ---------------------------------------------------------------------------------------

/// Per-destination-pixel list of (first source index, weights) for a 1-D triangle filter
/// whose support widens when downscaling (so it averages instead of aliasing).
fn filter_weights(src: u32, dst: u32) -> Vec<(usize, Vec<f32>)> {
    let ratio = src as f32 / dst as f32;
    let support = ratio.max(1.0);
    (0..dst)
        .map(|i| {
            let center = (i as f32 + 0.5) * ratio - 0.5;
            let lo = ((center - support).ceil().max(0.0)) as usize;
            let hi = ((center + support).floor().min(src as f32 - 1.0)) as usize;
            let mut ws: Vec<f32> = (lo..=hi.max(lo))
                .map(|j| (1.0 - (j as f32 - center).abs() / support).max(0.0))
                .collect();
            let sum: f32 = ws.iter().sum();
            if sum <= 0.0 {
                ws = vec![1.0];
                return (center.round().clamp(0.0, src as f32 - 1.0) as usize, ws);
            }
            ws.iter_mut().for_each(|w| *w /= sum);
            (lo, ws)
        })
        .collect()
}

/// Resample premultiplied BGRA (separable triangle filter, premultiplied space).
fn resample_premul(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    if sw == dw && sh == dh {
        return src[..(sw * sh * 4) as usize].to_vec();
    }
    let xw = filter_weights(sw, dw);
    let yw = filter_weights(sh, dh);
    // Horizontal pass: sh rows x dw columns, f32 x 4.
    let mut tmp = vec![0.0f32; (sh * dw * 4) as usize];
    for y in 0..sh as usize {
        for (x, (lo, ws)) in xw.iter().enumerate() {
            let mut acc = [0.0f32; 4];
            for (k, w) in ws.iter().enumerate() {
                let i = (y * sw as usize + lo + k) * 4;
                for c in 0..4 {
                    acc[c] += src[i + c] as f32 * w;
                }
            }
            let o = (y * dw as usize + x) * 4;
            tmp[o..o + 4].copy_from_slice(&acc);
        }
    }
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for (y, (lo, ws)) in yw.iter().enumerate() {
        for x in 0..dw as usize {
            let mut acc = [0.0f32; 4];
            for (k, w) in ws.iter().enumerate() {
                let i = ((lo + k) * dw as usize + x) * 4;
                for c in 0..4 {
                    acc[c] += tmp[i + c] * w;
                }
            }
            let o = (y * dw as usize + x) * 4;
            let a = acc[3].round().clamp(0.0, 255.0);
            for c in 0..3 {
                // Keep the premultiplied invariant color <= alpha.
                out[o + c] = acc[c].round().clamp(0.0, a) as u8;
            }
            out[o + 3] = a as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_keeps_flat_color() {
        let src: Vec<u8> = (0..32 * 32).flat_map(|_| [100u8, 50, 25, 200]).collect();
        let out = resample_premul(&src, 32, 32, 16, 16);
        assert_eq!(out.len(), 16 * 16 * 4);
        for px in out.chunks(4) {
            assert_eq!(px, [100, 50, 25, 200]);
        }
        let up = resample_premul(&src, 32, 32, 40, 40);
        assert!(up.chunks(4).all(|p| p == [100, 50, 25, 200]));
    }

    #[test]
    fn resample_odd_sizes_do_not_panic() {
        let src = vec![255u8; 48 * 48 * 4];
        for (dw, dh) in [(1, 1), (7, 13), (47, 47), (96, 3)] {
            let out = resample_premul(&src, 48, 48, dw, dh);
            assert_eq!(out.len(), (dw * dh * 4) as usize);
        }
    }
}
