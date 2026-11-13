//! The Queue view.

use crate::*;

/// Draw the Queue view; returns whether the track's title was drawn.
pub(crate) fn render_queue_view(f: &mut Frame, app: &App, theme: Theme, area: Rect) -> bool {
    let inner = area.inner(Margin::new(2, 1));
    if inner.height == 0 {
        return false;
    }
    // The title is on the line under "NOW PLAYING", after the "PLAYING FROM"
    // header and its gap when there is one.
    let title_row: u16 = if app.transport.source_name.is_empty() {
        1
    } else {
        3
    };
    let title_shown = app.playback.now.is_some() && inner.height > title_row;
    let max = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    // Context header — what's playing from.
    if !app.transport.source_name.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("PLAYING FROM  ", theme.muted()),
            Span::styled(
                truncate(&app.transport.source_name, max.saturating_sub(14)),
                Style::default()
                    .fg(theme.primary.into())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::raw(""));
    }

    // Now playing — the current track, above the up-next list.
    if let Some(n) = app.playback.now.as_ref() {
        lines.push(Line::from(Span::styled("NOW PLAYING", theme.heading())));
        lines.push(Line::from(vec![
            Span::styled("   ", theme.muted()),
            Span::styled(
                truncate(&n.title, max.saturating_sub(3)),
                Style::default()
                    .fg(theme.text.into())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", n.artist), theme.muted()),
        ]));
        lines.push(Line::raw(""));
    }

    lines.push(Line::from(Span::styled("UP NEXT", theme.heading())));
    lines.push(Line::raw(""));

    let used = lines.len();
    if app.transport.queue.is_empty() {
        lines.push(Line::from(Span::styled("queue is empty", theme.muted())));
    } else {
        for (i, q) in app
            .transport
            .queue
            .iter()
            .take(inner.height.saturating_sub(used as u16) as usize)
            .enumerate()
        {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>2}  ", i + 1), theme.muted()),
                Span::styled(
                    truncate(q, max.saturating_sub(4)),
                    Style::default().fg(theme.text.into()),
                ),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
    title_shown
}
