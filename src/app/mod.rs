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
    // The MXC colour publisher, when one could be bound. `None` means
    // publishing is disabled (`MYX_NO_COLOR_SOCKET`) or the bind failed — both
    // are ordinary states, not errors: a player that refuses to play music
    // because a socket is unavailable would be a worse player. Every use site
    // is a `if let Some(..)`, so `None` is simply inert.
    #[cfg(all(feature = "mxc", unix))]
    pub(crate) mxc: Option<myx::mxc::publish::Publisher>,
    pub(crate) status: String,
    pub(crate) browse: BrowseState,
    pub(crate) transport: Transport,
    pub(crate) search: SearchState,
    pub(crate) find: FindState,
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
    /// The views ←/→ step through right now.
    pub(crate) fn rotation(&self) -> &'static [RightView] {
        if self.view.layout == LayoutMode::Focus && !self.view.zen {
            &RightView::WITH_LIBRARY
        } else {
            &RightView::VIEWS
        }
    }

    /// Whether the library is on screen: its sidebar in Full, or its view in
    /// Focus — never under zen. Keys and hints for it only count while it is.
    pub(crate) fn library_visible(&self) -> bool {
        !self.view.zen
            && match self.view.layout {
                LayoutMode::Full => true,
                LayoutMode::Focus => self.view.mode == RightView::Library,
            }
    }

    /// Keep the current view one the rotation has, after the layout or zen
    /// changed: the Library view only exists in Focus without zen, and from
    /// it you land on Now Playing.
    pub(crate) fn settle_view(&mut self) {
        if !self.rotation().contains(&self.view.mode) {
            self.view.mode = RightView::NowPlaying;
        }
    }

    /// Which list `cur_items` is showing.
    pub(crate) fn list_key(&self) -> ListKey {
        if let Some(d) = self.browse.details.last() {
            ListKey::Page(self.browse.details.len(), d.context_uri.clone())
        } else if self.search.searching {
            ListKey::Search
        } else {
            ListKey::Section(self.browse.section)
        }
    }
    /// The `f` query narrowing the list on screen, if any.
    pub(crate) fn find_query(&self) -> Option<&str> {
        self.find.query_for(&self.list_key())
    }
    /// The rows of the list on screen to draw, as indices into `cur_items`.
    pub(crate) fn shown_rows(&self) -> Vec<usize> {
        shown_rows(self.cur_items(), self.find_query())
    }
    /// Whether row `i` can take the cursor (not a header, and found).
    pub(crate) fn selectable(&self, i: usize) -> bool {
        row_selectable(self.cur_items(), i, self.find_query())
    }
    /// The row under the cursor, if the cursor is on one that can be picked —
    /// never a header, and never a row the find query is hiding.
    pub(crate) fn selected_item(&self) -> Option<&LibItem> {
        let i = self.browse.selected;
        self.selectable(i).then(|| &self.cur_items()[i])
    }
    /// First selectable index (where a fresh selection should land).
    pub(crate) fn first_selectable(&self) -> usize {
        (0..self.cur_items().len())
            .find(|&i| self.selectable(i))
            .unwrap_or(0)
    }
    /// Move the selection by `dir`, skipping headers and rows find is hiding,
    /// clamped at the ends.
    pub(crate) fn move_sel(&mut self, dir: isize) {
        let n = self.cur_items().len() as isize;
        let mut i = self.browse.selected as isize;
        loop {
            i += dir;
            if i < 0 || i >= n {
                return;
            }
            if self.selectable(i as usize) {
                self.browse.selected = i as usize;
                return;
            }
        }
    }
    /// If the selection landed on a header or a hidden row (data loaded, a
    /// find narrowed the list), bump it to the first row that can be picked.
    pub(crate) fn normalize_selection(&mut self) {
        let i = self.browse.selected;
        if i < self.cur_items().len() && !self.selectable(i) {
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
        if let Err(e) = self.svc.engine.play_context(uri, shuffle) {
            self.status = format!("couldn't play: {e:#}");
        }
    }

    /// Play whatever's selected (in the current section, or in search results).
    /// Act on the selected item. Returns what the caller should do next.
    pub(crate) fn activate(&mut self) -> Activated {
        let Some(item) = self.selected_item().cloned() else {
            return Activated::None;
        };
        if item.is_header() {
            return Activated::None;
        }
        if item.is_play() {
            // Special synthetic rows: play the Liked list (optionally shuffled).
            if item.uri == "myx:action:liked-play" {
                let uris: Vec<String> = self
                    .browse
                    .library
                    .liked
                    .iter()
                    .filter(|i| i.is_track())
                    .map(|i| i.uri.clone())
                    .collect();
                if !uris.is_empty() {
                    self.transport.source = PlaySource::Liked;
                    self.transport.source_name = "Liked Songs".to_string();
                    self.status = "starting Liked Songs…".to_string();
                    // Honour the current shuffle toggle instead of a dedicated row.
                    if let Err(e) =
                        self.svc
                            .engine
                            .play_tracks(uris, None, 0, self.transport.shuffle)
                    {
                        self.status = format!("couldn't play: {e:#}");
                    }
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
        if item.is_track() {
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
                if let Err(e) = self.svc.engine.play_context_at(
                    ctx,
                    Some(item.uri.clone()),
                    0,
                    self.transport.shuffle,
                ) {
                    self.status = format!("couldn't play: {e:#}");
                }
                return Activated::None;
            }
            // Section track list.
            let uris = self
                .cur_items()
                .iter()
                .filter(|i| i.is_track())
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
            if let Err(e) =
                self.svc
                    .engine
                    .play_tracks(uris, Some(item.uri.clone()), 0, self.transport.shuffle)
            {
                self.status = format!("couldn't play: {e:#}");
            }
            return Activated::None;
        }
        // Otherwise it's a context (artist / album / playlist) — open it.
        self.status = format!("opening {}…", item.name);
        Activated::Open(item.uri, item.name)
    }
}
