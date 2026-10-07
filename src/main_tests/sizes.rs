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
    let combos: Vec<(RightView, Overlay)> = [RightView::Lyrics, RightView::Queue]
        .into_iter()
        .map(|v| (v, Overlay::None))
        .chain(
            [
                Overlay::None,
                Overlay::Actions,
                Overlay::Equalizer,
                Overlay::SearchPrompt,
                Overlay::FindPrompt,
            ]
            .into_iter()
            .map(|o| (RightView::NowPlaying, o)),
        )
        .collect();
    let mut term = Terminal::new(TestBackend::new(1, 1)).expect("test terminal");
    for (w, h) in sizes() {
        for zen in [false, true] {
            app.view.zen = zen;
            for &(view, overlay) in &combos {
                app.view.mode = view;
                with_overlay(&mut app, overlay);
                let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    draw(&mut term, &app, w, h)
                }));
                if caught.is_err() {
                    panic!("{label}: panicked at {w}x{h}, {view:?}, {overlay:?}, zen={zen}");
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
