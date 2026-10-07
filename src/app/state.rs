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
    /// A palette picked in the config (`theme = "…"`) stays put; covers only
    /// move the UI's colours when this is false.
    pub(crate) fixed: bool,
    /// What went wrong reading the configured theme, shown once in the status
    /// line when the library has loaded — the first moment it won't be
    /// overwritten straight away.
    pub(crate) notice: Option<String>,
}

/// `theme = "…"` from the config, resolved once: theme files and pywal's
/// colours are read at startup, not on every frame.
pub(crate) fn chosen_theme() -> &'static myx::user_theme::Resolved {
    static CHOSEN: std::sync::OnceLock<myx::user_theme::Resolved> = std::sync::OnceLock::new();
    CHOSEN.get_or_init(|| {
        myx::user_theme::resolve(
            &myx::config::get().theme,
            &myx::user_theme::Sources::from_env(),
        )
    })
}

/// The palette before any cover has arrived — or for good, with a fixed theme —
/// with the background as configured.
pub(crate) fn startup_theme() -> Theme {
    let palette = match chosen_theme().choice {
        myx::user_theme::Choice::Fixed(theme) => theme,
        myx::user_theme::Choice::Album => TOKYONIGHT,
    };
    Theme {
        transparent: myx::config::get().transparent,
        ..palette
    }
}

impl ThemeState {
    pub(crate) fn at_startup() -> Self {
        let chosen = chosen_theme();
        for warning in &chosen.warnings {
            liblog(warning);
        }
        let notice = chosen
            .warnings
            .first()
            .map(|first| match chosen.warnings.len() {
                1 => first.clone(),
                n => format!("{first} (+{} more)", n - 1),
            });
        Self {
            displayed: startup_theme(),
            target: startup_theme(),
            fade: None,
            fixed: matches!(chosen.choice, myx::user_theme::Choice::Fixed(_)),
            notice,
        }
    }

    /// A new cover's palette: faded to, unless the config fixed the theme.
    pub(crate) fn follow_cover(&mut self, to: Theme) {
        if !self.fixed {
            self.start_fade(to);
        }
    }

    pub(crate) fn start_fade(&mut self, to: Theme) {
        // A cover's palette knows nothing of the config; the startup flag rides along.
        let to = Theme {
            transparent: self.target.transparent,
            ..to
        };
        self.fade = Some(ThemeFade::new(
            self.displayed,
            to,
            Duration::from_millis(FADE_MS),
        ));
        self.target = to;
    }

    /// Flip the background between the terminal's and the palette's. A running
    /// fade is restarted towards the same palette so it lands with the new flag.
    pub(crate) fn set_transparent(&mut self, on: bool) {
        self.displayed.transparent = on;
        self.target.transparent = on;
        if self.fade.is_some() {
            self.start_fade(self.target);
        }
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
    // Audio was playing when the access point dropped, so the replacement
    // Connect device should pick the source back up once it is ready.
    pub(crate) resume_after_reconnect: bool,
    // What's playing (context/radio/liked), for faithful resume on reboot.
    pub(crate) source: PlaySource,
    pub(crate) source_name: String,
    /// Local DSP state. Kept with the transport controls because it affects the
    /// audio path and is persisted alongside volume/shuffle/repeat.
    pub(crate) equalizer: EqualizerSettings,
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

/// Which list is on screen: a library section, the search results, or the page
/// at some depth of the drill-in stack. Find remembers the one it was opened on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum ListKey {
    Section(Section),
    Search,
    Page(usize, String),
}

/// `f`: find in the list on screen. A lens, not an edit — rows that don't
/// match are skipped like headers are, so picking a match plays exactly what
/// it would have played unfiltered. It belongs to the list it was opened on
/// and lapses the moment another list is on screen.
#[derive(Default)]
pub(crate) struct FindState {
    /// The prompt is taking keys.
    pub(crate) typing: bool,
    pub(crate) input: tui_textarea::TextArea<'static>,
    /// The list the query was typed for.
    pub(crate) list: Option<ListKey>,
}

impl FindState {
    /// The typed query (the prompt is single-line).
    pub(crate) fn query(&self) -> &str {
        self.input.lines().first().map_or("", String::as_str)
    }

    /// The query in force on list `key`: only the list it was typed for, and
    /// only once there is something in it.
    pub(crate) fn query_for(&self, key: &ListKey) -> Option<&str> {
        let q = self.query().trim();
        (self.list.as_ref() == Some(key) && !q.is_empty()).then_some(q)
    }

    /// Start typing on list `key`. Reopening on the same list edits the query
    /// already there; on another list it starts empty.
    pub(crate) fn open(&mut self, key: ListKey) {
        if self.list.as_ref() != Some(&key) {
            self.input = Default::default();
            self.list = Some(key);
        }
        self.typing = true;
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Drop a find whose list has left the screen, so coming back to that
    /// list later shows all of it again.
    pub(crate) fn forget_unless(&mut self, key: &ListKey) {
        if self.list.as_ref().is_some_and(|l| l != key) {
            self.clear();
        }
    }
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
    // Ten-band equalizer overlay (opened with `e`).
    pub(crate) equalizer: Option<EqualizerOverlay>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct EqualizerOverlay {
    pub(crate) selected_band: usize,
}

/// Cross-cutting session bookkeeping: which metadata fetch is still in flight
/// and the input timestamps that make Ctrl-C and double-click work.
pub(crate) struct SessionState {
    pub(crate) restore_uri: Option<String>,
    // Track URI whose metadata was last requested. Fetches run on separate
    // blocking tasks and can land out of order when skipping quickly, so a
    // reply for any other track is stale and must be dropped.
    pub(crate) pending_meta: Option<String>,
    // Timestamp of last Ctrl-C — a second press within 1.5s quits.
    pub(crate) last_ctrl_c: Option<Instant>,
    pub(crate) last_click: Option<(u16, Instant)>,
}
