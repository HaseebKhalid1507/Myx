//! librespot's `log` bridge and the optional `MYX_LOG` debug file.

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
pub fn install_librespot_log() {
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
