//! The rest of `App`'s parts: services, theme, transport, browse, search, view, session.

use crate::*;

/// Long-lived services the UI talks to. All three are used through `&self`
/// (the `Arc<Mutex<_>>` is only ever cloned), so grouping them costs no
/// borrow flexibility.
pub(crate) struct Services {
    pub(crate) engine: Engine,
    pub(crate) picker: Picker,
    pub(crate) webapi: Arc<Mutex<WebApi>>,
}

pub(crate) const FADE_MS: u64 = 1500;

/// The palette currently on screen, plus the cross-fade walking it towards
/// the incoming track's palette. `displayed` is what every widget reads;
/// `target` is only used to snap exactly on completion.
pub(crate) struct ThemeState {
    pub(crate) displayed: Theme,
    pub(crate) target: Theme,
    pub(crate) fade: Option<ThemeFade>,
}

impl ThemeState {
    pub(crate) fn start_fade(&mut self, to: Theme) {
        self.fade = Some(ThemeFade::new(
            self.displayed,
            to,
            Duration::from_millis(FADE_MS),
        ));
        self.target = to;
    }

    pub(crate) fn advance(&mut self) {
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
pub(crate) struct Transport {
    pub(crate) shuffle: bool,
    pub(crate) repeat: bool,
    pub(crate) volume: u8, // 0..=100 (mirrors the 50% mixer default)
    pub(crate) queue: Vec<String>,
    pub(crate) queue_uris: Vec<String>,
    // Whether real playback has started this session (gates resume-on-play).
    pub(crate) playback_started: bool,
    // What's playing (context/radio/liked), for faithful resume on reboot.
    pub(crate) source: PlaySource,
    pub(crate) source_name: String,
}

/// The library browser: what's loaded, where the cursor is, and the drill-in
/// stack. The viewport offset is not here — it lives in `FrameOut`, since the
/// renderer owns it across frames.
pub(crate) struct BrowseState {
    pub(crate) library: Library,
    pub(crate) section: Section,
    pub(crate) selected: usize,
    pub(crate) sort: SortMode,
    // Drill-in stack (artist → album → …). Topmost is what's shown.
    pub(crate) details: Vec<Detail>,
}

/// The `/` search overlay: whether the prompt is capturing keys, the typed
/// query, and the results that temporarily replace the library list.
pub(crate) struct SearchState {
    pub(crate) input_mode: bool,
    // The prompt's editor. Read through `query()`, reset through `clear()`.
    pub(crate) input: tui_textarea::TextArea<'static>,
    pub(crate) searching: bool,
    // A submitted query whose results have not landed yet. `searching` means
    // "the search view is active"; this means "the wire is hot" — the empty
    // list renders "searching…" instead of "(empty)" while it's set.
    pub(crate) in_flight: bool,
    pub(crate) search_results: Vec<LibItem>,
    pub(crate) playlist_results: Option<Vec<LibItem>>,
}

impl SearchState {
    /// The typed query. First line only — the prompt is single-line (Enter is
    /// intercepted), so this also defuses any newline a paste might smuggle in.
    pub(crate) fn query(&self) -> &str {
        self.input.lines().first().map_or("", String::as_str)
    }

    /// Empty the editor (fresh buffer, cursor at column 0).
    pub(crate) fn clear(&mut self) {
        self.input = tui_textarea::TextArea::default();
    }
}

/// What the user is looking at: the right pane's mode, the zen (sidebar
/// hidden) toggle, the lyrics backing the Lyrics view, and the actions
/// overlay drawn on top of everything.
pub(crate) struct ViewState {
    // Which view fills the right pane.
    pub(crate) mode: RightView,
    // Sidebar hidden, so the right view (and its cover) gets the whole width.
    pub(crate) zen: bool,
    // Lyrics: (timestamp_ms, line). Synced when timestamps are non-zero.
    pub(crate) lyrics: Vec<(u32, String)>,
    pub(crate) lyrics_synced: bool,
    // Context actions menu overlay (opened with `a`).
    pub(crate) actions: Option<ActionMenu>,
    // Mouse position for a compact context popup; `None` keeps the centered
    // keyboard-driven actions overlay.
    pub(crate) action_anchor: Option<(u16, u16)>,
}

/// Cross-cutting session bookkeeping: what we resumed into, which metadata
/// fetch is still in flight, and the input timestamps that make Ctrl-C and
/// double-click work.
pub(crate) struct SessionState {
    pub(crate) restore_uri: Option<String>,
    pub(crate) restore_on_startup: bool,
    // Track URI whose metadata was last requested. Fetches run on separate
    // blocking tasks and can land out of order when skipping quickly, so a
    // reply for any other track is stale and must be dropped.
    pub(crate) pending_meta: Option<String>,
    // Whether we reclaimed a live server-side session (vs. local fallback).
    pub(crate) reclaimed: bool,
    // Timestamp of last Ctrl-C — a second press within 1.5s quits.
    pub(crate) last_ctrl_c: Option<Instant>,
    pub(crate) last_click: Option<(u16, Instant)>,
}
