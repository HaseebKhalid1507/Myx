//! WCAG contrast math and the `on_*` foreground picker.
//!
//! ## The sRGB gamma decode
//!
//! An 8-bit channel value is **not** light. sRGB stores color pre-encoded with
//! a ~2.2 gamma curve so that the limited 256 steps are spent where human
//! vision is most sensitive (the darks). Averaging or comparing those encoded
//! bytes directly — the `(r+g+b)/3 > 128` heuristic every theming project
//! reaches for first — measures *storage*, not *brightness*, and mis-ranks
//! saturated colors badly.
//!
//! So each channel is first normalized to `0.0..=1.0` and then linearized:
//!
//! ```text
//! c_lin = c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ^ 2.4
//! ```
//!
//! The low branch is a linear toe: the pure power function has an infinite
//! slope at zero, which is numerically nasty and would require impractical
//! precision near black, so the standard splices a straight segment in below
//! the knee. Above it, the offset-power curve is the actual inverse of the
//! sRGB transfer function.
//!
//! The linear channels are then weighted by the luminous efficiency of the
//! CIE Y primaries — `0.2126 R + 0.7152 G + 0.0722 B`. Green dominates because
//! the human eye is roughly ten times more sensitive to it than to blue, which
//! is why a saturated blue that "looks bright" is in fact a dark background.
//!
//! ## Why the protocol publishes this, not the consumer
//!
//! MXC exists so a subscriber can paint itself from Myx's album-derived
//! palette. If each consumer computed its own `is_dark` and its own foreground
//! choice, three things would go wrong:
//!
//! 1. **They'd disagree.** A bar using a naive average and a prompt using WCAG
//!    would pick opposite foregrounds for the same palette, and the desktop
//!    would visibly desynchronize on borderline album art.
//! 2. **They'd mostly be wrong.** Skipping the gamma decode is the common case
//!    in the wild (spec §3.3); it is a silent accessibility bug, not a crash.
//! 3. **It isn't their job.** Myx already knows the exact palette and its
//!    provenance. Shipping the derived answer is cheaper on the wire than a
//!    hex triple's worth of ambiguity, and it makes correctness a
//!    one-implementation problem.
//!
//! Hence [`Contrast`] rides in every `ThemeEvent` alongside the raw tokens:
//! consumers that care can recompute, but nobody *has* to.

use serde::{Deserialize, Serialize};

use crate::gradient::Rgb;
use crate::mxc::wire::{Colors, Hex};

/// WCAG 2.x relative luminance of an sRGB color, in `0.0..=1.0`.
///
/// Exactly the reference formula — pure black is `0.0`, pure white is `1.0`.
pub fn relative_luminance(c: Rgb) -> f64 {
    /// Undo the sRGB transfer function for one channel. See module docs for
    /// why the sub-knee branch is linear rather than a pure power.
    fn linearize(channel: u8) -> f64 {
        let c = channel as f64 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    0.2126 * linearize(c.r) + 0.7152 * linearize(c.g) + 0.0722 * linearize(c.b)
}

/// WCAG contrast ratio between two colors, in `1.0..=21.0`.
///
/// Symmetric by construction (the brighter color is always the numerator), so
/// callers never have to care which argument is foreground. The `0.05` terms
/// model ambient screen flare — without them the ratio would diverge to
/// infinity against pure black.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether a color reads as a dark surface.
///
/// The `0.5` split is on *luminance*, not on channel bytes, so a saturated
/// mid-blue correctly lands on the dark side. This is the same predicate that
/// populates `ThemeEvent::is_dark`.
pub fn is_dark(c: Rgb) -> bool {
    relative_luminance(c) < 0.5
}

/// WCAG AA minimum contrast ratio for normal-size body text.
pub const AA: f64 = 4.5;

/// The near-black foreground. Not pure `#000000`: terminals render true black
/// against a light surface as a harsh edge, and the extra 11/255 costs
/// essentially nothing in contrast (still ~19.6:1 on white).
pub const INK: Rgb = Rgb::new(0x0b, 0x0b, 0x0b);

/// The light foreground, pure white — nothing beats it on a dark surface.
pub const PAPER: Rgb = Rgb::new(0xff, 0xff, 0xff);

/// Pick whichever of [`INK`] / [`PAPER`] contrasts *more* against `bg`.
///
/// Deliberately "max ratio" rather than "first that passes AA": for mid-tone
/// backgrounds neither option may reach 4.5, and in that case the best
/// available answer is still strictly better than an arbitrary one.
pub fn best_on(bg: Rgb) -> Rgb {
    if contrast_ratio(bg, INK) >= contrast_ratio(bg, PAPER) {
        INK
    } else {
        PAPER
    }
}

/// Like [`best_on`], but keeps `preferred` when it already clears AA.
///
/// Used for `on_background` so the theme's own tinted text color survives
/// instead of being flattened to black-or-white — album-derived palettes are
/// usually tinted on purpose, and overriding a perfectly legible `#d8efff`
/// with `#ffffff` throws away the palette's character for no accessibility
/// gain.
pub fn best_on_preferring(bg: Rgb, preferred: Rgb) -> Rgb {
    if contrast_ratio(bg, preferred) >= AA {
        preferred
    } else {
        best_on(bg)
    }
}

/// Pre-computed legible foregrounds for the palette's key surfaces.
///
/// Rides in every `ThemeEvent`. See the module docs for why the publisher
/// answers this instead of the subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contrast {
    /// Text to place on `colors.primary`.
    pub on_primary: Hex,
    /// Text to place on `colors.secondary`.
    pub on_secondary: Hex,
    /// Text to place on `colors.accent`.
    pub on_accent: Hex,
    /// Body text on `colors.background` — the theme's own `text` when it
    /// passes AA, otherwise the best of ink/paper.
    pub on_background: Hex,
}

impl Contrast {
    /// Derive the whole block from a palette.
    ///
    /// The three role tokens get an unconditional ink/paper pick because they
    /// are used as *fills* behind short labels, where maximum legibility beats
    /// tonal harmony. Only `on_background` — which covers most of the pixels a
    /// user actually reads — is allowed to keep the palette's tint.
    pub fn compute(colors: &Colors) -> Contrast {
        Contrast {
            on_primary: Hex(best_on(colors.primary.into())),
            on_secondary: Hex(best_on(colors.secondary.into())),
            on_accent: Hex(best_on(colors.accent.into())),
            on_background: Hex(best_on_preferring(
                colors.background.into(),
                colors.text.into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: Rgb = Rgb::new(0, 0, 0);
    const WHITE: Rgb = Rgb::new(255, 255, 255);
    const EPS: f64 = 1e-9;

    #[test]
    fn luminance_endpoints_are_exact() {
        assert!((relative_luminance(WHITE) - 1.0).abs() < EPS);
        assert!(relative_luminance(BLACK).abs() < EPS);
    }

    #[test]
    fn black_on_white_is_the_maximum_21_to_1() {
        assert!((contrast_ratio(BLACK, WHITE) - 21.0).abs() < 0.01);
    }

    #[test]
    fn a_color_against_itself_is_1_to_1() {
        for c in [BLACK, WHITE, Rgb::new(0x64, 0xe0, 0xd0), INK, PAPER] {
            assert!(
                (contrast_ratio(c, c) - 1.0).abs() < EPS,
                "{c:?} against itself must be 1.0"
            );
        }
    }

    #[test]
    fn contrast_ratio_is_symmetric() {
        let pairs = [
            (BLACK, WHITE),
            (Rgb::new(0x08, 0x10, 0x18), Rgb::new(0xd8, 0xef, 0xff)),
            (Rgb::new(0x76, 0x76, 0x76), Rgb::new(0xf4, 0xaa, 0x48)),
        ];
        for (a, b) in pairs {
            assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < EPS);
        }
    }

    /// `#767676` on white is the canonical WCAG AA boundary grey: it is the
    /// darkest grey that still passes, and one step lighter fails.
    #[test]
    fn wcag_boundary_grey_brackets_aa() {
        let pass = contrast_ratio(Rgb::new(0x76, 0x76, 0x76), WHITE);
        let fail = contrast_ratio(Rgb::new(0x77, 0x77, 0x77), WHITE);
        assert!(pass >= AA, "#767676 on white must pass AA, got {pass}");
        assert!(fail < AA, "#777777 on white must fail AA, got {fail}");
    }

    #[test]
    fn is_dark_splits_on_luminance() {
        assert!(is_dark(Rgb::new(0x08, 0x10, 0x18)));
        assert!(!is_dark(WHITE));
    }

    #[test]
    fn best_on_meets_aa_against_a_bright_token() {
        let bg = Rgb::new(0x64, 0xe0, 0xd0);
        let fg = best_on(bg);
        assert!(
            contrast_ratio(bg, fg) >= AA,
            "best_on({bg:?}) = {fg:?} only reached {}",
            contrast_ratio(bg, fg)
        );
    }

    #[test]
    fn best_on_preferring_keeps_a_passing_preference() {
        let bg = Rgb::new(0x08, 0x10, 0x18);
        let text = Rgb::new(0xd8, 0xef, 0xff);
        assert_eq!(best_on_preferring(bg, text), text);
    }

    #[test]
    fn best_on_preferring_rejects_a_failing_preference() {
        let bg = BLACK;
        let text = Rgb::new(0x10, 0x10, 0x10);
        let got = best_on_preferring(bg, text);
        assert_ne!(got, text);
        assert_eq!(got, best_on(bg));
        assert!(contrast_ratio(bg, got) >= AA);
    }

    #[test]
    fn compute_prefers_theme_text_only_for_background() {
        let colors: Colors = serde_json::from_str(
            r##"{
                "primary":"#64e0d0","secondary":"#4a9fd8","accent":"#f4aa48",
                "error":"#e05561","warning":"#d9a441","success":"#61c766",
                "info":"#64e0d0","text":"#d8efff","text_muted":"#7a90a4",
                "background":"#081018","background_panel":"#101d2a",
                "background_element":"#18293a","border":"#22374a",
                "border_active":"#42d9d0","border_subtle":"#182838",
                "border_dimmest":"#101c28"
            }"##,
        )
        .unwrap();

        let c = Contrast::compute(&colors);
        assert_eq!(
            c.on_background, colors.text,
            "tinted text passes AA, keep it"
        );
        assert_eq!(c.on_primary, Hex(INK), "#64e0d0 is a light fill");
        assert_eq!(c.on_accent, Hex(INK), "#f4aa48 is a light fill");
    }
}
