//! The render tree.
//!
//! One-way dependency: everything here reads `&App` and writes `FrameOut`;
//! nothing here mutates application state. One module per screen, so the file
//! to open is the one named after the thing on screen.

mod equalizer;
mod footer;
mod library;
mod lyrics;
mod nowplaying;
mod overlay;
mod queue;
mod visualizer;

pub(crate) use equalizer::*;
pub(crate) use footer::*;
pub(crate) use library::*;
pub(crate) use lyrics::*;
pub(crate) use nowplaying::*;
pub(crate) use overlay::*;
pub(crate) use queue::*;
pub(crate) use visualizer::*;

use crate::*;
use unicode_width::UnicodeWidthStr;

pub(crate) fn render(f: &mut Frame, app: &App, out: &mut FrameOut, repaint: ArtRepaint) {
    out.art = None;
    out.title_shown = false;
    let theme = app.theme.displayed;
    let area = f.area();
    f.render_widget(Block::default().style(theme.base()), area);
    let area = area.inner(outer_margin(area));
    let rows = frame_rows(area);

    if let Some(header) = rows.header {
        render_header(f, app, out, theme, header);
    } else {
        out.hits.tabs.clear();
    }

    let right = if app.view.zen || rows.body.height == 0 {
        // Hidden, not zero-width: a rendered sidebar still claims mouse rects.
        out.hits.lib = None;
        out.hits.scroll = None;
        rows.body
    } else {
        let body = Layout::horizontal([Constraint::Percentage(30), Constraint::Min(24)])
            .spacing(3)
            .split(rows.body);
        render_library(f, app, out, theme, body[0]);
        body[1]
    };
    out.title_shown = match app.view.mode {
        RightView::NowPlaying => render_nowplaying_view(f, app, out, theme, right, repaint),
        RightView::Lyrics => render_lyrics(f, app, theme, right),
        RightView::Queue => render_queue_view(f, app, theme, right),
    };

    render_now_strip(f, app, out, theme, rows.strip_top, rows.progress);
    if let Some(footer) = rows.footer {
        render_footer(f, app, theme, footer);
    }

    if app.view.actions.is_some() {
        render_actions_overlay(f, app, theme, area);
    } else if app.view.equalizer.is_some() {
        // Keep album art visible: the editor lives in the active pane and
        // positions itself in free space around the cover.
        render_equalizer_overlay(f, app, out, theme, right);
    }
}

/// The frame's padding: two columns and a row while there's room for it, then
/// less, then none — at a few cells every one of them is worth more as content.
fn outer_margin(area: Rect) -> Margin {
    Margin::new(
        match area.width {
            40.. => 2,
            20..=39 => 1,
            _ => 0,
        },
        u16::from(area.height >= 18),
    )
}

/// Where the parts of the frame go, top to bottom. `None` is a part that
/// doesn't fit and isn't drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameRows {
    pub(crate) header: Option<Rect>,
    pub(crate) body: Rect,
    /// The strip's upper row: the volume meter, and the track when the view
    /// isn't showing it.
    pub(crate) strip_top: Option<Rect>,
    /// Time and the progress bar.
    pub(crate) progress: Option<Rect>,
    pub(crate) footer: Option<Rect>,
}

/// Lay the frame out by hand, most important part last to go.
///
/// Not the constraint solver: with too few rows ratatui keeps a `Min` body and
/// squeezes the `Length` rows around it, so the now-playing strip — the one
/// thing you need on a small screen — disappeared first. Here the progress row
/// is kept longest, then the header, then the view, the strip's upper row, the
/// key hints, and the spacing.
pub(crate) fn frame_rows(area: Rect) -> FrameRows {
    let h = area.height;
    // (header, spacers, strip_top, footer) for this many rows.
    let (header, spacers, strip_top, footer) = match h {
        16.. => (true, true, true, true),
        12..=15 => (true, false, true, true),
        8..=11 => (true, false, true, false),
        5..=7 => (true, false, false, false),
        _ => (false, false, false, false),
    };
    let progress = h >= 1;
    let fixed = u16::from(header)
        + 2 * u16::from(spacers)
        + u16::from(strip_top)
        + u16::from(progress)
        + u16::from(footer);
    let mut y = area.y;
    let mut take = |n: u16| {
        let r = Rect::new(area.x, y, area.width, n);
        y += n;
        r
    };
    let header = header.then(|| take(1));
    if spacers {
        take(1);
    }
    let body = take(h - fixed);
    if spacers {
        take(1);
    }
    let strip_top = strip_top.then(|| take(1));
    let progress = progress.then(|| take(1));
    let footer = footer.then(|| take(1));
    FrameRows {
        header,
        body,
        strip_top,
        progress,
        footer,
    }
}

/// How the view tabs are drawn in the header, richest that fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabsForm {
    /// `←→ Now Playing · Lyrics · Queue`
    Full,
    /// `◀ Now Playing ▶`
    Arrows,
    /// `Now Playing`
    Label,
    None,
}

/// Columns for the wordmark and the gap after it.
const LOGO_W: u16 = 6;
const HEADER_GAP: u16 = 3;

/// Which tabs fit in a header `width` wide, and how many columns that leaves
/// for the status text. The tabs come first: they're how you know which view
/// you're on; the status ("loaded Liked") only gets what's left over, and is
/// left out under a few columns rather than cut to a stub.
pub(crate) fn header_fit(width: u16, full: u16, arrows: u16, label: u16) -> (TabsForm, u16) {
    let room = width.saturating_sub(LOGO_W + HEADER_GAP);
    let (form, tabs) = if full <= room {
        (TabsForm::Full, full)
    } else if arrows <= room {
        (TabsForm::Arrows, arrows)
    } else if label <= room {
        (TabsForm::Label, label)
    } else {
        (TabsForm::None, 0)
    };
    let status = room.saturating_sub(tabs + 2);
    (form, if status >= 6 { status } else { 0 })
}

fn render_header(f: &mut Frame, app: &App, out: &mut FrameOut, theme: Theme, area: Rect) {
    let views = RightView::ALL;
    let label = app.view.mode.label();
    let full: u16 = 3
        + views.iter().map(|v| v.label().width() as u16).sum::<u16>()
        + 3 * (views.len() as u16 - 1);
    let arrows = label.width() as u16 + 4;
    let (form, status_w) = header_fit(area.width, full, arrows, label.width() as u16);

    // Fullwidth wordmark (each letter = 2 cells) reads as a bigger "myx"
    // than the terminal font allows on a single row; bolded for weight.
    if area.width >= LOGO_W {
        let mut left: Vec<Span> =
            gradient_line("\u{FF2D}\u{FF39}\u{FF38}", &[theme.primary, theme.accent])
                .into_iter()
                .map(|mut sp| {
                    sp.style = sp.style.add_modifier(Modifier::BOLD);
                    sp
                })
                .collect();
        if status_w > 0 && !app.status.is_empty() {
            left.push(Span::styled(
                format!("   {}", truncate(&app.status, status_w as usize)),
                theme.muted(),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(left)), area);
    }

    // The tabs, right-aligned, and a click target for each part drawn.
    let lit = Style::default()
        .fg(theme.primary.into())
        .add_modifier(Modifier::BOLD);
    let (spans, hits): (Vec<Span>, Vec<(RightView, u16, u16)>) = match form {
        TabsForm::Full => {
            let mut spans = vec![Span::styled("←→ ", theme.muted())];
            let mut hits = Vec::new();
            let mut x = 3;
            for (i, v) in views.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" · ", theme.muted()));
                    x += 3;
                }
                let style = if *v == app.view.mode {
                    lit
                } else {
                    theme.muted()
                };
                let w = v.label().width() as u16;
                spans.push(Span::styled(v.label(), style));
                hits.push((*v, x, w));
                x += w;
            }
            (spans, hits)
        }
        TabsForm::Arrows => {
            let (prev, next) = (app.view.mode.shift(-1), app.view.mode.shift(1));
            let w = label.width() as u16;
            (
                vec![
                    Span::styled("◀ ", theme.muted()),
                    Span::styled(label, lit),
                    Span::styled(" ▶", theme.muted()),
                ],
                vec![(prev, 0, 1), (next, w + 3, 1)],
            )
        }
        TabsForm::Label => (vec![Span::styled(label, lit)], Vec::new()),
        TabsForm::None => (Vec::new(), Vec::new()),
    };
    let drawn: u16 = spans.iter().map(|s| s.width() as u16).sum();
    let x0 = area.right().saturating_sub(drawn);
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(x0, area.y, drawn.min(area.width), 1),
    );
    out.hits.tabs = hits
        .into_iter()
        .map(|(v, dx, w)| (v, Rect::new(x0 + dx, area.y, w, 1)))
        .collect();
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
///
/// The background already painted under the art (the pane's) is kept: a plain
/// reset falls back to the terminal's default, which with a translucent
/// terminal is a see-through hole where the cover was.
pub(crate) fn wipe_area(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                let bg = cell.bg;
                cell.reset();
                cell.bg = bg;
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

#[cfg(test)]
mod layout_tests {
    use super::*;

    fn parts(r: &FrameRows) -> Vec<Rect> {
        [r.header, Some(r.body), r.strip_top, r.progress, r.footer]
            .into_iter()
            .flatten()
            .filter(|p| p.height > 0)
            .collect()
    }

    #[test]
    fn frame_rows_fit_the_area_without_overlap_at_every_height() {
        for h in 0..=60u16 {
            let area = Rect::new(3, 2, 50, h);
            let r = frame_rows(area);
            let ps = parts(&r);
            for p in &ps {
                assert!(
                    area.contains(p.as_position()) && p.bottom() <= area.bottom(),
                    "h={h} {p:?}"
                );
            }
            for (i, a) in ps.iter().enumerate() {
                for b in &ps[i + 1..] {
                    assert!(!a.intersects(*b), "h={h}: {a:?} overlaps {b:?}");
                }
            }
            let used: u16 = ps.iter().map(|p| p.height).sum();
            assert!(used <= h, "h={h}");
        }
    }

    #[test]
    fn the_progress_row_is_the_last_part_to_go() {
        // Order of disappearance as rows run out: spacing, key hints, the
        // strip's upper row, then the header; progress stays down to one row.
        let at = |h| frame_rows(Rect::new(0, 0, 80, h));
        assert!(at(1).progress.is_some());
        assert!(at(1).header.is_none());
        assert!(at(5).header.is_some() && at(4).header.is_none());
        assert!(at(8).strip_top.is_some() && at(7).strip_top.is_none());
        assert!(at(12).footer.is_some() && at(11).footer.is_none());
        assert_eq!(
            at(0),
            FrameRows {
                header: None,
                body: Rect::new(0, 0, 80, 0),
                strip_top: None,
                progress: None,
                footer: None,
            }
        );
        // Roomy: today's layout, a spacer either side of the body.
        let r = at(40);
        assert_eq!(r.header.unwrap().y, 0);
        assert_eq!(r.body.y, 2);
        assert_eq!(r.footer.unwrap().y, 39);
    }

    #[test]
    fn the_header_never_lets_status_and_tabs_overlap() {
        let (full, arrows, label) = (31, 15, 11);
        let mut last = TabsForm::Full;
        for width in (0..=120u16).rev() {
            let (form, status) = header_fit(width, full, arrows, label);
            let tabs = match form {
                TabsForm::Full => full,
                TabsForm::Arrows => arrows,
                TabsForm::Label => label,
                TabsForm::None => 0,
            };
            let gap = if status > 0 { 2 } else { 0 };
            assert!(
                LOGO_W + HEADER_GAP + status + gap + tabs <= width.max(LOGO_W + HEADER_GAP),
                "width {width}: {form:?} + status {status}"
            );
            // Narrowing only ever steps the tabs down, never back up.
            let rank = |f: TabsForm| f as u8;
            assert!(rank(form) >= rank(last), "width {width}");
            last = form;
        }
        assert_eq!(header_fit(200, full, arrows, label).0, TabsForm::Full);
        assert_eq!(header_fit(30, full, arrows, label).0, TabsForm::Arrows);
        assert_eq!(header_fit(20, full, arrows, label).0, TabsForm::Label);
        assert_eq!(header_fit(10, full, arrows, label).0, TabsForm::None);
    }

    #[test]
    fn hints_fit_whole_most_useful_first() {
        // (rank, width) in display order.
        let hints = [(3, 10), (1, 8), (4, 12), (2, 6)];
        assert_eq!(fit_hints(&hints, 100), [true, true, true, true]);
        // 25 columns: ranks 1, 2, 3 (8 + 6 + 10 = 24); rank 4 doesn't fit.
        assert_eq!(fit_hints(&hints, 25), [true, true, false, true]);
        assert_eq!(fit_hints(&hints, 13), [false, true, false, false]);
        assert_eq!(fit_hints(&hints, 0), [false; 4]);
        for width in 0..=40u16 {
            let keep = fit_hints(&hints, width);
            let used: u16 = hints
                .iter()
                .zip(&keep)
                .filter(|(_, k)| **k)
                .map(|(h, _)| h.1)
                .sum();
            assert!(used <= width, "width {width}");
        }
    }

    #[test]
    fn now_playing_never_overlaps_and_drops_the_spectrum_before_the_cover() {
        for cell in [
            ratatui_image::FontSize::new(10, 22),
            ratatui_image::FontSize::new(8, 16),
            ratatui_image::FontSize::new(1, 2),
        ] {
            for w in 1..=120u16 {
                for h in 0..=60u16 {
                    let area = Rect::new(5, 3, w, h);
                    let np = np_layout(area, cell, 20);
                    let parts: Vec<Rect> = [np.art, Some(np.info), np.visualizer]
                        .into_iter()
                        .flatten()
                        .filter(|r| r.area() > 0)
                        .collect();
                    for p in &parts {
                        assert!(
                            p.x >= area.x
                                && p.y >= area.y
                                && p.right() <= area.right()
                                && p.bottom() <= area.bottom(),
                            "{w}x{h} {cell:?}: {p:?} outside {area:?}"
                        );
                    }
                    for (i, a) in parts.iter().enumerate() {
                        for b in &parts[i + 1..] {
                            assert!(!a.intersects(*b), "{w}x{h} {cell:?}: {a:?} / {b:?}");
                        }
                    }
                    // The title row is there as long as there's a row.
                    assert_eq!(np.info.height, h.min(3), "{w}x{h}");
                    // The spectrum only alongside a cover at least 6 rows tall.
                    if np.visualizer.is_some() {
                        let art = np.art.expect("spectrum without a cover");
                        assert!(art.height >= 6, "{w}x{h}: {art:?}");
                    }
                    // A cover is either a real one (3+ rows) or none.
                    if let Some(art) = np.art {
                        assert!(art.height >= 3, "{w}x{h}: {art:?}");
                    }
                }
            }
        }
    }
}
