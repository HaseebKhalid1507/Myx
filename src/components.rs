//! Reusable render primitives in noodle's visual language.
//!
//! These are the building blocks that make the surface look designed:
//!   * `pill` / `gradient_pill` — the solid & gradient badges (GET/POST → EXPLICIT).
//!   * `gradient_line` — a per-character gradient string (progress bars, headings).
//!   * `left_bar_block` — the signature focus cue: a heavy `┃` on the left edge
//!     instead of a full box.

use crate::gradient::{self, Rgb};
use crate::theme::Theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders};

/// A solid pill badge: `bg` fill + one space of horizontal padding either side.
/// Mirrors noodle's `<Badge>`.
pub fn pill<'a>(label: &str, bg: Rgb, fg: Rgb) -> Vec<Span<'a>> {
    vec![Span::styled(
        format!(" {label} "),
        Style::default()
            .bg(bg.into())
            .fg(fg.into())
            .add_modifier(Modifier::BOLD),
    )]
}

/// A gradient pill: each character gets its own interpolated background, with a
/// padding cell tinted to the stop ends. Mirrors noodle's `<GradientBadge>`.
pub fn gradient_pill<'a>(label: &str, stops: &[Rgb], fg: Rgb) -> Vec<Span<'a>> {
    let chars: Vec<char> = label.chars().collect();
    let n = chars.len();
    let mut spans: Vec<Span<'a>> = Vec::with_capacity(n + 2);

    let first = gradient::interpolate(stops, 0.0);
    let last = gradient::interpolate(stops, 1.0);

    spans.push(Span::styled(" ", Style::default().bg(first.into())));
    for (i, ch) in chars.iter().enumerate() {
        let t = if n > 1 {
            i as f32 / (n - 1) as f32
        } else {
            0.0
        };
        let bg = gradient::interpolate(stops, t);
        spans.push(Span::styled(
            ch.to_string(),
            Style::default()
                .bg(bg.into())
                .fg(fg.into())
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(" ", Style::default().bg(last.into())));
    spans
}

/// Render `text` with a per-character horizontal gradient across `stops`.
/// Great for headings or a "played" progress segment.
pub fn gradient_line<'a>(text: &str, stops: &[Rgb]) -> Vec<Span<'a>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    chars
        .iter()
        .enumerate()
        .map(|(i, ch)| {
            let t = if n > 1 {
                i as f32 / (n - 1) as f32
            } else {
                0.0
            };
            let fg = gradient::interpolate(stops, t);
            Span::styled(ch.to_string(), Style::default().fg(fg.into()))
        })
        .collect()
}

/// The signature focus cue. A left-only heavy bar (`┃`) whose color signals
/// focus. Fill the block with `bg` so the row reads as elevated.
///
/// `focused` → `border_active`; otherwise `border_subtle`.
pub fn left_bar_block(theme: &Theme, focused: bool, bg: Color) -> Block<'static> {
    let bar_color: Color = if focused {
        theme.border_active.into()
    } else {
        theme.border_subtle.into()
    };
    Block::default()
        .borders(Borders::LEFT)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(bar_color))
        .style(Style::default().bg(bg))
}

/// A gradient horizontal progress/segment bar as a span vector.
///
/// `width` cells total; `filled` cells (0..=width) use the gradient, the rest use
/// the theme's dimmest border color as a track. Uses a solid block glyph so it
/// reads clean at any width.
pub fn gradient_progress<'a>(
    width: usize,
    filled: usize,
    stops: &[Rgb],
    track: Rgb,
) -> Vec<Span<'a>> {
    let filled = filled.min(width);
    let colors = gradient::sample(stops, width.max(1));
    (0..width)
        .map(|i| {
            if i < filled {
                Span::styled("▬", Style::default().fg(colors[i].into()))
            } else {
                Span::styled("▬", Style::default().fg(track.into()))
            }
        })
        .collect()
}
