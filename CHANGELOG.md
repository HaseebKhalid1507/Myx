# Changelog

Newest first. Format follows [Keep a Changelog](https://keepachangelog.com);
versions follow [semver](https://semver.org). Released sections are a record —
they are added to, never rewritten.

## [Unreleased]

### Added

- `~/.config/myx/config.toml` is written on first run with every key commented
  out, so there is a file to edit instead of a path to guess.
- `protocol` config key (`kitty`, `iterm2`, `sixel`, `halfblocks`) for when the
  startup detection picks wrong. `MYX_PROTOCOL` still overrides it.
- On-disk cache for catalogue reads and album art in `~/.cache/myx/api`. Repeat
  visits skip the network, and a stale entry is served when a request fails —
  which is what a spent API quota looks like. Entries older than 30 days are
  swept once per run.

### Changed

- Redraws are driven by changes rather than a fixed rate: input redraws within
  one terminal refresh, animation runs at 30fps, an untouched screen at 2fps
  instead of 60. A held arrow key now scrolls smoothly.
- Queue refresh and session persistence run on a timer instead of a frame
  counter — at 60fps they were firing every four seconds.
- The visualizer's frame rate only applies while Now Playing is on screen, and
  synced lyrics animate at the same rate so the highlighted line stays on time.
- Frames are presented atomically (synchronized output, DECSET 2026). A track
  change recolours every glyph at once, and the terminal used to render that
  half-applied.
- The theme cross-fade runs 1200ms instead of 300ms. Smoothness comes from the
  duration, not the frame rate: every present recomposes the viewport, and the
  inline cover shimmers if that happens 60 times a second.
- Zen mode ignores the keys that only drive the hidden library — `Tab`, `↑`/`↓`,
  `Enter`, `/`, `o`, `r`, `P`, `S`, `Esc` — instead of moving a selection nobody
  can see, and the footer drops their hints. `a` stays, retargeted onto the
  playing track rather than the invisible selection.

### Fixed

- A black-and-white cover no longer tints the whole UI burgundy. `rgb_to_hsl`
  reports hue 0 for every grey, and hue 0 is red, so an achromatic dominant
  swatch painted every surface with it. The base hue now comes from the most
  saturated swatch in the palette, and the tint's strength scales with how
  colourful the art actually is — greyscale art gets a neutral UI.
- Album art is transmitted only when it changes. The escape was written into its
  cell on every frame, so a recolour that repaints the screen dozens of times in
  a row made the cover flicker; other frames now just hold the cells.
- Overlays draw over the cover instead of under it, and closing one no longer
  leaves half a popup stencilled across the art. `ratatui-image` marks the image
  cells `Skip` without changing their symbols, and a blank cell compares equal to
  the blank already there — so `Clear` over an image wrote nothing at all.
  Wiping and overdrawing now use `CellDiffOption::AlwaysUpdate`.
- Switching back to Now Playing no longer drags the previous view's text across
  the cover.
- myx reconnects itself after an idle spell instead of needing a restart.
  librespot invalidates its session when the access point stops answering the
  keep-alive and leaves recovery to the caller, so every command afterwards
  failed with `Internal error { channel closed }`. A watchdog now rebuilds the
  session, player and Connect device — usually before a key is pressed — and the
  status line says so. Whatever was playing is not resumed: the replacement
  device starts idle.
- Album art no longer disappears after switching tmux windows. Where tmux
  reports sixel support it is drawn as sixel, unwrapped, so tmux stores the
  image itself and repaints it; kitty and iTerm2 images pass through untracked
  and are lost on the next repaint.
- The blind two-second art resend under tmux is gone. It re-encoded the cover to
  produce an identical cell the diff discarded, so it never recovered anything;
  `focus-events on` (see the README) and sixel do.
- `protocol` and `MYX_PROTOCOL` are honoured inside a sixel-capable tmux. The
  sixel picker deliberately drops tmux passthrough, so a forced kitty or iTerm2
  used to leave its escapes for tmux to eat, and no image appeared at all.
- kitty is detected from the client tmux has attached *now* rather than from
  `KITTY_WINDOW_ID`, which lingers in a session's environment and made a session
  reattached from another terminal ask for kitty images it couldn't draw.
- The graphics query runs before the tokio runtime and the player exist. Picking
  sixel swaps `TERM` around it, and `setenv` is only safe without concurrent
  readers.
- Cover requests that fail are no longer cached as image bytes, which would have
  meant a permanently broken cover — those entries never expire.
- Cache writes go through a temporary file and a rename, so an interrupted write
  can't leave a truncated entry behind. The temp name is unique per write, so two
  threads fetching the same URL can't interleave into one corrupt entry.

## [0.3.0] — 2026-07-28

### Added

- Native media controls (macOS, Windows, Linux) via souvlaki, with a winit event
  loop on macOS.
- CI: fmt, clippy and tests on push and pull request, once per change.

### Fixed

- Migrated to the February 2026 Web API: liking a track, artist pages, adding to
  a playlist and the Home feed all called endpoints that had been removed.
- Web API recovery completes before the TUI starts, so a re-authorization prompt
  can't be hidden by the alternate screen.
- Hour-long `429` backoffs are no longer waited out; a drill-in fails fast
  instead of appearing to hang.
- Native controls lifecycle hardened.
- Seeking updated for the changed librespot API.

## [0.2.5] — 2026-07-26

### Added

- `P` / `S` play the highlighted playlist directly.
- Scroll wheel adjusts volume — local mixer immediately, Spotify in the
  background.

### Fixed

- Album art transitions keep the old cover until the new one loads, and a
  dropped image is retransmitted rather than leaving the previous track's art.
- Album art renders in Warp, which does not support kitty placeholders.
- Select loop no longer spins at 2.9M iterations/sec.
- Saved state restores when the last session was stopped.
- Graceful fallback when the terminal has no keyboard-enhancement support.
- Playlist track listing.

### Changed

- Enter is labelled "select", space shows play or pause depending on state.

## [0.2.3] — 2026-07-24

### Added

- Media keys.

### Fixed

- Full Windows support: cross-platform `home_dir()` in place of raw `HOME`
  lookups, unix permissions guarded by `#[cfg(unix)]`.
- Action failures surface the real error instead of "action failed".

## [0.2.0] — 2026-07-23

### Added

- UX overhaul and mouse support.

### Fixed

- Post-audit hardening.

## [0.1.3] — 2026-07-23

### Added

- Seek with shift+arrows or a click on the progress bar.
- Sort lists with `o` (added / title / artist).
- Queue persists track URIs, so resume continues past the first song.
- Homebrew, AUR, `.deb` and crates.io publishing; cargo-dist release pipeline
  with prebuilt binaries.

### Changed

- Adaptive framerate; tokio workers capped at 4.
- Library sections reordered (Home / Liked / Playlists / Albums / Artists /
  Recent), with Shuffle and Play rows on Liked.
- Fat-LTO release profile.

### Fixed

- Frozen UI, single-instance safety, resilient library loading.
- `vergen` pinned so a fresh `cargo install` resolves librespot-core.

### Security

- Bundled client id removed — `MYX_CLIENT_ID` or `~/.config/myx/client_id` is
  now required.

## [0.1.0] — 2026-07-22

First release: terminal Spotify player with reactive theming, an FFT
visualizer, synced lyrics, library, search, radio and context resume.
