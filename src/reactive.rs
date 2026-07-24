//! Album-art-reactive theming — the signature move.
//!
//! Extracts a dominant palette from the cover with `color-thief`, then derives a
//! complete semantic [`Theme`] from it:
//!   * the **dominant** swatch sets a base hue that tints all three background
//!     layers and the border shades — this is what makes the whole UI feel like
//!     it *belongs* to the album;
//!   * the most **vibrant** swatch becomes `primary`, the most **hue-distant**
//!     vibrant swatch becomes `accent`, giving contrast that isn't muddy;
//!   * status colors snap to the nearest palette swatch of the right hue when one
//!     exists, otherwise fall back to a sensible synthesized tone.
//!
//! Everything is clamped through [`color::for_dark_fg`] so derived colors always
//! read cleanly on the dark surface, no matter how blown-out or murky the art is.

use image::DynamicImage;

use crate::color::{self, for_dark_fg, hue_distance, rgb_to_hsl, tint, vibrance};
use crate::gradient::Rgb;
use crate::theme::Theme;

/// Derive a theme from a decoded cover image. Falls back to a neutral dark theme
/// if palette extraction yields nothing.
pub fn derive_theme(img: &DynamicImage, name: &'static str) -> Theme {
    let rgb = img.to_rgb8();
    let swatches: Vec<Rgb> =
        match color_thief::get_palette(rgb.as_raw(), color_thief::ColorFormat::Rgb, 10, 8) {
            Ok(p) if !p.is_empty() => p.into_iter().map(|c| Rgb::new(c.r, c.g, c.b)).collect(),
            _ => return crate::theme::TOKYONIGHT,
        };
    theme_from_swatches(&swatches, name)
}

/// Pick the palette swatch whose hue is nearest `target_hue`. Returns the
/// dark-normalized swatch if one lands within `max_dist` degrees, else `None`.
fn nearest_hue(swatches: &[Rgb], target_hue: f32, max_dist: f32) -> Option<Rgb> {
    swatches
        .iter()
        .map(|&c| (c, hue_distance(rgb_to_hsl(c).h, target_hue)))
        .filter(|&(_, d)| d <= max_dist)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(c, _)| for_dark_fg(c))
}

fn theme_from_swatches(swatches: &[Rgb], name: &'static str) -> Theme {
    // Dominant swatch (color-thief returns it first) → base hue for the surface.
    let dominant = swatches[0];
    let base_hue = rgb_to_hsl(dominant).h;

    // Rank by vibrance for the accent selection.
    let mut ranked: Vec<Rgb> = swatches.to_vec();
    ranked.sort_by(|a, b| vibrance(*b).total_cmp(&vibrance(*a)));

    let primary_src = ranked[0];
    let primary_hue = rgb_to_hsl(primary_src).h;

    // Accent = the most hue-distant *reasonably vibrant* swatch from primary.
    let accent_src = ranked
        .iter()
        .skip(1)
        .filter(|&&c| vibrance(c) > 0.12)
        .max_by(|&&a, &&b| {
            hue_distance(rgb_to_hsl(a).h, primary_hue)
                .total_cmp(&hue_distance(rgb_to_hsl(b).h, primary_hue))
        })
        .copied()
        .unwrap_or(primary_src);

    let secondary_src = ranked.get(2).copied().unwrap_or(accent_src);

    let primary = for_dark_fg(primary_src);
    let accent = for_dark_fg(accent_src);
    let secondary = for_dark_fg(secondary_src);

    // Average saturation informs synthesized fallbacks so they don't clash.
    let avg_s = swatches.iter().map(|&c| rgb_to_hsl(c).s).sum::<f32>() / swatches.len() as f32;

    // Status colors: snap to a palette swatch of the right hue, else synthesize.
    let error = nearest_hue(swatches, 2.0, 35.0).unwrap_or_else(|| tint(2.0, avg_s.max(0.6), 0.63));
    let warning =
        nearest_hue(swatches, 38.0, 30.0).unwrap_or_else(|| tint(38.0, avg_s.max(0.6), 0.62));
    let success =
        nearest_hue(swatches, 140.0, 40.0).unwrap_or_else(|| tint(140.0, avg_s.max(0.45), 0.6));
    let info = primary;

    Theme {
        name,
        primary,
        secondary,
        accent,
        error,
        warning,
        success,
        info,
        text: color::tint(base_hue, 0.14, 0.93),
        text_muted: color::tint(base_hue, 0.13, 0.58),
        // Three background layers, all sharing the album's hue at rising lightness.
        background: color::tint(base_hue, 0.28, 0.075),
        background_panel: color::tint(base_hue, 0.26, 0.11),
        background_element: color::tint(base_hue, 0.22, 0.16),
        // Border shades: subtle chrome that still belongs to the palette.
        border: color::tint(base_hue, 0.16, 0.34),
        border_active: primary,
        border_subtle: color::tint(base_hue, 0.16, 0.24),
        border_dimmest: color::tint(base_hue, 0.18, 0.17),
    }
}
