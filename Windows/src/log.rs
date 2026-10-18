//! An append-only log in the data directory. It records events and errors,
//! never transcripts, tokens or audio.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

static PATH: OnceLock<Mutex<PathBuf>> = OnceLock::new();
const LIMIT: u64 = 1024 * 1024;

pub fn init(path: PathBuf) {
    let _ = PATH.set(Mutex::new(path));
}

pub fn write(message: std::fmt::Arguments) {
    let Some(path) = PATH.get() else { return };
    let Ok(path) = path.lock() else { return };
    if fs::metadata(&*path).is_ok_and(|metadata| metadata.len() > LIMIT) {
        let _ = fs::rename(&*path, path.with_extension("log.old"));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&*path) {
        let now = humantime::format_rfc3339_seconds(SystemTime::now());
        let _ = writeln!(file, "{now} {message}");
    }
}

macro_rules! log {
    ($($argument:tt)*) => { $crate::log::write(format_args!($($argument)*)) };
}
