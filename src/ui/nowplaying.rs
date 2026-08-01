//! The Now Playing view and the persistent bottom strip (volume, progress).

use super::*;
use crate::*;

/// View ①: album art with track details directly beneath — centered as a group.
pub(crate) fn render_nowplaying_view(
    f: &mut Frame,
    app: &App,
    theme: Theme,
    area: Rect,
    repaint: ArtRepaint,
) {
    if app.playback.now.is_none() {
        f.render_widget(
            Paragraph::new("Nothing playing.\nBrowse ← and press Enter.")
                .style(theme.muted())
                .alignment(Alignment::Center),
            center_v(area, 2),
        );
        return;
    }

    // Split: album art + track info on top, a compact spectrum below, lifted a
    // little off the bottom.
    let chunks = Layout::vertical([
        Constraint::Min(6),    // art + text
        Constraint::Length(7), // spectrum
        Constraint::Length(2), // breathing room (lifts the spectrum up)
    ])
    .split(area);
    let top = chunks[0];
    // Push the art + info group down a little from the top.
    let top = Rect {
        x: top.x,
        y: top.y + 3,
        width: top.width,
        height: top.height.saturating_sub(3),
    };
    let viz_area = chunks[1];

    // Derive the cover's cell footprint from the terminal's font aspect so a
    // square image renders square (and our centering math is exact).
    let font = app.svc.picker.font_size();
    let fw = font.width.max(1) as u32;
    let fh = font.height.max(1) as u32;

    // Reserve 3 rows for text (+1 gap). Cap the art so the group stays compact.
    let avail_h = top.height.saturating_sub(4);
    let mut art_h = avail_h.clamp(3, 14);
    // Square image width in cells for this height: w = h * fh / fw.
    let mut art_w = (art_h as u32 * fh / fw) as u16;
    if art_w > top.width {
        art_w = top.width;
        art_h = (art_w as u32 * fw / fh) as u16;
    }

    let group_h = art_h + 4; // art + gap + title + artist + album
    let art_y = top.y + top.height.saturating_sub(group_h) / 2;
    let art_x = top.x + top.width.saturating_sub(art_w) / 2;
    let art_rect = Rect {
        x: art_x,
        y: art_y,
        width: art_w,
        height: art_h,
    };

    match app.playback.now.as_ref().and_then(|n| n.cover.as_ref()) {
        _ if repaint == ArtRepaint::Wipe => wipe_area(f, art_rect),
        // Writing the escape means transmitting the image, so only do it when
        // something actually asked for it. A theme fade repaints every glyph on
        // screen dozens of times, and re-sending the cover on each of those is
        // what made it flicker.
        Some(cover) if repaint == ArtRepaint::Draw || cover.needs_send(art_rect) => {
            cover.render(f, art_rect)
        }
        // Already on screen: hold the cells so nothing overwrites the picture,
        // and send nothing.
        Some(_) => hold_area(f, art_rect),
        None => wipe_area(f, art_rect),
    }

    if let Some(n) = app.playback.now.as_ref() {
        let text_rect = Rect {
            x: top.x,
            y: art_rect.y + art_h + 1,
            width: top.width,
            height: 3,
        };
        let lines = vec![
            Line::from(Span::styled(
                truncate(&n.title, top.width as usize),
                Style::default()
                    .fg(theme.text.into())
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                truncate(&n.artist, top.width as usize),
                Style::default().fg(theme.primary.into()),
            )),
            Line::from(Span::styled(
                truncate(&n.album, top.width as usize),
                theme.muted(),
            )),
        ];
        f.render_widget(
            Paragraph::new(lines).alignment(Alignment::Center),
            text_rect,
        );
    }

    render_visualizer(f, app, theme, viz_area);
}

/// Slim persistent bottom strip: play state + track, then the progress bar.
pub(crate) fn render_now_strip(
    f: &mut Frame,
    app: &App,
    out: &mut FrameOut,
    theme: Theme,
    area: Rect,
) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    // Volume meter (top row, far right). Still honest in remote mode: +/- goes
    // to the remote device's volume.
    render_volume(f, app, out, theme, rows[0]);

    // Seek/progress bar (bottom row). Record bar geometry for click-to-seek.
    let pos = app.playback.position_ms();
    let left_len = format!("{} ", fmt_ms(pos)).chars().count() as u16;
    let right_len = format!(
        " {}",
        fmt_ms(
            app.playback
                .now
                .as_ref()
                .map(|n| n.duration_ms)
                .unwrap_or(0)
        )
    )
    .chars()
    .count() as u16;
    let bar_w = rows[1].width.saturating_sub(left_len + right_len);
    out.hits.bar = Some(Rect {
        x: rows[1].x + left_len,
        y: rows[1].y,
        width: bar_w,
        height: 1,
    });
    render_progress(f, app, theme, rows[1]);
}

pub(crate) fn render_progress(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let (pos, dur) = match &app.playback.now {
        Some(n) => (app.playback.position_ms(), n.duration_ms.max(1)),
        None => (0, 1),
    };
    // Compute the bar width from the exact label lengths so the duration sits
    // flush against the right edge (aligned with the volume meter above it).
    let left = format!("{} ", fmt_ms(pos));
    let right = format!(" {}", fmt_ms(dur));
    let reserve = left.chars().count() + right.chars().count();
    let bar_w = (area.width as usize).saturating_sub(reserve);
    let filled = ((pos as f32 / dur as f32) * bar_w as f32) as usize;

    let mut spans = vec![Span::styled(left, theme.muted())];
    spans.extend(gradient_progress(
        bar_w,
        filled,
        &[theme.primary, theme.accent],
        theme.border_dimmest,
    ));
    spans.push(Span::styled(right, theme.muted()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The volume meter — a graduated ramp + percentage, right-aligned in `area`.
/// Stashes the 8-bar region on `out` for click/drag control.
pub(crate) fn render_volume(
    f: &mut Frame,
    app: &App,
    out: &mut FrameOut,
    theme: Theme,
    area: Rect,
) {
    const VLEV: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let filled = (app.transport.volume as usize * VLEV.len() + 50) / 100;
    let mut vspans: Vec<Span> = Vec::with_capacity(VLEV.len() + 1);
    for (i, ch) in VLEV.iter().enumerate() {
        let color = if i < filled {
            theme.primary
        } else {
            theme.border_dimmest
        };
        vspans.push(Span::styled(
            ch.to_string(),
            Style::default().fg(color.into()),
        ));
    }
    vspans.push(Span::styled(
        format!(" {:>3}%", app.transport.volume),
        theme.muted(),
    ));
    f.render_widget(
        Paragraph::new(Line::from(vspans)).alignment(Alignment::Right),
        area,
    );
    // 8-bar region for click/drag. Content is 13 cells (8 bars + " NNN%"),
    // right-aligned, so the bars start 13 cells in from the right edge.
    out.hits.vol = Some(Rect {
        x: area.right().saturating_sub(13),
        y: area.y,
        width: VLEV.len() as u16,
        height: 1,
    });
}
