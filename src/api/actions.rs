//! Library writes and the action menu that drives them.
//!
//! The rest of `api/` reads; this file is the half that writes — saving and
//! unsaving tracks, albums and artists, following playlists, and queue and
//! playlist adds. `build_action_menu` is the exception: it builds the menu
//! model describing those writes rather than performing one.

use super::*;
use crate::*;

pub(crate) fn spawn_action_menu(
    webapi: Arc<Mutex<WebApi>>,
    item: LibItem,
    tx: flume::Sender<ActionMenu>,
) {
    tokio::task::spawn_blocking(move || {
        if let Some(token) = token_of(&webapi) {
            let _ = tx.send(build_action_menu(Some(&token), &item));
        }
    });
}

/// Build the context menu for `item`, checking saved/following state and
/// resolving related artist/album links up front.
pub(crate) fn build_action_menu(token: Option<&str>, item: &LibItem) -> ActionMenu {
    let mut parts = item.uri.split(':');
    parts.next();
    let kind = parts.next().unwrap_or("");
    let id = parts.next().unwrap_or("").to_string();
    let uri = item.uri.clone();
    // Only build the blocking client for the enriched (Some token) path; the
    // instant (None) path runs on the async loop where dropping reqwest's inner
    // runtime would panic.
    let client = token.map(|_| http_client());
    let mut items = Vec::new();

    match kind {
        "track" => {
            let saved = token
                .map(|t| api_contains(t, &format!("spotify:track:{id}")))
                .unwrap_or(false);
            items.push(ActionItem {
                label: if saved {
                    "♥  Remove from Liked".into()
                } else {
                    "♡  Add to Liked".into()
                },
                kind: ActionKind::ToggleLike {
                    id: id.clone(),
                    saved,
                },
            });
            items.push(ActionItem {
                label: "＋  Add to Queue".into(),
                kind: ActionKind::Queue {
                    uri: uri.clone(),
                    display: if item.subtitle.is_empty() {
                        item.name.clone()
                    } else {
                        format!("{} — {}", item.name, item.subtitle)
                    },
                },
            });
            items.push(ActionItem {
                label: "≡  Add to Playlist…".into(),
                kind: ActionKind::AddToPlaylistMenu {
                    track_uri: uri.clone(),
                },
            });
            // Resolve the track's artist + album for "Go to" navigation.
            if let Some(v) = client
                .as_ref()
                .zip(token)
                .and_then(|(c, t)| get_json_cached(c, &format!("{API}/tracks/{id}"), t))
            {
                if let (Some(au), Some(an)) = (
                    v["artists"][0]["uri"].as_str(),
                    v["artists"][0]["name"].as_str(),
                ) {
                    items.push(ActionItem {
                        label: format!("→  Go to Artist ({an})"),
                        kind: ActionKind::Open {
                            uri: au.to_string(),
                            name: an.to_string(),
                        },
                    });
                }
                if let (Some(lu), Some(ln)) =
                    (v["album"]["uri"].as_str(), v["album"]["name"].as_str())
                {
                    items.push(ActionItem {
                        label: "→  Go to Album".into(),
                        kind: ActionKind::Open {
                            uri: lu.to_string(),
                            name: ln.to_string(),
                        },
                    });
                }
            }
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        "artist" => {
            let following = token
                .map(|t| api_contains(t, &format!("spotify:artist:{id}")))
                .unwrap_or(false);
            items.push(ActionItem {
                label: if following {
                    "Unfollow".into()
                } else {
                    "Follow".into()
                },
                kind: ActionKind::ToggleFollowArtist { id, following },
            });
            items.push(ActionItem {
                label: "▶︎  Play".into(),
                kind: ActionKind::Play {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "→  Open".into(),
                kind: ActionKind::Open {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        "album" => {
            let saved = token
                .map(|t| api_contains(t, &format!("spotify:album:{id}")))
                .unwrap_or(false);
            items.push(ActionItem {
                label: if saved {
                    "Remove from Library".into()
                } else {
                    "Save Album".into()
                },
                kind: ActionKind::ToggleSaveAlbum {
                    id: id.clone(),
                    saved,
                },
            });
            items.push(ActionItem {
                label: "▶︎  Play".into(),
                kind: ActionKind::Play {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "→  Open Album".into(),
                kind: ActionKind::Open {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            if let Some(v) = client
                .as_ref()
                .zip(token)
                .and_then(|(c, t)| get_json_cached(c, &format!("{API}/albums/{id}"), t))
            {
                if let (Some(au), Some(an)) = (
                    v["artists"][0]["uri"].as_str(),
                    v["artists"][0]["name"].as_str(),
                ) {
                    items.push(ActionItem {
                        label: format!("→  Go to Artist ({an})"),
                        kind: ActionKind::Open {
                            uri: au.to_string(),
                            name: an.to_string(),
                        },
                    });
                }
            }
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        "playlist" => {
            items.push(ActionItem {
                label: "＋  Add to Your Library".into(),
                kind: ActionKind::FollowPlaylist { id },
            });
            items.push(ActionItem {
                label: "▶︎  Play".into(),
                kind: ActionKind::Play {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "→  Open".into(),
                kind: ActionKind::Open {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        _ => {}
    }
    ActionMenu {
        title: item.name.clone(),
        items,
        selected: 0,
    }
}

pub(crate) fn spawn_action(
    webapi: Arc<Mutex<WebApi>>,
    kind: ActionKind,
    tx: flume::Sender<String>,
) {
    tokio::task::spawn_blocking(move || {
        let msg = match token_of(&webapi) {
            Some(t) => run_action(&t, kind),
            None => "not authorized".to_string(),
        };
        let _ = tx.send(msg);
    });
}

pub(crate) fn run_action(token: &str, kind: ActionKind) -> String {
    let client = http_client();
    match kind {
        ActionKind::ToggleLike { id, saved } => {
            match library_write(&client, token, saved, &format!("spotify:track:{id}")) {
                Ok(()) => {
                    if saved {
                        "removed from Liked".into()
                    } else {
                        "added to Liked \u{2665} (press r to refresh)".into()
                    }
                }
                Err(e) => format!("like failed: {e}"),
            }
        }
        ActionKind::Queue { uri, .. } => {
            match api_modify(
                &client,
                token,
                "POST",
                &format!("{API}/me/player/queue?uri={}", urlencode(&uri)),
            ) {
                Ok(()) => "added to queue".into(),
                Err(e) => format!("queue failed: {e} (start playback first)"),
            }
        }
        ActionKind::AddToPlaylist {
            playlist_id,
            track_uri,
        } => {
            match api_modify(
                &client,
                token,
                "POST",
                &format!(
                    "{API}/playlists/{playlist_id}/items?uris={}",
                    urlencode(&track_uri)
                ),
            ) {
                Ok(()) => "added to playlist".into(),
                Err(e) => format!("add failed: {e}"),
            }
        }
        ActionKind::ToggleFollowArtist { id, following } => {
            match library_write(&client, token, following, &format!("spotify:artist:{id}")) {
                Ok(()) => {
                    if following {
                        "unfollowed".into()
                    } else {
                        "following".into()
                    }
                }
                Err(e) => format!("follow failed: {e}"),
            }
        }
        ActionKind::ToggleSaveAlbum { id, saved } => {
            match library_write(&client, token, saved, &format!("spotify:album:{id}")) {
                Ok(()) => {
                    if saved {
                        "removed album".into()
                    } else {
                        "saved album".into()
                    }
                }
                Err(e) => format!("album action failed: {e}"),
            }
        }
        ActionKind::FollowPlaylist { id } => {
            match library_write(&client, token, false, &format!("spotify:playlist:{id}")) {
                Ok(()) => "added to library".into(),
                Err(e) => format!("add failed: {e}"),
            }
        }
        _ => String::new(),
    }
}

/// Add or remove one library item — track, album, artist or playlist.
///
/// One endpoint for all four since February 2026; the per-type ones it replaced
/// are gone and answer 403. `uris` must go in the query string: a JSON body is
/// rejected as missing it.
pub(crate) fn library_write(
    client: &reqwest::blocking::Client,
    token: &str,
    remove: bool,
    uri: &str,
) -> Result<(), String> {
    api_modify(
        client,
        token,
        if remove { "DELETE" } else { "PUT" },
        &format!("{API}/me/library?uris={}", urlencode(uri)),
    )
}

/// Returns Ok on 2xx, else a short reason (HTTP status / network) so the UI can
/// say WHY instead of a generic "action failed". Retries once on 429.
pub(crate) fn api_modify(
    client: &reqwest::blocking::Client,
    token: &str,
    method: &str,
    url: &str,
) -> Result<(), String> {
    for attempt in 0..2 {
        let req = match method {
            "PUT" => client.put(url),
            "DELETE" => client.delete(url),
            _ => client.post(url),
        };
        match req.bearer_auth(token).header("Content-Length", "0").send() {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) if r.status().as_u16() == 429 && attempt == 0 => {
                let wait = r
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1)
                    .min(5);
                std::thread::sleep(Duration::from_secs(wait));
            }
            Ok(r) => return Err(format!("HTTP {}", r.status().as_u16())),
            Err(e) => {
                return Err(if e.is_timeout() {
                    "timeout".into()
                } else {
                    "network error".into()
                })
            }
        }
    }
    Err("rate limited".into())
}

/// Is `uri` in the user's library? Replaced the `/me/*/contains` family in
/// February 2026; those are gone and answer 403.
pub(crate) fn api_contains(token: &str, uri: &str) -> bool {
    let client = http_client();
    let url = format!("{API}/me/library/contains?uris={}", urlencode(uri));
    get_json(&client, &url, token)
        .and_then(|v| v.get(0).and_then(|b| b.as_bool()))
        .unwrap_or(false)
}
