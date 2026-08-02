//! The application state — the thing the other three layers are about.
//!
//! `ui/` reads `&App` and writes `FrameOut`; `input/` mutates `App`; `api/`
//! touches neither and talks HTTP over channels. This module is the state in
//! the middle. One module per part of the model, so the file to open is the one
//! named after it; `App` itself lives here, since every one of those parts
//! hangs off it.
//!
//! It would be tidier if this module depended on none of the others, and it
//! nearly does — with two exceptions, both in `event.rs`, where handling an
//! engine event spawns a fetch directly (`fetch_track_meta`, and the lyrics
//! fetch). Those reach into `api/`. The intended shape is for `event.rs` to
//! send a request over a channel and let `main.rs` — the wiring layer, which is
//! allowed to know both sides — service it. Until that lands, this is a real
//! edge in the graph, not an aspiration, so don't add more of them.

mod action;
mod event;
mod frame;
mod library;
mod persist;
mod playback;
mod state;

pub(crate) use action::*;
pub(crate) use event::*;
pub(crate) use frame::*;
pub(crate) use library::*;
pub(crate) use persist::*;
pub(crate) use playback::*;
pub(crate) use state::*;

use crate::*;

pub(crate) struct App {
    pub(crate) svc: Services,
    pub(crate) theme: ThemeState,
    pub(crate) playback: PlaybackState,
    // Best-effort OS integration. Headless/SSH sessions may not expose the
    // platform media service, but that must never prevent Myx from playing.
    pub(crate) media_controls: Option<MediaControls>,
    pub(crate) status: String,
    pub(crate) browse: BrowseState,
    pub(crate) transport: Transport,
    pub(crate) search: SearchState,
    pub(crate) view: ViewState,
    pub(crate) session: SessionState,
    // What the album art box owes the next frame. See ArtRepaint.
    pub(crate) art_repaint: ArtRepaint,
}

impl App {
    pub(crate) fn cur_items(&self) -> &[LibItem] {
        if let Some(d) = self.browse.details.last() {
            &d.items
        } else if self.search.searching {
            &self.search.search_results
        } else {
            self.browse.library.items(self.browse.section)
        }
    }
    pub(crate) fn cur_list_mut(&mut self) -> &mut Vec<LibItem> {
        if let Some(d) = self.browse.details.last_mut() {
            &mut d.items
        } else if self.search.searching {
            &mut self.search.search_results
        } else {
            self.browse.library.items_mut(self.browse.section)
        }
    }
    /// First non-header index (where a fresh selection should land).
    pub(crate) fn first_selectable(&self) -> usize {
        self.cur_items()
            .iter()
            .position(|i| !i.is_header)
            .unwrap_or(0)
    }
    /// Move the selection by `dir`, skipping header rows, clamped at the ends.
    pub(crate) fn move_sel(&mut self, dir: isize) {
        let items = self.cur_items();
        let n = items.len() as isize;
        if n == 0 {
            return;
        }
        let mut i = self.browse.selected as isize;
        loop {
            i += dir;
            if i < 0 || i >= n {
                return;
            }
            if !items[i as usize].is_header {
                self.browse.selected = i as usize;
                return;
            }
        }
    }
    /// If the selection landed on a header (e.g. after data loads), bump it off.
    pub(crate) fn normalize_selection(&mut self) {
        if self
            .cur_items()
            .get(self.browse.selected)
            .is_some_and(|i| i.is_header)
        {
            self.browse.selected = self.first_selectable();
        }
    }
    /// The single entry point for "play this context URI".
    ///
    /// Every caller must route through here so `source` / `source_name` stay in
    /// sync with what is actually playing — they back the Queue view's
    /// PLAYING FROM header and the resume-on-launch path in `resume_source`.
    /// `name` is a parameter rather than being derived from `details.last()`
    /// because the drill-in stack is empty when playing straight from a list.
    pub(crate) fn play_context_row(&mut self, uri: String, name: String, shuffle: bool) {
        self.status = format!("starting {name}…");
        self.transport.source = PlaySource::Context(uri.clone());
        self.transport.source_name = name;
        self.engine_play(|e| e.play_context(uri, shuffle));
    }

    /// Live FFT bands from whichever backend is producing audio.
    ///
    /// The external source writes into the same [`VisualizationSink`] the engine
    /// uses, so the visualizer needs no knowledge of which one is running.
    pub(crate) fn bands(&self) -> Option<&Arc<Mutex<VisBands>>> {
        if let Some(engine) = &self.svc.engine {
            return Some(&engine.bands);
        }
        self.svc.source.as_ref().map(|s| s.bands())
    }

    /// Toggle play/pause on whichever backend is live.
    ///
    /// Returns false when there is nothing to toggle, so the caller can explain
    /// itself rather than appearing to do nothing.
    pub(crate) fn transport_toggle(&mut self) -> bool {
        if let Some(engine) = &self.svc.engine {
            let _ = engine.toggle();
            return true;
        }
        if let Some(source) = &self.svc.source {
            source.toggle();
            let playing = source.is_playing();
            if let Some(now) = self.playback.now.as_mut() {
                now.is_playing = playing;
            }
            return true;
        }
        false
    }

    /// Resume whichever backend is live.
    pub(crate) fn transport_play(&mut self) -> bool {
        if let Some(engine) = &self.svc.engine {
            let _ = engine.play();
            return true;
        }
        if let Some(source) = &self.svc.source {
            source.play();
            if let Some(now) = self.playback.now.as_mut() {
                now.is_playing = true;
            }
            return true;
        }
        false
    }

    /// Stop whichever backend is live.
    pub(crate) fn transport_stop(&mut self) {
        if let Some(engine) = &self.svc.engine {
            engine.stop();
            return;
        }
        if let Some(source) = &self.svc.source {
            source.stop();
            if let Some(now) = self.playback.now.as_mut() {
                now.is_playing = false;
            }
        }
    }

    /// Mirror the external source's state into `playback.now`.
    ///
    /// The librespot path gets its now-playing from Spotify metadata; a local file
    /// has none, so the transport bar, progress and position would all stay empty.
    /// This synthesises the same shape from what the helper reported.
    pub(crate) fn sync_external_now(&mut self) {
        let Some(source) = self.svc.source.clone() else {
            return;
        };
        let state = source.state();
        if let Some(error) = state.error.as_deref() {
            if self.status != error {
                self.status = error.to_string();
            }
        }
        let Some(uri) = state.uri.clone() else {
            return;
        };
        let duration_ms = state.duration_ms.unwrap_or(0).min(u32::MAX as u64) as u32;

        match self.playback.now.as_mut() {
            Some(now) if now.uri == uri => {
                now.is_playing = state.playing;
                now.duration_ms = duration_ms;
                now.position_ms = state.position_ms;
                now.position_at = Instant::now();
            }
            _ => {
                // Name it after the file; there is no catalogue to ask.
                let title = uri
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(uri.as_str())
                    .to_string();
                let codec = state
                    .meta
                    .as_ref()
                    .map(|m| m.codec.clone())
                    .unwrap_or_default();
                self.playback.now = Some(NowPlaying {
                    uri,
                    title,
                    artist: source.program().to_string(),
                    album: codec,
                    duration_ms,
                    position_ms: state.position_ms,
                    position_at: Instant::now(),
                    is_playing: state.playing,
                    cover: None,
                });
                self.transport.playback_started = true;
            }
        }
    }

    /// A seek callback for [`PlaybackState::seek_to`] that hits whichever backend
    /// is live.
    ///
    /// For the external source this respawns the helper — it is stateless per
    /// invocation, so a seek *is* a new invocation.
    pub(crate) fn seek_sink(&self) -> impl FnMut(u32) + use<> {
        let engine = self.svc.engine.clone();
        let source = self.svc.source.clone();
        move |position_ms: u32| {
            if let Some(engine) = &engine {
                let _ = engine.seek(position_ms);
            } else if let Some(source) = &source {
                source.seek(position_ms);
            }
        }
    }

    /// Run a fire-and-forget engine command, if there is an engine.
    ///
    /// A `--guest` build has none, and every such call is a deliberate no-op.
    pub(crate) fn engine_do(&self, f: impl FnOnce(&Engine)) {
        if let Some(engine) = &self.svc.engine {
            f(engine);
        }
    }

    /// Run an engine play command, surfacing whatever went wrong — or, with no
    /// engine, saying so instead of silently doing nothing.
    pub(crate) fn engine_play(&mut self, f: impl FnOnce(&Engine) -> anyhow::Result<()>) {
        let Some(engine) = self.svc.engine.clone() else {
            // An external source is a different domain, not a fallback: it plays
            // whatever its helper can resolve, which is not a Spotify context.
            self.status = if self.svc.source.is_some() {
                "external source can't play Spotify tracks — streaming needs Premium".to_string()
            } else {
                "guest mode: playback needs Spotify Premium".to_string()
            };
            return;
        };
        if let Err(e) = f(&engine) {
            self.status = format!("couldn't play: {e:#}");
        }
    }

    /// Set the volume on whichever transport is actually producing sound.
    ///
    /// Exclusive on purpose: driving both would move the playhead-less engine's
    /// Connect device around for no reason, and a guest's engine is stopped.
    pub(crate) fn apply_volume(&mut self, volume: u8) {
        self.transport.volume = volume.min(100);
        let vol = self.transport.volume;
        self.engine_do(|e| {
            let _ = e.set_volume(vol_u16(vol));
        });
        if let Some(source) = &self.svc.source {
            source.set_volume(vol_u16(vol));
        }
    }

    /// Nudge the volume by `delta`, clamped to 0..=100.
    pub(crate) fn bump_volume(&mut self, delta: i16) {
        let next = (self.transport.volume as i16 + delta).clamp(0, 100) as u8;
        self.apply_volume(next);
    }

    /// Play whatever's selected (in the current section, or in search results).
    /// Act on the selected item. Returns what the caller should do next.
    pub(crate) fn activate(&mut self) -> Activated {
        let Some(item) = self.cur_items().get(self.browse.selected).cloned() else {
            return Activated::None;
        };
        if item.is_header {
            return Activated::None;
        }
        if item.is_play {
            // Special synthetic rows: play the Liked list (optionally shuffled).
            if item.uri == "myx:action:liked-play" {
                let uris: Vec<String> = self
                    .browse
                    .library
                    .liked
                    .iter()
                    .filter(|i| i.is_track)
                    .map(|i| i.uri.clone())
                    .collect();
                if !uris.is_empty() {
                    self.transport.source = PlaySource::Liked;
                    self.transport.source_name = "Liked Songs".to_string();
                    self.status = "starting Liked Songs…".to_string();
                    // Honour the current shuffle toggle instead of a dedicated row.
                    let shuffle = self.transport.shuffle;
                    self.engine_play(|e| e.play_tracks(uris, None, 0, shuffle));
                }
                return Activated::None;
            }
            // Inside a drill-in the enclosing title is the better label
            // ("Chill Vibes"); standalone play rows fall back to their own.
            let name = self
                .browse
                .details
                .last()
                .map(|d| d.title.clone())
                .unwrap_or_else(|| item.name.clone());
            let shuffle = self.transport.shuffle;
            self.play_context_row(item.uri, name, shuffle);
            return Activated::None;
        }
        if item.is_track {
            if self.search.searching {
                // A search-result song starts that song's radio (seed + similar).
                self.transport.source = PlaySource::Radio(item.uri.clone());
                self.transport.source_name = format!("Radio · {}", item.name);
                return Activated::Radio(item.uri);
            }
            // Inside a drill-in → play its context at this track (real queue).
            if let Some(d) = self.browse.details.last() {
                let ctx = d.context_uri.clone();
                self.transport.source = PlaySource::Context(ctx.clone());
                self.transport.source_name = d.title.clone();
                self.status = format!("starting {}…", item.name);
                let shuffle = self.transport.shuffle;
                let at = Some(item.uri.clone());
                self.engine_play(|e| e.play_context_at(ctx, at, 0, shuffle));
                return Activated::None;
            }
            // Section track list.
            let uris = self
                .cur_items()
                .iter()
                .filter(|i| i.is_track)
                .map(|i| i.uri.clone())
                .collect();
            self.status = format!("starting {}…", item.name);
            if self.browse.section == Section::Liked {
                self.transport.source = PlaySource::Liked;
                self.transport.source_name = "Liked Songs".to_string();
            } else {
                self.transport.source = PlaySource::None;
                self.transport.source_name = self.browse.section.label().to_string();
            }
            let shuffle = self.transport.shuffle;
            let at = Some(item.uri.clone());
            self.engine_play(|e| e.play_tracks(uris, at, 0, shuffle));
            return Activated::None;
        }
        // Otherwise it's a context (artist / album / playlist) — open it.
        self.status = format!("opening {}…", item.name);
        Activated::Open(item.uri, item.name)
    }
}
