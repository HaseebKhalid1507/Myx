//! On-disk cache for catalogue reads (`~/.cache/myx/api`).
//!
//! Spotify's development-mode quota is per app, and it runs out: an artist
//! drill-in costs four requests, so a few minutes of browsing can earn a `429`
//! with a `Retry-After` measured in hours. Cached bodies keep repeat visits off
//! the network entirely, and a stale entry is served when the request fails —
//! an album list from yesterday beats an empty page.
//!
//! Only immutable-ish catalogue data belongs here. Anything the user can change
//! (playback state, saved-track flags) must go straight to the API.

use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Entries older than this are swept once per run. Album art is the bulk of it
/// — a few hundred KB a day, which would otherwise just accumulate forever.
const KEEP: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Where a URL's cached body lives. The hash keeps tokens and query strings out
/// of the filename.
fn path_in(dir: &Path, url: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    dir.join(format!("{:016x}", h.finish()))
}

fn dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = crate::home_dir()?.join(".cache/myx/api");
        fs::create_dir_all(&dir).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }
        sweep(&dir, KEEP);
        Some(dir)
    })
    .as_deref()
}

/// Drop entries nobody has asked for in a month. Runs once, on the first cache
/// hit of the session.
fn sweep(dir: &Path, keep: Duration) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > keep);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// A cached body younger than `max_age`, or any age when `max_age` is `None`.
pub fn get(url: &str, max_age: Option<Duration>) -> Option<String> {
    get_in(dir()?, url, max_age)
}

pub fn put(url: &str, body: &str) {
    if let Some(dir) = dir() {
        put_in(dir, url, body);
    }
}

fn get_in(dir: &Path, url: &str, max_age: Option<Duration>) -> Option<String> {
    let path = path_in(dir, url);
    if let Some(max_age) = max_age {
        let age = fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()?
            .elapsed()
            .ok()?;
        if age > max_age {
            return None;
        }
    }
    fs::read_to_string(&path).ok()
}

fn put_in(dir: &Path, url: &str, body: &str) {
    write_atomic(&path_in(dir, url), body.as_bytes());
}

/// Album art, keyed the same way. Spotify's image URLs embed a content hash, so
/// a cached file never goes stale — the URL changes when the picture does.
pub fn get_bytes(url: &str) -> Option<Vec<u8>> {
    fs::read(path_in(dir()?, url)).ok()
}

pub fn put_bytes(url: &str, bytes: &[u8]) {
    if let Some(dir) = dir() {
        write_atomic(&path_in(dir, url), bytes);
    }
}

/// Write via a temporary file and rename, so a kill mid-write can't leave a
/// truncated entry behind — image bytes never expire, and a corrupt one would
/// mean a cover that stays broken forever.
///
/// The temp name carries a counter because two library threads can fetch the
/// same URL at once; sharing one scratch file would interleave their writes into
/// a corrupt entry that then never expires.
fn write_atomic(path: &Path, bytes: &[u8]) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!("{}.tmp", SEQ.fetch_add(1, Ordering::Relaxed)));
    if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("myx-httpcache-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_miss_reads_nothing() {
        let dir = scratch("miss");
        assert_eq!(get_in(&dir, "https://x/albums", Some(Duration::MAX)), None);
    }

    #[test]
    fn a_fresh_entry_comes_back() {
        let dir = scratch("fresh");
        put_in(&dir, "https://x/albums", "{\"items\":[]}");
        assert_eq!(
            get_in(&dir, "https://x/albums", Some(Duration::from_secs(60))),
            Some("{\"items\":[]}".to_string())
        );
    }

    #[test]
    fn an_expired_entry_is_a_miss_but_still_readable() {
        let dir = scratch("expired");
        put_in(&dir, "https://x/albums", "old");
        // Zero TTL expires it immediately; the stale read (no max_age) is what
        // rescues a drill-in when the quota is spent.
        assert_eq!(get_in(&dir, "https://x/albums", Some(Duration::ZERO)), None);
        assert_eq!(
            get_in(&dir, "https://x/albums", None),
            Some("old".to_string())
        );
    }

    #[test]
    fn a_sweep_keeps_fresh_entries_and_survives_an_empty_dir() {
        let dir = scratch("sweep");
        sweep(&dir, Duration::ZERO); // empty dir, nothing to do
        put_in(&dir, "https://x/albums", "fresh");
        sweep(&dir, KEEP);
        assert_eq!(
            get_in(&dir, "https://x/albums", None),
            Some("fresh".to_string())
        );
        // Everything is stale at zero age, so this clears the entry.
        sweep(&dir, Duration::ZERO);
        assert_eq!(get_in(&dir, "https://x/albums", None), None);
    }

    #[test]
    fn different_urls_do_not_share_an_entry() {
        let dir = scratch("keys");
        put_in(&dir, "https://x/artists/1/albums", "one");
        put_in(&dir, "https://x/artists/2/albums", "two");
        assert_eq!(
            get_in(&dir, "https://x/artists/1/albums", None),
            Some("one".to_string())
        );
    }
}
