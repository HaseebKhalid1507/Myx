//! myx — the fully-wired terminal Spotify player.
//!
//! librespot streaming engine + Web API (your own client id) + album-art-reactive
//! theming with cross-fades + live FFT visualizer, in noodle's visual language.
//! Multi-section library (playlists / liked / albums / artists), shuffle, repeat,
//! and a live queue view.

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

struct App {
    engine: Engine,
    picker: Picker,
    displayed: Theme,
    target: Theme,
    fade: Option<ThemeFade>,
    now: Option<NowPlaying>,
    webapi: Arc<Mutex<WebApi>>,
    // Best-effort OS integration. Headless/SSH sessions may not expose the
    // platform media service, but that must never prevent Myx from playing.
    media_controls: Option<MediaControls>,
    status: String,
    library: Library,
    section: Section,
    selected: usize,
    shuffle: bool,
    repeat: bool,
    volume: u8, // 0..=100 (mirrors the 50% mixer default)
    queue: Vec<String>,
    queue_uris: Vec<String>,
    // Search
    input_mode: bool,
    query: String,
    searching: bool,
    search_results: Vec<LibItem>,
    // Lyrics: (timestamp_ms, line). Synced when timestamps are non-zero.
    lyrics: Vec<(u32, String)>,
    lyrics_synced: bool,
    // Which view fills the right pane.
    view: RightView,
    // Drill-in stack (artist → album → …). Topmost is what's shown.
    details: Vec<Detail>,
    // Context actions menu overlay (opened with `a`).
    actions: Option<ActionMenu>,
    restore_uri: Option<String>,
    // Track URI whose metadata was last requested. Fetches run on separate
    // blocking tasks and can land out of order when skipping quickly, so a
    // reply for any other track is stale and must be dropped.
    pending_meta: Option<String>,
    // What the album art box owes the next frame. See ArtRepaint.
    art_repaint: ArtRepaint,
    // Whether real playback has started this session (gates resume-on-play).
    playback_started: bool,
    // Whether we reclaimed a live server-side session (vs. local fallback).
    reclaimed: bool,
    // What's playing (context/radio/liked), for faithful resume on reboot.
    source: PlaySource,
    source_name: String,
    sort: SortMode,
    // Last-rendered progress-bar rect (for click-to-seek).
    bar_rect: Option<Rect>,
    // Last-rendered sidebar scrollbar track + item count (drag-to-scroll).
    scroll_rect: Option<Rect>,
    scroll_len: usize,
    // Last-rendered volume-meter bar region (click/drag to set volume).
    vol_rect: Option<Rect>,
    // Timestamp of last Ctrl-C — a second press within 1.5s quits.
    last_ctrl_c: Option<Instant>,
    // Mouse hit rects: view tabs, library list viewport (+its offset), last click.
    tab_rects: Vec<(RightView, Rect)>,
    lib_rect: Option<Rect>,
    lib_offset: usize,
    last_click: Option<(u16, Instant)>,
    // Sidebar hidden, so the right view (and its cover) gets the whole width.
    zen: bool,
    // Shift+arrow scrubbing, coalesced (see `seek_step`).
    seek_target: Option<u32>,
    seek_last_step: Instant,
    seek_last_input: Instant,
}

fn should_apply_engine_position(from_engine: bool, seek_target: Option<u32>) -> bool {
    !(from_engine && seek_target.is_some())
}

impl App {
    fn start_fade(&mut self, to: Theme) {
        self.fade = Some(ThemeFade::new(
            self.displayed,
            to,
            Duration::from_millis(FADE_MS),
        ));
        self.target = to;
    }
    fn cur_items(&self) -> &[LibItem] {
        if let Some(d) = self.details.last() {
            &d.items
        } else if self.searching {
            &self.search_results
        } else {
            self.library.items(self.section)
        }
    }
    fn cur_list_mut(&mut self) -> &mut Vec<LibItem> {
        if let Some(d) = self.details.last_mut() {
            &mut d.items
        } else if self.searching {
            &mut self.search_results
        } else {
            self.library.items_mut(self.section)
        }
    }
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
    fn seek_to(&mut self, position_ms: u32) {
        let Some(dur) = self.now.as_ref().map(|n| n.duration_ms) else {
            return;
        };
        let new = position_ms.min(dur);
        let _ = self.engine.seek(new);
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
    fn flush_seek(&mut self, now: Instant) {
        if now.duration_since(self.seek_last_input) < SEEK_SETTLE {
            return;
        }
        if let Some(target) = self.seek_target.take() {
            self.seek_to(target);
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
        let mut i = self.selected as isize;
        loop {
            i += dir;
            if i < 0 || i >= n {
                return;
            }
            if !items[i as usize].is_header {
                self.selected = i as usize;
                return;
            }
        }
    }
    /// If the selection landed on a header (e.g. after data loads), bump it off.
    fn normalize_selection(&mut self) {
        if self
            .cur_items()
            .get(self.selected)
            .is_some_and(|i| i.is_header)
        {
            self.selected = self.first_selectable();
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
        self.source = PlaySource::Context(uri.clone());
        self.source_name = name;
        if let Err(e) = self.engine.play_context(uri, shuffle) {
            self.status = format!("couldn't play: {e:#}");
        }
    }

    /// Play whatever's selected (in the current section, or in search results).
    /// Act on the selected item. Returns what the caller should do next.
    fn activate(&mut self) -> Activated {
        let Some(item) = self.cur_items().get(self.selected).cloned() else {
            return Activated::None;
        };
        if item.is_header {
            return Activated::None;
        }
        if item.is_play {
            // Special synthetic rows: play the Liked list (optionally shuffled).
            if item.uri == "myx:action:liked-play" {
                let uris: Vec<String> = self
                    .library
                    .liked
                    .iter()
                    .filter(|i| i.is_track)
                    .map(|i| i.uri.clone())
                    .collect();
                if !uris.is_empty() {
                    self.source = PlaySource::Liked;
                    self.source_name = "Liked Songs".to_string();
                    self.status = "starting Liked Songs…".to_string();
                    // Honour the current shuffle toggle instead of a dedicated row.
                    if let Err(e) = self.engine.play_tracks(uris, None, 0, self.shuffle) {
                        self.status = format!("couldn't play: {e:#}");
                    }
                }
                return Activated::None;
            }
            // Inside a drill-in the enclosing title is the better label
            // ("Chill Vibes"); standalone play rows fall back to their own.
            let name = self
                .details
                .last()
                .map(|d| d.title.clone())
                .unwrap_or_else(|| item.name.clone());
            let shuffle = self.shuffle;
            self.play_context_row(item.uri, name, shuffle);
            return Activated::None;
        }
        if item.is_track {
            if self.searching {
                // A search-result song starts that song's radio (seed + similar).
                self.source = PlaySource::Radio(item.uri.clone());
                self.source_name = format!("Radio · {}", item.name);
                return Activated::Radio(item.uri);
            }
            // Inside a drill-in → play its context at this track (real queue).
            if let Some(d) = self.details.last() {
                let ctx = d.context_uri.clone();
                self.source = PlaySource::Context(ctx.clone());
                self.source_name = d.title.clone();
                self.status = format!("starting {}…", item.name);
                if let Err(e) =
                    self.engine
                        .play_context_at(ctx, Some(item.uri.clone()), 0, self.shuffle)
                {
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
            if self.section == Section::Liked {
                self.source = PlaySource::Liked;
                self.source_name = "Liked Songs".to_string();
            } else {
                self.source = PlaySource::None;
                self.source_name = self.section.label().to_string();
            }
            if let Err(e) = self
                .engine
                .play_tracks(uris, Some(item.uri.clone()), 0, self.shuffle)
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
        engine,
        picker,
        media_controls,
        displayed: TOKYONIGHT,
        target: TOKYONIGHT,
        fade: None,
        now,
        webapi,
        status: "loading library…".to_string(),
        library: Library::default(),
        section: Section::Home,
        selected: 0,
        shuffle: saved.shuffle,
        repeat: saved.repeat,
        volume: if saved.volume == 0 {
            80
        } else {
            saved.volume.min(100)
        },
        queue: saved.queue,
        queue_uris: saved.queue_uris,
        input_mode: false,
        query: String::new(),
        searching: false,
        search_results: Vec::new(),
        lyrics: Vec::new(),
        lyrics_synced: false,
        view: RightView::NowPlaying,
        details: Vec::new(),
        actions: None,
        restore_uri,
        pending_meta: None,
        art_repaint: ArtRepaint::Idle,
        playback_started: false,
        reclaimed: false,
        source: saved.source.clone(),
        source_name: saved.source_name.clone(),
        sort: SortMode::Added,
        bar_rect: None,
        scroll_rect: None,
        scroll_len: 0,
        vol_rect: None,
        last_ctrl_c: None,
        tab_rects: Vec::new(),
        lib_rect: None,
        lib_offset: 0,
        last_click: None,
        zen: false,
        seek_target: None,
        seek_last_step: Instant::now(),
        seek_last_input: Instant::now(),
    };

    run_ui(terminal, app, ev_rx).await
}

struct Radio {
    start_position_ms: u32,
    uris: Vec<String>,
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
    let (pstate_tx, pstate_rx) = flume::unbounded::<PlaybackState>();
    let (radio_tx, radio_rx) = flume::unbounded::<Result<Radio, String>>();
    let (libdone_tx, libdone_rx) = flume::unbounded::<bool>();
    let (souvlaki_tx, souvlaki_rx) = flume::unbounded::<MediaControlEvent>();
    spawn_library_fetch(app.webapi.clone(), lib_tx.clone(), libdone_tx.clone());

    // Reclaim server-side playback: read live state + transfer it onto myx so the
    // full context + queue + position come back.
    //
    // Clone: `spawn_restore` sends once and exits. Moving the sender in would
    // drop the last one, and a disconnected receiver resolves `recv_async()`
    // instantly and forever — spinning the select loop below.
    spawn_restore(
        app.webapi.clone(),
        app.engine.device_id(),
        pstate_tx.clone(),
    );

    // Re-enrich the restored last-played track (cover / theme / lyrics).
    if let Some(uri) = app.restore_uri.take() {
        if let Some(id) = track_id_from_uri(&uri) {
            app.pending_meta = Some(format!("spotify:track:{id}"));
            let webapi = app.webapi.clone();
            let tx = meta_tx.clone();
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
    let mut last_layout = (app.view, app.zen);
    let mut overlay_open = app.actions.is_some();

    loop {
        let touched = tokio::select! {
            biased;
            _ = frame.tick() => {
                app.flush_seek(Instant::now());
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
                    app.library.set(section, items);
                    sort_list(app.library.items_mut(section), app.sort);
                    if section == app.section {
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
                        spawn_library_fetch(app.webapi.clone(), lib_tx.clone(), libdone_tx.clone());
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
                            if let Err(e) = app.engine.play_tracks(radio.uris, None, radio.start_position_ms, false) {
                                app.status = format!("couldn't play radio: {e:#}");
                            }
                            app.playback_started = true;
                            app.status = "radio started".to_string();
                            // Grab the freshly-populated station queue shortly after.
                            let webapi = app.webapi.clone();
                            let tx = queue_tx.clone();
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
                let animating = app.fade.is_some()
                    || (app.view == RightView::Lyrics && app.lyrics_synced)
                    || (app.view == RightView::NowPlaying
                        && app.engine.bands.try_lock().map(|g| g.is_active).unwrap_or(false));
                if app.art_repaint != ArtRepaint::Idle {
                    dirty = true;
                }
                if (app.view, app.zen) != last_layout {
                    last_layout = (app.view, app.zen);
                    app.art_repaint = ArtRepaint::Wipe;
                    dirty = true;
                }
                // An overlay draws over the art and the terminal loses those
                // pixels, so the cover has to be sent again once it closes.
                // Opening one must not wipe: the image would be redrawn a frame
                // later, back on top of the popup.
                let overlay = app.actions.is_some();
                if overlay != overlay_open {
                    overlay_open = overlay;
                    if !overlay {
                        app.art_repaint = ArtRepaint::Wipe;
                    }
                    dirty = true;
                }
                if should_draw(dirty, animating, last_draw.elapsed()) {
                    advance_fade(&mut app);
                    // Present the frame atomically. Without this the terminal
                    // renders whatever has arrived so far, and a recolour that
                    // touches every glyph on screen shows up half-applied.
                    // Terminals that don't know the mode ignore it.
                    let _ = execute!(io::stdout(), BeginSynchronizedUpdate);
                    let drawn = terminal.draw(|f| render(f, &mut app));
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
                    if app.playback_started || app.reclaimed {
                        spawn_queue_fetch(app.webapi.clone(), queue_tx.clone());
                    }
                    save_state(&app);
                }
                false
            }
            ev = ev_rx.recv_async() => {
                let Ok(ev) = ev else { break };
                handle_engine_event(&mut app, ev, &meta_tx);
                true
            }
            ev = in_rx.recv_async() => {
                match ev {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        let quit = handle_key(&mut app, key.code, key.modifiers, &lib_tx, &queue_tx, &search_tx, &detail_tx, &menu_tx, &astatus_tx, &radio_tx, &libdone_tx);
                        if quit {
                            save_state(&app);
                            break;
                        }
                    }
                    Ok(Event::Mouse(m)) => {
                        let quit = handle_mouse(&mut app, m, &lib_tx, &queue_tx, &search_tx, &detail_tx, &menu_tx, &astatus_tx, &radio_tx, &libdone_tx);
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
                    Some(ev) => handle_media_control_event(&mut app, ev, &radio_tx),
                    None => {
                        app.media_controls = None;
                        liblog("media controls event channel closed; native integration disabled");
                    }
                }
                true
            }
            m = meta_rx.recv_async() => {
                if let Ok(meta) = m { apply_meta(&mut app, meta, &lyrics_tx); }
                true
            }
            q = queue_rx.recv_async() => {
                // Don't let an empty live queue (e.g. a bare resumed track) wipe
                // the restored/last-known snapshot.
                if let Ok(q) = q {
                    if !q.is_empty() {
                        app.queue = q.iter().map(|(d, _)| d.clone()).collect();
                        app.queue_uris = q.into_iter().map(|(_, u)| u).collect();
                    }
                }
                true
            }
            s = search_rx.recv_async() => {
                if let Ok(results) = s {
                    app.search_results = results;
                    app.selected = app.first_selectable();
                    app.status = if app.search_results.is_empty() {
                        "no results".to_string()
                    } else {
                        String::new()
                    };
                }
                true
            }
            ly = lyrics_rx.recv_async() => {
                if let Ok((lines, synced)) = ly {
                    app.lyrics = lines;
                    app.lyrics_synced = synced;
                }
                true
            }
            d = detail_rx.recv_async() => {
                if let Ok((context_uri, title, items)) = d {
                    app.details.push(Detail { context_uri, title, items, parent_selected: app.selected });
                    app.selected = app.first_selectable();
                    app.status.clear();
                }
                true
            }
            menu = menu_rx.recv_async() => {
                if let Ok(mut menu) = menu {
                    // Enrich only an already-open menu (don't reopen a closed one),
                    // preserving the user's current selection across the swap.
                    if app.actions.is_some() && !menu.items.is_empty() {
                        if let Some(open) = app.actions.as_ref() {
                            menu.selected = open.selected.min(menu.items.len() - 1);
                        }
                        app.actions = Some(menu);
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
                    app.reclaimed = true;
                    app.shuffle = state.shuffle;
                    app.repeat = state.repeat;
                    app.volume = state.volume.min(100);
                    let _ = app.engine.set_volume(vol_u16(app.volume));
                    app.now = Some(NowPlaying {
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
                    let webapi = app.webapi.clone();
                    let tx = meta_tx.clone();
                    let id = state.track_id.clone();
                    app.pending_meta = Some(format!("spotify:track:{id}"));
                    tokio::task::spawn_blocking(move || { let _ = tx.send(fetch_track_meta(&webapi, &id)); });
                    spawn_queue_fetch(app.webapi.clone(), queue_tx.clone());
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
        .now
        .as_ref()
        .map(|n| n.uri.clone())
        .filter(|u| !u.is_empty());
    let pos = app.now.as_ref().map(|n| n.position_ms).unwrap_or(0);

    match app.source.clone() {
        PlaySource::Context(ctx) => {
            if let Err(e) = app.engine.play_context_at(ctx, track, pos, app.shuffle) {
                app.status = format!("couldn't play: {e:#}");
            }
        }
        PlaySource::Radio(seed) => {
            let session = app.engine.session();
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
        PlaySource::Liked if !app.library.liked.is_empty() => {
            let uris: Vec<String> = app.library.liked.iter().map(|i| i.uri.clone()).collect();
            if let Err(e) = app.engine.play_tracks(uris, track, pos, app.shuffle) {
                app.status = format!("couldn't play: {e:#}");
            }
        }
        _ => {
            // No known context — resume the last track followed by the saved
            // queue so playback actually continues past the first song.
            if !app.queue_uris.is_empty() {
                let mut uris = Vec::with_capacity(app.queue_uris.len() + 1);
                if let Some(u) = &track {
                    uris.push(u.clone());
                }
                uris.extend(app.queue_uris.iter().cloned());
                if let Err(e) = app.engine.play_tracks(uris, track, pos, app.shuffle) {
                    app.status = format!("couldn't play: {e:#}");
                }
            } else {
                match track {
                    Some(uri) => {
                        if let Err(e) = app.engine.play_track_at(uri, pos) {
                            app.status = format!("couldn't play: {e:#}");
                        }
                    }
                    None => {
                        if let Err(e) = app.engine.play() {
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
    let Some(item) = app.cur_items().get(app.selected).cloned() else {
        return;
    };
    match context_target(&item) {
        Some((uri, name)) => app.play_context_row(uri, name, shuffle),
        None => app.status = "not a playlist, album, or artist".to_string(),
    }
}

/// Mouse input. Mirrors `handle_key`: returns `true` when the app should quit.
#[allow(clippy::too_many_arguments)]
fn handle_mouse(
    app: &mut App,
    m: crossterm::event::MouseEvent,
    lib_tx: &flume::Sender<(Section, Vec<LibItem>)>,
    queue_tx: &flume::Sender<Vec<(String, String)>>,
    search_tx: &flume::Sender<Vec<LibItem>>,
    detail_tx: &flume::Sender<(String, String, Vec<LibItem>)>,
    menu_tx: &flume::Sender<ActionMenu>,
    astatus_tx: &flume::Sender<String>,
    radio_tx: &flume::Sender<Result<Radio, String>>,
    libdone_tx: &flume::Sender<bool>,
) -> bool {
    if matches!(
        m.kind,
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
    ) {
        let is_down = matches!(m.kind, MouseEventKind::Down(MouseButton::Left));
        let mut consumed = false;
        // Drag the sidebar scrollbar (2-col grab target) to scroll.
        if let Some(sb) = app.scroll_rect {
            if m.column + 1 >= sb.x
                && m.column <= sb.x
                && m.row >= sb.y
                && m.row < sb.y + sb.height
                && sb.height > 0
            {
                consumed = true;
                let total = app.scroll_len;
                if total > 1 {
                    let denom = sb.height.saturating_sub(1).max(1) as f32;
                    let frac = (m.row - sb.y) as f32 / denom;
                    let sel = (frac * (total - 1) as f32).round() as usize;
                    app.selected = sel.min(total - 1);
                    app.normalize_selection();
                }
            }
        }
        // Click/drag the volume meter bars to set volume.
        if !consumed {
            if let Some(vr) = app.vol_rect {
                if m.row == vr.y && m.column >= vr.x && m.column < vr.x + vr.width && vr.width > 0 {
                    consumed = true;
                    let offset = (m.column - vr.x) as u32;
                    let vol = (((offset + 1) * 100) / vr.width as u32).min(100) as u8;
                    app.volume = vol;
                    let _ = app.engine.set_volume(vol_u16(app.volume));
                }
            }
        }
        // Otherwise an initial click on the progress bar seeks.
        if !consumed && is_down {
            if let Some(bar) = app.bar_rect {
                if m.row == bar.y
                    && m.column >= bar.x
                    && m.column < bar.x + bar.width
                    && bar.width > 0
                {
                    if let Some(dur) = app.now.as_ref().map(|n| n.duration_ms) {
                        let frac = (m.column - bar.x) as f32 / bar.width as f32;
                        app.seek_to((frac * dur as f32) as u32);
                    }
                }
            }
        }
        // View-tab click -> switch the right pane.
        if !consumed && is_down {
            let hit = app
                .tab_rects
                .iter()
                .find(|(_, r)| m.row == r.y && m.column >= r.x && m.column < r.x + r.width)
                .map(|(v, _)| *v);
            if let Some(v) = hit {
                app.view = v;
                consumed = true;
            }
        }
        // Library click -> select; double-click (same row <400ms) -> activate.
        if !consumed && is_down {
            if let Some(lr) = app.lib_rect {
                if m.column >= lr.x
                    && m.column < lr.x + lr.width
                    && m.row >= lr.y
                    && m.row < lr.y + lr.height
                {
                    let idx = app.lib_offset + (m.row - lr.y) as usize;
                    let selectable = app
                        .cur_items()
                        .get(idx)
                        .map(|it| !it.is_header)
                        .unwrap_or(false);
                    if selectable {
                        app.selected = idx;
                        let now = Instant::now();
                        let dbl = app
                            .last_click
                            .map(|(r0, t0)| {
                                r0 == m.row && now.duration_since(t0) < Duration::from_millis(400)
                            })
                            .unwrap_or(false);
                        if dbl {
                            app.last_click = None;
                            let quit = handle_key(
                                app,
                                KeyCode::Enter,
                                KeyModifiers::empty(),
                                lib_tx,
                                queue_tx,
                                search_tx,
                                detail_tx,
                                menu_tx,
                                astatus_tx,
                                radio_tx,
                                libdone_tx,
                            );
                            if quit {
                                return true;
                            }
                        } else {
                            app.last_click = Some((m.row, now));
                        }
                    }
                }
            }
        }
    }
    // Scroll wheel → volume (anywhere in the window).
    if matches!(
        m.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        match m.kind {
            MouseEventKind::ScrollUp => {
                app.volume = (app.volume + 5).min(100);
                let _ = app.engine.set_volume(vol_u16(app.volume));
            }
            MouseEventKind::ScrollDown => {
                app.volume = app.volume.saturating_sub(5);
                let _ = app.engine.set_volume(vol_u16(app.volume));
            }
            _ => {}
        }
    }
    false
}

/// Returns true if the app should quit.
#[allow(clippy::too_many_arguments)]
fn handle_key(
    app: &mut App,
    code: KeyCode,
    mods: KeyModifiers,
    lib_tx: &flume::Sender<(Section, Vec<LibItem>)>,
    queue_tx: &flume::Sender<Vec<(String, String)>>,
    search_tx: &flume::Sender<Vec<LibItem>>,
    detail_tx: &flume::Sender<(String, String, Vec<LibItem>)>,
    menu_tx: &flume::Sender<ActionMenu>,
    astatus_tx: &flume::Sender<String>,
    radio_tx: &flume::Sender<Result<Radio, String>>,
    libdone_tx: &flume::Sender<bool>,
) -> bool {
    // --- Actions menu captures input while open ---
    // Double-press Ctrl-C to quit (works from anywhere). Single press arms it.
    if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
        let now = Instant::now();
        if app
            .last_ctrl_c
            .map(|t| now.duration_since(t) < Duration::from_millis(1500))
            .unwrap_or(false)
        {
            return true;
        }
        app.last_ctrl_c = Some(now);
        app.status = "press Ctrl-C again to quit".to_string();
        return false;
    }

    if app.actions.is_some() {
        handle_action_key(app, code, detail_tx, astatus_tx);
        return false;
    }

    // --- Search input mode captures everything ---
    if app.input_mode {
        match code {
            KeyCode::Esc => app.input_mode = false,
            KeyCode::Enter => {
                app.input_mode = false;
                let q = app.query.trim().to_string();
                if !q.is_empty() {
                    app.searching = true;
                    app.selected = 0;
                    app.status = "searching…".to_string();
                    spawn_search(app.webapi.clone(), q, search_tx.clone());
                }
            }
            KeyCode::Backspace => {
                app.query.pop();
            }
            KeyCode::Char(c) => app.query.push(c),
            _ => {}
        }
        return false;
    }

    // Zen hides the library, so the keys that drive one do nothing rather than
    // moving a selection nobody can see. Placed after the overlays above, which
    // stay usable if one was already open when zen came on.
    if app.zen && drives_library(code) {
        return false;
    }

    match code {
        KeyCode::Char('/') => {
            app.input_mode = true;
            app.query.clear();
        }
        KeyCode::Char('q') => return true,
        KeyCode::Esc => {
            if let Some(d) = app.details.pop() {
                app.selected = d.parent_selected;
            } else if app.searching {
                app.searching = false;
                app.selected = 0;
            }
            // Nothing to back out of — Esc no longer quits (use q or Ctrl-C twice).
        }
        KeyCode::Char(' ') | KeyCode::Char('p') | KeyCode::Media(MediaKeyCode::PlayPause) => {
            if app.playback_started {
                let _ = app.engine.toggle();
            } else if app.reclaimed {
                // Resume the reclaimed server-side context (full queue intact).
                let _ = app.engine.play();
                app.playback_started = true;
            } else {
                // No live session — resume the persisted source (context/radio/liked).
                resume_source(app, radio_tx);
                app.playback_started = true;
            }
        }
        KeyCode::Media(MediaKeyCode::Stop) => {
            app.engine.stop();
        }
        KeyCode::Char('n') | KeyCode::Media(MediaKeyCode::TrackNext) => {
            let _ = app.engine.next();
        }
        KeyCode::Char('b') | KeyCode::Media(MediaKeyCode::TrackPrevious) => {
            let _ = app.engine.prev();
        }
        KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Media(MediaKeyCode::RaiseVolume) => {
            app.volume = (app.volume + 5).min(100);
            let _ = app.engine.set_volume(vol_u16(app.volume));
        }
        KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Media(MediaKeyCode::LowerVolume) => {
            app.volume = app.volume.saturating_sub(5);
            let _ = app.engine.set_volume(vol_u16(app.volume));
        }
        KeyCode::Char('s') => {
            app.shuffle = !app.shuffle;
            let _ = app.engine.shuffle(app.shuffle);
        }
        // Play the highlighted playlist / album / artist outright. Enter still
        // opens; this is the direct route that used to require two Enters or
        // the actions menu.
        KeyCode::Char('P') => play_selected_context(app, false),
        KeyCode::Char('S') => {
            // Flip the global toggle too, or the footer would show shuffle off
            // while playback is shuffled, and `resume_source` would later
            // replay this context unshuffled.
            app.shuffle = true;
            let _ = app.engine.shuffle(true);
            play_selected_context(app, true);
        }
        KeyCode::Char('R') => {
            app.repeat = !app.repeat;
            let _ = app.engine.repeat(app.repeat);
        }
        KeyCode::Char('r') => {
            app.status = "loading library…".to_string();
            spawn_library_fetch(app.webapi.clone(), lib_tx.clone(), libdone_tx.clone());
        }
        KeyCode::Char('o') => {
            app.sort = app.sort.next();
            let m = app.sort;
            sort_list(app.cur_list_mut(), m);
            app.selected = app.first_selectable();
            app.status = format!("sorted by {}", m.label());
        }
        KeyCode::Char('a') => {
            // Zen hides the library, so the menu belongs to what is playing —
            // acting on a selection nobody can see is how it ends up offering
            // "remove from Liked" for the wrong track.
            let item = if app.zen {
                app.now
                    .as_ref()
                    .filter(|n| !n.uri.is_empty())
                    .map(|n| LibItem::track(n.title.clone(), n.artist.clone(), n.uri.clone()))
            } else {
                app.cur_items().get(app.selected).cloned()
            };
            if let Some(item) = item {
                if !item.is_header && !item.is_play {
                    // Instant menu (no network), then enrich when the API returns.
                    app.actions = Some(build_action_menu(None, &item));
                    spawn_action_menu(app.webapi.clone(), item, menu_tx.clone());
                }
            }
        }
        // Tab / Shift+Tab (and [ ]) rotate the library sections.
        KeyCode::Tab | KeyCode::Char(']') => {
            app.searching = false;
            app.section = app.section.shift(1);
            app.selected = app.first_selectable();
        }
        KeyCode::BackTab | KeyCode::Char('[') => {
            app.searching = false;
            app.section = app.section.shift(-1);
            app.selected = app.first_selectable();
        }
        // Arrow keys rotate the right-pane view; Shift+arrows seek ±5s.
        KeyCode::Right if mods.contains(KeyModifiers::SHIFT) => app.seek_step(SEEK_STEP_MS),
        KeyCode::Left if mods.contains(KeyModifiers::SHIFT) => app.seek_step(-SEEK_STEP_MS),
        KeyCode::Right => {
            app.view = app.view.shift(1);
            if app.view == RightView::Queue && (app.reclaimed || app.playback_started) {
                spawn_queue_fetch(app.webapi.clone(), queue_tx.clone());
            }
        }
        KeyCode::Left => {
            app.view = app.view.shift(-1);
            if app.view == RightView::Queue && (app.reclaimed || app.playback_started) {
                spawn_queue_fetch(app.webapi.clone(), queue_tx.clone());
            }
        }
        // The frame loop notices the layout change and wipes the art box.
        KeyCode::Char('z') => app.zen = !app.zen,
        KeyCode::Down | KeyCode::Char('j') => app.move_sel(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_sel(-1),
        // Needs a terminal that reports modified Enter (kitty, WezTerm, foot).
        KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => play_selected_context(app, false),
        KeyCode::Enter => match app.activate() {
            Activated::Open(uri, name) => {
                spawn_detail_fetch(app.webapi.clone(), uri, name, detail_tx.clone());
            }
            Activated::Radio(uri) => {
                app.status = "starting radio…".to_string();
                let session = app.engine.session();
                let tx = radio_tx.clone();
                tokio::spawn(async move {
                    let res = match tokio::time::timeout(
                        Duration::from_secs(12),
                        engine::radio_tracks(&session, &uri),
                    )
                    .await
                    {
                        Ok(r) => r.map_err(|e| e.to_string()),
                        Err(_) => {
                            Err("timed out (mercury radio endpoint unresponsive)".to_string())
                        }
                    };
                    let _ = tx.send(res.map(|uris| Radio {
                        uris,
                        start_position_ms: 0,
                    }));
                });
            }
            Activated::None => {}
        },
        _ => {}
    }
    false
}

/// Keys whose whole effect is on the library pane.
///
/// Zen hides that pane, so these do nothing there rather than moving a selection
/// nobody can see — and `a` is deliberately absent, because it retargets onto
/// the playing track instead of going quiet.
fn drives_library(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Char('/' | '[' | ']' | 'j' | 'k' | 'o' | 'r' | 'P' | 'S')
    )
}

fn consume_media_event<T>(event: Result<T, flume::RecvError>, open: &mut bool) -> Option<T> {
    match event {
        Ok(event) => Some(event),
        Err(_) => {
            *open = false;
            None
        }
    }
}

fn handle_media_control_event(
    app: &mut App,
    ev: MediaControlEvent,
    radio_tx: &flume::Sender<Result<Radio, String>>,
) {
    match ev {
        MediaControlEvent::Next => {
            let _ = app.engine.next();
        }
        MediaControlEvent::Previous => {
            let _ = app.engine.prev();
        }
        MediaControlEvent::Toggle => {
            if app.playback_started {
                let _ = app.engine.toggle();
            } else if app.reclaimed {
                // Resume the reclaimed server-side context (full queue intact).
                let _ = app.engine.play();
                app.playback_started = true;
            } else {
                // No live session — resume the persisted source (context/radio/liked).
                resume_source(app, radio_tx);
                app.playback_started = true;
            }
        }
        MediaControlEvent::Play => {
            if app.playback_started {
                let _ = app.engine.play();
            } else if app.reclaimed {
                // Resume the reclaimed server-side context (full queue intact).
                let _ = app.engine.play();
                app.playback_started = true;
            } else {
                // No live session — resume the persisted source (context/radio/liked).
                resume_source(app, radio_tx);
                app.playback_started = true;
            }
        }
        MediaControlEvent::Pause => {
            let _ = app.engine.pause();
        }
        MediaControlEvent::Stop => {
            app.engine.stop();
        }
        MediaControlEvent::Seek(direction) => match direction {
            SeekDirection::Backward => app.seek_step(-5_000),
            SeekDirection::Forward => app.seek_step(5_000),
        },
        MediaControlEvent::SeekBy(direction, duration) => match direction {
            SeekDirection::Backward => app.seek_step(-(duration.as_millis() as i64)),
            SeekDirection::Forward => app.seek_step(duration.as_millis() as i64),
        },
        MediaControlEvent::SetPosition(MediaPosition(duration)) => {
            app.seek_to(duration.as_millis() as u32);
        }
        _ => {}
    }
}

/// Handle input while the actions menu is open.
fn handle_action_key(
    app: &mut App,
    code: KeyCode,
    detail_tx: &flume::Sender<(String, String, Vec<LibItem>)>,
    astatus_tx: &flume::Sender<String>,
) {
    match code {
        KeyCode::Esc | KeyCode::Char('a') => {
            app.actions = None;
            app.status.clear();
            return;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(m) = app.actions.as_mut() {
                m.selected = m.selected.saturating_sub(1);
            }
            return;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(m) = app.actions.as_mut() {
                m.selected = (m.selected + 1).min(m.items.len().saturating_sub(1));
            }
            return;
        }
        KeyCode::Enter => {}
        _ => return,
    }

    // Enter: act on the selected entry.
    let kind = app
        .actions
        .as_ref()
        .and_then(|m| m.items.get(m.selected))
        .map(|i| i.kind.clone());
    let Some(kind) = kind else { return };
    match kind {
        ActionKind::AddToPlaylistMenu { track_uri } => {
            let items: Vec<ActionItem> = app
                .library
                .playlists
                .iter()
                .filter_map(|p| {
                    let id = p.uri.rsplit(':').next()?.to_string();
                    Some(ActionItem {
                        label: p.name.clone(),
                        kind: ActionKind::AddToPlaylist {
                            playlist_id: id,
                            track_uri: track_uri.clone(),
                        },
                    })
                })
                .collect();
            if items.is_empty() {
                app.status = "no playlists to add to".to_string();
                app.actions = None;
            } else {
                app.actions = Some(ActionMenu {
                    title: "Add to playlist".to_string(),
                    items,
                    selected: 0,
                });
            }
        }
        ActionKind::Play { uri, name } => {
            // Previously called engine.play_context directly, leaving
            // source/source_name stale: PLAYING FROM showed the wrong context
            // and resume-on-launch replayed the previous one.
            let shuffle = app.shuffle;
            app.play_context_row(uri, name, shuffle);
            app.actions = None;
        }
        ActionKind::Open { uri, name } => {
            spawn_detail_fetch(app.webapi.clone(), uri, name, detail_tx.clone());
            app.actions = None;
        }
        ActionKind::CopyLink { uri } => {
            app.status = if copy_to_clipboard(&uri_to_url(&uri)) {
                "link copied".to_string()
            } else {
                "clipboard unavailable".to_string()
            };
            app.actions = None;
        }
        other => {
            spawn_action(app.webapi.clone(), other, astatus_tx.clone());
            app.actions = None;
        }
    }
}

/// Copy text to the system clipboard via whatever tool is available.
fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let candidates: [(&str, &[&str]); 4] = [
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["-b", "-i"]),
        ("pbcopy", &[]),
    ];
    for (cmd, args) in candidates {
        if let Ok(mut child) = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut sin) = child.stdin.take() {
                let _ = sin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return true;
        }
    }
    false
}

fn spawn_action_menu(webapi: Arc<Mutex<WebApi>>, item: LibItem, tx: flume::Sender<ActionMenu>) {
    tokio::task::spawn_blocking(move || {
        if let Some(token) = token_of(&webapi) {
            let _ = tx.send(build_action_menu(Some(&token), &item));
        }
    });
}

/// Build the context menu for `item`, checking saved/following state and
/// resolving related artist/album links up front.
fn build_action_menu(token: Option<&str>, item: &LibItem) -> ActionMenu {
    let mut parts = item.uri.split(':');
    parts.next();
    let kind = parts.next().unwrap_or("");
    let id = parts.next().unwrap_or("").to_string();
    let uri = item.uri.clone();
    // Only build the blocking client for the enriched (Some token) path; the
    // instant (None) path runs on the async loop where dropping reqwest's inner
    // runtime would panic.
    let client = token.map(|_| http_client());
    let mut items = Vec::new();

    match kind {
        "track" => {
            let saved = token
                .map(|t| api_contains(t, &format!("spotify:track:{id}")))
                .unwrap_or(false);
            items.push(ActionItem {
                label: if saved {
                    "♥  Remove from Liked".into()
                } else {
                    "♡  Add to Liked".into()
                },
                kind: ActionKind::ToggleLike {
                    id: id.clone(),
                    saved,
                },
            });
            items.push(ActionItem {
                label: "＋  Add to Queue".into(),
                kind: ActionKind::Queue { uri: uri.clone() },
            });
            items.push(ActionItem {
                label: "≡  Add to Playlist…".into(),
                kind: ActionKind::AddToPlaylistMenu {
                    track_uri: uri.clone(),
                },
            });
            // Resolve the track's artist + album for "Go to" navigation.
            if let Some(v) = client
                .as_ref()
                .zip(token)
                .and_then(|(c, t)| get_json_cached(c, &format!("{API}/tracks/{id}"), t))
            {
                if let (Some(au), Some(an)) = (
                    v["artists"][0]["uri"].as_str(),
                    v["artists"][0]["name"].as_str(),
                ) {
                    items.push(ActionItem {
                        label: format!("→  Go to Artist ({an})"),
                        kind: ActionKind::Open {
                            uri: au.to_string(),
                            name: an.to_string(),
                        },
                    });
                }
                if let (Some(lu), Some(ln)) =
                    (v["album"]["uri"].as_str(), v["album"]["name"].as_str())
                {
                    items.push(ActionItem {
                        label: "→  Go to Album".into(),
                        kind: ActionKind::Open {
                            uri: lu.to_string(),
                            name: ln.to_string(),
                        },
                    });
                }
            }
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        "artist" => {
            let following = token
                .map(|t| api_contains(t, &format!("spotify:artist:{id}")))
                .unwrap_or(false);
            items.push(ActionItem {
                label: if following {
                    "Unfollow".into()
                } else {
                    "Follow".into()
                },
                kind: ActionKind::ToggleFollowArtist { id, following },
            });
            items.push(ActionItem {
                label: "▶︎  Play".into(),
                kind: ActionKind::Play {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "→  Open".into(),
                kind: ActionKind::Open {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        "album" => {
            let saved = token
                .map(|t| api_contains(t, &format!("spotify:album:{id}")))
                .unwrap_or(false);
            items.push(ActionItem {
                label: if saved {
                    "Remove from Library".into()
                } else {
                    "Save Album".into()
                },
                kind: ActionKind::ToggleSaveAlbum {
                    id: id.clone(),
                    saved,
                },
            });
            items.push(ActionItem {
                label: "▶︎  Play".into(),
                kind: ActionKind::Play {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "→  Open Album".into(),
                kind: ActionKind::Open {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            if let Some(v) = client
                .as_ref()
                .zip(token)
                .and_then(|(c, t)| get_json_cached(c, &format!("{API}/albums/{id}"), t))
            {
                if let (Some(au), Some(an)) = (
                    v["artists"][0]["uri"].as_str(),
                    v["artists"][0]["name"].as_str(),
                ) {
                    items.push(ActionItem {
                        label: format!("→  Go to Artist ({an})"),
                        kind: ActionKind::Open {
                            uri: au.to_string(),
                            name: an.to_string(),
                        },
                    });
                }
            }
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        "playlist" => {
            items.push(ActionItem {
                label: "＋  Add to Your Library".into(),
                kind: ActionKind::FollowPlaylist { id },
            });
            items.push(ActionItem {
                label: "▶︎  Play".into(),
                kind: ActionKind::Play {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "→  Open".into(),
                kind: ActionKind::Open {
                    uri: uri.clone(),
                    name: item.name.clone(),
                },
            });
            items.push(ActionItem {
                label: "⧉  Copy Link".into(),
                kind: ActionKind::CopyLink { uri },
            });
        }
        _ => {}
    }
    ActionMenu {
        title: item.name.clone(),
        items,
        selected: 0,
    }
}

fn spawn_action(webapi: Arc<Mutex<WebApi>>, kind: ActionKind, tx: flume::Sender<String>) {
    tokio::task::spawn_blocking(move || {
        let msg = match token_of(&webapi) {
            Some(t) => run_action(&t, kind),
            None => "not authorized".to_string(),
        };
        let _ = tx.send(msg);
    });
}

fn run_action(token: &str, kind: ActionKind) -> String {
    let client = http_client();
    match kind {
        ActionKind::ToggleLike { id, saved } => {
            match library_write(&client, token, saved, &format!("spotify:track:{id}")) {
                Ok(()) => {
                    if saved {
                        "removed from Liked".into()
                    } else {
                        "added to Liked \u{2665} (press r to refresh)".into()
                    }
                }
                Err(e) => format!("like failed: {e}"),
            }
        }
        ActionKind::Queue { uri } => {
            match api_modify(
                &client,
                token,
                "POST",
                &format!("{API}/me/player/queue?uri={}", urlencode(&uri)),
            ) {
                Ok(()) => "added to queue".into(),
                Err(e) => format!("queue failed: {e} (start playback first)"),
            }
        }
        ActionKind::AddToPlaylist {
            playlist_id,
            track_uri,
        } => {
            match api_modify(
                &client,
                token,
                "POST",
                &format!(
                    "{API}/playlists/{playlist_id}/items?uris={}",
                    urlencode(&track_uri)
                ),
            ) {
                Ok(()) => "added to playlist".into(),
                Err(e) => format!("add failed: {e}"),
            }
        }
        ActionKind::ToggleFollowArtist { id, following } => {
            match library_write(&client, token, following, &format!("spotify:artist:{id}")) {
                Ok(()) => {
                    if following {
                        "unfollowed".into()
                    } else {
                        "following".into()
                    }
                }
                Err(e) => format!("follow failed: {e}"),
            }
        }
        ActionKind::ToggleSaveAlbum { id, saved } => {
            match library_write(&client, token, saved, &format!("spotify:album:{id}")) {
                Ok(()) => {
                    if saved {
                        "removed album".into()
                    } else {
                        "saved album".into()
                    }
                }
                Err(e) => format!("album action failed: {e}"),
            }
        }
        ActionKind::FollowPlaylist { id } => {
            match library_write(&client, token, false, &format!("spotify:playlist:{id}")) {
                Ok(()) => "added to library".into(),
                Err(e) => format!("add failed: {e}"),
            }
        }
        _ => String::new(),
    }
}

/// Add or remove one library item — track, album, artist or playlist.
///
/// One endpoint for all four since February 2026; the per-type ones it replaced
/// are gone and answer 403. `uris` must go in the query string: a JSON body is
/// rejected as missing it.
fn library_write(
    client: &reqwest::blocking::Client,
    token: &str,
    remove: bool,
    uri: &str,
) -> Result<(), String> {
    api_modify(
        client,
        token,
        if remove { "DELETE" } else { "PUT" },
        &format!("{API}/me/library?uris={}", urlencode(uri)),
    )
}

/// Returns Ok on 2xx, else a short reason (HTTP status / network) so the UI can
/// say WHY instead of a generic "action failed". Retries once on 429.
fn api_modify(
    client: &reqwest::blocking::Client,
    token: &str,
    method: &str,
    url: &str,
) -> Result<(), String> {
    for attempt in 0..2 {
        let req = match method {
            "PUT" => client.put(url),
            "DELETE" => client.delete(url),
            _ => client.post(url),
        };
        match req.bearer_auth(token).header("Content-Length", "0").send() {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) if r.status().as_u16() == 429 && attempt == 0 => {
                let wait = r
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1)
                    .min(5);
                std::thread::sleep(Duration::from_secs(wait));
            }
            Ok(r) => return Err(format!("HTTP {}", r.status().as_u16())),
            Err(e) => {
                return Err(if e.is_timeout() {
                    "timeout".into()
                } else {
                    "network error".into()
                })
            }
        }
    }
    Err("rate limited".into())
}

/// Is `uri` in the user's library? Replaced the `/me/*/contains` family in
/// February 2026; those are gone and answer 403.
fn api_contains(token: &str, uri: &str) -> bool {
    let client = http_client();
    let url = format!("{API}/me/library/contains?uris={}", urlencode(uri));
    get_json(&client, &url, token)
        .and_then(|v| v.get(0).and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// Snapshot the current session to disk (volume, last track, position, queue).
fn save_state(app: &App) {
    let last_played = app.now.as_ref().map(|now| LastPlayed {
        uri: now.uri.clone(),
        title: now.title.clone(),
        artist: now.artist.clone(),
        album: now.album.clone(),
        duration_ms: now.duration_ms,
        position_ms: app.position_ms(),
    });

    let s = SavedState {
        volume: app.volume,
        shuffle: app.shuffle,
        repeat: app.repeat,
        last_played,
        queue: app.queue.clone(),
        queue_uris: app.queue_uris.clone(),
        source: app.source.clone(),
        source_name: app.source_name.clone(),
    };
    s.save();
}

fn advance_fade(app: &mut App) {
    if let Some(fade) = &app.fade {
        app.displayed = fade.current();
        if fade.is_done() {
            app.displayed = app.target;
            app.fade = None;
        }
    }
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
                app.pending_meta = Some(format!("spotify:track:{track_id}"));
                let webapi = app.webapi.clone();
                let tx = meta_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(fetch_track_meta(&webapi, &track_id));
                });
            }
        }
        EngineEvent::Playing { position_ms, .. } => {
            if !app.playback_started {
                app.playback_started = true;
                // Reapply persisted modes + volume to the freshly-started playback.
                let _ = app.engine.shuffle(app.shuffle);
                let _ = app.engine.repeat(app.repeat);
                let _ = app.engine.set_volume(vol_u16(app.volume));
            }
            if let Some(n) = app.now.as_mut() {
                n.is_playing = true;
            }
            app.set_local_position(position_ms, true);
            if let Some(controls) = app.media_controls.as_mut() {
                let _ = controls.set_playback(MediaPlayback::Playing {
                    progress: Some(MediaPosition(Duration::from_millis(position_ms as u64))),
                });
            }
        }
        EngineEvent::Paused { position_ms, .. } => {
            if let Some(n) = app.now.as_mut() {
                n.is_playing = false;
            }
            app.set_local_position(position_ms, true);
            if let Some(controls) = app.media_controls.as_mut() {
                let _ = controls.set_playback(MediaPlayback::Paused {
                    progress: Some(MediaPosition(Duration::from_millis(position_ms as u64))),
                });
            }
        }
        EngineEvent::Stopped => {
            app.now = None;
            app.playback_started = false;
            // librespot cleared its context too, so only a fresh load can
            // resume from here — not a bare `play`.
            app.reclaimed = false;

            if let Some(controls) = app.media_controls.as_mut() {
                let _ = controls.set_playback(MediaPlayback::Stopped);
            }
        }
        EngineEvent::PositionCorrection { position_ms, .. } => {
            app.set_local_position(position_ms, true);
            if let (Some(n), Some(controls)) = (app.now.as_ref(), app.media_controls.as_mut()) {
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
            app.status = if app.playback_started {
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
    if !meta_is_current(app.pending_meta.as_deref(), &meta.uri) {
        return;
    }

    let cover = meta
        .image
        .image
        .as_ref()
        .map(|img| Cover::from_image(img.clone(), app.picker.clone()));
    // A different cover encodes to a different symbol, so the diff emits it on
    // its own — no wipe, which would flash a blank box between the two covers.
    app.art_repaint = ArtRepaint::Draw;
    app.status.clear();
    app.lyrics.clear();
    app.lyrics_synced = false;

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

    app.now = Some(NowPlaying {
        uri: meta.uri,
        title: meta.title,
        artist: meta.artist,
        album: meta.album,
        duration_ms: meta.duration_ms,
        position_ms: app.now.as_ref().map(|n| n.position_ms).unwrap_or(0),
        position_at: Instant::now(),
        is_playing: app
            .now
            .as_ref()
            .map(|n| n.is_playing)
            .unwrap_or(app.playback_started),
        cover,
    });

    if let Some(theme) = meta.theme {
        app.start_fade(theme);
    }

    if let Some(controls) = app.media_controls.as_mut() {
        let _ = controls.set_metadata(MediaMetadata {
            title: app.now.as_ref().map(|n| n.title.as_str()),
            artist: app.now.as_ref().map(|n| n.artist.as_str()),
            album: app.now.as_ref().map(|n| n.album.as_str()),
            cover_url: meta.image.url.as_deref(),
            duration: app
                .now
                .as_ref()
                .map(|n| Duration::from_millis(n.duration_ms as u64)),
        });
    }
}

// ------------------------------------------------------------------ web api

fn token_of(webapi: &Arc<Mutex<WebApi>>) -> Option<String> {
    // Refresh when the token is expiring so long sessions don't silently go
    // read-only (audit H1). Only holds the lock across the network call in the
    // rare refresh window; otherwise this is just a cheap clone.
    let token = {
        let mut w = webapi.lock().ok()?;
        match w.valid_token() {
            Ok(t) => t,
            Err(_) => w.cached_token(),
        }
    };
    (!token.is_empty()).then_some(token)
}

/// Base URL every Spotify Web API call is built from.
const API: &str = "https://api.spotify.com/v1";

/// How long to wait before retrying a 429, or `None` to give up now.
///
/// Spotify hands development-mode apps hour-long `Retry-After` values once a
/// quota is spent, and sleeping on those froze a drill-in for minutes before
/// failing anyway. Only brief backoffs are worth waiting out.
fn retry_delay(retry_after: Option<u64>) -> Option<Duration> {
    match retry_after.unwrap_or(3) {
        secs if secs <= 5 => Some(Duration::from_secs(secs + 1)),
        _ => None,
    }
}

/// GET a JSON endpoint, retrying on 429 (respecting Retry-After).
fn get_json(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
) -> Option<serde_json::Value> {
    for _ in 0..5 {
        let resp = match client.get(url).bearer_auth(token).send() {
            Ok(r) => r,
            Err(e) => {
                liblog(format!("api: {url} transport error: {e}"));
                return None;
            }
        };
        if resp.status().as_u16() == 429 {
            let after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            let Some(wait) = retry_delay(after) else {
                liblog(format!(
                    "api: {url} -> 429, retry-after {after:?}s, giving up"
                ));
                return None;
            };
            std::thread::sleep(wait);
            continue;
        }
        if !resp.status().is_success() {
            // Swallowing the status made a dead endpoint look like an empty one.
            liblog(format!("api: {url} -> HTTP {}", resp.status().as_u16()));
            return None;
        }
        return resp.json::<serde_json::Value>().ok();
    }
    None
}

/// How long a cached catalogue response counts as fresh. Discographies and
/// track lists change on the order of weeks; a day keeps browsing off the
/// network without anyone noticing the lag.
const CATALOGUE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// `get_json` for catalogue reads, served from disk when possible.
///
/// On a failed request an expired entry is used rather than nothing — that is
/// the case that matters, since a spent quota is exactly when the network stops
/// answering and the artist page would otherwise come up empty.
fn get_json_cached(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
) -> Option<serde_json::Value> {
    if let Some(body) = myx::httpcache::get(url, Some(CATALOGUE_TTL)) {
        return serde_json::from_str(&body).ok();
    }
    match get_json(client, url, token) {
        Some(v) => {
            myx::httpcache::put(url, &v.to_string());
            Some(v)
        }
        None => {
            let stale = myx::httpcache::get(url, None)?;
            liblog(format!("api: {url} failed; serving cached copy"));
            serde_json::from_str(&stale).ok()
        }
    }
}

/// Album art bytes, from disk when they've been seen before.
fn fetch_cover(client: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = myx::httpcache::get_bytes(url) {
        return Some(bytes);
    }
    let resp = client.get(url).send().ok()?;
    // An error page cached as image bytes would be a permanently broken cover:
    // the entry never expires, because the URL only changes with the picture.
    if !resp.status().is_success() {
        liblog(format!("cover: {url} -> HTTP {}", resp.status().as_u16()));
        return None;
    }
    let bytes = resp.bytes().ok()?.to_vec();
    myx::httpcache::put_bytes(url, &bytes);
    Some(bytes)
}

/// Fetch the library incrementally: fast sections first, Liked streamed in
/// chunks so the UI is usable within ~1s instead of waiting for everything.
fn spawn_library_fetch(
    webapi: Arc<Mutex<WebApi>>,
    tx: flume::Sender<(Section, Vec<LibItem>)>,
    done_tx: flume::Sender<bool>,
) {
    // Clone the already-refreshed access token BEFORE spawning. This removes the
    // shared WebApi mutex from the worker entirely (the stuck-library root cause).
    let token_opt = token_of(&webapi);
    liblog(format!(
        "spawn_library_fetch: token={}",
        token_opt.as_ref().map_or("missing", |_| "ok")
    ));
    std::thread::Builder::new()
        .name("myx-library".to_string())
        .spawn(move || {
            liblog("worker: entered");
            let Some(token) = token_opt else {
                liblog("worker: no token; aborting");
                let _ = done_tx.send(false);
                return;
            };

            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(12))
                .build()
                .expect("build library HTTP client");
            let mut got_any = false;
            let track_from = |t: &serde_json::Value| -> Option<LibItem> {
                Some(LibItem::track(
                    t["name"].as_str()?.to_string(),
                    t["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                    t["uri"].as_str()?.to_string(),
                ))
            };
            let artist_from = |a: &serde_json::Value| -> Option<LibItem> {
                Some(LibItem::ctx(
                    a["name"].as_str()?.to_string(),
                    String::new(),
                    a["uri"].as_str()?.to_string(),
                ))
            };
            let album_from = |a: &serde_json::Value| -> Option<LibItem> {
                Some(LibItem::ctx(
                    a["name"].as_str()?.to_string(),
                    format!("album · {}", a["artists"][0]["name"].as_str().unwrap_or("")),
                    a["uri"].as_str()?.to_string(),
                ))
            };

            // Home: a curated mix — recently played, top tracks, top artists, new releases.
            liblog("worker: fetching home/recent");
            let mut home: Vec<LibItem> = Vec::new();
            let recent5 = fetch_all_pages(
                &client,
                &format!("{API}/me/player/recently-played?limit=10"),
                &token,
                None,
                1,
                |it| track_from(&it["track"]),
            );
            if !recent5.is_empty() {
                home.push(LibItem::header("Recently Played"));
                home.extend(recent5.into_iter().take(6));
            }
            let top_tracks = fetch_all_pages(
                &client,
                &format!("{API}/me/top/tracks?limit=10"),
                &token,
                None,
                1,
                |t| track_from(t),
            );
            if !top_tracks.is_empty() {
                home.push(LibItem::header("Your Top Tracks"));
                home.extend(top_tracks.into_iter().take(8));
            }
            let top_artists = fetch_all_pages(
                &client,
                &format!("{API}/me/top/artists?limit=10"),
                &token,
                None,
                1,
                |a| artist_from(a),
            );
            if !top_artists.is_empty() {
                home.push(LibItem::header("Your Top Artists"));
                home.extend(top_artists.into_iter().take(6));
            }
            // `/browse/new-releases` was removed in February 2026.
            got_any |= !home.is_empty();
            liblog(format!("worker: home done, {} rows", home.len()));
            let _ = tx.send((Section::Home, home));

            let recent = fetch_all_pages(
                &client,
                &format!("{API}/me/player/recently-played?limit=50"),
                &token,
                None,
                1,
                |it| track_from(&it["track"]),
            );
            got_any |= !recent.is_empty();
            let _ = tx.send((Section::Recent, recent));

            let playlists = fetch_all_pages(
                &client,
                &format!("{API}/me/playlists?limit=50"),
                &token,
                None,
                10,
                |it| {
                    Some(LibItem::ctx(
                        it["name"].as_str()?.to_string(),
                        playlist_subtitle(
                            it["owner"]["display_name"].as_str().unwrap_or(""),
                            playlist_total(it),
                        ),
                        it["uri"].as_str()?.to_string(),
                    ))
                },
            );
            got_any |= !playlists.is_empty();
            let _ = tx.send((Section::Playlists, playlists));

            let albums = fetch_all_pages(
                &client,
                &format!("{API}/me/albums?limit=50"),
                &token,
                None,
                10,
                |it| album_from(&it["album"]),
            );
            got_any |= !albums.is_empty();
            let _ = tx.send((Section::Albums, albums));

            let artists = fetch_all_pages(
                &client,
                &format!("{API}/me/following?type=artist&limit=50"),
                &token,
                Some("artists"),
                5,
                |it| artist_from(it),
            );
            got_any |= !artists.is_empty();
            let _ = tx.send((Section::Artists, artists));

            // Liked can be huge — stream it in as pages arrive so the count climbs live.
            // Prepend Shuffle/Play action rows (shuffle first).
            let mut liked: Vec<LibItem> = vec![
                LibItem::play("▶︎  Play Liked Songs".into(), "myx:action:liked-play".into()),
                LibItem::header("Songs"),
            ];
            let mut url = Some(format!("{API}/me/tracks?limit=50"));
            let mut pages = 0;
            while let Some(u) = url.take() {
                if pages >= 100 {
                    break;
                }
                let Some(v) = get_json(&client, &u, &token) else {
                    break;
                };
                for it in v["items"].as_array().into_iter().flatten() {
                    if let Some(li) = track_from(&it["track"]) {
                        liked.push(li);
                    }
                }
                url = v["next"].as_str().map(String::from);
                pages += 1;
                if pages % 3 == 0 {
                    let _ = tx.send((Section::Liked, liked.clone()));
                }
            }
            got_any |= liked.len() > 2; // beyond the two action rows
            let _ = tx.send((Section::Liked, liked));

            liblog(format!("worker: all done got_any={got_any}"));
            let _ = done_tx.send(got_any);
        })
        .expect("spawn library worker");
}

/// One `/playlists/{id}/items` entry -> `LibItem`.
///
/// The payload nests the track under `item`; the older `/tracks` endpoint used
/// `track`. Both are accepted so this keeps working whichever shape is served.
///
/// `None` skips the row (`fetch_all_pages` filters rather than aborting), which
/// is what we want for entries with no playable track: `null` for items removed
/// from the catalogue, and region-locked or malformed rows.
fn parse_playlist_track(it: &serde_json::Value) -> Option<LibItem> {
    let t = if it["item"].is_object() {
        &it["item"]
    } else {
        &it["track"]
    };
    Some(LibItem::track(
        t["name"].as_str()?.to_string(),
        t["artists"][0]["name"].as_str().unwrap_or("").to_string(),
        t["uri"].as_str()?.to_string(),
    ))
}

/// Track count from a playlist object. Spotify renamed the field `tracks` ->
/// `items` alongside the `/tracks` -> `/items` endpoint move; read the new name
/// first and fall back so both shapes work.
fn playlist_total(p: &serde_json::Value) -> Option<u64> {
    p["items"]["total"]
        .as_u64()
        .or_else(|| p["tracks"]["total"].as_u64())
}

/// Playlist row subtitle: `"142 · owner"`, or just the owner when the API omits
/// the count.
///
/// Count first, deliberately: the row renderer truncates the subtitle tail-first
/// in a narrow pane, and the count is both short and the more informative half —
/// the owner is frequently the same name on every row.
fn playlist_subtitle(owner: &str, total: Option<u64>) -> String {
    match total {
        Some(n) if owner.is_empty() => n.to_string(),
        Some(n) => format!("{n} · {owner}"),
        None => owner.to_string(),
    }
}

fn fetch_all_pages(
    client: &reqwest::blocking::Client,
    first_url: &str,
    token: &str,
    nested: Option<&str>,
    max_pages: usize,
    parse: impl Fn(&serde_json::Value) -> Option<LibItem>,
) -> Vec<LibItem> {
    let mut out = Vec::new();
    let mut url = Some(first_url.to_string());
    let mut pages = 0;
    while let Some(u) = url.take() {
        if pages >= max_pages {
            break;
        }
        let Some(v) = get_json(client, &u, token) else {
            break;
        };
        let node = match nested {
            Some(k) => &v[k],
            None => &v,
        };
        for it in node["items"].as_array().into_iter().flatten() {
            if let Some(li) = parse(it) {
                out.push(li);
            }
        }
        url = node["next"].as_str().map(String::from);
        pages += 1;
    }
    out
}

fn spawn_queue_fetch(webapi: Arc<Mutex<WebApi>>, tx: flume::Sender<Vec<(String, String)>>) {
    tokio::task::spawn_blocking(move || {
        let q = match token_of(&webapi) {
            Some(token) => fetch_queue_blocking(&token),
            None => Vec::new(),
        };
        let _ = tx.send(q);
    });
}

/// Returns (display, uri) for each queued item.
fn fetch_queue_blocking(token: &str) -> Vec<(String, String)> {
    let client = http_client();
    let Some(v) = get_json(&client, &format!("{API}/me/player/queue"), token) else {
        return Vec::new();
    };
    v["queue"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|it| {
                    let name = it["name"].as_str()?;
                    let uri = it["uri"].as_str()?.to_string();
                    let artist = it["artists"][0]["name"].as_str().unwrap_or("");
                    Some((format!("{name} — {artist}"), uri))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn fetch_track_meta(webapi: &Arc<Mutex<WebApi>>, track_id: &str) -> TrackMeta {
    let uri = format!("spotify:track:{track_id}");
    let empty = || TrackMeta {
        uri: uri.clone(),
        title: String::new(),
        artist: String::new(),
        album: String::new(),
        duration_ms: 0,
        image: TrackImage {
            image: None,
            url: None,
        },
        theme: None,
    };
    let Some(token) = token_of(webapi) else {
        return empty();
    };
    let client = http_client();
    let Some(v) = get_json_cached(&client, &format!("{API}/tracks/{track_id}"), &token) else {
        return empty();
    };

    let title = v["name"].as_str().unwrap_or("").to_string();
    let artist = v["artists"][0]["name"].as_str().unwrap_or("").to_string();
    let album = v["album"]["name"].as_str().unwrap_or("").to_string();
    let duration_ms = v["duration_ms"].as_u64().unwrap_or(0) as u32;
    let cover_url = v["album"]["images"][0]["url"].as_str().map(String::from);

    let image = cover_url
        .clone()
        .and_then(|u| fetch_cover(&client, &u))
        .and_then(|bytes| image::load_from_memory(&bytes).ok());
    let theme = image.as_ref().map(|img| derive_theme(img, "album ✦"));

    TrackMeta {
        uri,
        title,
        artist,
        album,
        duration_ms,
        image: TrackImage {
            image,
            url: cover_url,
        },
        theme,
    }
}

// --- Search ---

fn spawn_search(webapi: Arc<Mutex<WebApi>>, query: String, tx: flume::Sender<Vec<LibItem>>) {
    tokio::task::spawn_blocking(move || {
        let results = match token_of(&webapi) {
            Some(token) => search_blocking(&token, &query),
            None => Vec::new(),
        };
        let _ = tx.send(results);
    });
}

fn search_blocking(token: &str, query: &str) -> Vec<LibItem> {
    let client = http_client();
    let url = format!(
        "{API}/search?q={}&type=track,artist,album,playlist&limit=6",
        urlencode(query)
    );
    let Some(v) = get_json(&client, &url, token) else {
        return Vec::new();
    };

    let mut out = Vec::new();

    let songs: Vec<LibItem> = v["tracks"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some(LibItem::track(
                t["name"].as_str()?.to_string(),
                t["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                t["uri"].as_str()?.to_string(),
            ))
        })
        .collect();
    let artists: Vec<LibItem> = v["artists"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            Some(LibItem::ctx(
                a["name"].as_str()?.to_string(),
                String::new(),
                a["uri"].as_str()?.to_string(),
            ))
        })
        .collect();
    let albums: Vec<LibItem> = v["albums"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|al| {
            Some(LibItem::ctx(
                al["name"].as_str()?.to_string(),
                al["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                al["uri"].as_str()?.to_string(),
            ))
        })
        .collect();
    let playlists: Vec<LibItem> = v["playlists"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(LibItem::ctx(
                p["name"].as_str()?.to_string(),
                playlist_subtitle(
                    p["owner"]["display_name"].as_str().unwrap_or(""),
                    playlist_total(p),
                ),
                p["uri"].as_str()?.to_string(),
            ))
        })
        .collect();

    for (title, group) in [
        ("Songs", songs),
        ("Artists", artists),
        ("Albums", albums),
        ("Playlists", playlists),
    ] {
        if !group.is_empty() {
            out.push(LibItem::header(title));
            out.extend(group);
        }
    }
    out
}

// --- Lyrics (lrclib) ---

fn fetch_lyrics_blocking(
    artist: &str,
    title: &str,
    album: &str,
    duration_ms: u32,
) -> (Vec<(u32, String)>, bool) {
    let client = http_client();
    let url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}&album_name={}&duration={}",
        urlencode(artist),
        urlencode(title),
        urlencode(album),
        duration_ms / 1000
    );
    let Ok(resp) = client
        .get(&url)
        .header("User-Agent", "myx (terminal spotify player)")
        .send()
    else {
        return (Vec::new(), false);
    };
    if !resp.status().is_success() {
        return (Vec::new(), false);
    }
    let Ok(v) = resp.json::<serde_json::Value>() else {
        return (Vec::new(), false);
    };

    if let Some(synced) = v["syncedLyrics"].as_str().filter(|s| !s.is_empty()) {
        return (parse_lrc(synced), true);
    }
    if let Some(plain) = v["plainLyrics"].as_str().filter(|s| !s.is_empty()) {
        let lines = plain.lines().map(|l| (0u32, l.to_string())).collect();
        return (lines, false);
    }
    (Vec::new(), false)
}

// --- Drill-in detail (artist / album / playlist) ---

fn spawn_detail_fetch(
    webapi: Arc<Mutex<WebApi>>,
    uri: String,
    name: String,
    tx: flume::Sender<(String, String, Vec<LibItem>)>,
) {
    tokio::task::spawn_blocking(move || {
        if let Some(token) = token_of(&webapi) {
            let (title, items) = fetch_detail_blocking(&token, &uri, &name);
            let _ = tx.send((uri, title, items));
        }
    });
}

/// An artist's most popular tracks. `/artists/{id}/top-tracks` was removed in
/// February 2026, so this searches instead — also popularity-ranked, ten max.
fn artist_top_tracks(
    client: &reqwest::blocking::Client,
    token: &str,
    id: &str,
    name: &str,
) -> Vec<LibItem> {
    let url = format!(
        "{API}/search?q={}&type=track&limit=10",
        urlencode(&format!("artist:{name}"))
    );
    let Some(v) = get_json_cached(client, &url, token) else {
        return Vec::new();
    };
    v["tracks"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        // Search matches loosely — keep only tracks this artist is credited on.
        .filter(|t| {
            t["artists"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x["id"].as_str() == Some(id)))
        })
        .filter_map(|t| {
            Some(LibItem::track(
                t["name"].as_str()?.to_string(),
                t["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                t["uri"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// An artist's albums and singles, deduped by name, newest first. Paged ten at
/// a time — since February 2026 a larger page is a 400 "Invalid limit".
fn artist_albums(client: &reqwest::blocking::Client, token: &str, id: &str) -> Vec<LibItem> {
    let mut seen = std::collections::HashSet::new();
    let mut albums: Vec<(String, LibItem)> = Vec::new();
    let mut url = Some(format!(
        "{API}/artists/{id}/albums?include_groups=album,single&limit=10"
    ));
    let mut pages = 0;
    while let Some(u) = url.take() {
        if pages >= 3 {
            break; // 30 releases; more pages burn the API quota per drill-in
        }
        let Some(v) = get_json_cached(client, &u, token) else {
            break;
        };
        for a in v["items"].as_array().into_iter().flatten() {
            let (Some(aname), Some(auri)) = (a["name"].as_str(), a["uri"].as_str()) else {
                continue;
            };
            if !seen.insert(aname.to_lowercase()) {
                continue;
            }
            let date = a["release_date"].as_str().unwrap_or("").to_string();
            let year = date.split('-').next().unwrap_or("").to_string();
            albums.push((
                date,
                LibItem::ctx(aname.to_string(), year, auri.to_string()),
            ));
        }
        url = v["next"].as_str().map(String::from);
        pages += 1;
    }
    albums.sort_by(|x, y| y.0.cmp(&x.0)); // newest first
    albums.into_iter().map(|(_, it)| it).collect()
}

fn fetch_detail_blocking(token: &str, uri: &str, name: &str) -> (String, Vec<LibItem>) {
    let client = http_client();
    let mut parts = uri.split(':');
    parts.next(); // "spotify"
    let kind = parts.next().unwrap_or("");
    let id = parts.next().unwrap_or("");

    // "Play all" row first.
    let mut items = vec![LibItem::play(format!("▶︎ Play {name}"), uri.to_string())];

    liblog(format!("detail: kind={kind} id={id} uri={uri}"));
    match kind {
        "artist" => {
            let tracks = artist_top_tracks(&client, token, id, name);
            if !tracks.is_empty() {
                items.push(LibItem::header("Popular"));
                items.extend(tracks);
            }
            let albums = artist_albums(&client, token, id);
            if !albums.is_empty() {
                items.push(LibItem::header("Albums"));
                items.extend(albums);
            }
            if items.len() == 1 {
                // Only the play row: every request failed, most often a spent
                // API quota. Say so instead of showing a bare button.
                items.push(LibItem::header("nothing loaded — API error or quota"));
            }
        }
        "album" => {
            if let Some(v) = get_json_cached(
                &client,
                &format!("{API}/albums/{id}/tracks?limit=50"),
                token,
            ) {
                for t in v["items"].as_array().into_iter().flatten() {
                    if let (Some(n), Some(u)) = (t["name"].as_str(), t["uri"].as_str()) {
                        items.push(LibItem::track(
                            n.to_string(),
                            t["artists"][0]["name"].as_str().unwrap_or("").to_string(),
                            u.to_string(),
                        ));
                    }
                }
            }
        }
        "playlist" => {
            // Follow `next` instead of taking only the first page: playlists
            // routinely exceed the 100-item page size, and the drill-in list
            // (plus "play from this track") was silently truncated.
            let before = items.len();
            items.extend(fetch_all_pages(
                &client,
                &format!("{API}/playlists/{id}/items?limit=100"),
                token,
                None, // items[] is top-level on this endpoint
                10,   // 1,000 tracks, matching the other sections' ceiling
                parse_playlist_track,
            ));
            if items.len() == before {
                // Some third-party playlists 403 even on /items. `fetch_all_pages`
                // has no error channel, so an empty result is indistinguishable
                // from an empty playlist — say both rather than showing a blank
                // pane with no explanation.
                items.push(LibItem::header("no tracks — empty or restricted"));
            }
        }
        _ => {}
    }

    liblog(format!("detail: {} rows for {uri}", items.len()));
    (name.to_string(), items)
}

// --- Live playback state (server-side) ---

/// The current playback as Spotify remembers it (across devices).
struct PlaybackState {
    track_id: String,
    progress_ms: u32,
    shuffle: bool,
    repeat: bool,
    volume: u8,
}

fn fetch_playback_state(token: &str) -> Option<PlaybackState> {
    let client = http_client();
    let resp = client
        .get(format!("{API}/me/player"))
        .bearer_auth(token)
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None; // 204 = nothing playing recently
    }
    let v: serde_json::Value = resp.json().ok()?;
    let track_id = v["item"]["id"].as_str()?.to_string();
    Some(PlaybackState {
        track_id,
        progress_ms: v["progress_ms"].as_u64().unwrap_or(0) as u32,
        shuffle: v["shuffle_state"].as_bool().unwrap_or(false),
        repeat: v["repeat_state"]
            .as_str()
            .map(|r| r != "off")
            .unwrap_or(false),
        volume: v["device"]["volume_percent"].as_u64().unwrap_or(50) as u8,
    })
}

/// Transfer the current server-side playback onto the myx device (with its full
/// context + queue + position). `play=false` transfers paused.
/// `Ok(())` on success, `Err(reason)` with something worth showing otherwise.
fn transfer_playback(token: &str, device_id: &str, play: bool) -> Result<(), String> {
    let client = http_client();
    match client
        .put(format!("{API}/me/player"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "device_ids": [device_id], "play": play }))
        .send()
    {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => {
            let code = r.status().as_u16();
            let body = r.text().unwrap_or_default();
            liblog(format!("transfer -> HTTP {code}: {body}"));
            Err(format!("HTTP {code}"))
        }
        Err(e) => {
            liblog(format!("transfer failed: {e}"));
            Err("network error".to_string())
        }
    }
}

/// Boot restore: read the live playback state, transfer it onto myx (retrying
/// while the device registers), and hand the state back to the UI.
fn spawn_restore(webapi: Arc<Mutex<WebApi>>, device_id: String, tx: flume::Sender<PlaybackState>) {
    tokio::task::spawn_blocking(move || {
        let Some(token) = token_of(&webapi) else {
            return;
        };
        let Some(state) = fetch_playback_state(&token) else {
            return;
        };
        // Retry the transfer — the Connect device can take a moment to appear.
        for _ in 0..6 {
            if transfer_playback(&token, &device_id, false).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        let _ = tx.send(state);
    });
}

// --- Live playback state (server-side) end ---

// ------------------------------------------------------------------ render

fn render(f: &mut Frame, app: &mut App) {
    let theme = app.displayed;
    let area = f.area();
    f.render_widget(Block::default().style(theme.base()), area);
    let area = area.inner(Margin::new(2, 1));

    let rows = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Length(1), // spacer
        Constraint::Min(6),    // body (library | active view)
        Constraint::Length(1), // spacer
        Constraint::Length(2), // now-playing strip
        Constraint::Length(1), // footer
    ])
    .split(area);

    // Header: wordmark + view tabs (right-aligned) + status.
    // Fullwidth wordmark (each letter = 2 cells) reads as a bigger "myx"
    // than the terminal font allows on a single row; bolded for weight.
    let mut header: Vec<Span> =
        gradient_line("\u{FF2D}\u{FF39}\u{FF38}", &[theme.primary, theme.accent])
            .into_iter()
            .map(|mut sp| {
                sp.style = sp.style.add_modifier(Modifier::BOLD);
                sp
            })
            .collect();
    if !app.status.is_empty() {
        header.push(Span::styled(format!("   {}", app.status), theme.muted()));
    }
    f.render_widget(Paragraph::new(Line::from(header)), rows[0]);
    f.render_widget(
        Paragraph::new(Line::from(view_tabs(app, theme))).alignment(Alignment::Right),
        rows[0],
    );
    // Per-tab hit rects for the mouse (mirrors view_tabs: "\u2190\u2192 " prefix + labels joined by " \u00b7 ").
    let mut total: usize = 3; // "\u2190\u2192 "
    for (i, v) in RightView::ALL.iter().enumerate() {
        if i > 0 {
            total += 3;
        } // " \u00b7 "
        total += v.label().chars().count();
    }
    let mut tx_x = rows[0]
        .right()
        .saturating_sub(total as u16)
        .saturating_add(3);
    let mut tabs = Vec::with_capacity(RightView::ALL.len());
    for (i, v) in RightView::ALL.iter().enumerate() {
        if i > 0 {
            tx_x = tx_x.saturating_add(3);
        }
        let w = v.label().chars().count() as u16;
        tabs.push((
            *v,
            Rect {
                x: tx_x,
                y: rows[0].y,
                width: w,
                height: 1,
            },
        ));
        tx_x = tx_x.saturating_add(w);
    }
    app.tab_rects = tabs;

    let right = if app.zen {
        // Hidden, not zero-width: a rendered sidebar still claims mouse rects.
        app.lib_rect = None;
        app.scroll_rect = None;
        rows[2]
    } else {
        let body = Layout::horizontal([Constraint::Percentage(30), Constraint::Min(24)])
            .spacing(3)
            .split(rows[2]);
        render_library(f, app, theme, body[0]);
        body[1]
    };
    match app.view {
        RightView::NowPlaying => render_nowplaying_view(f, app, theme, right),
        RightView::Lyrics => render_lyrics(f, app, theme, right),
        RightView::Queue => render_queue_view(f, app, theme, right),
    }

    render_now_strip(f, app, theme, rows[4]);
    render_footer(f, app, theme, rows[5]);

    if app.actions.is_some() {
        render_actions_overlay(f, app, theme, area);
    }
}

/// Blank `area` and force it out to the terminal, erasing an inline image.
///
/// `ratatui-image` marks the image's cells `Skip` without changing their
/// symbols, and a blank cell compares equal to the blank that was already there
/// — so an ordinary `Clear` writes nothing and the picture survives it.
/// `AlwaysUpdate` is what makes the diff emit them anyway.
fn wipe_area(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_diff_option(CellDiffOption::AlwaysUpdate);
            }
        }
    }
}

/// Keep the terminal's own pixels in `area` by telling the diff to leave those
/// cells alone — what `ratatui-image` does for the image it just drew, without
/// drawing it again.
fn hold_area(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_diff_option(CellDiffOption::Skip);
            }
        }
    }
}

/// Force whatever is already in `area` out to the terminal, so an overlay lands
/// on top of an inline image instead of being skipped over it.
fn force_area(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_diff_option(CellDiffOption::AlwaysUpdate);
            }
        }
    }
}

/// The `Now Playing · Lyrics · Visualizer` indicator, active one lit.
fn view_tabs<'a>(app: &App, theme: Theme) -> Vec<Span<'a>> {
    let mut spans = vec![Span::styled("←→ ", theme.muted())];
    for (i, v) in RightView::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", theme.muted()));
        }
        let style = if *v == app.view {
            Style::default()
                .fg(theme.primary.into())
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        spans.push(Span::styled(v.label(), style));
    }
    spans
}

/// Where the list viewport starts, given where it started last frame.
///
/// Keeps `margin` rows visible either side of the cursor (vim's `scrolloff`).
/// Threading the previous `offset` back in is what makes it sticky: the cursor
/// moves freely inside the window, which follows only when pushed.
fn scroll_offset(offset: usize, selected: usize, cap: usize, total: usize, margin: usize) -> usize {
    if cap == 0 || total <= cap {
        return 0;
    }
    // Wider than half the viewport and the two bounds below would fight.
    let margin = margin.min((cap - 1) / 2);
    offset
        .min(selected.saturating_sub(margin)) // cursor near the top → scroll up
        .max((selected + margin + 1).saturating_sub(cap)) // near the bottom → down
        .min(total - cap)
}

fn render_library(f: &mut Frame, app: &mut App, theme: Theme, area: Rect) {
    f.render_widget(Block::default().style(theme.panel()), area);
    let inner = area.inner(Margin::new(2, 1));
    if inner.height < 2 {
        return;
    }

    // Header line: drill-in title, search input/results, or section indicator.
    let head: Line = if let Some(d) = app.details.last() {
        Line::from(vec![
            Span::styled("‹ ", Style::default().fg(theme.primary.into())),
            Span::styled(
                truncate(&d.title, inner.width.saturating_sub(8) as usize),
                theme.heading(),
            ),
            Span::styled("  Esc", theme.muted()),
        ])
    } else if app.input_mode {
        Line::from(vec![
            Span::styled("search: ", theme.heading()),
            Span::styled(
                format!("{}▏", app.query),
                Style::default().fg(theme.text.into()),
            ),
        ])
    } else if app.searching {
        Line::from(vec![
            Span::styled("search: ", theme.heading()),
            Span::styled(app.query.clone(), Style::default().fg(theme.text.into())),
            Span::styled("  (Esc)", theme.muted()),
        ])
    } else {
        let mut spans = vec![
            Span::styled("‹ ", theme.muted()),
            Span::styled(app.section.label(), theme.heading()),
            Span::styled(" ›", theme.muted()),
            Span::styled(
                format!(
                    "  {}/{} · {}",
                    app.section.index() + 1,
                    Section::ALL.len(),
                    app.cur_items().len()
                ),
                theme.muted(),
            ),
        ];
        if app.sort != SortMode::Added {
            spans.push(Span::styled(
                format!("  ⇅{}", app.sort.label()),
                Style::default().fg(theme.accent.into()),
            ));
        }
        Line::from(spans)
    };
    f.render_widget(
        Paragraph::new(head).block(Block::default().style(theme.panel())),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    let list_top = inner.y + 2;
    if list_top >= inner.bottom() {
        return;
    }
    let cap = (inner.bottom() - list_top) as usize;
    let total_items = app.cur_items().len();

    if total_items == 0 {
        app.scroll_rect = None;
        app.lib_rect = None;
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("(empty)", theme.muted())))
                .block(Block::default().style(theme.panel())),
            Rect {
                x: inner.x,
                y: list_top,
                width: inner.width,
                height: 1,
            },
        );
        return;
    }

    let offset = scroll_offset(
        app.lib_offset,
        app.selected,
        cap,
        total_items,
        myx::config::get().scrolloff,
    );
    app.lib_rect = Some(Rect {
        x: inner.x,
        y: list_top,
        width: inner.width,
        height: cap as u16,
    });
    app.lib_offset = offset;
    let overflow = total_items > cap && inner.width > 2;
    // Reserve an extra gutter column for the scrollbar (+1 char of padding).
    let max = inner.width.saturating_sub(if overflow { 3 } else { 2 }) as usize;

    let items = app.cur_items();
    for (row, item) in items.iter().skip(offset).take(cap).enumerate() {
        let idx = offset + row;
        let y = list_top + row as u16;
        let rect = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        };

        // Header rows: a bold section label (Home feed groups), not selectable.
        if item.is_header {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    item.name.clone(),
                    Style::default()
                        .fg(theme.accent.into())
                        .add_modifier(Modifier::BOLD),
                )))
                .block(Block::default().style(theme.panel())),
                rect,
            );
            continue;
        }

        let selected = idx == app.selected;
        let bg = if selected {
            theme.background_element.into()
        } else {
            theme.background_panel.into()
        };
        let block = left_bar_block(&theme, selected, bg);
        let style = if selected {
            Style::default()
                .fg(theme.text.into())
                .add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        // Mark rows that `P` can play outright (playlist / album / artist), so
        // they're distinguishable from tracks at a glance.
        let playable_ctx = context_target(item).is_some() && !item.is_play;
        let max = if playable_ctx {
            max.saturating_sub(2)
        } else {
            max
        };
        let label = truncate(&item.name, max);
        let mut spans = Vec::new();
        if playable_ctx {
            spans.push(Span::styled(
                " ▶",
                Style::default().fg(theme.border_dimmest.into()),
            ));
        }
        spans.push(Span::styled(format!(" {label}"), style));
        if !item.subtitle.is_empty() {
            let used = label.chars().count() + 1;
            let room = max.saturating_sub(used + 3);
            if room > 3 {
                spans.push(Span::styled(
                    " · ",
                    Style::default().fg(theme.border_dimmest.into()),
                ));
                spans.push(Span::styled(
                    truncate(&item.subtitle, room),
                    theme.muted().add_modifier(Modifier::DIM),
                ));
            }
        }
        f.render_widget(Paragraph::new(Line::from(spans)).block(block), rect);
    }

    // Subtle scrollbar: a hairline 1/8 track with a slightly denser 1/4 thumb,
    // in the right gutter. Shown only on overflow; the track rect is stashed on
    // `app` so mouse drags can scroll it.
    if overflow {
        let total = total_items;
        let sb_x = inner.right();
        let track_h = cap;
        let thumb_h = (cap * cap).div_ceil(total).max(1).min(track_h);
        let travel = track_h - thumb_h;
        let max_off = total - cap;
        let thumb_y0 = (offset * travel + max_off / 2)
            .checked_div(max_off)
            .unwrap_or(0);
        for i in 0..track_h {
            let y = list_top + i as u16;
            if y >= inner.bottom() {
                break;
            }
            let in_thumb = i >= thumb_y0 && i < thumb_y0 + thumb_h;
            let (glyph, color) = if in_thumb {
                ("\u{258E}", theme.text_muted) // 1/4 block - thumb
            } else {
                ("\u{258F}", theme.border_dimmest) // 1/8 block - track
            };
            f.render_widget(
                Paragraph::new(Span::styled(glyph, Style::default().fg(color.into()))),
                Rect {
                    x: sb_x,
                    y,
                    width: 1,
                    height: 1,
                },
            );
        }
        app.scroll_rect = Some(Rect {
            x: sb_x,
            y: list_top,
            width: 1,
            height: track_h as u16,
        });
        app.scroll_len = total;
    } else {
        app.scroll_rect = None;
    }
}

/// View ①: album art with track details directly beneath — centered as a group.
fn render_nowplaying_view(f: &mut Frame, app: &mut App, theme: Theme, area: Rect) {
    if app.now.is_none() {
        f.render_widget(
            Paragraph::new("Nothing playing.\nBrowse ← and press Enter.")
                .style(theme.muted())
                .alignment(Alignment::Center),
            center_v(area, 2),
        );
        return;
    }

    // Split: album art + track info on top, a compact spectrum below, lifted a
    // little off the bottom.
    let chunks = Layout::vertical([
        Constraint::Min(6),    // art + text
        Constraint::Length(7), // spectrum
        Constraint::Length(2), // breathing room (lifts the spectrum up)
    ])
    .split(area);
    let top = chunks[0];
    // Push the art + info group down a little from the top.
    let top = Rect {
        x: top.x,
        y: top.y + 3,
        width: top.width,
        height: top.height.saturating_sub(3),
    };
    let viz_area = chunks[1];

    // Derive the cover's cell footprint from the terminal's font aspect so a
    // square image renders square (and our centering math is exact).
    let font = app.picker.font_size();
    let fw = font.width.max(1) as u32;
    let fh = font.height.max(1) as u32;

    // Reserve 3 rows for text (+1 gap). Cap the art so the group stays compact.
    let avail_h = top.height.saturating_sub(4);
    let mut art_h = avail_h.clamp(3, 14);
    // Square image width in cells for this height: w = h * fh / fw.
    let mut art_w = (art_h as u32 * fh / fw) as u16;
    if art_w > top.width {
        art_w = top.width;
        art_h = (art_w as u32 * fw / fh) as u16;
    }

    let group_h = art_h + 4; // art + gap + title + artist + album
    let art_y = top.y + top.height.saturating_sub(group_h) / 2;
    let art_x = top.x + top.width.saturating_sub(art_w) / 2;
    let art_rect = Rect {
        x: art_x,
        y: art_y,
        width: art_w,
        height: art_h,
    };

    let repaint = app.art_repaint;
    match app.now.as_mut().and_then(|n| n.cover.as_mut()) {
        _ if repaint == ArtRepaint::Wipe => wipe_area(f, art_rect),
        // Writing the escape means transmitting the image, so only do it when
        // something actually asked for it. A theme fade repaints every glyph on
        // screen dozens of times, and re-sending the cover on each of those is
        // what made it flicker.
        Some(cover) if repaint == ArtRepaint::Draw || cover.needs_send(art_rect) => {
            cover.render(f, art_rect)
        }
        // Already on screen: hold the cells so nothing overwrites the picture,
        // and send nothing.
        Some(_) => hold_area(f, art_rect),
        None => wipe_area(f, art_rect),
    }

    if let Some(n) = app.now.as_ref() {
        let text_rect = Rect {
            x: top.x,
            y: art_rect.y + art_h + 1,
            width: top.width,
            height: 3,
        };
        let lines = vec![
            Line::from(Span::styled(
                truncate(&n.title, top.width as usize),
                Style::default()
                    .fg(theme.text.into())
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                truncate(&n.artist, top.width as usize),
                Style::default().fg(theme.primary.into()),
            )),
            Line::from(Span::styled(
                truncate(&n.album, top.width as usize),
                theme.muted(),
            )),
        ];
        f.render_widget(
            Paragraph::new(lines).alignment(Alignment::Center),
            text_rect,
        );
    }

    render_visualizer(f, app, theme, viz_area);
}

/// Slim persistent bottom strip: play state + track, then the progress bar.
fn render_now_strip(f: &mut Frame, app: &mut App, theme: Theme, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    // Volume meter (top row, far right). Still honest in remote mode: +/- goes
    // to the remote device's volume.
    render_volume(f, app, theme, rows[0]);

    // Seek/progress bar (bottom row). Record bar geometry for click-to-seek.
    let pos = app.position_ms();
    let left_len = format!("{} ", fmt_ms(pos)).chars().count() as u16;
    let right_len = format!(
        " {}",
        fmt_ms(app.now.as_ref().map(|n| n.duration_ms).unwrap_or(0))
    )
    .chars()
    .count() as u16;
    let bar_w = rows[1].width.saturating_sub(left_len + right_len);
    app.bar_rect = Some(Rect {
        x: rows[1].x + left_len,
        y: rows[1].y,
        width: bar_w,
        height: 1,
    });
    render_progress(f, app, theme, rows[1]);
}

/// The volume meter — a graduated ramp + percentage, right-aligned in `area`.
/// Stashes the 8-bar region on `app` for click/drag control.
fn render_volume(f: &mut Frame, app: &mut App, theme: Theme, area: Rect) {
    const VLEV: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let filled = (app.volume as usize * VLEV.len() + 50) / 100;
    let mut vspans: Vec<Span> = Vec::with_capacity(VLEV.len() + 1);
    for (i, ch) in VLEV.iter().enumerate() {
        let color = if i < filled {
            theme.primary
        } else {
            theme.border_dimmest
        };
        vspans.push(Span::styled(
            ch.to_string(),
            Style::default().fg(color.into()),
        ));
    }
    vspans.push(Span::styled(format!(" {:>3}%", app.volume), theme.muted()));
    f.render_widget(
        Paragraph::new(Line::from(vspans)).alignment(Alignment::Right),
        area,
    );
    // 8-bar region for click/drag. Content is 13 cells (8 bars + " NNN%"),
    // right-aligned, so the bars start 13 cells in from the right edge.
    app.vol_rect = Some(Rect {
        x: area.right().saturating_sub(13),
        y: area.y,
        width: VLEV.len() as u16,
        height: 1,
    });
}

fn render_lyrics(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let inner = area.inner(Margin::new(2, 0));
    if inner.height == 0 {
        return;
    }
    let max = inner.width as usize;

    // Header: current track title + "artist · album", above the lyrics.
    let mut lyrics_area = inner;
    if let Some(n) = app.now.as_ref() {
        let head = Layout::vertical([
            Constraint::Length(1), // title
            Constraint::Length(1), // artist / album
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // lyrics
        ])
        .split(inner);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate(&n.title, max),
                Style::default()
                    .fg(theme.text.into())
                    .add_modifier(Modifier::BOLD),
            )))
            .alignment(Alignment::Center),
            head[0],
        );
        let sub = if n.album.is_empty() {
            n.artist.clone()
        } else {
            format!("{} · {}", n.artist, n.album)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(truncate(&sub, max), theme.muted())))
                .alignment(Alignment::Center),
            head[1],
        );
        lyrics_area = head[3];
    }

    if app.lyrics.is_empty() {
        let msg = if app.now.is_some() {
            "♪︎  no lyrics for this track"
        } else {
            "♪︎  nothing playing"
        };
        f.render_widget(
            Paragraph::new(msg)
                .style(theme.muted())
                .alignment(Alignment::Center),
            center_v(lyrics_area, 1),
        );
        return;
    }

    let h = lyrics_area.height as usize;
    let pos = app.position_ms();
    let cur = if app.lyrics_synced {
        app.lyrics.iter().rposition(|(t, _)| *t <= pos).unwrap_or(0)
    } else {
        0
    };
    let start = cur.saturating_sub(h / 2);

    let mut lines: Vec<Line> = Vec::with_capacity(h);
    for (i, (_, text)) in app.lyrics.iter().enumerate().skip(start).take(h) {
        let style = if app.lyrics_synced && i == cur {
            Style::default()
                .fg(theme.primary.into())
                .add_modifier(Modifier::BOLD)
        } else if app.lyrics_synced && i < cur {
            Style::default().fg(theme.border_subtle.into())
        } else {
            theme.muted()
        };
        let txt = if text.is_empty() {
            "♪︎".to_string()
        } else {
            truncate(text, max)
        };
        lines.push(Line::from(Span::styled(txt, style)));
    }
    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        lyrics_area,
    );
}

fn render_visualizer(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let active = app
        .engine
        .bands
        .try_lock()
        .map(|g| g.is_active)
        .unwrap_or(false);
    if !active {
        return;
    }
    let Ok(guard) = app.engine.bands.try_lock() else {
        return;
    };
    let values: [f32; NUM_BANDS] = guard.values;
    let peak = guard.peak_envelope.max(1e-6);
    drop(guard);

    // Cap the spectrum to a centered band — full-pane bars are too tall/wide.
    let vh = ((area.height as u32 * 3 / 5) as u16)
        .clamp(6, 14)
        .min(area.height);
    let vw = ((area.width as u32 * 9 / 10) as u16)
        .clamp(24, 80)
        .min(area.width);
    let vrect = Rect {
        x: area.x + area.width.saturating_sub(vw) / 2,
        y: area.y + area.height.saturating_sub(vh) / 2,
        width: vw,
        height: vh,
    };
    let w = vrect.width as usize;
    let h = vrect.height as usize;
    if w == 0 || h == 0 {
        return;
    }

    // 1. Box-average the bands into each column (anti-aliasing vs. single-pick).
    let mut cols = vec![0.0f32; w];
    for (x, c) in cols.iter_mut().enumerate() {
        let lo = x * NUM_BANDS / w;
        let hi = (((x + 1) * NUM_BANDS / w).max(lo + 1)).min(NUM_BANDS);
        let sum: f32 = values[lo..hi].iter().sum();
        let v = sum / (hi - lo) as f32;
        // Perceptual curve so quiet detail stays visible.
        *c = (v / peak).sqrt().clamp(0.0, 1.0);
    }

    // 2. Spatial smoothing — a couple of weighted passes so the envelope flows
    //    instead of spiking. This is what kills the "chopped" look.
    for _ in 0..2 {
        let src = cols.clone();
        for x in 0..w {
            let l = src[x.saturating_sub(1)];
            let r = src[(x + 1).min(w - 1)];
            cols[x] = l * 0.25 + src[x] * 0.5 + r * 0.25;
        }
    }

    // 3. Render with an eighth-block sub-cell tip and a vertical color gradient
    //    (info at the base → primary → accent at the peaks) for a smooth wash.
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let stops = [theme.info, theme.primary, theme.accent];

    let mut lines: Vec<Line> = Vec::with_capacity(h);
    for row in 0..h {
        let from_bottom = (h - 1 - row) as f32;
        let vfrac = if h > 1 {
            from_bottom / (h - 1) as f32
        } else {
            0.0
        };
        let color: ratatui::style::Color = gradient::interpolate(&stops, vfrac).into();
        let mut spans: Vec<Span> = Vec::with_capacity(w);
        for &v in &cols {
            let filled = v * h as f32 - from_bottom;
            let ch = if filled >= 1.0 {
                '█'
            } else if filled <= 0.0 {
                ' '
            } else {
                LEVELS[((filled * 8.0) as usize).clamp(1, 8) - 1]
            };
            if ch == ' ' {
                spans.push(Span::raw(" "));
            } else {
                spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), vrect);
}

fn render_progress(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let (pos, dur) = match &app.now {
        Some(n) => (app.position_ms(), n.duration_ms.max(1)),
        None => (0, 1),
    };
    // Compute the bar width from the exact label lengths so the duration sits
    // flush against the right edge (aligned with the volume meter above it).
    let left = format!("{} ", fmt_ms(pos));
    let right = format!(" {}", fmt_ms(dur));
    let reserve = left.chars().count() + right.chars().count();
    let bar_w = (area.width as usize).saturating_sub(reserve);
    let filled = ((pos as f32 / dur as f32) * bar_w as f32) as usize;

    let mut spans = vec![Span::styled(left, theme.muted())];
    spans.extend(gradient_progress(
        bar_w,
        filled,
        &[theme.primary, theme.accent],
        theme.border_dimmest,
    ));
    spans.push(Span::styled(right, theme.muted()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_footer(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let on = |b: bool| if b { theme.success } else { theme.text_muted };
    let key = |k: &'static str| Span::styled(k, Style::default().fg(theme.primary.into()));
    let lbl = |t: &'static str| Span::styled(t, theme.muted());
    let enter_lbl = enter_label(app.cur_items().get(app.selected));
    let play_lbl = if app.now.as_ref().is_some_and(|n| n.is_playing) {
        " pause   "
    } else {
        " play    "
    };
    // Flagged hints go with the library pane, which zen hides — a key that does
    // nothing must not be advertised.
    let hints = [
        (true, key("⇥"), lbl(" section   ")),
        (false, key("←→"), lbl(" view   ")),
        (true, key("/"), lbl(" search   ")),
        (
            true,
            key("⏎"),
            Span::styled(format!(" {enter_lbl}   "), theme.muted()),
        ),
        (true, key("⇧⏎"), lbl(" play   ")),
        (true, key("S"), lbl(" shuffle   ")),
        (false, key("␣"), Span::styled(play_lbl, theme.muted())),
        (false, key("n/b"), lbl(" skip   ")),
        (false, key("⇧←→"), lbl(" seek   ")),
        (true, key("o"), lbl(" sort   ")),
        (false, key("+/-"), lbl(" vol   ")),
        (
            false,
            Span::styled("s", Style::default().fg(on(app.shuffle).into())),
            lbl(" shuffle   "),
        ),
        (false, key("a"), lbl(" actions   ")),
        (
            false,
            Span::styled("z", Style::default().fg(on(app.zen).into())),
            lbl(" zen   "),
        ),
        (false, key("q"), lbl(" quit")),
    ];
    let line = Line::from(
        hints
            .into_iter()
            .filter(|(needs_library, ..)| !needs_library || !app.zen)
            .flat_map(|(_, k, label)| [k, label])
            .collect::<Vec<_>>(),
    );
    f.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
}

/// Context actions menu — a centered overlay list.
fn render_actions_overlay(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let Some(menu) = &app.actions else { return };
    let w = (area.width * 5 / 10).clamp(28, 52);
    let h = (menu.items.len() as u16 + 4).clamp(6, area.height.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    f.render_widget(Clear, rect);
    f.render_widget(Block::default().style(theme.element()), rect);
    let inner = rect.inner(Margin::new(2, 1));
    let max = inner.width as usize;
    let mut lines = vec![
        Line::from(Span::styled(truncate(&menu.title, max), theme.heading())),
        Line::raw(""),
    ];
    for (i, it) in menu
        .items
        .iter()
        .take(inner.height.saturating_sub(2) as usize)
        .enumerate()
    {
        if i == menu.selected {
            lines.push(Line::from(vec![
                Span::styled("› ", Style::default().fg(theme.primary.into())),
                Span::styled(
                    truncate(&it.label, max.saturating_sub(2)),
                    Style::default()
                        .fg(theme.text.into())
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                format!("  {}", truncate(&it.label, max.saturating_sub(2))),
                theme.muted(),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
    force_area(f, rect);
}

fn render_queue_view(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let inner = area.inner(Margin::new(2, 1));
    if inner.height == 0 {
        return;
    }
    let max = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    // Context header — what's playing from.
    if !app.source_name.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("PLAYING FROM  ", theme.muted()),
            Span::styled(
                truncate(&app.source_name, max.saturating_sub(14)),
                Style::default()
                    .fg(theme.primary.into())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::raw(""));
    }

    // Now playing — the current track, above the up-next list.
    if let Some(n) = app.now.as_ref() {
        lines.push(Line::from(Span::styled("NOW PLAYING", theme.heading())));
        lines.push(Line::from(vec![
            Span::styled("   ", theme.muted()),
            Span::styled(
                truncate(&n.title, max.saturating_sub(3)),
                Style::default()
                    .fg(theme.text.into())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", n.artist), theme.muted()),
        ]));
        lines.push(Line::raw(""));
    }

    lines.push(Line::from(Span::styled("UP NEXT", theme.heading())));
    lines.push(Line::raw(""));

    let used = lines.len();
    if app.queue.is_empty() {
        lines.push(Line::from(Span::styled("queue is empty", theme.muted())));
    } else {
        for (i, q) in app
            .queue
            .iter()
            .take(inner.height.saturating_sub(used as u16) as usize)
            .enumerate()
        {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>2}  ", i + 1), theme.muted()),
                Span::styled(
                    truncate(q, max.saturating_sub(4)),
                    Style::default().fg(theme.text.into()),
                ),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// A blocking HTTP client with a timeout so a stalled network can't wedge a
/// worker thread forever (audit H2).
fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}

// ------------------------------------------------------------------ terminal

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

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

/// The startup screen: wordmark, spinner, and what we're waiting on.
fn render_loading(f: &mut Frame, label: &str, frame: usize) {
    let theme = TOKYONIGHT;
    let area = f.area();
    f.render_widget(Block::default().style(theme.panel()), area);

    let top = area.y + area.height.saturating_sub(3) / 2;
    let row = |dy: u16| Rect {
        x: area.x,
        y: top.saturating_add(dy).min(area.bottom().saturating_sub(1)),
        width: area.width,
        height: 1,
    };

    let mark: Vec<Span> = gradient_line("\u{FF2D}\u{FF39}\u{FF38}", &[theme.primary, theme.accent])
        .into_iter()
        .map(|mut sp| {
            sp.style = sp.style.add_modifier(Modifier::BOLD);
            sp
        })
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(mark)).alignment(Alignment::Center),
        row(0),
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(SPINNER[frame % SPINNER.len()], theme.heading()),
            Span::styled(format!("  {label}…"), theme.muted()),
        ]))
        .alignment(Alignment::Center),
        row(2),
    );
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
#[path = "main_tests/mod.rs"]
mod main_tests;
