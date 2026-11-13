//! The librespot streaming engine.
//!
//! Authenticates, brings up a Spotify Connect device (Spirc) with our local EQ +
//! tee'd FFT sinks, and bridges librespot's player events into a clean [`EngineEvent`]
//! stream. This is what makes "track change" *real*: when Spotify hands us a new
//! track, a `TrackChanged` lands on the channel — the hook the reactive theme +
//! cover fade will fire from.
//!
//! Session/Spirc wiring mirrors aome510/spotify-player (`streaming.rs`, MIT,
//! © 2021 Thang Pham), stripped to just what myx needs.

pub mod auth;
mod playlist;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use librespot_connect::{
    ConnectConfig, LoadContextOptions, LoadRequest, LoadRequestOptions, Options as CtxOptions,
    PlayingTrack, Spirc,
};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::DeviceType;
use librespot_core::{Session, SessionConfig};
use librespot_playback::audio_backend::{self, Sink};
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::softmixer::SoftMixer;
use librespot_playback::mixer::{Mixer, MixerConfig};
use librespot_playback::player::{self, Player};

use crate::audio::equalizer::{shared_equalizer, EqualizerControl, EqualizerSink};
use crate::audio::output::{output_available, SilentSink};
use crate::audio::{EqualizerSettings, VisBands, VisualizationSink};
pub use playlist::{playlist_tracks, PlaylistTrack};

/// A normalized playback event surfaced to the rest of the app.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A new track became current — carries its Spotify URI. This is the reactive
    /// theme trigger.
    TrackChanged {
        uri: String,
    },
    Playing {
        uri: String,
        position_ms: u32,
    },
    Paused {
        uri: String,
        position_ms: u32,
    },
    Stopped,
    /// The access point went away and a replacement connection is being built.
    Reconnecting,
    /// Playback control works again. Whatever was playing is not resumed — the
    /// new Connect device starts idle.
    Reconnected,
    /// No audio output device: playing silently instead of crashing. See
    /// [`crate::audio::output`].
    NoAudioOutput,
    EndOfTrack {
        uri: String,
    },
    PositionCorrection {
        uri: String,
        position_ms: u32,
    },
}

/// Spotify's repeat modes, in the order its repeat button steps through them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Repeat {
    #[default]
    Off,
    /// The playlist, album or list starts over when it runs out.
    All,
    /// The current track plays again until it is skipped.
    One,
}

impl Repeat {
    /// The mode after this one: off → all → one → off, like Spotify's button.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    /// librespot's `(repeat context, repeat track)` flags. `One` sets both, so
    /// a skip — which clears the track flag — leaves the list repeating rather
    /// than turning repeat off.
    pub fn flags(self) -> (bool, bool) {
        (self != Self::Off, self == Self::One)
    }

    /// The mode for a pair of [`Self::flags`].
    pub fn from_flags(context: bool, track: bool) -> Self {
        match (context, track) {
            (_, true) => Self::One,
            (true, false) => Self::All,
            (false, false) => Self::Off,
        }
    }

    /// What a skip leaves behind. librespot stops repeating the track on any
    /// skip (`ConnectState::next_track`) and keeps repeating the list.
    pub fn after_skip(self) -> Self {
        match self {
            Self::One => Self::All,
            other => other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }
}

/// Everything that dies with the access-point connection.
///
/// librespot invalidates the session when its keep-alive ping goes unanswered
/// and documents that one "cannot yet be reused once invalidated", so coming
/// back from an idle spell means building a fresh set of these rather than
/// repairing the old ones.
struct Link {
    spirc: Spirc,
    player: Arc<Player>,
    session: Session,
}

/// A running engine: keep it alive (dropping it tears down the Connect device),
/// and read `bands` for the live visualizer.
pub struct Engine {
    pub bands: Arc<Mutex<VisBands>>,
    /// `None` only for [`Engine::detached`]: no connection, every command a no-op.
    inner: Option<Arc<Inner>>,
    /// The repeat mode, which every load carries. librespot clears repeat on
    /// each load (`handle_load` calls `reset_options`), so setting it once
    /// lasts only until the next playlist, album or track is started.
    repeat: Mutex<Repeat>,
}

/// The half of the engine the reconnect watchdog needs to hold, so it can swap
/// the connection out from under the `Engine` the UI is using.
struct Inner {
    link: Mutex<Arc<Link>>,
    mixer: Arc<SoftMixer>,
    bands: Arc<Mutex<VisBands>>,
    equalizer: EqualizerControl,
    events: flume::Sender<EngineEvent>,
    /// First-login credentials, used only until librespot has cached reusable
    /// ones: a first run authenticates with a one-shot OAuth token that a
    /// relogin hours later would be refused.
    creds: Credentials,
}

impl Inner {
    fn link(&self) -> Arc<Link> {
        Arc::clone(&self.link.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Whether the access point has gone away and every command would now fail
    /// with a closed channel.
    fn is_stale(&self) -> bool {
        self.link().session.is_invalid()
    }

    /// Build a replacement connection and swap it in. The dead one is dropped
    /// afterwards, which stops its player and ends its event bridge.
    async fn reconnect(&self) -> Result<()> {
        let fresh = Arc::new(
            connect(
                &self.creds,
                &self.mixer,
                &self.bands,
                &self.equalizer,
                &self.events,
            )
            .await?,
        );
        let dead = std::mem::replace(
            &mut *self.link.lock().unwrap_or_else(PoisonError::into_inner),
            fresh,
        );
        drop(dead);
        Ok(())
    }
}

impl Engine {
    /// An engine with no connection: commands fail softly, nothing plays. For
    /// tests that need a whole `App` (drawing every screen at every size)
    /// without logging in to Spotify.
    #[cfg(feature = "test-support")]
    pub fn detached() -> Self {
        Self {
            bands: Arc::new(Mutex::new(VisBands::new())),
            inner: None,
            repeat: Mutex::new(Repeat::Off),
        }
    }

    fn link(&self) -> Result<Arc<Link>> {
        self.inner
            .as_ref()
            .map(|inner| inner.link())
            .ok_or_else(|| anyhow!("not connected"))
    }

    /// Fetch a fresh Web API access token off the librespot session (login5).
    /// Used for track metadata + cover art lookups.
    pub async fn web_token(&self) -> Result<String> {
        let session = self.link()?.session.clone();
        let fut = session.login5().auth_token();
        let token = tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .map_err(|_| anyhow!("timed out fetching web token"))?
            .map_err(|e| anyhow!("web token error: {e:?}"))?;
        Ok(token.access_token)
    }

    /// Run a Spirc command, translating the failure that follows a dropped
    /// access point into something the status line can explain. The watchdog is
    /// already building a replacement; the key just has to be pressed again.
    fn command<T>(
        &self,
        what: &'static str,
        f: impl FnOnce(&Spirc) -> Result<T, librespot_core::Error>,
    ) -> Result<T> {
        let link = self.link()?;
        f(&link.spirc).map_err(|e| {
            if link.session.is_invalid() {
                anyhow!("connection dropped — reconnecting, try again in a moment")
            } else {
                anyhow!("{what}: {e}")
            }
        })
    }

    /// A [`Self::command`] that first reclaims the active-device role, which
    /// Spotify revokes from an idle paused device. Once revoked, librespot
    /// discards every other command; `Activate` is the one it still honours,
    /// and it no-ops when we are already active. Volume stays out of here — it
    /// must never yank playback off whatever device actually holds it.
    fn active_command<T>(
        &self,
        what: &'static str,
        f: impl FnOnce(&Spirc) -> Result<T, librespot_core::Error>,
    ) -> Result<T> {
        if let Ok(link) = self.link() {
            let _ = link.spirc.activate();
        }
        self.command(what, f)
    }

    /// Start playing a context (playlist / album / artist / track URI). When
    /// `shuffle` is set, Spotify shuffles the *entire* context server-side.
    pub fn play_context(&self, context_uri: impl Into<String>, shuffle: bool) -> Result<()> {
        let options = LoadRequestOptions {
            start_playing: true,
            context_options: context_options(shuffle, self.repeat_mode()),
            ..Default::default()
        };
        self.active_command("load context", |spirc| {
            spirc.load(LoadRequest::from_context_uri(context_uri.into(), options))
        })
    }

    /// Load a context and start at a specific track + position (context resume).
    pub fn play_context_at(
        &self,
        context_uri: String,
        track_uri: Option<String>,
        position_ms: u32,
        shuffle: bool,
    ) -> Result<()> {
        let options = LoadRequestOptions {
            start_playing: true,
            seek_to: position_ms,
            context_options: context_options(shuffle, self.repeat_mode()),
            playing_track: track_uri.map(PlayingTrack::Uri),
        };
        self.active_command("load context at", |spirc| {
            spirc.load(LoadRequest::from_context_uri(context_uri, options))
        })
    }

    /// Play an explicit list of track URIs as a context. `start_uri` picks the
    /// first track (ignored under shuffle); `shuffle` shuffles the whole list
    /// server-side — so shuffling Liked Songs covers *every* track we pass in.
    pub fn play_tracks(
        &self,
        tracks: Vec<String>,
        start_uri: Option<String>,
        start_position_ms: u32,
        shuffle: bool,
    ) -> Result<()> {
        let options = LoadRequestOptions {
            start_playing: true,
            context_options: context_options(shuffle, self.repeat_mode()),
            playing_track: start_uri.map(PlayingTrack::Uri),
            seek_to: start_position_ms,
        };
        self.active_command("load tracks", |spirc| {
            spirc.load(LoadRequest::from_tracks(tracks, options))
        })
    }

    /// Load a single track and start playing at `position_ms` — used to resume
    /// the last session's track when the user first hits play.
    pub fn play_track_at(&self, uri: String, position_ms: u32) -> Result<()> {
        let options = LoadRequestOptions {
            start_playing: true,
            seek_to: position_ms,
            context_options: context_options(false, self.repeat_mode()),
            ..Default::default()
        };
        self.active_command("resume track", |spirc| {
            spirc.load(LoadRequest::from_tracks(vec![uri], options))
        })
    }

    pub fn play(&self) -> Result<()> {
        self.active_command("play", Spirc::play)
    }
    pub fn pause(&self) -> Result<()> {
        self.active_command("pause", Spirc::pause)
    }
    pub fn stop(&self) {
        if let Ok(link) = self.link() {
            link.player.stop()
        }
    }
    pub fn toggle(&self) -> Result<()> {
        self.active_command("toggle", Spirc::play_pause)
    }
    pub fn next(&self) -> Result<()> {
        self.active_command("next", Spirc::next)?;
        // librespot has just dropped repeat-one; follow it, or the next load
        // would turn it back on.
        let mut repeat = self.repeat.lock().unwrap_or_else(PoisonError::into_inner);
        *repeat = repeat.after_skip();
        Ok(())
    }
    pub fn prev(&self) -> Result<()> {
        self.active_command("prev", Spirc::prev)
    }
    pub fn shuffle(&self, on: bool) -> Result<()> {
        self.active_command("shuffle", |spirc| spirc.shuffle(on))
    }
    /// The repeat mode now, and the one the next load starts with.
    pub fn repeat_mode(&self) -> Repeat {
        *self.repeat.lock().unwrap_or_else(PoisonError::into_inner)
    }
    /// Switch repeat for what's playing and for everything loaded after it. The
    /// mode is kept even when the player can't be reached, so the next load
    /// still honours it.
    pub fn set_repeat(&self, mode: Repeat) -> Result<()> {
        *self.repeat.lock().unwrap_or_else(PoisonError::into_inner) = mode;
        let (context, track) = mode.flags();
        self.active_command("repeat", |spirc| spirc.repeat(context))?;
        self.command("repeat track", |spirc| spirc.repeat_track(track))
    }
    /// Set volume in librespot's 0..=65535 range.
    pub fn set_volume(&self, vol: u16) -> Result<()> {
        // Apply to the local software mixer immediately (no network round-trip).
        if let Some(inner) = &self.inner {
            inner.mixer.set_volume(vol);
        }
        // Sync the volume to Spotify Connect in the background.
        self.command("set volume", |spirc| spirc.set_volume(vol))
    }
    /// Replace the complete local equalizer curve. The audio sink observes the
    /// snapshot between packets, so this never waits on or interrupts playback.
    pub fn set_equalizer(&self, settings: EqualizerSettings) {
        if let Some(inner) = &self.inner {
            *inner
                .equalizer
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = settings.normalized();
        }
    }
    /// Seek to an absolute position in the current track.
    pub fn seek(&self, position_ms: u32) -> Result<()> {
        self.active_command("seek", |spirc| spirc.set_position_ms(position_ms))
    }
    /// This device's Spotify Connect id — used to transfer playback back to myx.
    pub fn device_id(&self) -> String {
        self.link()
            .map(|link| link.session.device_id().to_string())
            .unwrap_or_default()
    }
    /// A cheap clone of the session (for off-thread mercury calls like radio).
    ///
    /// # Panics
    /// On a [`Engine::detached`] one, which only tests can make.
    pub fn session(&self) -> Session {
        self.link()
            .expect("a detached engine has no session")
            .session
            .clone()
    }
}

/// How a load starts: shuffled server-side or in the context's own order, and
/// repeating or not. Always set — a load without them is a load with repeat
/// off, since librespot resets both before applying these.
fn context_options(shuffle: bool, repeat: Repeat) -> Option<LoadContextOptions> {
    let (repeat, repeat_track) = repeat.flags();
    Some(LoadContextOptions::Options(CtxOptions {
        shuffle,
        repeat,
        repeat_track,
    }))
}

/// Fetch a track-seeded radio station via librespot's internal mercury protocol
/// (the same the desktop app uses) — works around the deprecated Web API
/// `/recommendations` endpoint. Returns the seed followed by similar tracks.
pub async fn radio_tracks(session: &Session, seed_uri: &str) -> Result<Vec<String>> {
    // 1) Resolve the seed URI to a radio station URI.
    let autoplay_url = format!("hm://autoplay-enabled/query?uri={seed_uri}");
    let resp = session
        .mercury()
        .get(autoplay_url)
        .map_err(|e| anyhow!("autoplay query: {e}"))?
        .await?;
    if resp.status_code != 200 {
        bail!("autoplay query status {}", resp.status_code);
    }
    let station_uri = String::from_utf8(resp.payload.first().cloned().unwrap_or_default())?;

    // 2) Fetch the station's track list.
    let radio_url = format!("hm://radio-apollo/v3/stations/{station_uri}");
    let resp = session
        .mercury()
        .get(radio_url)
        .map_err(|e| anyhow!("radio station: {e}"))?
        .await?;
    if resp.status_code != 200 {
        bail!("radio station status {}", resp.status_code);
    }
    let data = resp.payload.first().cloned().unwrap_or_default();
    let v: serde_json::Value = serde_json::from_slice(&data)?;

    // Each station track carries its `uri` directly.
    let mut uris = vec![seed_uri.to_string()];
    for t in v["tracks"].as_array().into_iter().flatten() {
        if let Some(uri) = t["uri"].as_str() {
            if uri.starts_with("spotify:track:") && uri != seed_uri {
                uris.push(uri.to_string());
            }
        }
    }
    Ok(uris)
}

/// Where librespot caches credentials + audio.
fn build_cache() -> Result<Cache> {
    let home = crate::home_dir().context("HOME/USERPROFILE not set")?;
    let base = home.join(".cache/myx");
    let audio = base.join("audio");
    std::fs::create_dir_all(&audio).context("create cache dir")?;
    Cache::new(Some(base), None, Some(audio), None).context("open librespot cache")
}

/// Translate a librespot player event into our own event type.
fn map_event(ev: player::PlayerEvent) -> Option<EngineEvent> {
    use player::PlayerEvent as P;
    match ev {
        P::TrackChanged { audio_item } => Some(EngineEvent::TrackChanged {
            uri: audio_item.track_id.to_uri().ok()?,
        }),
        P::Playing {
            track_id,
            position_ms,
            ..
        } => Some(EngineEvent::Playing {
            uri: track_id.to_uri().ok()?,
            position_ms,
        }),
        P::Paused {
            track_id,
            position_ms,
            ..
        } => Some(EngineEvent::Paused {
            uri: track_id.to_uri().ok()?,
            position_ms,
        }),
        P::Stopped { .. } => Some(EngineEvent::Stopped),
        P::PositionChanged {
            play_request_id: _,
            track_id,
            position_ms,
        }
        | P::PositionCorrection {
            play_request_id: _,
            track_id,
            position_ms,
        } => Some(EngineEvent::PositionCorrection {
            uri: track_id.to_uri().ok()?,
            position_ms,
        }),
        P::EndOfTrack { track_id, .. } => Some(EngineEvent::EndOfTrack {
            uri: track_id.to_uri().ok()?,
        }),
        _ => None,
    }
}

/// Streaming credentials, prompting for OAuth only on a first run. Split out
/// of [`run`] so the caller can do the interactive part before a TUI takes the
/// screen — the prompt prints a URL that the alternate screen would swallow.
pub fn credentials() -> Result<Credentials> {
    auth::get_creds(&build_cache()?).context("get credentials")
}

/// Whether [`credentials`] would need to prompt — i.e. nothing is cached yet.
pub fn needs_authorization() -> bool {
    build_cache().is_ok_and(|c| c.credentials().is_none())
}

/// Start the Connect device and begin emitting events on `tx`.
///
/// Must run inside a tokio runtime. Returns the live [`Engine`]; hold onto it for
/// the lifetime of playback.
pub async fn run(
    creds: Credentials,
    tx: flume::Sender<EngineEvent>,
    initial_volume_pct: u8,
    initial_repeat: Repeat,
) -> Result<Engine> {
    let bands = VisBands::shared();
    let equalizer = shared_equalizer();

    // 50% volume in librespot's 0..=65535 range.
    let volume: u16 = (u32::from(initial_volume_pct.clamp(0, 100)) * 65535 / 100) as u16;
    let mixer = Arc::new(SoftMixer::open(MixerConfig::default()).context("open softmixer")?);
    mixer.set_volume(volume);

    let link = connect(&creds, &mixer, &bands, &equalizer, &tx).await?;
    let inner = Arc::new(Inner {
        link: Mutex::new(Arc::new(link)),
        mixer,
        bands: Arc::clone(&bands),
        equalizer,
        events: tx,
        creds,
    });
    spawn_watchdog(&inner);

    Ok(Engine {
        bands,
        inner: Some(inner),
        repeat: Mutex::new(initial_repeat),
    })
}

/// Bring up a session, a player and a Connect device, and bridge the player's
/// events onto `events`. This is what a reconnect rebuilds.
async fn connect(
    first_login: &Credentials,
    mixer: &Arc<SoftMixer>,
    bands: &Arc<Mutex<VisBands>>,
    equalizer: &EqualizerControl,
    events: &flume::Sender<EngineEvent>,
) -> Result<Link> {
    let cache = build_cache()?;
    // Prefer the reusable credentials librespot stores on a successful login:
    // `first_login` may be the one-shot OAuth token from a first run, which
    // Spotify would refuse hours later.
    let creds = cache.credentials().unwrap_or_else(|| first_login.clone());
    let session = Session::new(SessionConfig::default(), Some(cache));

    let backend = audio_backend::find(None).expect("an audio backend should be available");
    let cfg = crate::config::get();
    let player_config = PlayerConfig {
        // Each correction pushes the whole Connect state to Spotify, so the old
        // 100ms announced us ten times a second. Position is extrapolated
        // locally; this only trims the drift.
        position_update_interval: Some(Duration::from_secs(1)),
        bitrate: cfg.bitrate(),
        normalisation: cfg.normalize_volume,
        ..Default::default()
    };

    let player = {
        let bands = Arc::clone(bands);
        let equalizer = Arc::clone(equalizer);
        let events = events.clone();
        Player::new(
            player_config,
            session.clone(),
            mixer.get_soft_volume(),
            move || -> Box<dyn Sink> {
                // rodio's sink unwraps the device on this player thread; with
                // none (startup checked, but it can go — a reconnect rebuilds
                // this sink) stand in silently and say why, instead of panicking.
                let real: Box<dyn Sink> = if output_available() {
                    backend(None, AudioFormat::default())
                } else {
                    let _ = events.send(EngineEvent::NoAudioOutput);
                    Box::new(SilentSink)
                };
                let visualizer =
                    Box::new(VisualizationSink::new(real, Arc::clone(&bands), 44_100.0));
                Box::new(EqualizerSink::new(visualizer, Arc::clone(&equalizer)))
            },
        )
    };

    // Bridge librespot player events -> EngineEvent. Also drive the visualizer's
    // `is_active` flag here (the sink fills the bands, but only playback state
    // knows whether audio is actually flowing).
    let mut channel = player.get_player_event_channel();
    let ev_bands = Arc::clone(bands);
    let tx = events.clone();
    tokio::spawn(async move {
        while let Some(ev) = channel.recv().await {
            match &ev {
                player::PlayerEvent::Playing { .. } => {
                    if let Ok(mut b) = ev_bands.lock() {
                        b.is_active = true;
                    }
                }
                player::PlayerEvent::Paused { .. } | player::PlayerEvent::Stopped { .. } => {
                    if let Ok(mut b) = ev_bands.lock() {
                        b.is_active = false;
                    }
                }
                _ => {}
            }
            if let Some(mapped) = map_event(ev) {
                if tx.send(mapped).is_err() {
                    break; // receiver dropped
                }
            }
        }
    });

    let connect_config = ConnectConfig {
        name: "myx".to_string(),
        device_type: DeviceType::Computer,
        // Carry the live volume across a reconnect — the mixer outlives the
        // connection, so the new device must not announce a stale level.
        initial_volume: mixer.volume(),
        is_group: false,
        disable_volume: false,
        volume_steps: 64,
    };

    let (spirc, spirc_task) = Spirc::new(
        connect_config,
        session.clone(),
        creds,
        player.clone(),
        mixer.clone(),
    )
    .await
    .context("initialize spirc")?;
    tokio::spawn(spirc_task);

    Ok(Link {
        spirc,
        player,
        session,
    })
}

/// How often the watchdog asks whether the access point is still there. The
/// check is a lock read; only an actual reconnect costs anything.
const HEALTH_CHECK: Duration = Duration::from_secs(5);
/// First and last wait between failed reconnects, so a laptop closed overnight
/// with no network doesn't resolve access points every five seconds until dawn.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(120);

fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RETRY_MAX)
}

/// Rebuild the connection whenever the access point drops it.
///
/// librespot marks the session invalid on a keep-alive timeout and leaves
/// recovery to the caller (`session.rs`: "TODO: Optionally reconnect"). Without
/// this, an idle myx wakes up to every command failing with a closed channel,
/// and the only fix is a restart. Polling means the repair usually lands long
/// before anyone presses a key.
fn spawn_watchdog(inner: &Arc<Inner>) {
    let weak = Arc::downgrade(inner);
    tokio::spawn(async move {
        let mut backoff = RETRY_MIN;
        loop {
            tokio::time::sleep(HEALTH_CHECK).await;
            // Gone means myx is quitting; the watchdog must not be what keeps
            // the Connect device alive.
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if !inner.is_stale() {
                backoff = RETRY_MIN;
                continue;
            }
            let _ = inner.events.send(EngineEvent::Reconnecting);
            let failed = inner.reconnect().await.is_err();
            if !failed {
                backoff = RETRY_MIN;
                let _ = inner.events.send(EngineEvent::Reconnected);
                continue;
            }
            // Hold nothing across the wait, or quitting blocks on it.
            drop(inner);
            tokio::time::sleep(backoff).await;
            backoff = next_backoff(backoff);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_reconnect_backs_off_up_to_the_cap() {
        assert!(next_backoff(RETRY_MIN) > RETRY_MIN);
        let mut wait = RETRY_MIN;
        for _ in 0..10 {
            wait = next_backoff(wait);
            assert!(wait <= RETRY_MAX, "{wait:?} is past the cap");
        }
        // An offline night must settle at the cap rather than grow without
        // bound — the watchdog has to still be trying when the network returns.
        assert_eq!(wait, RETRY_MAX);
    }

    #[test]
    fn repeat_steps_off_all_one_like_spotify() {
        assert_eq!(Repeat::default(), Repeat::Off);
        assert_eq!(Repeat::Off.next(), Repeat::All);
        assert_eq!(Repeat::All.next(), Repeat::One);
        assert_eq!(Repeat::One.next(), Repeat::Off);
    }

    #[test]
    fn repeat_one_loops_the_track_and_the_list_behind_it() {
        assert_eq!(Repeat::Off.flags(), (false, false));
        assert_eq!(Repeat::All.flags(), (true, false));
        assert_eq!(Repeat::One.flags(), (true, true));
        for mode in [Repeat::Off, Repeat::All, Repeat::One] {
            let (context, track) = mode.flags();
            assert_eq!(Repeat::from_flags(context, track), mode);
        }
    }

    #[test]
    fn a_skip_ends_repeat_one_but_keeps_the_list_repeating() {
        assert_eq!(Repeat::One.after_skip(), Repeat::All);
        assert_eq!(Repeat::All.after_skip(), Repeat::All);
        assert_eq!(Repeat::Off.after_skip(), Repeat::Off);
    }

    /// librespot turns repeat off on every load and back on only from these
    /// options, so a load that leaves them out is a load with repeat off.
    #[test]
    fn every_load_carries_the_repeat_mode() {
        for shuffle in [false, true] {
            for mode in [Repeat::Off, Repeat::All, Repeat::One] {
                let Some(LoadContextOptions::Options(o)) = context_options(shuffle, mode) else {
                    panic!("no options for shuffle={shuffle} {mode:?}");
                };
                assert_eq!(o.shuffle, shuffle);
                assert_eq!((o.repeat, o.repeat_track), mode.flags());
            }
        }
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn the_repeat_mode_is_kept_when_the_player_is_unreachable() {
        let engine = Engine::detached();
        assert!(engine.set_repeat(Repeat::One).is_err());
        assert_eq!(engine.repeat_mode(), Repeat::One);
    }
}
