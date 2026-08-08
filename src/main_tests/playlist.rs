use crate::*;
use serde_json::json;

fn ctx_row() -> LibItem {
    LibItem::ctx(
        "Chill Vibes".into(),
        "you · 142".into(),
        "spotify:playlist:1".into(),
    )
}

// ------------------------------------------------------ startup ordering

#[test]
fn authentication_finishes_before_terminal_initialization() {
    let order = std::cell::RefCell::new(Vec::new());

    let result = auth_then_terminal(
        || {
            order.borrow_mut().push("auth");
            Ok("authenticated")
        },
        || {
            order.borrow_mut().push("terminal");
            Ok("terminal")
        },
    )
    .expect("startup succeeds");

    assert_eq!(result, ("authenticated", "terminal"));
    assert_eq!(*order.borrow(), ["auth", "terminal"]);
}

#[test]
fn authentication_failure_never_initializes_terminal() {
    let terminal_initialized = std::cell::Cell::new(false);

    let result: Result<((), ())> = auth_then_terminal(
        || anyhow::bail!("cached refresh token was rejected"),
        || {
            terminal_initialized.set(true);
            Ok(())
        },
    );

    assert!(result.is_err());
    assert!(!terminal_initialized.get());
}

// ------------------------------------------------ optional integrations

#[test]
fn optional_integration_keeps_successful_service() {
    assert_eq!(
        optional_integration(true, || Ok::<_, ()>("media")),
        Some("media")
    );
}

#[test]
fn optional_integration_degrades_on_initialization_failure() {
    assert_eq!(
        optional_integration(true, || Err::<(), _>("no session bus")),
        None
    );
}

#[test]
fn optional_integration_skips_init_when_platform_is_unavailable() {
    let called = std::cell::Cell::new(false);
    let service = optional_integration(false, || {
        called.set(true);
        Ok::<_, ()>("media")
    });
    assert_eq!(service, None);
    assert!(!called.get());
}

#[test]
fn disconnected_media_channel_disables_future_receives() {
    let mut open = true;
    let event: Result<(), flume::RecvError> = Err(flume::RecvError::Disconnected);
    assert_eq!(consume_media_event(event, &mut open), None);
    assert!(!open);
}

#[test]
fn active_scrub_rejects_stale_engine_position() {
    assert!(!should_apply_engine_position(true, Some(42_000)));
    assert!(should_apply_engine_position(true, None));
    assert!(should_apply_engine_position(false, Some(42_000)));
}

#[test]
fn startup_restore_requires_the_setting_and_a_saved_track() {
    let mut saved = SavedState::default();
    assert!(!should_restore_saved_playback(true, None, &saved));

    saved.last_played = Some(LastPlayed::default());
    assert!(should_restore_saved_playback(true, None, &saved));
    assert!(!should_restore_saved_playback(false, None, &saved));
}

#[test]
fn explicit_startup_uri_wins_over_the_saved_track() {
    let saved = SavedState {
        last_played: Some(LastPlayed::default()),
        ..SavedState::default()
    };
    assert!(!should_restore_saved_playback(
        true,
        Some("spotify:album:1"),
        &saved,
    ));
}

// -------------------------------------------------------- context_target

#[test]
fn context_target_accepts_context_rows() {
    let (uri, name) = context_target(&ctx_row()).expect("playlist is a context");
    assert_eq!(uri, "spotify:playlist:1");
    assert_eq!(name, "Chill Vibes");
}

#[test]
fn context_target_accepts_synthesized_play_row() {
    // "▶︎ Play X" rows carry the context URI, so P works inside a drill-in.
    let row = LibItem::play("▶︎ Play Chill Vibes".into(), "spotify:playlist:1".into());
    assert_eq!(
        context_target(&row).map(|(u, _)| u),
        Some("spotify:playlist:1".to_string())
    );
}

#[test]
fn context_target_rejects_tracks_and_headers() {
    let track = LibItem::track("Song".into(), "Artist".into(), "spotify:track:9".into());
    assert!(context_target(&track).is_none());
    assert!(context_target(&LibItem::header("Songs")).is_none());
}

// --------------------------------------------------- parse_playlist_track

#[test]
fn parses_an_items_entry() {
    // The shape /playlists/{id}/items actually serves today.
    let it = json!({"added_at": "2024-01-01T00:00:00Z", "is_local": false, "item": {
        "name": "Coffee",
        "uri": "spotify:track:429NtPmr12aypzFH3FkN9l",
        "type": "track",
        "artists": [{"name": "beabadoobee"}]
    }});
    let li = parse_playlist_track(&it).expect("valid item");
    assert_eq!(li.name, "Coffee");
    assert_eq!(li.subtitle, "beabadoobee");
    assert_eq!(li.uri, "spotify:track:429NtPmr12aypzFH3FkN9l");
    assert!(li.is_track);
}

#[test]
fn still_parses_legacy_track_entry() {
    // Older /tracks shape, kept working through the API migration.
    let it = json!({"track": {
        "name": "Sailor Song",
        "uri": "spotify:track:abc",
        "artists": [{"name": "Gigi Perez"}]
    }});
    let li = parse_playlist_track(&it).expect("valid track");
    assert_eq!(li.name, "Sailor Song");
    assert_eq!(li.subtitle, "Gigi Perez");
}

#[test]
fn skips_null_entries() {
    // Real playlists contain these for items pulled from the catalogue.
    // Must yield None (skipped) rather than panic or abort the page.
    assert!(parse_playlist_track(&json!({ "item": null })).is_none());
    assert!(parse_playlist_track(&json!({ "track": null })).is_none());
    assert!(parse_playlist_track(&json!({})).is_none());
}

#[test]
fn skips_entry_without_uri() {
    let it = json!({"item": {"name": "No URI", "artists": [{"name": "X"}]}});
    assert!(parse_playlist_track(&it).is_none());
}

#[test]
fn missing_artists_yields_empty_artist_not_skip() {
    let it = json!({"item": {"name": "Untitled", "uri": "spotify:track:z"}});
    let li = parse_playlist_track(&it).expect("still playable without artists");
    assert_eq!(li.subtitle, "");
}

#[test]
fn total_prefers_items_over_legacy_tracks() {
    // Live /me/playlists shape: `items.total`, no `tracks` object at all.
    assert_eq!(
        playlist_total(&json!({"items": {"href": "…", "total": 155}})),
        Some(155)
    );
    assert_eq!(playlist_total(&json!({"tracks": {"total": 42}})), Some(42));
    assert_eq!(
        playlist_total(&json!({"items": {"total": 7}, "tracks": {"total": 9}})),
        Some(7)
    );
    assert_eq!(playlist_total(&json!({"name": "no counts"})), None);
}

#[test]
fn admits_local_files_and_episodes() {
    // Documents current behaviour: both parse as ordinary tracks.
    let local = json!({"is_local": true, "item": {
        "name": "Demo.mp3", "uri": "spotify:local:::Demo:180", "artists": [{"name": "Me"}]
    }});
    assert!(parse_playlist_track(&local).is_some());

    let episode = json!({"item": {
        "name": "Ep 12", "uri": "spotify:episode:e1", "type": "episode", "artists": []
    }});
    let li = parse_playlist_track(&episode).expect("episodes are admitted today");
    assert_eq!(li.subtitle, "");
}

// ------------------------------------------------------ playlist_subtitle

#[test]
fn subtitle_puts_count_before_owner() {
    // Count leads so it survives tail-first truncation in a narrow pane.
    assert_eq!(
        playlist_subtitle("ImLordVisssh", Some(155)),
        "155 · ImLordVisssh"
    );
    assert_eq!(playlist_subtitle("you", None), "you");
    assert_eq!(playlist_subtitle("", Some(12)), "12");
    assert_eq!(playlist_subtitle("", None), "");
}

// -------------------------------------------------------- meta_is_current

#[test]
fn stale_metadata_replies_are_dropped() {
    let a = "spotify:track:AAA";
    let b = "spotify:track:BBB";
    // Waiting on B: B's reply applies, A's late reply does not.
    assert!(meta_is_current(Some(b), b));
    assert!(!meta_is_current(Some(b), a));
    // Nothing outstanding -> accept (the guard only drops provable mismatches).
    assert!(meta_is_current(None, a));
}

// ------------------------------------------------------------ enter_label

#[test]
fn enter_label_matches_context_target() {
    let track = LibItem::track("Song".into(), "Artist".into(), "spotify:track:9".into());
    assert_eq!(enter_label(Some(&ctx_row())), "open");
    assert_eq!(enter_label(Some(&track)), "select");
    assert_eq!(enter_label(Some(&LibItem::header("Songs"))), "select");
    assert_eq!(enter_label(None), "select");

    // The invariant the footer relies on: Enter says "open" for exactly
    // the rows P can play.
    for row in [ctx_row(), track, LibItem::header("Songs")] {
        let opens = enter_label(Some(&row)) == "open";
        assert_eq!(opens, context_target(&row).is_some() && !row.is_play);
    }
}
