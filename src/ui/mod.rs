//! The render tree.
//!
//! One-way dependency: everything here reads `&App` and writes `FrameOut`;
//! nothing here mutates application state. One module per screen, so the file
//! to open is the one named after the thing on screen.

mod footer;
mod library;
mod lyrics;
mod nowplaying;
mod overlay;
mod queue;
mod visualizer;

pub(crate) use footer::*;
pub(crate) use library::*;
pub(crate) use lyrics::*;
pub(crate) use nowplaying::*;
pub(crate) use overlay::*;
pub(crate) use queue::*;
pub(crate) use visualizer::*;

use crate::*;

const NOW_PLAYING_SPLIT_MIN_WIDTH: u16 = 72;
const ZEN_QUEUE_SPLIT_MIN_WIDTH: u16 = 52;

pub(crate) fn render(f: &mut Frame, app: &App, out: &mut FrameOut, repaint: ArtRepaint) {
    let theme = app.theme.displayed;
    let area = f.area();
    f.render_widget(Block::default().style(theme.base()), area);
    let area = area.inner(Margin::new(2, 1));

    let rows = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Length(1), // spacer
        Constraint::Min(6),    // body (library | active view)
        Constraint::Length(1), // spacer
        Constraint::Length(2), // now-playing strip
        Constraint::Length(1), // footer
    ])
    .split(area);

    // Header: wordmark + view tabs (right-aligned) + status.
    // Fullwidth wordmark (each letter = 2 cells) reads as a bigger "myx"
    // than the terminal font allows on a single row; bolded for weight.
    let mut header: Vec<Span> =
        gradient_line("\u{FF2D}\u{FF39}\u{FF38}", &[theme.primary, theme.accent])
            .into_iter()
            .map(|mut sp| {
                sp.style = sp.style.add_modifier(Modifier::BOLD);
                sp
            })
            .collect();
    if !app.status.is_empty() {
        header.push(Span::styled(format!("   {}", app.status), theme.muted()));
    }
    f.render_widget(Paragraph::new(Line::from(header)), rows[0]);
    f.render_widget(
        Paragraph::new(Line::from(view_tabs(app, theme))).alignment(Alignment::Right),
        rows[0],
    );
    // Per-tab hit rects for the mouse (mirrors view_tabs: "\u2190\u2192 " prefix + labels joined by " \u00b7 ").
    let mut total: usize = 3; // "\u2190\u2192 "
    for (i, v) in RightView::ALL.iter().enumerate() {
        if i > 0 {
            total += 3;
        } // " \u00b7 "
        total += v.label().chars().count();
    }
    let mut tx_x = rows[0]
        .right()
        .saturating_sub(total as u16)
        .saturating_add(3);
    let mut tabs = Vec::with_capacity(RightView::ALL.len());
    for (i, v) in RightView::ALL.iter().enumerate() {
        if i > 0 {
            tx_x = tx_x.saturating_add(3);
        }
        let w = v.label().chars().count() as u16;
        tabs.push((
            *v,
            Rect {
                x: tx_x,
                y: rows[0].y,
                width: w,
                height: 1,
            },
        ));
        tx_x = tx_x.saturating_add(w);
    }
    out.hits.tabs = tabs;

    let body_area = rows[2];
    if app.view.zen {
        // Hidden, not zero-width: a rendered sidebar still claims mouse rects.
        out.hits.lib = None;
        out.hits.scroll = None;
        if app.view.mode == RightView::NowPlaying && body_area.width >= ZEN_QUEUE_SPLIT_MIN_WIDTH {
            let panes =
                Layout::horizontal([Constraint::Percentage(68), Constraint::Percentage(32)])
                    .spacing(2)
                    .split(body_area);
            render_nowplaying_view(f, app, theme, panes[0], repaint);
            render_queue_view(f, app, theme, panes[1]);
        } else {
            match app.view.mode {
                RightView::NowPlaying => render_nowplaying_view(f, app, theme, body_area, repaint),
                RightView::Lyrics => render_lyrics(f, app, theme, body_area),
                RightView::Queue => render_queue_view(f, app, theme, body_area),
            }
        }
    } else if app.view.mode == RightView::NowPlaying
        && body_area.width >= NOW_PLAYING_SPLIT_MIN_WIDTH
    {
        let panes = Layout::horizontal([
            Constraint::Percentage(24),
            Constraint::Percentage(48),
            Constraint::Percentage(28),
        ])
        .spacing(2)
        .split(body_area);
        render_library(f, app, out, theme, panes[0]);
        render_nowplaying_view(f, app, theme, panes[1], repaint);
        render_queue_view(f, app, theme, panes[2]);
    } else {
        let body = Layout::horizontal([Constraint::Percentage(30), Constraint::Min(24)])
            .spacing(3)
            .split(body_area);
        render_library(f, app, out, theme, body[0]);
        match app.view.mode {
            RightView::NowPlaying => render_nowplaying_view(f, app, theme, body[1], repaint),
            RightView::Lyrics => render_lyrics(f, app, theme, body[1]),
            RightView::Queue => render_queue_view(f, app, theme, body[1]),
        }
    }

    render_now_strip(f, app, out, theme, rows[4]);
    render_footer(f, app, theme, rows[5]);

    if app.view.actions.is_some() {
        render_actions_overlay(f, app, out, theme, area);
    } else {
        out.hits.actions.clear();
    }
}

/// The `Now Playing · Lyrics · Visualizer` indicator, active one lit.
pub(crate) fn view_tabs<'a>(app: &App, theme: Theme) -> Vec<Span<'a>> {
    let mut spans = vec![Span::styled("←→ ", theme.muted())];
    for (i, v) in RightView::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", theme.muted()));
        }
        let style = if *v == app.view.mode {
            Style::default()
                .fg(theme.primary.into())
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        spans.push(Span::styled(v.label(), style));
    }
    spans
}

/// Where the list viewport starts, given where it started last frame.
///
/// Keeps `margin` rows visible either side of the cursor (vim's `scrolloff`).
/// Threading the previous `offset` back in is what makes it sticky: the cursor
/// moves freely inside the window, which follows only when pushed.
pub(crate) fn scroll_offset(
    offset: usize,
    selected: usize,
    cap: usize,
    total: usize,
    margin: usize,
) -> usize {
    if cap == 0 || total <= cap {
        return 0;
    }
    // Wider than half the viewport and the two bounds below would fight.
    let margin = margin.min((cap - 1) / 2);
    offset
        .min(selected.saturating_sub(margin)) // cursor near the top → scroll up
        .max((selected + margin + 1).saturating_sub(cap)) // near the bottom → down
        .min(total - cap)
}

/// Blank `area` and force it out to the terminal, erasing an inline image.
///
/// `ratatui-image` marks the image's cells `Skip` without changing their
/// symbols, and a blank cell compares equal to the blank that was already there
/// — so an ordinary `Clear` writes nothing and the picture survives it.
/// `AlwaysUpdate` is what makes the diff emit them anyway.
pub(crate) fn wipe_area(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_diff_option(CellDiffOption::AlwaysUpdate);
            }
        }
    }
}

/// Keep the terminal's own pixels in `area` by telling the diff to leave those
/// cells alone — what `ratatui-image` does for the image it just drew, without
/// drawing it again.
pub(crate) fn hold_area(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_diff_option(CellDiffOption::Skip);
            }
        }
    }
}

/// Force whatever is already in `area` out to the terminal, so an overlay lands
/// on top of an inline image instead of being skipped over it.
pub(crate) fn force_area(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_diff_option(CellDiffOption::AlwaysUpdate);
            }
        }
    }
}
