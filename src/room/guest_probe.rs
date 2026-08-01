//! A one-shot diagnostic that runs the guest pipeline against a live host and
//! reports what came back.
//!
//! It calls the same functions the room player does — join, resolve, fetch,
//! splice, decode — so a green run says the real path works, not a stub of it.
//! Audio is pumped through a standalone mixer rather than the sound card, so
//! running this never makes noise.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

use crate::room::player::{fetch_track, substream_ranges, Substream};
use crate::room::RoomClient;

/// What one pass through the guest pipeline produced.
pub struct GuestReport {
    pub joined: bool,
    pub resolved_format: String,
    pub cdn_urls: usize,
    pub downloaded_bytes: usize,
    pub ogg_bytes: usize,
    pub duration: Duration,
    pub sample_rate: u32,
    pub channels: u16,
    pub peak: f32,
    pub played_ms: u128,
}

/// Join `url`, resolve and play `uri`, and report. Holds no Spotify credentials.
pub fn run(url: &str, token: &str, uri: &str) -> Result<GuestReport> {
    use rodio::Source;

    let http = RoomClient::http();
    RoomClient::join(&http, url, token).context("join the room")?;

    // A separate resolve purely so the report can name the format and URL count;
    // `fetch_track` below does its own, which also exercises the host's cache.
    let resolved =
        RoomClient::resolve_track(&http, url, token, uri).context("resolve the track")?;

    let client = RoomClient {
        url: url.to_string(),
        token: token.to_string(),
    };
    let ogg = Arc::new(fetch_track(&http, &client, uri).context("fetch the track")?);

    let (header, tail) = substream_ranges(&ogg, 0)
        .ok_or_else(|| anyhow!("no audio pages in the decrypted stream"))?;
    let decoder = rodio::Decoder::new(Substream::new(Arc::clone(&ogg), header.clone(), tail.clone()))
        .map_err(|e| anyhow!("decode: {e}"))?;
    let sample_rate = decoder.sample_rate();
    let channels = decoder.channels();

    let mut peak = 0.0f32;
    let mut samples = 0usize;
    for s in decoder {
        peak = peak.max(s.abs());
        samples += 1;
    }
    let duration = Duration::from_secs_f64(
        samples as f64 / channels.max(1) as f64 / sample_rate.max(1) as f64,
    );

    // Prove it reaches a sink and the playhead moves, without opening the
    // machine's audio device.
    let played_ms = pump_briefly(Arc::clone(&ogg))?;

    let downloaded_bytes = encrypted_size(&http, &resolved);
    Ok(GuestReport {
        joined: true,
        resolved_format: resolved.format,
        cdn_urls: resolved.urls.len(),
        downloaded_bytes,
        ogg_bytes: ogg.len(),
        duration,
        sample_rate,
        channels,
        peak,
        played_ms,
    })
}

/// Content-length of the encrypted object, for the report only.
fn encrypted_size(http: &reqwest::blocking::Client, resolved: &super::ResolvedTrack) -> usize {
    resolved
        .urls
        .iter()
        .find_map(|u| {
            let resp = http.get(u).send().ok()?;
            resp.content_length().map(|n| n as usize)
        })
        .unwrap_or(0)
}

/// Append the track to a real `rodio::Sink` fed by a standalone mixer, let it
/// run briefly, and report how far the playhead got.
fn pump_briefly(ogg: Arc<Vec<u8>>) -> Result<u128> {
    let (header, tail) =
        substream_ranges(&ogg, 0).ok_or_else(|| anyhow!("no audio pages to play"))?;
    let decoder = rodio::Decoder::new(Substream::new(ogg, header, tail))
        .map_err(|e| anyhow!("decode for playback: {e}"))?;

    let (mixer, mut source) = rodio::mixer::mixer(2, 44_100);
    mixer.add(rodio::source::Zero::new(2, 44_100));
    let sink = rodio::Sink::connect_new(&mixer);
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let pump = std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            for _ in 0..(2 * 44_100 / 1000) {
                if source.next().is_none() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    // The order `PlayerState::start` uses.
    sink.play();
    sink.append(decoder);

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && sink.get_pos() < Duration::from_millis(500) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let played = sink.get_pos().as_millis();
    let paused = sink.is_paused();
    stop.store(true, Ordering::Relaxed);
    let _ = pump.join();

    if paused {
        return Err(anyhow!("the sink ended up paused — the clear() regression"));
    }
    Ok(played)
}
