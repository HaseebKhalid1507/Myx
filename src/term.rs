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

// ------------------------------------------------------------- quit signals

/// The signals that ask a terminal program to go: `SIGTERM` (`kill`), `SIGHUP`
/// (its terminal was closed) and `SIGINT` (`kill -INT` — Ctrl-C itself arrives
/// as a key in raw mode). Caught so myx can quit the way `q` does — state
/// saved, terminal restored, colour subscribers told — instead of dying mid-
/// frame and leaving the terminal in raw mode on the alternate screen.
pub struct QuitSignals {
    #[cfg(unix)]
    caught: Option<[tokio::signal::unix::Signal; 3]>,
}

impl QuitSignals {
    /// Start catching them. Must run inside the tokio runtime. If they can't
    /// be caught, [`Self::recv`] simply never resolves — the signals keep
    /// their default effect, as before.
    pub fn install() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let caught = (|| {
                Some([
                    signal(SignalKind::terminate()).ok()?,
                    signal(SignalKind::hangup()).ok()?,
                    signal(SignalKind::interrupt()).ok()?,
                ])
            })();
            Self { caught }
        }
        #[cfg(not(unix))]
        Self {}
    }

    /// Wait for one and say which.
    pub async fn recv(&mut self) -> &'static str {
        #[cfg(unix)]
        if let Some([term, hup, int]) = &mut self.caught {
            return tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = hup.recv() => "SIGHUP",
                _ = int.recv() => "SIGINT",
            };
        }
        std::future::pending().await
    }
}

#[cfg(all(test, unix))]
mod quit_signal_tests {
    use super::*;

    #[test]
    fn a_hangup_is_caught_and_named_instead_of_killing_the_process() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let mut signals = QuitSignals::install();
            // Uncaught, this would end the test binary.
            let pid = std::process::id().to_string();
            let status = std::process::Command::new("kill")
                .args(["-HUP", &pid])
                .status()
                .expect("kill");
            assert!(status.success());
            let got = tokio::time::timeout(std::time::Duration::from_secs(5), signals.recv())
                .await
                .expect("the signal arrived");
            assert_eq!(got, "SIGHUP");
        });
    }
}
