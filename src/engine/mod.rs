//! The librespot streaming engine.
//!
//! Authenticates, brings up a Spotify Connect device (Spirc) with our tee'd FFT
//! sink, and bridges librespot's player events into a clean [`EngineEvent`]
//! stream. This is what makes "track change" *real*: when Spotify hands us a new
//! track, a `TrackChanged` lands on the channel — the hook the reactive theme +
//! cover fade will fire from.
//!
//! Session/Spirc wiring mirrors aome510/spotify-player (`streaming.rs`, MIT,
//! © 2021 Thang Pham), stripped to just what myx needs.

pub mod auth;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use librespot_connect::{
    ConnectConfig, LoadContextOptions, LoadRequest, LoadRequestOptions, Options as CtxOptions,
    PlayingTrack, Spirc,
};
use librespot_core::cache::Cache;
use librespot_core::config::DeviceType;
use librespot_core::{Session, SessionConfig};
use librespot_playback::audio_backend::{self, Sink};
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::softmixer::SoftMixer;
use librespot_playback::mixer::{Mixer, MixerConfig};
use librespot_playback::player::{self, Player};

use crate::audio::{VisBands, VisualizationSink};

/// A normalized playback event surfaced to the rest of the app.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A new track became current — carries its Spotify URI. This is the reactive
    /// theme trigger.
    TrackChanged { uri: String },
    Playing { uri: String, position_ms: u32 },
    Paused { uri: String, position_ms: u32 },
    EndOfTrack { uri: String },
}

/// A running engine: keep `spirc` alive (dropping it tears down the device), and
/// read `bands` for the live visualizer.
pub struct Engine {
    pub spirc: Spirc,
    pub bands: Arc<Mutex<VisBands>>,
    session: Session,
}

impl Engine {
    /// Fetch a fresh Web API access token off the librespot session (login5).
    /// Used for track metadata + cover art lookups.
    pub async fn web_token(&self) -> Result<String> {
        let fut = self.session.login5().auth_token();
        let token = tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .map_err(|_| anyhow!("timed out fetching web token"))?
            .map_err(|e| anyhow!("web token error: {e:?}"))?;
        Ok(token.access_token)
    }

    /// Start playing a context (playlist / album / artist / track URI). When
    /// `shuffle` is set, Spotify shuffles the *entire* context server-side.
    pub fn play_context(&self, context_uri: impl Into<String>, shuffle: bool) -> Result<()> {
        self.spirc.activate().ok();
        let options = LoadRequestOptions {
            start_playing: true,
            context_options: shuffle
                .then(|| LoadContextOptions::Options(CtxOptions { shuffle: true, ..Default::default() })),
            ..Default::default()
        };
        self.spirc
            .load(LoadRequest::from_context_uri(context_uri.into(), options))
            .context("load context")?;
        Ok(())
    }

    /// Load a context and start at a specific track + position (context resume).
    pub fn play_context_at(
        &self,
        context_uri: String,
        track_uri: Option<String>,
        position_ms: u32,
        shuffle: bool,
    ) -> Result<()> {
        self.spirc.activate().ok();
        let options = LoadRequestOptions {
            start_playing: true,
            seek_to: position_ms,
            context_options: shuffle
                .then(|| LoadContextOptions::Options(CtxOptions { shuffle: true, ..Default::default() })),
            playing_track: track_uri.map(PlayingTrack::Uri),
        };
        self.spirc
            .load(LoadRequest::from_context_uri(context_uri, options))
            .context("load context at")?;
        Ok(())
    }

    /// Play an explicit list of track URIs as a context. `start_uri` picks the
    /// first track (ignored under shuffle); `shuffle` shuffles the whole list
    /// server-side — so shuffling Liked Songs covers *every* track we pass in.
    pub fn play_tracks(
        &self,
        tracks: Vec<String>,
        start_uri: Option<String>,
        shuffle: bool,
    ) -> Result<()> {
        self.spirc.activate().ok();
        let options = LoadRequestOptions {
            start_playing: true,
            context_options: shuffle
                .then(|| LoadContextOptions::Options(CtxOptions { shuffle: true, ..Default::default() })),
            playing_track: start_uri.map(PlayingTrack::Uri),
            ..Default::default()
        };
        self.spirc
            .load(LoadRequest::from_tracks(tracks, options))
            .context("load tracks")?;
        Ok(())
    }

    /// Load a single track and start playing at `position_ms` — used to resume
    /// the last session's track when the user first hits play.
    pub fn play_track_at(&self, uri: String, position_ms: u32) -> Result<()> {
        self.spirc.activate().ok();
        let options = LoadRequestOptions {
            start_playing: true,
            seek_to: position_ms,
            ..Default::default()
        };
        self.spirc
            .load(LoadRequest::from_tracks(vec![uri], options))
            .context("resume track")?;
        Ok(())
    }

    pub fn play(&self) -> Result<()> {
        self.spirc.play().context("play")
    }
    pub fn pause(&self) -> Result<()> {
        self.spirc.pause().context("pause")
    }
    pub fn toggle(&self) -> Result<()> {
        self.spirc.play_pause().context("toggle")
    }
    pub fn next(&self) -> Result<()> {
        self.spirc.next().context("next")
    }
    pub fn prev(&self) -> Result<()> {
        self.spirc.prev().context("prev")
    }
    pub fn shuffle(&self, on: bool) -> Result<()> {
        self.spirc.shuffle(on).context("shuffle")
    }
    pub fn repeat(&self, on: bool) -> Result<()> {
        self.spirc.repeat(on).context("repeat")
    }
    /// Set volume in librespot's 0..=65535 range.
    pub fn set_volume(&self, vol: u16) -> Result<()> {
        self.spirc.set_volume(vol).context("set volume")
    }
    /// This device's Spotify Connect id — used to transfer playback back to myx.
    pub fn device_id(&self) -> String {
        self.session.device_id().to_string()
    }
    /// A cheap clone of the session (for off-thread mercury calls like radio).
    pub fn session(&self) -> Session {
        self.session.clone()
    }
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
    let home = std::env::var("HOME").context("HOME not set")?;
    let base = PathBuf::from(home).join(".cache/myx");
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
        P::EndOfTrack { track_id, .. } => Some(EngineEvent::EndOfTrack {
            uri: track_id.to_uri().ok()?,
        }),
        _ => None,
    }
}

/// Authenticate, start the Connect device, and begin emitting events on `tx`.
///
/// Must run inside a tokio runtime. Returns the live [`Engine`]; hold onto it for
/// the lifetime of playback.
pub async fn run(tx: flume::Sender<EngineEvent>, initial_volume_pct: u8) -> Result<Engine> {
    let cache = build_cache()?;
    let creds = auth::get_creds(&cache).context("get credentials")?;
    let session = Session::new(SessionConfig::default(), Some(cache));

    let bands = VisBands::shared();

    // 50% volume in librespot's 0..=65535 range.
    let volume: u16 = (u32::from(initial_volume_pct.clamp(0, 100)) * 65535 / 100) as u16;
    let mixer = Arc::new(SoftMixer::open(MixerConfig::default()).context("open softmixer")?);
    mixer.set_volume(volume);

    let backend = audio_backend::find(None).expect("an audio backend should be available");
    let player_config = PlayerConfig::default();

    let player = {
        let bands = Arc::clone(&bands);
        Player::new(
            player_config,
            session.clone(),
            mixer.get_soft_volume(),
            move || -> Box<dyn Sink> {
                let real = backend(None, AudioFormat::default());
                Box::new(VisualizationSink::new(real, Arc::clone(&bands), 44_100.0))
            },
        )
    };

    // Bridge librespot player events -> EngineEvent. Also drive the visualizer's
    // `is_active` flag here (the sink fills the bands, but only playback state
    // knows whether audio is actually flowing).
    let mut channel = player.get_player_event_channel();
    let ev_bands = Arc::clone(&bands);
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
        initial_volume: volume,
        is_group: false,
        disable_volume: false,
        volume_steps: 64,
    };

    let (spirc, spirc_task) = Spirc::new(connect_config, session.clone(), creds, player, mixer)
        .await
        .context("initialize spirc")?;
    tokio::spawn(spirc_task);

    Ok(Engine {
        spirc,
        bands,
        session,
    })
}
