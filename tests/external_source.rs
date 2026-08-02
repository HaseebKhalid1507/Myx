//! End-to-end tests for the external source: myx driving the real earshot binary.
//!
//! Nothing here is mocked except the output device. A real helper process is
//! spawned, its metadata line is read from stderr, its stdout Ogg is decoded by
//! symphonia, and the samples land in a recording sink instead of a speaker. That
//! is the whole integration apart from the hardware.
//!
//! The sink is injected rather than stubbed out: `ExternalSource::with_sink` is
//! the same entry point production uses, just handed a different builder. So
//! these tests exercise the shipping code path, not a parallel one.
//!
//! Needs the earshot binary. Point `MYX_TEST_EARSHOT` at it, or leave it and the
//! sibling checkout is used. Tests print a loud SKIPPED line when it is missing
//! rather than passing quietly — a skipped end-to-end test that reports success
//! is worse than no test.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;

use myx::audio::VisBands;
use myx::engine::EngineEvent;
use myx::external::{ExternalSource, SinkBuilder};

/// Counts what reached the sink, so a test can watch audio flow without hearing it.
#[derive(Default)]
struct Recorder {
    frames: AtomicU64,
    writes: AtomicUsize,
    starts: AtomicUsize,
    stops: AtomicUsize,
    /// Peak absolute sample value — proves the samples are audio, not silence.
    peak_milli: AtomicU64,
}

struct RecordingSink {
    rec: Arc<Recorder>,
    channels: u64,
}

impl Sink for RecordingSink {
    fn start(&mut self) -> SinkResult<()> {
        self.rec.starts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.rec.stops.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _: &mut Converter) -> SinkResult<()> {
        if let Ok(samples) = packet.samples() {
            self.rec.frames.fetch_add(
                samples.len() as u64 / self.channels.max(1),
                Ordering::Relaxed,
            );
            self.rec.writes.fetch_add(1, Ordering::Relaxed);
            let peak = samples.iter().fold(0.0f64, |m, s| m.max(s.abs()));
            let milli = (peak * 1000.0) as u64;
            self.rec.peak_milli.fetch_max(milli, Ordering::Relaxed);
        }
        Ok(())
    }
}

fn recorder_sink(rec: Arc<Recorder>, channels: u64) -> SinkBuilder {
    Arc::new(move |_rate: u32| {
        Box::new(RecordingSink {
            rec: Arc::clone(&rec),
            channels,
        })
    })
}

fn earshot_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MYX_TEST_EARSHOT") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    // Sibling checkout, debug then release.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("earshot");
    for candidate in ["target/debug/earshot", "target/release/earshot"] {
        let p = root.join(candidate);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Everything a test needs: a live source, the recorder behind its sink, and the
/// event stream myx's UI would be reading.
struct Harness {
    source: Arc<ExternalSource>,
    rec: Arc<Recorder>,
    events: flume::Receiver<EngineEvent>,
}

fn harness(program: &Path) -> Harness {
    let rec = Arc::new(Recorder::default());
    let (tx, rx) = flume::unbounded();
    let source = ExternalSource::with_sink(
        program.to_string_lossy().to_string(),
        VisBands::shared(),
        tx,
        u16::MAX,
        recorder_sink(Arc::clone(&rec), 2),
    );
    Harness {
        source,
        rec,
        events: rx,
    }
}

/// Spin until `f` holds, or give up. Returns whether it held.
fn wait_for(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

macro_rules! require_earshot {
    () => {
        match earshot_path() {
            Some(p) => p,
            None => {
                eprintln!(
                    "SKIPPED: earshot binary not found. Build the sibling checkout \
                     or set MYX_TEST_EARSHOT."
                );
                return;
            }
        }
    };
}

/// The whole pipe, once: spawn → metadata → decode → samples in the sink.
#[test]
fn plays_a_local_file_through_earshot() {
    let program = require_earshot!();
    let h = harness(&program);

    h.source
        .load(fixture("vorbis_5s.ogg").to_string_lossy().to_string(), 0);

    assert!(
        wait_for(Duration::from_secs(10), || h
            .rec
            .frames
            .load(Ordering::Relaxed)
            > 44_100),
        "no audio reached the sink (frames = {})",
        h.rec.frames.load(Ordering::Relaxed),
    );

    // The samples must be audio, not a stream of zeroes — a decode that silently
    // produced silence would otherwise look identical to success.
    assert!(
        h.rec.peak_milli.load(Ordering::Relaxed) > 10,
        "samples are silent; peak was {}/1000",
        h.rec.peak_milli.load(Ordering::Relaxed),
    );

    let state = h.source.state();
    assert_eq!(state.duration_ms, Some(5_000), "duration from earshot");
    assert!(state.playing);
    assert!(state.error.is_none(), "unexpected error: {:?}", state.error);

    let meta = state.meta.expect("metadata");
    assert_eq!(meta.codec, "vorbis");
    assert_eq!(meta.rate, 44_100);
    assert_eq!(meta.channels, 2);
}

/// earshot serves Opus perfectly well; myx cannot decode it, because symphonia
/// 0.5 ships no Opus decoder. That must fail loudly rather than looking like
/// playback that produced no sound.
///
/// Asserting the limitation keeps it a documented boundary instead of a mystery,
/// and this test starts passing for the right reason the day a decoder exists.
#[test]
fn opus_is_rejected_with_a_clear_message() {
    let program = require_earshot!();
    let h = harness(&program);
    h.source
        .load(fixture("opus_5s.opus").to_string_lossy().to_string(), 0);

    assert!(
        wait_for(Duration::from_secs(10), || h.source.state().error.is_some()),
        "Opus neither played nor reported an error — the worst of both",
    );
    let error = h.source.state().error.unwrap();
    assert!(
        error.to_lowercase().contains("opus") || error.to_lowercase().contains("decode"),
        "error does not explain the problem: {error}",
    );
    assert!(!h.source.state().playing);
    assert_eq!(
        h.rec.frames.load(Ordering::Relaxed),
        0,
        "no audio should have been produced",
    );
}

/// Position must be counted from earshot's `actual_start_ms`, not the request.
///
/// This is the number a progress bar draws. Trusting the request instead would
/// put the playhead up to a page ahead of the audio.
#[test]
fn position_starts_from_actual_start_not_requested() {
    let program = require_earshot!();
    let h = harness(&program);
    let path = fixture("vorbis_5s.ogg").to_string_lossy().to_string();

    h.source.load(path, 3_000);
    assert!(
        wait_for(Duration::from_secs(10), || h.source.state().meta.is_some()),
        "no metadata arrived",
    );

    let state = h.source.state();
    let meta = state.meta.expect("metadata");
    assert_eq!(meta.requested_start_ms, 3_000);
    // earshot lands on a page boundary, so this is earlier — and it must never
    // be later, or audio the caller asked for was dropped.
    assert!(
        meta.actual_start_ms <= 3_000,
        "actual {} overshot the request",
        meta.actual_start_ms,
    );
    assert!(
        meta.actual_start_ms >= 1_500,
        "actual {} is implausibly early for a 1s-page file",
        meta.actual_start_ms,
    );

    // The reported playhead sits at actual_start, not at 3000.
    assert!(
        h.source.position_ms() >= meta.actual_start_ms as u32,
        "position {} is behind actual_start {}",
        h.source.position_ms(),
        meta.actual_start_ms,
    );
    assert!(
        wait_for(Duration::from_secs(5), || h.source.position_ms()
            > meta.actual_start_ms as u32 + 200),
        "position did not advance past {}",
        meta.actual_start_ms,
    );
}

/// Pause is backpressure: myx stops reading, earshot blocks in `write`.
///
/// Observable as samples ceasing to arrive while the child stays alive. The
/// helper is not signalled and holds no pause state — the full pipe *is* the
/// pause.
#[test]
fn pause_stops_the_flow_and_resume_restarts_it() {
    let program = require_earshot!();
    let h = harness(&program);
    h.source
        .load(fixture("vorbis_5s.ogg").to_string_lossy().to_string(), 0);

    assert!(
        wait_for(Duration::from_secs(10), || h
            .rec
            .frames
            .load(Ordering::Relaxed)
            > 10_000),
        "playback never started",
    );

    h.source.pause();
    assert!(
        wait_for(Duration::from_secs(2), || !h.source.state().playing),
        "pause was not acknowledged",
    );

    // Let any in-flight packet land, then confirm the flow really stopped.
    std::thread::sleep(Duration::from_millis(300));
    let settled = h.rec.frames.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        h.rec.frames.load(Ordering::Relaxed),
        settled,
        "frames kept arriving while paused",
    );

    h.source.play();
    assert!(
        wait_for(Duration::from_secs(5), || h
            .rec
            .frames
            .load(Ordering::Relaxed)
            > settled),
        "resume did not restart the flow",
    );
}

/// Seek is respawn: earshot is stateless per invocation, so there is no seek to
/// send — only a new invocation to start.
#[test]
fn seek_respawns_the_helper_at_the_new_position() {
    let program = require_earshot!();
    let h = harness(&program);
    h.source
        .load(fixture("vorbis_5s.ogg").to_string_lossy().to_string(), 0);

    assert!(
        wait_for(Duration::from_secs(10), || h.source.state().meta.is_some()),
        "playback never started",
    );
    let before = h.source.state().meta.expect("metadata");
    assert_eq!(before.requested_start_ms, 0);

    h.source.seek(4_000);

    // A fresh invocation is visible in the metadata: the new one was asked for
    // 4000ms.
    assert!(
        wait_for(Duration::from_secs(10), || h
            .source
            .state()
            .meta
            .map(|m| m.requested_start_ms == 4_000)
            .unwrap_or(false)),
        "helper was not respawned at the new position (meta = {:?})",
        h.source.state().meta,
    );

    let after = h.source.state().meta.expect("metadata");
    assert!(after.actual_start_ms > before.actual_start_ms);
    assert!(
        h.source.position_ms() >= after.actual_start_ms as u32,
        "playhead did not jump forward",
    );
}

/// Reaching the end of a stream must surface as an event, not a stall.
#[test]
fn end_of_stream_reports_end_of_track() {
    let program = require_earshot!();
    let h = harness(&program);
    // Start near the end so the test is quick.
    h.source.load(
        fixture("vorbis_5s.ogg").to_string_lossy().to_string(),
        4_500,
    );

    let saw_end = wait_for(Duration::from_secs(20), || {
        h.events
            .try_iter()
            .any(|e| matches!(e, EngineEvent::EndOfTrack { .. }))
    });
    assert!(saw_end, "no EndOfTrack event");
    assert!(!h.source.state().playing, "still marked playing at the end");
}

/// The events myx's UI reacts to must arrive in a usable order.
#[test]
fn emits_track_changed_then_playing() {
    let program = require_earshot!();
    let h = harness(&program);
    h.source
        .load(fixture("vorbis_5s.ogg").to_string_lossy().to_string(), 0);

    let mut seen: Vec<&'static str> = Vec::new();
    wait_for(Duration::from_secs(10), || {
        for event in h.events.try_iter() {
            match event {
                EngineEvent::TrackChanged { .. } => seen.push("changed"),
                EngineEvent::Playing { .. } => seen.push("playing"),
                _ => {}
            }
        }
        seen.contains(&"changed") && seen.contains(&"playing")
    });
    let changed = seen.iter().position(|e| *e == "changed");
    let playing = seen.iter().position(|e| *e == "playing");
    assert!(changed.is_some(), "no TrackChanged: {seen:?}");
    assert!(playing.is_some(), "no Playing: {seen:?}");
    assert!(
        changed < playing,
        "TrackChanged must precede Playing: {seen:?}"
    );
}

/// Stop must dispose of the child rather than leaving it parked on a full pipe.
#[test]
fn stop_disposes_of_the_helper() {
    let program = require_earshot!();
    let h = harness(&program);
    h.source
        .load(fixture("vorbis_5s.ogg").to_string_lossy().to_string(), 0);
    assert!(
        wait_for(Duration::from_secs(10), || h
            .rec
            .frames
            .load(Ordering::Relaxed)
            > 10_000),
        "playback never started",
    );

    h.source.stop();
    assert!(
        wait_for(Duration::from_secs(5), || !h.source.state().playing),
        "stop was not acknowledged",
    );

    std::thread::sleep(Duration::from_millis(300));
    let settled = h.rec.frames.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        h.rec.frames.load(Ordering::Relaxed),
        settled,
        "audio continued after stop",
    );
    // The sink was told to stop, so the device is released.
    assert!(
        h.rec.stops.load(Ordering::Relaxed) >= 1,
        "sink was not stopped"
    );
}

/// The real output device, end to end — the one thing the other tests substitute.
///
/// Ignored by default because it needs hardware and makes audible noise. Run it
/// deliberately: `cargo test --test external_source -- --ignored`.
///
/// Also checks the visualizer, which is the visible proof that the external path
/// really is writing into myx's own `VisualizationSink` stack rather than a
/// parallel one: the FFT tee only fills those bands if the audio went through it.
#[test]
#[ignore = "needs an audio device and plays audible sound"]
fn plays_through_the_real_device_and_fills_the_visualizer() {
    let program = require_earshot!();
    let bands = VisBands::shared();
    let (tx, _rx) = flume::unbounded();
    // Quarter volume: this is audible, and a test should not shout.
    let source = ExternalSource::new(
        program.to_string_lossy().to_string(),
        Arc::clone(&bands),
        tx,
        u16::MAX / 4,
    );

    source.load(fixture("vorbis_5s.ogg").to_string_lossy().to_string(), 0);

    assert!(
        wait_for(Duration::from_secs(15), || source.position_ms() > 500),
        "playhead never advanced past 500ms on the real device (state = {:?})",
        source.state(),
    );
    assert!(
        source.state().error.is_none(),
        "error on the real device: {:?}",
        source.state().error,
    );
    assert!(
        wait_for(Duration::from_secs(10), || bands
            .lock()
            .map(|b| b.is_active && b.values.iter().any(|v| *v > 0.0))
            .unwrap_or(false)),
        "visualizer bands stayed empty, so audio did not pass through the tee",
    );

    source.stop();
}

// ------------------------------------------------------------- failure paths

/// A missing helper must land in the error state, not panic and not hang.
#[test]
fn a_missing_helper_reports_an_error() {
    let h = harness(Path::new("/definitely/not/a/real/program"));
    h.source.load("whatever.ogg", 0);

    assert!(
        wait_for(Duration::from_secs(5), || h.source.state().error.is_some()),
        "no error recorded for a missing helper",
    );
    assert!(!h.source.state().playing);
    let error = h.source.state().error.unwrap();
    assert!(
        error.contains("spawn") || error.contains("No such file"),
        "unhelpful error: {error}",
    );
}

/// A helper that fails on its input must not leave myx believing it is playing.
#[test]
fn a_helper_failure_reports_an_error() {
    let program = require_earshot!();
    let h = harness(&program);
    h.source.load("/definitely/not/a/real/file.ogg", 0);

    assert!(
        wait_for(Duration::from_secs(10), || {
            let s = h.source.state();
            s.error.is_some() || !s.playing
        }),
        "a failing helper left the source looking healthy",
    );
    assert!(!h.source.state().playing);
}

/// Loading a second URI must dispose of the first helper, not stack them.
#[test]
fn loading_again_replaces_the_previous_helper() {
    let program = require_earshot!();
    let h = harness(&program);

    h.source
        .load(fixture("vorbis_5s.ogg").to_string_lossy().to_string(), 0);
    assert!(
        wait_for(Duration::from_secs(10), || h.source.state().meta.is_some()),
        "first load never started",
    );
    assert_eq!(h.source.state().meta.unwrap().requested_start_ms, 0);

    // Reload the same file at a different offset: a fresh invocation is visible
    // in the metadata, and Opus cannot be used here (see the test above).
    h.source.load(
        fixture("vorbis_5s.ogg").to_string_lossy().to_string(),
        3_000,
    );
    assert!(
        wait_for(Duration::from_secs(10), || h
            .source
            .state()
            .meta
            .map(|m| m.requested_start_ms == 3_000)
            .unwrap_or(false)),
        "second load did not take over",
    );

    // Exactly one stream is live, so starts and stops stay balanced within one.
    let starts = h.rec.starts.load(Ordering::Relaxed);
    let stops = h.rec.stops.load(Ordering::Relaxed);
    assert!(
        starts.abs_diff(stops) <= 1,
        "helpers appear to be stacking: {starts} starts, {stops} stops",
    );
}
