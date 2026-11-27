//! Spotify Connect (#49): following another device's playback off the cluster
//! push without this myx's own player — winding down after handing over —
//! overwriting it; the keys driving that device and not this myx; and the
//! devices menu that moves playback between them.
//!
//! Needs `--features test-support` (CI's `--all-features` has it).

#![cfg(feature = "test-support")]

use super::sizes::{draw, playing_app, screen_text, sweep, test_app, HERE_ID};
use crate::*;
use myx::engine::{Device, DeviceKind};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

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
    here.device.id = HERE_ID.to_string();
    handle_player_reply(&mut app, PlayerReply::State(Ok(Some(here))), &meta);
    assert!(app.devices.remote.is_none());
    // Failures go to the status line.
    handle_player_reply(&mut app, PlayerReply::State(Err("HTTP 502".into())), &meta);
    assert!(app.status.contains("HTTP 502"), "{:?}", app.status);
    handle_player_reply(&mut app, PlayerReply::Status("couldn't skip".into()), &meta);
    assert_eq!(app.status, "couldn't skip");
}

// ------------------------------------------------------------ devices menu

fn listed() -> Vec<Device> {
    let sonos = Device {
        id: "s".into(),
        name: "Kitchen Sonos".into(),
        kind: DeviceKind::Speaker,
        volume: Some(20),
        is_active: false,
        controllable: false,
        is_this: false,
    };
    let tv = Device {
        id: "t".into(),
        name: "TV".into(),
        kind: DeviceKind::Tv,
        volume: None,
        controllable: true,
        ..sonos.clone()
    };
    vec![sonos, phone_device(), tv]
}

/// The menu open over `app`, the list in.
fn menu_app(mut app: App) -> (App, flume::Receiver<PlayerRequest>) {
    let rx = watch_player(&mut app);
    press(&mut app, KeyCode::Char('d'));
    assert!(app.devices.menu.is_some());
    assert!(matches!(rx.try_recv(), Ok(PlayerRequest::Devices)));
    apply_device_list(&mut app, Ok(listed()));
    (app, rx)
}

fn cursor_name(app: &App) -> String {
    let rows = app.device_rows();
    rows[app.device_cursor(&rows)].name.clone()
}

#[test]
fn the_menu_opens_on_the_playing_device_and_moves_playback_to_the_one_picked() {
    let (mut app, rx) = menu_app(remote_app());
    // This myx, Pixel 7, TV, and the Sonos greyed at the bottom.
    let names: Vec<String> = app.device_rows().into_iter().map(|d| d.name).collect();
    assert_eq!(names, ["myx · test", "Pixel 7", "TV", "Kitchen Sonos"]);
    assert_eq!(cursor_name(&app), "Pixel 7");
    press(&mut app, KeyCode::Down);
    assert_eq!(cursor_name(&app), "TV");
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(cursor_name(&app), "TV", "the Sonos can't be picked");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        vec![PlayerRequest::Command {
            device_id: "t".into(),
            device_name: "TV".into(),
            command: PlayerCommand::Transfer,
        }]
    );
    assert!(app.devices.menu.is_none());
    assert_eq!(app.status, "moving playback to TV…");
}

#[test]
fn picking_the_device_already_playing_just_closes() {
    let (mut app, rx) = menu_app(remote_app());
    press(&mut app, KeyCode::Enter);
    assert!(rx.try_iter().next().is_none());
    assert!(app.devices.menu.is_none());
    assert_eq!(app.status, "playback's already on Pixel 7");
}

#[test]
fn picking_this_myx_pulls_playback_over_through_librespot() {
    let (mut app, rx) = menu_app(remote_app());
    press(&mut app, KeyCode::Up);
    assert_eq!(cursor_name(&app), "myx · test");
    press(&mut app, KeyCode::Enter);
    // Not through the Web API, and the detached engine's refusal is said, not
    // dropped; the menu stays open to try again.
    assert!(rx.try_iter().next().is_none());
    assert!(
        app.status.starts_with("couldn't move playback here"),
        "{:?}",
        app.status
    );
    assert!(app.devices.menu.is_some());
}

#[test]
fn the_cursor_stays_on_its_device_when_the_list_changes_under_it() {
    let (mut app, _rx) = menu_app(remote_app());
    press(&mut app, KeyCode::Down);
    assert_eq!(cursor_name(&app), "TV");
    // An "Alarm clock" arrives, sorting above the TV.
    let mut more = listed();
    more.push(Device {
        id: "a".into(),
        name: "Alarm clock".into(),
        ..phone_device()
    });
    apply_device_list(&mut app, Ok(more));
    assert_eq!(cursor_name(&app), "TV");
}

#[test]
fn esc_or_d_closes_the_menu_and_d_in_the_search_box_is_text() {
    let (mut app, _rx) = menu_app(remote_app());
    press(&mut app, KeyCode::Esc);
    assert!(app.devices.menu.is_none());
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Char('d'));
    assert!(app.devices.menu.is_none());

    let mut app = remote_app();
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('d'));
    assert!(app.devices.menu.is_none());
    assert_eq!(app.search.query(), "d");
}

#[test]
fn the_open_menu_asks_again_only_every_few_seconds() {
    let (mut app, rx) = menu_app(remote_app());
    let opened = app.devices.menu.as_ref().unwrap().asked_at;
    app.refresh_devices_if_due(opened + Duration::from_secs(1));
    assert!(rx.try_iter().next().is_none());
    app.refresh_devices_if_due(opened + DEVICES_REFRESH);
    assert!(matches!(rx.try_recv(), Ok(PlayerRequest::Devices)));
    // Closed, it asks for nothing.
    app.devices.menu = None;
    app.refresh_devices_if_due(opened + DEVICES_REFRESH * 10);
    assert!(rx.try_iter().next().is_none());
}

fn menu_text(app: &App) -> String {
    let mut term = Terminal::new(TestBackend::new(1, 1)).expect("test terminal");
    draw(&mut term, app, 100, 30);
    screen_text(&term)
}

#[test]
fn the_menu_shows_each_device_and_greys_what_cant_be_driven() {
    let (app, _rx) = menu_app(remote_app());
    let text = menu_text(&app);
    assert!(text.contains("myx is the remote for Pixel 7"), "{text}");
    let row = |name: &str| {
        text.lines()
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("no {name} row in {text}"))
            .to_string()
    };
    // (The title, the status line and the footer name the phone too.)
    assert!(row("this device").contains("myx · test"));
    assert!(row("▶ Pixel 7").contains("phone ·  40%"));
    assert!(row("Kitchen Sonos").contains("can't control ·  20%"));
}

#[test]
fn with_no_other_device_the_menu_says_where_they_went() {
    let mut app = test_app();
    let _rx = watch_player(&mut app);
    app.open_devices();
    assert!(menu_text(&app).contains("looking for your other devices"));
    apply_device_list(&mut app, Ok(Vec::new()));
    let text = menu_text(&app);
    assert!(text.contains("open Spotify on your phone"), "{text}");
    // A failed ask is on the status line and in the menu.
    apply_device_list(&mut app, Err("HTTP 503".into()));
    assert!(menu_text(&app).contains("couldn't list devices: HTTP 503"));
    assert!(app.status.contains("HTTP 503"), "{:?}", app.status);
}

#[test]
fn with_nothing_playing_anywhere_this_myx_plays_what_was_on_screen() {
    let mut app = playing_app();
    app.transport.playback_started = false;
    let rx = watch_player(&mut app);
    press(&mut app, KeyCode::Char('d'));
    let _ = rx.try_recv();
    press(&mut app, KeyCode::Enter);
    // Resumed through this myx's own player (which, detached, can't), and
    // nothing asked of the Web API.
    assert!(rx.try_iter().next().is_none());
    assert!(app.transport.playback_started);
    assert!(app.devices.menu.is_none());
}

#[test]
fn the_push_marks_which_device_plays() {
    let (mut app, _rx) = menu_app(remote_app());
    let mut tv = phone("spotify:track:0", true);
    tv.device = Device {
        id: "t".into(),
        name: "TV".into(),
        kind: DeviceKind::Tv,
        ..phone_device()
    };
    apply(&mut app, push(Holder::Elsewhere(tv), 5));
    let rows = app.device_rows();
    let active: Vec<&str> = rows
        .iter()
        .filter(|d| d.is_active)
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(active, ["TV"]);
    assert_eq!(app.devices.remote.as_ref().unwrap().name, "TV");
    apply(&mut app, push(Holder::Here, 6));
    let rows = app.device_rows();
    assert!(rows.iter().find(|d| d.is_this).unwrap().is_active);
    assert_eq!(rows.iter().filter(|d| d.is_active).count(), 1);
}

#[test]
fn remote_mode_never_panics_at_any_size() {
    let mut app = remote_app();
    apply_device_list(&mut app, Ok(listed()));
    sweep(app, "remote");
}

#[test]
fn remote_mode_says_theres_no_sound_here() {
    let mut app = remote_app();
    app.view.mode = RightView::NowPlaying;
    let mut term = Terminal::new(TestBackend::new(1, 1)).expect("test terminal");
    draw(&mut term, &app, 120, 48);
    let text = screen_text(&term);
    assert!(
        text.contains("playing on Pixel 7 · no sound here"),
        "{text}"
    );
    assert!(text.contains("d on Pixel 7"), "{text}");
    assert!(text.contains(" 40%"), "the phone's volume: {text}");
}
