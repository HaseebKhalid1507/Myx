//! Time-based animation helpers.
//!
//! Right now this drives one thing: the theme cross-fade on track change. The
//! design is deliberately render-clock-independent — `ThemeFade` reads wall-clock
//! elapsed, so the fade lasts the same real duration whether the UI redraws at
//! 60fps or 10fps.

use std::time::{Duration, Instant};

use crate::theme::Theme;

/// Cubic ease-in-out — slow at the ends, quick through the middle. Makes the
/// recolor feel like it *breathes* instead of ramping linearly.
pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = -2.0 * t + 2.0;
        1.0 - (f * f * f) / 2.0
    }
}

/// A running cross-fade between two full themes.
#[derive(Debug, Clone)]
pub struct ThemeFade {
    from: Theme,
    to: Theme,
    start: Instant,
    duration: Duration,
}

impl ThemeFade {
    pub fn new(from: Theme, to: Theme, duration: Duration) -> Self {
        Self {
            from,
            to,
            start: Instant::now(),
            duration,
        }
    }

    /// Linear progress in `[0, 1]` based on wall-clock elapsed.
    pub fn progress(&self) -> f32 {
        let d = self.duration.as_secs_f32();
        if d <= 0.0 {
            return 1.0;
        }
        (self.start.elapsed().as_secs_f32() / d).clamp(0.0, 1.0)
    }

    pub fn is_done(&self) -> bool {
        self.progress() >= 1.0
    }

    /// The interpolated theme for the current instant, with easing applied.
    pub fn current(&self) -> Theme {
        Theme::lerp(&self.from, &self.to, ease_in_out_cubic(self.progress()))
    }

    /// The theme we're fading toward (used to snap cleanly when done).
    pub fn target(&self) -> Theme {
        self.to
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_endpoints() {
        assert!((ease_in_out_cubic(0.0)).abs() < 1e-6);
        assert!((ease_in_out_cubic(1.0) - 1.0).abs() < 1e-6);
        assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn fade_starts_at_from_ends_at_to() {
        let a = crate::theme::TOKYONIGHT;
        let b = crate::theme::GRUVBOX;
        // A zero-duration fade is immediately done and equals the target.
        let f = ThemeFade::new(a, b, Duration::from_millis(0));
        assert!(f.is_done());
        let cur = f.current();
        assert_eq!(cur.background.r, b.background.r);
    }
}
