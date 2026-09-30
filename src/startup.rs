//! Start-up diagnostics for the desktop app: logging (to a file on
//! Windows, where release builds have no console) and panics logged with
//! their backtrace before the app goes down.

use std::path::{Path, PathBuf};

/// Where the log goes on Windows: next to the app's other data.
pub(crate) fn log_path() -> PathBuf {
    crate::app::init::data_dir().join("rusty-painter.log")
}

/// A log over this size is moved aside (to `.old.log`) at start-up, so it
/// never grows without bound.
const MAX_LOG_BYTES: u64 = 4 << 20;

/// Start logging (`RUST_LOG` still picks the levels; otherwise the app's
/// own messages from `info` up and its libraries' warnings) and log panics.
pub(crate) fn init_logging() {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,rusty_painter=info"),
    );
    if cfg!(windows)
        && let Some(file) = open_log(&log_path())
    {
        builder.target(env_logger::Target::Pipe(Box::new(file)));
    }
    // A second call (tests) keeps the first logger.
    let _ = builder.try_init();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("{info}\n{}", std::backtrace::Backtrace::force_capture());
        previous(info);
    }));
}

/// The log file, opened for appending (the previous one moved aside when
/// too big); `None` if it can't be written.
fn open_log(path: &Path) -> Option<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("old.log"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_big_log_is_moved_aside_and_a_new_one_started() {
        let dir = std::env::temp_dir().join(format!("rp-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("app.log");
        {
            use std::io::Write;
            let mut file = open_log(&path).expect("writable");
            file.write_all(b"first\n").unwrap();
        }
        // Small: appended to.
        drop(open_log(&path));
        assert_eq!(std::fs::read(&path).unwrap(), b"first\n");
        // Too big: moved aside.
        std::fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize + 1]).unwrap();
        drop(open_log(&path));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert!(dir.join("app.old.log").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
