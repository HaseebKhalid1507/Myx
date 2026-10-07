//! The one-line keybinding hint footer.

use crate::*;

pub(crate) fn render_footer(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let on = |b: bool| if b { theme.success } else { theme.text_muted };
    let key = |k: &'static str| Span::styled(k, Style::default().fg(theme.primary.into()));
    let lbl = |t: &'static str| Span::styled(t, theme.muted());
    let enter_lbl = enter_label(app.selected_item());
    let play_lbl = if app.playback.now.as_ref().is_some_and(|n| n.is_playing) {
        " pause   "
    } else {
        " play    "
    };
    // Flagged hints go with the library pane, which zen hides — a key that does
    // nothing must not be advertised. The number is how much a hint matters:
    // when the row is too narrow for all of them, the least useful go first,
    // whole, and the rest keep their places.
    let hints = [
        (true, 6, key("⇥"), lbl(" section   ")),
        (false, 7, key("←→"), lbl(" view   ")),
        (true, 3, key("/"), lbl(" search   ")),
        (true, 4, key("f"), lbl(" find   ")),
        (
            true,
            1,
            key("⏎"),
            Span::styled(format!(" {enter_lbl}   "), theme.muted()),
        ),
        (true, 14, key("⇧⏎"), lbl(" play   ")),
        (true, 13, key("S"), lbl(" shuffle   ")),
        (false, 2, key("␣"), Span::styled(play_lbl, theme.muted())),
        (false, 5, key("n/b"), lbl(" skip   ")),
        (false, 11, key("⇧←→"), lbl(" seek   ")),
        (true, 15, key("o"), lbl(" sort   ")),
        (false, 10, key("+/-"), lbl(" vol   ")),
        (
            false,
            12,
            Span::styled("s", Style::default().fg(on(app.transport.shuffle).into())),
            lbl(" shuffle   "),
        ),
        (false, 9, key("a"), lbl(" actions   ")),
        (false, 16, key("e"), lbl(" eq   ")),
        (
            false,
            17,
            Span::styled("z", Style::default().fg(on(app.view.zen).into())),
            lbl(" zen   "),
        ),
        (false, 8, key("q"), lbl(" quit")),
    ];
    let hints: Vec<_> = hints
        .into_iter()
        .filter(|(needs_library, ..)| !needs_library || !app.view.zen)
        .collect();
    let sizes: Vec<(u8, u16)> = hints
        .iter()
        .map(|(_, rank, k, l)| (*rank, (k.width() + l.width()) as u16))
        .collect();
    let keep = fit_hints(&sizes, area.width);
    let mut spans: Vec<Span> = hints
        .into_iter()
        .zip(keep)
        .filter(|(_, keep)| *keep)
        .flat_map(|((_, _, k, l), _)| [k, l])
        .collect();
    // The last hint's gap would push the centred row off-centre.
    if let Some(last) = spans.last_mut() {
        last.content = last.content.trim_end().to_string().into();
    }
    let line = Line::from(spans);
    f.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
}

/// Which hints fit a row `width` wide: the most useful first (lowest rank),
/// each whole or not at all. `hints` is `(rank, width)` in display order; the
/// result says, in that order, which to draw.
pub(crate) fn fit_hints(hints: &[(u8, u16)], width: u16) -> Vec<bool> {
    let mut order: Vec<usize> = (0..hints.len()).collect();
    order.sort_by_key(|&i| hints[i].0);
    let mut keep = vec![false; hints.len()];
    let mut used: u16 = 0;
    for i in order {
        let w = hints[i].1;
        if used + w <= width {
            used += w;
            keep[i] = true;
        }
    }
    keep
}
