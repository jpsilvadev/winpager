use embed_manifest::manifest::DpiAwareness;
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // Per-monitor v2 DPI awareness. The default supportedOS entries (Windows 7..10) are
        // required for child layered windows (WS_EX_LAYERED on a WS_CHILD).
        embed_manifest(new_manifest("WinPager").dpi_awareness(DpiAwareness::PerMonitorV2))
            .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
