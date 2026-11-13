//! The Lyrics view.

use crate::*;

/// Draw the Lyrics view; returns whether the track's title was drawn.
pub(crate) fn render_lyrics(f: &mut Frame, app: &App, theme: Theme, area: Rect) -> bool {
    let inner = area.inner(Margin::new(2, 0));
    if inner.height == 0 {
        return false;
    }
    let max = inner.width as usize;

    // Header: current track title + "artist · album", above the lyrics — when
    // there's room for it and some lyrics too. Below that the lyrics get every
    // row and the strip shows the track instead.
    let mut lyrics_area = inner;
    let mut title_shown = false;
    if let Some(n) = app.playback.now.as_ref().filter(|_| inner.height >= 6) {
        title_shown = true;
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
        return title_shown;
    }

    let cur = if app.view.lyrics_synced {
        app.view
            .lyrics
            .iter()
            .rposition(|(t, _)| *t <= app.playback.position_ms())
            .unwrap_or(0)
    } else {
        0
    };
    let rows = lyric_rows(&app.view.lyrics, cur, max, lyrics_area.height as usize);

    let lines: Vec<Line> = rows
        .into_iter()
        .map(|row| {
            let style = if app.view.lyrics_synced && row.line == cur {
                Style::default()
                    .fg(theme.primary.into())
                    .add_modifier(Modifier::BOLD)
            } else if app.view.lyrics_synced && row.line < cur {
                Style::default().fg(theme.border_subtle.into())
            } else {
                theme.muted()
            };
            Line::from(Span::styled(row.text, style))
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        lyrics_area,
    );
    title_shown
}

/// Most rows one lyric line may take. Past this it ends in `…`, so one long
/// line can't take over a small pane.
pub(crate) const LYRIC_MAX_ROWS: usize = 3;

/// One screen row of the lyrics view, and the lyric line it belongs to.
#[derive(Debug, PartialEq)]
pub(crate) struct LyricRow {
    pub(crate) text: String,
    pub(crate) line: usize,
}

/// The rows that fill a `width` × `height` lyrics pane, each line wrapped
/// (see [`wrap_balanced`]) and line `cur` placed mid-pane. Rows are counted,
/// not lines, so the line being sung stays put however its neighbours wrap;
/// a neighbour cut by the pane's edge shows only the rows that fit. With
/// `cur` near the start there is nothing to put above it, and it sits higher.
pub(crate) fn lyric_rows(
    lyrics: &[(u32, String)],
    cur: usize,
    width: usize,
    height: usize,
) -> Vec<LyricRow> {
    let wrap = |i: usize| -> Vec<LyricRow> {
        let text = &lyrics[i].1;
        let text = if text.trim().is_empty() {
            "♪︎"
        } else {
            text
        };
        wrap_balanced(text, width, LYRIC_MAX_ROWS)
            .into_iter()
            .map(|text| LyricRow { text, line: i })
            .collect()
    };
    if height == 0 || width == 0 || cur >= lyrics.len() {
        return Vec::new();
    }

    let mut rows = wrap(cur);
    rows.truncate(height);
    // Rows above the current line: half the pane, less half of its own block.
    let mut above = (height / 2).saturating_sub(rows.len() / 2);
    let mut i = cur;
    while above > 0 && i > 0 {
        i -= 1;
        let mut line = wrap(i);
        // A line cut by the top edge keeps its last rows, as if scrolled.
        let skip = line.len().saturating_sub(above);
        above -= line.len() - skip;
        line.drain(..skip);
        line.append(&mut rows);
        rows = line;
    }
    let mut i = cur + 1;
    while rows.len() < height && i < lyrics.len() {
        rows.extend(wrap(i));
        i += 1;
    }
    rows.truncate(height);
    rows
}
