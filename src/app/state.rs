//! The rest of `App`'s parts: services, theme, transport, browse, search, view, session.

use crate::*;

/// Long-lived services the UI talks to. All three are used through `&self`
/// (the `Arc<Mutex<_>>` is only ever cloned), so grouping them costs no
/// borrow flexibility.
pub(crate) struct Services {
    /// The streaming engine — `None` when Myx booted with `--guest`.
    ///
    /// Streaming needs Premium; browsing does not. Keeping the engine optional
    /// is what lets Myx start at all without a Premium session, instead of
    /// blocking forever on an OAuth prompt it can never satisfy.
    pub(crate) engine: Option<Arc<Engine>>,
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
    pub(crate) query: String,
    pub(crate) searching: bool,
    pub(crate) search_results: Vec<LibItem>,
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
}

/// Cross-cutting session bookkeeping: what we resumed into, which metadata
/// fetch is still in flight, and the input timestamps that make Ctrl-C and
/// double-click work.
pub(crate) struct SessionState {
    pub(crate) restore_uri: Option<String>,
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
