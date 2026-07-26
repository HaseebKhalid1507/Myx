//! Album-art rendering via `ratatui-image`.
//!
//! Auto-detects the terminal's graphics protocol (kitty / sixel / iTerm2) at
//! startup and falls back to unicode half-blocks so *something* always renders.
//! The encoded protocol is cached per render area — re-encoding only happens when
//! the cover box changes size, keeping the render loop cheap.

use image::DynamicImage;
use ratatui::layout::{Rect, Size};
use ratatui::Frame;
use ratatui_image::picker::Picker;
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
        Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
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

    /// A blank plate in the given colour, drawn while the real cover loads.
    ///
    /// This has to be an *image*, not blank cells: an inline image is a graphic
    /// the terminal keeps until another image replaces it, so redrawing text
    /// over the area leaves the previous track's art on screen. Transmitting a
    /// flat image is what actually clears it.
    pub fn placeholder(rgb: (u8, u8, u8), picker: Picker) -> Self {
        let px = image::Rgb([rgb.0, rgb.1, rgb.2]);
        // 2x2 is enough — it is scaled up to the render area on encode.
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 2, px));
        Self::from_image(img, picker)
    }

    /// Render the cover into `area`, re-encoding only when the area changes.
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
