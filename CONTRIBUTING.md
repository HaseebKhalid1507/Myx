# Contributing to Myx

Hey — thanks for being here. Myx is a terminal Spotify player: librespot for
streaming, ratatui for the surface, and album art that repaints the whole UI on
every track change.

The codebase used to be one 5000-line `main.rs`. It isn't anymore. This document
is the map that refactor was for — read the **Architecture map** and the **App
state model**, and you'll know where your change goes.

---

## Quick start

```bash
git clone https://github.com/HaseebKhalid1507/Myx
cd Myx
cargo run
```

You need a free Spotify app client ID before it'll do anything (one minute, no
secret required) — see **Get started** in the [README](README.md). Set
`MYX_CLIENT_ID` or drop it in `~/.config/myx/config.toml`.

**Native deps.** On Linux you need ALSA and OpenSSL headers plus `pkg-config`:

```bash
# Debian / Ubuntu
sudo apt-get install libasound2-dev pkg-config libssl-dev
```

macOS ships what you need. Windows builds via `cargo` with no extra steps.

**Or use Nix** and skip all of that:

```bash
nix develop     # dev shell: cargo, clippy, rustfmt, rust-analyzer
nix build       # build the package
nix flake check
```

**Debugging.** Set `MYX_LOG` (any value turns logging on; `debug`/`trace` open
librespot up, `warn` quiets it) and tail `~/.cache/myx/myx.log`. The TUI takes
over the alternate screen, so stderr is invisible — this is your printf.
`MYX_PROTOCOL` forces the album-art graphics protocol if auto-detection picks
wrong.

---

## Architecture map

One line per module. The rule of thumb: **if it's on screen, it's in `src/ui/`.
If it's state or input, it's in `src/main.rs`. Everything else is a library
module in `src/lib.rs`.**

`main.rs` is the binary; everything else is the `myx` library crate, which means
library modules are unit-testable without a terminal, a network, or a sound card.

### The engine (streaming feature)

| Path | What lives here |
| --- | --- |
| `engine/mod.rs` | The librespot engine. Brings up a Spotify Connect device (Spirc) with our tee'd audio sink, and bridges librespot's player events into a clean `EngineEvent` stream. `TrackChanged` landing on that channel is what makes track changes *real* — it's the hook the theme fade and cover reload fire from. Also `radio_tracks` (seeded autoplay). |
| `engine/auth.rs` | OAuth 2.0 PKCE against librespot's public desktop client ID, with a tiny localhost callback server. Produces librespot `Credentials`. Adapted from spotify-player (MIT). |
| `audio/visualizer.rs` | The FFT visualizer sink. It's a **tee**: every audio packet is forwarded unchanged to the real backend (playback is never affected) while a windowed FFT writes frequency bands into a plain `Arc<Mutex<VisBands>>`. |
| `webapi.rs` | The Spotify **Web API** side: a *separate* OAuth PKCE flow using your own app's client ID, so metadata and library calls get their own rate-limit bucket instead of fighting librespot's saturated shared one. Token cached to `~/.cache/myx/webapi.json` and auto-refreshed. |
| `httpcache.rs` | On-disk cache for catalogue reads (`~/.cache/myx/api`). Spotify's dev quota runs out fast; stale entries are served when a request fails, because yesterday's album list beats an empty page. |

### The look

| Path | What lives here |
| --- | --- |
| `theme.rs` | The design-token system: three background layers, semantic color roles, four border shades. Every widget reads a `Theme`, never a raw color. |
| `color.rs` | Color science — RGB↔HSL, clamping for dark backgrounds, hue distance, tint synthesis. Pure math. |
| `reactive.rs` | **The signature move.** Extracts a palette from the cover with `color-thief` and derives a whole semantic `Theme` from it — dominant swatch tints the backgrounds, most vibrant becomes `primary`, most hue-distant becomes the accent. |
| `gradient.rs` | Gradient/pill color math. Pure. |
| `anim.rs` | Time-based animation, currently the theme cross-fade. Wall-clock driven, so a fade lasts the same real duration at 60fps or 10fps. |
| `components.rs` | Reusable render primitives in the visual language: `pill`, `gradient_pill`, `gradient_line`, `left_bar_block`. |
| `cover.rs` | Album art via `ratatui-image`. Detects kitty / sixel / iTerm2 at startup, falls back to half-blocks. Caches the encoded protocol per render area. |

### The surface

`src/ui/` is the render tree. **One module per thing on screen** — the file to
open is the one named after what you're looking at.

| Path | What lives here |
| --- | --- |
| `ui/mod.rs` | The top-level `render()`: overall layout, header + wordmark, view tabs, dispatch to the right pane. Also `scroll_offset`, the sticky-viewport math. |
| `ui/library.rs` | Left sidebar: sections, search results, drill-in lists. |
| `ui/nowplaying.rs` | The Now Playing view *and* the persistent bottom strip (volume, progress). |
| `ui/lyrics.rs` / `ui/queue.rs` / `ui/visualizer.rs` | The other views and the spectrum bars under the art. |
| `ui/overlay.rs` | Things drawn on top: the actions menu, the startup loading screen. |
| `ui/footer.rs` | The one-line keybinding hint. |

### The rest

| Path | What lives here |
| --- | --- |
| `main.rs` | `App` state, the async event loop (`run_ui`), all input handling (`handle_key`, `handle_mouse`, media keys), and the Web API worker functions that fetch library/search/detail/lyrics off-thread. |
| `lyrics/parse.rs` | LRC parsing. Pure, no I/O. (Network fetching still lives in `main.rs` — moving it out is a welcome PR.) |
| `util.rs` | Small pure helpers shared by UI and workers — `truncate`, `fmt_ms`, `center_v`, URI munging. Dependency-light on purpose so it unit-tests trivially. |
| `config.rs` | `~/.config/myx/config.toml`. Missing, empty or malformed all fall back to defaults — a typo must never lock someone out. |
| `term.rs` | Terminal setup/teardown and the single-instance lock. |
| `liblog.rs` | The `log` bridge for librespot plus the `MYX_LOG` debug file. |

---

## The App state model

`App` is deliberately small — around eleven fields, and most of them are grouped
sub-structs. **Before adding a field to `App`, find the group it belongs to.**

| Field | Owns |
| --- | --- |
| `svc: Services` | Long-lived services: `engine`, `picker`, `webapi`. All used through `&self`. |
| `theme: ThemeState` | `displayed` (what every widget reads), `target`, and the in-flight cross-fade. |
| `playback: PlaybackState` | The playhead: `now`, plus the coalesced Shift+arrow scrub state. Together because every scrub method touches both. |
| `transport: Transport` | Controls and queue: shuffle, repeat, volume, queue, play source. Nothing here touches the playhead. |
| `browse: BrowseState` | The library browser: loaded items, section, cursor, sort, drill-in stack. **The viewport offset is *not* here** — see below. |
| `search: SearchState` | The `/` overlay: input mode, query, results. |
| `view: ViewState` | What you're looking at: right-pane `mode`, `zen`, lyrics, actions overlay. |
| `session: SessionState` | Cross-cutting bookkeeping: restore URI, in-flight metadata fetch, double-click and Ctrl-C timestamps. |

Plus three ungrouped ones that genuinely belong nowhere else: `media_controls`
(best-effort OS media integration — may be `None` on headless/SSH, and that must
never stop playback), `status`, and `art_repaint`.

**Where do methods go?** On the sub-struct, if they only touch that struct —
`PlaybackState::position_ms` is a good example. A handful stay on `App` on
purpose because they *straddle* groups and can't be pushed down:

- `cur_items` / `cur_list_mut` — reads `browse.details`, then `search.searching`,
  then `browse.library`. It resolves "what list is on screen right now" across
  three groups.
- `activate` — the ⏎ handler; touches browse, transport and view.
- `move_sel`, `first_selectable`, `normalize_selection` — selection logic that
  reads `cur_items()`.
- `play_context_row`.

That's the whole list. If your new method fits inside one group, put it there.

---

## Rendering rules

This is the one invariant we care most about:

> **Render functions take `&App` and write only to `FrameOut`.**

`src/ui/` has a one-way dependency on state. Nothing in the render tree mutates
the app. That's what keeps "why did the UI change?" answerable by reading the
input handlers alone, and it's why every render function can borrow `&App`
without fighting the borrow checker.

`FrameOut` is the render tree's only output channel, and it holds two kinds of
thing:

- **`hits: HitRects`** — mouse hit-test rectangles (progress bar, scrollbar,
  volume meter, view tabs, library viewport). Pure output: every field is reset
  or cleared on the frame that draws the thing it belongs to. Written by the
  renderer, read only by `handle_mouse`.
- **`lib_offset`** — the library viewport's start row. Unlike `hits` this is
  read-modify-write: last frame's value feeds `scroll_offset`, and the result is
  stored back. That round trip is exactly what makes scrolling *sticky* — the
  cursor moves freely inside the window, which follows only when pushed. It's
  owned by `run_ui` so it survives across frames, which is also why it lives in
  `FrameOut` and not in `BrowseState`.

**Please don't reach for `&mut App` in the render path.** If a render function
seems to need mutable state, that's a signal the state belongs in `FrameOut`, or
that the mutation belongs in the event loop before the draw call. Ask in the PR
— we'd rather talk it through than lose the separation.

---

## Testing

```bash
cargo test --all-features
```

Tests live in three places, and which one you use depends on visibility:

| Where | For |
| --- | --- |
| `tests/` | Integration tests against the public `myx` library API (`tests/util.rs`, `tests/lyrics.rs`). |
| `src/main_tests/` | The binary's unit tests — kept in-crate because they exercise items private to `main.rs` (`nav.rs`, `playlist.rs`, `live.rs`). |
| Inline `#[cfg(test)]` | Module-local tests next to the code (`anim`, `color`, `config`, `cover`, `engine`, `gradient`, `httpcache`, `reactive`). |

### Characterization-test style

Most tests here are **characterization tests**: they lock in what the code does
*today*, quirks included. If one fails, someone changed behavior — deliberately
or not, and the test's job is to make you notice.

Two habits that come with it:

1. **Name the quirk in the test name and comment it.** Don't quietly assert a
   weird value; say it's weird. `assert_eq!(scroll_offset(0, 9, 10, 10, 3), 0,
   "total == cap still fits")` tells the next person why.
2. **When you fix a quirk, flip the test to lock the fix** rather than deleting
   it, and say so in the comment.

### Live tests

`src/main_tests/live.rs` talks to the real Spotify API and is `#[ignore]`d, so
`cargo test` stays offline and CI stays green without an account. Run them
yourself when you touch an endpoint:

```bash
cargo test --bin myx -- --ignored --nocapture
```

They read the cached token from `~/.cache/myx/webapi.json` — run `myx` once
first. They exist because Spotify changed an endpoint out from under the artist
page and nobody noticed.

---

## Pull requests

**CI must be green.** It runs on Linux and macOS, plus a Nix build, with
`RUSTFLAGS: -D warnings`. Three gates:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features
cargo test --all-features
```

Run those three locally before you push and you'll almost never be surprised.

A few asks, none of them exotic:

- **Keep commits focused.** One idea per commit, with a message that says why.
  Look at `git log` — that's the register we're going for.
- **No unrelated reformatting.** A diff that touches thirty files because your
  editor re-wrapped comments is a diff nobody can review. `cargo fmt` and
  nothing more.
- **Comments explain *why*, not *what*.** This codebase is full of them and it's
  the reason it's readable — the `HitRects` doc comment, the `ArtRepaint`
  explanation, the note on why `spawn_restore` clones its sender. Match that.
- **Feature-gate correctly.** `streaming` is the default feature and pulls in
  librespot, tokio, reqwest and friends. Library modules that need them are
  `#[cfg(feature = "streaming")]` in `src/lib.rs` — keep new pure modules
  outside the gate so they build and test without the network stack.
- **Touching playback, auth, or the engine?** Say how you tested it. Those paths
  don't have full automated coverage and we're honest about that.
- **Not sure?** Open an issue or a draft PR early. Talking about a design before
  it's written is cheaper for everyone than a rewrite after.

That's it. Go make it better. 🎧
