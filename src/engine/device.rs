//! Who this myx is to Spotify Connect: the name other devices list it under,
//! and an id that stays the same from one launch to the next.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::liblog::liblog;

/// The name Spotify's device lists show for this myx: `device_name` from
/// `config.toml`, or `myx · <hostname>` so two machines can be told apart.
pub(crate) fn device_name() -> String {
    name_from(
        crate::config::get().device_name.as_deref(),
        &gethostname::gethostname().to_string_lossy(),
    )
}

fn name_from(configured: Option<&str>, hostname: &str) -> String {
    if let Some(name) = configured.map(str::trim).filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    // `bella.local` → `bella`: the domain adds nothing in a device list, and
    // `localhost` says nothing at all.
    match hostname.trim().split('.').next() {
        Some(host) if !host.is_empty() && host != "localhost" => format!("myx · {host}"),
        _ => "myx".to_string(),
    }
}

/// This install's Spotify Connect device id, kept in `~/.cache/myx/device_id`.
///
/// librespot makes up a new id for every session unless it is given one, so
/// each launch and each reconnect looked to Spotify like a different device
/// that happened to share the name. Made once per install (random, not derived
/// from the name, so two machines with the same `device_name` stay apart) and
/// kept when the device is renamed.
pub(crate) fn device_id() -> String {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| match path() {
        Some(path) => load_or_create(&path),
        None => fresh(),
    })
    .clone()
}

fn path() -> Option<PathBuf> {
    Some(crate::home_dir()?.join(".cache/myx/device_id"))
}

fn load_or_create(path: &Path) -> String {
    if let Some(id) = std::fs::read_to_string(path).ok().and_then(|s| valid(&s)) {
        return id;
    }
    let id = fresh();
    if let Err(e) = save(path, &id) {
        // Not a reason to stop: this session still gets an id, as before.
        liblog(format!(
            "engine: couldn't save the device id to {} ({e}); it will change next launch",
            path.display()
        ));
    }
    id
}

/// The 40 hex digits Spotify's own clients use, or `None` for anything else
/// (an empty or hand-mangled file is replaced, not sent to Spotify).
fn valid(s: &str) -> Option<String> {
    let s = s.trim();
    (s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

fn fresh() -> String {
    use std::fmt::Write;
    let mut bytes = [0u8; 20];
    rand::fill(&mut bytes[..]);
    bytes.iter().fold(String::with_capacity(40), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

/// Via a temporary file and a rename, so a kill mid-write can't leave half an
/// id that the next launch would throw away.
fn save(path: &Path, id: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, id)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configured_name_wins_and_is_trimmed() {
        assert_eq!(name_from(Some("  desk "), "bella"), "desk");
    }

    #[test]
    fn without_one_the_hostname_tells_machines_apart() {
        assert_eq!(name_from(None, "bella"), "myx · bella");
        assert_eq!(name_from(Some("   "), "bella"), "myx · bella");
        assert_eq!(
            name_from(None, "Haseebs-MacBook-Pro.local"),
            "myx · Haseebs-MacBook-Pro"
        );
        assert_eq!(name_from(None, ""), "myx");
        assert_eq!(name_from(None, "localhost"), "myx");
    }

    #[test]
    fn a_fresh_id_looks_like_spotifys_and_differs_each_time() {
        let (a, b) = (fresh(), fresh());
        assert_eq!(valid(&a).as_deref(), Some(a.as_str()));
        assert_ne!(a, b);
    }

    #[test]
    fn only_forty_hex_digits_count_as_an_id() {
        let id = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(valid(&format!("{id}\n")).as_deref(), Some(id));
        assert_eq!(valid(&id.to_uppercase()).as_deref(), Some(id));
        assert_eq!(valid(""), None);
        assert_eq!(valid(&id[1..]), None);
        assert_eq!(valid(&id.replace('a', "z")), None);
    }

    #[test]
    fn the_id_is_made_once_and_then_kept() {
        let dir = std::env::temp_dir().join(format!("myx-device-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("device_id");

        let first = load_or_create(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        assert_eq!(load_or_create(&path), first);

        // A mangled file is replaced, and the replacement is what's kept.
        std::fs::write(&path, "not an id").unwrap();
        let second = load_or_create(&path);
        assert_ne!(second, first);
        assert_eq!(valid(&second).as_deref(), Some(second.as_str()));
        assert_eq!(load_or_create(&path), second);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
