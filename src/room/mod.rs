//! The listening room — ONE premium session, N listeners, each their own song.
//!
//! The host's Myx runs a tiny HTTP room server on top of the live engine
//! session. A guest (any Spotify account, free or not) joins with a room
//! token and asks the host's *premium* session to resolve a track — file id,
//! AES key, signed CDN URLs. The guest then downloads the encrypted file
//! straight from Spotify's CDN and plays it locally, so:
//!
//!   - the host's session never plays, and the account's single-stream rule
//!     is untouched;
//!   - the guest gets premium bitrate (320 kbps) even on a free account;
//!   - audio bytes flow CDN → guest, never through the host twice.
//!
//! Hosting and joining are mutually exclusive: a guest never hosts, a host
//! never guests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use librespot_core::audio_key::AudioKey;
use librespot_core::cdn_url::CdnUrl;
use librespot_core::spotify_uri::SpotifyUri;
use librespot_core::{Session, SpotifyId};
use librespot_metadata::audio::{AudioFileFormat, AudioItem};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tiny_http::{Header, Response, StatusCode};

use crate::engine::Engine;
use crate::liblog::liblog;
use crate::room::player::RoomPlayer;

pub(crate) mod player;
pub mod guest_probe;

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod tests;

pub use player::RoomEvent;

/// Default port the room server binds. Everything else about the room is set
/// from the Room view (keys `h` host, `J` join, `L` leave).
pub const ROOM_PORT: u16 = 8787;
/// storage-resolve URLs are valid 24h; re-resolve after 12h to be safe.
const RESOLVE_TTL: Duration = Duration::from_secs(12 * 3600);
/// How long a resolve may take before the guest gives up.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(45);
/// Guest HTTP timeout — covers the big CDN downloads too.
pub(crate) const ROOM_HTTP_TIMEOUT: Duration = Duration::from_secs(180);
/// Concurrent request handlers on the host.
///
/// tiny_http is thread-per-request and a request waiting on a resolve holds its
/// handler thread, so this is the hard ceiling on guests that can be waiting at
/// once — not merely a throughput knob. Threads parked on a channel cost almost
/// nothing, so it is sized for a roomful of people rather than for the machine.
const SERVER_WORKERS: usize = 64;
/// Stack per handler thread. They serialise small JSON and nothing else, so the
/// 2 MiB default would reserve 128 MiB of address space for no reason.
const WORKER_STACK_BYTES: usize = 512 * 1024;
/// Audio-key requests allowed in flight upstream at once.
///
/// Every one of them multiplexes over the host's single librespot session, and
/// a burst of key requests is both slow and exactly the pattern that makes an
/// account conspicuous. Deliberately small: coalescing, not parallelism, is
/// what makes this scale.
const UPSTREAM_CONCURRENCY: usize = 8;
/// How long a worker waits for a request before re-checking the stop flag.
const ACCEPT_POLL: Duration = Duration::from_millis(250);

/// The signed-url + key payload the host mints and the guest plays.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ResolvedTrack {
    pub(crate) track: String,
    pub(crate) format: String,
    pub(crate) key_hex: String,
    pub(crate) urls: Vec<String>,
}

/// A resolved track, cached 12h so repeated plays never re-hit the session.
#[derive(Debug)]
struct Track {
    key: AudioKey,
    format: AudioFileFormat,
    urls: Vec<String>,
    resolved_at: Instant,
}

/// Everyone waiting on one in-flight resolve. The error is a `String` rather
/// than an `anyhow::Error` so a single failure can be handed to every waiter.
type Waiters = Vec<flume::Sender<Result<Arc<Track>, String>>>;

/// Shared between the request handlers and the resolve worker.
#[derive(Default)]
struct ResolveState {
    /// Resolved tracks, good for `RESOLVE_TTL`.
    tracks: RwLock<HashMap<String, Arc<Track>>>,
    /// URIs being resolved right now, and who is waiting on each.
    inflight: Mutex<HashMap<String, Waiters>>,
    hits: AtomicU64,
    coalesced: AtomicU64,
    upstream: AtomicU64,
}

impl ResolveState {
    fn fresh(&self, uri: &str) -> Option<Arc<Track>> {
        let tracks = self.tracks.read().unwrap();
        tracks
            .get(uri)
            .filter(|t| t.resolved_at.elapsed() < RESOLVE_TTL)
            .map(Arc::clone)
    }

    /// Publish a finished resolve to the cache and to every waiter.
    ///
    /// The two locks are taken in separate scopes on purpose: `get` takes
    /// `inflight` and then `tracks`, so holding both here in the other order
    /// would be a lock-order inversion.
    fn complete(&self, uri: &str, result: Result<Arc<Track>, String>) {
        if let Ok(track) = &result {
            let mut tracks = self.tracks.write().unwrap();
            // Signed URLs expire, so stale entries are dead weight either way.
            tracks.retain(|_, t| t.resolved_at.elapsed() < RESOLVE_TTL);
            tracks.insert(uri.to_string(), Arc::clone(track));
        }
        let waiters = self.inflight.lock().unwrap().remove(uri).unwrap_or_default();
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
    }
}

/// Memoized, single-flight, concurrency-bounded track resolution.
///
/// The scaling property that matters is not parallelism — it is that a hundred
/// guests pressing play on the same song cost **one** audio-key request. Naive
/// caching does not give you that: a hundred simultaneous misses all miss, and
/// all stampede upstream. So callers register interest first, and only the
/// first one for a given URI actually dispatches.
///
/// `upstream` is injected so the concurrency behaviour can be tested without a
/// Spotify session.
pub(crate) struct Resolver {
    state: Arc<ResolveState>,
    tx: flume::Sender<String>,
}

impl Resolver {
    fn spawn<F, Fut>(upstream: F, concurrency: usize) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Arc<Track>, String>> + Send + 'static,
    {
        let (tx, rx) = flume::unbounded::<String>();
        let state = Arc::new(ResolveState::default());
        let worker_state = Arc::clone(&state);
        let upstream = Arc::new(upstream);

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    liblog(format!("room: resolve runtime failed: {e}"));
                    return;
                }
            };
            rt.block_on(async move {
                let permits = Arc::new(tokio::sync::Semaphore::new(concurrency));
                while let Ok(uri) = rx.recv_async().await {
                    // Block here rather than spawning unbounded: back-pressure
                    // belongs upstream of the session, not on Spotify.
                    let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                        break;
                    };
                    let state = Arc::clone(&worker_state);
                    let upstream = Arc::clone(&upstream);
                    tokio::spawn(async move {
                        let result = upstream(uri.clone()).await;
                        state.complete(&uri, result);
                        drop(permit);
                    });
                }
            });
        });

        Self { state, tx }
    }

    /// Resolve `uri`, sharing the work with anyone already waiting on it.
    fn get(&self, uri: &str) -> Result<Arc<Track>, String> {
        if let Some(track) = self.state.fresh(uri) {
            self.state.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(track);
        }

        let (tx, rx) = flume::bounded(1);
        let dispatch = {
            let mut inflight = self.state.inflight.lock().unwrap();
            // Re-check under the lock: the resolve we just missed may have
            // landed while we were between the two.
            if let Some(track) = self.state.fresh(uri) {
                self.state.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(track);
            }
            match inflight.get_mut(uri) {
                Some(waiters) => {
                    waiters.push(tx);
                    false
                }
                None => {
                    inflight.insert(uri.to_string(), vec![tx]);
                    true
                }
            }
        };

        if dispatch {
            self.state.upstream.fetch_add(1, Ordering::Relaxed);
            if self.tx.send(uri.to_string()).is_err() {
                self.state.inflight.lock().unwrap().remove(uri);
                return Err("resolve worker gone".to_string());
            }
        } else {
            self.state.coalesced.fetch_add(1, Ordering::Relaxed);
        }

        match rx.recv_timeout(RESOLVE_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Err(format!(
                "resolve timed out after {}s",
                RESOLVE_TIMEOUT.as_secs()
            )),
        }
    }
}

/// A read-only snapshot of the room server for the UI.
#[derive(Clone)]
pub struct HostInfo {
    pub port: u16,
    pub token: String,
    pub resolves: u64,
    pub joins: u64,
    /// Served straight from cache, no upstream call.
    pub cache_hits: u64,
    /// Folded into an already-running resolve for the same track.
    pub coalesced: u64,
    /// Audio-key requests actually made against the Spotify session.
    pub upstream: u64,
}

/// The host side: an HTTP room server resolving tracks through the engine's
/// live session. All session work happens on one resolve worker thread with
/// its own tokio runtime, so the room never tears down the AP connection.
pub(crate) struct RoomHost {
    server: Arc<tiny_http::Server>,
    port: u16,
    token: RwLock<String>,
    stop: Arc<AtomicBool>,
    joins: Arc<AtomicU64>,
    resolves: Arc<AtomicU64>,
    resolver: Resolver,
}

impl RoomHost {
    pub(crate) fn info(&self) -> HostInfo {
        HostInfo {
            port: self.port,
            token: self.token.read().unwrap().clone(),
            resolves: self.resolves.load(Ordering::Relaxed),
            joins: self.joins.load(Ordering::Relaxed),
            cache_hits: self.resolver.state.hits.load(Ordering::Relaxed),
            coalesced: self.resolver.state.coalesced.load(Ordering::Relaxed),
            upstream: self.resolver.state.upstream.load(Ordering::Relaxed),
        }
    }

    /// Mint a new token and persist it, invalidating every guest immediately.
    ///
    /// The counterpart to persistence: the token now survives restarts, so
    /// there has to be a deliberate way to revoke one that leaked.
    pub(crate) fn rotate_token(&self) -> String {
        let fresh = random_token();
        if let Some(path) = token_path() {
            persist_token(&path, &fresh);
        }
        *self.token.write().unwrap() = fresh.clone();
        fresh
    }

    fn handle(&self, mut request: tiny_http::Request) {
        let method = request.method().as_str().to_string();
        let path = request.url().split('?').next().unwrap_or("/").to_string();
        let body: serde_json::Value = if method == "POST" {
            let mut s = String::new();
            use std::io::Read;
            let _ = request.as_reader().take(1 << 20).read_to_string(&mut s);
            serde_json::from_str(&s).unwrap_or_else(|_| json!({}))
        } else {
            json!({})
        };
        let token_ok = body
            .get("token")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t == self.token.read().unwrap().as_str());
        let respond = |request: tiny_http::Request, code: u16, value: serde_json::Value| {
            let _ = request.respond(
                Response::from_string(value.to_string())
                    .with_status_code(StatusCode(code))
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap()),
            );
        };
        match (method.as_str(), path.as_str()) {
            ("GET", "/info") => respond(
                request,
                200,
                json!({"app": "myx", "room": "0.1", "join": "/join", "resolve": "/resolve"}),
            ),
            ("POST", "/join") => {
                if token_ok {
                    self.joins.fetch_add(1, Ordering::Relaxed);
                    respond(request, 200, json!({"ok": true}));
                } else {
                    respond(request, 403, json!({"error": "invalid room token"}));
                }
            }
            ("POST", "/resolve") => {
                if !token_ok {
                    return respond(request, 403, json!({"error": "invalid room token"}));
                }
                let Some(uri) = body
                    .get("uri")
                    .and_then(|u| u.as_str())
                    .filter(|u| !u.is_empty())
                else {
                    return respond(request, 400, json!({"error": "missing uri"}));
                };
                match self.resolver.get(uri) {
                    Ok(t) => {
                        self.resolves.fetch_add(1, Ordering::Relaxed);
                        let key_hex = t.key.0.iter().map(|b| format!("{b:02x}")).collect::<String>();
                        respond(
                            request,
                            200,
                            json!({
                                "track": uri,
                                "format": format!("{:?}", t.format),
                                "key_hex": key_hex,
                                "urls": t.urls,
                            }),
                        );
                    }
                    Err(e) => respond(request, 400, json!({"error": format!("resolve failed: {e:#}")})),
                }
            }
            _ => respond(request, 404, json!({"error": "not found"})),
        }
    }

}

/// The room service the UI talks to: hosting state plus the guest player.
pub struct RoomHandle {
    host: Arc<Mutex<Option<Arc<RoomHost>>>>,
    pub player: Arc<RoomPlayer>,
}

impl RoomHandle {
    pub fn new(events: flume::Sender<RoomEvent>) -> Arc<Self> {
        Arc::new(Self {
            host: Arc::new(Mutex::new(None)),
            player: RoomPlayer::new(events),
        })
    }

    /// Bind the room server and start resolving through `engine`'s session.
    pub fn start_host(&self, engine: Arc<Engine>, port: u16) -> Result<HostInfo> {
        if self.host.lock().unwrap().is_some() {
            bail!("already hosting a room");
        }
        let token = load_or_create_token(token_path().as_deref());
        let server = Arc::new(
            tiny_http::Server::http(format!("0.0.0.0:{port}"))
                .map_err(|e| anyhow!("bind :{port}: {e}"))?,
        );

        // Resolution goes through a memoized, single-flight, bounded resolver
        // rather than a serial worker: N guests on the same track cost one
        // audio-key request, and distinct tracks overlap instead of queueing.
        let worker_engine = Arc::clone(&engine);
        let resolver = Resolver::spawn(
            move |uri| {
                let engine = Arc::clone(&worker_engine);
                async move {
                    // A fresh session clone per request picks up reconnects.
                    resolve(&engine.session(), &uri)
                        .await
                        .map_err(|e| format!("{e:#}"))
                }
            },
            UPSTREAM_CONCURRENCY,
        );

        let host = Arc::new(RoomHost {
            server: Arc::clone(&server),
            port,
            token: RwLock::new(token),
            stop: Arc::new(AtomicBool::new(false)),
            joins: Arc::new(AtomicU64::new(0)),
            resolves: Arc::new(AtomicU64::new(0)),
            resolver,
        });
        // A fixed pool rather than a thread per request: the old code spawned an
        // unbounded OS thread for every connection, so any client — friendly or
        // not — could exhaust the host's memory just by opening sockets.
        //
        // Each worker polls with a timeout instead of blocking forever, because
        // `Server::unblock` only ever wakes a single thread; the stop flag is
        // what actually shuts the pool down.
        for _ in 0..SERVER_WORKERS {
            let worker = Arc::clone(&host);
            let spawned = std::thread::Builder::new()
                .name("myx-room".into())
                .stack_size(WORKER_STACK_BYTES)
                .spawn(move || {
                while !worker.stop.load(Ordering::Relaxed) {
                    match worker.server.recv_timeout(ACCEPT_POLL) {
                        Ok(Some(request)) => worker.handle(request),
                        Ok(None) => continue, // idle tick — re-check the stop flag
                        Err(_) => break,
                    }
                }
                });
            if let Err(e) = spawned {
                liblog(format!("room: could not start a handler thread: {e}"));
            }
        }

        let info = host.info();
        *self.host.lock().unwrap() = Some(host);
        Ok(info)
    }

    pub fn stop_host(&self) {
        if let Some(host) = self.host.lock().unwrap().take() {
            host.stop.store(true, Ordering::Relaxed);
            host.server.unblock();
        }
    }

    /// Rotate the room token, locking out anyone holding the old one.
    /// `None` when not hosting.
    pub fn rotate_token(&self) -> Option<String> {
        self.host.lock().unwrap().as_ref().map(|h| h.rotate_token())
    }

    pub fn hosting(&self) -> bool {
        self.host.lock().unwrap().is_some()
    }

    pub fn host_info(&self) -> Option<HostInfo> {
        self.host.lock().unwrap().as_ref().map(|h| h.info())
    }
}

/// Resolve a track URI through a session: audio item, key, CDN URLs.
/// This is the account's only cost — it never starts playback.
async fn resolve(session: &Session, uri_str: &str) -> Result<Arc<Track>> {
    let uri = SpotifyUri::from_uri(uri_str)?;
    let track_id: SpotifyId = (&uri).try_into()?;

    let item = AudioItem::get_file(session, uri)
        .await
        .context("get audio item")?;
    let format = [
        AudioFileFormat::OGG_VORBIS_320,
        AudioFileFormat::OGG_VORBIS_160,
        AudioFileFormat::OGG_VORBIS_96,
    ]
    .into_iter()
    .find(|f| item.files.contains_key(f))
    .context("no vorbis file for track")?;
    let file_id = item.files.get(&format).context("missing file id")?;

    let key = session
        .audio_key()
        .request(track_id, *file_id)
        .await
        .context("request audio key")?;
    let cdn = CdnUrl::new(*file_id)
        .resolve_audio(session)
        .await
        .context("resolve CDN storage")?;
    let urls: Vec<String> = cdn.try_get_urls()?.into_iter().map(String::from).collect();

    Ok(Arc::new(Track {
        key,
        format,
        urls,
        resolved_at: Instant::now(),
    }))
}

/// Guest-side HTTP: join a room and resolve tracks through the host's session.
#[derive(Clone)]
pub(crate) struct RoomClient {
    pub(crate) url: String,
    pub(crate) token: String,
}

impl RoomClient {
    pub(crate) fn http() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(ROOM_HTTP_TIMEOUT)
            .build()
            .expect("room http client")
    }

    pub(crate) fn join(http: &reqwest::blocking::Client, url: &str, token: &str) -> Result<()> {
        let resp = http
            .post(format!("http://{url}/join"))
            .json(&json!({"token": token}))
            .send()
            .context("join request")?;
        let status = resp.status().as_u16();
        let text = resp.text().context("join reply")?;
        match status {
            200 => Ok(()),
            403 => bail!("invalid room token"),
            _ => bail!("join failed (HTTP {status}): {text}"),
        }
    }

    pub(crate) fn resolve_track(
        http: &reqwest::blocking::Client,
        url: &str,
        token: &str,
        uri: &str,
    ) -> Result<ResolvedTrack> {
        let resp = http
            .post(format!("http://{url}/resolve"))
            .json(&json!({"token": token, "uri": uri}))
            .send()
            .context("resolve request")?;
        let status = resp.status().as_u16();
        let text = resp.text().context("resolve reply")?;
        if status != 200 {
            bail!("resolve failed (HTTP {status}): {text}");
        }
        Ok(serde_json::from_str(&text)?)
    }
}

fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where the room token lives, alongside the rest of Myx's state.
fn token_path() -> Option<std::path::PathBuf> {
    crate::data_dir().map(|d| d.join("room_token"))
}

fn valid_token(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Write the token 0600 — it is a credential, not configuration.
fn persist_token(path: &std::path::Path, token: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(path, token).is_err() {
        liblog(format!("room: could not persist token to {}", path.display()));
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

/// The room's token, stable across restarts.
///
/// Generating a fresh one per `h` press meant every restart silently locked out
/// every guest, with nothing on their end but `invalid room token`. Persisting
/// it means the host shares it once. Rotation stays available and deliberate —
/// see [`RoomHost::rotate_token`] — because a token you cannot revoke is worse
/// than one that changes behind your back.
fn load_or_create_token(path: Option<&std::path::Path>) -> String {
    let Some(path) = path else {
        return random_token(); // no state dir: in-memory token, as before
    };
    if let Ok(existing) = std::fs::read_to_string(path) {
        let existing = existing.trim();
        if valid_token(existing) {
            return existing.to_string();
        }
        liblog("room: stored token was malformed, generating a new one");
    }
    let token = random_token();
    persist_token(path, &token);
    token
}
