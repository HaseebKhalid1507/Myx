//! Things drawn on top of everything else: the actions and devices menus, and
//! the startup loading screen.

use super::*;
use crate::*;
use unicode_width::UnicodeWidthStr;

/// Context actions menu — a centered overlay list.
pub(crate) fn render_actions_overlay(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let Some(menu) = &app.view.actions else {
        return;
    };
    // Never bigger than the screen: a clamp to `height - 2` panicked (min >
    // max) on a terminal under 8 rows.
    let w = (area.width / 2).clamp(28, 52).min(area.width);
    let h = (menu.items.len() as u16 + 4)
        .max(6)
        .min(area.height.saturating_sub(2).max(1))
        .min(area.height);
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

/// The devices menu (`d`): every device on the account, ▶ on the one that's
/// playing, greyed where it can't be driven from here.
pub(crate) fn render_devices_overlay(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let Some(menu) = &app.devices.menu else {
        return;
    };
    let rows = app.device_rows();
    let cursor = app.device_cursor(&rows);
    let w = (area.width / 5 * 3).clamp(36, 64).min(area.width);
    let inner_w = w.saturating_sub(4) as usize;
    // Below the list: what went wrong asking, or — with nothing but this myx
    // to show — where the others went; then the keys. Wrapped here, so the
    // box can be sized to them.
    let mut notes: Vec<(String, Style)> = Vec::new();
    if let Some(e) = &menu.error {
        let error = Style::default().fg(theme.error.into());
        notes.push((format!("couldn't list devices: {e}"), error));
    }
    if rows.len() < 2 {
        let note = if app.devices.listed.is_none() && menu.error.is_none() {
            "looking for your other devices…"
        } else {
            "no other devices — open Spotify on your phone; one that's asleep drops off the list"
        };
        notes.push((note.to_string(), theme.muted()));
    }
    notes.push((
        "⏎ play there · ↑↓ move · esc close".to_string(),
        theme.muted(),
    ));
    let notes: Vec<Line> = notes
        .iter()
        .flat_map(|(text, style)| {
            wrap_balanced(text, inner_w, 3)
                .into_iter()
                .map(|row| Line::from(Span::styled(row, *style)))
        })
        .collect();
    let note_rows = notes.len() as u16;
    let h = (rows.len() as u16 + note_rows + 5)
        .min(area.height.saturating_sub(2).max(1))
        .min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(Block::default().style(theme.element()), rect);
    let inner = rect.inner(Margin::new(2, 1));
    let max = inner.width as usize;

    let title = match &app.devices.remote {
        Some(remote) => format!("Devices · myx is the remote for {}", remote.name),
        None => "Devices".to_string(),
    };
    let mut lines = vec![
        Line::from(Span::styled(truncate(&title, max), theme.heading())),
        Line::raw(""),
    ];
    // The list before the notes when the box is squeezed: title, a gap, the
    // devices — the cursor's row kept in view — and a gap.
    let list_h = inner.height.min(rows.len() as u16 + 3);
    let room = usize::from(list_h.saturating_sub(3)).max(1);
    let skip = (cursor + 1).saturating_sub(room);
    for (i, d) in rows.iter().enumerate().skip(skip).take(room) {
        lines.push(device_line(d, i == cursor, theme, max));
    }
    lines.push(Line::raw(""));
    f.render_widget(
        Paragraph::new(lines),
        Rect {
            height: list_h,
            ..inner
        },
    );
    f.render_widget(
        Paragraph::new(notes),
        Rect {
            y: inner.y + list_h,
            height: inner.height - list_h,
            ..inner
        },
    );
    force_area(f, rect);
}

/// One device row: `› ▶ Pixel 7 ··· phone · 40%`, the name cut to fit.
fn device_line(d: &Device, selected: bool, theme: Theme, max: usize) -> Line<'static> {
    let cursor = if selected { "› " } else { "  " };
    let playing = if d.is_active { "▶ " } else { "  " };
    let what = if d.is_this {
        "this device"
    } else if !d.controllable {
        "can't control"
    } else {
        d.kind.label()
    };
    let right = match d.volume {
        Some(v) => format!("{what} · {v:>3}%"),
        None => what.to_string(),
    };
    let room = max.saturating_sub(4 + right.width() + 2);
    let name = truncate(device_label(&d.name), room);
    let gap = max.saturating_sub(4 + name.width() + right.width());
    let style = if !d.controllable {
        theme.muted().add_modifier(Modifier::DIM)
    } else if selected {
        Style::default()
            .fg(theme.text.into())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text.into())
    };
    Line::from(vec![
        Span::styled(cursor, Style::default().fg(theme.primary.into())),
        Span::styled(playing, Style::default().fg(theme.success.into())),
        Span::styled(name, style),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, theme.muted()),
    ])
}

pub(crate) const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// The startup screen: wordmark, spinner, and what we're waiting on.
pub(crate) fn render_loading(f: &mut Frame, label: &str, frame: usize) {
    let theme = startup_theme();
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
