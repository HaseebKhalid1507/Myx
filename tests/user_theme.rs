//! `theme = "…"`: built-ins, theme files, seeds, pywal and base16.

#![cfg(feature = "streaming")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use myx::gradient::Rgb;
use myx::reactive::theme_from_colors;
use myx::theme::{Theme, CATPPUCCIN, GRUVBOX, TOKYONIGHT};
use myx::user_theme::{base16, parse_hex, resolve, wal_theme, Choice, Resolved, Sources};

/// A scratch home for one test: a themes dir and a pywal cache dir, removed
/// when it drops.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "myx-theme-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("themes")).unwrap();
        std::fs::create_dir_all(root.join("wal")).unwrap();
        Self { root }
    }
    fn theme(&self, file: &str, text: &str) -> &Self {
        std::fs::write(self.root.join("themes").join(file), text).unwrap();
        self
    }
    fn wal(&self, json: &str) -> &Self {
        std::fs::write(self.root.join("wal/colors.json"), json).unwrap();
        self
    }
    fn sources(&self) -> Sources {
        Sources {
            themes_dir: Some(self.root.join("themes")),
            wal_colors: Some(self.root.join("wal/colors.json")),
        }
    }
    fn resolve(&self, name: &str) -> Resolved {
        resolve(name, &self.sources())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixed(r: &Resolved) -> Theme {
    match r.choice {
        Choice::Fixed(t) => t,
        Choice::Album => panic!(
            "expected a fixed theme, got album; warnings: {:?}",
            r.warnings
        ),
    }
}

fn is_album(r: &Resolved) -> bool {
    matches!(r.choice, Choice::Album)
}

fn hex(s: &str) -> Rgb {
    parse_hex(s).unwrap()
}

/// Every colour, for comparing two themes whole.
fn colors(t: &Theme) -> [Rgb; 16] {
    [
        t.primary,
        t.secondary,
        t.accent,
        t.error,
        t.warning,
        t.success,
        t.info,
        t.text,
        t.text_muted,
        t.background,
        t.background_panel,
        t.background_element,
        t.border,
        t.border_active,
        t.border_subtle,
        t.border_dimmest,
    ]
}

const OCEAN: &str = r#"scheme: "Ocean"
author: "Chris Kempson (http://chriskempson.com)"
base00: "2b303b"
base01: "343d46"
base02: "4f5b66"
base03: "65737e"
base04: "a7adba"
base05: "c0c5ce"
base06: "dfe1e8"
base07: "eff1f5"
base08: "bf616a"
base09: "d08770"
base0A: "ebcb8b"
base0B: "a3be8c"
base0C: "96b5b4"
base0D: "8fa1b3"
base0E: "b48ead"
base0F: "ab7967"
"#;

const WAL: &str = r##"{
  "wallpaper": "/home/u/wall.jpg",
  "alpha": "100",
  "special": { "background": "#0f1419", "foreground": "#c3c7cb", "cursor": "#c3c7cb" },
  "colors": {
    "color0": "#0f1419", "color1": "#3B5E7A", "color2": "#5A6E86", "color3": "#B5734F",
    "color4": "#4F86A8", "color5": "#8C7FA2", "color6": "#A18FAE", "color7": "#c3c7cb",
    "color8": "#888b8e", "color9": "#3B5E7A", "color10": "#5A6E86", "color11": "#B5734F",
    "color12": "#4F86A8", "color13": "#8C7FA2", "color14": "#A18FAE", "color15": "#c3c7cb"
  }
}"##;

// ------------------------------------------------------------------ parse_hex

#[test]
fn hex_colours_in_the_usual_spellings() {
    assert_eq!(parse_hex("#7aa2f7"), Some(Rgb::new(0x7a, 0xa2, 0xf7)));
    assert_eq!(parse_hex("7AA2F7"), Some(Rgb::new(0x7a, 0xa2, 0xf7)));
    assert_eq!(parse_hex(" #abc "), Some(Rgb::new(0xaa, 0xbb, 0xcc)));
    for bad in ["", "#", "#12345", "#1234567", "zzzzzz", "#ab c", "#+12345"] {
        assert_eq!(parse_hex(bad), None, "{bad:?}");
    }
}

// ---------------------------------------------------------- album & built-ins

#[test]
fn album_is_the_default_and_needs_no_files() {
    let s = Scratch::new();
    for name in ["", "album", "  Album "] {
        let r = s.resolve(name);
        assert!(is_album(&r), "{name:?}");
        assert!(r.warnings.is_empty(), "{name:?}: {:?}", r.warnings);
    }
}

#[test]
fn a_built_in_name_in_any_case() {
    let s = Scratch::new();
    let t = fixed(&s.resolve("Catppuccin"));
    assert_eq!(colors(&t), colors(&CATPPUCCIN));
}

#[test]
fn an_unknown_name_falls_back_to_the_album_and_says_where_it_looked() {
    let s = Scratch::new();
    let r = s.resolve("nope");
    assert!(is_album(&r));
    assert_eq!(r.warnings.len(), 1);
    assert!(r.warnings[0].contains("nope.toml"), "{:?}", r.warnings);
    assert!(r.warnings[0].contains("gruvbox"), "lists the built-ins");
}

#[test]
fn a_theme_name_cannot_reach_outside_the_themes_dir() {
    let s = Scratch::new();
    for name in ["../../etc/passwd", "a/b", ".hidden"] {
        let r = s.resolve(name);
        assert!(is_album(&r), "{name}");
        assert!(r.warnings[0].contains("not a path"), "{:?}", r.warnings);
    }
}

// ---------------------------------------------------------------- theme files

#[test]
fn a_file_starts_from_its_base_and_overrides_what_it_sets() {
    let s = Scratch::new();
    s.theme(
        "mine.toml",
        "base = \"gruvbox\"\nprimary = \"#ff0000\"\nbackground = \"#000\"\n",
    );
    let r = s.resolve("mine");
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let t = fixed(&r);
    assert_eq!(t.name, "mine");
    assert_eq!(t.primary, hex("#ff0000"));
    assert_eq!(t.background, Rgb::new(0, 0, 0));
    assert_eq!(t.accent, GRUVBOX.accent);
    assert_eq!(t.text, GRUVBOX.text);
}

#[test]
fn without_a_base_a_file_starts_from_tokyonight() {
    let s = Scratch::new();
    s.theme("mine.toml", "accent = \"#00ff00\"\n");
    let t = fixed(&s.resolve("mine"));
    assert_eq!(t.accent, hex("#00ff00"));
    assert_eq!(t.primary, TOKYONIGHT.primary);
}

#[test]
fn a_bad_colour_costs_only_that_colour() {
    let s = Scratch::new();
    s.theme(
        "mine.toml",
        "base = \"gruvbox\"\nprimary = \"#zzzzzz\"\naccent = \"#00ff00\"\ntext = 5\n",
    );
    let r = s.resolve("mine");
    let t = fixed(&r);
    assert_eq!(t.primary, GRUVBOX.primary);
    assert_eq!(t.text, GRUVBOX.text);
    assert_eq!(t.accent, hex("#00ff00"));
    assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
    assert!(r
        .warnings
        .iter()
        .any(|w| w.contains("primary = \"#zzzzzz\"")));
    assert!(r.warnings.iter().any(|w| w.contains("text = 5")));
}

#[test]
fn an_unknown_key_is_named_and_the_rest_still_applies() {
    let s = Scratch::new();
    s.theme("mine.toml", "primay = \"#ff0000\"\naccent = \"#00ff00\"\n");
    let r = s.resolve("mine");
    let t = fixed(&r);
    assert_eq!(t.accent, hex("#00ff00"));
    assert_eq!(t.primary, TOKYONIGHT.primary);
    assert_eq!(r.warnings.len(), 1);
    assert!(r.warnings[0].contains("\"primay\""), "{:?}", r.warnings);
    assert!(r.warnings[0].contains("primary"), "lists the real names");
}

#[test]
fn a_file_that_is_not_toml_falls_back_to_the_album() {
    let s = Scratch::new();
    s.theme("mine.toml", "primary = \"#ff0000\n");
    let r = s.resolve("mine");
    assert!(is_album(&r));
    assert!(r.warnings[0].contains("not valid TOML"), "{:?}", r.warnings);
}

#[test]
fn a_seed_derives_the_theme_like_a_cover_would() {
    let s = Scratch::new();
    let seed = ["#7aa2f7", "#bb9af7", "#1a1b26"];
    s.theme(
        "seeded.toml",
        "seed = [\"#7aa2f7\", \"#bb9af7\", \"#1a1b26\"]\nerror = \"#ff0000\"\n",
    );
    let r = s.resolve("seeded");
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let t = fixed(&r);
    let want = theme_from_colors(&seed.map(hex), "x").unwrap();
    // Everything as derived, then the override on top.
    let mut want_colors = colors(&want);
    want_colors[3] = hex("#ff0000");
    assert_eq!(colors(&t), want_colors);
    assert_eq!(t.name, "seeded");
}

#[test]
fn a_seed_skips_what_is_not_a_colour() {
    let s = Scratch::new();
    s.theme("seeded.toml", "seed = [\"#7aa2f7\", \"blue\", 3]\n");
    let r = s.resolve("seeded");
    assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
    let t = fixed(&r);
    let want = theme_from_colors(&[hex("#7aa2f7")], "x").unwrap();
    assert_eq!(colors(&t), colors(&want));
}

#[test]
fn a_seed_with_nothing_usable_falls_back_to_the_base() {
    let s = Scratch::new();
    s.theme("seeded.toml", "base = \"gruvbox\"\nseed = [\"blue\"]\n");
    let r = s.resolve("seeded");
    assert_eq!(colors(&fixed(&r)), colors(&GRUVBOX));
    assert!(r.warnings.iter().any(|w| w.contains("no usable colours")));
}

#[test]
fn seed_wins_over_base_and_says_so() {
    let s = Scratch::new();
    s.theme("both.toml", "base = \"gruvbox\"\nseed = [\"#7aa2f7\"]\n");
    let r = s.resolve("both");
    let want = theme_from_colors(&[hex("#7aa2f7")], "x").unwrap();
    assert_eq!(colors(&fixed(&r)), colors(&want));
    assert!(
        r.warnings[0].contains("base \"gruvbox\" is ignored"),
        "{:?}",
        r.warnings
    );
}

#[test]
fn a_base_must_not_be_another_theme_file() {
    // Two files naming each other as base would never finish resolving.
    let s = Scratch::new();
    s.theme("a.toml", "base = \"b\"\n")
        .theme("b.toml", "base = \"a\"\n");
    let r = s.resolve("a");
    assert_eq!(colors(&fixed(&r)), colors(&TOKYONIGHT));
    assert!(
        r.warnings[0].contains("starting from tokyonight"),
        "{:?}",
        r.warnings
    );
}

#[test]
fn a_base_can_be_a_base16_scheme_or_pywal() {
    let s = Scratch::new();
    s.theme("ocean.yaml", OCEAN)
        .theme("on-ocean.toml", "base = \"ocean\"\naccent = \"#ffffff\"\n")
        .theme("on-wal.toml", "base = \"wal\"\n")
        .wal(WAL);
    let t = fixed(&s.resolve("on-ocean"));
    assert_eq!(t.background, hex("2b303b"));
    assert_eq!(t.accent, hex("#ffffff"));
    let t = fixed(&s.resolve("on-wal"));
    assert_eq!(t.background, hex("#0f1419"));
}

// ---------------------------------------------------------------------- pywal

#[test]
fn wal_keeps_its_own_background_and_text_exactly() {
    let t = wal_theme(WAL).unwrap();
    assert_eq!(t.background, hex("#0f1419"));
    assert_eq!(t.text, hex("#c3c7cb"));
    // The layers in between are blends of the two, lightest last.
    let lum = |c: Rgb| c.r as u32 + c.g as u32 + c.b as u32;
    assert!(lum(t.background) < lum(t.background_panel));
    assert!(lum(t.background_panel) < lum(t.background_element));
    assert!(lum(t.background_element) < lum(t.text_muted));
    // The colour roles come from the palette, not the surface.
    assert_ne!(t.primary, t.background);
    assert_ne!(t.primary, t.text);
}

#[test]
fn wal_through_resolve() {
    let s = Scratch::new();
    s.wal(WAL);
    let t = fixed(&s.resolve("wal"));
    assert_eq!(colors(&t), colors(&wal_theme(WAL).unwrap()));
    assert_eq!(fixed(&s.resolve("pywal")).text, hex("#c3c7cb"));
}

#[test]
fn wal_without_its_colours_says_to_run_wal() {
    let s = Scratch::new();
    let r = s.resolve("wal");
    assert!(is_album(&r));
    assert!(r.warnings[0].contains("run wal first"), "{:?}", r.warnings);
}

#[test]
fn a_broken_wal_file_is_a_warning() {
    let s = Scratch::new();
    s.wal("{ not json");
    assert!(s.resolve("wal").warnings[0].contains("not JSON"));
    s.wal(r##"{"special": {"background": "#000000", "foreground": "#ffffff"}, "colors": {}}"##);
    let r = s.resolve("wal");
    assert!(is_album(&r));
    assert!(r.warnings[0].contains("colors.color1"), "{:?}", r.warnings);
}

// --------------------------------------------------------------------- base16

#[test]
fn base16_slots_map_to_their_meaning() {
    let t = base16(OCEAN, "ocean").unwrap();
    assert_eq!(t.background, hex("2b303b"));
    assert_eq!(t.background_panel, hex("343d46"));
    assert_eq!(t.background_element, hex("4f5b66"));
    assert_eq!(t.text, hex("c0c5ce"));
    assert_eq!(t.error, hex("bf616a"));
    assert_eq!(t.warning, hex("ebcb8b"));
    assert_eq!(t.success, hex("a3be8c"));
    assert_eq!(t.info, hex("96b5b4"));
    assert_eq!(t.primary, hex("8fa1b3"));
    assert_eq!(t.secondary, hex("b48ead"));
    assert_eq!(t.accent, hex("d08770"));
}

#[test]
fn base16_in_tinted_themings_palette_layout() {
    // Newer schemes nest the slots under `palette:`, with '#', in any quotes,
    // sometimes with a comment after.
    let nested: String =
        std::iter::once("system: \"base16\"\nname: \"Ocean\"\npalette:\n".to_string())
            .chain(OCEAN.lines().filter(|l| l.starts_with("base0")).map(|l| {
                let (k, v) = l.split_once(": ").unwrap();
                format!("  {k}: '#{}' # {k}\n", v.trim_matches('"'))
            }))
            .collect();
    assert_eq!(
        colors(&base16(&nested, "x").unwrap()),
        colors(&base16(OCEAN, "x").unwrap())
    );
}

#[test]
fn base16_missing_slots_are_named() {
    let partial: String = OCEAN
        .lines()
        .filter(|l| !l.starts_with("base0A") && !l.starts_with("base0F"))
        .map(|l| format!("{l}\n"))
        .collect();
    let e = base16(&partial, "x").unwrap_err();
    assert!(e.contains("base0A, base0F"), "{e}");
}

#[test]
fn a_base16_yaml_is_found_by_name_but_a_toml_of_the_same_name_wins() {
    let s = Scratch::new();
    s.theme("ocean.yaml", OCEAN);
    assert_eq!(fixed(&s.resolve("ocean")).background, hex("2b303b"));
    s.theme("ocean.toml", "base = \"gruvbox\"\n");
    assert_eq!(colors(&fixed(&s.resolve("ocean"))), colors(&GRUVBOX));
    // .yml works too.
    s.theme("eighties.yml", OCEAN);
    assert_eq!(fixed(&s.resolve("eighties")).text, hex("c0c5ce"));
}
