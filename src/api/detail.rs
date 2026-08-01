//! Drill-in detail (artist / album / playlist).

use super::*;
use crate::*;

pub(crate) fn spawn_detail_fetch(
    webapi: Arc<Mutex<WebApi>>,
    uri: String,
    name: String,
    tx: flume::Sender<(String, String, Vec<LibItem>)>,
) {
    tokio::task::spawn_blocking(move || {
        if let Some(token) = token_of(&webapi) {
            let (title, items) = fetch_detail_blocking(&token, &uri, &name);
            let _ = tx.send((uri, title, items));
        }
    });
}

/// An artist's most popular tracks. `/artists/{id}/top-tracks` was removed in
/// February 2026, so this searches instead — also popularity-ranked, ten max.
pub(crate) fn artist_top_tracks(
    client: &reqwest::blocking::Client,
    token: &str,
    id: &str,
    name: &str,
) -> Vec<LibItem> {
    let url = format!(
        "{API}/search?q={}&type=track&limit=10",
        urlencode(&format!("artist:{name}"))
    );
    let Some(v) = get_json_cached(client, &url, token) else {
        return Vec::new();
    };
    v["tracks"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        // Search matches loosely — keep only tracks this artist is credited on.
        .filter(|t| {
            t["artists"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x["id"].as_str() == Some(id)))
        })
        .filter_map(|t| {
            Some(LibItem::track(
                t["name"].as_str()?.to_string(),
                t["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                t["uri"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// An artist's albums and singles, deduped by name, newest first. Paged ten at
/// a time — since February 2026 a larger page is a 400 "Invalid limit".
pub(crate) fn artist_albums(
    client: &reqwest::blocking::Client,
    token: &str,
    id: &str,
) -> Vec<LibItem> {
    let mut seen = std::collections::HashSet::new();
    let mut albums: Vec<(String, LibItem)> = Vec::new();
    let mut url = Some(format!(
        "{API}/artists/{id}/albums?include_groups=album,single&limit=10"
    ));
    let mut pages = 0;
    while let Some(u) = url.take() {
        if pages >= 3 {
            break; // 30 releases; more pages burn the API quota per drill-in
        }
        let Some(v) = get_json_cached(client, &u, token) else {
            break;
        };
        for a in v["items"].as_array().into_iter().flatten() {
            let (Some(aname), Some(auri)) = (a["name"].as_str(), a["uri"].as_str()) else {
                continue;
            };
            if !seen.insert(aname.to_lowercase()) {
                continue;
            }
            let date = a["release_date"].as_str().unwrap_or("").to_string();
            let year = date.split('-').next().unwrap_or("").to_string();
            albums.push((
                date,
                LibItem::ctx(aname.to_string(), year, auri.to_string()),
            ));
        }
        url = v["next"].as_str().map(String::from);
        pages += 1;
    }
    albums.sort_by(|x, y| y.0.cmp(&x.0)); // newest first
    albums.into_iter().map(|(_, it)| it).collect()
}

pub(crate) fn fetch_detail_blocking(token: &str, uri: &str, name: &str) -> (String, Vec<LibItem>) {
    let client = http_client();
    let mut parts = uri.split(':');
    parts.next(); // "spotify"
    let kind = parts.next().unwrap_or("");
    let id = parts.next().unwrap_or("");

    // "Play all" row first.
    let mut items = vec![LibItem::play(format!("▶︎ Play {name}"), uri.to_string())];

    liblog(format!("detail: kind={kind} id={id} uri={uri}"));
    match kind {
        "artist" => {
            let tracks = artist_top_tracks(&client, token, id, name);
            if !tracks.is_empty() {
                items.push(LibItem::header("Popular"));
                items.extend(tracks);
            }
            let albums = artist_albums(&client, token, id);
            if !albums.is_empty() {
                items.push(LibItem::header("Albums"));
                items.extend(albums);
            }
            if items.len() == 1 {
                // Only the play row: every request failed, most often a spent
                // API quota. Say so instead of showing a bare button.
                items.push(LibItem::header("nothing loaded — API error or quota"));
            }
        }
        "album" => {
            if let Some(v) = get_json_cached(
                &client,
                &format!("{API}/albums/{id}/tracks?limit=50"),
                token,
            ) {
                for t in v["items"].as_array().into_iter().flatten() {
                    if let (Some(n), Some(u)) = (t["name"].as_str(), t["uri"].as_str()) {
                        items.push(LibItem::track(
                            n.to_string(),
                            t["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                            u.to_string(),
                        ));
                    }
                }
            }
        }
        "playlist" => {
            // Follow `next` instead of taking only the first page: playlists
            // routinely exceed the 100-item page size, and the drill-in list
            // (plus "play from this track") was silently truncated.
            let before = items.len();
            items.extend(fetch_all_pages(
                &client,
                &format!("{API}/playlists/{id}/items?limit=100"),
                token,
                None, // items[] is top-level on this endpoint
                10,   // 1,000 tracks, matching the other sections' ceiling
                parse_playlist_track,
            ));
            if items.len() == before {
                // Some third-party playlists 403 even on /items. `fetch_all_pages`
                // has no error channel, so an empty result is indistinguishable
                // from an empty playlist — say both rather than showing a blank
                // pane with no explanation.
                items.push(LibItem::header("no tracks — empty or restricted"));
            }
        }
        _ => {}
    }

    liblog(format!("detail: {} rows for {uri}", items.len()));
    (name.to_string(), items)
}
