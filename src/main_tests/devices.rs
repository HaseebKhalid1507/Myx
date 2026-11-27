//! Remote mode (#49): following another device's playback off the cluster
//! push, and not letting this myx's own player — winding down after handing
//! over — overwrite what's on screen.
//!
//! Needs `--features test-support` (CI's `--all-features` has it).

#![cfg(feature = "test-support")]

use super::sizes::playing_app;
use crate::*;
use myx::engine::{Device, DeviceKind};

pub(crate) const PHONE_ID: &str = "1111111111111111111111111111111111111111";

pub(crate) fn phone_device() -> Device {
    Device {
        id: PHONE_ID.to_string(),
        name: "Pixel 7".to_string(),
        kind: DeviceKind::Phone,
        volume: Some(40),
        is_active: true,
        controllable: true,
        is_this: false,
    }
}

/// The phone playing `track` from a playlist, 30s in.
pub(crate) fn phone(track: &str, playing: bool) -> Playback {
    Playback {
        device: phone_device(),
        track_uri: track.to_string(),
        context_uri: "spotify:playlist:p".to_string(),
        context_name: "Mix".to_string(),
        position_ms: 30_000,
        duration_ms: 200_000,
        is_playing: playing,
        shuffle: true,
        repeat: Repeat::One,
    }
}

fn push(holder: Holder, changed_ms: i64) -> Snapshot {
    Snapshot {
        holder,
        devices: None,
        changed_ms,
    }
}

fn apply(app: &mut App, snap: Snapshot) {
    apply_snapshot(app, snap, &flume::unbounded().0);
}

fn engine_event(app: &mut App, ev: EngineEvent) {
    handle_engine_event(app, ev, &flume::unbounded().0);
}

/// `playing_app`, now following the phone, which plays what was on screen
/// (so no metadata fetch goes out and no runtime is needed).
pub(crate) fn remote_app() -> App {
    let mut app = playing_app();
    apply(
        &mut app,
        push(Holder::Elsewhere(phone("spotify:track:0", true)), 1),
    );
    app
}

#[test]
fn another_device_taking_over_is_followed() {
    let app = remote_app();
    let remote = app.devices.remote.as_ref().expect("remote mode");
    assert_eq!(remote.device_id, PHONE_ID);
    assert_eq!(remote.name, "Pixel 7");
    // Nothing is loaded here any more: play back here has to load.
    assert!(!app.transport.playback_started);
    let now = app.playback.now.as_ref().unwrap();
    assert!(now.is_playing);
    assert!((30_000..31_000).contains(&app.playback.position_ms()));
    assert!(app.status.contains("Pixel 7"), "{:?}", app.status);
}

#[test]
fn the_footer_and_meter_show_the_remote_devices_settings_not_ours() {
    let mut app = remote_app();
    app.transport.volume = 80;
    assert_eq!(app.shown_volume(), Some(40));
    assert!(app.shown_shuffle() && !app.transport.shuffle);
    assert_eq!(app.shown_repeat(), Repeat::One);
    assert_eq!(app.svc.engine.repeat_mode(), Repeat::Off, "ours untouched");
    // A device whose volume can't be set from here shows none.
    app.devices.remote.as_mut().unwrap().volume = None;
    assert_eq!(app.shown_volume(), None);
}

#[tokio::test]
async fn a_new_remote_track_is_fetched_and_resume_follows_its_context() {
    let mut app = remote_app();
    apply(
        &mut app,
        push(Holder::Elsewhere(phone("spotify:track:new", false)), 2),
    );
    assert_eq!(
        app.session.pending_meta.as_deref(),
        Some("spotify:track:new")
    );
    assert!(matches!(&app.transport.source, PlaySource::Context(c) if c == "spotify:playlist:p"));
    assert_eq!(app.transport.source_name, "Mix");
    assert!(!app.playback.now.as_ref().unwrap().is_playing);
}

#[tokio::test]
async fn the_queue_is_dropped_when_the_remote_moves_to_another_context() {
    // remote_app's push brought the phone's playlist in place of ours.
    let mut app = remote_app();
    assert!(app.transport.queue.is_empty(), "the old queue was ours");
    // Same playlist, next track: the queue stays until the sync replaces it.
    app.transport.queue = vec!["Queued · Artist".into()];
    apply(
        &mut app,
        push(Holder::Elsewhere(phone("spotify:track:1", true)), 2),
    );
    assert_eq!(app.session.pending_meta.as_deref(), Some("spotify:track:1"));
    assert_eq!(app.transport.queue.len(), 1);
    // A list started from here has no context on Spotify's side: ours stays.
    app.transport.source = PlaySource::Liked;
    app.transport.source_name = "Liked Songs".into();
    let mut liked = phone("spotify:track:2", true);
    liked.context_uri.clear();
    liked.context_name.clear();
    apply(&mut app, push(Holder::Elsewhere(liked), 3));
    assert_eq!(app.session.pending_meta.as_deref(), Some("spotify:track:2"));
    assert!(matches!(app.transport.source, PlaySource::Liked));
    assert_eq!(app.transport.source_name, "Liked Songs");
    assert_eq!(app.transport.queue.len(), 1);
}

#[test]
fn the_remote_stopping_leaves_its_track_paused_here() {
    let mut app = remote_app();
    apply(&mut app, push(Holder::Nobody, 3));
    assert!(app.devices.remote.is_none());
    let now = app.playback.now.as_ref().expect("its track stays up");
    assert!(!now.is_playing);
    assert_eq!(now.uri, "spotify:track:0");
    assert!(app.status.contains("stopped"), "{:?}", app.status);
}

#[test]
fn this_myx_becoming_active_ends_remote_mode_with_the_phones_shuffle_and_repeat() {
    let mut app = remote_app();
    app.transport.volume = 80;
    apply(&mut app, push(Holder::Here, 3));
    assert!(app.devices.remote.is_none());
    assert_eq!(app.status, "playing here");
    // They came with the playback, so the first play here mustn't undo them…
    assert!(app.transport.shuffle);
    assert_eq!(app.svc.engine.repeat_mode(), Repeat::One);
    // …but the phone's volume was the phone's.
    assert_eq!(app.transport.volume, 80);
}

#[test]
fn an_overtaken_push_is_dropped() {
    let mut app = remote_app();
    apply(&mut app, push(Holder::Here, 10));
    // Older than the one applied: it must not put remote mode back.
    apply(
        &mut app,
        push(Holder::Elsewhere(phone("spotify:track:0", true)), 5),
    );
    assert!(app.devices.remote.is_none());
    // GET /me/player carries no change time and always applies.
    apply(
        &mut app,
        push(Holder::Elsewhere(phone("spotify:track:0", true)), 0),
    );
    assert!(app.devices.remote.is_some());
}

#[test]
fn this_myx_winding_down_after_handing_over_leaves_the_screen_alone() {
    let mut app = remote_app();
    for ev in [
        EngineEvent::Paused {
            uri: "spotify:track:old".into(),
            position_ms: 0,
        },
        EngineEvent::PositionCorrection {
            uri: "spotify:track:old".into(),
            position_ms: 0,
        },
        EngineEvent::Stopped,
        EngineEvent::EndOfTrack {
            uri: "spotify:track:old".into(),
        },
    ] {
        engine_event(&mut app, ev);
        let now = app.playback.now.as_ref().expect("still the remote's track");
        assert!(now.is_playing);
        assert!(app.playback.position_ms() >= 30_000);
        assert!(app.devices.remote.is_some());
    }
}

#[test]
fn this_myx_playing_means_it_took_over() {
    let mut app = remote_app();
    engine_event(
        &mut app,
        EngineEvent::Playing {
            uri: "spotify:track:0".into(),
            position_ms: 31_000,
        },
    );
    assert!(app.devices.remote.is_none());
    assert!(app.transport.playback_started);
}

/// The phone plays on through a dropped connection; resuming here after the
/// reconnect would take playback off it.
#[test]
fn a_dropped_connection_in_remote_mode_resumes_nothing_here() {
    let mut app = remote_app();
    engine_event(&mut app, EngineEvent::Reconnecting);
    assert!(!app.transport.resume_after_reconnect);
    assert!(app.playback.now.as_ref().unwrap().is_playing);
    engine_event(&mut app, EngineEvent::Reconnected);
    assert!(!app.transport.resume_after_reconnect);
    assert!(app.devices.remote.is_some());
}

// ------------------------------------------------------ keys in remote mode

/// `app` with its player worker's queue in reach, emptied.
fn watch_player(app: &mut App) -> flume::Receiver<PlayerRequest> {
    let (tx, rx) = flume::unbounded();
    app.svc.player = tx;
    rx
}

fn sent(rx: &flume::Receiver<PlayerRequest>) -> Vec<PlayerCommand> {
    rx.try_iter()
        .map(|request| match request {
            PlayerRequest::Command {
                device_id, command, ..
            } => {
                assert_eq!(device_id, PHONE_ID, "{command:?} went to the wrong device");
                command
            }
            other => panic!("not a command: {other:?}"),
        })
        .collect()
}

fn press(app: &mut App, code: KeyCode) {
    handle_key(app, code, KeyModifiers::empty(), &super::sizes::chans());
}

#[test]
fn the_transport_keys_drive_the_remote_device_not_this_myx() {
    let mut app = remote_app();
    let rx = watch_player(&mut app);
    app.transport.volume = 80;

    press(&mut app, KeyCode::Char(' '));
    assert!(
        !app.playback.now.as_ref().unwrap().is_playing,
        "shown at once"
    );
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char('b'));
    press(&mut app, KeyCode::Char('+'));
    press(&mut app, KeyCode::Char('-'));
    press(&mut app, KeyCode::Char('-'));
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Char('R'));
    assert_eq!(
        sent(&rx),
        vec![
            PlayerCommand::Pause,
            PlayerCommand::Play,
            PlayerCommand::Next,
            PlayerCommand::Previous,
            PlayerCommand::Volume(45),
            PlayerCommand::Volume(40),
            PlayerCommand::Volume(35),
            PlayerCommand::Shuffle(false),
            PlayerCommand::Repeat(Repeat::Off),
        ]
    );
    // The remote's settings moved; this myx's own did not.
    let remote = app.devices.remote.as_ref().unwrap();
    assert_eq!(remote.volume, Some(35));
    assert!(!remote.shuffle);
    assert_eq!(remote.repeat, Repeat::Off);
    assert_eq!(app.transport.volume, 80);
    assert!(!app.transport.shuffle);
    // R said what it did, rather than "the player didn't get it: not
    // connected" — which is what reaching the (detached) engine says.
    assert_eq!(app.status, "repeat off");
}

#[test]
fn a_scrub_on_the_remote_is_one_seek_once_the_keys_stop() {
    let mut app = remote_app();
    let rx = watch_player(&mut app);
    for _ in 0..3 {
        handle_key(
            &mut app,
            KeyCode::Right,
            KeyModifiers::SHIFT,
            &super::sizes::chans(),
        );
    }
    assert!(sent(&rx).is_empty(), "nothing until the keys settle");
    app.flush_seek(Instant::now() + Duration::from_secs(1));
    match sent(&rx).as_slice() {
        [PlayerCommand::Seek(ms)] => assert!((35_000..36_000).contains(ms), "{ms}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn outside_remote_mode_nothing_goes_to_the_web_api() {
    let mut app = playing_app();
    let rx = watch_player(&mut app);
    for key in [' ', 'n', 'b', '+', '-', 's', 'R'] {
        press(&mut app, KeyCode::Char(key));
    }
    assert!(rx.try_iter().next().is_none());
    assert_eq!(app.transport.volume, 75, "+ then - then - on this myx");
}

#[test]
fn a_device_that_cant_be_driven_says_so_and_is_sent_nothing() {
    let mut app = remote_app();
    let rx = watch_player(&mut app);
    app.devices.remote.as_mut().unwrap().controllable = false;
    press(&mut app, KeyCode::Char('n'));
    assert!(sent(&rx).is_empty());
    assert!(
        app.status.contains("can't be controlled"),
        "{:?}",
        app.status
    );

    let mut app = remote_app();
    let rx = watch_player(&mut app);
    app.devices.remote.as_mut().unwrap().volume = None;
    press(&mut app, KeyCode::Char('+'));
    assert!(sent(&rx).is_empty());
    assert!(app.status.contains("volume"), "{:?}", app.status);
}

#[test]
fn picking_a_track_plays_it_on_the_remote_device() {
    let mut app = remote_app();
    let rx = watch_player(&mut app);
    app.browse.section = Section::Liked;
    app.browse.selected = 3;
    press(&mut app, KeyCode::Enter);
    let liked: Vec<String> = (0..40).map(|i| format!("spotify:track:{i}")).collect();
    assert_eq!(
        sent(&rx),
        vec![PlayerCommand::Start(Load::Tracks {
            uris: liked,
            start: Some("spotify:track:3".into()),
            position_ms: 0,
        })]
    );
}

#[test]
fn shuffle_playing_a_playlist_on_the_remote_shuffles_it_first() {
    let mut app = remote_app();
    let rx = watch_player(&mut app);
    app.devices.remote.as_mut().unwrap().shuffle = false;
    app.browse.library.set(
        Section::Playlists,
        vec![LibItem::ctx(
            "Chill Vibes".into(),
            "you".into(),
            "spotify:playlist:1".into(),
        )],
    );
    app.browse.section = Section::Playlists;
    app.browse.selected = 0;
    press(&mut app, KeyCode::Char('S'));
    assert_eq!(
        sent(&rx),
        vec![
            PlayerCommand::Shuffle(true),
            PlayerCommand::Start(Load::Context {
                uri: "spotify:playlist:1".into(),
                track: None,
                position_ms: 0,
            }),
        ]
    );
    assert!(app.shown_shuffle());
}

#[test]
fn the_web_apis_answer_about_who_plays_puts_myx_in_or_out_of_remote_mode() {
    let mut app = playing_app();
    let meta = flume::unbounded().0;
    handle_player_reply(
        &mut app,
        PlayerReply::State(Ok(Some(phone("spotify:track:0", true)))),
        &meta,
    );
    assert!(app.devices.remote.is_some());
    handle_player_reply(&mut app, PlayerReply::State(Ok(None)), &meta);
    assert!(app.devices.remote.is_none());
    // This myx itself: not a remote.
    let mut here = phone("spotify:track:0", true);
    here.device.id = super::sizes::HERE_ID.to_string();
    handle_player_reply(&mut app, PlayerReply::State(Ok(Some(here))), &meta);
    assert!(app.devices.remote.is_none());
    // Failures go to the status line.
    handle_player_reply(&mut app, PlayerReply::State(Err("HTTP 502".into())), &meta);
    assert!(app.status.contains("HTTP 502"), "{:?}", app.status);
    handle_player_reply(&mut app, PlayerReply::Status("couldn't skip".into()), &meta);
    assert_eq!(app.status, "couldn't skip");
}
