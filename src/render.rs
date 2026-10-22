//! OWNER: render agent. Draws the pager with Direct2D into a premultiplied 32-bit DIB
//! and pushes it to a WS_EX_LAYERED window with UpdateLayeredWindow.

use std::path::Path;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::layout::Layout;
use crate::model::{LayoutConfig, Snapshot, ViewState};
use crate::theme::Theme;

pub struct Renderer {}

impl Renderer {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {})
    }

    /// Render a frame of size `layout.width` x `layout.height` and apply it to the layered
    /// window `hwnd` (UpdateLayeredWindow with pptDst = None; the host positions the window).
    /// Every pixel must have alpha >= 1 so the whole pager receives mouse input.
    pub fn present(
        &mut self,
        hwnd: HWND,
        snapshot: &Snapshot,
        layout: &Layout,
        view: &ViewState,
        theme: &Theme,
        cfg: &LayoutConfig,
    ) -> anyhow::Result<()> {
        // STUB: translucent cells with a brighter current desktop, written as raw pixels.
        let _ = (view, theme, cfg);
        let (w, h) = (layout.width.max(1), layout.height.max(1));
        unsafe {
            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(screen_dc);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let px = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);
            px.fill(0x0100_0000);
            for (i, c) in layout.cells.iter().enumerate() {
                let color: u32 = if i == snapshot.current { 0xC060_90C0 } else { 0x6030_3030 };
                for y in c.top as i32..c.bottom as i32 {
                    for x in c.left as i32..c.right as i32 {
                        if x >= 0 && y >= 0 && x < w && y < h {
                            px[(y * w + x) as usize] = color;
                        }
                    }
                }
            }
            let old = SelectObject(mem_dc, bmp);
            let size = SIZE { cx: w, cy: h };
            let src = POINT::default();
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
                ..Default::default()
            };
            let r = UpdateLayeredWindow(
                hwnd,
                screen_dc,
                None,
                Some(&size),
                mem_dc,
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            SelectObject(mem_dc, old);
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            r?;
        }
        Ok(())
    }

    /// Same drawing as `present`, written to a PNG (for tests and visual review).
    pub fn render_to_png(
        &mut self,
        path: &Path,
        snapshot: &Snapshot,
        layout: &Layout,
        view: &ViewState,
        theme: &Theme,
        cfg: &LayoutConfig,
    ) -> anyhow::Result<()> {
        let _ = (path, snapshot, layout, view, theme, cfg);
        todo!()
    }
}
