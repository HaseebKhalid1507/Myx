//! The guest's room player: resolve through the host's premium session,
//! download the encrypted file from the CDN, decrypt locally, and play the
//! Ogg stream through rodio. One thread owns the audio output; the UI just
//! sends commands and reads events.
//!
//! Playback events mirror `EngineEvent` (via `RoomEvent::Playback`) so the UI
//! treats room audio exactly like engine audio — same now-playing strip, same
//! theme fades, same lyrics.

use std::collections::{HashMap, VecDeque};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use librespot_audio::AudioDecrypt;
use librespot_core::audio_key::AudioKey;

use crate::engine::EngineEvent;
use crate::room::{ResolvedTrack, RoomClient};

#[cfg(test)]
#[path = "player_tests.rs"]
mod tests;

/// Spotify prepends a ~0xa7-byte Ogg packet (normalisation data); the stream
/// proper starts at the next OggS page.
const OGG_HEADER_END: usize = 0xa7;
/// How much decrypted audio to hold in memory (~13 tracks at 320 kbps).
const CACHE_BUDGET_BYTES: usize = 128 * 1024 * 1024;
/// Command-loop poll interval (also the playback liveness check).
const POLL: Duration = Duration::from_millis(300);
/// How often position corrections are reported to the UI.
const POSITION_TICK: Duration = Duration::from_millis(1000);
const SAMPLE_RATE: u64 = 44_100;

/// Events the room player reports to the UI.
#[derive(Debug, Clone)]
pub enum RoomEvent {
    /// Reply to a join attempt (the actual join round-trip happens off the
    /// UI thread, so this arrives asynchronously).
    Joined { ok: bool, message: String },
    /// A track is being resolved and downloaded — this can take a while, so
    /// the UI says so rather than looking hung.
    Loading { uri: String },
    /// A normal playback event, shaped like an engine's.
    Playback(EngineEvent),
    /// The current track ran out; the UI decides what plays next.
    Ended { uri: String },
    Error { message: String },
}

pub struct RoomPlayer {
    tx: flume::Sender<PlayerCmd>,
}

enum PlayerCmd {
    Join {
        url: String,
        token: String,
    },
    Leave,
    Play {
        uri: String,
        pos_ms: u32,
    },
    /// Posted by a loader thread when a resolve + download finishes. `gen`
    /// identifies which `Play` asked for it, so a superseded load is dropped.
    Loaded {
        uri: String,
        pos_ms: u32,
        gen: u64,
        result: Result<Vec<u8>, String>,
    },
    Toggle,
    Stop,
    Seek(u32),
    Volume(f32),
}

impl RoomPlayer {
    pub fn new(events: flume::Sender<RoomEvent>) -> Arc<Self> {
        let (tx, rx) = flume::unbounded();
        let self_tx = tx.clone();
        std::thread::spawn(move || {
            PlayerState::new(events, self_tx).run(rx);
        });
        Arc::new(Self { tx })
    }

    pub fn join(&self, url: String, token: String) {
        let _ = self.tx.send(PlayerCmd::Join { url, token });
    }
    pub fn leave(&self) {
        let _ = self.tx.send(PlayerCmd::Leave);
    }
    pub fn play(&self, uri: String, pos_ms: u32) {
        let _ = self.tx.send(PlayerCmd::Play { uri, pos_ms });
    }
    pub fn toggle(&self) {
        let _ = self.tx.send(PlayerCmd::Toggle);
    }
    pub fn stop(&self) {
        let _ = self.tx.send(PlayerCmd::Stop);
    }
    pub fn seek(&self, position_ms: u32) {
        let _ = self.tx.send(PlayerCmd::Seek(position_ms));
    }
    pub fn volume(&self, volume: f32) {
        let _ = self.tx.send(PlayerCmd::Volume(volume));
    }
}

/// Decrypted Ogg bytes per track URI, shared by repeat plays and seeks.
///
/// Bounded by *bytes*, not track count: a four-minute 320 kbps track decrypts
/// to roughly 10 MB, so a plain 20-entry cap quietly reserved ~200 MB. Eviction
/// is oldest-first, and only ever drops what it must — the previous code wiped
/// every entry at once, so the next track re-downloaded the whole working set.
#[derive(Default)]
struct TrackCache {
    map: HashMap<String, Arc<Vec<u8>>>,
    order: VecDeque<String>,
    bytes: usize,
}

impl TrackCache {
    fn get(&self, uri: &str) -> Option<Arc<Vec<u8>>> {
        self.map.get(uri).map(Arc::clone)
    }

    fn insert(&mut self, uri: String, data: Arc<Vec<u8>>) {
        if let Some(old) = self.map.remove(&uri) {
            self.bytes -= old.len();
            self.order.retain(|u| *u != uri);
        }
        self.bytes += data.len();
        self.order.push_back(uri.clone());
        self.map.insert(uri, data);
        while self.bytes > CACHE_BUDGET_BYTES && self.order.len() > 1 {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(dropped) = self.map.remove(&oldest) {
                self.bytes -= dropped.len();
            }
        }
    }
}

struct PlayerState {
    events: flume::Sender<RoomEvent>,
    /// Lets a finished background load post itself back into the command loop.
    self_tx: flume::Sender<PlayerCmd>,
    http: reqwest::blocking::Client,
    client: Option<RoomClient>,
    cache: TrackCache,
    /// Bumped on every `Play`, so a slow load that lands after the user has
    /// moved on is discarded instead of hijacking the sink.
    load_gen: u64,
    stream: Option<rodio::OutputStream>,
    sink: Option<rodio::Sink>,
    current: Option<String>,
    base_pos_ms: u32,
    paused: bool,
    volume: f32,
    last_pos: Instant,
}

impl PlayerState {
    fn new(events: flume::Sender<RoomEvent>, self_tx: flume::Sender<PlayerCmd>) -> Self {
        Self {
            events,
            self_tx,
            http: RoomClient::http(),
            client: None,
            cache: TrackCache::default(),
            load_gen: 0,
            stream: None,
            sink: None,
            current: None,
            base_pos_ms: 0,
            paused: false,
            volume: 1.0,
            last_pos: Instant::now(),
        }
    }

    fn run(&mut self, rx: flume::Receiver<PlayerCmd>) {
        loop {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match rx.recv_timeout(POLL) {
                    Ok(cmd) => self.handle(cmd),
                    Err(flume::RecvTimeoutError::Timeout) => self.tick(),
                    Err(flume::RecvTimeoutError::Disconnected) => return None,
                }
                Some(())
            }));
            match outcome {
                Ok(Some(())) => {}
                Ok(None) => break,
                Err(_) => {
                    self.send(RoomEvent::Error {
                        message: "room player hit an internal error and reset".to_string(),
                    });
                    *self = Self::new(self.events.clone(), self.self_tx.clone());
                }
            }
        }
    }

    fn send(&self, ev: RoomEvent) {
        let _ = self.events.send(ev);
    }

    fn handle(&mut self, cmd: PlayerCmd) {
        match cmd {
            PlayerCmd::Join { url, token } => match RoomClient::join(&self.http, &url, &token) {
                Ok(()) => {
                    self.client = Some(RoomClient { url, token });
                    self.send(RoomEvent::Joined {
                        ok: true,
                        message: "room joined".to_string(),
                    });
                }
                Err(e) => self.send(RoomEvent::Joined {
                    ok: false,
                    message: format!("join failed: {e:#}"),
                }),
            },
            PlayerCmd::Leave => {
                self.client = None;
                self.stop_all();
                self.stream = None; // hand the audio device back
            }
            PlayerCmd::Play { uri, pos_ms } => self.begin_play(uri, pos_ms),
            PlayerCmd::Loaded {
                uri,
                pos_ms,
                gen,
                result,
            } => self.finish_play(uri, pos_ms, gen, result),
            PlayerCmd::Toggle => {
                let Some(sink) = &self.sink else { return };
                let uri = self.current.clone().unwrap_or_default();
                if self.paused {
                    sink.play();
                    self.paused = false;
                    self.send(RoomEvent::Playback(EngineEvent::Playing {
                        uri,
                        position_ms: self.position(),
                    }));
                } else {
                    sink.pause();
                    self.paused = true;
                    self.send(RoomEvent::Playback(EngineEvent::Paused {
                        uri,
                        position_ms: self.position(),
                    }));
                }
            }
            PlayerCmd::Stop => self.stop_all(),
            PlayerCmd::Seek(pos) => self.seek_to(pos),
            PlayerCmd::Volume(v) => {
                self.volume = v;
                if let Some(s) = &self.sink {
                    s.set_volume(v);
                }
            }
        }
    }

    /// Start playing `uri`, fetching it first if it is not already cached.
    ///
    /// A resolve plus a ~10 MB CDN download can take tens of seconds, so it
    /// runs on a throwaway thread and posts itself back as [`PlayerCmd::Loaded`].
    /// Doing it inline blocked the command loop: pause, stop, leave and volume
    /// all sat unread, and `tick` stopped reporting the playhead.
    fn begin_play(&mut self, uri: String, pos_ms: u32) {
        let Some(client) = self.client.clone() else {
            self.send(RoomEvent::Error {
                message: "not joined to a room — press J in the Room view first".to_string(),
            });
            return;
        };
        self.load_gen = self.load_gen.wrapping_add(1);
        let gen = self.load_gen;

        if let Some(bytes) = self.cache.get(&uri) {
            self.start_loaded(uri, pos_ms, bytes);
            return;
        }

        self.send(RoomEvent::Loading { uri: uri.clone() });
        let http = self.http.clone();
        let back = self.self_tx.clone();
        std::thread::spawn(move || {
            let result = fetch_track(&http, &client, &uri).map_err(|e| format!("{e:#}"));
            let _ = back.send(PlayerCmd::Loaded {
                uri,
                pos_ms,
                gen,
                result,
            });
        });
    }

    /// A background load landed. Ignore it if the user has since asked for
    /// something else, otherwise cache it and play.
    fn finish_play(&mut self, uri: String, pos_ms: u32, gen: u64, result: Result<Vec<u8>, String>) {
        if gen != self.load_gen {
            return; // superseded by a newer Play
        }
        match result {
            Ok(bytes) => {
                let bytes = Arc::new(bytes);
                self.cache.insert(uri.clone(), Arc::clone(&bytes));
                self.start_loaded(uri, pos_ms, bytes);
            }
            Err(e) => self.fail(format!("room: {e}")),
        }
    }

    fn start_loaded(&mut self, uri: String, pos_ms: u32, bytes: Arc<Vec<u8>>) {
        self.paused = false;
        match self.start(&bytes, pos_ms) {
            Ok(true) => {
                self.current = Some(uri.clone());
                self.send(RoomEvent::Playback(EngineEvent::TrackChanged {
                    uri: uri.clone(),
                }));
                self.send(RoomEvent::Playback(EngineEvent::Playing {
                    uri,
                    position_ms: pos_ms,
                }));
            }
            Ok(false) => self.fail("room: nothing to play at that position".to_string()),
            Err(e) => self.fail(format!("playback failed: {e:#}")),
        }
    }

    /// Open the output device once and hold it for the player's lifetime.
    fn ensure_stream(&mut self) -> Result<()> {
        if self.stream.is_none() {
            let mut stream = rodio::OutputStreamBuilder::from_default_device()
                .and_then(|b| b.open_stream())
                .map_err(|e| anyhow!("no audio output: {e}"))?;
            stream.log_on_drop(false);
            self.stream = Some(stream);
        }
        Ok(())
    }

    /// Put `bytes` on a brand-new sink, starting at `pos_ms` and honouring the
    /// current pause state. `Ok(false)` means `pos_ms` is past the end.
    ///
    /// Deliberately *not* `Sink::clear()` + `append()`. rodio's `clear()` pauses
    /// the sink as a documented side effect ("Removes all currently loaded
    /// `Source`s from the `Sink`, and pauses it") and `append()` never undoes
    /// it — only `play()` does. The old code therefore left decoded audio
    /// buffered on a paused sink: non-empty, playhead pinned at 0:00, UI
    /// happily reporting "playing". `clear()` also blocks until the queue
    /// drains. A fresh sink per track has neither problem.
    fn start(&mut self, bytes: &Arc<Vec<u8>>, pos_ms: u32) -> Result<bool> {
        let Some((header, tail)) = substream_ranges(bytes, pos_ms) else {
            return Ok(false);
        };
        // Build the decoder before touching the device, so a corrupt stream
        // fails without disturbing whatever is currently playing.
        let substream = Substream::new(Arc::clone(bytes), header, tail);
        let decoder = rodio::Decoder::new(substream).map_err(|e| anyhow!("decode: {e}"))?;
        self.ensure_stream()?;
        let stream = self.stream.as_ref().expect("opened just above");

        // Drop the old sink before connecting the new one so the two can never
        // overlap on the mixer.
        self.sink = None;
        let sink = rodio::Sink::connect_new(stream.mixer());
        sink.set_volume(self.volume);
        // Settle the transport state before queueing audio, so resuming into a
        // paused seek can never leak a few audible samples first.
        if self.paused {
            sink.pause();
        } else {
            sink.play();
        }
        sink.append(decoder);
        self.sink = Some(sink);
        self.base_pos_ms = pos_ms;
        self.last_pos = Instant::now();
        Ok(true)
    }

    /// Report a failure and leave the transport idle.
    ///
    /// Clearing `current` matters: a dead sink with `current` still set looks
    /// exactly like "the track just finished" to [`Self::tick`], which would
    /// emit `Ended` and silently skip the queue forward.
    fn fail(&mut self, message: String) {
        self.sink = None;
        self.current = None;
        self.paused = false;
        self.send(RoomEvent::Error { message });
    }

    fn seek_to(&mut self, pos_ms: u32) {
        let Some(uri) = self.current.clone() else { return };
        let Some(bytes) = self.cache.get(&uri) else { return };
        match self.start(&bytes, pos_ms) {
            Ok(true) => self.send(RoomEvent::Playback(EngineEvent::PositionCorrection {
                uri,
                position_ms: pos_ms,
            })),
            // Seeking past the outro finishes the track, exactly as playing
            // through to the end would.
            Ok(false) => {
                self.sink = None;
                self.current = None;
                self.send(RoomEvent::Ended { uri });
            }
            Err(e) => self.fail(format!("seek failed: {e:#}")),
        }
    }

    /// Stop playback. The output device stays open — reopening it per track
    /// is slow and, on CoreAudio, prone to glitching. `Leave` releases it.
    fn stop_all(&mut self) {
        // Discard any in-flight load, or it would start playing after the stop.
        self.load_gen = self.load_gen.wrapping_add(1);
        self.current = None;
        self.paused = false;
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        self.send(RoomEvent::Playback(EngineEvent::Stopped));
    }

    fn tick(&mut self) {
        let Some(sink) = &self.sink else { return };
        if sink.empty() {
            if let Some(uri) = self.current.take() {
                self.sink = None;
                self.paused = false;
                self.send(RoomEvent::Ended { uri });
            }
            return;
        }
        // `sink.is_paused()` is the one that matters: rodio pauses a sink
        // behind your back (see `Self::start`), so our own `paused` flag can
        // read false while the playhead is frozen.
        crate::liblog::liblog(format!(
            "room tick: pos={}ms sink_pos={:?} paused={} sink_paused={} queued={}",
            self.position(),
            sink.get_pos(),
            self.paused,
            sink.is_paused(),
            sink.len()
        ));
        if !self.paused && self.last_pos.elapsed() >= POSITION_TICK {
            if let Some(uri) = self.current.as_ref() {
                self.last_pos = Instant::now();
                self.send(RoomEvent::Playback(EngineEvent::PositionCorrection {
                    uri: uri.clone(),
                    position_ms: self.position(),
                }));
            }
        }
    }

    fn position(&self) -> u32 {
        self.base_pos_ms
            + self
                .sink
                .as_ref()
                .map(|s| s.get_pos().as_millis() as u32)
                .unwrap_or(0)
    }

}

/// Resolve `uri` through the host, download the encrypted file from the CDN,
/// decrypt it (AES-128-CTR) and strip Spotify's proprietary leading packet.
///
/// A free function rather than a method so a loader thread can run it without
/// touching player state; the caller owns caching.
pub(crate) fn fetch_track(
    http: &reqwest::blocking::Client,
    client: &RoomClient,
    uri: &str,
) -> Result<Vec<u8>> {
    let resolved: ResolvedTrack = RoomClient::resolve_track(http, &client.url, &client.token, uri)
        .context("resolve")?;
    let key = decode_key(&resolved.key_hex)?;

    let mut encrypted: Option<Vec<u8>> = None;
    let mut last_err = None;
    for url in &resolved.urls {
        match http.get(url).send() {
            Ok(mut resp) if matches!(resp.status().as_u16(), 200 | 206) => {
                let mut bytes = Vec::new();
                match resp.read_to_end(&mut bytes) {
                    Ok(_) => {
                        encrypted = Some(bytes);
                        break;
                    }
                    Err(e) => last_err = Some(format!("{e}")),
                }
            }
            Ok(resp) => last_err = Some(format!("HTTP {}", resp.status().as_u16())),
            Err(e) => last_err = Some(format!("{e}")),
        }
    }
    let encrypted = encrypted.ok_or_else(|| {
        anyhow!(
            "all {} CDN urls failed ({})",
            resolved.urls.len(),
            last_err.unwrap_or_else(|| "no urls".to_string())
        )
    })?;

    let mut decrypted = Vec::with_capacity(encrypted.len());
    AudioDecrypt::new(Some(AudioKey(key)), Cursor::new(encrypted))
        .read_to_end(&mut decrypted)
        .context("decrypt")?;

    let header_end = if decrypted.len() > OGG_HEADER_END
        && decrypted[OGG_HEADER_END..].starts_with(b"OggS")
    {
        OGG_HEADER_END
    } else {
        decrypted
            .windows(4)
            .position(|w| w == b"OggS")
            .context("decrypted audio contains no Ogg stream — wrong key?")?
    };
    decrypted.drain(..header_end);
    Ok(decrypted)
}

/// Parse the host's 32-char hex audio key.
///
/// The host is remote and may be hostile, so this validates before indexing:
/// slicing a `&str` by byte offsets panics when a multi-byte character
/// straddles a boundary, and a bad room must never take the guest down.
fn decode_key(hex: &str) -> Result<[u8; 16]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 32 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        bail!("expected 32 hex characters, got {:?}", hex.len());
    }
    let mut key = [0u8; 16];
    for (i, byte) in key.iter_mut().enumerate() {
        let pair = std::str::from_utf8(&bytes[2 * i..2 * i + 2]).expect("ascii checked above");
        *byte = u8::from_str_radix(pair, 16).context("bad key hex")?;
    }
    Ok(key)
}

/// Ogg page boundaries as (offset, length).
fn ogg_pages(data: &[u8]) -> Vec<(usize, usize)> {
    let mut pages = Vec::new();
    let mut pos = 0;
    while pos + 27 <= data.len() && &data[pos..pos + 4] == b"OggS" {
        let nsegs = data[pos + 26] as usize;
        let seg_end = pos + 27 + nsegs;
        if seg_end > data.len() {
            break;
        }
        let seg_sum: usize = data[pos + 27..seg_end].iter().map(|&b| b as usize).sum();
        let page_end = seg_end + seg_sum;
        if page_end > data.len() {
            break;
        }
        pages.push((pos, page_end - pos));
        pos = page_end;
    }
    pages
}

/// Index of the first audio page — the page after the three Vorbis header
/// packets (id, comment, setup), each of which ends on a lacing < 255.
fn audio_start_index(data: &[u8], pages: &[(usize, usize)]) -> usize {
    let mut packet_ends = 0;
    for (i, &(off, _len)) in pages.iter().enumerate() {
        let nsegs = data[off + 26] as usize;
        let segs = &data[off + 27..off + 27 + nsegs];
        for &lace in segs {
            if lace < 255 {
                packet_ends += 1;
            }
        }
        if packet_ends >= 3 {
            return (i + 1).min(pages.len());
        }
    }
    pages.len()
}

/// The two byte ranges that make a playable Ogg stream starting at `pos_ms`:
/// the codec header pages, then every audio page from that position on. Both
/// runs are contiguous, so no copying is needed to splice them.
///
/// Returns `None` when `pos_ms` lands past the last audio page, so the caller
/// can treat the seek as end-of-track. Falling back to "play everything" here
/// would silently restart the song from 0:00 on a seek to the outro.
pub(crate) fn substream_ranges(data: &[u8], pos_ms: u32) -> Option<(Range<usize>, Range<usize>)> {
    let pages = ogg_pages(data);
    if pages.is_empty() {
        return None;
    }
    let audio_start = audio_start_index(data, &pages);
    if audio_start >= pages.len() {
        return None; // headers only — there is no audio to play
    }
    let target_samples = (pos_ms as u64).saturating_mul(SAMPLE_RATE) / 1000;
    let target_samples = i64::try_from(target_samples).unwrap_or(i64::MAX);

    let first_audio_page = if pos_ms == 0 {
        // Play from the top verbatim. Scanning granules here would skip any
        // leading page carrying a continued packet, which Ogg marks with a
        // granule of -1 — dropping it can leave the stream undecodable.
        0
    } else {
        pages[audio_start..].iter().position(|&(off, _len)| {
            let granule = i64::from_le_bytes(data[off + 6..off + 14].try_into().expect("granule"));
            granule >= target_samples
        })?
    };

    let (last_off, last_len) = *pages.last().expect("checked non-empty above");
    Some((
        0..pages[audio_start].0,
        pages[audio_start + first_audio_page].0..(last_off + last_len),
    ))
}

/// A `Read + Seek` view over a track's header pages plus a tail of the same
/// buffer, sharing the cached `Arc` instead of copying.
///
/// The previous version materialised a fresh `Vec` for every play and seek, so
/// a playing track cost twice its size in memory (~20 MB for a four-minute 320
/// kbps file) and each seek re-allocated and memcpy'd the whole thing.
pub(crate) struct Substream {
    data: Arc<Vec<u8>>,
    header: Range<usize>,
    tail: Range<usize>,
    pos: u64,
}

impl Substream {
    pub(crate) fn new(data: Arc<Vec<u8>>, header: Range<usize>, tail: Range<usize>) -> Self {
        Self {
            data,
            header,
            tail,
            pos: 0,
        }
    }

    fn len(&self) -> u64 {
        (self.header.len() + self.tail.len()) as u64
    }

    /// Map a logical offset onto the backing buffer.
    fn source_index(&self, at: usize) -> usize {
        if at < self.header.len() {
            self.header.start + at
        } else {
            self.tail.start + (at - self.header.len())
        }
    }
}

impl Read for Substream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let at = self.pos.min(self.len()) as usize;
        let remaining = self.len() as usize - at;
        if remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        // Never read across the header/tail seam in one go; the caller loops.
        let run_end = if at < self.header.len() {
            self.header.len()
        } else {
            self.len() as usize
        };
        let n = buf.len().min(run_end - at);
        let from = self.source_index(at);
        buf[..n].copy_from_slice(&self.data[from..from + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Substream {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        // i128 throughout: `SeekFrom::Start(u64::MAX) as i64` is -1, which would
        // turn a seek past the end into a spurious "before start" error.
        let target: i128 = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(n) => i128::from(self.len()) + i128::from(n),
            SeekFrom::Current(n) => i128::from(self.pos) + i128::from(n),
        };
        if target < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before start of stream",
            ));
        }
        self.pos = target.min(i128::from(self.len())) as u64;
        Ok(self.pos)
    }
}
