use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

const LOG_PATH: &str = "/tmp/amux.log";

pub fn init() {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_PATH)
        .ok();
    *LOG_FILE.lock().unwrap() = file;
}

pub fn log(msg: &str) {
    if let Ok(mut guard) = LOG_FILE.lock() {
        if let Some(ref mut file) = *guard {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(file, "[{}] {}", timestamp, msg);
            let _ = file.flush();
        }
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::log(&format!($($arg)*))
    };
}
