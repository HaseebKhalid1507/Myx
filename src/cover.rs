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
    pub fn make_picker() -> Picker {
        let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

        // Warp advertises kitty support but lacks its unicode-placeholder
        // placement, so kitty renders as tofu. Override *after* the query to
        // keep the detected font size — blacklisting kitty loses it and falls
        // back to halfblocks.
        if picker.protocol_type() == ProtocolType::Kitty
            && std::env::var("TERM_PROGRAM").is_ok_and(|t| t.contains("WarpTerminal"))
        {
            picker.set_protocol_type(ProtocolType::Iterm2);
        }

        // Escape hatch for mis-detected terminals.
        if let Ok(want) = std::env::var("MYX_PROTOCOL") {
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
            match self
                .picker
                .new_protocol(self.img.clone(), Size::new(area.width, area.height), Resize::Fit(None))
            {
                Ok(protocol) => self.cached = Some((area, protocol)),
                Err(_) => return,
            }
        }

        if let Some((_, protocol)) = &self.cached {
            frame.render_widget(Image::new(protocol), area);
        }
    }
}
