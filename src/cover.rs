//! Album-art rendering via `ratatui-image`.
//!
//! Auto-detects the terminal's graphics protocol (kitty / sixel / iTerm2) at
//! startup and falls back to unicode half-blocks so *something* always renders.
//! The encoded protocol is cached per render area — re-encoding only happens when
//! the cover box changes size, keeping the render loop cheap.

use image::DynamicImage;
use ratatui::layout::{Rect, Size};
use ratatui::Frame;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};
use std::cell::RefCell;
use std::sync::OnceLock;

pub struct Cover {
    img: DynamicImage,
    picker: Picker,
    /// (area it was encoded for, encoded protocol).
    ///
    /// Behind a `RefCell` so rendering can take `&self`: the TUI is
    /// single-threaded and `Cover::render` runs at most once per cover per
    /// frame, so the borrow never overlaps another one — no reentrancy, no
    /// aliasing. That guarantee is a runtime panic rather than a compile error,
    /// so moving rendering off this thread would have to move the cache too.
    cached: RefCell<Option<(Rect, Protocol)>>,
}

impl Cover {
    /// Build a `Picker` by querying the terminal, falling back to half-blocks.
    ///
    /// Must be called after raw mode is enabled so the query can round-trip, and
    /// before any thread is spawned — see [`untmuxed_sixel_picker`].
    pub fn make_picker(preferred: Option<&str>) -> Picker {
        let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

        // An explicit choice beats every heuristic below — that is the whole
        // point of the escape hatch.
        let forced = std::env::var("MYX_PROTOCOL")
            .ok()
            .or_else(|| preferred.map(String::from))
            .and_then(|want| parse_protocol(&want));

        // The cover has to survive a window switch, and sixel is the only
        // protocol tmux stores in its own pane buffer and repaints itself.
        // Everything else rides through as passthrough, untracked, and is gone
        // the moment tmux repaints the pane from that buffer.
        //
        // Sending it *unwrapped* is what makes tmux store it, which also strips
        // the passthrough every other protocol needs — so a forced kitty or
        // iTerm2 must never come out of this branch, or its escapes reach tmux
        // bare and get eaten.
        if forced.is_none_or(|p| p == ProtocolType::Sixel) && tmux_stores_sixel() {
            return untmuxed_sixel_picker(picker.font_size());
        }

        if let Some(proto) = forced {
            // Set *after* the query so the detected font size survives:
            // blacklisting a protocol up front loses it and falls back to
            // halfblocks.
            picker.set_protocol_type(proto);
            return picker;
        }

        if std::env::var("TERM_PROGRAM").is_ok_and(|t| t.contains("WarpTerminal")) {
            // Warp answers the kitty query but does not place unicode
            // placeholders, which is how `ratatui-image` draws kitty — the cells
            // come out empty and the cover is a see-through hole. WezTerm has
            // the same gap and needs no help here: `ratatui-image` blacklists
            // kitty for it already.
            picker.set_protocol_type(ProtocolType::Iterm2);
        } else if picker.protocol_type() == ProtocolType::Halfblocks && outer_terminal_is_kitty() {
            // Inside tmux the graphics query goes unanswered even when the outer
            // terminal draws images — the cell-size reply still arrives, so it
            // looks like a legitimate halfblocks terminal.
            picker.set_protocol_type(ProtocolType::Kitty);
        }

        picker
    }

    /// Load a cover image from disk. Returns `None` if the file can't be decoded.
    pub fn load(path: &str, picker: Picker) -> Option<Self> {
        let img = image::open(path).ok()?;
        Some(Self::from_image(img, picker))
    }

    /// Build a cover from an already-decoded image (so the caller can also derive
    /// a reactive theme from the same pixels).
    pub fn from_image(img: DynamicImage, picker: Picker) -> Self {
        Self {
            img,
            picker,
            cached: RefCell::new(None),
        }
    }

    /// Render the cover into `area`, re-encoding only when the area changes.
    /// Drop the cached encode so the next render re-encodes and ratatui
    /// sees a fresh cell, forcing retransmission.
    pub fn invalidate_cache(&mut self) {
        *self.cached.borrow_mut() = None;
    }

    /// Whether drawing into `area` would have to re-encode, meaning the image
    /// must go to the terminal again.
    pub fn needs_send(&self, area: Rect) -> bool {
        self.cached
            .borrow()
            .as_ref()
            .map(|(cached_area, _)| *cached_area != area)
            .unwrap_or(true)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mut cached = self.cached.borrow_mut();
        let needs_encode = cached
            .as_ref()
            .map(|(cached_area, _)| *cached_area != area)
            .unwrap_or(true);

        if needs_encode {
            match self.picker.new_protocol(
                self.img.clone(),
                Size::new(area.width, area.height),
                Resize::Fit(None),
            ) {
                Ok(protocol) => *cached = Some((area, protocol)),
                Err(_) => return,
            }
        }

        if let Some((_, protocol)) = &*cached {
            frame.render_widget(Image::new(protocol), area);
        }
    }
}

/// The requested protocol, or `None` for anything unrecognised — a typo must
/// fall back to detection rather than to a protocol the terminal can't draw.
fn parse_protocol(want: &str) -> Option<ProtocolType> {
    match want.to_ascii_lowercase().as_str() {
        "kitty" => Some(ProtocolType::Kitty),
        "iterm2" => Some(ProtocolType::Iterm2),
        "sixel" => Some(ProtocolType::Sixel),
        "halfblocks" => Some(ProtocolType::Halfblocks),
        _ => None,
    }
}

/// What tmux says about the client attached *right now*, as
/// `<termfeatures>|<termname>`. Empty outside tmux.
fn tmux_client_info() -> &'static str {
    static INFO: OnceLock<String> = OnceLock::new();
    INFO.get_or_init(|| {
        if std::env::var_os("TMUX").is_none() {
            return String::new();
        }
        std::process::Command::new("tmux")
            .args(["display", "-p", "#{client_termfeatures}|#{client_termname}"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    })
}

/// Split a `tmux display` reply into the two things we ask it about: whether the
/// client can store sixel, and whether it is kitty.
fn parse_client_info(raw: &str) -> (bool, bool) {
    match raw.split_once('|') {
        Some((features, term)) => (features.contains("sixel"), term.contains("kitty")),
        None => (false, false),
    }
}

/// Whether this tmux both runs us and can store sixel images itself. tmux only
/// reports `sixel` in its terminal features when it was built with sixel
/// support and the outer terminal advertises it.
fn tmux_stores_sixel() -> bool {
    parse_client_info(tmux_client_info()).0
}

/// Whether the terminal actually drawing our output is kitty.
///
/// Inside tmux this has to come from tmux: `KITTY_WINDOW_ID` records whichever
/// terminal *created* the session and stays in its environment forever, so a
/// session reattached from somewhere else would still claim to be kitty and get
/// kitty escapes it can't draw.
fn outer_terminal_is_kitty() -> bool {
    if std::env::var_os("TMUX").is_some() {
        return parse_client_info(tmux_client_info()).1;
    }
    std::env::var_os("KITTY_WINDOW_ID").is_some()
}

/// A sixel picker that does *not* wrap its escapes in tmux passthrough.
///
/// `ratatui-image` adds that wrapper whenever the environment looks like tmux,
/// which is exactly what stops tmux from parsing the image and keeping it. The
/// markers are hidden only for the moment the picker reads them.
///
/// # Safety
///
/// `set_var` mutates the process-wide environment block, which can reallocate it
/// under a concurrent `getenv` of *any* variable. [`Cover::make_picker`] is
/// therefore called from `main` before the tokio runtime and the player engine
/// exist. The only thread that can still overlap is the one `ratatui-image`
/// spawns for its terminal query, and by the time the query's result reaches us
/// that thread is restoring the terminal mode — past its last environment read.
///
/// ponytail: the clean fix is a `ratatui-image` API for opting out of the tmux
/// wrapper; until then, the call-site ordering is the guarantee.
fn untmuxed_sixel_picker(font_size: ratatui_image::FontSize) -> Picker {
    let saved = [
        ("TERM", std::env::var("TERM").ok()),
        ("TERM_PROGRAM", std::env::var("TERM_PROGRAM").ok()),
    ];
    unsafe {
        std::env::set_var("TERM", "xterm-256color");
        std::env::remove_var("TERM_PROGRAM");
    }
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(font_size);
    for (key, value) in saved {
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
    picker.set_protocol_type(ProtocolType::Sixel);
    picker
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use std::time::Instant;

    #[test]
    fn a_protocol_name_maps_to_its_protocol_and_a_typo_to_detection() {
        assert_eq!(parse_protocol("Sixel"), Some(ProtocolType::Sixel));
        assert_eq!(parse_protocol("kitty"), Some(ProtocolType::Kitty));
        assert_eq!(parse_protocol("iterm2"), Some(ProtocolType::Iterm2));
        assert_eq!(parse_protocol("halfblocks"), Some(ProtocolType::Halfblocks));
        assert_eq!(parse_protocol("kity"), None);
    }

    #[test]
    fn tmux_reports_what_the_attached_client_can_do() {
        let (sixel, kitty) = parse_client_info("256,RGB,sixel,title|xterm-256color\n");
        assert!(sixel);
        assert!(!kitty);

        let (sixel, kitty) = parse_client_info("256,RGB,title|xterm-kitty\n");
        assert!(!sixel);
        assert!(kitty);

        // Outside tmux there is no reply at all, and nothing may be inferred:
        // guessing kitty here would send escapes a plain terminal prints raw.
        assert_eq!(parse_client_info(""), (false, false));
    }

    /// What one cover re-encode costs on the UI thread, per protocol. Ignored
    /// because it measures rather than asserts:
    ///   cargo test --lib -- --ignored --nocapture encode_cost
    #[test]
    #[ignore]
    fn encode_cost() {
        let mut img = image::RgbImage::new(640, 640);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
        }
        let img = DynamicImage::ImageRgb8(img);
        let area = Rect::new(0, 0, 30, 15);

        for proto in [
            ProtocolType::Halfblocks,
            ProtocolType::Kitty,
            ProtocolType::Iterm2,
            ProtocolType::Sixel,
        ] {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(proto);
            let mut cover = Cover::from_image(img.clone(), picker);
            let runs = 20;
            let t = Instant::now();
            for _ in 0..runs {
                cover.invalidate_cache();
                let _ = cover.picker.new_protocol(
                    cover.img.clone(),
                    Size::new(area.width, area.height),
                    Resize::Fit(None),
                );
            }
            println!("{proto:?}: {:?} per encode", t.elapsed() / runs);
        }
    }
}
