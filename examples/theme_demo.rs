//! Live showcase of the myx design system.
//!
//! Cycles themes with ←/→ (or h/l), quits with q/Esc. Renders a mock now-playing
//! surface using every primitive: 3-layer backgrounds, left-bar focus, pills,
//! gradient badges, a gradient progress bar, and real album art (ratatui-image).

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Frame, Terminal};

use myx::anim::ThemeFade;
use myx::components::{gradient_line, gradient_pill, gradient_progress, left_bar_block, pill};
use myx::cover::Cover;
use myx::reactive::derive_theme;
use myx::theme::{Theme, THEMES};

struct App {
    themes: Vec<Theme>,
    theme_idx: usize,
    /// The theme currently on screen (may be mid-fade).
    displayed: Theme,
    /// Active cross-fade, if any.
    fade: Option<ThemeFade>,
    cover: Option<Cover>,
}

impl App {
    /// Begin a ~300ms cross-fade from whatever's on screen now to `themes[idx]`.
    fn fade_to(&mut self, idx: usize) {
        self.theme_idx = idx;
        let target = self.themes[idx];
        self.fade = Some(ThemeFade::new(
            self.displayed,
            target,
            Duration::from_millis(300),
        ));
    }
}

fn main() -> io::Result<()> {
    let mut terminal = init()?;

    // Build the image picker *after* raw mode so the terminal query round-trips.
    let picker = Cover::make_picker();

    // Load the cover once, derive a reactive theme from the same pixels, then hand
    // the decoded image to the Cover renderer.
    let mut themes: Vec<Theme> = Vec::new();
    let cover = match image::open("assets/cover.jpg") {
        Ok(img) => {
            themes.push(derive_theme(&img, "album ✦"));
            Some(Cover::from_image(img, picker))
        }
        Err(_) => None,
    };
    themes.extend_from_slice(THEMES);

    let mut app = App {
        displayed: themes[0],
        themes,
        theme_idx: 0,
        fade: None,
        cover,
    };

    loop {
        // Advance any running cross-fade before drawing.
        if let Some(fade) = &app.fade {
            app.displayed = fade.current();
            if fade.is_done() {
                app.displayed = app.themes[app.theme_idx];
                app.fade = None;
            }
        }

        terminal.draw(|f| render(f, &mut app))?;

        // Tick fast while fading for smooth motion; idle otherwise.
        let timeout = if app.fade.is_some() {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(120)
        };

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Right | KeyCode::Char('l') => {
                        app.fade_to((app.theme_idx + 1) % app.themes.len());
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        app.fade_to((app.theme_idx + app.themes.len() - 1) % app.themes.len());
                    }
                    _ => {}
                }
            }
        }
    }

    restore(terminal)
}

fn render(f: &mut Frame, app: &mut App) {
    let theme = app.displayed;
    let area = f.area();

    f.render_widget(Block::default().style(theme.base()), area);
    let area = area.inner(Margin::new(2, 1));

    let rows = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1), // spacer
        Constraint::Length(3), // pill row
        Constraint::Length(1), // spacer
        Constraint::Min(10),   // two-pane body
        Constraint::Length(1), // spacer
        Constraint::Length(1), // progress
        Constraint::Length(1), // footer
    ])
    .split(area);

    // ---- Title (gradient heading) ----
    let title: Vec<Span> = gradient_line("myx  —  design system", &[theme.primary, theme.accent]);
    f.render_widget(Paragraph::new(Line::from(title)), rows[0]);

    // ---- Pill row: solid + gradient badges ----
    let mut pills: Vec<Span> = Vec::new();
    pills.extend(pill("EXPLICIT", theme.error, theme.background));
    pills.push(Span::raw("  "));
    pills.extend(pill("PLAYLIST", theme.info, theme.background));
    pills.push(Span::raw("  "));
    pills.extend(pill("♥ LIKED", theme.success, theme.background));
    pills.push(Span::raw("  "));
    pills.extend(gradient_pill(
        "NOW PLAYING",
        &[theme.primary, theme.secondary, theme.accent],
        theme.background,
    ));
    f.render_widget(
        Paragraph::new(Line::from(pills)).block(Block::default().style(theme.base())),
        rows[2].inner(Margin::new(0, 1)),
    );

    // ---- Two-pane body ----
    let body = Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
        .spacing(2)
        .split(rows[4]);

    render_library(f, theme, body[0]);
    render_now_playing(f, app, theme, body[1]);

    // ---- Gradient progress bar ----
    let (played, total) = (84u32, 227u32); // 1:24 / 3:47
    let bar_width = rows[6].width.saturating_sub(14) as usize;
    let filled = ((played as f32 / total as f32) * bar_width as f32) as usize;
    let mut prog: Vec<Span> = vec![Span::styled("1:24 ", theme.muted())];
    prog.extend(gradient_progress(
        bar_width,
        filled,
        &[theme.primary, theme.accent],
        theme.border_dimmest,
    ));
    prog.push(Span::styled(" 3:47", theme.muted()));
    f.render_widget(Paragraph::new(Line::from(prog)), rows[6]);

    // ---- Footer ----
    let footer = Line::from(vec![
        Span::styled("  ⏮  ⏯  ⏭    🔀 🔁    ♥      ", theme.muted()),
        Span::styled(theme.name, theme.heading()),
        Span::styled("      ←/→ cross-fade   q quit", theme.muted()),
    ]);
    f.render_widget(Paragraph::new(footer), rows[7]);
}

fn render_library(f: &mut Frame, theme: Theme, area: Rect) {
    f.render_widget(Block::default().style(theme.panel()), area);
    let inner = area.inner(Margin::new(1, 0));

    let items = [
        ("Liked Songs", true),
        ("Discover Weekly", false),
        ("Daily Mix 1", false),
        ("lofi beats", false),
        ("deep focus", false),
    ];

    let mut y = inner.y;
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("LIBRARY", theme.heading())]))
            .block(Block::default().style(theme.panel())),
        Rect { x: inner.x, y, width: inner.width, height: 1 },
    );
    y += 2;

    for (label, selected) in items {
        if y >= inner.bottom() {
            break;
        }
        let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
        let bg = if selected {
            theme.background_element.into()
        } else {
            theme.background_panel.into()
        };
        let block = left_bar_block(&theme, selected, bg);
        let text_style = if selected {
            Style::default().fg(theme.text.into()).add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {label}"), text_style))).block(block),
            row,
        );
        y += 1;
    }
}

fn render_now_playing(f: &mut Frame, app: &mut App, theme: Theme, area: Rect) {
    f.render_widget(Block::default().style(theme.panel()), area);
    let inner = area.inner(Margin::new(2, 1));

    // Split: album art on the left, metadata on the right.
    // Cover box is sized ~square in *pixels*: cells are ~1:2, so width ≈ 2×height.
    let art_h = inner.height.min(9);
    let art_w = (art_h * 2).min(inner.width.saturating_sub(20));
    let cols = Layout::horizontal([Constraint::Length(art_w), Constraint::Min(10)])
        .spacing(2)
        .split(inner);
    let art_rect = Rect { x: cols[0].x, y: cols[0].y, width: art_w, height: art_h };
    let meta_rect = cols[1];

    // Album art (or a placeholder box if the image failed to load).
    match app.cover.as_mut() {
        Some(cover) => cover.render(f, art_rect),
        None => f.render_widget(
            Paragraph::new("[ no art ]")
                .style(theme.muted())
                .alignment(Alignment::Center)
                .block(Block::default().style(theme.element())),
            art_rect,
        ),
    }

    // Metadata + mock visualizer.
    let lines = vec![
        Line::from(vec![Span::styled("NOW PLAYING", theme.heading())]),
        Line::raw(""),
        Line::from(vec![Span::styled(
            "Midnight City",
            Style::default().fg(theme.text.into()).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::styled("M83", theme.muted())]),
        Line::from(vec![Span::styled("Hurry Up, We're Dreaming · 2011", theme.muted())]),
        Line::raw(""),
        Line::from(gradient_line(
            "▁▂▃▅▇▆▄▂▁▃▅▇▆▄▂▁▂▃▅▇▆▄▃▂▁▂▄▆▇▅▃▁",
            &[theme.info, theme.primary, theme.accent],
        )),
    ];
    f.render_widget(
        Paragraph::new(lines).block(Block::default().style(theme.panel())),
        meta_rect,
    );
}

// ---- terminal plumbing ----

fn init() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore(mut terminal: Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
