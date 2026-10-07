//! The Now Playing view and the persistent bottom strip (volume, progress).

use super::*;
use crate::*;
use unicode_width::UnicodeWidthStr;

/// Where Now Playing's parts go in a view `area` for cells of `cell` pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NpLayout {
    /// The cover, square in pixels. `None` when there's no room for one.
    pub(crate) art: Option<Rect>,
    /// Title, artist, album — as many rows of those as fit (0 to 3).
    pub(crate) info: Rect,
    /// The spectrum. `None` when it would cost the cover its size.
    pub(crate) visualizer: Option<Rect>,
    /// The info sits beside the cover (left-aligned) rather than under it.
    pub(crate) beside: bool,
}

impl NpLayout {
    /// The row under the cover-and-info group.
    pub(crate) fn group_bottom(&self) -> u16 {
        self.art
            .map_or(self.info.bottom(), |a| a.bottom().max(self.info.bottom()))
    }
}

/// Rows above the cover while there are rows to spare.
const NP_INSET: u16 = 3;
const NP_INFO: u16 = 3;
const NP_ART_MAX: u16 = 14;
const NP_ART_MIN: u16 = 3;
/// The spectrum is kept only while the cover can still be this tall.
const NP_ART_MIN_WITH_VIZ: u16 = 6;
const NP_VIZ: u16 = 7;
/// Rows under the spectrum, lifting it off the strip.
const NP_VIZ_LIFT: u16 = 2;
/// Beside the cover: the gap to the info, and the least room the info needs.
const NP_SIDE_GAP: u16 = 2;
const NP_SIDE_TEXT_MIN: u16 = 14;

/// Lay out Now Playing in a view `area`, for cells of `cell` pixels and info
/// lines up to `text_w` columns wide (0 if unknown).
///
/// The cover goes above the info or beside it, whichever lets it be bigger: a
/// tall or roomy pane keeps the familiar stack, a short wide one becomes a
/// card. As rows run out the spectrum goes first, before the cover shrinks
/// below [`NP_ART_MIN_WITH_VIZ`]; then the inset above the cover; then the
/// cover itself, down to none; the title is the last row standing. Every part
/// lies inside `area` and none overlaps another. The equalizer overlay places
/// itself with this too, so the two can't disagree.
pub(crate) fn np_layout(area: Rect, cell: ratatui_image::FontSize, text_w: u16) -> NpLayout {
    let h = area.height;
    let viz_cost = NP_VIZ + NP_VIZ_LIFT;
    // With the spectrum if the cover beside it can still be NP_ART_MIN_WITH_VIZ
    // rows — which a narrow pane can deny it however tall it is — else without.
    if h >= NP_ART_MIN_WITH_VIZ + 1 + NP_INFO + viz_cost {
        let with = np_group(area, h - viz_cost, cell, text_w);
        if with.art.is_some_and(|a| a.height >= NP_ART_MIN_WITH_VIZ) {
            return NpLayout {
                visualizer: Some(Rect::new(area.x, area.y + h - viz_cost, area.width, NP_VIZ)),
                ..with
            };
        }
    }
    np_group(area, h, cell, text_w)
}

fn art_rows(layout: &NpLayout) -> u16 {
    layout.art.map_or(0, |a| a.height)
}

/// The cover and info in the top `top_h` rows: stacked or side by side,
/// whichever shows the bigger cover (stacked on a tie).
fn np_group(area: Rect, top_h: u16, cell: ratatui_image::FontSize, text_w: u16) -> NpLayout {
    let stacked = np_stacked(area, top_h, cell);
    match np_beside(area, top_h, cell, text_w) {
        Some(beside) if art_rows(&beside) > art_rows(&stacked) => beside,
        _ => stacked,
    }
}

/// Cover above, info centred under it, the group centred in the rows.
fn np_stacked(area: Rect, top_h: u16, cell: ratatui_image::FontSize) -> NpLayout {
    let (fw, fh) = (u32::from(cell.width.max(1)), u32::from(cell.height.max(1)));
    let info_h = top_h.min(NP_INFO);

    // The cover takes what's left over the info (and a row between them), up
    // to its cap, but only as a whole cover: a sliver of one helps no one.
    let room = top_h.saturating_sub(info_h + 1);
    let mut art_h = room.min(NP_ART_MAX);
    let mut art_w = (u32::from(art_h) * fh / fw) as u16;
    if art_w > area.width {
        art_w = area.width;
        art_h = (u32::from(art_w) * fw / fh) as u16;
    }
    let art = (art_h >= NP_ART_MIN).then_some((art_w, art_h));

    let group_h = info_h + art.map_or(0, |(_, ah)| ah + 1);
    // Push a cover down a little from the top, while rows allow; info alone is
    // simply centred.
    let inset = if art.is_some() {
        top_h.saturating_sub(group_h).min(NP_INSET)
    } else {
        0
    };
    let free = top_h - group_h - inset;
    let group_y = area.y + inset + free / 2;

    let art = art.map(|(aw, ah)| Rect::new(area.x + (area.width - aw) / 2, group_y, aw, ah));
    let info_y = art.map_or(group_y, |r| r.bottom() + 1);
    NpLayout {
        art,
        info: Rect::new(area.x, info_y, area.width, info_h),
        visualizer: None,
        beside: false,
    }
}

/// Cover on the left, info beside it, the pair centred: a mini player's card.
/// `None` when no whole cover fits next to readable info.
fn np_beside(
    area: Rect,
    top_h: u16,
    cell: ratatui_image::FontSize,
    text_w: u16,
) -> Option<NpLayout> {
    let (fw, fh) = (u32::from(cell.width.max(1)), u32::from(cell.height.max(1)));
    let mut art_h = top_h.min(NP_ART_MAX);
    let mut art_w = (u32::from(art_h) * fh / fw) as u16;
    let max_w = area.width.saturating_sub(NP_SIDE_GAP + NP_SIDE_TEXT_MIN);
    if art_w > max_w {
        art_w = max_w;
        art_h = (u32::from(art_w) * fw / fh) as u16;
    }
    if art_h < NP_ART_MIN || art_w == 0 {
        return None;
    }
    let room = area.width - art_w - NP_SIDE_GAP;
    let want = if text_w == 0 {
        NP_SIDE_TEXT_MIN
    } else {
        text_w
    };
    let info_w = want.min(room);
    let info_h = NP_INFO.min(art_h);
    let x0 = area.x + (area.width - (art_w + NP_SIDE_GAP + info_w)) / 2;
    let y0 = area.y + (top_h - art_h) / 2;
    Some(NpLayout {
        art: Some(Rect::new(x0, y0, art_w, art_h)),
        info: Rect::new(
            x0 + art_w + NP_SIDE_GAP,
            y0 + (art_h - info_h) / 2,
            info_w,
            info_h,
        ),
        visualizer: None,
        beside: true,
    })
}

/// View ①: album art with track details directly beneath — centered as a
/// group. Returns whether the title was drawn.
pub(crate) fn render_nowplaying_view(
    f: &mut Frame,
    app: &App,
    out: &mut FrameOut,
    theme: Theme,
    area: Rect,
    repaint: ArtRepaint,
) -> bool {
    let Some(n) = app.playback.now.as_ref() else {
        if area.height > 0 {
            f.render_widget(
                Paragraph::new("Nothing playing.\nBrowse ← and press Enter.")
                    .style(theme.muted())
                    .alignment(Alignment::Center),
                center_v(area, 2),
            );
        }
        return false;
    };
    let text_w = [&n.title, &n.artist, &n.album]
        .iter()
        .map(|t| t.width() as u16)
        .max()
        .unwrap_or(0);
    let layout = np_layout(area, app.svc.cell, text_w);

    if let Some(art_rect) = layout.art {
        out.art = Some(art_rect);
        match n.cover.as_ref() {
            _ if repaint == ArtRepaint::Wipe => wipe_area(f, art_rect),
            // A resize hasn't settled, so the cell size — and with it the sharp
            // cover's size — isn't known yet. The half-block one is made of cells
            // and can't be wrong; it gives way to the sharp one once measured.
            Some(cover) if app.view.cell_settling => {
                cover.render_preview(f, art_rect, app.svc.cell)
            }
            // Writing the escape means transmitting the image, so only do it when
            // something actually asked for it. A theme fade repaints every glyph on
            // screen dozens of times, and re-sending the cover on each of those is
            // what made it flicker.
            Some(cover)
                if repaint == ArtRepaint::Draw || cover.needs_send(art_rect, app.svc.cell) =>
            {
                cover.render(f, art_rect, app.svc.cell)
            }
            // Already on screen: hold the cells so nothing overwrites the picture,
            // and send nothing.
            Some(_) => hold_area(f, art_rect),
            None => wipe_area(f, art_rect),
        }
    }

    let width = layout.info.width as usize;
    let lines = [
        Line::from(Span::styled(
            truncate(&n.title, width),
            Style::default()
                .fg(theme.text.into())
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            truncate(&n.artist, width),
            Style::default().fg(theme.primary.into()),
        )),
        Line::from(Span::styled(truncate(&n.album, width), theme.muted())),
    ];
    let shown = layout.info.height as usize;
    f.render_widget(
        Paragraph::new(lines.into_iter().take(shown).collect::<Vec<_>>()).alignment(
            if layout.beside {
                Alignment::Left
            } else {
                Alignment::Center
            },
        ),
        layout.info,
    );

    if let Some(viz) = layout.visualizer {
        render_visualizer(f, app, theme, viz);
    }
    shown > 0
}

/// Slim persistent bottom strip. Upper row: the volume meter, and the playing
/// track whenever the view above isn't showing it (`out.title_shown`). Lower
/// row: time and the progress bar. Either row may be `None` on a short screen;
/// the progress row is the last to go.
pub(crate) fn render_now_strip(
    f: &mut Frame,
    app: &App,
    out: &mut FrameOut,
    theme: Theme,
    top: Option<Rect>,
    progress: Option<Rect>,
) {
    out.hits.vol = None;
    if let Some(top) = top {
        let track = app.playback.now.as_ref().filter(|_| !out.title_shown);
        // The meter is 13 cells; the track, when shown, keeps at least 12.
        let meter = if track.is_some() { 13 + 2 + 12 } else { 13 };
        if top.width >= meter {
            render_volume(f, app, out, theme, top);
        }
        if let Some(n) = track {
            let room = if out.hits.vol.is_some() {
                top.width.saturating_sub(13 + 2)
            } else {
                top.width
            };
            let state = if n.is_playing { "▶ " } else { "❚❚ " };
            let text = format!("{state}{} · {}", n.title, n.artist);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    truncate(&text, room as usize),
                    Style::default().fg(theme.text.into()),
                ))),
                Rect::new(top.x, top.y, room, 1),
            );
            out.title_shown = true;
        }
    }

    out.hits.bar = None;
    let Some(row) = progress else {
        return;
    };
    // Nothing above showed the track (a short screen): this row carries it,
    // before the time, in up to half the width.
    let track = app
        .playback
        .now
        .as_ref()
        .filter(|_| !out.title_shown)
        .map(|n| {
            let state = if n.is_playing { "▶ " } else { "❚❚ " };
            let room = (row.width / 2).max(12).min(row.width) as usize;
            format!(
                "{}  ",
                truncate(
                    &format!("{state}{} · {}", n.title, n.artist),
                    room.saturating_sub(2)
                )
            )
        });
    if track.is_some() {
        out.title_shown = true;
    }
    let prefix_len = track.as_deref().map_or(0, |t| t.width() as u16);
    // Seek/progress bar. Record bar geometry for click-to-seek.
    let pos = app.playback.position_ms();
    let left_len = prefix_len + format!("{} ", fmt_ms(pos)).chars().count() as u16;
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
    let bar_w = row.width.saturating_sub(left_len + right_len);
    if bar_w > 0 {
        out.hits.bar = Some(Rect {
            x: row.x + left_len,
            y: row.y,
            width: bar_w,
            height: 1,
        });
    }
    render_progress(f, app, theme, row, track);
}

pub(crate) fn render_progress(
    f: &mut Frame,
    app: &App,
    theme: Theme,
    area: Rect,
    track: Option<String>,
) {
    let (pos, dur) = match &app.playback.now {
        Some(n) => (app.playback.position_ms(), n.duration_ms.max(1)),
        None => (0, 1),
    };
    // Compute the bar width from the exact label lengths so the duration sits
    // flush against the right edge (aligned with the volume meter above it).
    let left = format!("{} ", fmt_ms(pos));
    let right = format!(" {}", fmt_ms(dur));
    let track_w = track.as_deref().map_or(0, |t| t.width());
    let reserve = track_w + left.chars().count() + right.chars().count();
    let bar_w = (area.width as usize).saturating_sub(reserve);
    let filled = ((pos as f32 / dur as f32) * bar_w as f32) as usize;

    let mut spans = Vec::new();
    if let Some(t) = track {
        spans.push(Span::styled(t, Style::default().fg(theme.text.into())));
    }
    spans.push(Span::styled(left, theme.muted()));
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

#[cfg(test)]
mod np_shape_tests {
    use super::*;

    const ZOOMED: ratatui_image::FontSize = ratatui_image::FontSize::new(14, 32);
    const NORMAL: ratatui_image::FontSize = ratatui_image::FontSize::new(10, 22);

    #[test]
    fn a_short_wide_pane_puts_the_cover_beside_the_info() {
        // Haseeb's zoomed-in widget: 30 columns, 6 rows for the view. Stacked,
        // a cover can't fit over three lines of info at all.
        let area = Rect::new(1, 1, 30, 6);
        let np = np_layout(area, ZOOMED, 9);
        assert!(np.beside);
        let art = np.art.expect("a cover");
        assert_eq!(art.height, 6);
        assert_eq!(np.info.x, art.right() + 2, "info right of the cover");
        assert_eq!(np.info.height, 3);
        assert!(
            np.info.y > art.y && np.info.bottom() < art.bottom(),
            "centred on it"
        );
    }

    #[test]
    fn a_tall_or_roomy_pane_keeps_the_stack() {
        for (area, cell) in [
            (Rect::new(0, 0, 30, 40), NORMAL),
            (Rect::new(0, 0, 120, 40), NORMAL),
            (Rect::new(0, 0, 80, 30), ZOOMED),
        ] {
            let np = np_layout(area, cell, 20);
            assert!(!np.beside, "{area:?}");
            assert!(np.art.is_some(), "{area:?}");
        }
    }

    #[test]
    fn the_arrangement_shown_has_the_bigger_cover() {
        for cell in [NORMAL, ZOOMED] {
            for w in 1..=120u16 {
                for h in 0..=40u16 {
                    let area = Rect::new(0, 0, w, h);
                    let stacked = art_rows(&np_stacked(area, h, cell));
                    let beside = np_beside(area, h, cell, 20).map_or(0, |l| art_rows(&l));
                    let chosen = art_rows(&np_group(area, h, cell, 20));
                    assert_eq!(chosen, stacked.max(beside), "{w}x{h} {cell:?}");
                }
            }
        }
    }

    #[test]
    fn info_alone_is_centred_not_pushed_down() {
        // Too short for any cover: the three lines sit in the middle, not
        // under an inset meant for a cover.
        // 10x5: no cover fits above three lines, nor beside them. The old
        // inset put the lines on rows 2-4, flush against the bottom.
        let area = Rect::new(0, 0, 10, 5);
        let np = np_layout(area, NORMAL, 9);
        assert!(np.art.is_none());
        assert_eq!(np.info.y, 1);
    }
}
