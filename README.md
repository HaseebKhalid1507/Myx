# myx

A lean, beautiful terminal Spotify player in Rust. Streams natively as a Spotify
Connect device, with album-art-reactive theming, a live audio visualizer, a
ten-band equalizer, and synced lyrics.

<p align="center"><video src="https://github.com/user-attachments/assets/90706ba9-4c48-43d0-ad16-17b95c95dc94" alt="myx recolors the whole interface to the album art" width="100%"></p>

<p align="center">
  <img src="https://github.com/HaseebKhalid1507/Myx/releases/download/readme-assets/myx.png" width="49%">
  <img src="https://github.com/user-attachments/assets/48dfec67-edd1-4903-a18d-8ed6d06ee5f9" width="49%">
  <img src="https://github.com/user-attachments/assets/dd3844ad-f0c7-41ec-a934-86bb0e3ef75b" width="49%">
  <img src="https://github.com/user-attachments/assets/08b3f505-5e48-4cd8-9b8d-0788d37f30c2" width="49%">
</p>

> Requires **Spotify Premium**. Works on Linux, macOS, and Windows. Album art is
> crispest on kitty, WezTerm, or foot.

## Install

```bash
# Arch (AUR)
yay -S myx

# macOS / Linux (Homebrew)
brew install HaseebKhalid1507/homebrew-tap/myx

# Cargo (all platforms — Linux, macOS, Windows)
cargo install myx

# Prebuilt binary (Linux x86_64, macOS)
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/HaseebKhalid1507/Myx/releases/latest/download/myx-installer.sh | sh
```

On **Windows**, install [Rust](https://rustup.rs) first, then `cargo install myx` in
PowerShell. Set `MYX_CLIENT_ID` as an environment variable or place your client ID in
`%USERPROFILE%\.config\myx\client_id`.

Or grab a `.deb` / archive from [Releases](https://github.com/HaseebKhalid1507/Myx/releases),
or build from source: `cargo install --path .`.

## Get started

You need a free Spotify app client ID (one minute):

1. [Spotify developer dashboard](https://developer.spotify.com/dashboard), then **Create app**
2. Add the redirect URI `http://127.0.0.1:8989/login`
3. Copy the **Client ID** and set it:

```bash
export MYX_CLIENT_ID=<your-client-id>
```

Then run:

```bash
myx
```

First launch opens your browser to log in (OAuth PKCE, no secret needed). Then
browse with `↑↓` and hit `⏎` to play. After that, just `myx`.

## Keys

```
⇥ / [ ]    switch section        ← →      switch view
↑↓ / j k   move                  ⏎        play / open
⇧ ⏎        play the highlighted album, playlist or artist
/          search Spotify        a        actions
f          find in this list
space      play · pause          n / b    next · prev
⇧ ← →      seek                  s        shuffle
+ / -      volume                R        repeat
o          sort                  r        reload
z          hide sidebar          e        equalizer
q          quit                  T        transparent background
```

`f` finds in whatever list is on screen — a playlist, an album, Liked Songs,
search results. Type to narrow it (every word has to appear in the title or
the artist), `↑`/`↓` to pick, `⏎` to play: the whole list plays from that
track, exactly as it would without the find. `Esc` clears it, and the next
`Esc` goes back as usual.

Media keys (Play/Pause, Stop, Next, Prev, Volume) work when the terminal is
focused. On macOS and Linux, AirPods and headphone controls work from anywhere
via the system's Now Playing integration. Mouse works too: click tabs, click a
track, double-click to play.

## Transparent background

myx can leave the background to your terminal, so its own colour, opacity and
blur show through while the text, accents and borders keep following the album
art. The selected row and popups stay on a solid fill so they still read.

- **Press `T`** (Shift+t) to flip between the transparent and the album-tinted
  background. The choice is saved to `~/.config/myx/config.toml`, so it sticks.
- **Or set it by hand**: `transparent = true` or `false` in the same file.

New installs start transparent. A config written by an older myx has no
`transparent` line and keeps the album-tinted background until you press `T`.
How see-through it gets is up to the terminal — WezTerm's
`window_background_opacity`, kitty's `background_opacity`, and so on.

## Themes

By default myx takes its colours from the album art and fades to each new
cover. To keep one palette instead, set `theme` in `~/.config/myx/config.toml`:

```toml
theme = "catppuccin"   # album (default), tokyonight, catppuccin, rosepine, gruvbox,
                       # wal, or the name of your own theme (below)
```

With `transparent = true` as well, the background is your terminal's and the
colours never change, so myx sits with the rest of your setup. Restart myx after
changing the theme. If a theme can't be read, myx says why in the status line
and uses the album's colours.

### Your own theme

Put `<name>.toml` in `~/.config/myx/themes/` and set `theme = "<name>"`. Start
from a palette and change only what you want:

```toml
# ~/.config/myx/themes/mine.toml
base = "catppuccin"     # optional: a built-in, "wal", or a base16 scheme name
primary = "#cba6f7"
accent = "#f5c2e7"
background = "#11111b"
```

Or give a few colours and let myx build the rest, the same way it builds a
theme from album art (any colour you set as well still wins):

```toml
seed = ["#7aa2f7", "#bb9af7", "#e0af68"]
```

Colours are `#rrggbb` or `#rgb`. A colour that doesn't parse, or a name myx
doesn't know, is skipped and reported; the rest of the theme still applies.
What each colour paints:

| Name | Where you see it |
|------|------------------|
| `primary` | headings, key hints, the current lyric line, the selected equalizer band, and the start of the logo and progress bar gradients |
| `accent` | highlighted rows, and the far end of the logo, progress bar and visualizer gradients |
| `info` | the low end of the visualizer (`primary` is its middle) |
| `success` | the footer's `s` and `z` hints while shuffle and zen are on, the equalizer's on switch and its bands |
| `text` · `text_muted` | normal and de-emphasised text |
| `background` · `background_panel` · `background_element` | the screen, the panes on it, and the selected row and popups |
| `border_active` · `border_subtle` | the bar beside the focused and the unfocused pane; `border_subtle` also dims lyrics already sung |
| `border_dimmest` | separators, empty progress and scrollbar tracks |
| `secondary` · `error` · `warning` · `border` | accepted, but nothing in myx draws with them yet |

### pywal

`theme = "wal"` uses the colours from your last `wal` run
(`~/.cache/wal/colors.json`): wal's own background and text exactly, the
shades between them blended from those two, and the accent colours picked from
wal's palette. Run `wal` again and restart myx to pick up a new wallpaper's
colours. myx is designed for dark backgrounds, so light wal schemes (`wal -l`)
won't look right.

### base16

Drop a base16 scheme into `~/.config/myx/themes/` as `<name>.yaml` (either the
classic layout or tinted-theming's `palette:` one) and set `theme = "<name>"`.
It can also be the `base` of your own theme file.

## Equalizer

Press `e` for the live ten-band equalizer. Choose Flat, Bass Boost, Rock, Jazz,
Vocal, Electronic or Treble Boost with `Tab` / `Shift+Tab`; changing a band
creates a Custom curve. Use `←`/`→` to choose a band, `↑`/`↓` to adjust it,
`Space` to compare against bypass, and `Esc` to close. Presets and sliders are
also clickable and draggable. Automatic headroom keeps boosted curves from
digitally clipping, and the complete curve is restored on the next launch.

Holding `⇧ ←` / `⇧ →` scrubs continuously and commits one seek when you let go.
`⇧ ⏎` needs a terminal that reports modified Enter (kitty, WezTerm, foot); `P`
does the same everywhere.

The album-art protocol is detected at startup and picked per terminal, including
inside tmux. If your tmux has no sixel support, add `set -g focus-events on` to
`~/.tmux.conf` so the art is re-sent when you switch back to myx's window.

## Config

`~/.config/myx/config.toml` is written on first run with every key but
`transparent` commented out, so there is a file to edit and nothing to look up:

```toml
# Rows kept visible above and below the cursor before the list scrolls.
scrolloff = 3

# Show the locally saved track, source and position when Myx starts, paused
# until you press play.
restore_on_startup = true

# Spotify app client id. MYX_CLIENT_ID overrides this if it is set.
client_id = "your-client-id"

# kitty, iterm2, sixel or halfblocks. Leave it out to auto-detect; set it if
# album art comes out as a coarse mosaic, which means the terminal never
# answered the detection query. MYX_PROTOCOL overrides this.
protocol = "kitty"

# Streaming quality in kbps: 96, 160 or 320. Any other whole number falls
# back to 160.
bitrate = 160

# Even out loudness across tracks, the equivalent of the official client's
# "Normalize volume". Leave it off to keep each track's own dynamics.
normalize_volume = false

# Leave the background to the terminal — its own colour, opacity and blur —
# instead of the album-tinted one. Text and accents still follow the cover.
# Press T in myx to flip it; this line is rewritten when you do.
transparent = true

# Colours. "album" follows each cover. To keep one palette instead, use
# tokyonight, catppuccin, rosepine or gruvbox; "wal" for pywal's colours; or
# the name of your own theme in ~/.config/myx/themes/ (see Themes above).
#theme = "album"
```

## Credits

Streaming adapts pieces of [spotify-player](https://github.com/aome510/spotify-player)
(MIT, © Thang Pham); visual language after [noodle](https://github.com/wilfredinni/noodle);
built on [ratatui](https://ratatui.rs) and [librespot](https://github.com/librespot-org/librespot).
See [NOTICE](NOTICE).

## License

MIT, see [LICENSE](LICENSE).
