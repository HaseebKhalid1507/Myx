//! Things drawn on top of everything else: the actions menu and the
//! startup loading screen.

use super::*;
use crate::*;

/// Context actions menu — centered for the keyboard, compact at the pointer for
/// a mouse-opened menu.
pub(crate) fn render_actions_overlay(
    f: &mut Frame,
    app: &App,
    out: &mut FrameOut,
    theme: Theme,
    area: Rect,
) {
    let Some(menu) = &app.view.actions else {
        return;
    };
    if let Some(anchor) = app.view.action_anchor {
        let rect = context_popup_rect(menu, anchor, area);
        let popup_bg = gradient::lerp_color(theme.background_element, theme.primary, 0.16);
        f.render_widget(Clear, rect);
        f.render_widget(
            Block::default().style(Style::default().bg(popup_bg.into()).fg(theme.text.into())),
            rect,
        );
        let inner = rect.inner(Margin::new(1, 0));
        out.hits.actions = menu
            .items
            .iter()
            .enumerate()
            .map(|(i, _)| Rect {
                x: inner.x,
                y: inner.y.saturating_add(i as u16),
                width: inner.width,
                height: 1,
            })
            .collect();
        let lines = menu.items.iter().enumerate().map(|(i, item)| {
            let style = if i == menu.selected {
                Style::default()
                    .fg(theme.text.into())
                    .add_modifier(Modifier::BOLD)
            } else {
                theme.muted()
            };
            Line::from(Span::styled(
                truncate(&item.label, inner.width as usize),
                style,
            ))
        });
        f.render_widget(Paragraph::new(lines.collect::<Vec<_>>()), inner);
        force_area(f, rect);
        return;
    }

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
    out.hits.actions = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, _)| Rect {
            x: inner.x,
            y: inner.y.saturating_add(2 + i as u16),
            width: inner.width,
            height: 1,
        })
        .filter(|r| r.y < inner.bottom())
        .collect();
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

pub(crate) fn context_popup_rect(menu: &ActionMenu, anchor: (u16, u16), area: Rect) -> Rect {
    let content_width = menu
        .items
        .iter()
        .map(|item| item.label.chars().count() as u16)
        .max()
        .unwrap_or(0);
    let width = content_width.saturating_add(2).max(8).min(area.width);
    let height = (menu.items.len() as u16).max(1).min(area.height);
    Rect {
        x: anchor.0.min(area.right().saturating_sub(width)).max(area.x),
        y: anchor
            .1
            .min(area.bottom().saturating_sub(height))
            .max(area.y),
        width,
        height,
    }
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
