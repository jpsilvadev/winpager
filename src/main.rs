// OWNER: host agent.
#![windows_subsystem = "windows"]

mod config;
mod enumerate;
mod host;
mod icons;
mod layout;
mod model;
mod render;
mod taskbar;
mod theme;
mod vd;

fn main() -> anyhow::Result<()> {
    let cfg = config::Config::load();
    host::run(cfg)
}
