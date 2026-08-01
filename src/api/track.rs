//! One track's metadata, cover, and derived theme.

use super::*;
use crate::*;

pub(crate) fn fetch_track_meta(webapi: &Arc<Mutex<WebApi>>, track_id: &str) -> TrackMeta {
    let uri = format!("spotify:track:{track_id}");
    let empty = || TrackMeta {
        uri: uri.clone(),
        title: String::new(),
        artist: String::new(),
        album: String::new(),
        duration_ms: 0,
        image: TrackImage {
            image: None,
            url: None,
        },
        theme: None,
    };
    let Some(token) = token_of(webapi) else {
        return empty();
    };
    let client = http_client();
    let Some(v) = get_json_cached(&client, &format!("{API}/tracks/{track_id}"), &token) else {
        return empty();
    };

    let title = v["name"].as_str().unwrap_or("").to_string();
    let artist = v["artists"][0]["name"].as_str().unwrap_or("").to_string();
    let album = v["album"]["name"].as_str().unwrap_or("").to_string();
    let duration_ms = v["duration_ms"].as_u64().unwrap_or(0) as u32;
    let cover_url = v["album"]["images"][0]["url"].as_str().map(String::from);

    let image = cover_url
        .clone()
        .and_then(|u| fetch_cover(&client, &u))
        .and_then(|bytes| image::load_from_memory(&bytes).ok());
    let theme = image.as_ref().map(|img| derive_theme(img, "album ✦"));

    TrackMeta {
        uri,
        title,
        artist,
        album,
        duration_ms,
        image: TrackImage {
            image,
            url: cover_url,
        },
        theme,
    }
}
