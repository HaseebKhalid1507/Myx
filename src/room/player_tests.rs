//! Device-free tests for the room player.
//!
//! The audio ones drive a standalone `rodio` mixer from a background thread,
//! exactly the way a real output device pulls samples. No sound card is
//! involved, so these behave identically on a laptop and in a container.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rodio::buffer::SamplesBuffer;
use rodio::source::Zero;

use super::*;

const CH: u16 = 2;
const SR: u32 = 44_100;

/// Materialise what [`Substream`] streams, for byte-level assertions.
/// `substream_equals_the_materialised_bytes` pins the two together, so every
/// assertion written against this also holds for the zero-copy reader.
fn substream_at(data: &[u8], pos_ms: u32) -> Option<Vec<u8>> {
    let (header, tail) = substream_ranges(data, pos_ms)?;
    let mut out = Vec::with_capacity(header.len() + tail.len());
    out.extend_from_slice(&data[header]);
    out.extend_from_slice(&data[tail]);
    Some(out)
}

fn read_substream(data: &Arc<Vec<u8>>, pos_ms: u32) -> Option<Vec<u8>> {
    let (header, tail) = substream_ranges(data, pos_ms)?;
    let mut s = Substream::new(Arc::clone(data), header, tail);
    let mut out = Vec::new();
    s.read_to_end(&mut out).expect("substream read");
    Some(out)
}

/// A mixer drained by a background thread, standing in for the audio device.
struct Pump {
    mixer: rodio::mixer::Mixer,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Pump {
    fn new() -> Self {
        let (mixer, mut source) = rodio::mixer::mixer(CH, SR);
        // Without a never-ending source the mixer detaches once every sink runs
        // dry, and later appends are never forwarded.
        mixer.add(Zero::new(CH, SR));
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            // ~1 ms of audio per pass keeps stream time near wall-clock.
            let per_ms = (CH as usize) * (SR as usize) / 1000;
            while !flag.load(Ordering::Relaxed) {
                for _ in 0..per_ms {
                    if source.next().is_none() {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        Self {
            mixer,
            stop,
            handle: Some(handle),
        }
    }

    fn sink(&self) -> rodio::Sink {
        rodio::Sink::connect_new(&self.mixer)
    }

    fn wait_for(&self, label: &str, sink: &rodio::Sink, f: impl Fn(&rodio::Sink) -> bool) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if f(sink) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "{label}: timed out (pos={:?}, paused={}, queued={})",
            sink.get_pos(),
            sink.is_paused(),
            sink.len()
        );
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// `secs` seconds of stereo silence — enough to watch a playhead move.
fn tone(secs: u32) -> SamplesBuffer {
    SamplesBuffer::new(CH, SR, vec![0.0f32; (CH as usize) * (SR as usize) * (secs as usize)])
}

// ------------------------------------------------------- the sink contract

/// The behaviour that caused "position frozen at 0:00, sink non-empty, we
/// never paused it": `Sink::clear()` pauses the sink, and `append()` does not
/// undo that. Pinned here so an upstream change is caught rather than silently
/// re-enabling the old approach.
#[test]
fn rodio_clear_leaves_the_sink_paused() {
    let pump = Pump::new();
    let sink = pump.sink();
    sink.clear();
    sink.append(tone(2));

    assert!(sink.is_paused(), "clear() must be assumed to pause the sink");
    assert!(!sink.empty(), "the appended source is still queued");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        sink.get_pos(),
        Duration::ZERO,
        "a paused sink buffers audio without ever advancing the playhead"
    );
}

/// What `PlayerState::start` actually does: a fresh sink, appended, played.
#[test]
fn fresh_sink_plays() {
    let pump = Pump::new();
    let sink = pump.sink();
    sink.append(tone(2));
    sink.play();

    assert!(!sink.empty());
    pump.wait_for("playhead", &sink, |s| s.get_pos() > Duration::ZERO);
}

/// Replacing the sink mid-queue is the common case (track 2 of a playlist).
#[test]
fn replacing_the_sink_plays_the_next_track() {
    let pump = Pump::new();
    let first = pump.sink();
    first.append(tone(2));
    first.play();
    pump.wait_for("first track", &first, |s| s.get_pos() > Duration::ZERO);

    drop(first);
    let second = pump.sink();
    second.append(tone(2));
    second.play();
    pump.wait_for("second track", &second, |s| {
        s.get_pos() > Duration::from_millis(50)
    });
}

/// Starting into a paused state must stay paused — seeking while paused should
/// not silently resume playback.
#[test]
fn a_paused_sink_does_not_advance() {
    let pump = Pump::new();
    let sink = pump.sink();
    // Same order as `PlayerState::start`: transport state, then audio.
    sink.pause();
    sink.append(tone(2));

    assert!(sink.is_paused());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sink.get_pos(), Duration::ZERO);
    assert!(!sink.empty(), "audio is queued, just not playing");
}

/// Resuming a sink that was started paused must pick up from where it stopped.
#[test]
fn a_paused_sink_resumes_on_play() {
    let pump = Pump::new();
    let sink = pump.sink();
    sink.pause();
    sink.append(tone(2));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(sink.get_pos(), Duration::ZERO);

    sink.play();
    pump.wait_for("resumed playhead", &sink, |s| s.get_pos() > Duration::ZERO);
}

// ------------------------------------------------------------ key parsing

#[test]
fn decode_key_roundtrips() {
    assert_eq!(
        decode_key("000102030405060708090a0b0c0d0e0f").unwrap(),
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    );
    assert_eq!(decode_key(&"ff".repeat(16)).unwrap(), [0xff; 16]);
}

#[test]
fn decode_key_rejects_bad_input() {
    assert!(decode_key("").is_err(), "empty");
    assert!(decode_key("00").is_err(), "too short");
    assert!(decode_key(&"0".repeat(33)).is_err(), "too long");
    assert!(decode_key(&"z".repeat(32)).is_err(), "non-hex");
}

/// A hostile host controls this string. Slicing by byte offsets used to risk a
/// panic when a multi-byte character straddled a boundary; it must error out.
#[test]
fn decode_key_rejects_multibyte_without_panicking() {
    let s = "€".repeat(8) + &"a".repeat(8); // 8*3 + 8 = 32 bytes, 16 chars
    assert_eq!(s.len(), 32, "must be 32 *bytes* to reach the old slice path");
    assert!(decode_key(&s).is_err());
}

// ------------------------------------------------- end-to-end guest pipeline
//
// A real HTTP host, real AES-128-CTR, real Ogg Vorbis, real decoding. The only
// thing not exercised is the librespot session behind the host's `/resolve`,
// which needs a Premium account.

/// 6 s of 44.1 kHz stereo Vorbis: 440 Hz for the first 3 s, 880 Hz for the last
/// 3 s. The frequency step is what makes a seek *verifiable* — landing in the
/// second half is audible to an FFT, not just to a page counter.
const TONE_OGG: &[u8] = include_bytes!("testdata/tone.ogg");

const FIXTURE_KEY: [u8; 16] = [
    0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78, 0x87, 0x96, 0xa5, 0xb4, 0xc3, 0xd2, 0xe1, 0xf0,
];

fn key_hex() -> String {
    FIXTURE_KEY.iter().map(|b| format!("{b:02x}")).collect()
}

/// Spotify's on-CDN layout: a proprietary leading packet, then the Ogg stream,
/// all AES-128-CTR encrypted. CTR is symmetric, so the real decrypt path is
/// also how the fixture gets encrypted.
fn spotify_style_payload() -> Vec<u8> {
    let mut plain = vec![0x5au8; OGG_HEADER_END];
    plain.extend_from_slice(TONE_OGG);
    let mut encrypted = Vec::new();
    AudioDecrypt::new(Some(AudioKey(FIXTURE_KEY)), Cursor::new(plain))
        .read_to_end(&mut encrypted)
        .expect("ctr encrypt");
    encrypted
}

/// A stand-in for the host's room server: the same HTTP contract, backed by a
/// fixture instead of a Premium session.
struct FakeHost {
    url: String,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FakeHost {
    fn start(token: &str) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind loopback");
        let port = server.server_addr().to_ip().expect("ip addr").port();
        let url = format!("127.0.0.1:{port}");
        let stop = Arc::new(AtomicBool::new(false));

        let flag = Arc::clone(&stop);
        let token = token.to_string();
        let body_url = url.clone();
        let payload = spotify_style_payload();
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                let Ok(Some(mut req)) = server.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let path = req.url().split('?').next().unwrap_or("/").to_string();
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let json: serde_json::Value =
                    serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                let authed = json.get("token").and_then(|t| t.as_str()) == Some(token.as_str());

                let reply = |req: tiny_http::Request, code: u16, body: String| {
                    let _ = req.respond(
                        tiny_http::Response::from_string(body)
                            .with_status_code(tiny_http::StatusCode(code)),
                    );
                };
                match path.as_str() {
                    "/join" if authed => reply(req, 200, r#"{"ok":true}"#.into()),
                    "/join" => reply(req, 403, r#"{"error":"invalid room token"}"#.into()),
                    "/resolve" if authed => reply(
                        req,
                        200,
                        serde_json::json!({
                            "track": "spotify:track:fixture",
                            "format": "OGG_VORBIS_320",
                            "key_hex": key_hex(),
                            "urls": [format!("http://{body_url}/cdn")],
                        })
                        .to_string(),
                    ),
                    "/resolve" => reply(req, 403, r#"{"error":"invalid room token"}"#.into()),
                    "/cdn" => {
                        let _ = req.respond(tiny_http::Response::from_data(payload.clone()));
                    }
                    _ => reply(req, 404, r#"{"error":"not found"}"#.into()),
                }
            }
        });
        Self {
            url,
            stop,
            handle: Some(handle),
        }
    }

    fn client(&self, token: &str) -> RoomClient {
        RoomClient {
            url: self.url.clone(),
            token: token.to_string(),
        }
    }
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Dominant frequency of a mono signal, via FFT. ~2.7 Hz resolution at 44.1 kHz.
fn dominant_hz(mono: &[f32], sample_rate: u32) -> f32 {
    use rustfft::{num_complex::Complex, FftPlanner};
    const N: usize = 16_384;
    assert!(
        mono.len() >= N,
        "need {N} samples for the FFT, got {}",
        mono.len()
    );
    let mut buf: Vec<Complex<f32>> = mono[..N]
        .iter()
        .map(|&re| Complex { re, im: 0.0 })
        .collect();
    FftPlanner::new().plan_fft_forward(N).process(&mut buf);
    let (bin, _) = buf[1..N / 2]
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.norm().total_cmp(&b.1.norm()))
        .expect("non-empty spectrum");
    (bin + 1) as f32 * sample_rate as f32 / N as f32
}

/// Decode an Ogg stream to mono f32, dropping the first `skip_ms` to step over
/// the codec's resync after a mid-stream start.
fn decode_mono(ogg: Vec<u8>, skip_ms: u32) -> (Vec<f32>, u32) {
    use rodio::Source;
    let decoder = rodio::Decoder::new(Cursor::new(ogg)).expect("decode ogg");
    let rate = decoder.sample_rate();
    let channels = decoder.channels() as usize;
    let skip = (rate as usize) * (skip_ms as usize) / 1000 * channels;
    let mono: Vec<f32> = decoder
        .skip(skip)
        .step_by(channels) // take one channel
        .collect();
    (mono, rate)
}

#[test]
fn fixture_is_the_two_tone_file_the_tests_assume() {
    let (mono, rate) = decode_mono(TONE_OGG.to_vec(), 250);
    assert_eq!(rate, 44_100);
    let hz = dominant_hz(&mono, rate);
    assert!(
        (hz - 440.0).abs() < 5.0,
        "fixture should open on 440 Hz, measured {hz:.1} Hz"
    );
}

/// The whole guest path: join → resolve → CDN download → AES-CTR decrypt →
/// strip Spotify's header. The result must be the original file, byte for byte.
#[test]
fn end_to_end_fetch_reproduces_the_original_audio() {
    let host = FakeHost::start("s3cret");
    let http = RoomClient::http();

    RoomClient::join(&http, &host.url, "s3cret").expect("join should succeed");

    let bytes = fetch_track(&http, &host.client("s3cret"), "spotify:track:fixture")
        .expect("fetch should succeed");
    assert_eq!(
        bytes.len(),
        TONE_OGG.len(),
        "decrypted payload must be exactly the original Ogg"
    );
    assert_eq!(bytes, TONE_OGG, "byte-for-byte round trip");
}

#[test]
fn a_wrong_token_is_rejected_at_both_endpoints() {
    let host = FakeHost::start("s3cret");
    let http = RoomClient::http();

    let joined = RoomClient::join(&http, &host.url, "guess");
    assert!(joined.is_err(), "join must reject a bad token");
    assert!(format!("{:#}", joined.unwrap_err()).contains("invalid room token"));

    let fetched = fetch_track(&http, &host.client("guess"), "spotify:track:fixture");
    assert!(fetched.is_err(), "resolve must reject a bad token");
}

/// The seek is real: starting at 4 s must produce the 880 Hz half of the
/// fixture, not the 440 Hz opening. This is the assertion that would have
/// caught the "seek restarts the track" bug by ear.
#[test]
fn seeking_lands_in_the_right_part_of_the_track() {
    let host = FakeHost::start("s3cret");
    let http = RoomClient::http();
    let bytes = fetch_track(&http, &host.client("s3cret"), "spotify:track:fixture").unwrap();

    let opening = substream_at(&bytes, 0).expect("position 0 must resolve");
    let (mono, rate) = decode_mono(opening, 250);
    let hz = dominant_hz(&mono, rate);
    assert!((hz - 440.0).abs() < 5.0, "0 s should be 440 Hz, got {hz:.1}");

    let outro = substream_at(&bytes, 4_000).expect("4 s must resolve");
    let (mono, rate) = decode_mono(outro, 250);
    let hz = dominant_hz(&mono, rate);
    assert!(
        (hz - 880.0).abs() < 5.0,
        "4 s should be 880 Hz, got {hz:.1} — a seek that lands at 440 Hz means \
         the track silently restarted"
    );
}

#[test]
fn seeking_past_a_real_track_reports_end_of_track() {
    let host = FakeHost::start("s3cret");
    let http = RoomClient::http();
    let bytes = fetch_track(&http, &host.client("s3cret"), "spotify:track:fixture").unwrap();

    assert!(substream_at(&bytes, 5_500).is_some(), "5.5 s is still inside");
    assert!(
        substream_at(&bytes, 9_000).is_none(),
        "9 s is past a 6 s track"
    );
}

/// Real audio, on a real sink, driven by a real mixer: the playhead must move
/// and the samples must not be silence. This is the end of the chain that was
/// broken — it would have failed outright before the fix.
#[test]
fn real_audio_actually_plays_through_a_sink() {
    let host = FakeHost::start("s3cret");
    let http = RoomClient::http();
    let bytes = fetch_track(&http, &host.client("s3cret"), "spotify:track:fixture").unwrap();
    let stream = substream_at(&bytes, 0).expect("position 0");

    let pump = Pump::new();
    let sink = pump.sink();
    // Exactly the order `PlayerState::start` uses.
    sink.play();
    sink.append(rodio::Decoder::new(Cursor::new(stream)).expect("decode"));

    pump.wait_for("real audio playhead", &sink, |s| {
        s.get_pos() > Duration::from_millis(100)
    });
    assert!(!sink.is_paused(), "sink must be playing");

    // And the signal that survived HTTP + AES-CTR is the same audio the
    // fixture holds — not silence, and not attenuated or corrupted.
    let peak = |ogg: Vec<u8>| {
        decode_mono(ogg, 250)
            .0
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };
    let round_tripped = peak(substream_at(&bytes, 0).expect("position 0"));
    let straight_from_disk = peak(TONE_OGG.to_vec());

    assert!(
        round_tripped > 0.01,
        "decoded audio is silence (peak {round_tripped})"
    );
    assert!(
        (round_tripped - straight_from_disk).abs() < 1e-6,
        "the pipeline must be lossless: {round_tripped} through the room vs \
         {straight_from_disk} straight from disk"
    );
    // ffmpeg reports this fixture at max_volume -20.9 dB; 10^(-20.9/20) = 0.0902.
    assert!(
        (round_tripped - 0.0902).abs() < 0.002,
        "decoded peak {round_tripped} should match ffmpeg's independent \
         measurement of the fixture (0.0902)"
    );
}

// ------------------------------------------------ zero-copy substream reader

/// The reader the decoder actually consumes must be byte-identical to the
/// materialised splice, at the track start and after a seek.
#[test]
fn substream_equals_the_materialised_bytes() {
    let data = Arc::new(TONE_OGG.to_vec());
    for pos in [0, 1_000, 2_500, 4_000, 5_500] {
        assert_eq!(
            read_substream(&data, pos),
            substream_at(&data, pos),
            "mismatch at {pos} ms"
        );
    }
    assert!(read_substream(&data, 9_000).is_none(), "past the end");
}

#[test]
fn substream_reads_correctly_across_the_header_seam() {
    let data = Arc::new(TONE_OGG.to_vec());
    let (header, tail) = substream_ranges(&data, 4_000).expect("4 s");
    let expected = substream_at(&data, 4_000).unwrap();

    // One byte at a time walks the header/tail seam the hard way.
    let mut s = Substream::new(Arc::clone(&data), header, tail);
    let mut got = Vec::new();
    let mut byte = [0u8; 1];
    while s.read(&mut byte).unwrap() == 1 {
        got.push(byte[0]);
    }
    assert_eq!(got, expected, "single-byte reads must match");
}

#[test]
fn substream_seeks_like_a_file() {
    let data = Arc::new(TONE_OGG.to_vec());
    let (header, tail) = substream_ranges(&data, 4_000).expect("4 s");
    let expected = substream_at(&data, 4_000).unwrap();
    let mut s = Substream::new(Arc::clone(&data), header, tail);

    assert_eq!(s.seek(SeekFrom::End(0)).unwrap(), expected.len() as u64);
    assert_eq!(s.seek(SeekFrom::Start(0)).unwrap(), 0);

    // A seek into the tail must land on the same byte the splice has there.
    let probe = expected.len() - 10;
    s.seek(SeekFrom::Start(probe as u64)).unwrap();
    let mut rest = Vec::new();
    s.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, &expected[probe..]);

    // Clamping, not panicking, past the end; and an error before the start.
    assert_eq!(
        s.seek(SeekFrom::Start(u64::MAX)).unwrap(),
        expected.len() as u64
    );
    assert_eq!(s.read(&mut [0u8; 8]).unwrap(), 0, "reads past end are EOF");
    assert!(s.seek(SeekFrom::Start(0)).is_ok());
    assert!(s.seek(SeekFrom::Current(-1)).is_err());
}

/// The point of the reader: playing a track must not double its memory.
#[test]
fn substream_does_not_copy_the_track() {
    let data = Arc::new(TONE_OGG.to_vec());
    let (header, tail) = substream_ranges(&data, 0).expect("position 0");
    let before = Arc::strong_count(&data);
    let s = Substream::new(Arc::clone(&data), header, tail);
    assert_eq!(
        Arc::strong_count(&data),
        before + 1,
        "the reader must share the cached buffer, not clone it"
    );
    drop(s);
    assert_eq!(Arc::strong_count(&data), before);
}

/// And it still decodes to the right audio — 4 s is the 880 Hz half.
#[test]
fn substream_reader_decodes_to_the_right_tone() {
    let data = Arc::new(TONE_OGG.to_vec());
    let (header, tail) = substream_ranges(&data, 4_000).expect("4 s");
    let decoder = rodio::Decoder::new(Substream::new(Arc::clone(&data), header, tail))
        .expect("decode from the zero-copy reader");

    use rodio::Source;
    let rate = decoder.sample_rate();
    let channels = decoder.channels() as usize;
    let skip = (rate as usize) * 250 / 1000 * channels;
    let mono: Vec<f32> = decoder.skip(skip).step_by(channels).collect();
    let hz = dominant_hz(&mono, rate);
    assert!(
        (hz - 880.0).abs() < 5.0,
        "the zero-copy reader should decode 880 Hz at 4 s, got {hz:.1}"
    );
}

/// Not a correctness test — run it for numbers:
/// `cargo test --lib substream_cost -- --ignored --nocapture`
#[test]
#[ignore = "measurement, not an assertion"]
fn substream_cost_report() {
    // ~10 MB of structurally valid Ogg: a four-minute 320 kbps track.
    // Each generated page is 27 + 255 + 255*254 bytes.
    const PAGE_BYTES: usize = 27 + 255 + 255 * 254;
    let pages = (10 * 1024 * 1024) / PAGE_BYTES;
    let audio: Vec<i64> = (1..=pages as i64).map(|i| i * 2_048).collect();
    let data = Arc::new(ogg_large(3, &audio));
    let mb = data.len() as f64 / (1024.0 * 1024.0);
    // Halfway through, at the fixture's 2048-samples-per-page / 44.1 kHz.
    let seek_ms = (pages as u32 / 2) * 2_048 * 1000 / 44_100;

    let t = Instant::now();
    let mut copied = 0usize;
    for _ in 0..50 {
        copied += substream_at(&data, seek_ms).expect("splice").len();
    }
    let materialise = t.elapsed();

    let t = Instant::now();
    for _ in 0..50 {
        let (h, tl) = substream_ranges(&data, seek_ms).expect("ranges");
        std::hint::black_box(Substream::new(Arc::clone(&data), h, tl));
    }
    let zero_copy = t.elapsed();

    println!("\n  track size            {mb:.1} MiB");
    println!("  seek to               {} s", seek_ms / 1000);
    println!(
        "  materialise (old)     {:>8.2?} for 50 seeks, {:.1} MiB allocated",
        materialise,
        copied as f64 / (1024.0 * 1024.0)
    );
    println!(
        "  Substream   (new)     {:>8.2?} for 50 seeks, 0 MiB allocated\n",
        zero_copy
    );
}

/// Like `ogg`, but with 255-byte payloads so a realistic file size is reachable.
fn ogg_large(headers: usize, audio: &[i64]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut page = |granule: i64, segs: usize| {
        out.extend_from_slice(b"OggS");
        out.push(0);
        out.push(0);
        out.extend_from_slice(&granule.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&[0u8; 4]);
        out.push(segs as u8);
        out.extend(std::iter::repeat_n(254u8, segs)); // < 255 ends each packet
        out.extend(std::iter::repeat_n(0u8, segs * 254));
    };
    for _ in 0..headers {
        page(0, 1);
    }
    for &g in audio {
        page(g, 255);
    }
    out
}

// ------------------------------------------------------------ track cache

fn blob(n: usize) -> Arc<Vec<u8>> {
    Arc::new(vec![0u8; n])
}

#[test]
fn cache_returns_what_it_stored() {
    let mut c = TrackCache::default();
    c.insert("a".into(), blob(10));
    assert_eq!(c.get("a").unwrap().len(), 10);
    assert!(c.get("b").is_none());
}

#[test]
fn cache_reinsert_does_not_double_count_bytes() {
    let mut c = TrackCache::default();
    c.insert("a".into(), blob(10));
    c.insert("a".into(), blob(10));
    assert_eq!(c.bytes, 10, "replacing an entry must not leak its bytes");
    assert_eq!(c.order.len(), 1, "and must not duplicate its order entry");
}

#[test]
fn cache_evicts_oldest_first_over_budget() {
    let mut c = TrackCache::default();
    let half = CACHE_BUDGET_BYTES / 2 + 1;
    c.insert("old".into(), blob(half));
    c.insert("new".into(), blob(half));
    assert!(c.get("old").is_none(), "oldest entry should be evicted");
    assert!(c.get("new").is_some(), "newest entry must survive");
    assert!(c.bytes <= CACHE_BUDGET_BYTES);
}

/// Eviction must never drop the entry just inserted, even if it alone blows the
/// budget — the player is about to play it, and `seek_to` re-reads it.
#[test]
fn cache_keeps_the_newest_entry_even_when_oversized() {
    let mut c = TrackCache::default();
    c.insert("huge".into(), blob(CACHE_BUDGET_BYTES * 2));
    assert!(c.get("huge").is_some());
    assert_eq!(c.order.len(), 1);
}

/// The old cache wiped every entry the moment it hit its cap, so the next
/// track re-downloaded the whole working set.
#[test]
fn cache_evicts_only_what_it_must() {
    let mut c = TrackCache::default();
    let chunk = CACHE_BUDGET_BYTES / 4;
    for name in ["a", "b", "c", "d", "e"] {
        c.insert(name.into(), blob(chunk));
    }
    let kept = ["a", "b", "c", "d", "e"]
        .iter()
        .filter(|n| c.get(n).is_some())
        .count();
    assert!(kept >= 3, "expected most entries retained, kept {kept}");
}

// --------------------------------------------------------------- ogg logic

/// A structurally valid Ogg stream: `headers` header pages (each ending a
/// packet) then audio pages carrying the given granule positions.
fn ogg(headers: usize, audio: &[i64]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut page = |granule: i64, payload: u8| {
        out.extend_from_slice(b"OggS");
        out.push(0); // version
        out.push(0); // header type
        out.extend_from_slice(&granule.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]); // serial
        out.extend_from_slice(&[0u8; 4]); // sequence
        out.extend_from_slice(&[0u8; 4]); // checksum
        out.push(1); // one segment
        out.push(payload); // lacing < 255 ends the packet here
        out.extend(std::iter::repeat_n(0u8, payload as usize));
    };
    for _ in 0..headers {
        page(0, 8);
    }
    for &g in audio {
        page(g, 16);
    }
    out
}

#[test]
fn ogg_pages_tiles_the_buffer_exactly() {
    let data = ogg(3, &[44_100, 88_200, 132_300]);
    let pages = ogg_pages(&data);
    assert_eq!(pages.len(), 6);
    let mut cursor = 0;
    for (off, len) in pages {
        assert_eq!(off, cursor, "pages must be contiguous");
        cursor += len;
    }
    assert_eq!(cursor, data.len());
}

#[test]
fn ogg_pages_drops_a_truncated_tail() {
    let mut data = ogg(3, &[44_100]);
    data.truncate(data.len() - 4);
    assert_eq!(ogg_pages(&data).len(), 3);
}

#[test]
fn ogg_pages_rejects_garbage() {
    assert!(ogg_pages(b"not ogg at all").is_empty());
}

#[test]
fn audio_start_index_skips_the_three_vorbis_headers() {
    let data = ogg(3, &[44_100, 88_200]);
    assert_eq!(audio_start_index(&data, &ogg_pages(&data)), 3);
}

#[test]
fn substream_at_zero_is_the_whole_track() {
    let data = ogg(3, &[44_100, 88_200, 132_300]);
    assert_eq!(
        substream_at(&data, 0).unwrap(),
        data,
        "position 0 must be byte-identical to the source"
    );
}

#[test]
fn substream_at_keeps_headers_and_drops_earlier_audio() {
    // Granules at 1 s, 2 s, 3 s; seeking to 1.5 s keeps headers + the 2 s and 3 s pages.
    let data = ogg(3, &[44_100, 88_200, 132_300]);
    let out = substream_at(&data, 1_500).unwrap();
    let pages = ogg_pages(&out);
    assert_eq!(pages.len(), 5, "3 header pages + 2 audio pages");
    assert!(out.starts_with(b"OggS"));
    let (off, _) = pages[3];
    assert_eq!(
        i64::from_le_bytes(out[off + 6..off + 14].try_into().unwrap()),
        88_200,
        "must resume at the first page at or past the target"
    );
}

#[test]
fn substream_at_lands_exactly_on_a_granule() {
    let data = ogg(3, &[44_100, 88_200]);
    let out = substream_at(&data, 1_000).unwrap();
    assert_eq!(ogg_pages(&out).len(), 5, "the 1 s page itself is included");
}

/// Seeking past the outro must report "no audio here" rather than silently
/// falling back to the whole track — that fallback restarted songs from 0:00.
#[test]
fn substream_at_past_the_end_is_none() {
    let data = ogg(3, &[44_100, 88_200, 132_300]);
    assert!(substream_at(&data, 60_000).is_none());
}

#[test]
fn substream_at_on_a_headers_only_stream_is_none() {
    let data = ogg(3, &[]);
    assert!(substream_at(&data, 0).is_none());
    assert!(substream_at(&data, 5_000).is_none());
}

/// Ogg marks a page whose packet does not complete with a granule of -1. At
/// position 0 the stream must be handed over verbatim rather than granule-
/// scanned, or such a leading page is dropped and the stream stops decoding.
#[test]
fn substream_at_zero_keeps_a_continued_first_page() {
    let data = ogg(3, &[-1, 44_100, 88_200]);
    let out = substream_at(&data, 0).unwrap();
    assert_eq!(out, data, "position 0 must not drop the -1 granule page");
    assert_eq!(ogg_pages(&out).len(), 6);
}

#[test]
fn substream_at_on_garbage_is_none() {
    assert!(substream_at(b"not ogg at all", 1_000).is_none());
}

/// `pos_ms * 44_100` overflows a u32 well before a real track ends; the
/// conversion must saturate instead of wrapping into a negative granule.
#[test]
fn substream_at_does_not_overflow_on_absurd_positions() {
    let data = ogg(3, &[44_100, 88_200]);
    assert!(substream_at(&data, u32::MAX).is_none());
}
