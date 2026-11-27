//! Spotify Connect from the app's side: the device myx is the remote for, when
//! another one holds playback, and following what it plays.
//!
//! Remote mode is `DevicesState::remote` being `Some`. Its playhead is the
//! ordinary `NowPlaying`, so Now Playing, the cover, lyrics, progress and the
//! MXC broadcast follow the remote device with nothing of their own to learn.

use crate::*;

#[derive(Default)]
pub(crate) struct DevicesState {
    /// The device holding playback while it isn't this myx. `Some` is remote
    /// mode: what's on screen is that device's.
    pub(crate) remote: Option<Remote>,
    /// The newest cluster change applied, on Spotify's clock.
    pub(crate) last_change_ms: i64,
}

/// The device remote mode drives, and its settings as last heard. Kept apart
/// from this myx's own: the phone's volume is not the volume this PC plays at
/// once playback comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Remote {
    pub(crate) device_id: String,
    pub(crate) name: String,
    /// `None`: its volume can't be set from here.
    pub(crate) volume: Option<u8>,
    pub(crate) shuffle: bool,
    pub(crate) repeat: Repeat,
}

impl Remote {
    pub(crate) fn of(p: &Playback) -> Self {
        Self {
            device_id: p.device.id.clone(),
            name: device_label(&p.device.name).to_string(),
            volume: p.device.volume,
            shuffle: p.shuffle,
            repeat: p.repeat,
        }
    }
}

/// A device's name for the screen; Connect lets a device go without one.
pub(crate) fn device_label(name: &str) -> &str {
    if name.trim().is_empty() {
        "another device"
    } else {
        name
    }
}

/// What resuming here should play once a remote device stops: its context, when
/// it's one this myx's player can load, else just the track. Kept in step with
/// the track on screen, so `resume_source` never plays one context's track out
/// of another.
pub(crate) fn remote_source(context_uri: &str) -> PlaySource {
    let loadable = ["spotify:playlist:", "spotify:album:", "spotify:artist:"]
        .iter()
        .any(|kind| context_uri.starts_with(kind))
        || (context_uri.starts_with("spotify:user:")
            && (context_uri.ends_with(":collection") || context_uri.contains(":playlist:")));
    if loadable {
        PlaySource::Context(context_uri.to_string())
    } else {
        PlaySource::None
    }
}

impl App {
    /// The volume the meter shows: the remote device's in remote mode (`None`
    /// when it can't be set from here), else this myx's.
    pub(crate) fn shown_volume(&self) -> Option<u8> {
        match &self.devices.remote {
            Some(remote) => remote.volume,
            None => Some(self.transport.volume),
        }
    }

    /// Shuffle as the footer shows it: the remote device's, or this myx's.
    pub(crate) fn shown_shuffle(&self) -> bool {
        self.devices
            .remote
            .as_ref()
            .map_or(self.transport.shuffle, |remote| remote.shuffle)
    }

    /// Repeat as the footer shows it: the remote device's, or this myx's.
    pub(crate) fn shown_repeat(&self) -> Repeat {
        self.devices
            .remote
            .as_ref()
            .map_or_else(|| self.svc.engine.repeat_mode(), |remote| remote.repeat)
    }

    /// Whether anything is playing that the Queue view could show: this myx's
    /// own playback, or the remote device's (the Web API queue is the active
    /// device's either way).
    pub(crate) fn playing_somewhere(&self) -> bool {
        self.transport.playback_started || self.devices.remote.is_some()
    }
}

/// Apply what a cluster push, or `GET /me/player`, says about who plays what.
pub(crate) fn apply_snapshot(app: &mut App, snap: Snapshot, meta_tx: &flume::Sender<TrackMeta>) {
    if snap.changed_ms > 0 {
        // Pushes come in order over one socket; this only drops one overtaken
        // on the way, which would flip the mode back for a moment.
        if snap.changed_ms < app.devices.last_change_ms {
            return;
        }
        app.devices.last_change_ms = snap.changed_ms;
    }
    match snap.holder {
        Holder::Elsewhere(p) => follow_remote(app, p, meta_tx),
        Holder::Here => leave_remote(app, false),
        Holder::Nobody => leave_remote(app, true),
    }
}

/// Another device holds playback: drive it, and show what it plays.
fn follow_remote(app: &mut App, p: Playback, meta_tx: &flume::Sender<TrackMeta>) {
    let remote = Remote::of(&p);
    let changed_device = app
        .devices
        .remote
        .as_ref()
        .is_none_or(|r| r.device_id != remote.device_id);
    if changed_device {
        liblog(format!("connect: following {}", remote.name));
        app.status = format!("playing on {} — myx is its remote", remote.name);
        // This myx's player stopped when the other device took over (Spirc's
        // `handle_cluster_update`), so nothing is loaded here any more: the
        // first play press back here has to load, not toggle. Nor is there
        // anything of ours to resume after a dropped connection.
        app.transport.playback_started = false;
        app.transport.resume_after_reconnect = false;
    }
    app.devices.remote = Some(remote);

    let new_track = !p.track_uri.is_empty()
        && app
            .playback
            .now
            .as_ref()
            .is_none_or(|n| n.uri != p.track_uri);
    if new_track {
        let fetching = request_track_meta(app, &p.track_uri, meta_tx);
        // Until the metadata lands, the last track's title stays up (as it
        // does locally), under the new uri so the next push doesn't ask
        // again. With none on screen, or none coming (an episode), the
        // progress still shows.
        if fetching && app.playback.now.is_some() {
            if let Some(n) = app.playback.now.as_mut() {
                n.uri = p.track_uri.clone();
            }
        } else {
            app.playback.now = Some(NowPlaying {
                uri: p.track_uri.clone(),
                title: String::new(),
                artist: String::new(),
                album: String::new(),
                duration_ms: p.duration_ms,
                position_ms: 0,
                position_at: Instant::now(),
                is_playing: p.is_playing,
                cover: None,
            });
        }
        app.transport.source = remote_source(&p.context_uri);
        app.transport.source_name = p.context_name.clone();
        // The old queue belongs to the old source; the next sync fetches the
        // remote device's.
        app.transport.queue.clear();
        app.transport.queue_uris.clear();
    }
    if let Some(n) = app.playback.now.as_mut() {
        n.is_playing = p.is_playing;
        if p.duration_ms > 0 {
            n.duration_ms = p.duration_ms;
        }
    }
    app.playback.set_local_position(p.position_ms, true);
    if let Some(controls) = app.media_controls.as_mut() {
        let progress = Some(MediaPosition(Duration::from_millis(u64::from(
            app.playback.position_ms(),
        ))));
        let _ = controls.set_playback(if p.is_playing {
            MediaPlayback::Playing { progress }
        } else {
            MediaPlayback::Paused { progress }
        });
    }
}

/// Back to driving this myx's own player. `stopped`: the remote device went
/// away (closed, asleep, signed out) and nothing plays anywhere; otherwise this
/// myx took over, and librespot's own events carry on from here.
pub(crate) fn leave_remote(app: &mut App, stopped: bool) {
    let Some(remote) = app.devices.remote.take() else {
        return;
    };
    liblog(format!(
        "connect: no longer following {} ({})",
        remote.name,
        if stopped {
            "it stopped"
        } else {
            "playing here"
        }
    ));
    if stopped {
        // Its track stays on screen, paused, and the source with it: the first
        // play press picks it up here, where it left off.
        app.playback.freeze();
        app.status = format!("{} stopped — ␣ plays it here", remote.name);
        if let Some(controls) = app.media_controls.as_mut() {
            let _ = controls.set_playback(MediaPlayback::Paused {
                progress: Some(MediaPosition(Duration::from_millis(u64::from(
                    app.playback.position_ms(),
                )))),
            });
        }
    } else {
        app.status = "playing here".to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resuming_here_replays_contexts_this_player_can_load() {
        for uri in [
            "spotify:playlist:1",
            "spotify:album:1",
            "spotify:artist:1",
            "spotify:user:me:collection",
            "spotify:user:me:playlist:1",
        ] {
            assert!(
                matches!(remote_source(uri), PlaySource::Context(ref c) if c == uri),
                "{uri}"
            );
        }
        // A station, a bare track list, nothing: just the track.
        for uri in ["spotify:station:track:1", "", "spotify:track:1"] {
            assert!(matches!(remote_source(uri), PlaySource::None), "{uri}");
        }
    }

    #[test]
    fn a_device_without_a_name_still_has_one_on_screen() {
        assert_eq!(device_label("Pixel"), "Pixel");
        assert_eq!(device_label("  "), "another device");
    }
}
