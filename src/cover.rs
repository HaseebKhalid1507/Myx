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

pub struct Cover {
    img: DynamicImage,
    picker: Picker,
    /// (area it was encoded for, encoded protocol).
    cached: Option<(Rect, Protocol)>,
}

impl Cover {
    /// Build a `Picker` by querying the terminal, falling back to half-blocks.
    /// Must be called after raw mode is enabled so the query can round-trip.
    pub fn make_picker(preferred: Option<&str>) -> Picker {
        let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

        // Warp and WezTerm answer the kitty query but neither places unicode
        // placeholders, which is how `ratatui-image` draws kitty — the cells
        // come out empty and the cover is a see-through hole. Both draw iTerm2
        // inline images.
        let no_placeholders = std::env::var_os("WEZTERM_EXECUTABLE").is_some()
            || std::env::var("TERM_PROGRAM").is_ok_and(|t| t.contains("WarpTerminal"));

        if tmux_stores_sixel() {
            // The cover has to survive a window switch, and sixel is the only
            // protocol tmux stores in its own pane buffer and repaints itself.
            // Everything else rides through as passthrough, untracked, and is
            // gone the moment tmux repaints the pane from that buffer.
            picker = untmuxed_sixel_picker(picker.font_size());
        } else if no_placeholders {
            // Overridden *after* the query so the detected font size survives:
            // blacklisting kitty up front loses it and falls back to halfblocks.
            picker.set_protocol_type(ProtocolType::Iterm2);
        } else if picker.protocol_type() == ProtocolType::Halfblocks
            && std::env::var_os("KITTY_WINDOW_ID").is_some()
        {
            // Inside tmux the graphics query goes unanswered even when the outer
            // terminal draws images — the cell-size reply still arrives, so it
            // looks like a legitimate halfblocks terminal.
            picker.set_protocol_type(ProtocolType::Kitty);
        }

        // Escape hatch for mis-detected terminals.
        if let Some(want) = std::env::var("MYX_PROTOCOL")
            .ok()
            .or_else(|| preferred.map(String::from))
        {
            match want.to_ascii_lowercase().as_str() {
                "kitty" => picker.set_protocol_type(ProtocolType::Kitty),
                "iterm2" => picker.set_protocol_type(ProtocolType::Iterm2),
                "sixel" => picker.set_protocol_type(ProtocolType::Sixel),
                "halfblocks" => picker.set_protocol_type(ProtocolType::Halfblocks),
                _ => {}
            }
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
            cached: None,
        }
    }

    /// Render the cover into `area`, re-encoding only when the area changes.
    /// Drop the cached encode so the next render re-encodes and ratatui
    /// sees a fresh cell, forcing retransmission.
    pub fn invalidate_cache(&mut self) {
        self.cached = None;
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let needs_encode = self
            .cached
            .as_ref()
            .map(|(cached_area, _)| *cached_area != area)
            .unwrap_or(true);

        if needs_encode {
            match self.picker.new_protocol(
                self.img.clone(),
                Size::new(area.width, area.height),
                Resize::Fit(None),
            ) {
                Ok(protocol) => self.cached = Some((area, protocol)),
                Err(_) => return,
            }
        }

        if let Some((_, protocol)) = &self.cached {
            frame.render_widget(Image::new(protocol), area);
        }
    }
}

/// Whether this tmux both runs us and can store sixel images itself. tmux only
/// reports `sixel` in its terminal features when it was built with sixel
/// support and the outer terminal advertises it.
fn tmux_stores_sixel() -> bool {
    if std::env::var_os("TMUX").is_none() {
        return false;
    }
    std::process::Command::new("tmux")
        .args(["display", "-p", "#{client_termfeatures}"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("sixel"))
}

/// A sixel picker that does *not* wrap its escapes in tmux passthrough.
///
/// `ratatui-image` adds that wrapper whenever the environment looks like tmux,
/// which is exactly what stops tmux from parsing the image and keeping it. The
/// markers are hidden only for the moment the picker reads them.
///
/// ponytail: `set_var` is process-wide, so this races anything else reading
/// `TERM` — nothing does, and it runs once during startup. The clean fix is a
/// `ratatui-image` API for opting out of the tmux wrapper.
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
