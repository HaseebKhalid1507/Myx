# myx

A lean, beautiful terminal Spotify player — built in Rust with [ratatui](https://ratatui.rs)
and [librespot](https://github.com/librespot-org/librespot). Streams natively as a
Spotify Connect device, with **album-art-reactive theming**, a live **FFT visualizer**,
**synced lyrics**, and full library browsing — all in the terminal.

> Requires **Spotify Premium** (Spotify Connect streaming is Premium-only).

## Features

- 🎨 **Album-art-reactive theming** — the whole UI recolors to the current cover, cross-fading on every track change
- 🌊 **Live FFT visualizer** — a smooth, color-graded spectrum driven by the actual audio
- 🎤 **Time-synced lyrics** — karaoke scroll via [lrclib](https://lrclib.net)
- 📚 **Full library** — Home feed, Recently Played, Playlists, Liked Songs, Albums, Artists
- 🔍 **Search** the whole catalog — songs, artists, albums, playlists
- 📻 **Song radio** — start a station from any track (via librespot's internal protocol)
- 🎯 **Drill-in navigation** — open an artist → popular tracks + albums; open albums/playlists
- ⚡ **Context actions** (`a`) — like, add to queue/playlist, follow, go to artist/album, copy link
- 🔀 Shuffle, repeat, volume, and a live queue view
- 💾 **Session resume** — reopens on your last track, at your position, in the same context

## Install

```bash
cargo install --path .    # or: cargo build --release
```

You'll need your own Spotify app client ID — free, and takes a minute:

1. Go to the [Spotify developer dashboard](https://developer.spotify.com/dashboard) → **Create app**
2. Add the redirect URI `http://127.0.0.1:8989/login`
3. Copy the **Client ID**, then either:
   ```bash
   export MYX_CLIENT_ID=<your-client-id>
   # or:
   mkdir -p ~/.config/myx && echo -n "<your-client-id>" > ~/.config/myx/client_id
   ```

No secret is needed (myx uses OAuth PKCE). Requires **Spotify Premium**.

## Keybinds

```
⇥ / [ ]      switch library section        ← →        switch view (Now Playing / Lyrics / Queue)
↑↓ / j k     move selection                ⏎          play / open
/            search                        a          actions menu
space / p    play · pause                  n / b      next · prev
+ / -        volume                        s          shuffle
Esc          back                          q          quit
```

## Credits

Built on the shoulders of open source — see [NOTICE](NOTICE). In short: the streaming
engine adapts pieces of [spotify-player](https://github.com/aome510/spotify-player)
(MIT, © Thang Pham), the visual language reinterprets
[noodle](https://github.com/wilfredinni/noodle), and it all rides on
[librespot](https://github.com/librespot-org/librespot).

## License

MIT — see [LICENSE](LICENSE).
