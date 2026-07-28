//! User settings from `~/.config/myx/config.toml`. Missing, empty or malformed
//! all fall back to defaults — a typo must never lock someone out of the app.

use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(default)]
pub struct Config {
    /// Rows kept visible above and below the list cursor, like vim's `scrolloff`.
    pub scrolloff: usize,
    /// Spotify app client id. `MYX_CLIENT_ID` takes precedence.
    pub client_id: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrolloff: 3,
            client_id: None,
        }
    }
}

/// The settings, read once. Shared so the client-id lookup and the UI can't
/// disagree about what the file says.
pub fn get() -> &'static Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    CONFIG.get_or_init(Config::load)
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        Some(crate::home_dir()?.join(".config/myx/config.toml"))
    }

    fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| Self::parse(&s))
            .unwrap_or_default()
    }

    fn parse(s: &str) -> Option<Self> {
        toml::from_str(s).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_all_defaults() {
        let c = Config::parse("").expect("empty toml is valid");
        assert_eq!(c.scrolloff, 3);
        assert!(c.client_id.is_none());
    }

    #[test]
    fn reads_keys() {
        let c = Config::parse("scrolloff = 5\nclient_id = \"abc\"").expect("valid toml");
        assert_eq!(c.scrolloff, 5);
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
}
