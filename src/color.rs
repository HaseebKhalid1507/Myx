//! Color science for reactive theming.
//!
//! RGB↔HSL conversion plus the manipulation helpers the palette-derivation engine
//! needs to turn a handful of raw album-art swatches into a coherent theme:
//! clamp colors so they read well on a dark background, measure hue distance to
//! pick contrasting accents, and synthesize tints for the background layers.

use crate::gradient::Rgb;

/// Hue (degrees, 0..360), saturation & lightness (0..1).
#[derive(Debug, Clone, Copy)]
pub struct Hsl {
    pub h: f32,
    pub s: f32,
    pub l: f32,
}

impl Hsl {
    pub fn new(h: f32, s: f32, l: f32) -> Self {
        Self {
            h: h.rem_euclid(360.0),
            s: s.clamp(0.0, 1.0),
            l: l.clamp(0.0, 1.0),
        }
    }
}

pub fn rgb_to_hsl(c: Rgb) -> Hsl {
    let r = f32::from(c.r) / 255.0;
    let g = f32::from(c.g) / 255.0;
    let b = f32::from(c.b) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;

    if d.abs() < f32::EPSILON {
        return Hsl { h: 0.0, s: 0.0, l };
    }

    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if (max - r).abs() < f32::EPSILON {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if (max - g).abs() < f32::EPSILON {
        60.0 * (((b - r) / d) + 2.0)
    } else {
        60.0 * (((r - g) / d) + 4.0)
    };
    Hsl {
        h: h.rem_euclid(360.0),
        s,
        l,
    }
}

pub fn hsl_to_rgb(hsl: Hsl) -> Rgb {
    let c = (1.0 - (2.0 * hsl.l - 1.0).abs()) * hsl.s;
    let hp = hsl.h / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = hsl.l - c / 2.0;
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgb::new(to(r1), to(g1), to(b1))
}

/// Shortest angular distance between two hues, in degrees (0..180).
pub fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// A crude "vibrance" score — prefer saturated, mid-lightness colors. Used to
/// rank swatches so the accent isn't a muddy near-black or a blown-out near-white.
pub fn vibrance(c: Rgb) -> f32 {
    let h = rgb_to_hsl(c);
    let l_penalty = 1.0 - (h.l - 0.55).abs() * 1.3;
    h.s * l_penalty.max(0.0)
}

/// Clamp a color into a range that reads cleanly as a foreground on a dark
/// surface: bright enough to pop, saturated enough to feel intentional.
pub fn for_dark_fg(c: Rgb) -> Rgb {
    let h = rgb_to_hsl(c);
    hsl_to_rgb(Hsl::new(h.h, h.s.clamp(0.45, 0.95), h.l.clamp(0.58, 0.76)))
}

/// Build a color from a base hue with explicit saturation & lightness. The
/// workhorse for synthesizing background layers and border shades that all share
/// the album's dominant hue.
pub fn tint(base_hue: f32, s: f32, l: f32) -> Rgb {
    hsl_to_rgb(Hsl::new(base_hue, s, l))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_roundtrip_is_stable() {
        for c in [
            Rgb::new(130, 170, 255),
            Rgb::new(40, 200, 90),
            Rgb::new(0, 0, 0),
            Rgb::new(255, 255, 255),
            Rgb::new(123, 45, 200),
        ] {
            let back = hsl_to_rgb(rgb_to_hsl(c));
            // allow ±2 per channel for float rounding
            assert!((i16::from(back.r) - i16::from(c.r)).abs() <= 2, "{c:?} -> {back:?}");
            assert!((i16::from(back.g) - i16::from(c.g)).abs() <= 2, "{c:?} -> {back:?}");
            assert!((i16::from(back.b) - i16::from(c.b)).abs() <= 2, "{c:?} -> {back:?}");
        }
    }

    #[test]
    fn hue_distance_wraps() {
        assert!((hue_distance(350.0, 10.0) - 20.0).abs() < 0.01);
        assert!((hue_distance(0.0, 180.0) - 180.0).abs() < 0.01);
    }

    #[test]
    fn for_dark_fg_lifts_dark_colors() {
        let dark = Rgb::new(20, 10, 40);
        let lifted = rgb_to_hsl(for_dark_fg(dark));
        assert!(lifted.l >= 0.57);
    }
}
