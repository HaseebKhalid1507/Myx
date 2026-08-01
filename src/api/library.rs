//! The library sections: home, recent, playlists, albums, artists, liked.

use super::*;
use crate::*;

/// Fetch the library incrementally: fast sections first, Liked streamed in
/// chunks so the UI is usable within ~1s instead of waiting for everything.
pub(crate) fn spawn_library_fetch(
    webapi: Arc<Mutex<WebApi>>,
    tx: flume::Sender<(Section, Vec<LibItem>)>,
    done_tx: flume::Sender<bool>,
) {
    // Clone the already-refreshed access token BEFORE spawning. This removes the
    // shared WebApi mutex from the worker entirely (the stuck-library root cause).
    let token_opt = token_of(&webapi);
    liblog(format!(
        "spawn_library_fetch: token={}",
        token_opt.as_ref().map_or("missing", |_| "ok")
    ));
    std::thread::Builder::new()
        .name("myx-library".to_string())
        .spawn(move || {
            liblog("worker: entered");
            let Some(token) = token_opt else {
                liblog("worker: no token; aborting");
                let _ = done_tx.send(false);
                return;
            };

            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(12))
                .build()
                .expect("build library HTTP client");
            let mut got_any = false;
            let track_from = |t: &serde_json::Value| -> Option<LibItem> {
                Some(LibItem::track(
                    t["name"].as_str()?.to_string(),
                    t["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                    t["uri"].as_str()?.to_string(),
                ))
            };
            let artist_from = |a: &serde_json::Value| -> Option<LibItem> {
                Some(LibItem::ctx(
                    a["name"].as_str()?.to_string(),
                    String::new(),
                    a["uri"].as_str()?.to_string(),
                ))
            };
            let album_from = |a: &serde_json::Value| -> Option<LibItem> {
                Some(LibItem::ctx(
                    a["name"].as_str()?.to_string(),
                    format!("album · {}", a["artists"][0]["name"].as_str().unwrap_or("")),
                    a["uri"].as_str()?.to_string(),
                ))
            };

            // Home: a curated mix — recently played, top tracks, top artists, new releases.
            liblog("worker: fetching home/recent");
            let mut home: Vec<LibItem> = Vec::new();
            let recent5 = fetch_all_pages(
                &client,
                &format!("{API}/me/player/recently-played?limit=10"),
                &token,
                None,
                1,
                |it| track_from(&it["track"]),
            );
            if !recent5.is_empty() {
                home.push(LibItem::header("Recently Played"));
                home.extend(recent5.into_iter().take(6));
            }
            let top_tracks = fetch_all_pages(
                &client,
                &format!("{API}/me/top/tracks?limit=10"),
                &token,
                None,
                1,
                |t| track_from(t),
            );
            if !top_tracks.is_empty() {
                home.push(LibItem::header("Your Top Tracks"));
                home.extend(top_tracks.into_iter().take(8));
            }
            let top_artists = fetch_all_pages(
                &client,
                &format!("{API}/me/top/artists?limit=10"),
                &token,
                None,
                1,
                |a| artist_from(a),
            );
            if !top_artists.is_empty() {
                home.push(LibItem::header("Your Top Artists"));
                home.extend(top_artists.into_iter().take(6));
            }
            // `/browse/new-releases` was removed in February 2026.
            got_any |= !home.is_empty();
            liblog(format!("worker: home done, {} rows", home.len()));
            let _ = tx.send((Section::Home, home));

            let recent = fetch_all_pages(
                &client,
                &format!("{API}/me/player/recently-played?limit=50"),
                &token,
                None,
                1,
                |it| track_from(&it["track"]),
            );
            got_any |= !recent.is_empty();
            let _ = tx.send((Section::Recent, recent));

            let playlists = fetch_all_pages(
                &client,
                &format!("{API}/me/playlists?limit=50"),
                &token,
                None,
                10,
                |it| {
                    Some(LibItem::ctx(
                        it["name"].as_str()?.to_string(),
                        playlist_subtitle(
                            it["owner"]["display_name"].as_str().unwrap_or(""),
                            playlist_total(it),
                        ),
                        it["uri"].as_str()?.to_string(),
                    ))
                },
            );
            got_any |= !playlists.is_empty();
            let _ = tx.send((Section::Playlists, playlists));

            let albums = fetch_all_pages(
                &client,
                &format!("{API}/me/albums?limit=50"),
                &token,
                None,
                10,
                |it| album_from(&it["album"]),
            );
            got_any |= !albums.is_empty();
            let _ = tx.send((Section::Albums, albums));

            let artists = fetch_all_pages(
                &client,
                &format!("{API}/me/following?type=artist&limit=50"),
                &token,
                Some("artists"),
                5,
                |it| artist_from(it),
            );
            got_any |= !artists.is_empty();
            let _ = tx.send((Section::Artists, artists));

            // Liked can be huge — stream it in as pages arrive so the count climbs live.
            // Prepend Shuffle/Play action rows (shuffle first).
            let mut liked: Vec<LibItem> = vec![
                LibItem::play("▶︎  Play Liked Songs".into(), "myx:action:liked-play".into()),
                LibItem::header("Songs"),
            ];
            let mut url = Some(format!("{API}/me/tracks?limit=50"));
            let mut pages = 0;
            while let Some(u) = url.take() {
                if pages >= 100 {
                    break;
                }
                let Some(v) = get_json(&client, &u, &token) else {
                    break;
                };
                for it in v["items"].as_array().into_iter().flatten() {
                    if let Some(li) = track_from(&it["track"]) {
                        liked.push(li);
                    }
                }
                url = v["next"].as_str().map(String::from);
                pages += 1;
                if pages % 3 == 0 {
                    let _ = tx.send((Section::Liked, liked.clone()));
                }
            }
            got_any |= liked.len() > 2; // beyond the two action rows
            let _ = tx.send((Section::Liked, liked));

            liblog(format!("worker: all done got_any={got_any}"));
            let _ = done_tx.send(got_any);
        })
        .expect("spawn library worker");
}

/// One `/playlists/{id}/items` entry -> `LibItem`.
///
/// The payload nests the track under `item`; the older `/tracks` endpoint used
/// `track`. Both are accepted so this keeps working whichever shape is served.
///
/// `None` skips the row (`fetch_all_pages` filters rather than aborting), which
/// is what we want for entries with no playable track: `null` for items removed
/// from the catalogue, and region-locked or malformed rows.
pub(crate) fn parse_playlist_track(it: &serde_json::Value) -> Option<LibItem> {
    let t = if it["item"].is_object() {
        &it["item"]
    } else {
        &it["track"]
    };
    Some(LibItem::track(
        t["name"].as_str()?.to_string(),
        t["artists"][0]["name"].as_str().unwrap_or("").to_string(),
        t["uri"].as_str()?.to_string(),
    ))
}

/// Track count from a playlist object. Spotify renamed the field `tracks` ->
/// `items` alongside the `/tracks` -> `/items` endpoint move; read the new name
/// first and fall back so both shapes work.
pub(crate) fn playlist_total(p: &serde_json::Value) -> Option<u64> {
    p["items"]["total"]
        .as_u64()
        .or_else(|| p["tracks"]["total"].as_u64())
}

/// Playlist row subtitle: `"142 · owner"`, or just the owner when the API omits
/// the count.
///
/// Count first, deliberately: the row renderer truncates the subtitle tail-first
/// in a narrow pane, and the count is both short and the more informative half —
/// the owner is frequently the same name on every row.
pub(crate) fn playlist_subtitle(owner: &str, total: Option<u64>) -> String {
    match total {
        Some(n) if owner.is_empty() => n.to_string(),
        Some(n) => format!("{n} · {owner}"),
        None => owner.to_string(),
    }
}

pub(crate) fn fetch_all_pages(
    client: &reqwest::blocking::Client,
    first_url: &str,
    token: &str,
    nested: Option<&str>,
    max_pages: usize,
    parse: impl Fn(&serde_json::Value) -> Option<LibItem>,
) -> Vec<LibItem> {
    let mut out = Vec::new();
    let mut url = Some(first_url.to_string());
    let mut pages = 0;
    while let Some(u) = url.take() {
        if pages >= max_pages {
            break;
        }
        let Some(v) = get_json(client, &u, token) else {
            break;
        };
        let node = match nested {
            Some(k) => &v[k],
            None => &v,
        };
        for it in node["items"].as_array().into_iter().flatten() {
            if let Some(li) = parse(it) {
                out.push(li);
            }
        }
        url = node["next"].as_str().map(String::from);
        pages += 1;
    }
    out
}
