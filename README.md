# myx

A lean, beautiful terminal Spotify player in Rust. Streams natively as a Spotify
Connect device, with album-art-reactive theming, a live audio visualizer, and
synced lyrics.

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
/          search                a        actions
space      play · pause          n / b    next · prev
⇧ ← →      seek                  s        shuffle
+ / -      volume                R        repeat
o          sort                  r        reload
z          hide sidebar          q        quit
```

Media keys (Play/Pause, Stop, Next, Prev, Volume) work when the terminal is
focused. Mouse works too: click tabs, click a track, double-click to play.

Holding `⇧ ←` / `⇧ →` scrubs continuously and commits one seek when you let go.
`⇧ ⏎` needs a terminal that reports modified Enter (kitty, WezTerm, foot); `P`
does the same everywhere.

The album-art protocol is detected at startup and picked per terminal, including
inside tmux. If your tmux has no sixel support, add `set -g focus-events on` to
`~/.tmux.conf` so the art is re-sent when you switch back to myx's window.

## Config

`~/.config/myx/config.toml` is written on first run with every key commented
out, so there is a file to edit and nothing to look up:

```toml
# Rows kept visible above and below the cursor before the list scrolls.
scrolloff = 3

# Resume the locally saved track, source and position when Myx starts.
restore_on_startup = true

# Spotify app client id. MYX_CLIENT_ID overrides this if it is set.
client_id = "your-client-id"

# kitty, iterm2, sixel or halfblocks. Leave it out to auto-detect; set it if
# album art comes out as a coarse mosaic, which means the terminal never
# answered the detection query. MYX_PROTOCOL overrides this.
protocol = "kitty"
```

## Credits

Streaming adapts pieces of [spotify-player](https://github.com/aome510/spotify-player)
(MIT, © Thang Pham); visual language after [noodle](https://github.com/wilfredinni/noodle);
built on [ratatui](https://ratatui.rs) and [librespot](https://github.com/librespot-org/librespot).
See [NOTICE](NOTICE).

## License

MIT, see [LICENSE](LICENSE).
