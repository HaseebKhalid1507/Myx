//! The left sidebar: sections, search results, drill-in lists.

use super::*;
use crate::*;

pub(crate) fn render_library(
    f: &mut Frame,
    app: &App,
    out: &mut FrameOut,
    theme: Theme,
    area: Rect,
) {
    f.render_widget(Block::default().style(theme.panel()), area);
    let inner = area.inner(Margin::new(2, 1));
    if inner.height < 2 {
        return;
    }

    // Header line: drill-in title, search input/results, or section indicator.
    let head: Line = if let Some(d) = app.browse.details.last() {
        Line::from(vec![
            Span::styled("‹ ", Style::default().fg(theme.primary.into())),
            Span::styled(
                truncate(&d.title, inner.width.saturating_sub(8) as usize),
                theme.heading(),
            ),
            Span::styled("  Esc", theme.muted()),
        ])
    } else if app.search.input_mode {
        let (before, after) = split_at_cursor(app.search.query(), app.search.input.cursor().1);
        Line::from(vec![
            Span::styled("search: ", theme.heading()),
            Span::styled(
                format!("{before}▏{after}"),
                Style::default().fg(theme.text.into()),
            ),
        ])
    } else if app.search.searching {
        Line::from(vec![
            Span::styled("search: ", theme.heading()),
            Span::styled(
                app.search.query().to_string(),
                Style::default().fg(theme.text.into()),
            ),
            Span::styled("  (Esc)", theme.muted()),
        ])
    } else {
        let mut spans = vec![
            Span::styled("‹ ", theme.muted()),
            Span::styled(app.browse.section.label(), theme.heading()),
            Span::styled(" ›", theme.muted()),
            Span::styled(
                format!(
                    "  {}/{} · {}",
                    app.browse.section.index() + 1,
                    Section::ALL.len(),
                    app.cur_items().len()
                ),
                theme.muted(),
            ),
        ];
        if app.browse.sort != SortMode::Added {
            spans.push(Span::styled(
                format!("  ⇅{}", app.browse.sort.label()),
                Style::default().fg(theme.accent.into()),
            ));
        }
        Line::from(spans)
    };
    f.render_widget(
        Paragraph::new(head).block(Block::default().style(theme.panel())),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    let list_top = inner.y + 2;
    if list_top >= inner.bottom() {
        return;
    }
    let cap = (inner.bottom() - list_top) as usize;
    let total_items = app.cur_items().len();

    if total_items == 0 {
        out.hits.scroll = None;
        out.hits.lib = None;
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("(empty)", theme.muted())))
                .block(Block::default().style(theme.panel())),
            Rect {
                x: inner.x,
                y: list_top,
                width: inner.width,
                height: 1,
            },
        );
        return;
    }

    let offset = scroll_offset(
        out.lib_offset,
        app.browse.selected,
        cap,
        total_items,
        myx::config::get().scrolloff,
    );
    out.hits.lib = Some(Rect {
        x: inner.x,
        y: list_top,
        width: inner.width,
        height: cap as u16,
    });
    out.lib_offset = offset;
    let overflow = total_items > cap && inner.width > 2;
    // Reserve an extra gutter column for the scrollbar (+1 char of padding).
    let max = inner.width.saturating_sub(if overflow { 3 } else { 2 }) as usize;

    let items = app.cur_items();
    for (row, item) in items.iter().skip(offset).take(cap).enumerate() {
        let idx = offset + row;
        let y = list_top + row as u16;
        let rect = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        };

        // Header rows: a bold section label (Home feed groups), not selectable.
        if item.is_header {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    item.name.clone(),
                    Style::default()
                        .fg(theme.accent.into())
                        .add_modifier(Modifier::BOLD),
                )))
                .block(Block::default().style(theme.panel())),
                rect,
            );
            continue;
        }

        let selected = idx == app.browse.selected;
        let bg = if selected {
            theme.background_element.into()
        } else {
            theme.background_panel.into()
        };
        let block = left_bar_block(&theme, selected, bg);
        let style = if selected {
            Style::default()
                .fg(theme.text.into())
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        // Mark rows that `P` can play outright (playlist / album / artist), so
        // they're distinguishable from tracks at a glance.
        let playable_ctx = context_target(item).is_some() && !item.is_play;
        let max = if playable_ctx {
            max.saturating_sub(2)
        } else {
            max
        };
        let label = truncate(&item.name, max);
        let mut spans = Vec::new();
        if playable_ctx {
            spans.push(Span::styled(
                " ▶",
                Style::default().fg(theme.border_dimmest.into()),
            ));
        }
        spans.push(Span::styled(format!(" {label}"), style));
        if !item.subtitle.is_empty() {
            let used = label.chars().count() + 1;
            let room = max.saturating_sub(used + 3);
            if room > 3 {
                spans.push(Span::styled(
                    " · ",
                    Style::default().fg(theme.border_dimmest.into()),
                ));
                spans.push(Span::styled(
                    truncate(&item.subtitle, room),
                    theme.muted().add_modifier(Modifier::DIM),
                ));
            }
        }
        f.render_widget(Paragraph::new(Line::from(spans)).block(block), rect);
    }

    // Subtle scrollbar: a hairline 1/8 track with a slightly denser 1/4 thumb,
    // in the right gutter. Shown only on overflow; the track rect is stashed on
    // `app` so mouse drags can scroll it.
    if overflow {
        let total = total_items;
        let sb_x = inner.right();
        let track_h = cap;
        let thumb_h = (cap * cap).div_ceil(total).max(1).min(track_h);
        let travel = track_h - thumb_h;
        let max_off = total - cap;
        let thumb_y0 = (offset * travel + max_off / 2)
            .checked_div(max_off)
            .unwrap_or(0);
        for i in 0..track_h {
            let y = list_top + i as u16;
            if y >= inner.bottom() {
                break;
            }
            let in_thumb = i >= thumb_y0 && i < thumb_y0 + thumb_h;
            let (glyph, color) = if in_thumb {
                ("\u{258E}", theme.text_muted) // 1/4 block - thumb
            } else {
                ("\u{258F}", theme.border_dimmest) // 1/8 block - track
            };
            f.render_widget(
                Paragraph::new(Span::styled(glyph, Style::default().fg(color.into()))),
                Rect {
                    x: sb_x,
                    y,
                    width: 1,
                    height: 1,
                },
            );
        }
        out.hits.scroll = Some(Rect {
            x: sb_x,
            y: list_top,
            width: 1,
            height: track_h as u16,
        });
        out.hits.scroll_len = total;
    } else {
        out.hits.scroll = None;
    }
}

/// Split the query at the editor's cursor column (a char index) so the ▏
/// glyph can be drawn where the cursor actually is. Clamps past-the-end.
pub(crate) fn split_at_cursor(q: &str, col: usize) -> (&str, &str) {
    let byte = q.char_indices().nth(col).map(|(i, _)| i).unwrap_or(q.len());
    q.split_at(byte)
}
