//! Who holds playback on this Spotify account, and what it's playing — read
//! off Spotify Connect's cluster push, which every Connect device receives.
//!
//! Spirc already listens on `hm://connect-state/v1/cluster` to notice another
//! device taking over. The dealer hands each message to every subscriber of
//! that address (`SubscriberMap::retain` sends to all of them, keeping the ones
//! still listening), so a second listener here reads the same push without
//! taking anything from Spirc. That is what lets myx follow a phone it isn't
//! playing for, with no polling.
//!
//! Turning a push into a [`Snapshot`] is pure; only the feed needs a session.

use std::future::poll_fn;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use librespot_core::dealer::manager::BoxedStreamResult;
use librespot_core::dealer::protocol::Message;
use librespot_core::Session;
use librespot_protocol::connect::{ClusterUpdate, DeviceInfo};
use librespot_protocol::devices::DeviceType;
use librespot_protocol::player::PlayerState;

use super::{EngineEvent, Repeat};
use crate::liblog::liblog;

const CLUSTER_URI: &str = "hm://connect-state/v1/cluster";

/// What sort of device it is, as the devices menu names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Computer,
    Phone,
    Tablet,
    Speaker,
    Tv,
    Car,
    Console,
    Cast,
    Watch,
    Other,
}

impl DeviceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Computer => "computer",
            Self::Phone => "phone",
            Self::Tablet => "tablet",
            Self::Speaker => "speaker",
            Self::Tv => "TV",
            Self::Car => "car",
            Self::Console => "console",
            Self::Cast => "cast",
            Self::Watch => "watch",
            Self::Other => "device",
        }
    }

    /// From the type a Connect device announces itself with.
    pub fn from_connect(kind: DeviceType) -> Self {
        match kind {
            DeviceType::COMPUTER | DeviceType::CHROMEBOOK => Self::Computer,
            DeviceType::SMARTPHONE => Self::Phone,
            DeviceType::TABLET => Self::Tablet,
            DeviceType::SPEAKER | DeviceType::AVR | DeviceType::HOME_THING => Self::Speaker,
            DeviceType::TV | DeviceType::STB => Self::Tv,
            DeviceType::AUTOMOBILE | DeviceType::CAR_THING => Self::Car,
            DeviceType::GAME_CONSOLE => Self::Console,
            DeviceType::CAST_AUDIO | DeviceType::CAST_VIDEO | DeviceType::AUDIO_DONGLE => {
                Self::Cast
            }
            DeviceType::SMARTWATCH => Self::Watch,
            _ => Self::Other,
        }
    }

    /// From the Web API's `type`: `Computer`, `Smartphone`, `CastAudio`, …
    pub fn from_web(kind: &str) -> Self {
        match kind.to_ascii_lowercase().as_str() {
            "computer" => Self::Computer,
            "smartphone" => Self::Phone,
            "tablet" => Self::Tablet,
            "speaker" | "avr" => Self::Speaker,
            "tv" | "stb" => Self::Tv,
            "automobile" => Self::Car,
            "gameconsole" => Self::Console,
            "castaudio" | "castvideo" | "audiodongle" => Self::Cast,
            "smartwatch" => Self::Watch,
            _ => Self::Other,
        }
    }
}

/// One Spotify Connect device on the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    /// Percent; `None` for a device whose volume can't be set from here.
    pub volume: Option<u8>,
    pub is_active: bool,
    /// Whether playback can be sent there and the device driven from here.
    pub controllable: bool,
    /// This myx.
    pub is_this: bool,
}

/// What the active device is playing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playback {
    pub device: Device,
    /// Empty when the device has nothing loaded.
    pub track_uri: String,
    pub context_uri: String,
    /// The context's name, when the device gave one.
    pub context_name: String,
    /// The playhead when this was read: the device's last report, moved on by
    /// the time since while it plays.
    pub position_ms: u32,
    pub duration_ms: u32,
    pub is_playing: bool,
    pub shuffle: bool,
    pub repeat: Repeat,
}

/// Who holds playback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// No device is active.
    Nobody,
    /// This myx is.
    Here,
    /// Another device is, playing this.
    Elsewhere(Playback),
}

impl Holder {
    /// From the active device's playback, if there is one, and this myx's id.
    pub fn of(active: Option<Playback>, here_id: &str) -> Self {
        match active {
            None => Self::Nobody,
            Some(p) if !here_id.is_empty() && p.device.id == here_id => Self::Here,
            Some(p) => Self::Elsewhere(p),
        }
    }
}

/// One reading of who plays what: a cluster push, or the Web API's
/// `GET /me/player`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub holder: Holder,
    /// The devices a push listed, hidden ones left out. `None` from the Web
    /// API, which names only the active device.
    pub devices: Option<Vec<Device>>,
    /// When the cluster last changed, on Spotify's clock; 0 when unknown.
    pub changed_ms: i64,
}

/// What one cluster push says, read at `now_ms` (Unix time). `None` for a push
/// without a cluster, which Spirc doesn't act on either.
pub fn snapshot(update: &ClusterUpdate, here_id: &str, now_ms: i64) -> Option<Snapshot> {
    let cluster = update.cluster.as_ref()?;
    let active_id = cluster.active_device_id.as_str();
    let mut devices: Vec<Device> = cluster
        .device
        .iter()
        .filter(|(_, info)| !info.capabilities.as_ref().is_some_and(|c| c.hidden))
        .map(|(id, info)| device(id, info, active_id, here_id))
        .collect();
    // A map has no order; a fixed one keeps two equal pushes equal.
    devices.sort_by(|a, b| a.id.cmp(&b.id));

    let active = (!active_id.is_empty()).then(|| {
        let device = match cluster.device.get(active_id) {
            Some(info) => device(active_id, info, active_id, here_id),
            None => Device {
                id: active_id.to_string(),
                name: String::new(),
                kind: DeviceKind::Other,
                volume: None,
                is_active: true,
                controllable: true,
                is_this: active_id == here_id,
            },
        };
        // The server's clock when it sent this, rather than ours: the gap
        // since the device's report is then the server's to measure.
        let read_at = if cluster.server_timestamp_ms > 0 {
            cluster.server_timestamp_ms
        } else {
            now_ms
        };
        playback(cluster.player_state.get_or_default(), device, read_at)
    });
    Some(Snapshot {
        holder: Holder::of(active, here_id),
        devices: Some(devices),
        changed_ms: cluster.changed_timestamp_ms,
    })
}

fn device(id: &str, info: &DeviceInfo, active_id: &str, here_id: &str) -> Device {
    let caps = info.capabilities.as_ref();
    Device {
        id: id.to_string(),
        name: info.name.clone(),
        kind: info
            .device_type
            .enum_value()
            .map_or(DeviceKind::Other, DeviceKind::from_connect),
        volume: (!caps.is_some_and(|c| c.disable_volume)).then(|| volume_percent(info.volume)),
        is_active: id == active_id,
        // A device that says nothing about itself is given the benefit of the
        // doubt: if it refuses, the status line says so.
        controllable: caps.is_none_or(|c| c.is_controllable && !c.restrict_to_local),
        is_this: !here_id.is_empty() && id == here_id,
    }
}

fn playback(state: &PlayerState, device: Device, read_at_ms: i64) -> Playback {
    let options = state.options.get_or_default();
    // Connect keeps `is_playing` set while paused; `is_paused` is the pause.
    let is_playing = state.is_playing && !state.is_paused;
    let duration_ms = u32::try_from(state.duration.max(0)).unwrap_or(u32::MAX);
    Playback {
        device,
        track_uri: state
            .track
            .as_ref()
            .map(|t| t.uri.clone())
            .unwrap_or_default(),
        context_uri: state.context_uri.clone(),
        context_name: state
            .context_metadata
            .get("context_description")
            .cloned()
            .unwrap_or_default(),
        position_ms: playhead(
            state.position_as_of_timestamp,
            state.timestamp,
            read_at_ms,
            is_playing,
            duration_ms,
        ),
        duration_ms,
        is_playing,
        shuffle: options.shuffling_context,
        repeat: Repeat::from_flags(options.repeating_context, options.repeating_track),
    }
}

/// Where the playhead is at `now_ms`, from a device's report that it was at
/// `position_ms` at `at_ms` — what local playback does with its own reports.
/// Paused, it stays put; playing, it has moved on by the time since. Never
/// backwards (clocks disagree) and never past the end (a report can be late);
/// a duration of 0 is unknown and caps nothing.
pub fn playhead(position_ms: i64, at_ms: i64, now_ms: i64, playing: bool, duration_ms: u32) -> u32 {
    let elapsed = if playing && at_ms > 0 {
        (now_ms - at_ms).max(0)
    } else {
        0
    };
    let mut at = position_ms.max(0).saturating_add(elapsed);
    if duration_ms > 0 {
        at = at.min(i64::from(duration_ms));
    }
    u32::try_from(at).unwrap_or(u32::MAX)
}

/// Connect's 0..=65535 volume as a percent, rounded.
pub fn volume_percent(raw: u32) -> u8 {
    let raw = raw.min(u32::from(u16::MAX));
    ((raw * 100 + u32::from(u16::MAX) / 2) / u32::from(u16::MAX)) as u8
}

/// Unix time in milliseconds.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The cluster push, as a stream.
///
/// Call before `Spirc::new`, as Spirc subscribes its own listeners: the dealer
/// starts in Spirc's task, which takes the dealer's builder before it sets the
/// dealer, and a subscription made in between fails. Made first, it is on the
/// builder and carried over.
pub(super) fn subscribe(session: &Session) -> Result<BoxedStreamResult<ClusterUpdate>> {
    session
        .dealer()
        .listen_for(CLUSTER_URI, Message::from_raw::<ClusterUpdate>)
        .context("subscribe to the cluster push")
}

/// Send each push on as an [`EngineEvent::Cluster`]. Ends with the session,
/// whose dealer closes the stream; a reconnect subscribes again.
pub(super) fn spawn_feed(
    mut updates: BoxedStreamResult<ClusterUpdate>,
    here_id: String,
    events: flume::Sender<EngineEvent>,
) {
    tokio::spawn(async move {
        while let Some(update) = poll_fn(|cx| updates.as_mut().poll_next(cx)).await {
            let update = match update {
                Ok(update) => update,
                Err(e) => {
                    liblog(format!("connect: unreadable cluster push: {e}"));
                    continue;
                }
            };
            let Some(snapshot) = snapshot(&update, &here_id, now_ms()) else {
                continue;
            };
            if events.send(EngineEvent::Cluster(snapshot)).is_err() {
                break; // receiver dropped
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_protocol::connect::{Capabilities, Cluster};
    use librespot_protocol::player::{ContextPlayerOptions, ProvidedTrack};
    use protobuf::{EnumOrUnknown, MessageField};

    const HERE: &str = "aaaa";
    const PHONE: &str = "bbbb";

    fn info(name: &str, kind: DeviceType, volume: u32, caps: Capabilities) -> DeviceInfo {
        DeviceInfo {
            name: name.to_string(),
            volume,
            device_type: EnumOrUnknown::new(kind),
            capabilities: MessageField::some(caps),
            ..Default::default()
        }
    }

    fn controllable() -> Capabilities {
        Capabilities {
            is_controllable: true,
            ..Default::default()
        }
    }

    /// The phone playing track `t` from a playlist, as its push reports it.
    fn phone_playing(paused: bool) -> ClusterUpdate {
        let mut cluster = Cluster {
            active_device_id: PHONE.to_string(),
            changed_timestamp_ms: 1_000,
            server_timestamp_ms: 10_000,
            player_state: MessageField::some(PlayerState {
                track: MessageField::some(ProvidedTrack {
                    uri: "spotify:track:t".to_string(),
                    ..Default::default()
                }),
                context_uri: "spotify:playlist:p".to_string(),
                context_metadata: [("context_description".to_string(), "Mix".to_string())]
                    .into_iter()
                    .collect(),
                timestamp: 7_000,
                position_as_of_timestamp: 30_000,
                duration: 200_000,
                is_playing: true,
                is_paused: paused,
                options: MessageField::some(ContextPlayerOptions {
                    shuffling_context: true,
                    repeating_context: true,
                    repeating_track: false,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        cluster.device.insert(
            PHONE.to_string(),
            info("Pixel", DeviceType::SMARTPHONE, 32_768, controllable()),
        );
        cluster.device.insert(
            HERE.to_string(),
            info("myx · bella", DeviceType::COMPUTER, 65_535, controllable()),
        );
        ClusterUpdate {
            cluster: MessageField::some(cluster),
            ..Default::default()
        }
    }

    fn elsewhere(update: &ClusterUpdate) -> Playback {
        match snapshot(update, HERE, 99_999).expect("a cluster").holder {
            Holder::Elsewhere(p) => p,
            other => panic!("expected another device, got {other:?}"),
        }
    }

    #[test]
    fn another_device_playing_is_followed_with_what_it_plays() {
        let p = elsewhere(&phone_playing(false));
        assert_eq!(p.device.id, PHONE);
        assert_eq!(p.device.name, "Pixel");
        assert_eq!(p.device.kind, DeviceKind::Phone);
        assert_eq!(p.device.volume, Some(50));
        assert!(p.device.is_active && !p.device.is_this);
        assert_eq!(p.track_uri, "spotify:track:t");
        assert_eq!(p.context_uri, "spotify:playlist:p");
        assert_eq!(p.context_name, "Mix");
        assert_eq!(p.duration_ms, 200_000);
        assert!(p.is_playing);
        assert!(p.shuffle);
        assert_eq!(p.repeat, Repeat::All);
        // Reported at 30s at 7s; the server sent it at 10s: 33s in, measured
        // on the server's clock rather than the one passed in.
        assert_eq!(p.position_ms, 33_000);
    }

    #[test]
    fn a_paused_device_is_paused_and_its_playhead_stays_put() {
        let p = elsewhere(&phone_playing(true));
        assert!(!p.is_playing);
        assert_eq!(p.position_ms, 30_000);
    }

    #[test]
    fn without_a_server_time_the_local_clock_measures_the_gap() {
        let mut update = phone_playing(false);
        update.cluster.as_mut().unwrap().server_timestamp_ms = 0;
        let p = match snapshot(&update, HERE, 8_000).unwrap().holder {
            Holder::Elsewhere(p) => p,
            other => panic!("{other:?}"),
        };
        assert_eq!(p.position_ms, 31_000);
    }

    #[test]
    fn this_myx_active_or_nobody_is_not_remote_mode() {
        let mut update = phone_playing(false);
        update.cluster.as_mut().unwrap().active_device_id = HERE.to_string();
        assert_eq!(snapshot(&update, HERE, 0).unwrap().holder, Holder::Here);
        update.cluster.as_mut().unwrap().active_device_id.clear();
        assert_eq!(snapshot(&update, HERE, 0).unwrap().holder, Holder::Nobody);
    }

    #[test]
    fn a_push_without_a_cluster_says_nothing() {
        assert_eq!(snapshot(&ClusterUpdate::default(), HERE, 0), None);
    }

    #[test]
    fn the_push_lists_devices_marking_this_one_and_the_active_one() {
        let snap = snapshot(&phone_playing(false), HERE, 0).unwrap();
        let devices = snap.devices.expect("a push lists devices");
        assert_eq!(devices.len(), 2);
        let here = devices.iter().find(|d| d.id == HERE).unwrap();
        assert!(here.is_this && !here.is_active);
        assert_eq!(here.volume, Some(100));
        let phone = devices.iter().find(|d| d.id == PHONE).unwrap();
        assert!(phone.is_active && !phone.is_this);
        assert_eq!(snap.changed_ms, 1_000);
    }

    #[test]
    fn hidden_devices_are_left_out_and_restricted_ones_cannot_be_driven() {
        let mut update = phone_playing(false);
        let cluster = update.cluster.as_mut().unwrap();
        let hidden = Capabilities {
            hidden: true,
            is_controllable: true,
            ..Default::default()
        };
        let observer = Capabilities::default(); // is_controllable: false
        let local_only = Capabilities {
            is_controllable: true,
            restrict_to_local: true,
            ..Default::default()
        };
        let fixed_volume = Capabilities {
            is_controllable: true,
            disable_volume: true,
            ..Default::default()
        };
        cluster
            .device
            .insert("h".into(), info("Ghost", DeviceType::OBSERVER, 0, hidden));
        cluster
            .device
            .insert("o".into(), info("Web", DeviceType::COMPUTER, 0, observer));
        cluster.device.insert(
            "l".into(),
            info("Car", DeviceType::AUTOMOBILE, 0, local_only),
        );
        cluster.device.insert(
            "f".into(),
            info("Speaker", DeviceType::SPEAKER, 9_000, fixed_volume),
        );
        let devices = snapshot(&update, HERE, 0).unwrap().devices.unwrap();
        let by_id = |id: &str| devices.iter().find(|d| d.id == id);
        assert!(by_id("h").is_none(), "hidden devices stay hidden");
        assert!(!by_id("o").unwrap().controllable);
        assert!(!by_id("l").unwrap().controllable);
        let speaker = by_id("f").unwrap();
        assert!(speaker.controllable);
        assert_eq!(speaker.volume, None);
    }

    #[test]
    fn an_active_device_missing_from_the_map_is_still_followed() {
        let mut update = phone_playing(false);
        update.cluster.as_mut().unwrap().device.remove(PHONE);
        let p = elsewhere(&update);
        assert_eq!(p.device.id, PHONE);
        assert!(p.device.controllable);
    }

    #[test]
    fn the_playhead_never_runs_backwards_or_past_the_end() {
        // Paused: wherever it was.
        assert_eq!(playhead(5_000, 1_000, 9_000, false, 60_000), 5_000);
        // Playing: on by the gap.
        assert_eq!(playhead(5_000, 1_000, 3_000, true, 60_000), 7_000);
        // A clock behind the report's: not backwards.
        assert_eq!(playhead(5_000, 9_000, 3_000, true, 60_000), 5_000);
        // A late report: not past the end.
        assert_eq!(playhead(5_000, 1_000, 900_000, true, 60_000), 60_000);
        // No timestamp, or no duration: taken as it is.
        assert_eq!(playhead(5_000, 0, 9_000, true, 60_000), 5_000);
        assert_eq!(playhead(5_000, 1_000, 3_000, true, 0), 7_000);
        assert_eq!(playhead(-40, 0, 0, false, 0), 0);
    }

    #[test]
    fn connect_volume_reads_as_a_percent() {
        assert_eq!(volume_percent(0), 0);
        assert_eq!(volume_percent(65_535), 100);
        assert_eq!(volume_percent(32_768), 50);
        assert_eq!(volume_percent(u32::MAX), 100);
    }

    #[test]
    fn device_kinds_read_the_same_from_connect_and_the_web_api() {
        for (connect, web, kind) in [
            (DeviceType::COMPUTER, "Computer", DeviceKind::Computer),
            (DeviceType::SMARTPHONE, "Smartphone", DeviceKind::Phone),
            (DeviceType::TABLET, "Tablet", DeviceKind::Tablet),
            (DeviceType::SPEAKER, "Speaker", DeviceKind::Speaker),
            (DeviceType::TV, "TV", DeviceKind::Tv),
            (DeviceType::AUTOMOBILE, "Automobile", DeviceKind::Car),
            (DeviceType::GAME_CONSOLE, "GameConsole", DeviceKind::Console),
            (DeviceType::CAST_AUDIO, "CastAudio", DeviceKind::Cast),
            (DeviceType::UNKNOWN, "Unknown", DeviceKind::Other),
        ] {
            assert_eq!(DeviceKind::from_connect(connect), kind, "{connect:?}");
            assert_eq!(DeviceKind::from_web(web), kind, "{web}");
        }
    }
}
