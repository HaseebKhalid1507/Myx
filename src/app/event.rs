//! Engine events and metadata replies, applied to `App`.

use crate::*;

pub(crate) fn handle_engine_event(
    app: &mut App,
    ev: EngineEvent,
    meta_tx: &flume::Sender<TrackMeta>,
) {
    // Position ticks would bury everything else in the log.
    if !matches!(ev, EngineEvent::PositionCorrection { .. }) {
        liblog(format!("engine: {ev:?}"));
    }
    match ev {
        EngineEvent::TrackChanged { uri } => {
            app.status = "loading track…".to_string();
            if let Some(track_id) = track_id_from_uri(&uri) {
                // Record what we're waiting for so an earlier track's reply,
                // landing late, is discarded instead of overwriting this one.
                app.session.pending_meta = Some(format!("spotify:track:{track_id}"));
                let webapi = app.svc.webapi.clone();
                let tx = meta_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(fetch_track_meta(&webapi, &track_id));
                });
            }
        }
        EngineEvent::Playing { position_ms, .. } => {
            if !app.transport.playback_started {
                app.transport.playback_started = true;
                // Reapply persisted modes + volume to the freshly-started playback.
                let _ = app.svc.engine.shuffle(app.transport.shuffle);
                let _ = app.svc.engine.repeat(app.transport.repeat);
                let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
            }
            if let Some(n) = app.playback.now.as_mut() {
                n.is_playing = true;
            }
            app.playback.set_local_position(position_ms, true);
            if let Some(controls) = app.media_controls.as_mut() {
                let _ = controls.set_playback(MediaPlayback::Playing {
                    progress: Some(MediaPosition(Duration::from_millis(position_ms as u64))),
                });
            }
        }
        EngineEvent::Paused { position_ms, .. } => {
            if let Some(n) = app.playback.now.as_mut() {
                n.is_playing = false;
            }
            app.playback.set_local_position(position_ms, true);
            if let Some(controls) = app.media_controls.as_mut() {
                let _ = controls.set_playback(MediaPlayback::Paused {
                    progress: Some(MediaPosition(Duration::from_millis(position_ms as u64))),
                });
            }
        }
        EngineEvent::Stopped => {
            app.playback.now = None;
            app.transport.playback_started = false;
            // librespot cleared its context too, so only a fresh load can
            // resume from here — not a bare `play`.
            app.session.reclaimed = false;

            if let Some(controls) = app.media_controls.as_mut() {
                let _ = controls.set_playback(MediaPlayback::Stopped);
            }
        }
        EngineEvent::PositionCorrection { position_ms, .. } => {
            app.playback.set_local_position(position_ms, true);
            if let (Some(n), Some(controls)) =
                (app.playback.now.as_ref(), app.media_controls.as_mut())
            {
                let playback = if n.is_playing {
                    MediaPlayback::Playing {
                        progress: Some(MediaPosition(Duration::from_millis(position_ms as u64))),
                    }
                } else {
                    MediaPlayback::Paused {
                        progress: Some(MediaPosition(Duration::from_millis(position_ms as u64))),
                    }
                };
                let _ = controls.set_playback(playback);
            }
        }
        EngineEvent::Reconnecting => {
            app.status = "connection dropped — reconnecting…".to_string();
        }
        EngineEvent::Reconnected => {
            // The replacement Connect device starts idle, so whatever was
            // playing is not resumed; say so rather than leave a silent player
            // looking broken.
            app.status = if app.transport.playback_started {
                "reconnected — press play to resume".to_string()
            } else {
                "reconnected".to_string()
            };
        }
        EngineEvent::EndOfTrack { .. } => {}
    }
}

/// Is this metadata reply the one we are still waiting for?
///
/// `None` means nothing specific was requested (e.g. a path that predates the
/// guard), so accept — the guard only ever discards a reply we can prove is for
/// a different track.
pub(crate) fn meta_is_current(pending: Option<&str>, meta_uri: &str) -> bool {
    pending.is_none_or(|p| p == meta_uri)
}

pub(crate) fn apply_meta(
    app: &mut App,
    meta: TrackMeta,
    lyrics_tx: &flume::Sender<(Vec<(u32, String)>, bool)>,
) {
    // Metadata fetches run on independent blocking tasks, so skipping quickly
    // (n/b) can land an earlier track's reply after a later one. Applying it
    // would replace the whole NowPlaying — title, artist and cover — with the
    // wrong track's data.
    if !meta_is_current(app.session.pending_meta.as_deref(), &meta.uri) {
        return;
    }

    let cover = meta
        .image
        .image
        .as_ref()
        .map(|img| Cover::from_image(img.clone(), app.svc.picker.clone()));
    // A different cover encodes to a different symbol, so the diff emits it on
    // its own — no wipe, which would flash a blank box between the two covers.
    app.art_repaint = ArtRepaint::Draw;
    app.status.clear();
    app.view.lyrics.clear();
    app.view.lyrics_synced = false;

    // Fetch synced lyrics from lrclib for the new track.
    if !meta.title.is_empty() {
        let (artist, title, album, dur) = (
            meta.artist.clone(),
            meta.title.clone(),
            meta.album.clone(),
            meta.duration_ms,
        );
        let tx = lyrics_tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(fetch_lyrics_blocking(&artist, &title, &album, dur));
        });
    }

    app.playback.now = Some(NowPlaying {
        uri: meta.uri,
        title: meta.title,
        artist: meta.artist,
        album: meta.album,
        duration_ms: meta.duration_ms,
        position_ms: app
            .playback
            .now
            .as_ref()
            .map(|n| n.position_ms)
            .unwrap_or(0),
        position_at: Instant::now(),
        is_playing: app
            .playback
            .now
            .as_ref()
            .map(|n| n.is_playing)
            .unwrap_or(app.transport.playback_started),
        cover,
    });

    if let Some(theme) = meta.theme {
        app.theme.start_fade(theme);
        // Same instant, same palette: whatever the UI is fading towards is
        // exactly what subscribers are told to fade towards.
        #[cfg(feature = "mxc")]
        publish_theme(app, &theme);
    }

    if let Some(controls) = app.media_controls.as_mut() {
        let _ = controls.set_metadata(MediaMetadata {
            title: app.playback.now.as_ref().map(|n| n.title.as_str()),
            artist: app.playback.now.as_ref().map(|n| n.artist.as_str()),
            album: app.playback.now.as_ref().map(|n| n.album.as_str()),
            cover_url: meta.image.url.as_deref(),
            duration: app
                .playback
                .now
                .as_ref()
                .map(|n| Duration::from_millis(n.duration_ms as u64)),
        });
    }
}

/// Broadcast the new palette over MXC, if a publisher is running.
///
/// Called from the one place a track's palette is adopted, so the `Origin` can
/// carry the metadata that produced it — that provenance is what lets a
/// subscriber show "now playing" text or ignore everything that is not album
/// art. The fields are read back out of `app.playback.now`, which was assigned
/// from this very `TrackMeta` a few lines above (the `meta` fields themselves
/// have been moved into it by then); `apply_meta` has already returned early
/// for a stale reply, so this is always the current track.
///
/// Empty strings are omitted rather than sent as `Some("")`: the wire contract
/// says an absent field is unknown, and `""` would force every consumer to
/// re-check what the publisher already knows.
///
/// `fade_ms` is [`FADE_MS`] itself — the same constant `start_fade` uses — so
/// a consumer's cross-fade cannot drift out of sync with Myx's own.
#[cfg(feature = "mxc")]
fn publish_theme(app: &App, theme: &Theme) {
    use myx::mxc::{Origin, OriginKind};

    let Some(publisher) = app.mxc.as_ref() else {
        return;
    };
    let now = app.playback.now.as_ref();
    let field = |get: fn(&NowPlaying) -> &str| {
        now.map(get)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    };

    let track = field(|n| n.title.as_str());
    publisher.publish(
        Origin {
            kind: OriginKind::AlbumArt,
            // The title is the human-facing label; a track with no title (a
            // local file, a podcast segment) still has a palette worth naming.
            name: track.clone().unwrap_or_else(|| "album".to_string()),
            track,
            artist: field(|n| n.artist.as_str()),
            album: field(|n| n.album.as_str()),
            track_id: field(|n| n.uri.as_str()),
        },
        theme,
        u32::try_from(FADE_MS).unwrap_or(u32::MAX),
    );
}

/// Does this row carry a playable context URI, and under what name?
///
/// Context rows (playlist / album / artist) and the synthesized "▶︎ Play X"
/// rows both do; headers and tracks do not. Kept pure and free-standing so it
/// is unit-testable — `App` owns a librespot `Spirc` and cannot be built in a
/// test. `enter_label` shares this predicate so Enter opens exactly the rows
/// `P` plays.
pub(crate) fn context_target(item: &LibItem) -> Option<(String, String)> {
    (!item.is_header && !item.is_track).then(|| (item.uri.clone(), item.name.clone()))
}

/// Enter opens context rows and plays everything else.
pub(crate) fn enter_label(item: Option<&LibItem>) -> &'static str {
    match item {
        Some(i) if !i.is_track && !i.is_header => "open",
        _ => "select",
    }
}

/// `P` / `S`: play the highlighted context from anywhere — library section,
/// search results, or inside a drill-in (`cur_items` resolves all three).
pub(crate) fn play_selected_context(app: &mut App, shuffle: bool) {
    let Some(item) = app.cur_items().get(app.browse.selected).cloned() else {
        return;
    };
    match context_target(&item) {
        Some((uri, name)) => app.play_context_row(uri, name, shuffle),
        None => app.status = "not a playlist, album, or artist".to_string(),
    }
}
