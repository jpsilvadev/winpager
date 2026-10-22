//! OWNER: host agent. Tiny file logger (the app is GUI-subsystem, so there is no console).
//! Log file: `%LOCALAPPDATA%\winpager\winpager.log`, truncated at startup.

use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

struct Sink {
    file: File,
    start: Instant,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Path of the log file, if `%LOCALAPPDATA%` is known.
pub fn path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(std::path::PathBuf::from(base).join("winpager").join("winpager.log"))
}

/// Create (truncate) the log file. Failure is silent: logging then becomes a no-op.
pub fn init() {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(file) = File::create(&p)
        && let Ok(mut g) = SINK.lock()
    {
        *g = Some(Sink { file, start: Instant::now() });
    }
}

#[doc(hidden)]
pub fn write(args: std::fmt::Arguments) {
    if let Ok(mut g) = SINK.lock()
        && let Some(s) = g.as_mut()
    {
        let t = s.start.elapsed().as_millis();
        let _ = writeln!(s.file, "[{:>5}.{:03}] {}", t / 1000, t % 1000, args);
        let _ = s.file.flush();
    }
}

/// `wlog!("something {}", x)` appends a timestamped line to the log file.
#[macro_export]
macro_rules! wlog {
    ($($arg:tt)*) => { $crate::log::write(format_args!($($arg)*)) };
}
