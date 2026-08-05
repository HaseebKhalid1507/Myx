//! Live playback state (server-side).

use super::*;
use crate::*;

/// The current playback as Spotify remembers it (across devices).
pub(crate) struct RemotePlaybackState {
    pub(crate) track_id: String,
    pub(crate) progress_ms: u32,
    pub(crate) shuffle: bool,
    pub(crate) repeat: bool,
    pub(crate) volume: u8,
}

pub(crate) enum RestoreOutcome {
    Reclaimed(RemotePlaybackState),
    Unavailable,
}

pub(crate) fn fetch_playback_state(token: &str) -> Option<RemotePlaybackState> {
    let client = http_client();
    let resp = client
        .get(format!("{API}/me/player"))
        .bearer_auth(token)
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None; // 204 = nothing playing recently
    }
    let v: serde_json::Value = resp.json().ok()?;
    let track_id = v["item"]["id"].as_str()?.to_string();
    Some(RemotePlaybackState {
        track_id,
        progress_ms: v["progress_ms"].as_u64().unwrap_or(0) as u32,
        shuffle: v["shuffle_state"].as_bool().unwrap_or(false),
        repeat: v["repeat_state"]
            .as_str()
            .map(|r| r != "off")
            .unwrap_or(false),
        volume: v["device"]["volume_percent"].as_u64().unwrap_or(50) as u8,
    })
}

/// Transfer the current server-side playback onto the myx device (with its full
/// context + queue + position). `play=false` transfers paused.
/// `Ok(())` on success, `Err(reason)` with something worth showing otherwise.
pub(crate) fn transfer_playback(token: &str, device_id: &str, play: bool) -> Result<(), String> {
    let client = http_client();
    match client
        .put(format!("{API}/me/player"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "device_ids": [device_id], "play": play }))
        .send()
    {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => {
            let code = r.status().as_u16();
            let body = r.text().unwrap_or_default();
            liblog(format!("transfer -> HTTP {code}: {body}"));
            Err(format!("HTTP {code}"))
        }
        Err(e) => {
            liblog(format!("transfer failed: {e}"));
            Err("network error".to_string())
        }
    }
}

/// Boot restore: read the live playback state, transfer it onto myx (retrying
/// while the device registers), and hand the state back to the UI.
pub(crate) fn spawn_restore(
    webapi: Arc<Mutex<WebApi>>,
    device_id: String,
    tx: flume::Sender<RestoreOutcome>,
) {
    tokio::task::spawn_blocking(move || {
        let Some(token) = token_of(&webapi) else {
            let _ = tx.send(RestoreOutcome::Unavailable);
            return;
        };
        let Some(state) = fetch_playback_state(&token) else {
            let _ = tx.send(RestoreOutcome::Unavailable);
            return;
        };
        // Retry the transfer — the Connect device can take a moment to appear.
        let mut transferred = false;
        for _ in 0..6 {
            if transfer_playback(&token, &device_id, true).is_ok() {
                transferred = true;
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        let outcome = if transferred {
            RestoreOutcome::Reclaimed(state)
        } else {
            RestoreOutcome::Unavailable
        };
        let _ = tx.send(outcome);
    });
}
