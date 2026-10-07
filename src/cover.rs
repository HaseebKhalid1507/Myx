//! Album-art rendering via `ratatui-image`.
//!
//! Auto-detects the terminal's graphics protocol (kitty / sixel / iTerm2) at
//! startup and falls back to unicode half-blocks so *something* always renders.
//! The encoded protocol is cached per render area and cell size — re-encoding
//! only happens when the cover box or the terminal's font changes, keeping the
//! render loop cheap.

use crossterm::cursor::{RestorePosition, SavePosition};
use crossterm::queue;
use image::DynamicImage;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::widgets::Widget;
use ratatui::Frame;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::{
    halfblocks::Halfblocks, iterm2::Iterm2, kitty::Kitty, sixel::Sixel, Protocol,
};
use ratatui_image::{FontSize, Image, Resize};
use std::cell::RefCell;
use std::io::{self, Write};
use std::sync::OnceLock;

pub struct Cover {
    img: DynamicImage,
    picker: Picker,
    /// (area and cell size it was encoded for, encoded protocol).
    ///
    /// Behind a `RefCell` so rendering can take `&self`: the TUI is
    /// single-threaded and `Cover::render` runs at most once per cover per
    /// frame, so the borrow never overlaps another one — no reentrancy, no
    /// aliasing. That guarantee is a runtime panic rather than a compile error,
    /// so moving rendering off this thread would have to move the cache too.
    cached: RefCell<Option<(Rect, FontSize, Protocol)>>,
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

    /// The cell size the picker measured at startup — right until the user
    /// changes the font; a long-running UI measures again (see `term`).
    pub fn startup_cell(&self) -> FontSize {
        self.picker.font_size()
    }

    /// Render the cover into `area`, re-encoding only when the area changes.
    /// Drop the cached encode so the next render re-encodes and ratatui
    /// sees a fresh cell, forcing retransmission.
    pub fn invalidate_cache(&mut self) {
        *self.cached.borrow_mut() = None;
    }

    /// Whether drawing into `area` at cell size `cell` would have to re-encode,
    /// meaning the image must go to the terminal again.
    pub fn needs_send(&self, area: Rect, cell: FontSize) -> bool {
        self.cached
            .borrow()
            .as_ref()
            .map(|(cached_area, cached_cell, _)| {
                *cached_area != area || !same_cell(*cached_cell, cell)
            })
            .unwrap_or(true)
    }

    /// Draw the cover into `area`, sized for cells of `cell` pixels — the
    /// terminal's cell *now*, which a font change can make differ from the one
    /// measured at startup.
    pub fn render(&self, frame: &mut Frame, area: Rect, cell: FontSize) {
        if !self.ensure_cached(area, cell) {
            return;
        }
        let cached = self.cached.borrow();
        if let Some((_, _, protocol)) = &*cached {
            frame.render_widget(Image::new(protocol), area);
        }
    }

    /// Replay the cached protocol straight to a terminal writer.
    ///
    /// This is used after a popup that covered the image closes. Going through
    /// the regular terminal diff can discard a byte-identical image anchor, so
    /// render it against a fresh buffer and send that diff in the same
    /// synchronized update as the popup-free frame.
    pub fn render_direct<W: Write>(
        &self,
        writer: &mut W,
        area: Rect,
        cell: FontSize,
    ) -> io::Result<()> {
        if !self.ensure_cached(area, cell) {
            return Ok(());
        }

        let cached = self.cached.borrow();
        let Some((_, _, protocol)) = &*cached else {
            return Ok(());
        };
        let previous = Buffer::empty(area);
        let mut current = Buffer::empty(area);
        Image::new(protocol).render(area, &mut current);

        queue!(writer, SavePosition)?;
        {
            let mut backend = CrosstermBackend::new(&mut *writer);
            backend.draw(previous.diff_iter(&current))?;
        }
        queue!(writer, RestorePosition)?;
        writer.flush()
    }

    /// Make sure the protocol cache matches `area` and `cell`. A failed encode
    /// leaves the cache empty and both rendering paths become a no-op.
    fn ensure_cached(&self, area: Rect, cell: FontSize) -> bool {
        if area.width == 0 || area.height == 0 || cell.width == 0 || cell.height == 0 {
            return false;
        }
        let mut cached = self.cached.borrow_mut();
        let needs_encode = cached
            .as_ref()
            .map(|(cached_area, cached_cell, _)| {
                *cached_area != area || !same_cell(*cached_cell, cell)
            })
            .unwrap_or(true);

        if needs_encode {
            match encode(&self.img, &self.picker, cell, area) {
                Ok(protocol) => *cached = Some((area, cell, protocol)),
                Err(_) => return false,
            }
        }
        true
    }
}

/// Encode `img` to fit `area` at cell size `cell`.
///
/// What `Picker::new_protocol` does, except that it always uses the cell the
/// picker measured at startup, and a `Picker`'s cell can't be changed: after a
/// font change the cover came out too small (bigger font) or overflowing and
/// cropped (smaller font). The protocol is still the one picked at startup,
/// and the tmux wrapping matches what the picker would have done.
fn encode(
    img: &DynamicImage,
    picker: &Picker,
    cell: FontSize,
    area: Rect,
) -> Result<Protocol, ratatui_image::errors::Errors> {
    let resize = Resize::Fit(None);
    let size = resize.size_for(img, cell, Size::new(area.width, area.height));
    let image = resize.resize(img, cell, size, None);
    let proto = picker.protocol_type();
    let wrap = tmux_wraps(proto);
    Ok(match proto {
        ProtocolType::Halfblocks => Protocol::Halfblocks(Halfblocks::new(image, size)?),
        ProtocolType::Sixel => Protocol::Sixel(Sixel::new(image, size, wrap)?),
        ProtocolType::Kitty => Protocol::Kitty(Kitty::new(image, size, kitty_image_id(), wrap)?),
        ProtocolType::Iterm2 => Protocol::ITerm2(Iterm2::new(image, size, wrap)?),
    })
}

/// `FontSize` has no `PartialEq`.
pub fn same_cell(a: FontSize, b: FontSize) -> bool {
    (a.width, a.height) == (b.width, b.height)
}

/// A fresh random kitty image id per encode, as `ratatui-image` uses, from std
/// alone (`rand` is a streaming-only dependency and this module isn't).
fn kitty_image_id() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let id = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish() as u32;
    id.max(1)
}

/// Whether escapes for `proto` go out wrapped in tmux passthrough: what the
/// startup `Picker` decided, worked out the same way. `ratatui-image` wraps
/// whenever the environment says tmux; `make_picker` builds the one exception,
/// sixel that tmux stores itself, which must go out bare.
fn tmux_wraps(proto: ProtocolType) -> bool {
    wraps_for_tmux(
        proto,
        std::env::var("TERM").ok().as_deref(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        tmux_stores_sixel,
    )
}

/// [`tmux_wraps`] with its inputs passed in, so it can be tested.
fn wraps_for_tmux(
    proto: ProtocolType,
    term: Option<&str>,
    term_program: Option<&str>,
    tmux_stores_sixel: impl Fn() -> bool,
) -> bool {
    let in_tmux = term.is_some_and(|t| t.starts_with("tmux")) || term_program == Some("tmux");
    in_tmux && !(proto == ProtocolType::Sixel && tmux_stores_sixel())
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

    #[test]
    fn cached_cover_can_be_replayed_without_a_blank_frame() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            4,
            4,
            image::Rgb([24, 120, 220]),
        ));
        let cover = Cover::from_image(img, Picker::halfblocks());
        let mut output = Vec::new();

        let cell = FontSize::new(10, 20);
        cover
            .render_direct(&mut output, Rect::new(3, 2, 4, 2), cell)
            .expect("direct cover render");

        assert!(!output.is_empty());
        assert!(String::from_utf8_lossy(&output).contains('▀'));
        assert!(!cover.needs_send(Rect::new(3, 2, 4, 2), cell));
    }

    fn square_cover() -> Cover {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            640,
            640,
            image::Rgb([200, 60, 40]),
        ));
        Cover::from_image(img, Picker::halfblocks())
    }

    #[test]
    fn a_font_change_makes_the_cover_re_encode_even_in_the_same_cells() {
        let cover = square_cover();
        let area = Rect::new(0, 0, 30, 14);
        let mut out = Vec::new();
        cover
            .render_direct(&mut out, area, FontSize::new(10, 22))
            .expect("render");
        assert!(!cover.needs_send(area, FontSize::new(10, 22)));
        // Same cells, bigger font: the cached encode is for the wrong pixels.
        assert!(cover.needs_send(area, FontSize::new(13, 29)));
    }

    #[test]
    fn the_cover_is_fitted_with_the_cell_it_is_given() {
        // A 20x10 box. With square 10x10 cells it is 200x100 px, so a square
        // cover fits 100 px: 10x10 cells. With tall 10x30 cells the same box
        // is 200x300 px and the cover fits 200 px: 20 cells wide, 7 rows. The
        // startup cell would have given the first answer for both.
        let cover = square_cover();
        let area = Rect::new(0, 0, 20, 10);
        for (cell, want) in [
            (FontSize::new(10, 10), Size::new(10, 10)),
            (FontSize::new(10, 30), Size::new(20, 7)),
        ] {
            let proto = encode(&cover.img, &cover.picker, cell, area).expect("encode");
            assert_eq!(proto.size(), want, "{cell:?}");
        }
    }

    #[test]
    fn only_tmux_wraps_and_never_the_sixel_tmux_keeps() {
        let stores = || true;
        let not = || false;
        // Outside tmux nothing is wrapped.
        assert!(!wraps_for_tmux(
            ProtocolType::Kitty,
            Some("xterm-kitty"),
            None,
            stores
        ));
        // tmux, detected either way ratatui-image detects it.
        assert!(wraps_for_tmux(
            ProtocolType::Kitty,
            Some("tmux-256color"),
            None,
            not
        ));
        assert!(wraps_for_tmux(
            ProtocolType::Kitty,
            Some("screen-256color"),
            Some("tmux"),
            not
        ));
        assert!(wraps_for_tmux(
            ProtocolType::Iterm2,
            Some("tmux-256color"),
            None,
            stores
        ));
        // Sixel goes bare only when tmux stores it; otherwise it rides passthrough.
        assert!(!wraps_for_tmux(
            ProtocolType::Sixel,
            Some("tmux-256color"),
            None,
            stores
        ));
        assert!(wraps_for_tmux(
            ProtocolType::Sixel,
            Some("tmux-256color"),
            None,
            not
        ));
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
