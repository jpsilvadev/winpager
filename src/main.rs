// OWNER: host agent.
#![windows_subsystem = "windows"]

use winpager::{config, host, log, wlog};

fn main() -> anyhow::Result<()> {
    // A second instance exits quietly, before it can truncate the running instance's log.
    let Some(_instance) = host::single_instance() else { return Ok(()) };
    log::init();
    std::panic::set_hook(Box::new(|info| {
        wlog!("panic: {info}");
    }));
    wlog!("winpager {} starting", env!("CARGO_PKG_VERSION"));
    let cfg = config::Config::load();
    wlog!("config: {cfg:?}");
    let r = host::run(cfg);
    wlog!("exiting: {:?}", r.as_ref().map(|_| ()).map_err(|e| format!("{e:#}")));
    r
}
