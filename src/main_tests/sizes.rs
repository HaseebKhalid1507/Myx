//! Every screen at every size: a whole `App` (detached engine, offline Web
//! API) drawn into a test terminal across the size range, in every view, with
//! and without each overlay, playing and not. Nothing may panic.
//!
//! Needs `--features test-support` (CI's `--all-features` has it).

#![cfg(feature = "test-support")]

use crate::*;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

/// A whole app, as `boot` builds one, minus Spotify: nothing loaded, nothing
/// playing. Tests set what they need on top.
pub(crate) fn test_app() -> App {
    App {
        svc: Services {
            engine: Engine::detached(),
            cell: ratatui_image::FontSize::new(10, 22),
            picker: Picker::halfblocks(),
            webapi: Arc::new(Mutex::new(WebApi::offline())),
        },
        media_controls: None,
        #[cfg(all(feature = "mxc", unix))]
        mxc: None,
        playback: PlaybackState {
            now: None,
            seek_target: None,
            seek_last_step: Instant::now(),
            seek_last_input: Instant::now(),
        },
        // Not `ThemeState::at_startup`: that reads the user's config file.
        theme: ThemeState {
            displayed: TOKYONIGHT,
            target: TOKYONIGHT,
            fade: None,
            fixed: false,
            notice: None,
        },
        status: String::new(),
        browse: BrowseState {
            library: Library::default(),
            section: Section::Home,
            selected: 0,
            sort: SortMode::Added,
            details: Vec::new(),
        },
        transport: Transport {
            shuffle: false,
            repeat: false,
            volume: 80,
            queue: Vec::new(),
            queue_uris: Vec::new(),
            playback_started: false,
            resume_after_reconnect: false,
            source: PlaySource::None,
            source_name: String::new(),
            equalizer: Default::default(),
        },
        search: SearchState {
            input_mode: false,
            input: Default::default(),
            searching: false,
            in_flight: false,
            search_results: Vec::new(),
        },
        find: FindState::default(),
        view: ViewState {
            mode: RightView::NowPlaying,
            zen: false,
            layout: LayoutMode::Full,
            lyrics: Vec::new(),
            lyrics_synced: false,
            actions: None,
            equalizer: None,
            cell_settling: false,
        },
        session: SessionState {
            restore_uri: None,
            pending_meta: None,
            last_ctrl_c: None,
            last_click: None,
        },
        art_repaint: ArtRepaint::Idle,
    }
}

/// `test_app` with a library, a track playing (cover, synced lyrics) and a queue.
pub(crate) fn playing_app() -> App {
    let mut app = test_app();
    let tracks: Vec<LibItem> = (0..40)
        .map(|i| {
            LibItem::track(
                format!("A fairly long track title number {i}"),
                "Some Artist feat. Another".to_string(),
                format!("spotify:track:{i}"),
            )
        })
        .collect();
    let mut home = vec![LibItem::header("Recently Played")];
    home.extend(tracks.iter().take(12).cloned());
    app.browse.library.set(Section::Home, home);
    app.browse.library.set(Section::Liked, tracks);
    let cover = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(8, 8, |x, y| {
        image::Rgb([(x * 30) as u8, (y * 30) as u8, 120])
    }));
    app.playback.now = Some(NowPlaying {
        uri: "spotify:track:0".to_string(),
        title: "RUNNING OUT OF TIME (a title long enough to need cutting)".to_string(),
        artist: "Tyler, The Creator".to_string(),
        album: "IGOR".to_string(),
        duration_ms: 177_000,
        position_ms: 15_000,
        position_at: Instant::now(),
        is_playing: true,
        cover: Some(Cover::from_image(cover, Picker::halfblocks())),
    });
    app.transport.playback_started = true;
    app.view.lyrics = (0..30)
        .map(|i| {
            (
                i * 4_000,
                format!("a lyric line, number {i}, long enough to wrap somewhere"),
            )
        })
        .collect();
    app.view.lyrics_synced = true;
    app.transport.queue = (0..20)
        .map(|i| format!("Queued track {i} · Artist"))
        .collect();
    app.transport.source_name = "AFTERPARTY".to_string();
    app
}

/// Draw `app` into `term` (resized to `w`×`h`).
fn draw(term: &mut Terminal<TestBackend>, app: &App, w: u16, h: u16) {
    term.backend_mut().resize(w, h);
    term.resize(ratatui::layout::Rect::new(0, 0, w, h))
        .expect("resize");
    let mut out = FrameOut::default();
    term.draw(|f| render(f, app, &mut out, ArtRepaint::Idle))
        .unwrap_or_else(|e| panic!("draw at {w}x{h}: {e}"));
}

/// Dense where small screens break, sparse above.
fn sizes() -> impl Iterator<Item = (u16, u16)> {
    let ws = (1..=40).chain((45..=120).step_by(5)).chain([160, 200]);
    let hs: Vec<u16> = (1..=20).chain((22..=50).step_by(4)).collect();
    ws.flat_map(move |w| hs.clone().into_iter().map(move |h| (w, h)))
}

#[derive(Clone, Copy, Debug)]
enum Overlay {
    None,
    Actions,
    Equalizer,
    SearchPrompt,
    FindPrompt,
}

fn with_overlay(app: &mut App, overlay: Overlay) {
    app.view.actions = None;
    app.view.equalizer = None;
    app.search.input_mode = false;
    app.find.typing = false;
    match overlay {
        Overlay::None => {}
        Overlay::Actions => {
            let item = LibItem::track("x".into(), "y".into(), "spotify:track:x".into());
            app.view.actions = Some(build_action_menu(None, &item));
        }
        Overlay::Equalizer => app.view.equalizer = Some(EqualizerOverlay::default()),
        Overlay::SearchPrompt => app.search.input_mode = true,
        Overlay::FindPrompt => {
            let list = app.list_key();
            app.find.open(list);
        }
    }
}

fn sweep(mut app: App, label: &str) {
    // Same 1:2 cell shape as a real terminal, tiny in pixels: the cover is
    // re-encoded for every size, and at 10x22 px a cell that's thousands of
    // pixels per frame for nothing a crash test needs.
    app.svc.cell = ratatui_image::FontSize::new(1, 2);
    // Overlays sit over the right view; only Now Playing reshapes for them.
    let combos: Vec<(LayoutMode, RightView, Overlay)> = [LayoutMode::Full, LayoutMode::Focus]
        .into_iter()
        .flat_map(|layout| {
            let views = [RightView::Library, RightView::Lyrics, RightView::Queue]
                .into_iter()
                .filter(move |v| layout == LayoutMode::Focus || *v != RightView::Library)
                .map(move |v| (layout, v, Overlay::None));
            let overlays = [
                Overlay::None,
                Overlay::Actions,
                Overlay::Equalizer,
                Overlay::SearchPrompt,
                Overlay::FindPrompt,
            ]
            .into_iter()
            .map(move |o| (layout, RightView::NowPlaying, o));
            views.chain(overlays)
        })
        .collect();
    let mut term = Terminal::new(TestBackend::new(1, 1)).expect("test terminal");
    for (w, h) in sizes() {
        for zen in [false, true] {
            app.view.zen = zen;
            for &(layout, view, overlay) in &combos {
                app.view.layout = layout;
                app.view.mode = view;
                with_overlay(&mut app, overlay);
                let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    draw(&mut term, &app, w, h)
                }));
                if caught.is_err() {
                    panic!(
                        "{label}: panicked at {w}x{h}, {layout:?}, {view:?}, {overlay:?}, zen={zen}"
                    );
                }
            }
        }
    }
}

#[test]
fn nothing_playing_never_panics_at_any_size() {
    sweep(test_app(), "nothing playing");
}

#[test]
fn playing_never_panics_at_any_size() {
    sweep(playing_app(), "playing");
}

/// The whole screen as text, row by row.
fn screen_text(term: &Terminal<TestBackend>) -> String {
    let buf = term.backend().buffer();
    let area = buf.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Somewhere in `text` an `m:ss` time — the position moves while a track plays.
fn has_time(text: &str) -> bool {
    let b = text.as_bytes();
    b.windows(4).any(|w| {
        w[0].is_ascii_digit() && w[1] == b':' && w[2].is_ascii_digit() && w[3].is_ascii_digit()
    })
}

#[test]
fn what_is_playing_survives_every_size() {
    // However small the screen, if a track is playing you can see which, and
    // where in it you are: the view shows the title, else the strip does, else
    // the progress row carries it. (Below 24 columns there's no room to promise
    // a readable title.)
    let mut app = playing_app();
    app.svc.cell = ratatui_image::FontSize::new(1, 2);
    let mut term = Terminal::new(TestBackend::new(1, 1)).expect("test terminal");
    let cases = [
        (LayoutMode::Full, RightView::NowPlaying),
        (LayoutMode::Full, RightView::Lyrics),
        (LayoutMode::Full, RightView::Queue),
        (LayoutMode::Focus, RightView::Library),
        (LayoutMode::Focus, RightView::NowPlaying),
        (LayoutMode::Focus, RightView::Lyrics),
        (LayoutMode::Focus, RightView::Queue),
    ];
    for zen in [false, true] {
        app.view.zen = zen;
        for (layout, view) in cases {
            app.view.layout = layout;
            app.view.mode = view;
            for w in (24..=120).step_by(3) {
                for h in 1..=50 {
                    draw(&mut term, &app, w, h);
                    let text = screen_text(&term);
                    assert!(
                        text.contains("RUNNING"),
                        "no title at {w}x{h}, {layout:?} {view:?}, zen={zen}:\n{text}"
                    );
                    assert!(
                        has_time(&text),
                        "no time at {w}x{h}, {layout:?} {view:?}, zen={zen}:\n{text}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_footer_never_shows_a_hint_cut_short() {
    let mut app = playing_app();
    app.svc.cell = ratatui_image::FontSize::new(1, 2);
    let mut term = Terminal::new(TestBackend::new(1, 1)).expect("test terminal");
    // Every hint the footer can show, whole. Hints are separated by three
    // spaces, so each three-space-separated piece must be one of these.
    let whole = [
        "⇥ section",
        "←→ view",
        "/ search",
        "f find",
        "⏎ select",
        "⏎ open",
        "⇧⏎ play",
        "S shuffle",
        "␣ play",
        "␣ pause",
        "n/b skip",
        "⇧←→ seek",
        "o sort",
        "+/- vol",
        "s shuffle",
        "a actions",
        "e eq",
        "z zen",
        "q quit",
    ];
    for w in 10..=200u16 {
        draw(&mut term, &app, w, 30);
        let text = screen_text(&term);
        // The last row with anything on it (an outer margin row may follow).
        let footer = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .expect("a footer row")
            .trim();
        for piece in footer.split("   ").map(str::trim).filter(|p| !p.is_empty()) {
            assert!(
                whole.contains(&piece),
                "cut hint {piece:?} at width {w}: {footer:?}"
            );
        }
    }
}

// ------------------------------------------------------------------ layouts

#[test]
fn full_when_it_fits_focus_when_it_doesnt() {
    use LayoutMode::*;
    assert_eq!(
        choose_layout(FULL_MIN_COLS, FULL_MIN_ROWS, Focus, None),
        Full
    );
    assert_eq!(choose_layout(FULL_MIN_COLS - 1, 40, Focus, None), Focus);
    assert_eq!(choose_layout(200, FULL_MIN_ROWS - 1, Focus, None), Focus);
    // A forced layout holds at any size.
    assert_eq!(choose_layout(20, 5, Full, Some(Full)), Full);
    assert_eq!(choose_layout(200, 60, Full, Some(Focus)), Focus);
}

#[test]
fn a_size_on_the_edge_does_not_flip_the_layout() {
    use LayoutMode::*;
    // Wobbling a column either side of the threshold, as a drag or a zoom
    // step does, keeps whichever layout it started in.
    let mut layout = choose_layout(FULL_MIN_COLS, 30, Focus, None);
    assert_eq!(layout, Full);
    for cols in [
        FULL_MIN_COLS - 1,
        FULL_MIN_COLS,
        FULL_MIN_COLS - 2,
        FULL_MIN_COLS - 1,
    ] {
        layout = choose_layout(cols, 30, layout, None);
        assert_eq!(layout, Full, "{cols} cols");
    }
    // Only well below it does Full give way — and coming back needs the full
    // threshold again.
    layout = choose_layout(FULL_MIN_COLS - 3, 30, layout, None);
    assert_eq!(layout, Focus);
    layout = choose_layout(FULL_MIN_COLS - 1, 30, layout, None);
    assert_eq!(layout, Focus);
}

#[test]
fn the_layout_setting_reads_leniently() {
    assert_eq!(forced_layout("auto"), None);
    assert_eq!(forced_layout(""), None);
    assert_eq!(forced_layout(" Full "), Some(LayoutMode::Full));
    assert_eq!(forced_layout("focus"), Some(LayoutMode::Focus));
    assert_eq!(forced_layout("compact"), None, "unknown: auto");
}

#[test]
fn library_is_a_view_only_in_focus_and_never_under_zen() {
    let mut app = test_app();
    assert_eq!(app.rotation(), RightView::VIEWS);
    app.view.layout = LayoutMode::Focus;
    assert_eq!(app.rotation(), RightView::WITH_LIBRARY);
    app.view.zen = true;
    assert_eq!(app.rotation(), RightView::VIEWS, "zen: no library anywhere");
}

#[test]
fn leaving_focus_or_turning_zen_on_takes_you_off_the_library_view() {
    let mut app = test_app();
    app.view.layout = LayoutMode::Focus;
    app.view.mode = RightView::Library;
    app.settle_view();
    assert_eq!(app.view.mode, RightView::Library, "still valid");
    app.view.layout = LayoutMode::Full;
    app.settle_view();
    assert_eq!(app.view.mode, RightView::NowPlaying);

    app.view.layout = LayoutMode::Focus;
    app.view.mode = RightView::Library;
    app.view.zen = true;
    app.settle_view();
    assert_eq!(app.view.mode, RightView::NowPlaying);
}

// ------------------------------------------------------------- keys in Focus

fn chans() -> UiChannels {
    UiChannels {
        meta: flume::unbounded().0,
        lib: flume::unbounded().0,
        queue: flume::unbounded().0,
        search: flume::unbounded().0,
        lyrics: flume::unbounded().0,
        detail: flume::unbounded().0,
        menu: flume::unbounded().0,
        astatus: flume::unbounded().0,
        radio: flume::unbounded().0,
        libdone: flume::unbounded().0,
    }
}

fn press(app: &mut App, code: KeyCode) {
    handle_key(app, code, KeyModifiers::empty(), &chans());
}

fn focus_app() -> App {
    let mut app = playing_app();
    app.transport.playback_started = false; // no spawned fetches in a unit test
    app.view.layout = LayoutMode::Focus;
    app.browse.section = Section::Liked;
    app.browse.selected = 0;
    app
}

#[test]
fn arrows_step_through_the_library_view_in_focus() {
    let mut app = focus_app();
    app.view.mode = RightView::Queue;
    press(&mut app, KeyCode::Right);
    assert_eq!(app.view.mode, RightView::Library);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.view.mode, RightView::NowPlaying);
    press(&mut app, KeyCode::Left);
    assert_eq!(app.view.mode, RightView::Library);
    // Full has no Library view: Queue wraps straight to Now Playing.
    app.view.layout = LayoutMode::Full;
    app.view.mode = RightView::Queue;
    press(&mut app, KeyCode::Right);
    assert_eq!(app.view.mode, RightView::NowPlaying);
}

#[test]
fn library_keys_do_nothing_while_the_library_is_off_screen() {
    let mut app = focus_app();
    app.view.mode = RightView::NowPlaying;
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Down);
    assert_eq!(app.browse.selected, 0, "moved a selection nobody can see");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.browse.section, Section::Liked);
    press(&mut app, KeyCode::Char('f'));
    assert!(!app.find.typing);
    // On the Library view they work.
    app.view.mode = RightView::Library;
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.browse.selected, 1);
}

#[test]
fn search_brings_the_library_up_in_focus_but_not_under_zen() {
    let mut app = focus_app();
    app.view.mode = RightView::Lyrics;
    press(&mut app, KeyCode::Char('/'));
    assert_eq!(app.view.mode, RightView::Library);
    assert!(app.search.input_mode, "the prompt is open");

    let mut app = focus_app();
    app.view.zen = true;
    app.view.mode = RightView::NowPlaying;
    press(&mut app, KeyCode::Char('/'));
    assert_eq!(app.view.mode, RightView::NowPlaying);
    assert!(!app.search.input_mode, "zen: no library, no search");
}

#[test]
fn zen_in_focus_leaves_the_library_view_and_keeps_it_out() {
    let mut app = focus_app();
    app.view.mode = RightView::Library;
    press(&mut app, KeyCode::Char('z'));
    assert!(app.view.zen);
    assert_eq!(app.view.mode, RightView::NowPlaying);
    for _ in 0..4 {
        press(&mut app, KeyCode::Right);
        assert_ne!(app.view.mode, RightView::Library);
    }
}
