//! Search.

use super::*;
use crate::*;

pub(crate) fn spawn_search(
    webapi: Arc<Mutex<WebApi>>,
    query: String,
    tx: flume::Sender<Vec<LibItem>>,
) {
    tokio::task::spawn_blocking(move || {
        let results = match token_of(&webapi) {
            Some(token) => search_blocking(&token, &query),
            None => Vec::new(),
        };
        let _ = tx.send(results);
    });
}

pub(crate) fn search_blocking(token: &str, query: &str) -> Vec<LibItem> {
    let client = http_client();
    let url = format!(
        "{API}/search?q={}&type=track,artist,album,playlist&limit=6",
        urlencode(query)
    );
    let Some(v) = get_json(&client, &url, token) else {
        return Vec::new();
    };

    let mut out = Vec::new();

    let songs: Vec<LibItem> = v["tracks"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some(LibItem::track(
                t["name"].as_str()?.to_string(),
                t["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                t["uri"].as_str()?.to_string(),
            ))
        })
        .collect();
    let artists: Vec<LibItem> = v["artists"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            Some(LibItem::ctx(
                a["name"].as_str()?.to_string(),
                String::new(),
                a["uri"].as_str()?.to_string(),
            ))
        })
        .collect();
    let albums: Vec<LibItem> = v["albums"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|al| {
            Some(LibItem::ctx(
                al["name"].as_str()?.to_string(),
                al["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                al["uri"].as_str()?.to_string(),
            ))
        })
        .collect();
    let playlists: Vec<LibItem> = v["playlists"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(LibItem::ctx(
                p["name"].as_str()?.to_string(),
                playlist_subtitle(
                    p["owner"]["display_name"].as_str().unwrap_or(""),
                    playlist_total(p),
                ),
                p["uri"].as_str()?.to_string(),
            ))
        })
        .collect();

    for (title, group) in [
        ("Songs", songs),
        ("Artists", artists),
        ("Albums", albums),
        ("Playlists", playlists),
    ] {
        if !group.is_empty() {
            out.push(LibItem::header(title));
            out.extend(group);
        }
    }
    out
}
