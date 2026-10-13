//! Diagnostics: a log file next to the settings, since a release build on
//! Windows has no console to write to.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{LevelFilter, Log, Metadata, Record};

const FILE_NAME: &str = "vivace.log";
const PREVIOUS_FILE_NAME: &str = "vivace.old.log";
// Past this size the log is set aside at startup, so it never grows unbounded.
const ROTATE_BYTES: u64 = 1024 * 1024;
// Selects how much is logged: error, warn (default), info, debug or trace.
const LEVEL_VARIABLE: &str = "VIVACE_LOG";

struct Logger {
    level: LevelFilter,
    file: Option<Mutex<File>>,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let line = format!(
            "{seconds} {:5} {} {}\n",
            record.level(),
            record.target(),
            record.args()
        );
        // There is nowhere left to report a failure to write a log line.
        _ = std::io::stderr().write_all(line.as_bytes());
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            _ = file.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {}
}

/// Starts logging to stderr and, when the data directory is usable, to a
/// file in it. Panics are logged too, so a crash leaves a trace.
pub fn init() {
    let level = std::env::var(LEVEL_VARIABLE)
        .ok()
        .and_then(|level| level.parse().ok())
        .unwrap_or(LevelFilter::Warn);
    let file = vivace_core::data_dir()
        .ok()
        .and_then(|directory| open(&directory));
    let logger = Logger {
        level,
        file: file.map(Mutex::new),
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        log::error!("{panic}");
        default_hook(panic);
    }));
}

fn open(directory: &Path) -> Option<File> {
    fs::create_dir_all(directory).ok()?;
    let path: PathBuf = directory.join(FILE_NAME);
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > ROTATE_BYTES) {
        // Losing the old log is acceptable if the rename fails.
        _ = fs::rename(&path, directory.join(PREVIOUS_FILE_NAME));
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}
