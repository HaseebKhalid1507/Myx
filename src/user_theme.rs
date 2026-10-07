//! Which palette the UI wears: the album's (the default), a built-in one, or
//! one of your own — a theme file, a few seed colours, pywal's current colours
//! or a base16 scheme. Set with `theme = "…"` in `config.toml`.
//!
//! Nothing here can stop Myx from starting. A theme that can't be read falls
//! back to the album's colours, and every problem comes back as a warning
//! instead: a bad colour costs that one colour, an unknown key is named, and a
//! broken file costs that theme only — never the rest of `config.toml`, which
//! parses all-or-nothing.

use std::path::{Path, PathBuf};

use crate::gradient::{lerp_color, Rgb};
use crate::reactive::theme_from_colors;
use crate::theme::{Theme, THEMES, TOKYONIGHT};

/// What the UI should do with its colours.
#[derive(Debug, Clone, Copy)]
pub enum Choice {
    /// Fade to each cover's palette, as Myx always has.
    Album,
    /// Keep this palette whatever is playing.
    Fixed(Theme),
}

/// A [`Choice`] plus everything worth telling the user about how it was made.
#[derive(Debug)]
pub struct Resolved {
    pub choice: Choice,
    pub warnings: Vec<String>,
}

/// Where user themes come from. Its own type so tests can point it at a
/// scratch directory instead of the real home.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// `~/.config/myx/themes`, holding `<name>.toml` and base16 `<name>.yaml`.
    pub themes_dir: Option<PathBuf>,
    /// pywal's `colors.json`.
    pub wal_colors: Option<PathBuf>,
}

impl Sources {
    pub fn from_env() -> Self {
        let home = crate::home_dir();
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.as_ref().map(|h| h.join(".cache")));
        Self {
            themes_dir: home.map(|h| h.join(".config/myx/themes")),
            wal_colors: cache.map(|c| c.join("wal/colors.json")),
        }
    }
}

/// The 16 colours a theme is made of, as they are named in theme files.
pub const TOKENS: [&str; 16] = [
    "primary",
    "secondary",
    "accent",
    "error",
    "warning",
    "success",
    "info",
    "text",
    "text_muted",
    "background",
    "background_panel",
    "background_element",
    "border",
    "border_active",
    "border_subtle",
    "border_dimmest",
];

fn token_mut<'a>(t: &'a mut Theme, key: &str) -> Option<&'a mut Rgb> {
    Some(match key {
        "primary" => &mut t.primary,
        "secondary" => &mut t.secondary,
        "accent" => &mut t.accent,
        "error" => &mut t.error,
        "warning" => &mut t.warning,
        "success" => &mut t.success,
        "info" => &mut t.info,
        "text" => &mut t.text,
        "text_muted" => &mut t.text_muted,
        "background" => &mut t.background,
        "background_panel" => &mut t.background_panel,
        "background_element" => &mut t.background_element,
        "border" => &mut t.border,
        "border_active" => &mut t.border_active,
        "border_subtle" => &mut t.border_subtle,
        "border_dimmest" => &mut t.border_dimmest,
        _ => return None,
    })
}

/// `#rrggbb`, `rrggbb`, `#rgb` or `rgb`, any case. Unlike [`Rgb::from_hex`],
/// which turns garbage into black, this says so.
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.trim();
    let h = h.strip_prefix('#').unwrap_or(h);
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    let nibble = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
    match h.len() {
        6 => Some(Rgb::new(byte(0)?, byte(2)?, byte(4)?)),
        3 => Some(Rgb::new(nibble(0)?, nibble(1)?, nibble(2)?)),
        _ => None,
    }
}

/// Theme names live for the whole process; there is one per run.
fn leak(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

fn builtin(name: &str) -> Option<Theme> {
    THEMES
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .copied()
}

/// Resolve `theme = "…"` from `config.toml`.
pub fn resolve(name: &str, src: &Sources) -> Resolved {
    let mut warnings = Vec::new();
    let name = name.trim();
    let choice = match pick(name, src, &mut warnings) {
        Ok(choice) => choice,
        Err(e) => {
            warnings.push(format!("theme \"{name}\": {e} — using the album's colours"));
            Choice::Album
        }
    };
    Resolved { choice, warnings }
}

fn pick(name: &str, src: &Sources, warnings: &mut Vec<String>) -> Result<Choice, String> {
    if name.is_empty() || name.eq_ignore_ascii_case("album") {
        return Ok(Choice::Album);
    }
    if let Some(t) = builtin(name) {
        return Ok(Choice::Fixed(t));
    }
    if is_wal(name) {
        return wal(src).map(Choice::Fixed);
    }
    let dir = theme_dir(name, src)?;
    let toml_path = dir.join(format!("{name}.toml"));
    if toml_path.is_file() {
        let text = read(&toml_path)?;
        return theme_file(&text, leak(name.to_string()), src, warnings).map(Choice::Fixed);
    }
    if let Some(path) = base16_path(dir, name) {
        return base16(&read(&path)?, leak(name.to_string())).map(Choice::Fixed);
    }
    Err(format!(
        "not a built-in theme ({}), \"album\", \"wal\", or a file at {}",
        THEMES.iter().map(|t| t.name).collect::<Vec<_>>().join(", "),
        toml_path.display()
    ))
}

fn is_wal(name: &str) -> bool {
    name.eq_ignore_ascii_case("wal") || name.eq_ignore_ascii_case("pywal")
}

/// The themes directory, after checking `name` can't step outside it.
fn theme_dir<'a>(name: &str, src: &'a Sources) -> Result<&'a Path, String> {
    if name.contains(['/', '\\']) || name.starts_with('.') {
        return Err("a theme name is a file name in ~/.config/myx/themes, not a path".into());
    }
    src.themes_dir
        .as_deref()
        .ok_or_else(|| "no home directory to look for themes in".to_string())
}

fn base16_path(dir: &Path, name: &str) -> Option<PathBuf> {
    ["yaml", "yml"]
        .iter()
        .map(|ext| dir.join(format!("{name}.{ext}")))
        .find(|p| p.is_file())
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))
}

/// The palette a theme file's `base = "…"` starts from: a built-in, "wal", or
/// a base16 scheme in the themes directory. Not another theme file, so two
/// files can never point at each other.
fn base_palette(name: &str, src: &Sources) -> Result<Theme, String> {
    if let Some(t) = builtin(name) {
        return Ok(t);
    }
    if is_wal(name) {
        return wal(src);
    }
    let dir = theme_dir(name, src)?;
    match base16_path(dir, name) {
        Some(path) => base16(&read(&path)?, leak(name.to_string())),
        None => Err(format!(
            "base \"{name}\" is not a built-in theme, \"wal\", or a base16 scheme ({name}.yaml)"
        )),
    }
}

// ---------------------------------------------------------------- theme files

/// A TOML value as a user would recognise it in their file. `toml::Value`'s own
/// `Display` needs the crate's serializer, which Myx doesn't build.
fn describe(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => format!("\"{s}\""),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(d) => d.to_string(),
        toml::Value::Array(_) => "a list".into(),
        toml::Value::Table(_) => "a table".into(),
    }
}

/// A `<name>.toml` theme:
///
/// ```toml
/// base = "catppuccin"                         # where unset colours come from
/// seed = ["#7aa2f7", "#bb9af7", "#1a1b26"]    # or derive them, like a cover
/// primary = "#cba6f7"                         # then any of the 16 colours
/// ```
///
/// Only a file that isn't TOML at all is an error; anything else wrong with it
/// is a warning and the rest of the file still applies.
pub fn theme_file(
    text: &str,
    name: &'static str,
    src: &Sources,
    warnings: &mut Vec<String>,
) -> Result<Theme, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| format!("not valid TOML: {e}"))?;
    let mut warn = |msg: String| warnings.push(format!("theme \"{name}\": {msg}"));

    let seed = match table.get("seed") {
        None => None,
        Some(toml::Value::Array(items)) => {
            let mut colors = Vec::new();
            for item in items {
                match item.as_str().and_then(parse_hex) {
                    Some(c) => colors.push(c),
                    None => warn(format!(
                        "seed: {} is not a colour like \"#7aa2f7\"",
                        describe(item)
                    )),
                }
            }
            if colors.is_empty() {
                warn("seed has no usable colours".into());
            }
            theme_from_colors(&colors, name)
        }
        Some(other) => {
            warn(format!(
                "seed should be a list of colours, like [\"#7aa2f7\", \"#bb9af7\"], not {}",
                describe(other)
            ));
            None
        }
    };

    let base = match table.get("base") {
        None => None,
        Some(toml::Value::String(b)) if seed.is_some() => {
            warn(format!(
                "base \"{b}\" is ignored: seed already sets every colour"
            ));
            None
        }
        Some(toml::Value::String(b)) => match base_palette(b.trim(), src) {
            Ok(t) => Some(t),
            Err(e) => {
                warn(format!("{e}; starting from tokyonight"));
                None
            }
        },
        Some(other) => {
            warn(format!(
                "base should be a name in quotes, not {}",
                describe(other)
            ));
            None
        }
    };

    let mut theme = Theme {
        name,
        ..seed.or(base).unwrap_or(TOKYONIGHT)
    };

    for (key, value) in &table {
        if key == "base" || key == "seed" {
            continue;
        }
        let Some(slot) = token_mut(&mut theme, key) else {
            warn(format!(
                "unknown key \"{key}\" (colours are: {})",
                TOKENS.join(", ")
            ));
            continue;
        };
        match value.as_str().and_then(parse_hex) {
            Some(c) => *slot = c,
            None => warn(format!(
                "{key} = {} is not a colour like \"#7aa2f7\"",
                describe(value)
            )),
        }
    }
    Ok(theme)
}

// ---------------------------------------------------------------------- pywal

fn wal(src: &Sources) -> Result<Theme, String> {
    let path = src
        .wal_colors
        .as_deref()
        .ok_or_else(|| "no home directory to find pywal's colours in".to_string())?;
    if !path.is_file() {
        return Err(format!(
            "pywal's colours aren't at {} — run wal first",
            path.display()
        ));
    }
    wal_theme(&read(path)?)
}

/// pywal's `colors.json`. Background and text are wal's own, exactly, so Myx
/// sits on the same surface as the terminal; the surfaces in between are
/// blended from those two; the colour roles are picked from `color1`–`color6`
/// the way a cover's are.
pub fn wal_theme(json: &str) -> Result<Theme, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("pywal's colors.json is not JSON: {e}"))?;
    let get = |section: &str, key: &str| -> Result<Rgb, String> {
        v[section][key]
            .as_str()
            .and_then(parse_hex)
            .ok_or_else(|| format!("pywal's colors.json has no colour at {section}.{key}"))
    };
    let bg = get("special", "background")?;
    let fg = get("special", "foreground")?;
    let swatches = (1..=6)
        .map(|i| get("colors", &format!("color{i}")))
        .collect::<Result<Vec<_>, _>>()?;
    let derived = theme_from_colors(&swatches, "wal").unwrap_or(TOKYONIGHT);
    Ok(on_surface(derived, bg, fg))
}

/// `theme` with its background, text and every shade between them taken from
/// a scheme's own background and foreground.
fn on_surface(theme: Theme, bg: Rgb, fg: Rgb) -> Theme {
    let mix = |t: f32| lerp_color(bg, fg, t);
    Theme {
        background: bg,
        background_panel: mix(0.04),
        background_element: mix(0.10),
        text: fg,
        text_muted: mix(0.55),
        border: mix(0.35),
        border_subtle: mix(0.22),
        border_dimmest: mix(0.12),
        ..theme
    }
}

// --------------------------------------------------------------------- base16

/// A base16 scheme (`base00` … `base0F`), in either the classic flat layout or
/// tinted-theming's `palette:` one. Only those 16 lines are read, so this needs
/// no YAML parser.
///
/// base16 gives every slot a meaning, so the mapping is direct: 00–02 are the
/// three background layers, 03–05 the comment/dim/default foregrounds, and
/// 08–0E red, orange, yellow, green, cyan, blue, magenta.
pub fn base16(text: &str, name: &'static str) -> Result<Theme, String> {
    let mut slots: [Option<Rgb>; 16] = [None; 16];
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
        let Some(index) = key
            .strip_prefix("base0")
            .filter(|d| d.len() == 1)
            .and_then(|d| usize::from_str_radix(d, 16).ok())
        else {
            continue;
        };
        let value = value.trim();
        // "#2b303b" or '2b303b' or 2b303b, any comment after it ignored.
        let value = match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or(""),
            _ => value.split_whitespace().next().unwrap_or(""),
        };
        slots[index] = parse_hex(value);
    }
    let missing: Vec<String> = (0..16)
        .filter(|&i| slots[i].is_none())
        .map(|i| format!("base0{i:X}"))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "not a base16 scheme: no colour for {}",
            missing.join(", ")
        ));
    }
    let b = |i: usize| slots[i].unwrap_or(Rgb::new(0, 0, 0));
    Ok(Theme {
        name,
        primary: b(0xD),
        secondary: b(0xE),
        accent: b(0x9),
        error: b(0x8),
        warning: b(0xA),
        success: b(0xB),
        info: b(0xC),
        text: b(0x5),
        text_muted: b(0x4),
        background: b(0x0),
        background_panel: b(0x1),
        background_element: b(0x2),
        border: b(0x3),
        border_active: b(0xD),
        border_subtle: b(0x2),
        border_dimmest: b(0x1),
        transparent: false,
    })
}
