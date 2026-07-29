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

/// How saturated the palette's most colourful swatch must be for the surface to
/// take its full tint. Below this the UI slides toward neutral, which is what a
/// black-and-white cover should actually look like.
const FULL_TINT_AT: f32 = 0.35;

fn theme_from_swatches(swatches: &[Rgb], name: &'static str) -> Theme {
    // The dominant swatch is usually the surface hue, but a white or black cover
    // has no hue at all — `rgb_to_hsl` reports 0 for grey, and 0 is red. Take the
    // hue from the most saturated swatch that has one instead.
    let hue_src = swatches
        .iter()
        .copied()
        .max_by(|&a, &b| rgb_to_hsl(a).s.total_cmp(&rgb_to_hsl(b).s))
        .unwrap_or(swatches[0]);
    let base_hue = rgb_to_hsl(hue_src).h;
    // ...and if even that swatch is grey, tint nothing: the art has no colour to
    // borrow, so the UI stays neutral rather than picking one at random.
    let tint_strength = (rgb_to_hsl(hue_src).s / FULL_TINT_AT).clamp(0.0, 1.0);
    let surface = |s: f32, l: f32| color::tint(base_hue, s * tint_strength, l);

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
        text: surface(0.14, 0.93),
        text_muted: surface(0.13, 0.58),
        // Three background layers, all sharing the album's hue at rising lightness.
        background: surface(0.28, 0.075),
        background_panel: surface(0.26, 0.11),
        background_element: surface(0.22, 0.16),
        // Border shades: subtle chrome that still belongs to the palette.
        border: surface(0.16, 0.34),
        border_active: primary,
        border_subtle: surface(0.16, 0.24),
        border_dimmest: surface(0.18, 0.17),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::hue_distance;

    #[test]
    fn a_greyscale_cover_does_not_come_out_red() {
        // `rgb_to_hsl` reports hue 0 for every grey, and hue 0 is red — which is
        // how a black-and-white cover used to tint the whole UI burgundy.
        let greys = [
            Rgb::new(250, 250, 250),
            Rgb::new(18, 18, 18),
            Rgb::new(128, 128, 130),
        ];
        let theme = theme_from_swatches(&greys, "grey");
        for (what, c) in [
            ("background", theme.background),
            ("text", theme.text),
            ("border", theme.border),
            ("primary", theme.primary),
        ] {
            let s = rgb_to_hsl(c).s;
            assert!(s < 0.15, "{what} invented a hue: saturation {s}");
        }
    }

    #[test]
    fn a_colourful_cover_keeps_its_hue() {
        let blues = [
            Rgb::new(30, 60, 200),
            Rgb::new(20, 40, 160),
            Rgb::new(60, 90, 220),
        ];
        let theme = theme_from_swatches(&blues, "blue");
        let bg = rgb_to_hsl(theme.background);
        assert!(bg.s > 0.15, "colourful art lost its tint: {}", bg.s);
        assert!(hue_distance(bg.h, 225.0) < 45.0, "hue drifted to {}", bg.h);
    }

    #[test]
    fn a_mostly_white_cover_borrows_the_one_colour_it_has() {
        // A white sleeve with a small coloured mark should take that mark's hue,
        // not the hue of the white that dominates it.
        let mostly_white = [
            Rgb::new(252, 252, 250),
            Rgb::new(240, 240, 238),
            Rgb::new(40, 150, 90),
        ];
        let theme = theme_from_swatches(&mostly_white, "white");
        let bg = rgb_to_hsl(theme.background);
        assert!(hue_distance(bg.h, 150.0) < 45.0, "hue is {}", bg.h);
    }
}
