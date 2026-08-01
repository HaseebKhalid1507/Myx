//! The Room view — host or join a listening room.
//!
//! Hosting shares the account's Premium session with guests; joining lets a
//! (possibly free) account play any track through the host's session.

use crate::*;
use myx::room::HostInfo;

pub(crate) fn render_room(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let inner = area.inner(Margin::new(2, 1));
    if inner.height == 0 {
        return;
    }
    let max = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    match app.room.mode {
        RoomMode::Idle if app.guest_only() => {
            lines.push(Line::from(Span::styled("GUEST MODE", theme.heading())));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "No streaming engine — this Myx plays only through a room.",
                theme.muted(),
            )));
            lines.push(Line::from(Span::styled(
                "Ask the host for their host:port and room token, then press J.",
                theme.muted(),
            )));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "Browsing works as usual; hosting needs a Premium session.",
                theme.muted(),
            )));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "J  join a room    L  leave",
                theme.muted(),
            )));
        }
        RoomMode::Idle => {
            lines.push(Line::from(Span::styled("LISTENING ROOM", theme.heading())));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "One Premium session, many listeners — each plays their own song.",
                theme.muted(),
            )));
            lines.push(Line::from(Span::styled(
                "Host (h): friends join your room and play through YOUR premium session.",
                theme.muted(),
            )));
            lines.push(Line::from(Span::styled(
                "Join (J): you become a guest — play any track as if you had premium.",
                theme.muted(),
            )));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "h  host this room    J  join a room    L  leave",
                theme.muted(),
            )));
        }
        RoomMode::Host => {
            lines.push(Line::from(Span::styled(
                "HOSTING",
                Style::default().fg(theme.success.into()),
            )));
            lines.push(Line::raw(""));
            let info = app
                .svc
                .room
                .host_info()
                .unwrap_or_else(|| HostInfo {
                    port: 0,
                    token: String::new(),
                    resolves: 0,
                    joins: 0,
                    cache_hits: 0,
                    coalesced: 0,
                    upstream: 0,
                });
            lines.push(Line::from(vec![
                Span::styled("port    ", theme.muted()),
                Span::styled(
                    format!("{}", info.port),
                    Style::default().fg(theme.text.into()),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("token   ", theme.muted()),
                Span::styled(
                    info.token.clone(),
                    Style::default().fg(theme.primary.into()),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("joined  ", theme.muted()),
                Span::styled(
                    format!("{}", info.joins),
                    Style::default().fg(theme.text.into()),
                ),
                Span::styled("   resolved  ", theme.muted()),
                Span::styled(
                    format!("{}", info.resolves),
                    Style::default().fg(theme.text.into()),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("upstream", theme.muted()),
                Span::styled(
                    format!("  {}", info.upstream),
                    Style::default().fg(theme.text.into()),
                ),
                Span::styled("   cached  ", theme.muted()),
                Span::styled(
                    format!("{}", info.cache_hits),
                    Style::default().fg(theme.text.into()),
                ),
                Span::styled("   shared  ", theme.muted()),
                Span::styled(
                    format!("{}", info.coalesced),
                    Style::default().fg(theme.text.into()),
                ),
            ]));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "Friends run myx → Room → J → host:port + token above.",
                theme.muted(),
            )));
            lines.push(Line::from(Span::styled(
                "For remote guests expose this port (ngrok http 8787) and share the",
                theme.muted(),
            )));
            lines.push(Line::from(Span::styled(
                "token — your session never plays, so Spotify's one-stream rule holds.",
                theme.muted(),
            )));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "h/L  stop hosting    T  new token (locks out current guests)",
                theme.muted(),
            )));
        }
        RoomMode::Guest => {
            lines.push(Line::from(Span::styled(
                "JOINED ROOM",
                Style::default().fg(theme.success.into()),
            )));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("host   ", theme.muted()),
                Span::styled(
                    truncate(&app.room.guest_url, max.saturating_sub(7)),
                    Style::default().fg(theme.text.into()),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("token  ", theme.muted()),
                Span::styled(
                    truncate(&app.room.guest_token, max.saturating_sub(7)),
                    Style::default().fg(theme.primary.into()),
                ),
            ]));
            if let Some(n) = app.playback.now.as_ref() {
                lines.push(Line::raw(""));
                lines.push(Line::from(vec![
                    Span::styled("playing ", theme.muted()),
                    Span::styled(
                        truncate(&n.title, max.saturating_sub(9)),
                        Style::default()
                            .fg(theme.text.into())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {}", n.artist), theme.muted()),
                ]));
            }
            if !app.room.queue.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("queue   ", theme.muted()),
                    Span::styled(
                        format!(
                            "{}/{}  (n/b to skip)",
                            app.room.queue_index + 1,
                            app.room.queue.len()
                        ),
                        Style::default().fg(theme.text.into()),
                    ),
                ]));
            }
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "Browse/search anything and press Enter — audio comes from the",
                theme.muted(),
            )));
            lines.push(Line::from(Span::styled(
                "host's premium session via the room. Your own account is just for",
                theme.muted(),
            )));
            lines.push(Line::from(Span::styled(
                "browsing: playlists, likes and all.",
                theme.muted(),
            )));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "J  re-join    L  leave room",
                theme.muted(),
            )));
        }
    }

    // Join form overlay while input is captured.
    if let Some(stage) = app.room.input {
        let label = match stage {
            RoomInput::Url => "host url (host:port)",
            RoomInput::Token => "room token",
        };
        let value = match stage {
            RoomInput::Url => &app.room.guest_url,
            RoomInput::Token => &app.room.guest_token,
        };
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{label}: "),
                Style::default().fg(theme.primary.into()),
            ),
            Span::styled(
                truncate(value, max.saturating_sub(label.len() + 2)),
                Style::default().fg(theme.text.into()),
            ),
            Span::styled("_", Style::default().fg(theme.accent.into())),
        ]));
        lines.push(Line::from(Span::styled(
            "enter  next · esc  cancel",
            theme.muted(),
        )));
    }

    f.render_widget(Paragraph::new(lines), inner);
}
