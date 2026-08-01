//! myx — the fully-wired terminal Spotify player.
//!
//! librespot streaming engine + Web API (your own client id) + album-art-reactive
//! theming with cross-fades + live FFT visualizer, in noodle's visual language.
//! Multi-section library (playlists / liked / albums / artists), shuffle, repeat,
//! and a live queue view.

/// The Spotify Web API layer. Talks HTTP, hands plain data back over channels.
/// Lives in the binary (not the library) because it speaks the model types
/// defined here.
mod api;
/// The input layer. Turns terminal and media-key events into `App` mutations
/// and channel sends — the one layer that writes state.
/// Lives in the binary (not the library) because it mutates `App`, which is here.
mod input;
/// The render tree. Reads `App`, writes `FrameOut`; never the other way round.
/// Lives in the binary (not the library) because it needs `App`, which is here.
mod ui;

use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MediaKeyCode, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::buffer::CellDiffOption;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use ratatui_image::picker::Picker;

use api::*;
use input::*;
use myx::anim::ThemeFade;
use myx::audio::NUM_BANDS;
use myx::components::{gradient_line, gradient_progress, left_bar_block};
use myx::cover::Cover;
use myx::engine::{self, Engine, EngineEvent};
use myx::gradient::{self};
use myx::liblog::{install_librespot_log, liblog};
use myx::lyrics::parse::parse_lrc;
use myx::reactive::derive_theme;
use myx::term::{acquire_single_instance_lock, init_terminal, restore_terminal, Term};
use myx::theme::{Theme, TOKYONIGHT};
use myx::util::{center_v, fmt_ms, track_id_from_uri, truncate, uri_to_url, urlencode, vol_u16};
use myx::webapi::WebApi;
use ui::{render, render_loading};

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

const FADE_MS: u64 = 1500;

/// How far one Shift+arrow press moves the playhead.
const SEEK_STEP_MS: i64 = 5_000;
/// Fastest a held Shift+arrow may step. macOS repeats keys ~30×/s; unthrottled,
/// a one-second hold would throw the playhead 2½ minutes down the track.
const SEEK_REPEAT: Duration = Duration::from_millis(200);
/// Quiet time after the last press before the scrub reaches the engine.
const SEEK_SETTLE: Duration = Duration::from_millis(250);

fn scrub_target(from_ms: u32, duration_ms: u32, delta_ms: i64) -> u32 {
    (from_ms as i64 + delta_ms).clamp(0, duration_ms as i64) as u32
}

/// What the album art box owes the next frame.
///
/// `ratatui-image` puts the whole image in one cell's symbol and marks the rest
/// of the box `Skip`, which the diff never touches again — so leftovers stay,
/// and a re-encode is byte-identical for sixel and iTerm2 and gets discarded.
/// Blanking the box for one frame is the only change the diff will emit, and it
/// makes the image that follows one too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtRepaint {
    /// The box holds what it should.
    Idle,
    /// Blank the box this frame.
    Wipe,
    /// Draw the art; the wipe has gone out.
    Draw,
}

impl ArtRepaint {
    fn advance(self) -> Self {
        match self {
            Self::Wipe => Self::Draw,
            _ => Self::Idle,
        }
    }
}

/// Ceiling on the redraw rate: one frame per ~60Hz terminal refresh.
const MIN_FRAME: Duration = Duration::from_millis(16);
/// Redraw rate while the visualizer or a theme fade is running.
const ANIM_FRAME: Duration = Duration::from_millis(33);
/// Redraw rate when nothing changed — enough to keep the clock and progress bar
/// honest without repainting an identical frame 60 times a second.
const IDLE_REDRAW: Duration = Duration::from_millis(500);
/// How often the live queue is re-fetched and the session persisted.
const SYNC_EVERY: Duration = Duration::from_secs(24);

/// Whether this frame is worth drawing.
///
/// Input beats animation beats the idle clock. Smoothness of the recolour comes
/// from its duration, not from the frame rate: every present makes the terminal
/// recompose the viewport, and the inline cover shimmers if that happens 60
/// times a second.
fn should_draw(dirty: bool, animating: bool, since_last: Duration) -> bool {
    if dirty {
        since_last >= MIN_FRAME
    } else if animating {
        since_last >= ANIM_FRAME
    } else {
        since_last >= IDLE_REDRAW
    }
}

// ------------------------------------------------------------------ model

#[derive(Clone, Copy, PartialEq, Eq)]
enum RightView {
    NowPlaying,
    Lyrics,
    Queue,
}

impl RightView {
    const ALL: [RightView; 3] = [RightView::NowPlaying, RightView::Lyrics, RightView::Queue];
    fn label(self) -> &'static str {
        match self {
            RightView::NowPlaying => "Now Playing",
            RightView::Lyrics => "Lyrics",
            RightView::Queue => "Queue",
        }
    }
    fn shift(self, delta: isize) -> RightView {
        let i = RightView::ALL.iter().position(|&v| v == self).unwrap_or(0) as isize;
        let n = RightView::ALL.len() as isize;
        RightView::ALL[(i + delta).rem_euclid(n) as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Home,
    Recent,
    Playlists,
    Liked,
    Albums,
    Artists,
}

impl Section {
    const ALL: [Section; 6] = [
        Section::Home,
        Section::Liked,
        Section::Playlists,
        Section::Albums,
        Section::Artists,
        Section::Recent,
    ];
    fn label(self) -> &'static str {
        match self {
            Section::Home => "Home",
            Section::Recent => "Recent",
            Section::Playlists => "Playlists",
            Section::Liked => "Liked",
            Section::Albums => "Albums",
            Section::Artists => "Artists",
        }
    }
    fn index(self) -> usize {
        Section::ALL.iter().position(|&s| s == self).unwrap_or(0)
    }
    fn shift(self, delta: isize) -> Section {
        let n = Section::ALL.len() as isize;
        let i = (self.index() as isize + delta).rem_euclid(n) as usize;
        Section::ALL[i]
    }
}

/// A library entry. Behavior on Enter is driven by the flags:
/// header = non-selectable label; track = play as a track list; play = play this
/// URI as a context; otherwise = open (drill into) this context.
#[derive(Clone)]
struct LibItem {
    name: String,
    subtitle: String,
    uri: String,
    is_track: bool,
    is_header: bool,
    is_play: bool,
    order: u32, // original fetch position (for the "Added" sort)
}

impl LibItem {
    fn track(name: String, subtitle: String, uri: String) -> Self {
        Self {
            name,
            subtitle,
            uri,
            is_track: true,
            is_header: false,
            is_play: false,
            order: 0,
        }
    }
    fn ctx(name: String, subtitle: String, uri: String) -> Self {
        Self {
            name,
            subtitle,
            uri,
            is_track: false,
            is_header: false,
            is_play: false,
            order: 0,
        }
    }
    fn play(name: String, uri: String) -> Self {
        Self {
            name,
            subtitle: String::new(),
            uri,
            is_track: false,
            is_header: false,
            is_play: true,
            order: 0,
        }
    }
    fn header(name: &str) -> Self {
        Self {
            name: name.to_string(),
            subtitle: String::new(),
            uri: String::new(),
            is_track: false,
            is_header: true,
            is_play: false,
            order: 0,
        }
    }
}

/// Sort order for browsable lists.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortMode {
    Added,
    Title,
    Artist,
}

impl SortMode {
    fn label(self) -> &'static str {
        match self {
            SortMode::Added => "added",
            SortMode::Title => "title",
            SortMode::Artist => "artist",
        }
    }
    fn next(self) -> SortMode {
        match self {
            SortMode::Added => SortMode::Title,
            SortMode::Title => SortMode::Artist,
            SortMode::Artist => SortMode::Added,
        }
    }
}

/// Sort a list in place, keeping leading header/play rows pinned at the top.
fn sort_list(items: &mut [LibItem], mode: SortMode) {
    let pin = items
        .iter()
        .take_while(|i| i.is_header || i.is_play)
        .count();
    let tail = &mut items[pin..];
    match mode {
        SortMode::Added => tail.sort_by_key(|i| i.order),
        SortMode::Title => tail.sort_by_key(|i| i.name.to_lowercase()),
        SortMode::Artist => tail.sort_by_key(|i| i.subtitle.to_lowercase()),
    }
}

/// A drill-in detail view (artist / album / playlist contents).
struct Detail {
    context_uri: String,
    title: String,
    items: Vec<LibItem>,
    parent_selected: usize,
}

/// What an action-menu entry does when activated.
#[derive(Clone)]
enum ActionKind {
    ToggleLike {
        id: String,
        saved: bool,
    },
    Queue {
        uri: String,
    },
    AddToPlaylistMenu {
        track_uri: String,
    },
    AddToPlaylist {
        playlist_id: String,
        track_uri: String,
    },
    ToggleFollowArtist {
        id: String,
        following: bool,
    },
    ToggleSaveAlbum {
        id: String,
        saved: bool,
    },
    FollowPlaylist {
        id: String,
    },
    Play {
        uri: String,
        /// Carried so the play path can set `source_name` — without it the
        /// Queue view's PLAYING FROM header and the persisted resume source
        /// go stale.
        name: String,
    },
    Open {
        uri: String,
        name: String,
    },
    CopyLink {
        uri: String,
    },
}

struct ActionItem {
    label: String,
    kind: ActionKind,
}

struct ActionMenu {
    title: String,
    items: Vec<ActionItem>,
    selected: usize,
}

/// Result of activating (Enter on) a library item.
enum Activated {
    None,
    Open(String, String), // drill into a context (uri, name)
    Radio(String),        // start this song's radio (seed uri)
}

#[derive(Default, Clone)]
struct Library {
    home: Vec<LibItem>,
    recent: Vec<LibItem>,
    playlists: Vec<LibItem>,
    liked: Vec<LibItem>,
    albums: Vec<LibItem>,
    artists: Vec<LibItem>,
}

impl Library {
    fn items(&self, s: Section) -> &[LibItem] {
        match s {
            Section::Home => &self.home,
            Section::Recent => &self.recent,
            Section::Playlists => &self.playlists,
            Section::Liked => &self.liked,
            Section::Albums => &self.albums,
            Section::Artists => &self.artists,
        }
    }
    fn items_mut(&mut self, s: Section) -> &mut Vec<LibItem> {
        match s {
            Section::Home => &mut self.home,
            Section::Recent => &mut self.recent,
            Section::Playlists => &mut self.playlists,
            Section::Liked => &mut self.liked,
            Section::Albums => &mut self.albums,
            Section::Artists => &mut self.artists,
        }
    }
    fn set(&mut self, s: Section, items: Vec<LibItem>) {
        match s {
            Section::Home => self.home = items,
            Section::Recent => self.recent = items,
            Section::Playlists => self.playlists = items,
            Section::Liked => self.liked = items,
            Section::Albums => self.albums = items,
            Section::Artists => self.artists = items,
        }
    }
}

struct NowPlaying {
    uri: String,
    title: String,
    artist: String,
    album: String,
    duration_ms: u32,
    position_ms: u32,
    position_at: Instant,
    is_playing: bool,
    cover: Option<Cover>,
}

struct TrackMeta {
    uri: String,
    title: String,
    artist: String,
    album: String,
    duration_ms: u32,
    image: TrackImage,
    theme: Option<Theme>,
}

struct TrackImage {
    url: Option<String>,
    image: Option<image::DynamicImage>,
}

/// What kind of thing is currently playing — persisted so we can resume the real
/// context (and its live queue) on reboot, not just a bare track.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
enum PlaySource {
    #[default]
    None,
    Context(String), // playlist / album / artist URI
    Radio(String),   // seed track URI
    Liked,
}

/// Persisted across sessions (~/.cache/myx/state.json).
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SavedState {
    volume: u8,
    #[serde(default)]
    shuffle: bool,
    #[serde(default)]
    repeat: bool,
    #[serde(default)]
    last_played: Option<LastPlayed>,
    queue: Vec<String>,
    #[serde(default)]
    queue_uris: Vec<String>,
    #[serde(default)]
    source: PlaySource,
    #[serde(default)]
    source_name: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct LastPlayed {
    uri: String,
    title: String,
    artist: String,
    album: String,
    duration_ms: u32,
    position_ms: u32,
}

impl SavedState {
    fn path() -> Option<std::path::PathBuf> {
        Some(myx::home_dir()?.join(".cache/myx/state.json"))
    }
    fn load() -> SavedState {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Mouse hit rects, written by the renderer and read only by `handle_mouse`.
///
/// Pure output: every field is (re)set or cleared on each frame that draws the
/// thing it belongs to, so nothing here is threaded frame-to-frame.
#[derive(Default)]
struct HitRects {
    /// Last-rendered progress-bar rect (for click-to-seek).
    bar: Option<Rect>,
    /// Last-rendered sidebar scrollbar track + item count (drag-to-scroll).
    scroll: Option<Rect>,
    scroll_len: usize,
    /// Last-rendered volume-meter bar region (click/drag to set volume).
    vol: Option<Rect>,
    /// View tabs in the header.
    tabs: Vec<(RightView, Rect)>,
    /// Library list viewport.
    lib: Option<Rect>,
}

/// Everything the renderer writes, kept out of `App` so every render function
/// can take `&App`.
#[derive(Default)]
struct FrameOut {
    hits: HitRects,
    /// Library viewport start row. Unlike `hits`, this is read-modify-write:
    /// the renderer feeds the previous frame's value into `scroll_offset` and
    /// stores the result back, which is what makes scrolling sticky. Owned by
    /// `run_ui` so it survives across frames.
    lib_offset: usize,
}

/// Long-lived services the UI talks to. All three are used through `&self`
/// (the `Arc<Mutex<_>>` is only ever cloned), so grouping them costs no
/// borrow flexibility.
struct Services {
    engine: Engine,
    picker: Picker,
    webapi: Arc<Mutex<WebApi>>,
}

/// The palette currently on screen, plus the cross-fade walking it towards
/// the incoming track's palette. `displayed` is what every widget reads;
/// `target` is only used to snap exactly on completion.
struct ThemeState {
    displayed: Theme,
    target: Theme,
    fade: Option<ThemeFade>,
}

impl ThemeState {
    fn start_fade(&mut self, to: Theme) {
        self.fade = Some(ThemeFade::new(
            self.displayed,
            to,
            Duration::from_millis(FADE_MS),
        ));
        self.target = to;
    }

    fn advance(&mut self) {
        if let Some(fade) = &self.fade {
            self.displayed = fade.current();
            if fade.is_done() {
                self.displayed = self.target;
                self.fade = None;
            }
        }
    }
}

/// Playback controls and the queue — everything the transport bar and the
/// persisted `SavedState` care about. None of it touches the playhead.
struct Transport {
    shuffle: bool,
    repeat: bool,
    volume: u8, // 0..=100 (mirrors the 50% mixer default)
    queue: Vec<String>,
    queue_uris: Vec<String>,
    // Whether real playback has started this session (gates resume-on-play).
    playback_started: bool,
    // What's playing (context/radio/liked), for faithful resume on reboot.
    source: PlaySource,
    source_name: String,
}

/// The playhead: what's playing plus the coalesced Shift+arrow scrub state.
///
/// These live together because every scrub method touches both — `seek_step`
/// reads `now.duration_ms` and writes the `seek_*` fields, and
/// `set_local_position` gates a write to `now` on `seek_target`.
struct PlaybackState {
    now: Option<NowPlaying>,
    // Shift+arrow scrubbing, coalesced (see `seek_step`).
    seek_target: Option<u32>,
    seek_last_step: Instant,
    seek_last_input: Instant,
}

/// The library browser: what's loaded, where the cursor is, and the drill-in
/// stack. The viewport offset is not here — it lives in `FrameOut`, since the
/// renderer owns it across frames.
struct BrowseState {
    library: Library,
    section: Section,
    selected: usize,
    sort: SortMode,
    // Drill-in stack (artist → album → …). Topmost is what's shown.
    details: Vec<Detail>,
}

/// The `/` search overlay: whether the prompt is capturing keys, the typed
/// query, and the results that temporarily replace the library list.
struct SearchState {
    input_mode: bool,
    query: String,
    searching: bool,
    search_results: Vec<LibItem>,
}

/// What the user is looking at: the right pane's mode, the zen (sidebar
/// hidden) toggle, the lyrics backing the Lyrics view, and the actions
/// overlay drawn on top of everything.
struct ViewState {
    // Which view fills the right pane.
    mode: RightView,
    // Sidebar hidden, so the right view (and its cover) gets the whole width.
    zen: bool,
    // Lyrics: (timestamp_ms, line). Synced when timestamps are non-zero.
    lyrics: Vec<(u32, String)>,
    lyrics_synced: bool,
    // Context actions menu overlay (opened with `a`).
    actions: Option<ActionMenu>,
}

/// Cross-cutting session bookkeeping: what we resumed into, which metadata
/// fetch is still in flight, and the input timestamps that make Ctrl-C and
/// double-click work.
struct SessionState {
    restore_uri: Option<String>,
    // Track URI whose metadata was last requested. Fetches run on separate
    // blocking tasks and can land out of order when skipping quickly, so a
    // reply for any other track is stale and must be dropped.
    pending_meta: Option<String>,
    // Whether we reclaimed a live server-side session (vs. local fallback).
    reclaimed: bool,
    // Timestamp of last Ctrl-C — a second press within 1.5s quits.
    last_ctrl_c: Option<Instant>,
    last_click: Option<(u16, Instant)>,
}

struct App {
    svc: Services,
    theme: ThemeState,
    playback: PlaybackState,
    // Best-effort OS integration. Headless/SSH sessions may not expose the
    // platform media service, but that must never prevent Myx from playing.
    media_controls: Option<MediaControls>,
    status: String,
    browse: BrowseState,
    transport: Transport,
    search: SearchState,
    view: ViewState,
    session: SessionState,
    // What the album art box owes the next frame. See ArtRepaint.
    art_repaint: ArtRepaint,
}

fn should_apply_engine_position(from_engine: bool, seek_target: Option<u32>) -> bool {
    !(from_engine && seek_target.is_some())
}

impl PlaybackState {
    fn position_ms(&self) -> u32 {
        match &self.now {
            Some(n) if n.is_playing => {
                (n.position_ms + n.position_at.elapsed().as_millis() as u32).min(n.duration_ms)
            }
            Some(n) => n.position_ms.min(n.duration_ms),
            None => 0,
        }
    }
    /// Move the progress bar, without telling the engine. Reports from the
    /// engine are ignored mid-scrub — what we painted is newer than anything
    /// librespot has heard about.
    fn set_local_position(&mut self, position_ms: u32, from_engine: bool) {
        if !should_apply_engine_position(from_engine, self.seek_target) {
            return;
        }
        if let Some(n) = self.now.as_mut() {
            n.position_ms = position_ms.min(n.duration_ms);
            n.position_at = Instant::now();
        }
    }
    /// Seek to an absolute position (clamped), updating the local display too.
    fn seek_to(&mut self, engine: &Engine, position_ms: u32) {
        let Some(dur) = self.now.as_ref().map(|n| n.duration_ms) else {
            return;
        };
        let new = position_ms.min(dur);
        let _ = engine.seek(new);
        self.set_local_position(new, false);
    }
    /// One Shift+arrow press, moving the playhead by `delta_ms`.
    ///
    /// A seek per key repeat overshot the track and made librespot flush and
    /// refill its audio buffer 30×/s — that pile-up was the stutter. Repeats are
    /// throttled and the engine seek deferred to `flush_seek`.
    fn seek_step(&mut self, delta_ms: i64) {
        let now = Instant::now();
        if self.seek_target.is_some() && now.duration_since(self.seek_last_step) < SEEK_REPEAT {
            // The settle timer must see it, or a long hold commits early.
            self.seek_last_input = now;
            return;
        }
        let Some(dur) = self.now.as_ref().map(|n| n.duration_ms) else {
            return;
        };
        let from = self.seek_target.unwrap_or_else(|| self.position_ms());
        let target = scrub_target(from, dur, delta_ms);
        self.seek_target = Some(target);
        self.seek_last_step = now;
        self.seek_last_input = now;
        self.set_local_position(target, false);
    }
    /// Commit a finished scrub as a single engine seek, once the keys stop.
    fn flush_seek(&mut self, engine: &Engine, now: Instant) {
        if now.duration_since(self.seek_last_input) < SEEK_SETTLE {
            return;
        }
        if let Some(target) = self.seek_target.take() {
            self.seek_to(engine, target);
        }
    }
}

impl App {
    fn cur_items(&self) -> &[LibItem] {
        if let Some(d) = self.browse.details.last() {
            &d.items
        } else if self.search.searching {
            &self.search.search_results
        } else {
            self.browse.library.items(self.browse.section)
        }
    }
    fn cur_list_mut(&mut self) -> &mut Vec<LibItem> {
        if let Some(d) = self.browse.details.last_mut() {
            &mut d.items
        } else if self.search.searching {
            &mut self.search.search_results
        } else {
            self.browse.library.items_mut(self.browse.section)
        }
    }
    /// First non-header index (where a fresh selection should land).
    fn first_selectable(&self) -> usize {
        self.cur_items()
            .iter()
            .position(|i| !i.is_header)
            .unwrap_or(0)
    }
    /// Move the selection by `dir`, skipping header rows, clamped at the ends.
    fn move_sel(&mut self, dir: isize) {
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
    fn normalize_selection(&mut self) {
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
    fn play_context_row(&mut self, uri: String, name: String, shuffle: bool) {
        self.status = format!("starting {name}…");
        self.transport.source = PlaySource::Context(uri.clone());
        self.transport.source_name = name;
        if let Err(e) = self.svc.engine.play_context(uri, shuffle) {
            self.status = format!("couldn't play: {e:#}");
        }
    }

    /// Play whatever's selected (in the current section, or in search results).
    /// Act on the selected item. Returns what the caller should do next.
    fn activate(&mut self) -> Activated {
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

// ------------------------------------------------------------------ main

fn main() -> Result<()> {
    install_librespot_log();
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Refuse to start a second instance — two myx's racing on the shared Web API
    // token cache corrupts the OAuth refresh dance (Spotify rotates refresh tokens).
    let _instance_lock = acquire_single_instance_lock();

    // Restore last session first, so the engine starts at the saved volume.
    let saved = SavedState::load();
    let init_vol = if saved.volume == 0 {
        80
    } else {
        saved.volume.min(100)
    };

    // OAuth may need to print a browser URL, including when a cached refresh
    // token has been revoked. Complete both auth flows before entering the
    // alternate screen so that recovery prompts can never be hidden by the TUI.
    if engine::needs_authorization() || !WebApi::is_cached() {
        println!("myx: first run — authorizing with Spotify…");
    }
    let ((creds, webapi), mut terminal) = auth_then_terminal(
        || {
            let creds = engine::credentials()?;
            let webapi = WebApi::init().context("authorize web api")?;
            Ok((creds, webapi))
        },
        init_terminal,
    )?;

    // Query the terminal for its graphics protocol before anything else is
    // running: picking sixel swaps `TERM` around the query, and `setenv` is only
    // safe without concurrent readers. Hence the hand-built runtime below rather
    // than `#[tokio::main]`, which would already have spawned its workers by the
    // time this line ran.
    let picker = Cover::make_picker(myx::config::get().protocol.as_deref());
    // Halfblocks here means the graphics query got no answer — the art will look
    // like a 25×26 mosaic. MYX_PROTOCOL overrides it.
    liblog(format!(
        "cover: {:?}, font {:?}",
        picker.protocol_type(),
        picker.font_size()
    ));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .context("start tokio runtime")?;
    let res = runtime.block_on(boot(&mut terminal, saved, init_vol, creds, webapi, picker));
    restore_terminal(&mut terminal)?;
    res
}

// Run every potentially-interactive authentication step before constructing the
// terminal. This tiny seam is deliberately generic so the ordering can be
// regression-tested without Spotify credentials or a real terminal.
fn auth_then_terminal<A, T, Auth, Init>(auth: Auth, init_terminal: Init) -> Result<(A, T)>
where
    Auth: FnOnce() -> Result<A>,
    Init: FnOnce() -> Result<T>,
{
    let authenticated = auth()?;
    let terminal = init_terminal()?;
    Ok((authenticated, terminal))
}

fn optional_integration<T, E>(ready: bool, init: impl FnOnce() -> Result<T, E>) -> Option<T> {
    ready.then(init).and_then(Result::ok)
}

/// Everything from the loading screen to the event loop. Split out of `main` so
/// a failure on the way up still leaves the terminal restored.
async fn boot(
    terminal: &mut Term,
    saved: SavedState,
    init_vol: u8,
    creds: librespot_core::authentication::Credentials,
    webapi: WebApi,
    picker: Picker,
) -> Result<()> {
    let (ev_tx, ev_rx) = flume::unbounded::<EngineEvent>();
    let engine = with_loader(
        terminal,
        "connecting to Spotify",
        engine::run(creds, ev_tx, init_vol),
    )
    .await?
    .context("start engine")?;

    let webapi = Arc::new(Mutex::new(webapi));

    if let Some(uri) = std::env::args().nth(1) {
        let _ = engine.play_context(uri, false);
    }

    // Rebuild the last now-playing (paused) for a seamless resume look.
    let now = saved.last_played.as_ref().map(|last_played| NowPlaying {
        uri: last_played.uri.clone(),
        title: last_played.title.clone(),
        artist: last_played.artist.clone(),
        album: last_played.album.clone(),
        duration_ms: last_played.duration_ms,
        position_ms: last_played.position_ms,
        position_at: Instant::now(),
        is_playing: false,
        cover: None,
    });

    let restore_uri = saved.last_played.as_ref().map(|lp| lp.uri.clone());

    // HWND is a Windows-specific API.
    #[cfg(unix)]
    let hwnd = None;

    // Myx is a TUI with no window of its own, get the console's window instead.
    #[cfg(windows)]
    let hwnd = Some(unsafe { windows_win::sys::GetConsoleWindow() });

    // macOS media controls require an event loop. Failure only disables native
    // integration; the terminal player remains fully usable.
    #[cfg(target_os = "macos")]
    let media_event_loop = winit::event_loop::EventLoop::new().ok();
    #[cfg(not(target_os = "macos"))]
    let media_platform_ready = true;
    #[cfg(target_os = "macos")]
    let media_platform_ready = media_event_loop.is_some();

    let media_controls = optional_integration(media_platform_ready, || {
        MediaControls::new(PlatformConfig {
            dbus_name: "myx",
            display_name: "Myx",
            hwnd,
        })
    });
    if media_platform_ready && media_controls.is_none() {
        liblog("media controls unavailable; continuing without native integration");
    }

    let app = App {
        svc: Services {
            engine,
            picker,
            webapi,
        },
        media_controls,
        playback: PlaybackState {
            now,
            seek_target: None,
            seek_last_step: Instant::now(),
            seek_last_input: Instant::now(),
        },
        theme: ThemeState {
            displayed: TOKYONIGHT,
            target: TOKYONIGHT,
            fade: None,
        },
        status: "loading library…".to_string(),
        browse: BrowseState {
            library: Library::default(),
            section: Section::Home,
            selected: 0,
            sort: SortMode::Added,
            details: Vec::new(),
        },
        transport: Transport {
            shuffle: saved.shuffle,
            repeat: saved.repeat,
            volume: if saved.volume == 0 {
                80
            } else {
                saved.volume.min(100)
            },
            queue: saved.queue,
            queue_uris: saved.queue_uris,
            playback_started: false,
            source: saved.source.clone(),
            source_name: saved.source_name.clone(),
        },
        search: SearchState {
            input_mode: false,
            query: String::new(),
            searching: false,
            search_results: Vec::new(),
        },
        view: ViewState {
            mode: RightView::NowPlaying,
            zen: false,
            lyrics: Vec::new(),
            lyrics_synced: false,
            actions: None,
        },
        session: SessionState {
            restore_uri,
            pending_meta: None,
            reclaimed: false,
            last_ctrl_c: None,
            last_click: None,
        },
        art_repaint: ArtRepaint::Idle,
    };

    run_ui(terminal, app, ev_rx).await
}

struct Radio {
    start_position_ms: u32,
    uris: Vec<String>,
}

/// Every `Sender` the UI loop hands to input handlers and spawned fetches.
/// Receivers stay local to `run_ui` because `select!` needs them there.
struct UiChannels {
    meta: flume::Sender<TrackMeta>,
    lib: flume::Sender<(Section, Vec<LibItem>)>,
    queue: flume::Sender<Vec<(String, String)>>,
    search: flume::Sender<Vec<LibItem>>,
    lyrics: flume::Sender<(Vec<(u32, String)>, bool)>,
    detail: flume::Sender<(String, String, Vec<LibItem>)>,
    menu: flume::Sender<ActionMenu>,
    astatus: flume::Sender<String>,
    pstate: flume::Sender<RemotePlaybackState>,
    radio: flume::Sender<Result<Radio, String>>,
    libdone: flume::Sender<bool>,
}

async fn run_ui(
    terminal: &mut Term,
    mut app: App,
    ev_rx: flume::Receiver<EngineEvent>,
) -> Result<()> {
    let (in_tx, in_rx) = flume::unbounded::<Event>();
    std::thread::spawn(move || loop {
        if matches!(event::poll(Duration::from_millis(200)), Ok(true)) {
            if let Ok(ev) = event::read() {
                if in_tx.send(ev).is_err() {
                    break;
                }
            }
        }
    });

    let (meta_tx, meta_rx) = flume::unbounded::<TrackMeta>();
    let (lib_tx, lib_rx) = flume::unbounded::<(Section, Vec<LibItem>)>();
    let (queue_tx, queue_rx) = flume::unbounded::<Vec<(String, String)>>();
    let (search_tx, search_rx) = flume::unbounded::<Vec<LibItem>>();
    let (lyrics_tx, lyrics_rx) = flume::unbounded::<(Vec<(u32, String)>, bool)>();
    let (detail_tx, detail_rx) = flume::unbounded::<(String, String, Vec<LibItem>)>();
    let (menu_tx, menu_rx) = flume::unbounded::<ActionMenu>();
    let (astatus_tx, astatus_rx) = flume::unbounded::<String>();
    let (pstate_tx, pstate_rx) = flume::unbounded::<RemotePlaybackState>();
    let (radio_tx, radio_rx) = flume::unbounded::<Result<Radio, String>>();
    let (libdone_tx, libdone_rx) = flume::unbounded::<bool>();
    let (souvlaki_tx, souvlaki_rx) = flume::unbounded::<MediaControlEvent>();
    let chans = UiChannels {
        meta: meta_tx,
        lib: lib_tx,
        queue: queue_tx,
        search: search_tx,
        lyrics: lyrics_tx,
        detail: detail_tx,
        menu: menu_tx,
        astatus: astatus_tx,
        pstate: pstate_tx,
        radio: radio_tx,
        libdone: libdone_tx,
    };
    spawn_library_fetch(
        app.svc.webapi.clone(),
        chans.lib.clone(),
        chans.libdone.clone(),
    );

    // Reclaim server-side playback: read live state + transfer it onto myx so the
    // full context + queue + position come back.
    //
    // Clone: `spawn_restore` sends once and exits. Moving the sender in would
    // drop the last one, and a disconnected receiver resolves `recv_async()`
    // instantly and forever — spinning the select loop below.
    spawn_restore(
        app.svc.webapi.clone(),
        app.svc.engine.device_id(),
        chans.pstate.clone(),
    );

    // Re-enrich the restored last-played track (cover / theme / lyrics).
    if let Some(uri) = app.session.restore_uri.take() {
        if let Some(id) = track_id_from_uri(&uri) {
            app.session.pending_meta = Some(format!("spotify:track:{id}"));
            let webapi = app.svc.webapi.clone();
            let tx = chans.meta.clone();
            tokio::task::spawn_blocking(move || {
                let _ = tx.send(fetch_track_meta(&webapi, &id));
            });
        }
    }

    if let Some(controls) = app.media_controls.as_mut() {
        if controls
            .attach(move |event| {
                let _ = souvlaki_tx.send(event);
            })
            .is_err()
        {
            liblog("media controls failed to attach; continuing without native integration");
            app.media_controls = None;
        }
    }
    let mut media_events_open = true;

    let mut lib_attempts: u32 = 0;
    // A persistent interval must live OUTSIDE the select loop. Recreating a
    // `sleep()` every loop starves forever when player events are continuously
    // ready: the future gets cancelled/reset before its deadline. That was the
    // frozen-UI bug.
    let mut frame = tokio::time::interval(Duration::from_millis(16));
    frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_draw = Instant::now() - IDLE_REDRAW;
    let mut last_sync = Instant::now();
    // Nothing is on screen yet, so the first tick must draw.
    let mut dirty = true;
    let mut last_layout = (app.view.mode, app.view.zen);
    let mut overlay_open = app.view.actions.is_some();
    // What the renderer writes. Lives across frames: the hit rects are what the
    // mouse handler reads between draws, and `lib_offset` is fed back into the
    // next frame's sticky-viewport calculation.
    let mut out = FrameOut::default();

    loop {
        let touched = tokio::select! {
            biased;
            _ = frame.tick() => {
                app.playback.flush_seek(&app.svc.engine, Instant::now());
                // Drain library updates deterministically before rendering. Keeping
                // this solely as a select arm could starve under a hot player-event
                // stream / 60fps visualizer — which looked like a frozen library.
                while let Ok((section, mut items)) = lib_rx.try_recv() {
                    let count = items.len();
                    dirty = true;
                    liblog(format!("ui: received {} rows for {}", count, section.label()));
                    for (i, it) in items.iter_mut().enumerate() {
                        it.order = i as u32;
                    }
                    app.browse.library.set(section, items);
                    sort_list(app.browse.library.items_mut(section), app.browse.sort);
                    if section == app.browse.section {
                        app.normalize_selection();
                    }
                    app.status = format!("loaded {}", section.label());
                }
                while let Ok(got_any) = libdone_rx.try_recv() {
                    dirty = true;
                    if got_any {
                        lib_attempts = 0;
                        app.status.clear();
                    } else if lib_attempts < 2 {
                        lib_attempts += 1;
                        app.status = "retrying library…".to_string();
                        spawn_library_fetch(app.svc.webapi.clone(), chans.lib.clone(), chans.libdone.clone());
                    } else {
                        app.status = "library failed — press r to reload".to_string();
                    }
                }
                // Radio results are drained here (not as a `select!` arm) for the
                // same reason as the library: under the biased 16ms frame tick a
                // pure recv arm starves and the station never plays.
                while let Ok(rad) = radio_rx.try_recv() {
                    dirty = true;
                    match rad {
                        Ok(radio) if !radio.uris.is_empty() => {
                            if let Err(e) = app.svc.engine.play_tracks(radio.uris, None, radio.start_position_ms, false) {
                                app.status = format!("couldn't play radio: {e:#}");
                            }
                            app.transport.playback_started = true;
                            app.status = "radio started".to_string();
                            // Grab the freshly-populated station queue shortly after.
                            let webapi = app.svc.webapi.clone();
                            let tx = chans.queue.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_millis(1500)).await;
                                spawn_queue_fetch(webapi, tx);
                            });
                        }
                        Ok(_) => {
                            app.status = "radio: no tracks returned".to_string();
                        }
                        Err(e) => {
                            app.status = format!("radio failed: {e}");
                        }
                    }
                }

                // The visualizer only animates while it is on screen; on Queue
                // its frame rate buys nothing. Synced lyrics move too — at the
                // idle rate the highlighted line lands half a second late.
                let animating = app.theme.fade.is_some()
                    || (app.view.mode == RightView::Lyrics && app.view.lyrics_synced)
                    || (app.view.mode == RightView::NowPlaying
                        && app.svc.engine.bands.try_lock().map(|g| g.is_active).unwrap_or(false));
                if app.art_repaint != ArtRepaint::Idle {
                    dirty = true;
                }
                if (app.view.mode, app.view.zen) != last_layout {
                    last_layout = (app.view.mode, app.view.zen);
                    app.art_repaint = ArtRepaint::Wipe;
                    dirty = true;
                }
                // An overlay draws over the art and the terminal loses those
                // pixels, so the cover has to be sent again once it closes.
                // Opening one must not wipe: the image would be redrawn a frame
                // later, back on top of the popup.
                let overlay = app.view.actions.is_some();
                if overlay != overlay_open {
                    overlay_open = overlay;
                    if !overlay {
                        app.art_repaint = ArtRepaint::Wipe;
                    }
                    dirty = true;
                }
                if should_draw(dirty, animating, last_draw.elapsed()) {
                    app.theme.advance();
                    // Present the frame atomically. Without this the terminal
                    // renders whatever has arrived so far, and a recolour that
                    // touches every glyph on screen shows up half-applied.
                    // Terminals that don't know the mode ignore it.
                    let _ = execute!(io::stdout(), BeginSynchronizedUpdate);
                    let repaint = app.art_repaint;
                    let drawn = terminal.draw(|f| render(f, &app, &mut out, repaint));
                    let _ = execute!(io::stdout(), EndSynchronizedUpdate);
                    drawn?;
                    app.art_repaint = app.art_repaint.advance();
                    last_draw = Instant::now();
                    dirty = false;
                }
                if last_sync.elapsed() >= SYNC_EVERY {
                    last_sync = Instant::now();
                    // Refresh the live queue while playing so the snapshot stays
                    // current, then persist it (survives reboot).
                    if app.transport.playback_started || app.session.reclaimed {
                        spawn_queue_fetch(app.svc.webapi.clone(), chans.queue.clone());
                    }
                    save_state(&app);
                }
                false
            }
            ev = ev_rx.recv_async() => {
                let Ok(ev) = ev else { break };
                handle_engine_event(&mut app, ev, &chans.meta);
                true
            }
            ev = in_rx.recv_async() => {
                match ev {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        let quit = handle_key(&mut app, key.code, key.modifiers, &chans);
                        if quit {
                            save_state(&app);
                            break;
                        }
                    }
                    Ok(Event::Mouse(m)) => {
                        let quit = handle_mouse(&mut app, &out, m, &chans);
                        if quit {
                            save_state(&app);
                            break;
                        }
                    }
                    // A repaint from the terminal's own buffer loses inline art.
                    // A repaint from the terminal's own buffer loses inline art;
                    // returning to the window is when it has to go back out.
                    // tmux only forwards focus with `focus-events on` — see the
                    // README — and stores sixel itself either way.
                    Ok(Event::Resize(..)) | Ok(Event::FocusGained) => {
                        app.art_repaint = ArtRepaint::Wipe;
                    }
                    _ => {}
                }
                true
            }
            ev = souvlaki_rx.recv_async(), if media_events_open => {
                match consume_media_event(ev, &mut media_events_open) {
                    Some(ev) => handle_media_control_event(&mut app, ev, &chans.radio),
                    None => {
                        app.media_controls = None;
                        liblog("media controls event channel closed; native integration disabled");
                    }
                }
                true
            }
            m = meta_rx.recv_async() => {
                if let Ok(meta) = m { apply_meta(&mut app, meta, &chans.lyrics); }
                true
            }
            q = queue_rx.recv_async() => {
                // Don't let an empty live queue (e.g. a bare resumed track) wipe
                // the restored/last-known snapshot.
                if let Ok(q) = q {
                    if !q.is_empty() {
                        app.transport.queue = q.iter().map(|(d, _)| d.clone()).collect();
                        app.transport.queue_uris = q.into_iter().map(|(_, u)| u).collect();
                    }
                }
                true
            }
            s = search_rx.recv_async() => {
                if let Ok(results) = s {
                    app.search.search_results = results;
                    app.browse.selected = app.first_selectable();
                    app.status = if app.search.search_results.is_empty() {
                        "no results".to_string()
                    } else {
                        String::new()
                    };
                }
                true
            }
            ly = lyrics_rx.recv_async() => {
                if let Ok((lines, synced)) = ly {
                    app.view.lyrics = lines;
                    app.view.lyrics_synced = synced;
                }
                true
            }
            d = detail_rx.recv_async() => {
                if let Ok((context_uri, title, items)) = d {
                    app.browse.details.push(Detail { context_uri, title, items, parent_selected: app.browse.selected });
                    app.browse.selected = app.first_selectable();
                    app.status.clear();
                }
                true
            }
            menu = menu_rx.recv_async() => {
                if let Ok(mut menu) = menu {
                    // Enrich only an already-open menu (don't reopen a closed one),
                    // preserving the user's current selection across the swap.
                    if app.view.actions.is_some() && !menu.items.is_empty() {
                        if let Some(open) = app.view.actions.as_ref() {
                            menu.selected = open.selected.min(menu.items.len() - 1);
                        }
                        app.view.actions = Some(menu);
                    }
                }
                true
            }
            st = astatus_rx.recv_async() => {
                if let Ok(msg) = st { app.status = msg; }
                true
            }
            ps = pstate_rx.recv_async() => {
                if let Ok(state) = ps {
                    app.session.reclaimed = true;
                    app.transport.shuffle = state.shuffle;
                    app.transport.repeat = state.repeat;
                    app.transport.volume = state.volume.min(100);
                    let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
                    app.playback.now = Some(NowPlaying {
                        uri: format!("spotify:track:{}", state.track_id),
                        title: String::new(),
                        artist: String::new(),
                        album: String::new(),
                        duration_ms: 0,
                        position_ms: state.progress_ms,
                        position_at: Instant::now(),
                        is_playing: false,
                        cover: None,
                    });
                    let webapi = app.svc.webapi.clone();
                    let tx = chans.meta.clone();
                    let id = state.track_id.clone();
                    app.session.pending_meta = Some(format!("spotify:track:{id}"));
                    tokio::task::spawn_blocking(move || { let _ = tx.send(fetch_track_meta(&webapi, &id)); });
                    spawn_queue_fetch(app.svc.webapi.clone(), chans.queue.clone());
                }
                true
            }
        };
        dirty |= touched;
    }
    Ok(())
}

/// Resume the persisted playback source at the last track/position — the
/// faithful reboot resume (real context ⇒ real queue continuation).
fn resume_source(app: &mut App, radio_tx: &flume::Sender<Result<Radio, String>>) {
    let track = app
        .playback
        .now
        .as_ref()
        .map(|n| n.uri.clone())
        .filter(|u| !u.is_empty());
    let pos = app
        .playback
        .now
        .as_ref()
        .map(|n| n.position_ms)
        .unwrap_or(0);

    match app.transport.source.clone() {
        PlaySource::Context(ctx) => {
            if let Err(e) = app
                .svc
                .engine
                .play_context_at(ctx, track, pos, app.transport.shuffle)
            {
                app.status = format!("couldn't play: {e:#}");
            }
        }
        PlaySource::Radio(seed) => {
            let session = app.svc.engine.session();
            let tx = radio_tx.clone();
            app.status = "resuming radio…".to_string();
            tokio::spawn(async move {
                let res = match tokio::time::timeout(
                    Duration::from_secs(12),
                    engine::radio_tracks(&session, &seed),
                )
                .await
                {
                    Ok(r) => r.map_err(|e| e.to_string()),
                    Err(_) => Err("timed out (mercury radio endpoint unresponsive)".to_string()),
                };

                let _ = tx.send(res.map(|uris| Radio {
                    uris,
                    start_position_ms: pos,
                }));
            });
        }
        PlaySource::Liked if !app.browse.library.liked.is_empty() => {
            let uris: Vec<String> = app
                .browse
                .library
                .liked
                .iter()
                .map(|i| i.uri.clone())
                .collect();
            if let Err(e) = app
                .svc
                .engine
                .play_tracks(uris, track, pos, app.transport.shuffle)
            {
                app.status = format!("couldn't play: {e:#}");
            }
        }
        _ => {
            // No known context — resume the last track followed by the saved
            // queue so playback actually continues past the first song.
            if !app.transport.queue_uris.is_empty() {
                let mut uris = Vec::with_capacity(app.transport.queue_uris.len() + 1);
                if let Some(u) = &track {
                    uris.push(u.clone());
                }
                uris.extend(app.transport.queue_uris.iter().cloned());
                if let Err(e) = app
                    .svc
                    .engine
                    .play_tracks(uris, track, pos, app.transport.shuffle)
                {
                    app.status = format!("couldn't play: {e:#}");
                }
            } else {
                match track {
                    Some(uri) => {
                        if let Err(e) = app.svc.engine.play_track_at(uri, pos) {
                            app.status = format!("couldn't play: {e:#}");
                        }
                    }
                    None => {
                        if let Err(e) = app.svc.engine.play() {
                            app.status = format!("couldn't play: {e:#}");
                        }
                    }
                }
            }
        }
    }
}

/// Does this row carry a playable context URI, and under what name?
///
/// Context rows (playlist / album / artist) and the synthesized "▶︎ Play X"
/// rows both do; headers and tracks do not. Kept pure and free-standing so it
/// is unit-testable — `App` owns a librespot `Spirc` and cannot be built in a
/// test. `enter_label` shares this predicate so Enter opens exactly the rows
/// `P` plays.
fn context_target(item: &LibItem) -> Option<(String, String)> {
    (!item.is_header && !item.is_track).then(|| (item.uri.clone(), item.name.clone()))
}

/// Enter opens context rows and plays everything else.
fn enter_label(item: Option<&LibItem>) -> &'static str {
    match item {
        Some(i) if !i.is_track && !i.is_header => "open",
        _ => "select",
    }
}

/// `P` / `S`: play the highlighted context from anywhere — library section,
/// search results, or inside a drill-in (`cur_items` resolves all three).
fn play_selected_context(app: &mut App, shuffle: bool) {
    let Some(item) = app.cur_items().get(app.browse.selected).cloned() else {
        return;
    };
    match context_target(&item) {
        Some((uri, name)) => app.play_context_row(uri, name, shuffle),
        None => app.status = "not a playlist, album, or artist".to_string(),
    }
}

/// Snapshot the current session to disk (volume, last track, position, queue).
fn save_state(app: &App) {
    let last_played = app.playback.now.as_ref().map(|now| LastPlayed {
        uri: now.uri.clone(),
        title: now.title.clone(),
        artist: now.artist.clone(),
        album: now.album.clone(),
        duration_ms: now.duration_ms,
        position_ms: app.playback.position_ms(),
    });

    let s = SavedState {
        volume: app.transport.volume,
        shuffle: app.transport.shuffle,
        repeat: app.transport.repeat,
        last_played,
        queue: app.transport.queue.clone(),
        queue_uris: app.transport.queue_uris.clone(),
        source: app.transport.source.clone(),
        source_name: app.transport.source_name.clone(),
    };
    s.save();
}

fn handle_engine_event(app: &mut App, ev: EngineEvent, meta_tx: &flume::Sender<TrackMeta>) {
    // Position ticks would bury everything else in the log.
    if !matches!(ev, EngineEvent::PositionCorrection { .. }) {
        liblog(format!("engine: {ev:?}"));
    }
    match ev {
        EngineEvent::TrackChanged { uri } => {
            app.status = "loading track…".to_string();
            if let Some(track_id) = track_id_from_uri(&uri) {
                // Record what we're waiting for so an earlier track's reply,
                // landing late, is discarded instead of overwriting this one.
                app.session.pending_meta = Some(format!("spotify:track:{track_id}"));
                let webapi = app.svc.webapi.clone();
                let tx = meta_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(fetch_track_meta(&webapi, &track_id));
                });
            }
        }
        EngineEvent::Playing { position_ms, .. } => {
            if !app.transport.playback_started {
                app.transport.playback_started = true;
                // Reapply persisted modes + volume to the freshly-started playback.
                let _ = app.svc.engine.shuffle(app.transport.shuffle);
                let _ = app.svc.engine.repeat(app.transport.repeat);
                let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
            }
            if let Some(n) = app.playback.now.as_mut() {
                n.is_playing = true;
            }
            app.playback.set_local_position(position_ms, true);
            if let Some(controls) = app.media_controls.as_mut() {
                let _ = controls.set_playback(MediaPlayback::Playing {
                    progress: Some(MediaPosition(Duration::from_millis(position_ms as u64))),
                });
            }
        }
        EngineEvent::Paused { position_ms, .. } => {
            if let Some(n) = app.playback.now.as_mut() {
                n.is_playing = false;
            }
            app.playback.set_local_position(position_ms, true);
            if let Some(controls) = app.media_controls.as_mut() {
                let _ = controls.set_playback(MediaPlayback::Paused {
                    progress: Some(MediaPosition(Duration::from_millis(position_ms as u64))),
                });
            }
        }
        EngineEvent::Stopped => {
            app.playback.now = None;
            app.transport.playback_started = false;
            // librespot cleared its context too, so only a fresh load can
            // resume from here — not a bare `play`.
            app.session.reclaimed = false;

            if let Some(controls) = app.media_controls.as_mut() {
                let _ = controls.set_playback(MediaPlayback::Stopped);
            }
        }
        EngineEvent::PositionCorrection { position_ms, .. } => {
            app.playback.set_local_position(position_ms, true);
            if let (Some(n), Some(controls)) =
                (app.playback.now.as_ref(), app.media_controls.as_mut())
            {
                let playback = if n.is_playing {
                    MediaPlayback::Playing {
                        progress: Some(MediaPosition(Duration::from_millis(position_ms as u64))),
                    }
                } else {
                    MediaPlayback::Paused {
                        progress: Some(MediaPosition(Duration::from_millis(position_ms as u64))),
                    }
                };
                let _ = controls.set_playback(playback);
            }
        }
        EngineEvent::Reconnecting => {
            app.status = "connection dropped — reconnecting…".to_string();
        }
        EngineEvent::Reconnected => {
            // The replacement Connect device starts idle, so whatever was
            // playing is not resumed; say so rather than leave a silent player
            // looking broken.
            app.status = if app.transport.playback_started {
                "reconnected — press play to resume".to_string()
            } else {
                "reconnected".to_string()
            };
        }
        EngineEvent::EndOfTrack { .. } => {}
    }
}

/// Is this metadata reply the one we are still waiting for?
///
/// `None` means nothing specific was requested (e.g. a path that predates the
/// guard), so accept — the guard only ever discards a reply we can prove is for
/// a different track.
fn meta_is_current(pending: Option<&str>, meta_uri: &str) -> bool {
    pending.is_none_or(|p| p == meta_uri)
}

fn apply_meta(
    app: &mut App,
    meta: TrackMeta,
    lyrics_tx: &flume::Sender<(Vec<(u32, String)>, bool)>,
) {
    // Metadata fetches run on independent blocking tasks, so skipping quickly
    // (n/b) can land an earlier track's reply after a later one. Applying it
    // would replace the whole NowPlaying — title, artist and cover — with the
    // wrong track's data.
    if !meta_is_current(app.session.pending_meta.as_deref(), &meta.uri) {
        return;
    }

    let cover = meta
        .image
        .image
        .as_ref()
        .map(|img| Cover::from_image(img.clone(), app.svc.picker.clone()));
    // A different cover encodes to a different symbol, so the diff emits it on
    // its own — no wipe, which would flash a blank box between the two covers.
    app.art_repaint = ArtRepaint::Draw;
    app.status.clear();
    app.view.lyrics.clear();
    app.view.lyrics_synced = false;

    // Fetch synced lyrics from lrclib for the new track.
    if !meta.title.is_empty() {
        let (artist, title, album, dur) = (
            meta.artist.clone(),
            meta.title.clone(),
            meta.album.clone(),
            meta.duration_ms,
        );
        let tx = lyrics_tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(fetch_lyrics_blocking(&artist, &title, &album, dur));
        });
    }

    app.playback.now = Some(NowPlaying {
        uri: meta.uri,
        title: meta.title,
        artist: meta.artist,
        album: meta.album,
        duration_ms: meta.duration_ms,
        position_ms: app
            .playback
            .now
            .as_ref()
            .map(|n| n.position_ms)
            .unwrap_or(0),
        position_at: Instant::now(),
        is_playing: app
            .playback
            .now
            .as_ref()
            .map(|n| n.is_playing)
            .unwrap_or(app.transport.playback_started),
        cover,
    });

    if let Some(theme) = meta.theme {
        app.theme.start_fade(theme);
    }

    if let Some(controls) = app.media_controls.as_mut() {
        let _ = controls.set_metadata(MediaMetadata {
            title: app.playback.now.as_ref().map(|n| n.title.as_str()),
            artist: app.playback.now.as_ref().map(|n| n.artist.as_str()),
            album: app.playback.now.as_ref().map(|n| n.album.as_str()),
            cover_url: meta.image.url.as_deref(),
            duration: app
                .playback
                .now
                .as_ref()
                .map(|n| Duration::from_millis(n.duration_ms as u64)),
        });
    }
}

// ------------------------------------------------------------------ terminal

/// Run `task`, drawing the startup screen until it finishes.
async fn with_loader<T>(
    terminal: &mut Term,
    label: &str,
    task: impl std::future::Future<Output = T>,
) -> Result<T> {
    tokio::pin!(task);
    let mut tick = tokio::time::interval(Duration::from_millis(80));
    let mut frame: usize = 0;
    loop {
        tokio::select! {
            biased;
            done = &mut task => return Ok(done),
            _ = tick.tick() => {
                terminal.draw(|f| render_loading(f, label, frame))?;
                frame = frame.wrapping_add(1);
            }
        }
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
#[path = "main_tests/mod.rs"]
mod main_tests;
