//! myx — the fully-wired terminal Spotify player.
//!
//! librespot streaming engine + Web API (your own client id) + album-art-reactive
//! theming with cross-fades + live FFT visualizer, in noodle's visual language.
//! Multi-section library (playlists / liked / albums / artists), shuffle, repeat,
//! and a live queue view.

use std::io::{self, Stdout};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use ratatui_image::picker::Picker;

use myx::anim::ThemeFade;
use myx::audio::NUM_BANDS;
use myx::components::{gradient_line, gradient_progress, left_bar_block};
use myx::cover::Cover;
use myx::engine::{self, Engine, EngineEvent};
use myx::gradient::{self};
use myx::reactive::derive_theme;
use myx::theme::{Theme, TOKYONIGHT};
use myx::webapi::WebApi;

type Term = Terminal<CrosstermBackend<Stdout>>;
const FADE_MS: u64 = 300;
const FRAME_MS: u64 = 22;

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
}

impl LibItem {
    fn track(name: String, subtitle: String, uri: String) -> Self {
        Self { name, subtitle, uri, is_track: true, is_header: false, is_play: false }
    }
    fn ctx(name: String, subtitle: String, uri: String) -> Self {
        Self { name, subtitle, uri, is_track: false, is_header: false, is_play: false }
    }
    fn play(name: String, uri: String) -> Self {
        Self { name, subtitle: String::new(), uri, is_track: false, is_header: false, is_play: true }
    }
    fn header(name: &str) -> Self {
        Self { name: name.to_string(), subtitle: String::new(), uri: String::new(), is_track: false, is_header: true, is_play: false }
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
    ToggleLike { id: String, saved: bool },
    Queue { uri: String },
    AddToPlaylistMenu { track_uri: String },
    AddToPlaylist { playlist_id: String, track_uri: String },
    ToggleFollowArtist { id: String, following: bool },
    ToggleSaveAlbum { id: String, saved: bool },
    FollowPlaylist { id: String },
    Play { uri: String },
    Open { uri: String, name: String },
    CopyLink { uri: String },
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
    fn is_empty(&self) -> bool {
        self.home.is_empty()
            && self.recent.is_empty()
            && self.playlists.is_empty()
            && self.liked.is_empty()
            && self.albums.is_empty()
            && self.artists.is_empty()
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
    image: Option<image::DynamicImage>,
    theme: Option<Theme>,
}

/// What kind of thing is currently playing — persisted so we can resume the real
/// context (and its live queue) on reboot, not just a bare track.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
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
    last_uri: String,
    last_title: String,
    last_artist: String,
    last_album: String,
    last_duration_ms: u32,
    last_position_ms: u32,
    queue: Vec<String>,
    #[serde(default)]
    source: PlaySource,
    #[serde(default)]
    source_name: String,
}

impl SavedState {
    fn path() -> Option<std::path::PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(std::path::PathBuf::from(home).join(".cache/myx/state.json"))
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
    status: String,
    library: Library,
    section: Section,
    selected: usize,
    shuffle: bool,
    repeat: bool,
    volume: u8, // 0..=100 (mirrors the 50% mixer default)
    queue: Vec<String>,
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
    // A last-played track URI to re-enrich (cover/theme/lyrics) on boot.
    restore_uri: Option<String>,
    // Whether real playback has started this session (gates resume-on-play).
    playback_started: bool,
    // Whether we reclaimed a live server-side session (vs. local fallback).
    reclaimed: bool,
    // What's playing (context/radio/liked), for faithful resume on reboot.
    source: PlaySource,
    source_name: String,
}

impl App {
    fn start_fade(&mut self, to: Theme) {
        self.fade = Some(ThemeFade::new(self.displayed, to, Duration::from_millis(FADE_MS)));
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
    fn position_ms(&self) -> u32 {
        match &self.now {
            Some(n) if n.is_playing => {
                (n.position_ms + n.position_at.elapsed().as_millis() as u32).min(n.duration_ms)
            }
            Some(n) => n.position_ms.min(n.duration_ms),
            None => 0,
        }
    }
    /// First non-header index (where a fresh selection should land).
    fn first_selectable(&self) -> usize {
        self.cur_items().iter().position(|i| !i.is_header).unwrap_or(0)
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
        if self.cur_items().get(self.selected).is_some_and(|i| i.is_header) {
            self.selected = self.first_selectable();
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
            if item.uri == "myx:action:liked-shuffle" || item.uri == "myx:action:liked-play" {
                let shuffle = item.uri.ends_with("shuffle");
                let uris: Vec<String> = self.library.liked.iter().filter(|i| i.is_track).map(|i| i.uri.clone()).collect();
                if !uris.is_empty() {
                    self.shuffle = shuffle;
                    self.source = PlaySource::Liked;
                    self.source_name = "Liked Songs".to_string();
                    self.status = "starting Liked Songs…".to_string();
                    let _ = self.engine.play_tracks(uris, None, shuffle);
                }
                return Activated::None;
            }
            self.status = format!("starting {}…", item.name);
            self.source = PlaySource::Context(item.uri.clone());
            self.source_name = self.details.last().map(|d| d.title.clone()).unwrap_or_default();
            let _ = self.engine.play_context(item.uri, self.shuffle);
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
                let _ = self.engine.play_context_at(ctx, Some(item.uri.clone()), 0, self.shuffle);
                return Activated::None;
            }
            // Section track list.
            let uris = self.cur_items().iter().filter(|i| i.is_track).map(|i| i.uri.clone()).collect();
            self.status = format!("starting {}…", item.name);
            if self.section == Section::Liked {
                self.source = PlaySource::Liked;
                self.source_name = "Liked Songs".to_string();
            } else {
                self.source = PlaySource::None;
                self.source_name = self.section.label().to_string();
            }
            let _ = self.engine.play_tracks(uris, Some(item.uri.clone()), self.shuffle);
            return Activated::None;
        }
        // Otherwise it's a context (artist / album / playlist) — open it.
        self.status = format!("opening {}…", item.name);
        Activated::Open(item.uri, item.name)
    }
}

// ------------------------------------------------------------------ main

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Restore last session first, so the engine starts at the saved volume.
    let saved = SavedState::load();
    let init_vol = if saved.volume == 0 { 50 } else { saved.volume.min(100) };

    println!("myx: connecting to Spotify…");
    let (ev_tx, ev_rx) = flume::unbounded::<EngineEvent>();
    let engine = engine::run(ev_tx, init_vol).await.context("start engine")?;
    println!("myx: streaming device live.");

    let webapi = tokio::task::spawn_blocking(WebApi::init)
        .await
        .context("web api init task")?
        .context("authorize web api")?;
    let webapi = Arc::new(Mutex::new(webapi));

    if let Some(uri) = std::env::args().nth(1) {
        let _ = engine.play_context(uri, false);
    }

    let mut terminal = init_terminal()?;
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

    // Rebuild the last now-playing (paused) for a seamless resume look.
    let now = if !saved.last_uri.is_empty() {
        Some(NowPlaying {
            uri: saved.last_uri.clone(),
            title: saved.last_title.clone(),
            artist: saved.last_artist.clone(),
            album: saved.last_album.clone(),
            duration_ms: saved.last_duration_ms,
            position_ms: saved.last_position_ms,
            position_at: Instant::now(),
            is_playing: false,
            cover: None,
        })
    } else {
        None
    };
    let restore_uri = (!saved.last_uri.is_empty()).then(|| saved.last_uri.clone());

    let app = App {
        engine,
        picker,
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
        volume: if saved.volume == 0 { 50 } else { saved.volume.min(100) },
        queue: saved.queue,
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
        playback_started: false,
        reclaimed: false,
        source: saved.source.clone(),
        source_name: saved.source_name.clone(),
    };

    let res = run_ui(&mut terminal, app, ev_rx).await;
    restore_terminal(&mut terminal)?;
    res
}

async fn run_ui(terminal: &mut Term, mut app: App, ev_rx: flume::Receiver<EngineEvent>) -> Result<()> {
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
    let (queue_tx, queue_rx) = flume::unbounded::<Vec<String>>();
    let (search_tx, search_rx) = flume::unbounded::<Vec<LibItem>>();
    let (lyrics_tx, lyrics_rx) = flume::unbounded::<(Vec<(u32, String)>, bool)>();
    let (detail_tx, detail_rx) = flume::unbounded::<(String, String, Vec<LibItem>)>();
    let (menu_tx, menu_rx) = flume::unbounded::<ActionMenu>();
    let (astatus_tx, astatus_rx) = flume::unbounded::<String>();
    let (pstate_tx, pstate_rx) = flume::unbounded::<PlaybackState>();
    let (radio_tx, radio_rx) = flume::unbounded::<Vec<String>>();
    spawn_library_fetch(app.webapi.clone(), lib_tx.clone());

    // Reclaim server-side playback: read live state + transfer it onto myx so the
    // full context + queue + position come back.
    spawn_restore(app.webapi.clone(), app.engine.device_id(), pstate_tx);

    // Re-enrich the restored last-played track (cover / theme / lyrics).
    if let Some(uri) = app.restore_uri.take() {
        if let Some(id) = track_id_from_uri(&uri) {
            let webapi = app.webapi.clone();
            let tx = meta_tx.clone();
            tokio::task::spawn_blocking(move || {
                let _ = tx.send(fetch_track_meta(&webapi, &id));
            });
        }
    }

    let mut frame = tokio::time::interval(Duration::from_millis(FRAME_MS));
    let mut frame_count: u64 = 0;

    loop {
        tokio::select! {
            _ = frame.tick() => {
                advance_fade(&mut app);
                terminal.draw(|f| render(f, &mut app))?;
                frame_count += 1;
                if frame_count.is_multiple_of(240) {
                    // Refresh the live queue while playing so the snapshot stays
                    // current, then persist it (survives reboot).
                    if app.playback_started || app.reclaimed {
                        spawn_queue_fetch(app.webapi.clone(), queue_tx.clone());
                    }
                    save_state(&app);
                }
            }
            ev = ev_rx.recv_async() => {
                let Ok(ev) = ev else { break };
                handle_engine_event(&mut app, ev, &meta_tx);
            }
            ev = in_rx.recv_async() => {
                let Ok(Event::Key(key)) = ev else { continue };
                if key.kind != KeyEventKind::Press { continue; }
                if handle_key(&mut app, key.code, &lib_tx, &queue_tx, &search_tx, &detail_tx, &menu_tx, &astatus_tx, &radio_tx) {
                    save_state(&app);
                    break;
                }
            }
            m = meta_rx.recv_async() => {
                if let Ok(meta) = m { apply_meta(&mut app, meta, &lyrics_tx); }
            }
            lib = lib_rx.recv_async() => {
                if let Ok((section, items)) = lib {
                    app.library.set(section, items);
                    if section == app.section {
                        app.normalize_selection();
                    }
                    if !app.library.is_empty() {
                        app.status.clear();
                    }
                }
            }
            q = queue_rx.recv_async() => {
                // Don't let an empty live queue (e.g. a bare resumed track) wipe
                // the restored/last-known snapshot.
                if let Ok(q) = q {
                    if !q.is_empty() {
                        app.queue = q;
                    }
                }
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
            }
            ly = lyrics_rx.recv_async() => {
                if let Ok((lines, synced)) = ly {
                    app.lyrics = lines;
                    app.lyrics_synced = synced;
                }
            }
            d = detail_rx.recv_async() => {
                if let Ok((context_uri, title, items)) = d {
                    app.details.push(Detail { context_uri, title, items, parent_selected: app.selected });
                    app.selected = app.first_selectable();
                    app.status.clear();
                }
            }
            menu = menu_rx.recv_async() => {
                if let Ok(menu) = menu {
                    if !menu.items.is_empty() {
                        app.actions = Some(menu);
                    }
                }
            }
            st = astatus_rx.recv_async() => {
                if let Ok(msg) = st { app.status = msg; }
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
                    tokio::task::spawn_blocking(move || { let _ = tx.send(fetch_track_meta(&webapi, &id)); });
                    spawn_queue_fetch(app.webapi.clone(), queue_tx.clone());
                }
            }
            rad = radio_rx.recv_async() => {
                if let Ok(uris) = rad {
                    if !uris.is_empty() {
                        // Seed first, similar tracks queue up behind it.
                        let _ = app.engine.play_tracks(uris, None, false);
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
                }
            }
        }
    }
    Ok(())
}

/// Resume the persisted playback source at the last track/position — the
/// faithful reboot resume (real context ⇒ real queue continuation).
fn resume_source(app: &mut App, radio_tx: &flume::Sender<Vec<String>>) {
    let track = app.now.as_ref().map(|n| n.uri.clone()).filter(|u| !u.is_empty());
    let pos = app.now.as_ref().map(|n| n.position_ms).unwrap_or(0);
    match app.source.clone() {
        PlaySource::Context(ctx) => {
            let _ = app.engine.play_context_at(ctx, track, pos, app.shuffle);
        }
        PlaySource::Radio(seed) => {
            let session = app.engine.session();
            let tx = radio_tx.clone();
            app.status = "resuming radio…".to_string();
            tokio::spawn(async move {
                if let Ok(uris) = engine::radio_tracks(&session, &seed).await {
                    let _ = tx.send(uris);
                }
            });
        }
        PlaySource::Liked if !app.library.liked.is_empty() => {
            let uris: Vec<String> = app.library.liked.iter().map(|i| i.uri.clone()).collect();
            let _ = app.engine.play_tracks(uris, track, app.shuffle);
        }
        _ => {
            // Fallback: single last track at position.
            match track {
                Some(uri) => { let _ = app.engine.play_track_at(uri, pos); }
                None => { let _ = app.engine.play(); }
            }
        }
    }
}

/// Returns true if the app should quit.
#[allow(clippy::too_many_arguments)]
fn handle_key(
    app: &mut App,
    code: KeyCode,
    lib_tx: &flume::Sender<(Section, Vec<LibItem>)>,
    queue_tx: &flume::Sender<Vec<String>>,
    search_tx: &flume::Sender<Vec<LibItem>>,
    detail_tx: &flume::Sender<(String, String, Vec<LibItem>)>,
    menu_tx: &flume::Sender<ActionMenu>,
    astatus_tx: &flume::Sender<String>,
    radio_tx: &flume::Sender<Vec<String>>,
) -> bool {
    // --- Actions menu captures input while open ---
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
            KeyCode::Backspace => { app.query.pop(); }
            KeyCode::Char(c) => app.query.push(c),
            _ => {}
        }
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
            } else {
                return true;
            }
        }
        KeyCode::Char(' ') | KeyCode::Char('p') => {
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
        KeyCode::Char('n') => { let _ = app.engine.next(); }
        KeyCode::Char('b') => { let _ = app.engine.prev(); }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            app.volume = (app.volume + 5).min(100);
            let _ = app.engine.set_volume(vol_u16(app.volume));
        }
        KeyCode::Char('-') | KeyCode::Char('_') => {
            app.volume = app.volume.saturating_sub(5);
            let _ = app.engine.set_volume(vol_u16(app.volume));
        }
        KeyCode::Char('s') => {
            app.shuffle = !app.shuffle;
            let _ = app.engine.shuffle(app.shuffle);
        }
        KeyCode::Char('R') => {
            app.repeat = !app.repeat;
            let _ = app.engine.repeat(app.repeat);
        }
        KeyCode::Char('r') => {
            app.status = "loading library…".to_string();
            spawn_library_fetch(app.webapi.clone(), lib_tx.clone());
        }
        KeyCode::Char('a') => {
            let item = app.cur_items().get(app.selected).cloned();
            if let Some(item) = item {
                if !item.is_header && !item.is_play {
                    app.status = "…".to_string();
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
        // Arrow keys rotate the right-pane view (Now Playing / Lyrics / Queue).
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
        KeyCode::Down | KeyCode::Char('j') => app.move_sel(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_sel(-1),
        KeyCode::Enter => {
            match app.activate() {
                Activated::Open(uri, name) => {
                    spawn_detail_fetch(app.webapi.clone(), uri, name, detail_tx.clone());
                }
                Activated::Radio(uri) => {
                    app.status = "starting radio…".to_string();
                    let session = app.engine.session();
                    let tx = radio_tx.clone();
                    tokio::spawn(async move {
                        if let Ok(uris) = engine::radio_tracks(&session, &uri).await {
                            let _ = tx.send(uris);
                        }
                    });
                }
                Activated::None => {}
            }
        }
        _ => {}
    }
    false
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
    let kind = app.actions.as_ref().and_then(|m| m.items.get(m.selected)).map(|i| i.kind.clone());
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
                        kind: ActionKind::AddToPlaylist { playlist_id: id, track_uri: track_uri.clone() },
                    })
                })
                .collect();
            if items.is_empty() {
                app.status = "no playlists to add to".to_string();
                app.actions = None;
            } else {
                app.actions = Some(ActionMenu { title: "Add to playlist".to_string(), items, selected: 0 });
            }
        }
        ActionKind::Play { uri } => {
            let _ = app.engine.play_context(uri, app.shuffle);
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

/// Convert a `spotify:kind:id` URI to an open.spotify.com link.
fn uri_to_url(uri: &str) -> String {
    let mut p = uri.split(':');
    p.next();
    let kind = p.next().unwrap_or("");
    let id = p.next().unwrap_or("");
    format!("https://open.spotify.com/{kind}/{id}")
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
        if let Ok(mut child) = Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
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
            let _ = tx.send(build_action_menu(&token, &item));
        }
    });
}

/// Build the context menu for `item`, checking saved/following state and
/// resolving related artist/album links up front.
fn build_action_menu(token: &str, item: &LibItem) -> ActionMenu {
    let mut parts = item.uri.split(':');
    parts.next();
    let kind = parts.next().unwrap_or("");
    let id = parts.next().unwrap_or("").to_string();
    let uri = item.uri.clone();
    let client = reqwest::blocking::Client::new();
    let mut items = Vec::new();

    match kind {
        "track" => {
            let saved = api_contains(token, &format!("https://api.spotify.com/v1/me/tracks/contains?ids={id}"));
            items.push(ActionItem {
                label: if saved { "♥  Remove from Liked".into() } else { "♡  Add to Liked".into() },
                kind: ActionKind::ToggleLike { id: id.clone(), saved },
            });
            items.push(ActionItem { label: "＋  Add to Queue".into(), kind: ActionKind::Queue { uri: uri.clone() } });
            items.push(ActionItem { label: "≡  Add to Playlist…".into(), kind: ActionKind::AddToPlaylistMenu { track_uri: uri.clone() } });
            // Resolve the track's artist + album for "Go to" navigation.
            if let Some(v) = get_json(&client, &format!("https://api.spotify.com/v1/tracks/{id}"), token) {
                if let (Some(au), Some(an)) = (v["artists"][0]["uri"].as_str(), v["artists"][0]["name"].as_str()) {
                    items.push(ActionItem { label: format!("→  Go to Artist ({an})"), kind: ActionKind::Open { uri: au.to_string(), name: an.to_string() } });
                }
                if let (Some(lu), Some(ln)) = (v["album"]["uri"].as_str(), v["album"]["name"].as_str()) {
                    items.push(ActionItem { label: "→  Go to Album".into(), kind: ActionKind::Open { uri: lu.to_string(), name: ln.to_string() } });
                }
            }
            items.push(ActionItem { label: "⧉  Copy Link".into(), kind: ActionKind::CopyLink { uri } });
        }
        "artist" => {
            let following = api_contains(token, &format!("https://api.spotify.com/v1/me/following/contains?type=artist&ids={id}"));
            items.push(ActionItem {
                label: if following { "Unfollow".into() } else { "Follow".into() },
                kind: ActionKind::ToggleFollowArtist { id, following },
            });
            items.push(ActionItem { label: "▶  Play".into(), kind: ActionKind::Play { uri: uri.clone() } });
            items.push(ActionItem { label: "→  Open".into(), kind: ActionKind::Open { uri: uri.clone(), name: item.name.clone() } });
            items.push(ActionItem { label: "⧉  Copy Link".into(), kind: ActionKind::CopyLink { uri } });
        }
        "album" => {
            let saved = api_contains(token, &format!("https://api.spotify.com/v1/me/albums/contains?ids={id}"));
            items.push(ActionItem {
                label: if saved { "Remove from Library".into() } else { "Save Album".into() },
                kind: ActionKind::ToggleSaveAlbum { id: id.clone(), saved },
            });
            items.push(ActionItem { label: "▶  Play".into(), kind: ActionKind::Play { uri: uri.clone() } });
            items.push(ActionItem { label: "→  Open Album".into(), kind: ActionKind::Open { uri: uri.clone(), name: item.name.clone() } });
            if let Some(v) = get_json(&client, &format!("https://api.spotify.com/v1/albums/{id}"), token) {
                if let (Some(au), Some(an)) = (v["artists"][0]["uri"].as_str(), v["artists"][0]["name"].as_str()) {
                    items.push(ActionItem { label: format!("→  Go to Artist ({an})"), kind: ActionKind::Open { uri: au.to_string(), name: an.to_string() } });
                }
            }
            items.push(ActionItem { label: "⧉  Copy Link".into(), kind: ActionKind::CopyLink { uri } });
        }
        "playlist" => {
            items.push(ActionItem { label: "＋  Add to Your Library".into(), kind: ActionKind::FollowPlaylist { id } });
            items.push(ActionItem { label: "▶  Play".into(), kind: ActionKind::Play { uri: uri.clone() } });
            items.push(ActionItem { label: "→  Open".into(), kind: ActionKind::Open { uri: uri.clone(), name: item.name.clone() } });
            items.push(ActionItem { label: "⧉  Copy Link".into(), kind: ActionKind::CopyLink { uri } });
        }
        _ => {}
    }
    ActionMenu { title: item.name.clone(), items, selected: 0 }
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
    let client = reqwest::blocking::Client::new();
    match kind {
        ActionKind::ToggleLike { id, saved } => {
            let m = if saved { "DELETE" } else { "PUT" };
            if api_modify(&client, token, m, &format!("https://api.spotify.com/v1/me/tracks?ids={id}")) {
                if saved { "removed from Liked".into() } else { "added to Liked ♥".into() }
            } else {
                "action failed".into()
            }
        }
        ActionKind::Queue { uri } => {
            if api_modify(&client, token, "POST", &format!("https://api.spotify.com/v1/me/player/queue?uri={}", urlencode(&uri))) {
                "added to queue".into()
            } else {
                "queue failed (needs active playback)".into()
            }
        }
        ActionKind::AddToPlaylist { playlist_id, track_uri } => {
            if api_modify(&client, token, "POST", &format!("https://api.spotify.com/v1/playlists/{playlist_id}/tracks?uris={}", urlencode(&track_uri))) {
                "added to playlist".into()
            } else {
                "add failed".into()
            }
        }
        ActionKind::ToggleFollowArtist { id, following } => {
            let m = if following { "DELETE" } else { "PUT" };
            if api_modify(&client, token, m, &format!("https://api.spotify.com/v1/me/following?type=artist&ids={id}")) {
                if following { "unfollowed".into() } else { "following".into() }
            } else {
                "action failed".into()
            }
        }
        ActionKind::ToggleSaveAlbum { id, saved } => {
            let m = if saved { "DELETE" } else { "PUT" };
            if api_modify(&client, token, m, &format!("https://api.spotify.com/v1/me/albums?ids={id}")) {
                if saved { "removed album".into() } else { "saved album".into() }
            } else {
                "action failed".into()
            }
        }
        ActionKind::FollowPlaylist { id } => {
            if api_modify(&client, token, "PUT", &format!("https://api.spotify.com/v1/playlists/{id}/followers")) {
                "added to library".into()
            } else {
                "action failed".into()
            }
        }
        _ => String::new(),
    }
}

fn api_modify(client: &reqwest::blocking::Client, token: &str, method: &str, url: &str) -> bool {
    let req = match method {
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        _ => client.post(url),
    };
    req.bearer_auth(token)
        .header("Content-Length", "0")
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn api_contains(token: &str, url: &str) -> bool {
    let client = reqwest::blocking::Client::new();
    get_json(&client, url, token)
        .and_then(|v| v.get(0).and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// Snapshot the current session to disk (volume, last track, position, queue).
fn save_state(app: &App) {
    let s = SavedState {
        volume: app.volume,
        shuffle: app.shuffle,
        repeat: app.repeat,
        last_uri: app.now.as_ref().map(|n| n.uri.clone()).unwrap_or_default(),
        last_title: app.now.as_ref().map(|n| n.title.clone()).unwrap_or_default(),
        last_artist: app.now.as_ref().map(|n| n.artist.clone()).unwrap_or_default(),
        last_album: app.now.as_ref().map(|n| n.album.clone()).unwrap_or_default(),
        last_duration_ms: app.now.as_ref().map(|n| n.duration_ms).unwrap_or(0),
        last_position_ms: app.position_ms(),
        queue: app.queue.clone(),
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
    match ev {
        EngineEvent::TrackChanged { uri } => {
            app.status = "loading track…".to_string();
            if let Some(track_id) = track_id_from_uri(&uri) {
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
                n.position_ms = position_ms;
                n.position_at = Instant::now();
            }
        }
        EngineEvent::Paused { position_ms, .. } => {
            if let Some(n) = app.now.as_mut() {
                n.is_playing = false;
                n.position_ms = position_ms;
                n.position_at = Instant::now();
            }
        }
        EngineEvent::EndOfTrack { .. } => {}
    }
}

fn apply_meta(
    app: &mut App,
    meta: TrackMeta,
    lyrics_tx: &flume::Sender<(Vec<(u32, String)>, bool)>,
) {
    let cover = meta
        .image
        .as_ref()
        .map(|img| Cover::from_image(img.clone(), app.picker.clone()));
    app.status.clear();
    app.lyrics.clear();
    app.lyrics_synced = false;

    // Fetch synced lyrics from lrclib for the new track.
    if !meta.title.is_empty() {
        let (artist, title, album, dur) =
            (meta.artist.clone(), meta.title.clone(), meta.album.clone(), meta.duration_ms);
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
        is_playing: app.now.as_ref().map(|n| n.is_playing).unwrap_or(true),
        cover,
    });
    if let Some(theme) = meta.theme {
        app.start_fade(theme);
    }
}

// ------------------------------------------------------------------ web api

fn token_of(webapi: &Arc<Mutex<WebApi>>) -> Option<String> {
    webapi.lock().ok()?.valid_token().ok()
}

/// GET a JSON endpoint, retrying on 429 (respecting Retry-After).
fn get_json(client: &reqwest::blocking::Client, url: &str, token: &str) -> Option<serde_json::Value> {
    for _ in 0..5 {
        let resp = client.get(url).bearer_auth(token).send().ok()?;
        if resp.status().as_u16() == 429 {
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(3)
                .min(30);
            std::thread::sleep(Duration::from_secs(wait + 1));
            continue;
        }
        if !resp.status().is_success() {
            return None;
        }
        return resp.json::<serde_json::Value>().ok();
    }
    None
}

/// Fetch the library incrementally: fast sections first, Liked streamed in
/// chunks so the UI is usable within ~1s instead of waiting for everything.
fn spawn_library_fetch(webapi: Arc<Mutex<WebApi>>, tx: flume::Sender<(Section, Vec<LibItem>)>) {
    tokio::task::spawn_blocking(move || {
        let Some(token) = token_of(&webapi) else { return };
        let client = reqwest::blocking::Client::new();
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
                "artist".into(),
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
        let mut home: Vec<LibItem> = Vec::new();
        let recent5 = fetch_all_pages(&client, "https://api.spotify.com/v1/me/player/recently-played?limit=10", &token, None, 1, |it| track_from(&it["track"]));
        if !recent5.is_empty() {
            home.push(LibItem::header("Recently Played"));
            home.extend(recent5.into_iter().take(6));
        }
        let top_tracks = fetch_all_pages(&client, "https://api.spotify.com/v1/me/top/tracks?limit=10", &token, None, 1, |t| track_from(t));
        if !top_tracks.is_empty() {
            home.push(LibItem::header("Your Top Tracks"));
            home.extend(top_tracks.into_iter().take(8));
        }
        let top_artists = fetch_all_pages(&client, "https://api.spotify.com/v1/me/top/artists?limit=10", &token, None, 1, |a| artist_from(a));
        if !top_artists.is_empty() {
            home.push(LibItem::header("Your Top Artists"));
            home.extend(top_artists.into_iter().take(6));
        }
        let new_releases = fetch_all_pages(&client, "https://api.spotify.com/v1/browse/new-releases?limit=10", &token, Some("albums"), 1, |a| album_from(a));
        if !new_releases.is_empty() {
            home.push(LibItem::header("New Releases"));
            home.extend(new_releases.into_iter().take(6));
        }
        let _ = tx.send((Section::Home, home));

        let _ = tx.send((Section::Recent, fetch_all_pages(&client, "https://api.spotify.com/v1/me/player/recently-played?limit=50", &token, None, 1, |it| track_from(&it["track"]))));
        let _ = tx.send((Section::Playlists, fetch_all_pages(&client, "https://api.spotify.com/v1/me/playlists?limit=50", &token, None, 10, |it| {
            Some(LibItem::ctx(
                it["name"].as_str()?.to_string(),
                it["owner"]["display_name"].as_str().unwrap_or("").to_string(),
                it["uri"].as_str()?.to_string(),
            ))
        })));
        let _ = tx.send((Section::Albums, fetch_all_pages(&client, "https://api.spotify.com/v1/me/albums?limit=50", &token, None, 10, |it| album_from(&it["album"]))));
        let _ = tx.send((Section::Artists, fetch_all_pages(&client, "https://api.spotify.com/v1/me/following?type=artist&limit=50", &token, Some("artists"), 5, |it| artist_from(it))));

        // Liked can be huge — stream it in as pages arrive so the count climbs live.
        // Prepend Shuffle/Play action rows (shuffle first).
        let mut liked: Vec<LibItem> = vec![
            LibItem::play("🔀  Shuffle Liked Songs".into(), "myx:action:liked-shuffle".into()),
            LibItem::play("▶  Play Liked Songs".into(), "myx:action:liked-play".into()),
        ];
        let mut url = Some("https://api.spotify.com/v1/me/tracks?limit=50".to_string());
        let mut pages = 0;
        while let Some(u) = url.take() {
            if pages >= 100 {
                break;
            }
            let Some(v) = get_json(&client, &u, &token) else { break };
            for it in v["items"].as_array().into_iter().flatten() {
                if let Some(li) = track_from(&it["track"]) {
                    liked.push(li);
                }
            }
            url = v["next"].as_str().map(String::from);
            pages += 1;
            // Push a partial update every few pages.
            if pages % 3 == 0 {
                let _ = tx.send((Section::Liked, liked.clone()));
            }
        }
        let _ = tx.send((Section::Liked, liked));
    });
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
        let Some(v) = get_json(client, &u, token) else { break };
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

fn spawn_queue_fetch(webapi: Arc<Mutex<WebApi>>, tx: flume::Sender<Vec<String>>) {
    tokio::task::spawn_blocking(move || {
        let q = match token_of(&webapi) {
            Some(token) => fetch_queue_blocking(&token),
            None => Vec::new(),
        };
        let _ = tx.send(q);
    });
}

fn fetch_queue_blocking(token: &str) -> Vec<String> {
    let client = reqwest::blocking::Client::new();
    let Some(v) = get_json(&client, "https://api.spotify.com/v1/me/player/queue", token) else {
        return Vec::new();
    };
    v["queue"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|it| {
                    let name = it["name"].as_str()?;
                    let artist = it["artists"][0]["name"].as_str().unwrap_or("");
                    Some(format!("{name} — {artist}"))
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
        image: None,
        theme: None,
    };
    let Some(token) = token_of(webapi) else { return empty() };
    let client = reqwest::blocking::Client::new();
    let Some(v) = get_json(&client, &format!("https://api.spotify.com/v1/tracks/{track_id}"), &token)
    else {
        return empty();
    };

    let title = v["name"].as_str().unwrap_or("").to_string();
    let artist = v["artists"][0]["name"].as_str().unwrap_or("").to_string();
    let album = v["album"]["name"].as_str().unwrap_or("").to_string();
    let duration_ms = v["duration_ms"].as_u64().unwrap_or(0) as u32;
    let cover_url = v["album"]["images"][0]["url"].as_str().map(String::from);

    let image = cover_url.and_then(|u| {
        let bytes = client.get(u).send().ok()?.bytes().ok()?;
        image::load_from_memory(&bytes).ok()
    });
    let theme = image.as_ref().map(|img| derive_theme(img, "album ✦"));

    TrackMeta { uri, title, artist, album, duration_ms, image, theme }
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
    let client = reqwest::blocking::Client::new();
    let url = format!(
        "https://api.spotify.com/v1/search?q={}&type=track,artist,album,playlist&limit=6",
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
        .filter_map(|a| Some(LibItem::ctx(a["name"].as_str()?.to_string(), String::new(), a["uri"].as_str()?.to_string())))
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
                p["owner"]["display_name"].as_str().unwrap_or("").to_string(),
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

fn fetch_lyrics_blocking(artist: &str, title: &str, album: &str, duration_ms: u32) -> (Vec<(u32, String)>, bool) {
    let client = reqwest::blocking::Client::new();
    let url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}&album_name={}&duration={}",
        urlencode(artist),
        urlencode(title),
        urlencode(album),
        duration_ms / 1000
    );
    let Ok(resp) = client.get(&url).header("User-Agent", "myx (terminal spotify player)").send() else {
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

/// Parse LRC `[mm:ss.xx] text` lines into sorted (ms, text) pairs.
fn parse_lrc(lrc: &str) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = Vec::new();
    for line in lrc.lines() {
        // A line may carry multiple timestamps; collect them, then the trailing text.
        let mut rest = line;
        let mut stamps: Vec<u32> = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            let tag = &rest[1..end];
            if let Some(ms) = parse_lrc_stamp(tag) {
                stamps.push(ms);
            }
            rest = rest[end + 1..].trim_start();
            if stamps.is_empty() {
                break; // not a timestamp tag (e.g. metadata) — bail
            }
        }
        let text = rest.trim().to_string();
        for ms in stamps {
            out.push((ms, text.clone()));
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

fn parse_lrc_stamp(tag: &str) -> Option<u32> {
    // mm:ss.xx or mm:ss
    let (mm, rest) = tag.split_once(':')?;
    let mm: u32 = mm.parse().ok()?;
    let (ss, cs) = match rest.split_once('.') {
        Some((s, c)) => (s.parse::<u32>().ok()?, c),
        None => (rest.parse::<u32>().ok()?, "0"),
    };
    let cs: u32 = format!("{cs:0<3}")[..3].parse().unwrap_or(0);
    Some((mm * 60 + ss) * 1000 + cs)
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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

fn fetch_detail_blocking(token: &str, uri: &str, name: &str) -> (String, Vec<LibItem>) {
    let client = reqwest::blocking::Client::new();
    let mut parts = uri.split(':');
    parts.next(); // "spotify"
    let kind = parts.next().unwrap_or("");
    let id = parts.next().unwrap_or("");

    // "Play all" row first.
    let mut items = vec![LibItem::play(format!("▶ Play {name}"), uri.to_string())];

    match kind {
        "artist" => {
            // Popular tracks (already ranked by popularity).
            if let Some(v) = get_json(
                &client,
                &format!("https://api.spotify.com/v1/artists/{id}/top-tracks?market=from_token"),
                token,
            ) {
                let tracks: Vec<LibItem> = v["tracks"]
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
                if !tracks.is_empty() {
                    items.push(LibItem::header("Popular"));
                    items.extend(tracks);
                }
            }
            // Albums + singles, deduped by name, newest first, year in subtitle.
            if let Some(v) = get_json(
                &client,
                &format!("https://api.spotify.com/v1/artists/{id}/albums?include_groups=album,single&limit=50"),
                token,
            ) {
                let mut seen = std::collections::HashSet::new();
                let mut albums: Vec<(String, LibItem)> = Vec::new();
                for a in v["items"].as_array().into_iter().flatten() {
                    let (Some(aname), Some(auri)) = (a["name"].as_str(), a["uri"].as_str()) else {
                        continue;
                    };
                    if !seen.insert(aname.to_lowercase()) {
                        continue;
                    }
                    let date = a["release_date"].as_str().unwrap_or("").to_string();
                    let year = date.split('-').next().unwrap_or("").to_string();
                    albums.push((date, LibItem::ctx(aname.to_string(), year, auri.to_string())));
                }
                albums.sort_by(|x, y| y.0.cmp(&x.0)); // newest first
                if !albums.is_empty() {
                    items.push(LibItem::header("Albums"));
                    items.extend(albums.into_iter().map(|(_, it)| it));
                }
            }
        }
        "album" => {
            if let Some(v) = get_json(
                &client,
                &format!("https://api.spotify.com/v1/albums/{id}/tracks?limit=50"),
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
            if let Some(v) = get_json(
                &client,
                &format!("https://api.spotify.com/v1/playlists/{id}/tracks?limit=100"),
                token,
            ) {
                for it in v["items"].as_array().into_iter().flatten() {
                    let t = &it["track"];
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
        _ => {}
    }

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
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get("https://api.spotify.com/v1/me/player")
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
        repeat: v["repeat_state"].as_str().map(|r| r != "off").unwrap_or(false),
        volume: v["device"]["volume_percent"].as_u64().unwrap_or(50) as u8,
    })
}

/// Transfer the current server-side playback onto the myx device (with its full
/// context + queue + position). `play=false` transfers paused.
fn transfer_playback(token: &str, device_id: &str, play: bool) -> bool {
    let client = reqwest::blocking::Client::new();
    client
        .put("https://api.spotify.com/v1/me/player")
        .bearer_auth(token)
        .json(&serde_json::json!({ "device_ids": [device_id], "play": play }))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Boot restore: read the live playback state, transfer it onto myx (retrying
/// while the device registers), and hand the state back to the UI.
fn spawn_restore(
    webapi: Arc<Mutex<WebApi>>,
    device_id: String,
    tx: flume::Sender<PlaybackState>,
) {
    tokio::task::spawn_blocking(move || {
        let Some(token) = token_of(&webapi) else { return };
        let Some(state) = fetch_playback_state(&token) else { return };
        // Retry the transfer — the Connect device can take a moment to appear.
        for _ in 0..6 {
            if transfer_playback(&token, &device_id, false) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        let _ = tx.send(state);
    });
}

// --- Live playback state (server-side) end ---

fn track_id_from_uri(uri: &str) -> Option<String> {
    let mut parts = uri.split(':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("spotify"), Some("track"), Some(id)) => Some(id.to_string()),
        _ => None,
    }
}

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
    let mut header = gradient_line("myx", &[theme.primary, theme.accent]);
    if !app.status.is_empty() {
        header.push(Span::styled(format!("   {}", app.status), theme.muted()));
    }
    f.render_widget(Paragraph::new(Line::from(header)), rows[0]);
    f.render_widget(
        Paragraph::new(Line::from(view_tabs(app, theme))).alignment(Alignment::Right),
        rows[0],
    );

    let body = Layout::horizontal([Constraint::Percentage(30), Constraint::Min(24)])
        .spacing(3)
        .split(rows[2]);

    render_library(f, app, theme, body[0]);
    match app.view {
        RightView::NowPlaying => render_nowplaying_view(f, app, theme, body[1]),
        RightView::Lyrics => render_lyrics(f, app, theme, body[1]),
        RightView::Queue => render_queue_view(f, app, theme, body[1]),
    }

    render_now_strip(f, app, theme, rows[4]);
    render_footer(f, app, theme, rows[5]);

    if app.actions.is_some() {
        render_actions_overlay(f, app, theme, area);
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
            Style::default().fg(theme.primary.into()).add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        spans.push(Span::styled(v.label(), style));
    }
    spans
}

fn render_library(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    f.render_widget(Block::default().style(theme.panel()), area);
    let inner = area.inner(Margin::new(1, 0));
    if inner.height < 2 {
        return;
    }

    // Header line: drill-in title, search input/results, or section indicator.
    let head: Line = if let Some(d) = app.details.last() {
        Line::from(vec![
            Span::styled("‹ ", Style::default().fg(theme.primary.into())),
            Span::styled(truncate(&d.title, inner.width.saturating_sub(8) as usize), theme.heading()),
            Span::styled("  Esc", theme.muted()),
        ])
    } else if app.input_mode {
        Line::from(vec![
            Span::styled("search: ", theme.heading()),
            Span::styled(format!("{}▏", app.query), Style::default().fg(theme.text.into())),
        ])
    } else if app.searching {
        Line::from(vec![
            Span::styled("search: ", theme.heading()),
            Span::styled(app.query.clone(), Style::default().fg(theme.text.into())),
            Span::styled("  (Esc)", theme.muted()),
        ])
    } else {
        Line::from(vec![
            Span::styled("‹ ", theme.muted()),
            Span::styled(app.section.label(), theme.heading()),
            Span::styled(" ›", theme.muted()),
            Span::styled(
                format!("  {}/{} · {}", app.section.index() + 1, Section::ALL.len(), app.cur_items().len()),
                theme.muted(),
            ),
        ])
    };
    f.render_widget(
        Paragraph::new(head).block(Block::default().style(theme.panel())),
        Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
    );

    let list_top = inner.y + 2;
    if list_top >= inner.bottom() {
        return;
    }
    let cap = (inner.bottom() - list_top) as usize;
    let items = app.cur_items();

    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("(empty)", theme.muted())))
                .block(Block::default().style(theme.panel())),
            Rect { x: inner.x, y: list_top, width: inner.width, height: 1 },
        );
        return;
    }

    let offset = if app.selected >= cap { app.selected + 1 - cap } else { 0 };
    let max = inner.width.saturating_sub(2) as usize;

    for (row, item) in items.iter().skip(offset).take(cap).enumerate() {
        let idx = offset + row;
        let y = list_top + row as u16;
        let rect = Rect { x: inner.x, y, width: inner.width, height: 1 };

        // Header rows: a bold section label (Home feed groups), not selectable.
        if item.is_header {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    item.name.clone(),
                    Style::default().fg(theme.accent.into()).add_modifier(Modifier::BOLD),
                )))
                .block(Block::default().style(theme.panel())),
                rect,
            );
            continue;
        }

        let selected = idx == app.selected;
        let bg = if selected { theme.background_element.into() } else { theme.background_panel.into() };
        let block = left_bar_block(&theme, selected, bg);
        let style = if selected {
            Style::default().fg(theme.text.into()).add_modifier(Modifier::BOLD)
        } else {
            theme.muted()
        };
        let label = truncate(&item.name, max);
        let mut spans = vec![Span::styled(format!(" {label}"), style)];
        if !item.subtitle.is_empty() {
            let used = label.chars().count() + 1;
            let room = max.saturating_sub(used + 2);
            if room > 3 {
                spans.push(Span::styled(
                    format!("  {}", truncate(&item.subtitle, room)),
                    theme.muted(),
                ));
            }
        }
        f.render_widget(Paragraph::new(Line::from(spans)).block(block), rect);
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
    let art_rect = Rect { x: art_x, y: art_y, width: art_w, height: art_h };

    if let Some(cover) = app.now.as_mut().and_then(|n| n.cover.as_mut()) {
        cover.render(f, art_rect);
    } else {
        f.render_widget(
            Paragraph::new("♫").alignment(Alignment::Center).style(theme.muted()),
            art_rect,
        );
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
                Style::default().fg(theme.text.into()).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                truncate(&n.artist, top.width as usize),
                Style::default().fg(theme.primary.into()),
            )),
            Line::from(Span::styled(truncate(&n.album, top.width as usize), theme.muted())),
        ];
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), text_rect);
    }

    render_visualizer(f, app, theme, viz_area);
}

/// Vertically center a `height`-row rect inside `area`.
fn center_v(area: Rect, height: u16) -> Rect {
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect { x: area.x, y, width: area.width, height: height.min(area.height) }
}

/// Slim persistent bottom strip: play state + track, then the progress bar.
fn render_now_strip(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    // Track line (left) + volume meter (far right).
    let cols = Layout::horizontal([Constraint::Min(10), Constraint::Length(13)]).split(rows[0]);

    let track_line = match &app.now {
        Some(n) => {
            let (glyph, gc) = if n.is_playing { ("▶ ", theme.success) } else { ("⏸ ", theme.warning) };
            Line::from(vec![
                Span::styled(glyph, Style::default().fg(gc.into())),
                Span::styled(
                    n.title.clone(),
                    Style::default().fg(theme.text.into()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", n.artist), theme.muted()),
            ])
        }
        None => Line::from(Span::styled("— nothing playing —", theme.muted())),
    };
    f.render_widget(Paragraph::new(track_line), cols[0]);

    // Volume meter: a graduated ramp (small → tall) + percentage, right-aligned.
    const VLEV: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let filled = (app.volume as usize * VLEV.len() + 50) / 100;
    let mut vspans: Vec<Span> = Vec::with_capacity(VLEV.len() + 1);
    for (i, ch) in VLEV.iter().enumerate() {
        let color = if i < filled { theme.primary } else { theme.border_dimmest };
        vspans.push(Span::styled(ch.to_string(), Style::default().fg(color.into())));
    }
    vspans.push(Span::styled(format!(" {:>3}%", app.volume), theme.muted()));
    f.render_widget(Paragraph::new(Line::from(vspans)).alignment(Alignment::Right), cols[1]);

    render_progress(f, app, theme, rows[1]);
}

/// Convert a 0..=100 percentage to librespot's 0..=65535 volume range.
fn vol_u16(pct: u8) -> u16 {
    (pct as u32 * 65535 / 100) as u16
}

fn render_lyrics(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let inner = area.inner(Margin::new(2, 0));
    if inner.height == 0 {
        return;
    }
    if app.lyrics.is_empty() {
        let msg = if app.now.is_some() { "♪  no lyrics for this track" } else { "♪  nothing playing" };
        f.render_widget(
            Paragraph::new(msg).style(theme.muted()).alignment(Alignment::Center),
            center_v(inner, 1),
        );
        return;
    }

    let h = inner.height as usize;
    let pos = app.position_ms();
    let cur = if app.lyrics_synced {
        app.lyrics.iter().rposition(|(t, _)| *t <= pos).unwrap_or(0)
    } else {
        0
    };
    // Center the current line within the pane.
    let start = cur.saturating_sub(h / 2);
    let max = inner.width as usize;

    let mut lines: Vec<Line> = Vec::with_capacity(h);
    for (i, (_, text)) in app.lyrics.iter().enumerate().skip(start).take(h) {
        let style = if app.lyrics_synced && i == cur {
            Style::default().fg(theme.primary.into()).add_modifier(Modifier::BOLD)
        } else if app.lyrics_synced && i < cur {
            // Already-sung lines fade more than upcoming ones.
            Style::default().fg(theme.border_subtle.into())
        } else {
            theme.muted()
        };
        let txt = if text.is_empty() { "♪".to_string() } else { truncate(text, max) };
        lines.push(Line::from(Span::styled(txt, style)));
    }
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

fn render_visualizer(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let active = app.engine.bands.try_lock().map(|g| g.is_active).unwrap_or(false);
    if !active {
        return;
    }
    let Ok(guard) = app.engine.bands.try_lock() else { return };
    let values: [f32; NUM_BANDS] = guard.values;
    let peak = guard.peak_envelope.max(1e-6);
    drop(guard);

    // Cap the spectrum to a centered band — full-pane bars are too tall/wide.
    let vh = ((area.height as u32 * 3 / 5) as u16).clamp(6, 14).min(area.height);
    let vw = ((area.width as u32 * 9 / 10) as u16).clamp(24, 80).min(area.width);
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
        let vfrac = if h > 1 { from_bottom / (h - 1) as f32 } else { 0.0 };
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
    spans.extend(gradient_progress(bar_w, filled, &[theme.primary, theme.accent], theme.border_dimmest));
    spans.push(Span::styled(right, theme.muted()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_footer(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let on = |b: bool| if b { theme.success } else { theme.text_muted };
    let key = |k: &'static str| Span::styled(k, Style::default().fg(theme.primary.into()));
    let lbl = |t: &'static str| Span::styled(t, theme.muted());
    let line = Line::from(vec![
        key("⇥"), lbl(" section   "),
        key("←→"), lbl(" view   "),
        key("/"), lbl(" search   "),
        key("⏎"), lbl(" play   "),
        key("␣"), lbl(" pause   "),
        key("n/b"), lbl(" skip   "),
        key("+/-"), lbl(" vol   "),
        Span::styled("s", Style::default().fg(on(app.shuffle).into())),
        lbl(" shuffle   "),
        key("a"), lbl(" actions   "),
        key("q"), lbl(" quit"),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Context actions menu — a centered overlay list.
fn render_actions_overlay(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let Some(menu) = &app.actions else { return };
    let w = (area.width * 5 / 10).clamp(28, 52);
    let h = (menu.items.len() as u16 + 4).clamp(6, area.height.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let rect = Rect { x, y, width: w, height: h };

    f.render_widget(Clear, rect);
    f.render_widget(Block::default().style(theme.element()), rect);
    let inner = rect.inner(Margin::new(2, 1));
    let max = inner.width as usize;
    let mut lines = vec![
        Line::from(Span::styled(truncate(&menu.title, max), theme.heading())),
        Line::raw(""),
    ];
    for (i, it) in menu.items.iter().take(inner.height.saturating_sub(2) as usize).enumerate() {
        if i == menu.selected {
            lines.push(Line::from(vec![
                Span::styled("› ", Style::default().fg(theme.primary.into())),
                Span::styled(
                    truncate(&it.label, max.saturating_sub(2)),
                    Style::default().fg(theme.text.into()).add_modifier(Modifier::BOLD),
                ),
            ]));
        } else {
            lines.push(Line::from(Span::styled(format!("  {}", truncate(&it.label, max.saturating_sub(2))), theme.muted())));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
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
                Style::default().fg(theme.primary.into()).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::raw(""));
    }

    lines.push(Line::from(Span::styled("UP NEXT", theme.heading())));
    lines.push(Line::raw(""));

    let used = lines.len();
    if app.queue.is_empty() {
        lines.push(Line::from(Span::styled("queue is empty", theme.muted())));
    } else {
        for (i, q) in app.queue.iter().take(inner.height.saturating_sub(used as u16) as usize).enumerate() {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>2}  ", i + 1), theme.muted()),
                Span::styled(truncate(q, max.saturating_sub(4)), Style::default().fg(theme.text.into())),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

fn fmt_ms(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

// ------------------------------------------------------------------ terminal

fn init_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
