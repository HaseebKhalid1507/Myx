//! User settings from `~/.config/myx/config.toml`. Missing, empty or malformed
//! all fall back to defaults — a typo must never lock someone out of the app.

use librespot_playback::config::Bitrate;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(default)]
pub struct Config {
    /// Rows kept visible above and below the list cursor, like vim's `scrolloff`.
    pub scrolloff: usize,
    /// Show the locally saved track, source and position when Myx starts,
    /// paused until the first play press.
    pub restore_on_startup: bool,
    /// Spotify app client id. `MYX_CLIENT_ID` takes precedence.
    pub client_id: Option<String>,
    /// Terminal graphics protocol: kitty, iterm2, sixel or halfblocks. Set this
    /// when the startup query misfires and the art comes out as a mosaic.
    /// `MYX_PROTOCOL` takes precedence.
    pub protocol: Option<String>,
    /// Streaming quality in kbps. Spotify serves 96, 160 and 320; any other
    /// whole number falls back to 160, in keeping with the rest of this file.
    /// Widest TOML integer on purpose: this file parses whole-or-nothing, so a
    /// value that failed to deserialize would not cost you the bitrate, it
    /// would silently reset every other key including `client_id`.
    pub bitrate: i64,
    /// Even out loudness across tracks, the equivalent of the official client's
    /// "Normalize volume". Off leaves each track's own dynamics alone.
    pub normalize_volume: bool,
    /// Leave the background to the terminal instead of painting the
    /// album-tinted one. Text and accents still follow the cover. Off when the
    /// key is missing; the first-run template sets it, so new installs start
    /// transparent. `T` in the app flips it and rewrites the line.
    pub transparent: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrolloff: 3,
            restore_on_startup: true,
            client_id: None,
            protocol: None,
            bitrate: 160,
            normalize_volume: false,
            transparent: false,
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
/// Every key but `transparent` is commented out, so it parses to the defaults
/// apart from the transparent background new installs start with.
const TEMPLATE: &str = "\
# myx settings. Every key is optional — uncomment one to change it.

# Rows kept visible above and below the list cursor, like vim's scrolloff.
#scrolloff = 3

# Show the locally saved track, source and position when Myx starts, paused
# until you press play.
#restore_on_startup = true

# Spotify app client id. MYX_CLIENT_ID overrides this if it is set.
#client_id = \"\"

# Terminal graphics protocol: kitty, iterm2, sixel or halfblocks.
# Leave it commented to auto-detect; set it if album art comes out as a coarse
# mosaic, which means the detection query went unanswered.
#protocol = \"kitty\"

# Streaming quality in kbps: 96, 160 or 320. Any other whole number falls
# back to 160.
#bitrate = 160

# Even out loudness across tracks, like the official client's \"Normalize
# volume\". Leave it off to keep each track's own dynamics.
#normalize_volume = false

# Leave the background to the terminal — its own colour, opacity and blur —
# instead of the album-tinted one. Text and accents still follow the cover.
# Press T in myx to flip it; this line is rewritten when you do.
transparent = true
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

    /// The configured quality snapped to one Spotify actually serves, so a
    /// stray number degrades to the default instead of reaching the player.
    /// One total function rather than a second match at the call site: the
    /// mapping that decides what you hear should be testable without a
    /// sound card.
    pub fn bitrate(&self) -> Bitrate {
        match self.bitrate {
            96 => Bitrate::Bitrate96,
            320 => Bitrate::Bitrate320,
            160 => Bitrate::Bitrate160,
            other => {
                // Snapping is deliberate, but a user who typed 256 otherwise
                // has no way to find out their setting never reached the player.
                log::warn!("bitrate {other} is not one Spotify serves; using 160");
                Bitrate::Bitrate160
            }
        }
    }

    /// Persist a flip of `transparent` by rewriting its one line, so the rest
    /// of a hand-edited file — comments and all — survives.
    pub fn save_transparent(on: bool) -> std::io::Result<()> {
        Self::save_key("transparent", &on.to_string())
    }

    /// Persist the client id typed at the first-run prompt, the same way. The
    /// caller has checked it is a plain hex id, so it needs no TOML escaping.
    pub fn save_client_id(id: &str) -> std::io::Result<()> {
        Self::save_key("client_id", &format!("\"{id}\""))
    }

    /// Set one top-level `key = value` line in `config.toml`, leaving every
    /// other line alone. A missing file starts from the template.
    fn save_key(key: &str, value: &str) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory")
        })?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => TEMPLATE.to_string(),
            // A file we cannot read may still be one we could overwrite; the
            // template must never clobber it.
            Err(e) => return Err(e),
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, set_key(&text, key, value))
    }
}

/// Best effort: a read-only home just means no file, never a failed start.
fn write_template(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, TEMPLATE);
}

/// `text` with `key = value`. An existing line, live or commented out, is
/// replaced in place; otherwise one is added above the first table, since a
/// bare key after a `[table]` header would belong to that table.
fn set_key(text: &str, key: &str, value: &str) -> String {
    let is_key = |l: &str| {
        l.trim_start()
            .trim_start_matches('#')
            .trim_start()
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    };
    let is_live = |l: &str| !l.trim_start().starts_with('#');
    let is_table = |l: &str| l.trim_start().starts_with('[') && l.trim_end().ends_with(']');

    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let line = format!("{key} = {value}");
    let existing = lines
        .iter()
        .position(|l| is_live(l) && is_key(l))
        .or_else(|| lines.iter().position(|l| is_key(l)));
    match existing {
        Some(i) => lines[i] = line,
        None => {
            let at = lines
                .iter()
                .position(|l| is_table(l))
                .unwrap_or(lines.len());
            lines.insert(at, line);
        }
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_transparent(text: &str, on: bool) -> String {
        set_key(text, "transparent", &on.to_string())
    }

    #[test]
    fn saving_a_client_id_fills_the_template_line_and_reads_back() {
        let id = "0123456789abcdef0123456789abcdef";
        let text = set_key(TEMPLATE, "client_id", &format!("\"{id}\""));
        assert_eq!(text.lines().count(), TEMPLATE.lines().count());
        let c = Config::parse(&text).expect("valid toml");
        assert_eq!(c.client_id.as_deref(), Some(id));

        // An empty live key and a commented one: only the live line changes,
        // so the file never ends up with the key twice.
        let text = set_key(
            "#client_id = \"\"\nclient_id = \"\"\n",
            "client_id",
            &format!("\"{id}\""),
        );
        assert_eq!(text, format!("#client_id = \"\"\nclient_id = \"{id}\"\n"));
        let c = Config::parse(&text).expect("valid toml");
        assert_eq!(c.client_id.as_deref(), Some(id));
    }

    #[test]
    fn empty_config_is_all_defaults() {
        let c = Config::parse("").expect("empty toml is valid");
        assert_eq!(c.scrolloff, 3);
        assert!(c.restore_on_startup);
        assert!(c.client_id.is_none());
        assert_eq!(c.bitrate, 160);
        assert!(!c.normalize_volume);
        assert!(!c.transparent);
    }

    #[test]
    fn reads_keys() {
        let c = Config::parse("scrolloff = 5\nrestore_on_startup = false\nclient_id = \"abc\"")
            .expect("valid toml");
        assert_eq!(c.scrolloff, 5);
        assert!(!c.restore_on_startup);
        assert_eq!(c.client_id.as_deref(), Some("abc"));

        let c = Config::parse("normalize_volume = true").expect("valid toml");
        assert!(c.normalize_volume);

        let c = Config::parse("transparent = true").expect("valid toml");
        assert!(c.transparent);
    }

    #[test]
    fn an_unserved_bitrate_falls_back_to_the_default() {
        // Same contract as the rest of the file: a wrong value costs you the
        // setting, never the app. Negative and out-of-u16-range are the cases
        // that matter -- a narrower field type fails the parse on those, and
        // this file parses whole-or-nothing.
        for (written, effective) in [
            (96, Bitrate::Bitrate96),
            (160, Bitrate::Bitrate160),
            (320, Bitrate::Bitrate320),
            (0, Bitrate::Bitrate160),
            (256, Bitrate::Bitrate160),
            (-1, Bitrate::Bitrate160),
            (99999, Bitrate::Bitrate160),
        ] {
            let c = Config::parse(&format!("bitrate = {written}")).expect("valid toml");
            assert_eq!(c.bitrate(), effective, "bitrate = {written}");
        }
    }

    #[test]
    fn an_unserved_bitrate_does_not_cost_you_the_other_keys() {
        // The whole file parses or none of it does, so a bitrate that failed to
        // deserialize would take client_id -- the one setting the app cannot
        // start without -- down with it.
        let c = Config::parse("client_id = \"abc\"\nbitrate = 99999").expect("valid toml");
        assert_eq!(c.client_id.as_deref(), Some("abc"));
        assert_eq!(c.bitrate(), Bitrate::Bitrate160);
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
        assert_eq!(c.bitrate, d.bitrate);
        assert_eq!(c.normalize_volume, d.normalize_volume);
        // The one live key: new installs start transparent, while a config
        // without the line keeps the album background, which is the default.
        assert!(c.transparent);
        assert!(!d.transparent);
    }

    #[test]
    fn flipping_transparency_rewrites_only_its_own_line() {
        assert_eq!(
            set_transparent("scrolloff = 5\ntransparent = false\n", true),
            "scrolloff = 5\ntransparent = true\n"
        );
        // A commented-out line is taken over in place, next to its doc comment.
        assert_eq!(
            set_transparent("# doc\n#transparent = false\nbitrate = 320\n", true),
            "# doc\ntransparent = true\nbitrate = 320\n"
        );
        // A lookalike key is not ours.
        assert_eq!(
            set_transparent("transparent_level = 3\n", false),
            "transparent_level = 3\ntransparent = false\n"
        );
    }

    #[test]
    fn a_config_without_the_line_gets_one_toml_reads_back() {
        let text = set_transparent("client_id = \"abc\"", true);
        let c = Config::parse(&text).expect("valid toml");
        assert!(c.transparent);
        assert_eq!(c.client_id.as_deref(), Some("abc"));

        // After a table header a bare key would belong to the table.
        assert_eq!(
            set_transparent("a = 1\n[x]\nb = 2\n", true),
            "a = 1\ntransparent = true\n[x]\nb = 2\n"
        );
    }

    #[test]
    fn flipping_the_template_keeps_every_other_line() {
        let off = set_transparent(TEMPLATE, false);
        assert_eq!(off.lines().count(), TEMPLATE.lines().count());
        assert!(!Config::parse(&off).expect("valid toml").transparent);
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
