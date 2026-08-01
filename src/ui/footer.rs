//! The one-line keybinding hint footer.

use crate::*;

pub(crate) fn render_footer(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let on = |b: bool| if b { theme.success } else { theme.text_muted };
    let key = |k: &'static str| Span::styled(k, Style::default().fg(theme.primary.into()));
    let lbl = |t: &'static str| Span::styled(t, theme.muted());
    let enter_lbl = enter_label(app.cur_items().get(app.browse.selected));
    let play_lbl = if app.playback.now.as_ref().is_some_and(|n| n.is_playing) {
        " pause   "
    } else {
        " play    "
    };
    // Flagged hints go with the library pane, which zen hides — a key that does
    // nothing must not be advertised.
    let hints = [
        (true, key("⇥"), lbl(" section   ")),
        (false, key("←→"), lbl(" view   ")),
        (true, key("/"), lbl(" search   ")),
        (
            true,
            key("⏎"),
            Span::styled(format!(" {enter_lbl}   "), theme.muted()),
        ),
        (true, key("⇧⏎"), lbl(" play   ")),
        (true, key("S"), lbl(" shuffle   ")),
        (false, key("␣"), Span::styled(play_lbl, theme.muted())),
        (false, key("n/b"), lbl(" skip   ")),
        (false, key("⇧←→"), lbl(" seek   ")),
        (true, key("o"), lbl(" sort   ")),
        (false, key("+/-"), lbl(" vol   ")),
        (
            false,
            Span::styled("s", Style::default().fg(on(app.transport.shuffle).into())),
            lbl(" shuffle   "),
        ),
        (false, key("a"), lbl(" actions   ")),
        (
            false,
            Span::styled("z", Style::default().fg(on(app.view.zen).into())),
            lbl(" zen   "),
        ),
        (false, key("q"), lbl(" quit")),
    ];
    let line = Line::from(
        hints
            .into_iter()
            .filter(|(needs_library, ..)| !needs_library || !app.view.zen)
            .flat_map(|(_, k, label)| [k, label])
            .collect::<Vec<_>>(),
    );
    f.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
}
