//! Transport commands, sent to whichever device holds playback: this myx's own
//! player, or — in remote mode — the device it's the remote for, through the
//! Web API (`api::player` does the HTTP).
//!
//! The split is the point. This myx's player claims the active-device role
//! before every command (`Engine::active_command`), which on a remote's
//! keypress would pull playback off the phone and onto the PC. Here, nothing
//! reaches the engine while another device holds playback.

use crate::*;

/// What a key asks of a remote device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlayerCommand {
    Play,
    Pause,
    Next,
    Previous,
    Seek(u32),
    /// Percent.
    Volume(u8),
    Shuffle(bool),
    Repeat(Repeat),
    /// Start something new there.
    Start(Load),
    /// Move playback there, and keep it playing.
    Transfer,
}

impl PlayerCommand {
    /// What it does, for "couldn't … on <device>".
    pub(crate) fn verb(&self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Next => "skip",
            Self::Previous => "go back",
            Self::Seek(_) => "seek",
            Self::Volume(_) => "set the volume",
            Self::Shuffle(_) => "switch shuffle",
            Self::Repeat(_) => "switch repeat",
            Self::Start(_) => "start playback",
            Self::Transfer => "move playback",
        }
    }
}

/// Something to start playing: a context, at a track and position or from the
/// top, or a list of tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Load {
    Context {
        uri: String,
        track: Option<String>,
        position_ms: u32,
    },
    Tracks {
        uris: Vec<String>,
        start: Option<String>,
        position_ms: u32,
    },
}

/// A job for the Web API player worker, which does them one at a time, in
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlayerRequest {
    Command {
        device_id: String,
        /// For what the status line says about it.
        device_name: String,
        command: PlayerCommand,
    },
    /// `GET /me/player`: who plays what. The cluster push only says when
    /// something changes, so a fresh connection asks.
    State,
    /// `GET /me/player/devices`, for the devices menu.
    Devices,
}

/// What the worker found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlayerReply {
    State(Result<Option<Playback>, String>),
    Devices(Result<Vec<Device>, String>),
    /// For the status line: a command that failed, or one worth confirming.
    Status(String),
}

impl App {
    /// Send `command` to the device remote mode drives. `None` outside remote
    /// mode: the caller drives this myx's player instead. `Some(Err)` names a
    /// device that can't be driven from here.
    fn command_remote(&self, command: PlayerCommand) -> Option<Result<(), String>> {
        let remote = self.devices.remote.as_ref()?;
        if !remote.controllable {
            return Some(Err(format!(
                "{} can't be controlled from here",
                remote.name
            )));
        }
        // The worker lives as long as the app; a send can only fail in a
        // test that didn't keep the receiver.
        let _ = self.svc.player.send(PlayerRequest::Command {
            device_id: remote.device_id.clone(),
            device_name: remote.name.clone(),
            command,
        });
        Some(Ok(()))
    }

    /// Space: play or pause. Here, the first press after launch (or after a
    /// remote stopped) loads the saved source rather than toggling nothing.
    pub(crate) fn play_pause(&mut self, radio_tx: &flume::Sender<Result<Radio, String>>) {
        if self.devices.remote.is_some() {
            let playing = self.playback.now.as_ref().is_some_and(|n| n.is_playing);
            let command = if playing {
                PlayerCommand::Pause
            } else {
                PlayerCommand::Play
            };
            self.remote_play_state(command, !playing);
        } else if self.transport.playback_started {
            let _ = self.svc.engine.toggle();
        } else {
            // Resume the persisted source (context/radio/liked).
            resume_source(self, radio_tx);
            self.transport.playback_started = true;
        }
    }

    pub(crate) fn play(&mut self, radio_tx: &flume::Sender<Result<Radio, String>>) {
        if self.devices.remote.is_some() {
            self.remote_play_state(PlayerCommand::Play, true);
        } else if self.transport.playback_started {
            let _ = self.svc.engine.play();
        } else {
            resume_source(self, radio_tx);
            self.transport.playback_started = true;
        }
    }

    pub(crate) fn pause(&mut self) {
        if self.devices.remote.is_some() {
            self.remote_play_state(PlayerCommand::Pause, false);
        } else {
            let _ = self.svc.engine.pause();
        }
    }

    /// Stop. A remote device is paused instead: the Web API has no stop.
    pub(crate) fn stop(&mut self) {
        if self.devices.remote.is_some() {
            self.remote_play_state(PlayerCommand::Pause, false);
        } else {
            self.svc.engine.stop();
        }
    }

    /// Play or pause the remote device, and show it straight away; the push
    /// that follows corrects anything the device did otherwise.
    fn remote_play_state(&mut self, command: PlayerCommand, playing: bool) {
        match self.command_remote(command) {
            Some(Ok(())) => self.playback.set_playing(playing),
            Some(Err(e)) => self.status = e,
            None => {}
        }
    }

    pub(crate) fn next(&mut self) {
        match self.command_remote(PlayerCommand::Next) {
            None => {
                let _ = self.svc.engine.next();
            }
            Some(Err(e)) => self.status = e,
            Some(Ok(())) => {}
        }
    }

    pub(crate) fn prev(&mut self) {
        match self.command_remote(PlayerCommand::Previous) {
            None => {
                let _ = self.svc.engine.prev();
            }
            Some(Err(e)) => self.status = e,
            Some(Ok(())) => {}
        }
    }

    /// Set the volume, in percent: the remote device's, or this myx's.
    pub(crate) fn set_volume(&mut self, pct: u8) {
        let pct = pct.min(100);
        let Some(remote) = self.devices.remote.as_ref() else {
            self.transport.volume = pct;
            let _ = self.svc.engine.set_volume(vol_u16(pct));
            return;
        };
        if remote.volume.is_none() {
            self.status = format!("{}'s volume can't be set from here", remote.name);
            return;
        }
        match self.command_remote(PlayerCommand::Volume(pct)) {
            Some(Ok(())) => {
                if let Some(remote) = self.devices.remote.as_mut() {
                    remote.volume = Some(pct);
                }
            }
            Some(Err(e)) => self.status = e,
            None => {}
        }
    }

    /// `+` / `-` and the scroll wheel: the volume by `delta` percent.
    pub(crate) fn nudge_volume(&mut self, delta: i8) {
        let from = self.shown_volume().unwrap_or(0);
        self.set_volume(from.saturating_add_signed(delta));
    }

    /// Shuffle on or off: the remote device's, or this myx's.
    pub(crate) fn set_shuffle(&mut self, on: bool) {
        match self.command_remote(PlayerCommand::Shuffle(on)) {
            None => {
                self.transport.shuffle = on;
                let _ = self.svc.engine.shuffle(on);
            }
            Some(Ok(())) => {
                if let Some(remote) = self.devices.remote.as_mut() {
                    remote.shuffle = on;
                }
            }
            Some(Err(e)) => self.status = e,
        }
    }

    /// Switch repeat: the remote device's, or this myx's — whose mode is kept
    /// for the next load even when the player can't be told.
    pub(crate) fn set_repeat(&mut self, mode: Repeat) -> anyhow::Result<()> {
        match self.command_remote(PlayerCommand::Repeat(mode)) {
            None => self.svc.engine.set_repeat(mode),
            Some(Ok(())) => {
                if let Some(remote) = self.devices.remote.as_mut() {
                    remote.repeat = mode;
                }
                Ok(())
            }
            Some(Err(e)) => Err(anyhow::anyhow!(e)),
        }
    }

    /// Seek to an absolute position (clamped to the track), on screen at once.
    pub(crate) fn seek_to(&mut self, position_ms: u32) {
        let Some(target) = self.playback.seek_to(position_ms) else {
            return;
        };
        match self.command_remote(PlayerCommand::Seek(target)) {
            None => {
                let _ = self.svc.engine.seek(target);
            }
            Some(Err(e)) => self.status = e,
            Some(Ok(())) => {}
        }
    }

    /// Commit a finished Shift+arrow scrub as one seek, once the keys stop.
    pub(crate) fn flush_seek(&mut self, now: Instant) {
        if let Some(target) = self.playback.settled_seek(now) {
            self.seek_to(target);
        }
    }

    /// Start `load` on whichever device holds playback; returns whether it
    /// went. `shuffle` is for this myx's player, which takes it with the load;
    /// a remote device keeps its own setting (`s` switches it).
    pub(crate) fn play_load(&mut self, load: Load, shuffle: bool) -> bool {
        if self.devices.remote.is_some() {
            return match self.command_remote(PlayerCommand::Start(load)) {
                Some(Err(e)) => {
                    self.status = e;
                    false
                }
                _ => true,
            };
        }
        let engine = &self.svc.engine;
        let result = match load {
            Load::Context {
                uri,
                track,
                position_ms,
            } => engine.play_context_at(uri, track, position_ms, shuffle),
            Load::Tracks {
                uris,
                start,
                position_ms,
            } => engine.play_tracks(uris, start, position_ms, shuffle),
        };
        match result {
            Ok(()) => true,
            Err(e) => {
                self.status = format!("couldn't play: {e:#}");
                false
            }
        }
    }

    /// Ask the Web API who plays what, for when no push will say so soon.
    pub(crate) fn refresh_player_state(&self) {
        let _ = self.svc.player.send(PlayerRequest::State);
    }
}

/// Apply what the player worker found.
pub(crate) fn handle_player_reply(
    app: &mut App,
    reply: PlayerReply,
    meta_tx: &flume::Sender<TrackMeta>,
) {
    match reply {
        PlayerReply::Status(msg) => app.status = msg,
        PlayerReply::State(Ok(active)) => {
            let holder = Holder::of(active, &app.devices.here_id);
            let snap = Snapshot {
                holder,
                devices: None,
                changed_ms: 0,
            };
            apply_snapshot(app, snap, meta_tx);
        }
        PlayerReply::State(Err(e)) => {
            liblog(format!("player: couldn't ask who's playing: {e}"));
            app.status = format!("couldn't check your other devices: {e}");
        }
        PlayerReply::Devices(listed) => apply_device_list(app, listed),
    }
}
