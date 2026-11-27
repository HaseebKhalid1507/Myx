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
    assert!(app.transport.queue.is_empty(), "the old queue was ours");
    assert!(!app.playback.now.as_ref().unwrap().is_playing);
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
fn this_myx_becoming_active_ends_remote_mode() {
    let mut app = remote_app();
    apply(&mut app, push(Holder::Here, 3));
    assert!(app.devices.remote.is_none());
    assert_eq!(app.status, "playing here");
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
