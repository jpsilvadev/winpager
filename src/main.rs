// OWNER: host agent.
#![windows_subsystem = "windows"]

use winpager::{config, host};

fn main() -> anyhow::Result<()> {
    let cfg = config::Config::load();
    host::run(cfg)
}
