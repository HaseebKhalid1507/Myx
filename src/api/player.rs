//! The Web API's player: commands for the device myx is the remote for, and
//! who's playing what.
//!
//! Everything runs on one worker thread, one request at a time and in order:
//! "shuffle on, then play this" has to arrive in that order, and Spotify says
//! it doesn't promise an order between player calls made at once. Requests
//! that pile up behind a slow one are thinned out first (see [`coalesce`]), so
//! a held `+` is one volume change, not thirty — the Client ID's rate limit is
//! shared with the library.

use super::*;
use crate::*;

/// Most tracks sent in one start request. Spotify documents no limit for
/// `uris`, but a whole large Liked Songs makes a body it won't take, so the
/// list is cut to a window around the picked track.
pub(crate) const MAX_START_URIS: usize = 500;

/// One HTTP call to the player API.
#[derive(Debug, PartialEq)]
pub(crate) struct Call {
    pub(crate) method: &'static str,
    pub(crate) url: String,
    pub(crate) body: Option<serde_json::Value>,
}

impl Call {
    fn get(url: String) -> Self {
        Self {
            method: "GET",
            url,
            body: None,
        }
    }
}

/// The Web API's name for a repeat mode.
pub(crate) fn repeat_state(mode: Repeat) -> &'static str {
    match mode {
        Repeat::Off => "off",
        Repeat::All => "context",
        Repeat::One => "track",
    }
}

fn repeat_from_state(state: &str) -> Repeat {
    match state {
        "track" => Repeat::One,
        "context" => Repeat::All,
        _ => Repeat::Off,
    }
}

/// The call that sends `command` to device `device_id`.
pub(crate) fn command_call(device_id: &str, command: &PlayerCommand) -> Call {
    let device = urlencode(device_id);
    let to = |method: &'static str, path: &str, body: Option<serde_json::Value>| {
        let sep = if path.contains('?') { '&' } else { '?' };
        Call {
            method,
            url: format!("{API}/me/player/{path}{sep}device_id={device}"),
            body,
        }
    };
    match command {
        PlayerCommand::Play => to("PUT", "play", None),
        PlayerCommand::Pause => to("PUT", "pause", None),
        PlayerCommand::Next => to("POST", "next", None),
        PlayerCommand::Previous => to("POST", "previous", None),
        PlayerCommand::Seek(ms) => to("PUT", &format!("seek?position_ms={ms}"), None),
        PlayerCommand::Volume(pct) => to("PUT", &format!("volume?volume_percent={pct}"), None),
        PlayerCommand::Shuffle(on) => to("PUT", &format!("shuffle?state={on}"), None),
        PlayerCommand::Repeat(mode) => to(
            "PUT",
            &format!("repeat?state={}", repeat_state(*mode)),
            None,
        ),
        PlayerCommand::Start(load) => to("PUT", "play", Some(start_body(load))),
    }
}

fn start_body(load: &Load) -> serde_json::Value {
    match load {
        Load::Context {
            uri,
            track,
            position_ms,
        } => {
            let mut body = serde_json::json!({ "context_uri": uri, "position_ms": position_ms });
            // Spotify takes an offset only into an album or a playlist.
            if let Some(track) = track.as_ref().filter(|_| takes_offset(uri)) {
                body["offset"] = serde_json::json!({ "uri": track });
            }
            body
        }
        Load::Tracks {
            uris,
            start,
            position_ms,
        } => {
            let (uris, start) = start_window(uris, start.as_deref(), MAX_START_URIS);
            let mut body = serde_json::json!({ "uris": uris, "position_ms": position_ms });
            if let Some(start) = start {
                body["offset"] = serde_json::json!({ "uri": start });
            }
            body
        }
    }
}

fn takes_offset(context_uri: &str) -> bool {
    context_uri.starts_with("spotify:album:") || context_uri.contains(":playlist:")
}

/// At most `cap` of `uris`, always with `start` among them when it's in the
/// list (a few tracks before it, so `b` has somewhere to go), else from the
/// top. The start comes back only if it's in what's returned.
pub(crate) fn start_window<'a>(
    uris: &'a [String],
    start: Option<&'a str>,
    cap: usize,
) -> (&'a [String], Option<&'a str>) {
    let at = start.and_then(|s| uris.iter().position(|u| u == s));
    if uris.len() <= cap {
        return (uris, at.and(start));
    }
    let from = at.map_or(0, |i| i.saturating_sub(cap / 10).min(uris.len() - cap));
    (&uris[from..from + cap], at.and(start))
}

/// What a refused player call means, for the status line. Spotify's error
/// body carries a `reason` for the cases worth explaining.
pub(crate) fn failure(status: u16, body: &str, retry_after: Option<u64>) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let reason = v["error"]["reason"].as_str().unwrap_or("");
    let message = v["error"]["message"].as_str().unwrap_or("").trim();
    match (status, reason) {
        (_, "PREMIUM_REQUIRED") => "Spotify Premium is needed to control devices".to_string(),
        (404, _) | (_, "NO_ACTIVE_DEVICE") => {
            "it isn't reachable — open Spotify on it (a phone that's asleep drops off)".to_string()
        }
        (401, _) => "Spotify refused the Web API token — restart myx to sign in again".to_string(),
        (403, _) if !message.is_empty() => format!("Spotify refused: {message}"),
        (403, _) => "Spotify refused (HTTP 403)".to_string(),
        (429, _) => match retry_after {
            Some(secs) => format!("Spotify is rate-limiting myx — try again in {secs}s"),
            None => "Spotify is rate-limiting myx — try again shortly".to_string(),
        },
        (500..=599, _) => format!("Spotify had a problem (HTTP {status})"),
        _ if !message.is_empty() => format!("HTTP {status}: {message}"),
        _ => format!("HTTP {status}"),
    }
}

fn web_device(d: &serde_json::Value) -> Device {
    let id = d["id"].as_str().unwrap_or("");
    let volume = d["volume_percent"]
        .as_u64()
        .map(|v| v.min(100) as u8)
        .filter(|_| d["supports_volume"].as_bool().unwrap_or(true));
    Device {
        id: id.to_string(),
        name: d["name"].as_str().unwrap_or("").to_string(),
        kind: DeviceKind::from_web(d["type"].as_str().unwrap_or("")),
        volume,
        is_active: d["is_active"].as_bool().unwrap_or(false),
        // Spotify: a restricted device takes no Web API commands. One without
        // an id can't be named in one.
        controllable: !id.is_empty() && !d["is_restricted"].as_bool().unwrap_or(false),
        is_this: false,
    }
}

/// What `GET /me/player` says is playing, and where. `None` when no device is
/// active (Spotify answers 204, or a body without a device).
pub(crate) fn parse_player(v: &serde_json::Value) -> Option<Playback> {
    if !v["device"].is_object() {
        return None;
    }
    let item = &v["item"];
    let ms = |x: &serde_json::Value| u32::try_from(x.as_u64().unwrap_or(0)).unwrap_or(u32::MAX);
    Some(Playback {
        device: Device {
            is_active: true,
            ..web_device(&v["device"])
        },
        track_uri: item["uri"].as_str().unwrap_or("").to_string(),
        context_uri: v["context"]["uri"].as_str().unwrap_or("").to_string(),
        context_name: String::new(),
        // Measured as the reply left Spotify, so already the playhead now.
        // (`timestamp` is when playback last changed, not when this was read.)
        position_ms: ms(&v["progress_ms"]),
        duration_ms: ms(&item["duration_ms"]),
        is_playing: v["is_playing"].as_bool().unwrap_or(false),
        shuffle: v["shuffle_state"].as_bool().unwrap_or(false),
        repeat: repeat_from_state(v["repeat_state"].as_str().unwrap_or("off")),
    })
}

/// Requests that piled up behind a slow one, minus those a later one makes
/// pointless: of back-to-back volume changes or seeks for the same device only
/// the last is sent, and of back-to-back state checks, one. Order is kept.
pub(crate) fn coalesce(batch: Vec<PlayerRequest>) -> Vec<PlayerRequest> {
    let mut out: Vec<PlayerRequest> = Vec::with_capacity(batch.len());
    for request in batch {
        if out.last().is_some_and(|last| superseded(last, &request)) {
            out.pop();
        }
        out.push(request);
    }
    out
}

fn superseded(earlier: &PlayerRequest, later: &PlayerRequest) -> bool {
    use PlayerCommand::{Seek, Volume};
    match (earlier, later) {
        (
            PlayerRequest::Command {
                device_id: a,
                command: Volume(_),
                ..
            },
            PlayerRequest::Command {
                device_id: b,
                command: Volume(_),
                ..
            },
        )
        | (
            PlayerRequest::Command {
                device_id: a,
                command: Seek(_),
                ..
            },
            PlayerRequest::Command {
                device_id: b,
                command: Seek(_),
                ..
            },
        ) => a == b,
        (PlayerRequest::State, PlayerRequest::State) => true,
        _ => false,
    }
}

/// Make one call: the reply's JSON on success (`None` for an empty one), or
/// what went wrong. One short 429 is waited out, as `get_json` does.
fn call_api(
    client: &reqwest::blocking::Client,
    token: &str,
    call: &Call,
) -> Result<Option<serde_json::Value>, String> {
    let mut retry_after = None;
    for attempt in 0..2 {
        let req = match call.method {
            "PUT" => client.put(&call.url),
            "POST" => client.post(&call.url),
            _ => client.get(&call.url),
        }
        .bearer_auth(token);
        let req = match (&call.body, call.method) {
            (Some(body), _) => req.json(body),
            // Spotify answers 411 to a PUT or POST without a length.
            (None, "PUT" | "POST") => req.header("Content-Length", "0"),
            (None, _) => req,
        };
        let resp = req.send().map_err(|e| {
            if e.is_timeout() {
                "Spotify didn't answer in time".to_string()
            } else {
                "network error".to_string()
            }
        })?;
        let status = resp.status().as_u16();
        if resp.status().is_success() {
            return Ok(resp.json::<serde_json::Value>().ok());
        }
        retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        if status == 429 && attempt == 0 {
            if let Some(wait) = retry_delay(retry_after) {
                liblog(format!(
                    "player: {} -> 429, retry-after {retry_after:?}s, waiting {}s",
                    call.url,
                    wait.as_secs()
                ));
                std::thread::sleep(wait);
                continue;
            }
        }
        // Swallowing the status made a refusal look like nothing happening.
        let body = resp.text().unwrap_or_default();
        liblog(format!(
            "player: {} {} -> HTTP {status}: {body}",
            call.method, call.url
        ));
        return Err(failure(status, &body, retry_after));
    }
    Err(failure(429, "", retry_after))
}

/// `GET /me/player`: what the active device is playing, if any device is.
pub(crate) fn fetch_player_state(webapi: &Arc<Mutex<WebApi>>) -> Result<Option<Playback>, String> {
    let token = token_of(webapi).ok_or("not signed in to the Web API")?;
    let reply = call_api(
        &http_client(),
        &token,
        &Call::get(format!("{API}/me/player")),
    )?;
    Ok(reply.as_ref().and_then(parse_player))
}

fn run(
    client: &reqwest::blocking::Client,
    webapi: &Arc<Mutex<WebApi>>,
    request: PlayerRequest,
) -> Option<PlayerReply> {
    match request {
        PlayerRequest::Command {
            device_id,
            device_name,
            command,
        } => {
            let sent = match token_of(webapi) {
                Some(token) => call_api(client, &token, &command_call(&device_id, &command)),
                None => Err("not signed in to the Web API".to_string()),
            };
            sent.err().map(|why| {
                PlayerReply::Status(format!(
                    "couldn't {} on {device_name}: {why}",
                    command.verb()
                ))
            })
        }
        PlayerRequest::State => Some(PlayerReply::State(fetch_player_state(webapi))),
    }
}

/// Start the player worker: a thread of its own, since its HTTP blocks and it
/// lives as long as the app. It ends when every sender is gone; a plain
/// thread, so quitting never waits on a call in flight.
pub(crate) fn spawn_player_worker(
    webapi: Arc<Mutex<WebApi>>,
    requests: flume::Receiver<PlayerRequest>,
    replies: flume::Sender<PlayerReply>,
) {
    let spawned = std::thread::Builder::new()
        .name("myx-player".to_string())
        .spawn(move || {
            let client = http_client();
            while let Ok(first) = requests.recv() {
                let mut batch = vec![first];
                batch.extend(requests.try_iter());
                for request in coalesce(batch) {
                    if let Some(reply) = run(&client, &webapi, request) {
                        if replies.send(reply).is_err() {
                            return;
                        }
                    }
                }
            }
        });
    if let Err(e) = spawned {
        liblog(format!(
            "player: no worker thread ({e}); other devices can't be controlled"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DEV: &str = "abc def";

    fn call(command: PlayerCommand) -> Call {
        command_call(DEV, &command)
    }

    #[test]
    fn repeat_steps_map_to_the_web_apis_names_and_back() {
        for (mode, state) in [
            (Repeat::Off, "off"),
            (Repeat::All, "context"),
            (Repeat::One, "track"),
        ] {
            assert_eq!(repeat_state(mode), state);
            assert_eq!(repeat_from_state(state), mode);
        }
    }

    #[test]
    fn each_command_goes_to_its_endpoint_on_that_device() {
        let at = |path: &str| format!("{API}/me/player/{path}");
        for (command, method, url) in [
            (PlayerCommand::Play, "PUT", at("play?device_id=abc%20def")),
            (PlayerCommand::Pause, "PUT", at("pause?device_id=abc%20def")),
            (PlayerCommand::Next, "POST", at("next?device_id=abc%20def")),
            (
                PlayerCommand::Previous,
                "POST",
                at("previous?device_id=abc%20def"),
            ),
            (
                PlayerCommand::Seek(61_000),
                "PUT",
                at("seek?position_ms=61000&device_id=abc%20def"),
            ),
            (
                PlayerCommand::Volume(35),
                "PUT",
                at("volume?volume_percent=35&device_id=abc%20def"),
            ),
            (
                PlayerCommand::Shuffle(true),
                "PUT",
                at("shuffle?state=true&device_id=abc%20def"),
            ),
            (
                PlayerCommand::Repeat(Repeat::One),
                "PUT",
                at("repeat?state=track&device_id=abc%20def"),
            ),
        ] {
            let c = call(command.clone());
            assert_eq!(
                (c.method, c.url.as_str()),
                (method, url.as_str()),
                "{command:?}"
            );
            assert_eq!(c.body, None, "{command:?}");
        }
    }

    #[test]
    fn starting_a_playlist_at_a_track_sends_the_context_and_an_offset() {
        let c = call(PlayerCommand::Start(Load::Context {
            uri: "spotify:playlist:p".into(),
            track: Some("spotify:track:t".into()),
            position_ms: 5_000,
        }));
        assert_eq!(c.method, "PUT");
        assert_eq!(c.url, format!("{API}/me/player/play?device_id=abc%20def"));
        assert_eq!(
            c.body,
            Some(json!({
                "context_uri": "spotify:playlist:p",
                "offset": { "uri": "spotify:track:t" },
                "position_ms": 5_000,
            }))
        );
        // An artist takes no offset: it starts at its top.
        let c = call(PlayerCommand::Start(Load::Context {
            uri: "spotify:artist:a".into(),
            track: Some("spotify:track:t".into()),
            position_ms: 0,
        }));
        assert_eq!(c.body.unwrap().get("offset"), None);
    }

    #[test]
    fn starting_tracks_sends_them_with_the_picked_one_as_the_offset() {
        let uris: Vec<String> = (0..3).map(|i| format!("spotify:track:{i}")).collect();
        let c = call(PlayerCommand::Start(Load::Tracks {
            uris,
            start: Some("spotify:track:1".into()),
            position_ms: 0,
        }));
        assert_eq!(
            c.body,
            Some(json!({
                "uris": ["spotify:track:0", "spotify:track:1", "spotify:track:2"],
                "offset": { "uri": "spotify:track:1" },
                "position_ms": 0,
            }))
        );
    }

    #[test]
    fn a_long_list_is_cut_to_a_window_that_keeps_the_picked_track() {
        let uris: Vec<String> = (0..2_000).map(|i| format!("spotify:track:{i}")).collect();
        let pick = |i: usize| format!("spotify:track:{i}");
        for at in [0, 10, 999, 1_990, 1_999] {
            let wanted = pick(at);
            let (window, start) = start_window(&uris, Some(wanted.as_str()), 500);
            assert_eq!(window.len(), 500, "at {at}");
            assert_eq!(start, Some(wanted.as_str()), "at {at}");
            assert!(window.contains(&wanted), "at {at}");
        }
        // A few before the pick, so there is a track to go back to.
        let wanted = pick(999);
        let (window, _) = start_window(&uris, Some(wanted.as_str()), 500);
        assert_eq!(window[0], pick(949));
        // No pick, or one that isn't there: the top, and no offset.
        let (window, start) = start_window(&uris, None, 500);
        assert_eq!((window[0].as_str(), start), ("spotify:track:0", None));
        let (_, start) = start_window(&uris, Some("spotify:track:x"), 500);
        assert_eq!(start, None);
        // Short lists go whole.
        let (window, start) = start_window(&uris[..3], Some("spotify:track:2"), 500);
        assert_eq!((window.len(), start), (3, Some("spotify:track:2")));
    }

    #[test]
    fn refusals_say_what_to_do_about_them() {
        let body = |reason: &str, message: &str| {
            json!({ "error": { "status": 0, "reason": reason, "message": message } }).to_string()
        };
        assert!(failure(403, &body("PREMIUM_REQUIRED", "x"), None).contains("Premium"));
        assert!(failure(404, &body("NO_ACTIVE_DEVICE", ""), None).contains("open Spotify"));
        assert!(failure(404, "", None).contains("open Spotify"));
        assert_eq!(
            failure(403, &body("UNKNOWN", "Restriction violated"), None),
            "Spotify refused: Restriction violated"
        );
        assert!(failure(429, "", Some(30)).contains("30s"));
        assert!(failure(429, "", None).contains("rate-limiting"));
        assert!(failure(502, "<html>", None).contains("502"));
        assert_eq!(
            failure(400, &body("", "Bad offset"), None),
            "HTTP 400: Bad offset"
        );
        assert_eq!(failure(418, "", None), "HTTP 418");
    }

    #[test]
    fn the_player_reply_reads_as_the_active_devices_playback() {
        let v = json!({
            "device": { "id": "p", "name": "Pixel", "type": "Smartphone",
                        "is_active": true, "volume_percent": 50, "supports_volume": true },
            "shuffle_state": true,
            "repeat_state": "context",
            "timestamp": 1,
            "context": { "uri": "spotify:album:a" },
            "progress_ms": 42_000,
            "is_playing": true,
            "item": { "uri": "spotify:track:t", "duration_ms": 180_000 },
        });
        let p = parse_player(&v).expect("a device is active");
        assert_eq!(p.device.id, "p");
        assert!(p.device.is_active);
        assert_eq!(p.track_uri, "spotify:track:t");
        assert_eq!(p.context_uri, "spotify:album:a");
        assert_eq!((p.position_ms, p.duration_ms), (42_000, 180_000));
        assert!(p.is_playing && p.shuffle);
        assert_eq!(p.repeat, Repeat::All);
        assert_eq!(parse_player(&json!({})), None);
        assert_eq!(parse_player(&serde_json::Value::Null), None);
    }

    fn command(device: &str, command: PlayerCommand) -> PlayerRequest {
        PlayerRequest::Command {
            device_id: device.to_string(),
            device_name: device.to_string(),
            command,
        }
    }

    #[test]
    fn a_burst_of_volume_or_seeks_is_sent_once_in_order() {
        use PlayerCommand::{Next, Seek, Volume};
        let batch = vec![
            command("p", Volume(50)),
            command("p", Volume(55)),
            command("p", Volume(60)),
            command("p", Next),
            command("p", Seek(1)),
            command("p", Seek(2)),
            command("q", Seek(3)),
            PlayerRequest::State,
            PlayerRequest::State,
            command("p", Volume(65)),
        ];
        assert_eq!(
            coalesce(batch),
            vec![
                command("p", Volume(60)),
                command("p", Next),
                command("p", Seek(2)),
                command("q", Seek(3)),
                PlayerRequest::State,
                command("p", Volume(65)),
            ]
        );
    }
}
