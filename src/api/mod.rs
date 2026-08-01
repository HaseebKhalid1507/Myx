//! The Spotify Web API layer.
//!
//! One-way dependency: everything here talks HTTP and hands plain data back to
//! the app over channels; nothing here touches `App` or the render tree. One
//! module per thing fetched, so the file to open is the one named after it.
//!
//! This module holds the transport core — the client, the token, and the two
//! `GET` helpers every other file in here is built on.

mod actions;
mod detail;
mod library;
mod lyrics;
mod playback;
mod queue;
mod search;
mod track;

pub(crate) use actions::*;
pub(crate) use detail::*;
pub(crate) use library::*;
pub(crate) use lyrics::*;
pub(crate) use playback::*;
pub(crate) use queue::*;
pub(crate) use search::*;
pub(crate) use track::*;

use crate::*;

pub(crate) fn token_of(webapi: &Arc<Mutex<WebApi>>) -> Option<String> {
    // Refresh when the token is expiring so long sessions don't silently go
    // read-only (audit H1). Only holds the lock across the network call in the
    // rare refresh window; otherwise this is just a cheap clone.
    let token = {
        let mut w = webapi.lock().ok()?;
        match w.valid_token() {
            Ok(t) => t,
            Err(_) => w.cached_token(),
        }
    };
    (!token.is_empty()).then_some(token)
}

/// Base URL every Spotify Web API call is built from.
pub(crate) const API: &str = "https://api.spotify.com/v1";

/// How long to wait before retrying a 429, or `None` to give up now.
///
/// Spotify hands development-mode apps hour-long `Retry-After` values once a
/// quota is spent, and sleeping on those froze a drill-in for minutes before
/// failing anyway. Only brief backoffs are worth waiting out.
pub(crate) fn retry_delay(retry_after: Option<u64>) -> Option<Duration> {
    match retry_after.unwrap_or(3) {
        secs if secs <= 5 => Some(Duration::from_secs(secs + 1)),
        _ => None,
    }
}

/// GET a JSON endpoint, retrying on 429 (respecting Retry-After).
pub(crate) fn get_json(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
) -> Option<serde_json::Value> {
    for _ in 0..5 {
        let resp = match client.get(url).bearer_auth(token).send() {
            Ok(r) => r,
            Err(e) => {
                liblog(format!("api: {url} transport error: {e}"));
                return None;
            }
        };
        if resp.status().as_u16() == 429 {
            let after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            let Some(wait) = retry_delay(after) else {
                liblog(format!(
                    "api: {url} -> 429, retry-after {after:?}s, giving up"
                ));
                return None;
            };
            std::thread::sleep(wait);
            continue;
        }
        if !resp.status().is_success() {
            // Swallowing the status made a dead endpoint look like an empty one.
            liblog(format!("api: {url} -> HTTP {}", resp.status().as_u16()));
            return None;
        }
        return resp.json::<serde_json::Value>().ok();
    }
    None
}

/// How long a cached catalogue response counts as fresh. Discographies and
/// track lists change on the order of weeks; a day keeps browsing off the
/// network without anyone noticing the lag.
pub(crate) const CATALOGUE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// `get_json` for catalogue reads, served from disk when possible.
///
/// On a failed request an expired entry is used rather than nothing — that is
/// the case that matters, since a spent quota is exactly when the network stops
/// answering and the artist page would otherwise come up empty.
pub(crate) fn get_json_cached(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
) -> Option<serde_json::Value> {
    if let Some(body) = myx::httpcache::get(url, Some(CATALOGUE_TTL)) {
        return serde_json::from_str(&body).ok();
    }
    match get_json(client, url, token) {
        Some(v) => {
            myx::httpcache::put(url, &v.to_string());
            Some(v)
        }
        None => {
            let stale = myx::httpcache::get(url, None)?;
            liblog(format!("api: {url} failed; serving cached copy"));
            serde_json::from_str(&stale).ok()
        }
    }
}

/// Album art bytes, from disk when they've been seen before.
pub(crate) fn fetch_cover(client: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = myx::httpcache::get_bytes(url) {
        return Some(bytes);
    }
    let resp = client.get(url).send().ok()?;
    // An error page cached as image bytes would be a permanently broken cover:
    // the entry never expires, because the URL only changes with the picture.
    if !resp.status().is_success() {
        liblog(format!("cover: {url} -> HTTP {}", resp.status().as_u16()));
        return None;
    }
    let bytes = resp.bytes().ok()?.to_vec();
    myx::httpcache::put_bytes(url, &bytes);
    Some(bytes)
}

/// A blocking HTTP client with a timeout so a stalled network can't wedge a
/// worker thread forever (audit H2).
pub(crate) fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}
