//! The Lyrics view.

use crate::*;

pub(crate) fn render_lyrics(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let inner = area.inner(Margin::new(2, 0));
    if inner.height == 0 {
        return;
    }
    let max = inner.width as usize;

    // Header: current track title + "artist · album", above the lyrics.
    let mut lyrics_area = inner;
    if let Some(n) = app.playback.now.as_ref() {
        let head = Layout::vertical([
            Constraint::Length(1), // title
            Constraint::Length(1), // artist / album
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // lyrics
        ])
        .split(inner);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate(&n.title, max),
                Style::default()
                    .fg(theme.text.into())
                    .add_modifier(Modifier::BOLD),
            )))
            .alignment(Alignment::Center),
            head[0],
        );
        let sub = if n.album.is_empty() {
            n.artist.clone()
        } else {
            format!("{} · {}", n.artist, n.album)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(truncate(&sub, max), theme.muted())))
                .alignment(Alignment::Center),
            head[1],
        );
        lyrics_area = head[3];
    }

    if app.view.lyrics.is_empty() {
        let msg = if app.playback.now.is_some() {
            "♪︎  no lyrics for this track"
        } else {
            "♪︎  nothing playing"
        };
        f.render_widget(
            Paragraph::new(msg)
                .style(theme.muted())
                .alignment(Alignment::Center),
            center_v(lyrics_area, 1),
        );
        return;
    }

    let h = lyrics_area.height as usize;
    let pos = app.playback.position_ms();
    let cur = if app.view.lyrics_synced {
        app.view
            .lyrics
            .iter()
            .rposition(|(t, _)| *t <= pos)
            .unwrap_or(0)
    } else {
        0
    };
    let start = cur.saturating_sub(h / 2);

    let mut lines: Vec<Line> = Vec::with_capacity(h);
    for (i, (_, text)) in app.view.lyrics.iter().enumerate().skip(start).take(h) {
        let style = if app.view.lyrics_synced && i == cur {
            Style::default()
                .fg(theme.primary.into())
                .add_modifier(Modifier::BOLD)
        } else if app.view.lyrics_synced && i < cur {
            Style::default().fg(theme.border_subtle.into())
        } else {
            theme.muted()
        };
        let txt = if text.is_empty() {
            "♪︎".to_string()
        } else {
            truncate(text, max)
        };
        lines.push(Line::from(Span::styled(txt, style)));
    }
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        lyrics_area,
    );
}
