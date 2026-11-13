//! Playlist catalogue fallback through Spotify's internal context resolver.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Metadata, Track};
use tokio::task::JoinSet;

const MAX_TRACKS: usize = 1_000;
const METADATA_WORKERS: usize = 2;
const METADATA_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistTrack {
    pub uri: String,
    pub name: String,
    pub artist: String,
}

/// Resolve a playlist and its track labels without the restricted Web API.
///
/// The context endpoint gives us the complete, ordered URI list. Track labels
/// live in a separate metadata endpoint, so those reads run concurrently and
/// are cached by URI. The small worker count avoids throttling on large lists.
pub async fn playlist_tracks(session: &Session, context_uri: &str) -> Result<Vec<PlaylistTrack>> {
    let context = session
        .spclient()
        .get_context(context_uri)
        .await
        .context("resolve playlist context")?;
    let uris: Vec<String> = context
        .pages
        .iter()
        .flat_map(|page| page.tracks.iter())
        .filter_map(|track| track.uri.clone())
        .filter(|uri| uri.starts_with("spotify:track:"))
        .take(MAX_TRACKS)
        .collect();
    if uris.is_empty() {
        bail!("playlist context has no tracks");
    }

    let mut resolved = vec![None; uris.len()];
    let mut jobs = JoinSet::new();
    let mut next = 0;
    while next < uris.len() || !jobs.is_empty() {
        while next < uris.len() && jobs.len() < METADATA_WORKERS {
            let index = next;
            let uri = uris[index].clone();
            let session = session.clone();
            jobs.spawn(async move { (index, resolve_track(&session, uri).await) });
            next += 1;
        }
        if let Some(Ok((index, track))) = jobs.join_next().await {
            resolved[index] = track;
        }
    }

    let tracks: Vec<_> = resolved.into_iter().flatten().collect();
    if tracks.is_empty() {
        bail!("playlist track metadata unavailable");
    }
    Ok(tracks)
}

async fn resolve_track(session: &Session, uri: String) -> Option<PlaylistTrack> {
    let cache_key = format!("librespot:track-label:{uri}");
    if let Some(body) = crate::httpcache::get(&cache_key, None) {
        if let Ok((name, artist)) = serde_json::from_str::<(String, String)>(&body) {
            return Some(PlaylistTrack { uri, name, artist });
        }
    }

    let spotify_uri = SpotifyUri::from_uri(&uri).ok()?;
    for attempt in 0..2 {
        if let Ok(Ok(metadata)) =
            tokio::time::timeout(METADATA_TIMEOUT, Track::get(session, &spotify_uri)).await
        {
            let mut artists: Vec<_> = metadata
                .artists
                .iter()
                .map(|artist| artist.name.as_str())
                .collect();
            if artists.is_empty() {
                artists = metadata
                    .artists_with_role
                    .iter()
                    .map(|artist| artist.name.as_str())
                    .collect();
            }
            let artist = artists.join(", ");
            let name = metadata.name;
            if let Ok(body) = serde_json::to_string(&(name.as_str(), artist.as_str())) {
                crate::httpcache::put(&cache_key, &body);
            }
            return Some(PlaylistTrack { uri, name, artist });
        }
        if attempt == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    None
}
