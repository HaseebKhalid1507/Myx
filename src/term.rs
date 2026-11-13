//! Terminal setup/teardown and the single-instance lock.

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

/// Hold an exclusive lock so only one myx runs at a time. Returns the lock file
/// (kept alive for the process lifetime; the OS releases it on exit, even a crash).
pub fn acquire_single_instance_lock() -> std::fs::File {
    use fs2::FileExt;
    let path = crate::home_dir()
        .map(|h| h.join(".cache/myx/lock"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/myx.lock"));
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .expect("open lock file");
    if file.try_lock_exclusive().is_err() {
        eprintln!("myx is already running (another instance holds the lock).");
        eprintln!(
            "Close it first, or remove {} if it's stale.",
            path.display()
        );
        std::process::exit(1);
    }
    file
}

pub fn init_terminal() -> Result<Term> {
    // Restore the terminal on panic so a crash doesn't strand the user in a
    // raw-mode / alt-screen shell (audit H6). Runs before the default hook (and
    // before the abort under panic=abort).
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = execute!(
            out,
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableFocusChange,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        let _ = disable_raw_mode();
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        // Notices a return to this tmux window, when art must be re-sent.
        crossterm::event::EnableFocusChange
    )?;
    // Media key support requires keyboard enhancement (Windows Terminal, kitty, etc.).
    // Silently skip on terminals that don't support it (legacy Windows console).
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

pub fn restore_terminal(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableFocusChange,
        LeaveAlternateScreen
    )?;
    let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    terminal.show_cursor()?;
    Ok(())
}

// ------------------------------------------------------------ cell size, live

/// Waits out a burst of resizes before the cell size is measured again. A font
/// zoom or a window drag is dozens of `Resize` events, and the cell only
/// matters once they stop — measuring on each would be a terminal round trip
/// per step.
#[derive(Debug, Default)]
pub struct CellWatch {
    since: Option<std::time::Instant>,
}

impl CellWatch {
    /// How long the terminal must stay quiet before measuring.
    pub const SETTLE: std::time::Duration = std::time::Duration::from_millis(150);

    /// A resize arrived; (re)start the wait.
    pub fn resized(&mut self, now: std::time::Instant) {
        self.since = Some(now);
    }

    /// Between a resize and the measurement that follows it.
    pub fn settling(&self) -> bool {
        self.since.is_some()
    }

    /// True exactly once per burst: when it has been quiet for [`Self::SETTLE`].
    pub fn due(&mut self, now: std::time::Instant) -> bool {
        match self.since {
            Some(t) if now.duration_since(t) >= Self::SETTLE => {
                self.since = None;
                true
            }
            _ => false,
        }
    }
}

/// The attached client's cell size in pixels, from tmux. Exact — tmux had it
/// from the outer terminal — and nothing is written to or read from the pane.
/// `None` outside tmux, or from a tmux too old to know.
pub fn tmux_cell_size() -> Option<ratatui_image::FontSize> {
    std::env::var_os("TMUX")?;
    let out = std::process::Command::new("tmux")
        .args([
            "display",
            "-p",
            "#{client_cell_width} #{client_cell_height}",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    parse_tmux_cell(&String::from_utf8_lossy(&out.stdout))
}

/// `"10 22\n"` → 10×22. Zero or junk is "doesn't know".
pub fn parse_tmux_cell(s: &str) -> Option<ratatui_image::FontSize> {
    let mut it = s.split_whitespace().map(str::parse::<u16>);
    let (w, h) = (it.next()?.ok()?, it.next()?.ok()?);
    (w > 0 && h > 0).then(|| ratatui_image::FontSize::new(w, h))
}

/// The reply to `CSI 16 t`, `CSI 6 ; height ; width t`, found in `bytes`
/// (other input may surround it). `None` until a whole reply has arrived.
pub fn parse_cell_report(bytes: &[u8]) -> Option<ratatui_image::FontSize> {
    let start = bytes.windows(4).position(|w| w == b"\x1b[6;")? + 4;
    let rest = &bytes[start..];
    let end = rest.iter().position(|&b| b == b't')?;
    let body = std::str::from_utf8(&rest[..end]).ok()?;
    let (h, w) = body.split_once(';')?;
    let (h, w) = (h.parse::<u16>().ok()?, w.parse::<u16>().ok()?);
    (w > 0 && h > 0).then(|| ratatui_image::FontSize::new(w, h))
}

/// Ask the terminal for its cell size (`CSI 16 t`) and wait up to `timeout`
/// for the answer — the query `ratatui-image` makes at startup, made again.
///
/// The caller must have stopped everything else reading stdin first, or the
/// reply is parsed as keystrokes. Unlike re-running the picker's query, this
/// leaves no reader behind when the terminal doesn't answer: it reads only
/// what `poll` says is there, until the deadline.
#[cfg(unix)]
pub fn query_cell_size(timeout: std::time::Duration) -> Option<ratatui_image::FontSize> {
    use std::io::Write;
    use std::os::fd::AsFd;

    let mut out = io::stdout();
    out.write_all(b"\x1b[16t").ok()?;
    out.flush().ok()?;

    let stdin = io::stdin();
    let fd = stdin.as_fd();
    let deadline = std::time::Instant::now() + timeout;
    let mut got = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        let wait = rustix::time::Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos() as _,
        };
        let mut fds = [rustix::event::PollFd::new(
            &fd,
            rustix::event::PollFlags::IN,
        )];
        match rustix::event::poll(&mut fds, Some(&wait)) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        let mut chunk = [0u8; 64];
        let n = rustix::io::read(fd, &mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        got.extend_from_slice(&chunk[..n]);
        if let Some(cell) = parse_cell_report(&got) {
            return Some(cell);
        }
    }
}

#[cfg(not(unix))]
pub fn query_cell_size(_timeout: std::time::Duration) -> Option<ratatui_image::FontSize> {
    None
}

#[cfg(test)]
mod cell_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wh(c: Option<ratatui_image::FontSize>) -> Option<(u16, u16)> {
        c.map(|c| (c.width, c.height))
    }

    #[test]
    fn a_burst_of_resizes_is_measured_once_after_it_settles() {
        let mut w = CellWatch::default();
        let t0 = Instant::now();
        assert!(!w.settling());
        w.resized(t0);
        w.resized(t0 + Duration::from_millis(40));
        w.resized(t0 + Duration::from_millis(90));
        assert!(w.settling());
        // 150 ms after the *last* one, not the first.
        assert!(!w.due(t0 + Duration::from_millis(200)));
        assert!(w.due(t0 + Duration::from_millis(240)));
        assert!(!w.settling());
        assert!(!w.due(t0 + Duration::from_millis(400)), "once per burst");
    }

    #[test]
    fn tmux_reports_the_cell_as_two_numbers() {
        assert_eq!(wh(parse_tmux_cell("10 22\n")), Some((10, 22)));
        assert_eq!(wh(parse_tmux_cell("0 0\n")), None, "tmux that doesn't know");
        assert_eq!(wh(parse_tmux_cell("")), None);
        assert_eq!(wh(parse_tmux_cell("x y")), None);
    }

    #[test]
    fn the_cell_report_is_height_then_width() {
        assert_eq!(wh(parse_cell_report(b"\x1b[6;22;10t")), Some((10, 22)));
        // Whatever arrived around it doesn't matter.
        assert_eq!(wh(parse_cell_report(b"j\x1b[6;29;13tk")), Some((13, 29)));
        assert_eq!(wh(parse_cell_report(b"\x1b[6;22;1")), None, "not finished");
        assert_eq!(
            wh(parse_cell_report(b"\x1b[4;800;600t")),
            None,
            "another report"
        );
        assert_eq!(wh(parse_cell_report(b"\x1b[6;0;10t")), None);
    }
}
