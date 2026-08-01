//! `myx theme` — the command-line face of MXC.
//!
//! MXC is only useful if reading it is trivial from a shell. A status bar, a
//! prompt, or a `sway` reload hook should not have to link Rust, learn the
//! wire format, or hand-roll NDJSON framing. So the player ships its own
//! subscriber:
//!
//! ```text
//! eval "$(myx theme get)"          # MYX_PRIMARY, MYX_ON_ACCENT, …
//! myx theme watch --format css     # stream :root {} blocks
//! myx theme watch --exec 'my-bar-reload'
//! ```
//!
//! ## Why this lives in the library, not in `main.rs`
//!
//! `main.rs` is the player: it authorizes with Spotify, starts librespot, and
//! takes over the terminal. None of that may happen for `myx theme` — the
//! whole point is a fast, scriptable read of a socket. `main.rs` therefore
//! only *dispatches* here (before any engine or auth work) and exits with the
//! code [`run`] returns. Keeping the implementation next to
//! [`subscribe`](crate::mxc::subscribe) also means the protocol and its
//! reference consumer cannot drift apart.
//!
//! ## The two pieces of CLI hygiene that actually matter
//!
//! - **`SIGPIPE` is reset to its default.** Rust sets `SIGPIPE` to `SIG_IGN`
//!   for every binary, and [`publish`](crate::mxc::publish) deliberately keeps
//!   it that way — a subscriber pressing Ctrl-C must never kill the music
//!   player. A CLI wants the exact opposite: `myx theme watch | head -1` should
//!   die quietly on the closed pipe like `cat` does, not print an I/O error.
//!   [`run`] flips the disposition back for this process only, which is safe
//!   precisely because the CLI path never binds a publisher.
//! - **stdout is flushed after every message.** stdout is block-buffered when
//!   it is a pipe, and palettes arrive minutes apart, so an unflushed
//!   `myx theme watch | while read …` looks like a hang for the length of a
//!   whole album.
//!
//! ## Formats
//!
//! [`Format::Sh`] is the complete view: all 16 palette tokens, the four
//! contrast values, and the envelope metadata a consumer needs to decide
//! whether it cares (`MYX_ORIGIN_KIND`) and how to animate (`MYX_FADE_MS`).
//! [`Format::Css`] and [`Format::Hex`] emit the 20 *colors* only — a `:root`
//! block has no sensible place for `fade_ms`, and a two-column table is worth
//! more to `cut`/`awk` when every row is a color. Anything that needs the full
//! envelope uses `sh` or [`Format::Json`].

use std::io::{self, Write};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::mxc::subscribe::{self, Subscriber};
use crate::mxc::wire::{Message, OriginKind, ThemeEvent};

/// Exit code for a usage error (bad flag, missing value, unknown subcommand).
/// Distinct from [`EXIT_RUNTIME`] so a script can tell "I typed it wrong" from
/// "Myx is not running".
const EXIT_USAGE: i32 = 2;

/// Exit code for a runtime failure — most often "no publisher is listening".
const EXIT_RUNTIME: i32 = 1;

/// How many `--exec` children may be running at once before updates start
/// being skipped.
///
/// See [`ExecRunner`] for why skipping is the right failure mode.
const MAX_INFLIGHT: usize = 4;

/// What to print for each message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `MYX_PRIMARY='#64e0d0'` — safe to `eval`.
    Sh,
    /// `--myx-primary: #64e0d0;` inside a `:root { }` block.
    Css,
    /// `<token>\t<hex>`, one per line.
    Hex,
    /// The message as NDJSON.
    Json,
}

/// Which subcommand was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    /// Print the snapshot (the first message on the connection) and exit.
    Get,
    /// Stream every update, reconnecting forever.
    Watch,
}

/// A fully parsed `myx theme …` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub cmd: Cmd,
    pub format: Format,
    /// `None` means [`crate::mxc::socket_path`]. Resolved late so the default
    /// is not baked into a parsed value that tests want to compare.
    pub socket: Option<PathBuf>,
    pub exec: Option<String>,
}

/// The parse outcome. `--help` is a successful *non-run*, not an error, so it
/// gets its own variant rather than being smuggled through `Err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Run(Args),
    Help,
}

/// The `--help` / usage text. Also printed to stderr on a usage error.
pub const USAGE: &str = "\
myx theme <get|watch> [options]

  get      print the current palette (the snapshot) and exit
  watch    stream every palette update until killed

Options:
  --format <sh|css|hex|json>   output format (default: sh)
  --socket <path>              theme socket (default: $XDG_RUNTIME_DIR/myx/theme.sock)
  --exec <cmd>                 run `sh -c <cmd>` per update, MYX_* exported
  -h, --help                   show this help
";

/// Parse the arguments *after* the `theme` subcommand word.
///
/// Pure and total: no I/O, no environment, no defaults resolved. That is what
/// makes it unit-testable, and the reason [`Args::socket`] stays an `Option`.
///
/// Both `--flag value` and `--flag=value` are accepted because both appear in
/// the wild, and rejecting one would be a papercut with no upside.
pub fn parse_args(argv: &[String]) -> Result<Parsed, String> {
    let mut cmd: Option<Cmd> = None;
    let mut format = Format::Sh;
    let mut socket: Option<PathBuf> = None;
    let mut exec: Option<String> = None;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        // Split `--flag=value` once, up front, so each flag arm below only has
        // to deal with "is the value inline or in the next argv slot".
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg, None),
        };

        /// Take the value for `flag`, from `--flag=value` or the next slot.
        macro_rules! value {
            () => {
                match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        argv.get(i)
                            .cloned()
                            .ok_or_else(|| format!("{flag} needs a value"))?
                    }
                }
            };
        }

        match flag {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--format" => {
                let v = value!();
                format = match v.as_str() {
                    "sh" => Format::Sh,
                    "css" => Format::Css,
                    "hex" => Format::Hex,
                    "json" => Format::Json,
                    other => {
                        return Err(format!(
                            "unknown format {other:?} (expected sh, css, hex or json)"
                        ))
                    }
                };
            }
            "--socket" => socket = Some(PathBuf::from(value!())),
            "--exec" => exec = Some(value!()),
            "get" if cmd.is_none() => cmd = Some(Cmd::Get),
            "watch" if cmd.is_none() => cmd = Some(Cmd::Watch),
            other if other.starts_with('-') => return Err(format!("unknown option {other:?}")),
            other => return Err(format!("unexpected argument {other:?}")),
        }
        i += 1;
    }

    match cmd {
        Some(cmd) => Ok(Parsed::Run(Args {
            cmd,
            format,
            socket,
            exec,
        })),
        None => Err("expected a subcommand: get or watch".to_string()),
    }
}

/// The serialized name of an origin kind, matching the wire encoding.
///
/// Written as an exhaustive match rather than a serde round-trip so that
/// adding a variant to [`OriginKind`] is a compile error here instead of a
/// silent `unknown` in every shell script on the machine.
fn origin_kind_str(kind: OriginKind) -> &'static str {
    match kind {
        OriginKind::AlbumArt => "album_art",
        OriginKind::Builtin => "builtin",
        OriginKind::Fallback => "fallback",
    }
}

/// The 16 palette tokens followed by the four contrast tokens, in a fixed
/// order shared by every format.
///
/// One list, so `sh`, `css` and `hex` can never disagree about which tokens
/// exist or what they are called.
fn color_tokens(ev: &ThemeEvent) -> [(&'static str, String); 20] {
    let c = &ev.colors;
    let k = &ev.contrast;
    [
        ("primary", c.primary.to_string()),
        ("secondary", c.secondary.to_string()),
        ("accent", c.accent.to_string()),
        ("error", c.error.to_string()),
        ("warning", c.warning.to_string()),
        ("success", c.success.to_string()),
        ("info", c.info.to_string()),
        ("text", c.text.to_string()),
        ("text_muted", c.text_muted.to_string()),
        ("background", c.background.to_string()),
        ("background_panel", c.background_panel.to_string()),
        ("background_element", c.background_element.to_string()),
        ("border", c.border.to_string()),
        ("border_active", c.border_active.to_string()),
        ("border_subtle", c.border_subtle.to_string()),
        ("border_dimmest", c.border_dimmest.to_string()),
        ("on_primary", k.on_primary.to_string()),
        ("on_secondary", k.on_secondary.to_string()),
        ("on_accent", k.on_accent.to_string()),
        ("on_background", k.on_background.to_string()),
    ]
}

/// Every `MYX_*` name/value pair for one palette: the 20 colors plus the
/// envelope metadata.
///
/// This is the single definition of the `sh` surface — [`format_sh`] renders
/// it and `--exec` exports it, so a script reading `$MYX_FADE_MS` sees exactly
/// what `eval "$(myx theme get)"` would have set.
///
/// `is_dark` is rendered as `1`/`0` rather than `true`/`false` so the natural
/// shell test — `[ "$MYX_IS_DARK" = 1 ]` — is also the correct one.
fn env_pairs(ev: &ThemeEvent) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = color_tokens(ev)
        .into_iter()
        .map(|(k, v)| (format!("MYX_{}", k.to_uppercase()), v))
        .collect();
    out.push((
        "MYX_IS_DARK".to_string(),
        if ev.is_dark { "1" } else { "0" }.to_string(),
    ));
    out.push((
        "MYX_ORIGIN_KIND".to_string(),
        origin_kind_str(ev.origin.kind).to_string(),
    ));
    out.push(("MYX_ORIGIN_NAME".to_string(), ev.origin.name.clone()));
    out.push(("MYX_FADE_MS".to_string(), ev.fade_ms.to_string()));
    out
}

/// Wrap `s` in single quotes, POSIX-safely.
///
/// **This is a security boundary, not a formatting nicety.** `MYX_ORIGIN_NAME`
/// is a track title straight out of Spotify metadata — attacker-influenced
/// text that a user is about to `eval`. Inside single quotes every byte is
/// literal except `'` itself, so the entire escape is: close the quote, emit a
/// backslash-escaped quote, reopen. `Don't Stop` becomes `'Don'\''t Stop'`.
fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// `MYX_PRIMARY='#64e0d0'` lines, `eval`-safe. No trailing newline.
pub fn format_sh(ev: &ThemeEvent) -> String {
    env_pairs(ev)
        .into_iter()
        .map(|(k, v)| format!("{k}={}", sh_quote(&v)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `:root { }` block of `--myx-*` custom properties. No trailing newline.
pub fn format_css(ev: &ThemeEvent) -> String {
    let mut out = String::from(":root {\n");
    for (k, v) in color_tokens(ev) {
        out.push_str(&format!("  --myx-{}: {v};\n", k.replace('_', "-")));
    }
    out.push('}');
    out
}

/// `<token>\t<hex>` lines. No trailing newline.
pub fn format_hex(ev: &ThemeEvent) -> String {
    color_tokens(ev)
        .into_iter()
        .map(|(k, v)| format!("{k}\t{v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render one message in `format`, without a trailing newline.
///
/// A `bye` has no palette, so in the three text formats it becomes a comment
/// in that format's own comment syntax: a consumer piping into `eval`, a CSS
/// file, or `awk` keeps working, and a human tailing the stream still sees
/// that the publisher went away.
///
/// `json` is the message re-encoded with [`Message::to_ndjson`] — the same
/// encoder the publisher uses, so for any message this build understands the
/// line is byte-identical to what came off the socket. (Fields invented by a
/// future Myx are dropped, because the subscriber parsed them away long before
/// this function ran; use a newer client if you need them.)
pub fn format_message(msg: &Message, format: Format) -> String {
    match (msg, format) {
        (_, Format::Json) => msg
            .to_ndjson()
            .unwrap_or_default()
            .trim_end_matches('\n')
            .to_string(),
        (Message::Theme(ev), Format::Sh) => format_sh(ev),
        (Message::Theme(ev), Format::Css) => format_css(ev),
        (Message::Theme(ev), Format::Hex) => format_hex(ev),
        (Message::Bye(b), Format::Css) => {
            format!("/* myx: publisher going away ({:?}) */", b.reason)
        }
        (Message::Bye(b), Format::Sh | Format::Hex) => {
            format!("# myx: publisher going away ({:?})", b.reason)
        }
    }
}

/// Fire-and-forget runner for `--exec`.
///
/// **Chosen behavior: never wait during `watch`.** The command is spawned with
/// the `sh`-format values in its environment and is not awaited, because
/// blocking the reader would let a slow hook stall the stream and eventually
/// get this subscriber evicted by the publisher for lagging (see
/// `publish::PEER_QUEUE`).
///
/// Not waiting at all would leak zombies, so finished children are reaped with
/// a non-blocking `try_wait` on each update. If [`MAX_INFLIGHT`] children are
/// still running the update is *skipped* with a warning rather than piling on:
/// a hook that cannot keep up with track changes is misconfigured, and
/// unbounded fan-out would turn that into a fork bomb.
///
/// `get` is the exception — it waits, since the process is about to exit and
/// an orphaned child would be killed mid-run.
struct ExecRunner {
    cmd: String,
    inflight: Vec<Child>,
}

impl ExecRunner {
    fn new(cmd: String) -> Self {
        Self {
            cmd,
            inflight: Vec::new(),
        }
    }

    fn run(&mut self, pairs: &[(String, String)], wait: bool) {
        // Reap first: this is what keeps `inflight` an accurate count rather
        // than a list of zombies.
        self.inflight
            .retain_mut(|c| !matches!(c.try_wait(), Ok(Some(_)) | Err(_)));
        if !wait && self.inflight.len() >= MAX_INFLIGHT {
            eprintln!("myx theme: --exec still busy ({MAX_INFLIGHT} running); skipping update");
            return;
        }

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&self.cmd)
            // The child inherits our stdout on purpose: `--exec 'echo $MYX_PRIMARY'`
            // should print where the user is looking. stdin is closed so a hook
            // that reads by accident fails fast instead of stealing the terminal.
            .stdin(Stdio::null());
        for (k, v) in pairs {
            command.env(k, v);
        }

        match command.spawn() {
            Ok(mut child) => {
                if wait {
                    let _ = child.wait();
                } else {
                    self.inflight.push(child);
                }
            }
            Err(e) => eprintln!("myx theme: --exec failed to start: {e}"),
        }
    }
}

/// Print one message and, if configured, run the `--exec` hook.
///
/// Returns the write result so the caller can stop on a closed pipe. The
/// explicit flush is the difference between a working `| while read` and an
/// apparent hang — see the module docs.
fn emit(
    msg: &Message,
    format: Format,
    exec: Option<&mut ExecRunner>,
    wait_for_exec: bool,
) -> io::Result<()> {
    let out = io::stdout();
    let mut lock = out.lock();
    writeln!(lock, "{}", format_message(msg, format))?;
    lock.flush()?;

    if let (Some(runner), Message::Theme(ev)) = (exec, msg) {
        runner.run(&env_pairs(ev), wait_for_exec);
    }
    Ok(())
}

/// Run `myx theme …` and return the process exit code.
///
/// `argv` is everything after the `theme` word. Never panics and never returns
/// to the player: `main.rs` exits with this code.
pub fn run(argv: &[String]) -> i32 {
    let args = match parse_args(argv) {
        Ok(Parsed::Run(a)) => a,
        Ok(Parsed::Help) => {
            print!("{USAGE}");
            return 0;
        }
        Err(e) => {
            eprintln!("myx theme: {e}\n\n{USAGE}");
            return EXIT_USAGE;
        }
    };

    // Undo the runtime's SIG_IGN so `| head -1` is a clean death, not an error.
    restore_default_sigpipe();

    let path = args.socket.clone().unwrap_or_else(crate::mxc::socket_path);
    let mut exec = args.exec.clone().map(ExecRunner::new);

    match args.cmd {
        Cmd::Get => {
            let mut sub = match Subscriber::connect(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "myx theme: no publisher at {}: {e}\n\
                         (is myx running? MYX_NO_COLOR_SOCKET disables publishing)",
                        path.display()
                    );
                    return EXIT_RUNTIME;
                }
            };
            match sub.next_message() {
                // `get` waits for the hook: the process is about to exit, and
                // an orphan would be killed mid-run.
                Ok(Some(msg)) => match emit(&msg, args.format, exec.as_mut(), true) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("myx theme: {e}");
                        EXIT_RUNTIME
                    }
                },
                Ok(None) => {
                    eprintln!("myx theme: publisher closed without sending a palette");
                    EXIT_RUNTIME
                }
                Err(e) => {
                    eprintln!("myx theme: {e}");
                    EXIT_RUNTIME
                }
            }
        }
        Cmd::Watch => {
            // `watch` never gives up: reconnecting across a myx restart is the
            // entire contract. The only way out is a write failure (the pipe
            // closed) or a signal.
            let _ = subscribe::watch(&path, |msg| {
                match emit(&msg, args.format, exec.as_mut(), false) {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(_) => ControlFlow::Break(()),
                }
            });
            0
        }
    }
}

/// Put `SIGPIPE` back to `SIG_DFL` for this process.
///
/// The inverse of `publish::ignore_sigpipe`, and safe only because the CLI
/// path never binds a publisher: here, a downstream `head` closing the pipe
/// *should* end us, exactly as it ends `cat`.
fn restore_default_sigpipe() {
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    // SAFETY: `signal` touches no memory we own and cannot fail in a way that
    // matters here; the previous disposition is intentionally discarded.
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gradient::Rgb;
    use crate::mxc::contrast::Contrast;
    use crate::mxc::wire::{ByeEvent, ByeReason, Colors, Hex, Origin, ThemeEvent};
    use crate::mxc::PROTOCOL_VERSION;

    fn sample_colors() -> Colors {
        Colors {
            primary: Hex(Rgb::new(0x64, 0xe0, 0xd0)),
            secondary: Hex(Rgb::new(0x4a, 0x9f, 0xd8)),
            accent: Hex(Rgb::new(0xf4, 0xaa, 0x48)),
            error: Hex(Rgb::new(0xe0, 0x55, 0x61)),
            warning: Hex(Rgb::new(0xd9, 0xa4, 0x41)),
            success: Hex(Rgb::new(0x61, 0xc7, 0x66)),
            info: Hex(Rgb::new(0x64, 0xe0, 0xd0)),
            text: Hex(Rgb::new(0xd8, 0xef, 0xff)),
            text_muted: Hex(Rgb::new(0x7a, 0x90, 0xa4)),
            background: Hex(Rgb::new(0x08, 0x10, 0x18)),
            background_panel: Hex(Rgb::new(0x10, 0x1d, 0x2a)),
            background_element: Hex(Rgb::new(0x18, 0x29, 0x3a)),
            border: Hex(Rgb::new(0x22, 0x37, 0x4a)),
            border_active: Hex(Rgb::new(0x42, 0xd9, 0xd0)),
            border_subtle: Hex(Rgb::new(0x18, 0x28, 0x38)),
            border_dimmest: Hex(Rgb::new(0x10, 0x1c, 0x28)),
        }
    }

    /// The reference event every formatter test asserts against. `name` is
    /// deliberately hostile: a real track title with an apostrophe.
    fn event(name: &str) -> ThemeEvent {
        ThemeEvent {
            v: PROTOCOL_VERSION,
            seq: 0,
            ts: 1_785_616_484_123,
            origin: Origin {
                kind: OriginKind::AlbumArt,
                name: name.to_string(),
                track: Some(name.to_string()),
                artist: Some("New Order".into()),
                album: None,
                track_id: Some("spotify:track:0S8".into()),
            },
            fade_ms: 1500,
            is_dark: true,
            colors: sample_colors(),
            contrast: Contrast::compute(&sample_colors()),
        }
    }

    #[test]
    fn sh_format_is_exact() {
        assert_eq!(
            format_sh(&event("Blue Monday")),
            "\
MYX_PRIMARY='#64e0d0'
MYX_SECONDARY='#4a9fd8'
MYX_ACCENT='#f4aa48'
MYX_ERROR='#e05561'
MYX_WARNING='#d9a441'
MYX_SUCCESS='#61c766'
MYX_INFO='#64e0d0'
MYX_TEXT='#d8efff'
MYX_TEXT_MUTED='#7a90a4'
MYX_BACKGROUND='#081018'
MYX_BACKGROUND_PANEL='#101d2a'
MYX_BACKGROUND_ELEMENT='#18293a'
MYX_BORDER='#22374a'
MYX_BORDER_ACTIVE='#42d9d0'
MYX_BORDER_SUBTLE='#182838'
MYX_BORDER_DIMMEST='#101c28'
MYX_ON_PRIMARY='#0b0b0b'
MYX_ON_SECONDARY='#0b0b0b'
MYX_ON_ACCENT='#0b0b0b'
MYX_ON_BACKGROUND='#d8efff'
MYX_IS_DARK='1'
MYX_ORIGIN_KIND='album_art'
MYX_ORIGIN_NAME='Blue Monday'
MYX_FADE_MS='1500'"
        );
    }

    /// The one that matters: origin names are untrusted metadata about to be
    /// `eval`'d. A naive `'{}'` would let `'; rm -rf ~; '` out of the quotes.
    #[test]
    fn sh_format_escapes_single_quotes_in_origin_name() {
        let out = format_sh(&event("Don't Stop '; rm -rf ~; '"));
        let line = out
            .lines()
            .find(|l| l.starts_with("MYX_ORIGIN_NAME="))
            .expect("origin name line");
        assert_eq!(
            line,
            r#"MYX_ORIGIN_NAME='Don'\''t Stop '\''; rm -rf ~; '\'''"#
        );
        // And prove it: `eval` the emitted line exactly the way a user does
        // (`eval "$(myx theme get)"` — one unsplit argument) and check the
        // variable comes back byte-for-byte, with nothing executed.
        if let Ok(out) = Command::new("sh")
            .arg("-c")
            .arg(r#"eval "$1"; printf %s "$MYX_ORIGIN_NAME""#)
            .arg("sh") // $0
            .arg(line) // $1
            .output()
        {
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "Don't Stop '; rm -rf ~; '"
            );
        }
    }

    #[test]
    fn sh_quote_round_trips_through_a_real_shell_when_available() {
        // Belt and braces: ask `sh` itself what the quoting means.
        let hostile = "a'b\"c $HOME `id` \\ d";
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {}", sh_quote(hostile)))
            .output();
        if let Ok(out) = out {
            assert_eq!(String::from_utf8_lossy(&out.stdout), hostile);
        }
    }

    #[test]
    fn css_format_is_exact() {
        assert_eq!(
            format_css(&event("Blue Monday")),
            "\
:root {
  --myx-primary: #64e0d0;
  --myx-secondary: #4a9fd8;
  --myx-accent: #f4aa48;
  --myx-error: #e05561;
  --myx-warning: #d9a441;
  --myx-success: #61c766;
  --myx-info: #64e0d0;
  --myx-text: #d8efff;
  --myx-text-muted: #7a90a4;
  --myx-background: #081018;
  --myx-background-panel: #101d2a;
  --myx-background-element: #18293a;
  --myx-border: #22374a;
  --myx-border-active: #42d9d0;
  --myx-border-subtle: #182838;
  --myx-border-dimmest: #101c28;
  --myx-on-primary: #0b0b0b;
  --myx-on-secondary: #0b0b0b;
  --myx-on-accent: #0b0b0b;
  --myx-on-background: #d8efff;
}"
        );
    }

    #[test]
    fn hex_format_is_tab_separated_two_columns() {
        let out = format_hex(&event("Blue Monday"));
        assert_eq!(out.lines().count(), 20);
        assert_eq!(out.lines().next().unwrap(), "primary\t#64e0d0");
        assert_eq!(out.lines().last().unwrap(), "on_background\t#d8efff");
        for line in out.lines() {
            let (token, hex) = line.split_once('\t').expect("exactly one tab");
            assert!(!token.contains(' '), "token must be cut-friendly: {token}");
            assert!(hex.starts_with('#') && hex.len() == 7, "bad hex {hex}");
        }
    }

    #[test]
    fn json_format_is_the_ndjson_line_without_its_newline() {
        let msg = Message::Theme(event("Blue Monday"));
        let out = format_message(&msg, Format::Json);
        assert!(!out.ends_with('\n'));
        assert_eq!(format!("{out}\n"), msg.to_ndjson().unwrap());
        // And it really is a single parseable line.
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["colors"]["primary"], "#64e0d0");
    }

    #[test]
    fn bye_becomes_a_comment_in_each_text_format() {
        let bye = Message::Bye(ByeEvent {
            v: PROTOCOL_VERSION,
            seq: 9,
            ts: 1,
            reason: ByeReason::Shutdown,
        });
        assert_eq!(
            format_message(&bye, Format::Sh),
            "# myx: publisher going away (Shutdown)"
        );
        assert_eq!(
            format_message(&bye, Format::Hex),
            "# myx: publisher going away (Shutdown)"
        );
        assert_eq!(
            format_message(&bye, Format::Css),
            "/* myx: publisher going away (Shutdown) */"
        );
        assert!(format_message(&bye, Format::Json).contains(r#""t":"bye""#));
    }

    // ------------------------------------------------------------ parsing

    fn parse(args: &[&str]) -> Result<Parsed, String> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_args(&owned)
    }

    fn run_args(args: &[&str]) -> Args {
        match parse(args).expect("should parse") {
            Parsed::Run(a) => a,
            Parsed::Help => panic!("expected a runnable invocation"),
        }
    }

    #[test]
    fn defaults_are_get_sh_default_socket_no_exec() {
        let a = run_args(&["get"]);
        assert_eq!(a.cmd, Cmd::Get);
        assert_eq!(a.format, Format::Sh);
        assert_eq!(a.socket, None);
        assert_eq!(a.exec, None);
    }

    #[test]
    fn every_format_name_parses() {
        for (name, want) in [
            ("sh", Format::Sh),
            ("css", Format::Css),
            ("hex", Format::Hex),
            ("json", Format::Json),
        ] {
            assert_eq!(run_args(&["watch", "--format", name]).format, want);
            // `--format=json` must mean the same thing as `--format json`.
            let joined = format!("--format={name}");
            assert_eq!(run_args(&["watch", &joined]).format, want);
        }
    }

    #[test]
    fn flags_may_precede_the_subcommand() {
        let a = run_args(&["--format", "css", "--socket", "/tmp/x.sock", "watch"]);
        assert_eq!(a.cmd, Cmd::Watch);
        assert_eq!(a.format, Format::Css);
        assert_eq!(a.socket, Some(PathBuf::from("/tmp/x.sock")));
    }

    #[test]
    fn exec_captures_the_whole_command_string() {
        let a = run_args(&["watch", "--exec", "notify-send \"$MYX_PRIMARY\""]);
        assert_eq!(a.exec.as_deref(), Some("notify-send \"$MYX_PRIMARY\""));
    }

    #[test]
    fn help_is_not_an_error() {
        assert_eq!(parse(&["--help"]), Ok(Parsed::Help));
        assert_eq!(parse(&["get", "-h"]), Ok(Parsed::Help));
    }

    #[test]
    fn bad_input_is_rejected_with_a_message_not_a_panic() {
        for bad in [
            vec![],                          // no subcommand
            vec!["frobnicate"],              // unknown subcommand
            vec!["get", "--format"],         // missing value
            vec!["get", "--format", "yaml"], // unknown format
            vec!["get", "--socket"],         // missing value
            vec!["get", "--nope"],           // unknown flag
            vec!["get", "watch"],            // two subcommands
        ] {
            assert!(parse(&bad).is_err(), "{bad:?} must be a usage error");
        }
    }
}
