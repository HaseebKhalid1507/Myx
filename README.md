# myx

A lean, beautiful terminal Spotify player in Rust. Streams natively as a Spotify
Connect device, with album-art-reactive theming, a live audio visualizer, and
synced lyrics.

<p align="center"><img src="https://github.com/HaseebKhalid1507/Myx/releases/download/readme-assets/myx.png" alt="myx recolors the whole interface to the album art" width="100%"></p>

<p align="center">
  <img src="https://github.com/HaseebKhalid1507/Myx/releases/download/readme-assets/theme-1.png" width="32%">
  <img src="https://github.com/HaseebKhalid1507/Myx/releases/download/readme-assets/theme-2.png" width="32%">
  <img src="https://github.com/HaseebKhalid1507/Myx/releases/download/readme-assets/theme-3.png" width="32%">
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
/          search                a        actions
space      play · pause          n / b    next · prev
⇧ ← →      seek                  s        shuffle
+ / -      volume                R        repeat
o          sort                  r        reload
q          quit
```

Media keys (Play/Pause, Stop, Next, Prev, Volume) work when the terminal is
focused. Mouse works too: click tabs, click a track, double-click to play.

## Credits

Streaming adapts pieces of [spotify-player](https://github.com/aome510/spotify-player)
(MIT, © Thang Pham); visual language after [noodle](https://github.com/wilfredinni/noodle);
built on [ratatui](https://ratatui.rs) and [librespot](https://github.com/librespot-org/librespot).
See [NOTICE](NOTICE).

## License

MIT, see [LICENSE](LICENSE).
