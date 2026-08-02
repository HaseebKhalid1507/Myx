//! External audio source: play whatever a helper program writes to stdout.
//!
//! The helper is [earshot], whose contract is `earshot <uri> [--start-ms N]` →
//! Ogg on stdout, one metadata line on stderr, meaningful exit codes. That makes
//! the integration a byte pipe rather than a library dependency: no Spotify
//! session, no librespot `Player`, and nothing here knows what a URI means.
//!
//! Why this exists: streaming through librespot needs the Premium-only
//! `streaming` scope. `--guest` boots without it so a free account can browse,
//! but until now the transport keys did nothing. An external source fills that
//! hole for anything the helper can resolve.
//!
//! ## How the transport maps onto a subprocess
//!
//! earshot is stateless per invocation, which decides all three verbs:
//!
//! | myx action | what happens |
//! |---|---|
//! | pause | **stop reading stdout.** The kernel pipe fills and earshot blocks in `write`. No signal, no state. |
//! | seek | kill and respawn with a new `--start-ms`. |
//! | stop | kill. |
//!
//! Pause-by-backpressure is earshot's design, not a trick: a full pipe *is* the
//! pause. Worth knowing that a Linux pipe holds 64 KiB — roughly 1.6 s at
//! 320 kbps — so the helper parks almost immediately rather than racing ahead.
//!
//! ## Two contract obligations we must honour
//!
//! - **stderr must be drained.** earshot logs there, and a full stderr pipe would
//!   block *its* writer — which surfaces as audio stalling and looks exactly like
//!   a decode bug. A dedicated thread reads stderr to EOF for the child's whole
//!   life.
//! - **`actual_start_ms`, not the requested position.** A splice lands on a page
//!   boundary and a decoder discards the head of a spliced stream, so asking for
//!   90 000 ms may really begin at 89 020 ms. Position is counted from what
//!   earshot reports, or the progress bar drifts by up to a second.
//!
//! Audio is written into myx's existing [`VisualizationSink`] stack, so the
//! external path shares the real output device and gets the FFT visualizer for
//! free.
//!
//! ## Codec support: Ogg Vorbis only
//!
//! Decoding uses symphonia, which as of 0.5 ships no Opus decoder — there is no
//! `opus` feature and no `symphonia-codec-opus` crate. Ogg Opus therefore fails
//! with an explicit error rather than silence.
//!
//! That is the right scope for now rather than a gap to paper over: Spotify
//! serves `OGG_VORBIS`, so Vorbis is what this path exists to play. Supporting
//! Opus would mean a native libopus dependency, which is a decision worth taking
//! deliberately when a consumer actually needs it.
//!
//! [earshot]: https://github.com/vishalmakwana111/earshot

use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use librespot_playback::audio_backend::{self, Sink};
use librespot_playback::config::AudioFormat;
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::audio::{VisBands, VisualizationSink};
use crate::engine::EngineEvent;

/// Helper program used when nothing else is configured.
pub const DEFAULT_PROGRAM: &str = "earshot";

/// Builds the audio sink for a stream, given its sample rate.
///
/// Injectable because the sink is the only part of this module that needs
/// hardware. Everything else — spawning the helper, honouring its metadata,
/// decoding, position accounting, pause and respawn — is byte plumbing, and
/// testing it should not require an output device or make noise.
pub type SinkBuilder = Arc<dyn Fn(u32) -> Box<dyn Sink> + Send + Sync>;

/// Short enough for myx's one-line status slot, and still actionable.
///
/// The status line shares a narrow row with the view indicator, so a full
/// `curl | sh` one-liner is truncated mid-URL there — which is worse than no
/// hint, because it looks like advice and cannot be followed. `cargo install
/// earshot` fits and is sufficient on its own; [`INSTALL_HINT_FULL`] adds the
/// alternatives for anyone running with `MYX_LOG` set.
const INSTALL_HINT: &str = "install: cargo install earshot";

/// The complete install instructions. Only reaches the log, which is opt-in via
/// `MYX_LOG`, so this supplements the status line rather than replacing it.
const INSTALL_HINT_FULL: &str = "install the helper with one of:\n      cargo install earshot\n      curl --proto '=https' --tlsv1.2 -LsSf     https://github.com/vishalmakwana111/earshot/releases/latest/download/earshot-installer.sh | sh\n    or point myx at an existing binary with --source <path>";

/// How long to wait for the helper's metadata line before giving up.
const META_TIMEOUT: Duration = Duration::from_secs(10);

/// How often to push an authoritative position to the UI. Matches the interval
/// the librespot path uses, so both backends drift the same amount.
const POSITION_INTERVAL: Duration = Duration::from_secs(1);

/// What the helper told us about the stream it is about to write.
#[derive(Debug, Clone, Default)]
pub struct SourceMeta {
    pub codec: String,
    pub rate: u32,
    pub channels: u16,
    pub duration_ms: Option<u64>,
    pub requested_start_ms: u64,
    /// Where the audio *actually* begins. Never later than requested.
    pub actual_start_ms: u64,
}

impl SourceMeta {
    /// Parse earshot's one-line metadata JSON.
    fn parse(json: &str) -> Result<SourceMeta> {
        let v: serde_json::Value =
            serde_json::from_str(json).context("parse source metadata json")?;
        Ok(SourceMeta {
            codec: v["codec"].as_str().unwrap_or("unknown").to_string(),
            rate: v["rate"].as_u64().unwrap_or(44_100) as u32,
            channels: v["channels"].as_u64().unwrap_or(2) as u16,
            duration_ms: v["duration_ms"].as_u64(),
            requested_start_ms: v["requested_start_ms"].as_u64().unwrap_or(0),
            actual_start_ms: v["actual_start_ms"].as_u64().unwrap_or(0),
        })
    }
}

/// Snapshot of the source, readable from the UI thread.
#[derive(Debug, Clone, Default)]
pub struct SourceState {
    pub uri: Option<String>,
    pub playing: bool,
    pub position_ms: u32,
    pub duration_ms: Option<u64>,
    pub meta: Option<SourceMeta>,
    /// Last failure, for the status line.
    pub error: Option<String>,
}

enum Ctl {
    Load { uri: String, start_ms: u64 },
    Resume,
    Pause,
    Stop,
    Volume(u16),
    Shutdown,
}

/// Handle to the worker thread that owns the helper process.
///
/// Every method is a message send: nothing here blocks on audio.
pub struct ExternalSource {
    program: String,
    ctl: flume::Sender<Ctl>,
    state: Arc<Mutex<SourceState>>,
    /// Same FFT bands the librespot path uses, so the visualizer works here too.
    bands: Arc<Mutex<VisBands>>,
}

impl ExternalSource {
    /// Start the worker against the real audio device.
    ///
    /// Spawns no helper process until [`ExternalSource::load`].
    pub fn new(
        program: impl Into<String>,
        bands: Arc<Mutex<VisBands>>,
        events: flume::Sender<EngineEvent>,
        initial_volume: u16,
    ) -> Arc<Self> {
        // Share myx's real output path, so the external source uses the same
        // device and feeds the same visualizer as the librespot engine.
        let vis_bands = Arc::clone(&bands);
        let sink: SinkBuilder = Arc::new(move |rate: u32| {
            let backend = audio_backend::find(None).expect("an audio backend should be available");
            let real = backend(None, AudioFormat::default());
            Box::new(VisualizationSink::new(
                real,
                Arc::clone(&vis_bands),
                rate as f32,
            ))
        });
        Self::with_sink(program, bands, events, initial_volume, sink)
    }

    /// Start the worker against a caller-supplied sink. See [`SinkBuilder`].
    pub fn with_sink(
        program: impl Into<String>,
        bands: Arc<Mutex<VisBands>>,
        events: flume::Sender<EngineEvent>,
        initial_volume: u16,
        sink: SinkBuilder,
    ) -> Arc<Self> {
        let program = program.into();
        let (ctl_tx, ctl_rx) = flume::unbounded();
        let state = Arc::new(Mutex::new(SourceState::default()));

        {
            let program = program.clone();
            let state = Arc::clone(&state);
            let bands = Arc::clone(&bands);
            let sink = Arc::clone(&sink);
            std::thread::Builder::new()
                .name("myx-external-source".to_string())
                .spawn(move || {
                    Worker {
                        program,
                        bands,
                        events,
                        state,
                        volume: initial_volume,
                        stream: None,
                        sink,
                    }
                    .run(ctl_rx);
                })
                .expect("spawn external source thread");
        }

        Arc::new(ExternalSource {
            program,
            ctl: ctl_tx,
            state,
            bands,
        })
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    /// Live FFT bands, for the visualizer.
    pub fn bands(&self) -> &Arc<Mutex<VisBands>> {
        &self.bands
    }

    pub fn state(&self) -> SourceState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn is_playing(&self) -> bool {
        self.state.lock().map(|s| s.playing).unwrap_or(false)
    }

    pub fn position_ms(&self) -> u32 {
        self.state.lock().map(|s| s.position_ms).unwrap_or(0)
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.state.lock().ok().and_then(|s| s.duration_ms)
    }

    pub fn loaded_uri(&self) -> Option<String> {
        self.state.lock().ok().and_then(|s| s.uri.clone())
    }

    /// Load a URI and begin playing at `start_ms`.
    pub fn load(&self, uri: impl Into<String>, start_ms: u64) {
        let _ = self.ctl.send(Ctl::Load {
            uri: uri.into(),
            start_ms,
        });
    }

    pub fn play(&self) {
        let _ = self.ctl.send(Ctl::Resume);
    }

    pub fn pause(&self) {
        let _ = self.ctl.send(Ctl::Pause);
    }

    pub fn toggle(&self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn stop(&self) {
        let _ = self.ctl.send(Ctl::Stop);
    }

    /// Seek by respawning the helper. Stateless per invocation is the whole
    /// point: there is no seek to send, only a new invocation to start.
    pub fn seek(&self, position_ms: u32) {
        let uri = self.loaded_uri();
        if let Some(uri) = uri {
            let _ = self.ctl.send(Ctl::Load {
                uri,
                start_ms: position_ms as u64,
            });
        }
    }

    pub fn set_volume(&self, volume: u16) {
        let _ = self.ctl.send(Ctl::Volume(volume));
    }
}

impl Drop for ExternalSource {
    fn drop(&mut self) {
        let _ = self.ctl.send(Ctl::Shutdown);
    }
}

/// A live helper process and everything needed to keep decoding it.
struct Stream {
    child: Child,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    sink: Box<dyn Sink>,
    converter: Converter,
    uri: String,
    meta: SourceMeta,
    /// Frames (per channel) written since this spawn.
    frames: u64,
    /// Reused across packets; allocated on the first one that carries audio.
    sample_buf: Option<SampleBuffer<f32>>,
    last_report: Instant,
    /// Kept so the child's stderr keeps being drained for its whole life.
    _stderr_drain: std::thread::JoinHandle<()>,
}

impl Stream {
    fn position_ms(&self) -> u32 {
        let rate = self.meta.rate.max(1) as u64;
        let elapsed = self.frames * 1000 / rate;
        (self.meta.actual_start_ms + elapsed).min(u32::MAX as u64) as u32
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.sink.stop();
        // Killing the child is disposal, which earshot treats as a clean exit.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Worker {
    program: String,
    bands: Arc<Mutex<VisBands>>,
    events: flume::Sender<EngineEvent>,
    state: Arc<Mutex<SourceState>>,
    volume: u16,
    stream: Option<Stream>,
    sink: SinkBuilder,
}

impl Worker {
    fn run(mut self, ctl: flume::Receiver<Ctl>) {
        loop {
            // Idle or paused: block rather than spin. A paused stream is left
            // alive and simply unread, which is what makes the helper park on a
            // full pipe.
            let playing = self.playing();
            let message = if playing {
                match ctl.try_recv() {
                    Ok(m) => Some(m),
                    Err(flume::TryRecvError::Empty) => None,
                    Err(flume::TryRecvError::Disconnected) => return,
                }
            } else {
                match ctl.recv() {
                    Ok(m) => Some(m),
                    Err(_) => return,
                }
            };

            if let Some(message) = message {
                match message {
                    Ctl::Shutdown => {
                        self.stream = None;
                        return;
                    }
                    Ctl::Load { uri, start_ms } => self.load(uri, start_ms),
                    Ctl::Resume => self.resume(),
                    Ctl::Pause => self.pause(),
                    Ctl::Stop => self.stop(),
                    Ctl::Volume(v) => self.volume = v,
                }
                continue;
            }

            self.pump();
        }
    }

    fn playing(&self) -> bool {
        self.stream.is_some() && self.state.lock().map(|s| s.playing).unwrap_or(false)
    }

    fn set_active(&self, active: bool) {
        if let Ok(mut b) = self.bands.lock() {
            b.is_active = active;
        }
    }

    fn update<F: FnOnce(&mut SourceState)>(&self, f: F) {
        if let Ok(mut s) = self.state.lock() {
            f(&mut s);
        }
    }

    fn load(&mut self, uri: String, start_ms: u64) {
        // Drop first: the old helper must die before the new one opens the
        // output device.
        self.stream = None;
        self.set_active(false);

        match spawn_stream(&self.program, &uri, start_ms, &self.sink) {
            Ok(stream) => {
                let meta = stream.meta.clone();
                let position = stream.position_ms();
                self.update(|s| {
                    s.uri = Some(uri.clone());
                    s.playing = true;
                    s.position_ms = position;
                    s.duration_ms = meta.duration_ms;
                    s.meta = Some(meta);
                    s.error = None;
                });
                self.stream = Some(stream);
                self.set_active(true);
                let _ = self
                    .events
                    .send(EngineEvent::TrackChanged { uri: uri.clone() });
                let _ = self.events.send(EngineEvent::Playing {
                    uri,
                    position_ms: position,
                });
            }
            Err(e) => {
                let message = format!("{e:#}");
                self.update(|s| {
                    s.playing = false;
                    s.error = Some(message);
                });
                let _ = self.events.send(EngineEvent::Stopped);
            }
        }
    }

    fn resume(&mut self) {
        if self.stream.is_none() {
            return;
        }
        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.sink.start();
        }
        self.set_active(true);
        let (uri, position) = match self.stream.as_ref() {
            Some(s) => (s.uri.clone(), s.position_ms()),
            None => return,
        };
        self.update(|s| s.playing = true);
        let _ = self.events.send(EngineEvent::Playing {
            uri,
            position_ms: position,
        });
    }

    fn pause(&mut self) {
        if self.stream.is_none() {
            return;
        }
        // Stopping the sink and ceasing to read is the whole pause: the helper
        // fills the pipe and blocks in write.
        if let Some(stream) = self.stream.as_mut() {
            let _ = stream.sink.stop();
        }
        self.set_active(false);
        let (uri, position) = match self.stream.as_ref() {
            Some(s) => (s.uri.clone(), s.position_ms()),
            None => return,
        };
        self.update(|s| s.playing = false);
        let _ = self.events.send(EngineEvent::Paused {
            uri,
            position_ms: position,
        });
    }

    fn stop(&mut self) {
        self.stream = None;
        self.set_active(false);
        self.update(|s| {
            s.playing = false;
            s.position_ms = 0;
        });
        let _ = self.events.send(EngineEvent::Stopped);
    }

    /// Decode and play one packet.
    fn pump(&mut self) {
        let Some(stream) = self.stream.as_mut() else {
            return;
        };
        let volume = self.volume as f64 / u16::MAX as f64;

        let packet = match stream.format.next_packet() {
            Ok(p) => p,
            // The helper closed stdout: end of stream.
            Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                let uri = stream.uri.clone();
                self.stream = None;
                self.set_active(false);
                self.update(|s| s.playing = false);
                let _ = self.events.send(EngineEvent::EndOfTrack { uri });
                return;
            }
            Err(SymphoniaError::ResetRequired) => {
                let uri = stream.uri.clone();
                self.stream = None;
                self.set_active(false);
                self.update(|s| s.playing = false);
                let _ = self.events.send(EngineEvent::EndOfTrack { uri });
                return;
            }
            Err(e) => {
                let message = format!("source read failed: {e}");
                log::warn!("{message}");
                self.stream = None;
                self.set_active(false);
                self.update(|s| {
                    s.playing = false;
                    s.error = Some(message);
                });
                let _ = self.events.send(EngineEvent::Stopped);
                return;
            }
        };

        if packet.track_id() != stream.track_id {
            return;
        }

        match stream.decoder.decode(&packet) {
            Ok(decoded) => {
                let frames = decoded.frames();
                // A packet that decodes to nothing is normal, not a failure:
                // Vorbis is a lapped transform, so the first packet of a stream —
                // and of every splice — has no predecessor to overlap-add
                // against and yields zero frames. Symphonia's
                // `copy_interleaved_ref` panics on an empty buffer, so this
                // guard is load-bearing rather than defensive.
                if frames > 0 {
                    let buf = stream.sample_buf.get_or_insert_with(|| {
                        SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec())
                    });
                    buf.copy_interleaved_ref(decoded);

                    let samples: Vec<f64> =
                        buf.samples().iter().map(|&s| s as f64 * volume).collect();
                    if let Err(e) = stream
                        .sink
                        .write(AudioPacket::Samples(samples), &mut stream.converter)
                    {
                        log::warn!("audio sink write failed: {e}");
                    }
                    stream.frames += frames as u64;
                }
            }
            // A damaged packet is recoverable: skip it rather than end playback.
            Err(SymphoniaError::DecodeError(e)) => log::debug!("skipped bad packet: {e}"),
            Err(e) => log::warn!("decode failed: {e}"),
        }

        let position = stream.position_ms();
        let uri = stream.uri.clone();
        let due = stream.last_report.elapsed() >= POSITION_INTERVAL;
        if due {
            stream.last_report = Instant::now();
        }
        self.update(|s| s.position_ms = position);
        if due {
            let _ = self.events.send(EngineEvent::PositionCorrection {
                uri,
                position_ms: position,
            });
        }
    }
}

/// Spawn the helper and get as far as a decodable stream.
fn spawn_stream(
    program: &str,
    uri: &str,
    start_ms: u64,
    build_sink: &SinkBuilder,
) -> Result<Stream> {
    let mut child = Command::new(program)
        .arg(uri)
        .arg("--start-ms")
        .arg(start_ms.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| spawn_error(program, e))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("{program}: no stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("{program}: no stderr"))?;

    // The helper's contract requires its stderr to be drained for the whole run:
    // a full stderr pipe blocks its writer, which would look like an audio stall.
    let (meta_tx, meta_rx) = flume::bounded::<String>(1);
    let drain = std::thread::Builder::new()
        .name("myx-source-stderr".to_string())
        .spawn(move || {
            let reader = BufReader::new(stderr);
            let mut meta_sent = false;
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if !meta_sent {
                    if let Some(json) = line.strip_prefix("earshot-meta: ") {
                        let _ = meta_tx.send(json.to_string());
                        meta_sent = true;
                        continue;
                    }
                }
                log::debug!("source: {line}");
            }
        })
        .context("spawn stderr drain")?;

    // Metadata arrives before the first audio byte, so this cannot deadlock
    // against the decoder below.
    let meta = match meta_rx.recv_timeout(META_TIMEOUT) {
        Ok(json) => SourceMeta::parse(&json)?,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program}: no metadata line within {META_TIMEOUT:?}");
        }
    };

    let source = PipeSource { inner: stdout };
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("ogg");

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("probe source stream")?;
    let format = probed.format;

    let track = format
        .default_track()
        .ok_or_else(|| anyhow!("source stream has no audio track"))?;
    let track_id = track.id;
    let decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .with_context(|| {
            format!(
                "cannot decode {}: myx plays Ogg Vorbis (symphonia has no Opus decoder)",
                meta.codec,
            )
        })?;

    let mut sink = build_sink(meta.rate);
    sink.start().map_err(|e| anyhow!("start audio sink: {e}"))?;

    Ok(Stream {
        child,
        format,
        decoder,
        track_id,
        sink,
        converter: Converter::new(None),
        uri: uri.to_string(),
        meta,
        frames: 0,
        sample_buf: None,
        last_report: Instant::now(),
        _stderr_drain: drain,
    })
}

/// Explain a failed spawn in terms the user can act on.
///
/// A bare program name was resolved through `PATH`, so "not found" means "not
/// installed" and deserves the install line. A name containing a separator was
/// given explicitly, so the useful thing to say is that *that path* is wrong —
/// quoting an installer there would just be noise.
fn spawn_error(program: &str, source: io::Error) -> anyhow::Error {
    let looks_like_a_path = program.contains(std::path::MAIN_SEPARATOR) || program.contains('/');
    match (source.kind(), looks_like_a_path) {
        (io::ErrorKind::NotFound, false) => {
            // The status line carries the short form, which stands alone; this
            // adds the alternatives for anyone who has logging enabled.
            log::warn!("{program} is not on PATH. {INSTALL_HINT_FULL}");
            anyhow!("{program} is not on PATH — {INSTALL_HINT}")
        }
        (io::ErrorKind::NotFound, true) => {
            anyhow!("no such source program: {program}")
        }
        (io::ErrorKind::PermissionDenied, _) => {
            anyhow!("{program} is not executable: {source}")
        }
        _ => anyhow!("could not start {program}: {source}"),
    }
}

/// The helper's stdout as a symphonia media source.
///
/// Explicitly not seekable: seeking a subprocess's pipe is meaningless, and
/// saying so lets symphonia pick its non-seeking code paths instead of failing
/// mid-probe. Seeking at the myx level respawns the helper instead.
struct PipeSource {
    inner: ChildStdout,
}

impl Read for PipeSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for PipeSource {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "an external source pipe cannot seek; respawn instead",
        ))
    }
}

impl MediaSource for PipeSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_earshot_metadata() {
        let json = r#"{"codec":"vorbis","rate":44100,"channels":2,
            "duration_ms":214533,"requested_start_ms":90000,
            "actual_start_ms":89020,"start_granule":3925504,
            "container_offset":0,"header_pages":2}"#;
        let meta = SourceMeta::parse(json).expect("parse");
        assert_eq!(meta.codec, "vorbis");
        assert_eq!(meta.rate, 44_100);
        assert_eq!(meta.channels, 2);
        assert_eq!(meta.duration_ms, Some(214_533));
        assert_eq!(meta.requested_start_ms, 90_000);
        // The number the progress bar must use.
        assert_eq!(meta.actual_start_ms, 89_020);
    }

    #[test]
    fn metadata_defaults_are_sane_when_fields_are_missing() {
        let meta = SourceMeta::parse("{}").expect("parse");
        assert_eq!(meta.rate, 44_100);
        assert_eq!(meta.channels, 2);
        assert_eq!(meta.duration_ms, None);
        assert_eq!(meta.actual_start_ms, 0);
    }

    #[test]
    fn malformed_metadata_is_an_error_not_a_panic() {
        assert!(SourceMeta::parse("not json at all").is_err());
        assert!(SourceMeta::parse("").is_err());
    }

    /// Position must be counted from where the helper says audio *actually*
    /// begins. Using the requested position instead would drift by up to a page.
    #[test]
    fn position_counts_from_actual_start_not_requested() {
        let meta = SourceMeta {
            rate: 44_100,
            channels: 2,
            requested_start_ms: 90_000,
            actual_start_ms: 89_020,
            ..Default::default()
        };
        // One second of frames decoded.
        let frames = 44_100u64;
        let elapsed = frames * 1000 / meta.rate as u64;
        assert_eq!(meta.actual_start_ms + elapsed, 90_020);
        // Had we trusted the request, we would report 91_000 — a second out.
        assert_ne!(meta.requested_start_ms + elapsed, 90_020);
    }
}
