//! User settings from `~/.config/myx/config.toml`. Missing, empty or malformed
//! all fall back to defaults — a typo must never lock someone out of the app.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(default)]
pub struct Config {
    /// Rows kept visible above and below the list cursor, like vim's `scrolloff`.
    pub scrolloff: usize,
    /// Resume the locally saved track, source and position when Myx starts.
    pub restore_on_startup: bool,
    /// Spotify app client id. `MYX_CLIENT_ID` takes precedence.
    pub client_id: Option<String>,
    /// Terminal graphics protocol: kitty, iterm2, sixel or halfblocks. Set this
    /// when the startup query misfires and the art comes out as a mosaic.
    /// `MYX_PROTOCOL` takes precedence.
    pub protocol: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrolloff: 3,
            restore_on_startup: true,
            client_id: None,
            protocol: None,
        }
    }
}

/// The settings, read once. Shared so the client-id lookup and the UI can't
/// disagree about what the file says.
pub fn get() -> &'static Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    CONFIG.get_or_init(Config::load)
}

/// Written on first run so there is a file to edit instead of a path to guess.
/// Every key is commented out, so it parses to exactly the defaults.
const TEMPLATE: &str = "\
# myx settings. Every key is optional — uncomment one to change it.

# Rows kept visible above and below the list cursor, like vim's scrolloff.
#scrolloff = 3

# Resume the locally saved track, source and position when Myx starts.
#restore_on_startup = true

# Spotify app client id. MYX_CLIENT_ID overrides this if it is set.
#client_id = \"\"

# Terminal graphics protocol: kitty, iterm2, sixel or halfblocks.
# Leave it commented to auto-detect; set it if album art comes out as a coarse
# mosaic, which means the detection query went unanswered.
#protocol = \"kitty\"
";

impl Config {
    pub fn path() -> Option<PathBuf> {
        Some(crate::home_dir()?.join(".config/myx/config.toml"))
    }

    fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        if !path.exists() {
            write_template(&path);
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| Self::parse(&s))
            .unwrap_or_default()
    }

    fn parse(s: &str) -> Option<Self> {
        toml::from_str(s).ok()
    }
}

/// Best effort: a read-only home just means no file, never a failed start.
fn write_template(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, TEMPLATE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_all_defaults() {
        let c = Config::parse("").expect("empty toml is valid");
        assert_eq!(c.scrolloff, 3);
        assert!(c.restore_on_startup);
        assert!(c.client_id.is_none());
    }

    #[test]
    fn reads_keys() {
        let c = Config::parse("scrolloff = 5\nrestore_on_startup = false\nclient_id = \"abc\"")
            .expect("valid toml");
        assert_eq!(c.scrolloff, 5);
        assert!(!c.restore_on_startup);
        assert_eq!(c.client_id.as_deref(), Some("abc"));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // An older myx must not choke on a config written for a newer one.
        let c = Config::parse("scrolloff = 1\nfuture_key = true").expect("valid toml");
        assert_eq!(c.scrolloff, 1);
    }

    #[test]
    fn malformed_config_falls_back_rather_than_failing() {
        assert!(Config::parse("scrolloff = \"three\"").is_none());
    }

    #[test]
    fn the_first_run_template_parses_to_the_defaults() {
        // Everything in it is commented out, so writing it can never change how
        // myx behaves — it only shows what there is to change.
        let c = Config::parse(TEMPLATE).expect("template is valid toml");
        let d = Config::default();
        assert_eq!(c.scrolloff, d.scrolloff);
        assert_eq!(c.restore_on_startup, d.restore_on_startup);
        assert!(c.client_id.is_none());
        assert!(c.protocol.is_none());
    }

    #[test]
    fn the_template_is_written_once_and_never_over_an_existing_file() {
        let dir = std::env::temp_dir().join("myx-config-template");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        write_template(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TEMPLATE);

        std::fs::write(&path, "scrolloff = 9").unwrap();
        // `load` only writes when the file is missing; the edit has to survive.
        assert!(path.exists());
        assert_eq!(
            Config::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .scrolloff,
            9
        );
    }
}
