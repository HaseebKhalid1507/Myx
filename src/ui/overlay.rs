//! Things drawn on top of everything else: the actions menu and the
//! startup loading screen.

use super::*;
use crate::*;

/// Context actions menu — a centered overlay list.
pub(crate) fn render_actions_overlay(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let Some(menu) = &app.view.actions else {
        return;
    };
    let w = (area.width * 5 / 10).clamp(28, 52);
    let h = (menu.items.len() as u16 + 4).clamp(6, area.height.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    f.render_widget(Clear, rect);
    f.render_widget(Block::default().style(theme.element()), rect);
    let inner = rect.inner(Margin::new(2, 1));
    let max = inner.width as usize;
    let mut lines = vec![
        Line::from(Span::styled(truncate(&menu.title, max), theme.heading())),
        Line::raw(""),
    ];
    for (i, it) in menu
        .items
        .iter()
        .take(inner.height.saturating_sub(2) as usize)
        .enumerate()
    {
        if i == menu.selected {
            lines.push(Line::from(vec![
                Span::styled("› ", Style::default().fg(theme.primary.into())),
                Span::styled(
                    truncate(&it.label, max.saturating_sub(2)),
                    Style::default()
                        .fg(theme.text.into())
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                format!("  {}", truncate(&it.label, max.saturating_sub(2))),
                theme.muted(),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
    force_area(f, rect);
}

pub(crate) const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// The startup screen: wordmark, spinner, and what we're waiting on.
pub(crate) fn render_loading(f: &mut Frame, label: &str, frame: usize) {
    let theme = TOKYONIGHT;
    let area = f.area();
    f.render_widget(Block::default().style(theme.panel()), area);

    let top = area.y + area.height.saturating_sub(3) / 2;
    let row = |dy: u16| Rect {
        x: area.x,
        y: top.saturating_add(dy).min(area.bottom().saturating_sub(1)),
        width: area.width,
        height: 1,
    };

    let mark: Vec<Span> = gradient_line("\u{FF2D}\u{FF39}\u{FF38}", &[theme.primary, theme.accent])
        .into_iter()
        .map(|mut sp| {
            sp.style = sp.style.add_modifier(Modifier::BOLD);
            sp
        })
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(mark)).alignment(Alignment::Center),
        row(0),
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(SPINNER[frame % SPINNER.len()], theme.heading()),
            Span::styled(format!("  {label}…"), theme.muted()),
        ]))
        .alignment(Alignment::Center),
        row(2),
    );
}
