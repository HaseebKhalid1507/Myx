//! The session snapshot on disk (~/.cache/myx/state.json).

use crate::*;

/// Persisted across sessions (~/.cache/myx/state.json).
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedState {
    pub(crate) volume: u8,
    #[serde(default)]
    pub(crate) shuffle: bool,
    #[serde(default)]
    pub(crate) repeat: bool,
    #[serde(default)]
    pub(crate) last_played: Option<LastPlayed>,
    pub(crate) queue: Vec<String>,
    #[serde(default)]
    pub(crate) queue_uris: Vec<String>,
    #[serde(default)]
    pub(crate) source: PlaySource,
    #[serde(default)]
    pub(crate) source_name: String,
    #[serde(default)]
    pub(crate) equalizer: EqualizerSettings,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct LastPlayed {
    pub(crate) uri: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) duration_ms: u32,
    pub(crate) position_ms: u32,
}

impl SavedState {
    pub(crate) fn path() -> Option<std::path::PathBuf> {
        Some(myx::home_dir()?.join(".cache/myx/state.json"))
    }
    pub(crate) fn load() -> SavedState {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .map(|mut state: Self| {
                state.equalizer = state.equalizer.normalized();
                state
            })
            .unwrap_or_default()
    }
    pub(crate) fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string(self) {
            // Write via a temporary file and rename, so a crash or kill mid-write
            // can't leave a truncated state.json behind — that would lose the
            // entire saved queue, position and equalizer curve.
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
}

/// Snapshot the current session to disk (volume, last track, position, queue).
pub(crate) fn save_state(app: &App) {
    let last_played = app.playback.now.as_ref().map(|now| LastPlayed {
        uri: now.uri.clone(),
        title: now.title.clone(),
        artist: now.artist.clone(),
        album: now.album.clone(),
        duration_ms: now.duration_ms,
        position_ms: app.playback.position_ms(),
    });

    let s = SavedState {
        volume: app.transport.volume,
        shuffle: app.transport.shuffle,
        repeat: app.transport.repeat,
        last_played,
        queue: app.transport.queue.clone(),
        queue_uris: app.transport.queue_uris.clone(),
        source: app.transport.source.clone(),
        source_name: app.transport.source_name.clone(),
        equalizer: app.transport.equalizer,
    };
    s.save();
}

#[cfg(test)]
mod equalizer_persistence_tests {
    use super::*;

    #[test]
    fn old_state_without_equalizer_defaults_to_active_flat() {
        let state: SavedState =
            serde_json::from_str(r#"{"volume":80,"queue":[],"source_name":""}"#)
                .expect("old state remains readable");
        assert_eq!(state.equalizer, EqualizerSettings::default());
    }

    #[test]
    fn custom_equalizer_round_trips_with_bypass_state() {
        let state = SavedState {
            equalizer: EqualizerSettings {
                enabled: false,
                gains_db: [6, 5, 4, 3, 2, 1, 0, -1, -2, -3],
            },
            ..Default::default()
        };
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: SavedState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.equalizer, state.equalizer);
    }
}
