//! Input while the actions menu overlay is open, plus the clipboard escape
//! hatch its "copy link" entries use.

use crate::*;

/// Handle input while the actions menu is open.
pub(crate) fn handle_action_key(
    app: &mut App,
    code: KeyCode,
    detail_tx: &flume::Sender<(String, String, Vec<LibItem>)>,
    astatus_tx: &flume::Sender<String>,
) {
    match code {
        KeyCode::Esc | KeyCode::Char('a') => {
            app.view.actions = None;
            app.view.action_anchor = None;
            app.status.clear();
            return;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(m) = app.view.actions.as_mut() {
                m.selected = m.selected.saturating_sub(1);
            }
            return;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(m) = app.view.actions.as_mut() {
                m.selected = (m.selected + 1).min(m.items.len().saturating_sub(1));
            }
            return;
        }
        KeyCode::Enter => {}
        _ => return,
    }

    // Enter: act on the selected entry.
    let kind = app
        .view
        .actions
        .as_ref()
        .and_then(|m| m.items.get(m.selected))
        .map(|i| i.kind.clone());
    let Some(kind) = kind else { return };
    match kind {
        ActionKind::AddToPlaylistMenu { track_uri } => {
            let items: Vec<ActionItem> = app
                .browse
                .library
                .playlists
                .iter()
                .filter_map(|p| {
                    let id = p.uri.rsplit(':').next()?.to_string();
                    Some(ActionItem {
                        label: p.name.clone(),
                        kind: ActionKind::AddToPlaylist {
                            playlist_id: id,
                            track_uri: track_uri.clone(),
                        },
                    })
                })
                .collect();
            if items.is_empty() {
                app.status = "no playlists to add to".to_string();
                app.view.actions = None;
                app.view.action_anchor = None;
            } else {
                app.view.action_anchor = None;
                app.view.actions = Some(ActionMenu {
                    title: "Add to playlist".to_string(),
                    items,
                    selected: 0,
                });
            }
        }
        ActionKind::Play { uri, name } => {
            // Previously called engine.play_context directly, leaving
            // source/source_name stale: PLAYING FROM showed the wrong context
            // and resume-on-launch replayed the previous one.
            let shuffle = app.transport.shuffle;
            app.play_context_row(uri, name, shuffle);
            app.view.actions = None;
            app.view.action_anchor = None;
        }
        ActionKind::Open { uri, name } => {
            spawn_detail_fetch(app.svc.webapi.clone(), uri, name, detail_tx.clone());
            app.view.actions = None;
            app.view.action_anchor = None;
        }
        ActionKind::CopyLink { uri } => {
            app.status = if copy_to_clipboard(&uri_to_url(&uri)) {
                "link copied".to_string()
            } else {
                "clipboard unavailable".to_string()
            };
            app.view.actions = None;
            app.view.action_anchor = None;
        }
        ActionKind::Queue { uri, display } => {
            app.transport.queue.insert(0, display.clone());
            app.transport.queue_uris.insert(0, uri.clone());
            spawn_action(
                app.svc.webapi.clone(),
                ActionKind::Queue { uri, display },
                astatus_tx.clone(),
            );
            app.view.actions = None;
            app.view.action_anchor = None;
        }
        other => {
            spawn_action(app.svc.webapi.clone(), other, astatus_tx.clone());
            app.view.actions = None;
            app.view.action_anchor = None;
        }
    }
}

/// Copy text to the system clipboard via whatever tool is available.
pub(crate) fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let candidates: [(&str, &[&str]); 4] = [
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["-b", "-i"]),
        ("pbcopy", &[]),
    ];
    for (cmd, args) in candidates {
        if let Ok(mut child) = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut sin) = child.stdin.take() {
                let _ = sin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return true;
        }
    }
    false
}
