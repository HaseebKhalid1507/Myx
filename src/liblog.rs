//! librespot's `log` bridge and the optional `MYX_LOG` debug file.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Temporary-but-useful diagnostics for startup/library failures. Kept out of
/// the TUI because alternate-screen rendering hides stderr.
/// Forwards librespot's own `log` output into `myx.log`. Its Connect
/// diagnostics are the only account of why the device went inactive, and
/// without a logger installed they go nowhere.
pub struct LibrespotLog;

impl log::Log for LibrespotLog {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        liblog(format!(
            "{} {}: {}",
            record.level(),
            record.target(),
            record.args()
        ));
    }
    fn flush(&self) {}
}

/// Any value of `MYX_LOG` turns logging on; the value only picks how loud
/// librespot is. `debug`/`trace` open it up, `warn` quiets it back down.
///
/// `main` calls this first, so it also starts the clock that [`Step`] lines
/// measure "since launch" from.
pub fn install_librespot_log() {
    since_launch();
    let Ok(level) = std::env::var("MYX_LOG") else {
        return;
    };
    let filter = match level.to_ascii_lowercase().as_str() {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "warn" => log::LevelFilter::Warn,
        // Connect names its own faults at info ("device became inactive");
        // warn shows only the fallout.
        _ => log::LevelFilter::Info,
    };
    if log::set_boxed_logger(Box::new(LibrespotLog)).is_ok() {
        log::set_max_level(filter);
    }
}

/// Optional debug log — silent unless `MYX_LOG` is set. Writes to
/// ~/.cache/myx/myx.log (user-owned dir 0700, file 0600) instead of a
/// world-writable fixed /tmp path (audit H5).
pub fn liblog(msg: impl AsRef<str>) {
    use std::io::Write;
    if std::env::var_os("MYX_LOG").is_none() {
        return;
    }
    let Some(home) = crate::home_dir() else {
        return;
    };
    let dir = home.join(".cache/myx");
    if std::fs::create_dir_all(&dir).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(dir.join("myx.log")) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let _ = writeln!(f, "{ts:.3} {}", msg.as_ref());
    }
}

/// Time since launch: since the first call, which [`install_librespot_log`]
/// makes the first thing `main` does.
pub fn since_launch() -> Duration {
    static LAUNCH: OnceLock<Instant> = OnceLock::new();
    LAUNCH.get_or_init(Instant::now).elapsed()
}

/// One step that can keep myx waiting, timed in `myx.log`.
///
/// A line when it starts, so a step that never finishes is the last thing in
/// the log, and one when it ends, with how long it took. The end line is
/// written on drop, so a step that fails out early with `?` still gets one —
/// which is why it says "took" and not "done".
///
/// A slow start (#72) is otherwise a gap in the log with nothing to say which
/// of login, the terminal query or the access point it was.
#[must_use = "the step ends when this is dropped"]
pub struct Step {
    label: String,
    started: Instant,
}

impl Step {
    pub fn start(label: impl Into<String>) -> Self {
        let label = label.into();
        liblog(format!(
            "{label}… (at +{:.3}s)",
            since_launch().as_secs_f64()
        ));
        Self {
            label,
            started: Instant::now(),
        }
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        liblog(step_took(
            &self.label,
            self.started.elapsed(),
            since_launch(),
        ));
    }
}

/// Run `f` as a [`Step`].
pub fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let _step = Step::start(label);
    f()
}

fn step_took(label: &str, took: Duration, at: Duration) -> String {
    format!(
        "{label} took {:.3}s (at +{:.3}s)",
        took.as_secs_f64(),
        at.as_secs_f64()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_line_says_how_long_and_when_to_the_millisecond() {
        assert_eq!(
            step_took(
                "startup: connect to Spotify",
                Duration::from_millis(254_123),
                Duration::from_millis(260_001),
            ),
            "startup: connect to Spotify took 254.123s (at +260.001s)"
        );
    }
}
