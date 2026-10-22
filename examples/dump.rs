//! Test harness for the data layer: prints the current snapshot and timings.
//!
//!   cargo run --example dump [-- --icons sheet.png]
//!
//! With `--icons` also writes a contact sheet of all window icons (mid-grey background).

use std::time::Instant;

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW, WM_APP,
};
use windows::core::w;
use winpager::enumerate::build_snapshot;
use winpager::icons::IconCache;
use winpager::model::Snapshot;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let icons_out = args.iter().position(|a| a == "--icons").and_then(|i| args.get(i + 1)).cloned();

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let mut icons = IconCache::new();
    let t = Instant::now();
    let snap = build_snapshot(&mut icons, &[])?;
    let cold = t.elapsed();

    let n = 20;
    let t = Instant::now();
    for _ in 0..n {
        build_snapshot(&mut icons, &[])?;
    }
    let warm = t.elapsed() / n;

    print_snapshot(&snap);
    println!("\nfirst (cold) snapshot: {cold:?}");
    println!("warm snapshot (avg of {n}, no listener => desktop list uncached): {warm:?}");

    // With a live listener the desktop list is cached.
    let listener = winpager::vd::listen(message_window(), WM_APP)?;
    build_snapshot(&mut icons, &[])?;
    let t = Instant::now();
    for _ in 0..n {
        build_snapshot(&mut icons, &[])?;
    }
    println!("warm snapshot (avg of {n}, with vd listener): {:?}", t.elapsed() / n);

    if args.iter().any(|a| a == "--test-create-remove") {
        let before = winpager::vd::desktops()?.0.len();
        winpager::vd::create()?;
        let mid = winpager::vd::desktops()?;
        println!("create: {before} -> {} desktops", mid.0.len());
        anyhow::ensure!(mid.0.len() == before + 1, "desktop was not created");
        winpager::vd::remove(mid.0.len() - 1)?;
        let after = winpager::vd::desktops()?;
        println!("remove: -> {} desktops, current={}", after.0.len(), after.1);
        anyhow::ensure!(after.0.len() == before, "desktop was not removed");
    }

    // Drain whatever events arrived (create/remove above), then check that drop doesn't hang.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut got = 0;
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, WM_APP, WM_APP, PM_REMOVE).as_bool() {
            got += 1;
        }
    }
    println!("listener notifications received: {got}");
    let t = Instant::now();
    drop(listener);
    println!("listener dropped in {:?}", t.elapsed());

    if let Some(path) = icons_out {
        write_sheet(&snap, &path)?;
        println!("wrote icon sheet to {path}");
    }
    Ok(())
}

fn print_snapshot(s: &Snapshot) {
    println!("screen: {:?}", s.screen);
    println!("desktops ({}):", s.desktops.len());
    for d in &s.desktops {
        println!(
            "  [{}] {:?}{}",
            d.index,
            d.display_name(),
            if d.index == s.current { "  <- current" } else { "" }
        );
    }
    println!("windows ({}), topmost first:", s.windows.len());
    for (i, w) in s.windows.iter().enumerate() {
        let desk = match w.desktop {
            None => "pinned".to_string(),
            Some(id) => s
                .desktops
                .iter()
                .find(|d| d.id == id)
                .map(|d| d.index.to_string())
                .unwrap_or_else(|| "?".into()),
        };
        let icon = match &w.icon {
            Some(i) => format!("{}x{}#{}", i.width, i.height, i.id),
            None => "none".into(),
        };
        println!(
            "  #{i:<2} {:#010x} desk={desk:<6} rect=({},{} {}x{}) fg={} icon={icon:<10} {:?}",
            w.hwnd,
            w.rect.left,
            w.rect.top,
            w.rect.width(),
            w.rect.height(),
            w.is_foreground as u8,
            w.title
        );
    }
}

/// Message-only window to receive listener notifications.
fn message_window() -> isize {
    unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("dump"),
            Default::default(),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            None,
            None,
        )
        .map(|h| h.0 as isize)
        .unwrap_or(0)
    }
}

// ---- contact sheet --------------------------------------------------------------------

fn write_sheet(s: &Snapshot, path: &str) -> anyhow::Result<()> {
    const CELL: usize = 144;
    const COLS: usize = 6;
    const SCALE: usize = 2;
    let count = s.windows.len().max(1);
    let rows = count.div_ceil(COLS);
    let (w, h) = (COLS * CELL, rows * CELL);
    let mut rgb = vec![128u8; w * h * 3]; // mid-grey
    for (i, win) in s.windows.iter().enumerate() {
        let Some(ic) = &win.icon else { continue };
        let (cx, cy) = ((i % COLS) * CELL, (i / COLS) * CELL);
        let (iw, ih) = (ic.width as usize, ic.height as usize);
        let (sw, sh) = (iw * SCALE, ih * SCALE);
        let (ox, oy) = (cx + CELL.saturating_sub(sw) / 2, cy + CELL.saturating_sub(sh) / 2);
        for y in 0..sh.min(CELL) {
            for x in 0..sw.min(CELL) {
                let p = &ic.bgra_premul[((y / SCALE) * iw + x / SCALE) * 4..][..4];
                let a = p[3] as u32;
                let o = ((oy + y) * w + ox + x) * 3;
                // premultiplied "over": out = src + dst * (1 - a)
                for (c, src) in [p[2], p[1], p[0]].into_iter().enumerate() {
                    rgb[o + c] = (src as u32 + rgb[o + c] as u32 * (255 - a) / 255).min(255) as u8;
                }
            }
        }
    }
    std::fs::write(path, png_rgb(w as u32, h as u32, &rgb))?;
    Ok(())
}

/// Minimal PNG writer: RGB8, stored (uncompressed) deflate blocks.
fn png_rgb(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w as usize * 3 + 1) * h as usize);
    for row in rgb.chunks(w as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(chunks.peek().is_none() as u8);
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}
