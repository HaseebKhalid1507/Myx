//! Terminal key input — the main keymap.

use crate::*;

/// Returns true if the app should quit.
pub(crate) fn handle_key(
    app: &mut App,
    code: KeyCode,
    mods: KeyModifiers,
    chans: &UiChannels,
) -> bool {
    // A find belongs to the list it was typed for; once another list is on
    // screen (a page opened, a section switched), it's gone.
    let list = app.list_key();
    app.find.forget_unless(&list);

    // --- Actions menu captures input while open ---
    // Double-press Ctrl-C to quit (works from anywhere). Single press arms it.
    if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
        let now = Instant::now();
        if app
            .session
            .last_ctrl_c
            .map(|t| now.duration_since(t) < Duration::from_millis(1500))
            .unwrap_or(false)
        {
            return true;
        }
        app.session.last_ctrl_c = Some(now);
        app.status = "press Ctrl-C again to quit".to_string();
        return false;
    }

    if app.view.actions.is_some() {
        handle_action_key(app, code, &chans.detail, &chans.astatus);
        return false;
    }

    if app.view.equalizer.is_some() {
        handle_equalizer_key(app, code);
        return false;
    }

    // --- Search input mode captures everything ---
    if app.search.input_mode {
        match code {
            KeyCode::Esc => app.search.input_mode = false,
            KeyCode::Enter => {
                app.search.input_mode = false;
                let q = app.search.query().trim().to_string();
                if !q.is_empty() {
                    // Results replace whatever list is open, pages included;
                    // a page left on top would hide them.
                    app.browse.details.clear();
                    app.search.searching = true;
                    app.search.in_flight = true;
                    app.browse.selected = 0;
                    app.status = "searching…".to_string();
                    spawn_search(app.svc.webapi.clone(), q, chans.search.clone());
                }
            }
            // Ctrl-U clears the query — readline muscle memory. The fork
            // binds Ctrl-U to undo; shadow it (same call as agent-runtime).
            KeyCode::Char('u') if mods.contains(crossterm::event::KeyModifiers::CONTROL) => {
                app.search.clear();
            }
            // Everything else — typing, cursor movement, word ops — is the
            // editor's business. Enter is intercepted above, so no newlines.
            _ => {
                app.search
                    .input
                    .input(crossterm::event::KeyEvent::new(code, mods));
            }
        }
        return false;
    }

    // --- Find prompt: typing narrows the list; arrows still move ---
    if app.find.typing {
        match code {
            KeyCode::Esc => {
                app.find.clear();
                app.normalize_selection();
                return false;
            }
            KeyCode::Up => {
                app.move_sel(-1);
                return false;
            }
            KeyCode::Down => {
                app.move_sel(1);
                return false;
            }
            // Enter picks the highlighted match, the way it always does; the
            // list stays narrowed until Esc.
            KeyCode::Enter => {
                app.find.typing = false;
                if app.find_query().is_none() {
                    app.find.clear();
                    return false;
                }
            }
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
                app.find.input = Default::default();
                app.browse.selected = app.first_selectable();
                return false;
            }
            _ => {
                app.find
                    .input
                    .input(crossterm::event::KeyEvent::new(code, mods));
                // Like fzf: a new query puts the cursor on its first match.
                app.browse.selected = app.first_selectable();
                return false;
            }
        }
    }

    // Keys that drive the library do nothing while it isn't on screen — under
    // zen, or in the Focus layout on another view — rather than moving a
    // selection nobody can see. Except `/` in Focus: search results go in the
    // library, so it brings the Library view up to search in. Placed after the
    // overlays above, which stay usable if one was already open.
    if !app.library_visible() && drives_library(code) {
        let search_brings_it_up =
            code == KeyCode::Char('/') && app.rotation().contains(&RightView::Library);
        if !search_brings_it_up {
            return false;
        }
        app.view.mode = RightView::Library;
    }

    match code {
        KeyCode::Char('e') => {
            app.view.equalizer = Some(EqualizerOverlay::default());
        }
        KeyCode::Char('/') => {
            app.search.input_mode = true;
            app.search.clear();
        }
        KeyCode::Char('f') => {
            let list = app.list_key();
            app.find.open(list);
            app.normalize_selection();
        }
        KeyCode::Char('q') => return true,
        // Esc undoes one thing: a find first, then the page, then the search.
        KeyCode::Esc if app.find_query().is_some() => {
            app.find.clear();
            app.normalize_selection();
        }
        KeyCode::Esc => {
            if let Some(d) = app.browse.details.pop() {
                app.browse.selected = d.parent_selected;
            } else if app.search.searching {
                app.search.searching = false;
                app.browse.selected = 0;
            }
            // Nothing to back out of — Esc no longer quits (use q or Ctrl-C twice).
        }
        KeyCode::Char(' ') | KeyCode::Char('p') | KeyCode::Media(MediaKeyCode::PlayPause) => {
            if app.transport.playback_started {
                let _ = app.svc.engine.toggle();
            } else {
                // Resume the persisted source (context/radio/liked).
                resume_source(app, &chans.radio);
                app.transport.playback_started = true;
            }
        }
        KeyCode::Media(MediaKeyCode::Stop) => {
            app.svc.engine.stop();
        }
        KeyCode::Char('n') | KeyCode::Media(MediaKeyCode::TrackNext) => {
            let _ = app.svc.engine.next();
        }
        KeyCode::Char('b') | KeyCode::Media(MediaKeyCode::TrackPrevious) => {
            let _ = app.svc.engine.prev();
        }
        KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Media(MediaKeyCode::RaiseVolume) => {
            app.transport.volume = (app.transport.volume + 5).min(100);
            let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
        }
        KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Media(MediaKeyCode::LowerVolume) => {
            app.transport.volume = app.transport.volume.saturating_sub(5);
            let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
        }
        KeyCode::Char('s') => {
            app.transport.shuffle = !app.transport.shuffle;
            let _ = app.svc.engine.shuffle(app.transport.shuffle);
        }
        // Play the highlighted playlist / album / artist outright. Enter still
        // opens; this is the direct route that used to require two Enters or
        // the actions menu.
        KeyCode::Char('P') => play_selected_context(app, false),
        KeyCode::Char('S') => {
            // Flip the global toggle too, or the footer would show shuffle off
            // while playback is shuffled, and `resume_source` would later
            // replay this context unshuffled.
            app.transport.shuffle = true;
            let _ = app.svc.engine.shuffle(true);
            play_selected_context(app, true);
        }
        KeyCode::Char('R') => {
            app.transport.repeat = !app.transport.repeat;
            let _ = app.svc.engine.repeat(app.transport.repeat);
        }
        KeyCode::Char('r') => {
            app.status = "loading library…".to_string();
            app.browse.library.reset_loading();
            spawn_library_fetch(
                app.svc.webapi.clone(),
                chans.lib.clone(),
                chans.libdone.clone(),
            );
        }
        KeyCode::Char('o') => {
            app.browse.sort = app.browse.sort.next();
            let m = app.browse.sort;
            sort_list(app.cur_list_mut(), m);
            app.browse.selected = app.first_selectable();
            app.status = format!("sorted by {}", m.label());
        }
        KeyCode::Char('a') => {
            // Zen hides the library, so the menu belongs to what is playing —
            // acting on a selection nobody can see is how it ends up offering
            // "remove from Liked" for the wrong track.
            let item = if !app.library_visible() {
                app.playback
                    .now
                    .as_ref()
                    .filter(|n| !n.uri.is_empty())
                    .map(|n| LibItem::track(n.title.clone(), n.artist.clone(), n.uri.clone()))
            } else {
                app.selected_item().cloned()
            };
            if let Some(item) = item {
                if !item.is_header() && !item.is_play() {
                    // Instant menu (no network), then enrich when the API returns.
                    app.view.actions = Some(build_action_menu(None, &item));
                    spawn_action_menu(app.svc.webapi.clone(), item, chans.menu.clone());
                }
            }
        }
        // Tab / Shift+Tab (and [ ]) rotate the library sections.
        KeyCode::Tab | KeyCode::Char(']') => {
            app.search.searching = false;
            app.browse.section = app.browse.section.shift(1);
            app.browse.selected = app.first_selectable();
        }
        KeyCode::BackTab | KeyCode::Char('[') => {
            app.search.searching = false;
            app.browse.section = app.browse.section.shift(-1);
            app.browse.selected = app.first_selectable();
        }
        // Arrow keys rotate the right-pane view; Shift+arrows seek ±5s.
        KeyCode::Right if mods.contains(KeyModifiers::SHIFT) => {
            app.playback.seek_step(SEEK_STEP_MS)
        }
        KeyCode::Left if mods.contains(KeyModifiers::SHIFT) => {
            app.playback.seek_step(-SEEK_STEP_MS)
        }
        KeyCode::Right => {
            app.view.mode = app.view.mode.shift_in(app.rotation(), 1);
            if app.view.mode == RightView::Queue && app.transport.playback_started {
                spawn_queue_fetch(app.svc.webapi.clone(), chans.queue.clone());
            }
        }
        KeyCode::Left => {
            app.view.mode = app.view.mode.shift_in(app.rotation(), -1);
            if app.view.mode == RightView::Queue && app.transport.playback_started {
                spawn_queue_fetch(app.svc.webapi.clone(), chans.queue.clone());
            }
        }
        // The frame loop notices the layout change and wipes the art box.
        KeyCode::Char('z') => {
            app.view.zen = !app.view.zen;
            // In Focus, zen takes the Library view out of the rotation.
            app.settle_view();
        }
        // Shifted on purpose: a stray press repaints everything and rewrites
        // config.toml.
        KeyCode::Char('T') => {
            let on = !app.theme.target.transparent;
            app.theme.set_transparent(on);
            app.status = match myx::config::Config::save_transparent(on) {
                Ok(()) => format!("transparent background {}", if on { "on" } else { "off" }),
                Err(e) => format!("background switched, but config.toml was not saved: {e}"),
            };
        }
        KeyCode::Down | KeyCode::Char('j') => app.move_sel(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_sel(-1),
        // Needs a terminal that reports modified Enter (kitty, WezTerm, foot).
        KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => play_selected_context(app, false),
        KeyCode::Enter => match app.activate() {
            Activated::Open(uri, name) => {
                spawn_detail_fetch(app.svc.webapi.clone(), uri, name, chans.detail.clone());
            }
            Activated::Radio(uri) => {
                app.status = "starting radio…".to_string();
                let session = app.svc.engine.session();
                let tx = chans.radio.clone();
                tokio::spawn(async move {
                    let res = match tokio::time::timeout(
                        Duration::from_secs(12),
                        engine::radio_tracks(&session, &uri),
                    )
                    .await
                    {
                        Ok(r) => r.map_err(|e| e.to_string()),
                        Err(_) => {
                            Err("timed out (mercury radio endpoint unresponsive)".to_string())
                        }
                    };
                    let _ = tx.send(res.map(|uris| Radio {
                        uris,
                        start_position_ms: 0,
                    }));
                });
            }
            Activated::None => {}
        },
        _ => {}
    }
    false
}

/// Keys whose whole effect is on the library pane.
///
/// Zen hides that pane, so these do nothing there rather than moving a selection
/// nobody can see — and `a` is deliberately absent, because it retargets onto
/// the playing track instead of going quiet.
pub(crate) fn drives_library(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Char('/' | 'f' | '[' | ']' | 'j' | 'k' | 'o' | 'r' | 'P' | 'S')
    )
}
