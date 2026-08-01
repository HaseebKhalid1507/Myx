//! The live play queue.

use super::*;
use crate::*;

pub(crate) fn spawn_queue_fetch(
    webapi: Arc<Mutex<WebApi>>,
    tx: flume::Sender<Vec<(String, String)>>,
) {
    tokio::task::spawn_blocking(move || {
        let q = match token_of(&webapi) {
            Some(token) => fetch_queue_blocking(&token),
            None => Vec::new(),
        };
        let _ = tx.send(q);
    });
}

/// Returns (display, uri) for each queued item.
pub(crate) fn fetch_queue_blocking(token: &str) -> Vec<(String, String)> {
    let client = http_client();
    let Some(v) = get_json(&client, &format!("{API}/me/player/queue"), token) else {
        return Vec::new();
    };
    v["queue"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|it| {
                    let name = it["name"].as_str()?;
                    let uri = it["uri"].as_str()?.to_string();
                    let artist = it["artists"][0]["name"].as_str().unwrap_or("");
                    Some((format!("{name} — {artist}"), uri))
                })
                .collect()
        })
        .unwrap_or_default()
}
