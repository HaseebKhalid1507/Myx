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
/// The application state. `ui` reads it, `input` writes it, `api` feeds it over
/// channels. It depends on none of the three except in `app/event.rs`, which
/// still calls two fetches in `api` directly — see that module's docs.
/// Lives in the binary (not the library) because it is what this binary is.
mod app;
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
use app::*;
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

// ------------------------------------------------------------------ main

fn main() -> Result<()> {
    // `myx theme …` is a socket client, not a player: it must not authorize
    // with Spotify, start librespot, or touch the terminal. Intercepting argv
    // here — before anything else in `main` runs — is what guarantees that,
    // and it also keeps `theme` from reaching the "first positional argument
    // is a Spotify URI" path in `boot`.
    #[cfg(all(feature = "mxc", unix))]
    {
        let argv: Vec<String> = std::env::args().collect();
        if argv.get(1).is_some_and(|a| a == "theme") {
            std::process::exit(myx::mxc::cli::run(&argv[2..]));
        }
    }

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
    let outcome = runtime.block_on(boot(&mut terminal, saved, init_vol, creds, webapi, picker));
    let restored = restore_terminal(&mut terminal);
    // Say goodbye *after* the screen is back, so a subscriber that has stopped
    // reading can never hold the alternate screen open while `shutdown` waits
    // on it. On the error path the publisher was dropped inside `boot`, and
    // `Publisher`'s `Drop` sends the same `bye` — this call exists so that the
    // clean path does not depend on drop order.
    let res = match outcome {
        Ok(handle) => {
            shutdown_publisher(handle);
            Ok(())
        }
        Err(e) => Err(e),
    };
    restored?;
    res
}

/// What `boot` hands back so `main` can say goodbye on the exit path.
///
/// A type alias rather than `#[cfg]` on the signature: the non-MXC build then
/// differs in exactly one place instead of in every function that carries the
/// value through.
#[cfg(all(feature = "mxc", unix))]
type MxcHandle = Option<myx::mxc::publish::Publisher>;
#[cfg(not(all(feature = "mxc", unix)))]
type MxcHandle = ();

/// Send `bye` to every subscriber and close the socket.
#[cfg(all(feature = "mxc", unix))]
fn shutdown_publisher(handle: MxcHandle) {
    if let Some(publisher) = handle {
        publisher.shutdown(myx::mxc::ByeReason::Shutdown);
    }
}

#[cfg(not(all(feature = "mxc", unix)))]
fn shutdown_publisher(_handle: MxcHandle) {}

/// Bind the MXC theme socket, or run without one.
///
/// **Publishing is opt-out.** Album-reactive colour is a headline feature, so
/// it is on unless `MYX_NO_COLOR_SOCKET` is set to something other than `0` or
/// the empty string.
///
/// A bind failure is never fatal — not a stale socket, not a read-only
/// `XDG_RUNTIME_DIR`, not an exhausted thread limit. Myx is a music player
/// first; losing colour publishing costs a subscriber a repaint, whereas
/// refusing to start costs the user their music. Failures go to the librespot
/// log, where the rest of the optional-integration diagnostics already live.
#[cfg(all(feature = "mxc", unix))]
fn bind_publisher() -> MxcHandle {
    if std::env::var("MYX_NO_COLOR_SOCKET").is_ok_and(|v| !v.is_empty() && v != "0") {
        liblog("mxc: MYX_NO_COLOR_SOCKET set; colour publishing disabled");
        return None;
    }
    let path = myx::mxc::socket_path();
    match myx::mxc::publish::Publisher::bind(&path) {
        Ok(publisher) => {
            liblog(format!("mxc: publishing on {}", path.display()));
            Some(publisher)
        }
        Err(e) => {
            liblog(format!(
                "mxc: could not bind {} ({e}); continuing without colour publishing",
                path.display()
            ));
            None
        }
    }
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
) -> Result<MxcHandle> {
    let (ev_tx, ev_rx) = flume::unbounded::<EngineEvent>();
    let engine = with_loader(
        terminal,
        "connecting to Spotify",
        engine::run(creds, ev_tx, init_vol),
    )
    .await?
    .context("start engine")?;

    let webapi = Arc::new(Mutex::new(webapi));

    // The one positional argument is a Spotify URI. `theme` never reaches
    // here (see `main`), but the guard keeps that true if the dispatch is ever
    // compiled out.
    let startup_uri = std::env::args().nth(1).filter(|a| a != "theme");
    if let Some(uri) = startup_uri.as_ref() {
        let _ = engine.play_context(uri.clone(), false);
    }

    // Rebuild the last now-playing while the live restore is negotiated.
    let now = startup_uri
        .is_none()
        .then_some(saved.last_played.as_ref())
        .flatten()
        .map(|last_played| NowPlaying {
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

    let restore_uri = startup_uri
        .is_none()
        .then(|| saved.last_played.as_ref().map(|lp| lp.uri.clone()))
        .flatten();

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
        #[cfg(all(feature = "mxc", unix))]
        mxc: bind_publisher(),
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
            playback_started: startup_uri.is_some(),
            source: saved.source.clone(),
            source_name: saved.source_name.clone(),
        },
        search: SearchState {
            input_mode: false,
            query: String::new(),
            searching: false,
            search_results: Vec::new(),
            playlist_results: None,
        },
        view: ViewState {
            mode: RightView::NowPlaying,
            zen: false,
            lyrics: Vec::new(),
            lyrics_synced: false,
            actions: None,
            action_anchor: None,
        },
        session: SessionState {
            restore_uri,
            restore_on_startup: startup_uri.is_none(),
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
    pstate: flume::Sender<RestoreOutcome>,
    radio: flume::Sender<Result<Radio, String>>,
    libdone: flume::Sender<bool>,
}

async fn run_ui(
    terminal: &mut Term,
    mut app: App,
    ev_rx: flume::Receiver<EngineEvent>,
) -> Result<MxcHandle> {
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
    let (pstate_tx, pstate_rx) = flume::unbounded::<RestoreOutcome>();
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

    // The local snapshot is what Myx was actually playing when it closed, so it
    // wins over Spotify's server-side state, which may belong to another device.
    // Only reclaim that live state when no local track exists.
    if app.session.restore_on_startup {
        if app.playback.now.is_some() {
            resume_source(&mut app, &chans.radio);
            app.transport.playback_started = true;
        } else {
            // `spawn_restore` sends once and exits. Keep the sender in `chans`,
            // or a disconnected receiver would spin the select loop forever.
            spawn_restore(
                app.svc.webapi.clone(),
                app.svc.engine.device_id(),
                chans.pstate.clone(),
            );
        }
    }

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
    let mut art_overlay_open = app.view.actions.is_some() && app.view.action_anchor.is_none();
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
                    let new_layout = (app.view.mode, app.view.zen);
                    app.art_repaint = repaint_for_layout_change(last_layout, new_layout);
                    last_layout = new_layout;
                    dirty = true;
                }
                // The centered actions overlay can cover the art, so the cover
                // has to be sent again once it closes. The compact mouse popup
                // stays in the library pane and must not make the cover flash.
                let art_overlay = app.view.actions.is_some() && app.view.action_anchor.is_none();
                if art_overlay != art_overlay_open {
                    art_overlay_open = art_overlay;
                    if !art_overlay {
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
                if let EngineEvent::TrackChanged { uri } = &ev {
                    advance_queue(&mut app.transport.queue, &mut app.transport.queue_uris, uri);
                    let webapi = app.svc.webapi.clone();
                    let tx = chans.queue.clone();
                    tokio::spawn(async move {
                        // Connect emits TrackChanged before Spotify's queue
                        // endpoint always reflects the transition.
                        tokio::time::sleep(Duration::from_millis(750)).await;
                        spawn_queue_fetch(webapi, tx);
                    });
                }
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
                    // Resizes lose inline art. Focus only does so when tmux
                    // repaints a pane; compositor focus-follows-mouse events do
                    // not and must not make the cover flash.
                    Ok(Event::Resize(..)) => {
                        app.art_repaint = ArtRepaint::Wipe;
                    }
                    Ok(Event::FocusGained) if std::env::var_os("TMUX").is_some() => {
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
                    app.search.playlist_results = None;
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
                if let Ok(msg) = st {
                    if msg.starts_with("queue failed:") {
                        spawn_queue_fetch(app.svc.webapi.clone(), chans.queue.clone());
                    }
                    app.status = msg;
                }
                true
            }
            ps = pstate_rx.recv_async() => {
                if let Ok(outcome) = ps {
                    match outcome {
                        RestoreOutcome::Reclaimed(state) => {
                            app.session.reclaimed = true;
                            app.transport.playback_started = true;
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
                                is_playing: true,
                                cover: None,
                            });
                            let webapi = app.svc.webapi.clone();
                            let tx = chans.meta.clone();
                            let id = state.track_id.clone();
                            app.session.pending_meta = Some(format!("spotify:track:{id}"));
                            tokio::task::spawn_blocking(move || { let _ = tx.send(fetch_track_meta(&webapi, &id)); });
                            spawn_queue_fetch(app.svc.webapi.clone(), chans.queue.clone());
                        }
                        RestoreOutcome::Unavailable if app.playback.now.is_some() => {
                            resume_source(&mut app, &chans.radio);
                            app.transport.playback_started = true;
                        }
                        RestoreOutcome::Unavailable => {}
                    }
                }
                true
            }
        };
        dirty |= touched;
    }
    // Hand the publisher back to `main` so the `bye` goes out on the same path
    // that restores the terminal, rather than relying on where `App` happens
    // to be dropped.
    #[cfg(all(feature = "mxc", unix))]
    {
        Ok(app.mxc.take())
    }
    #[cfg(not(all(feature = "mxc", unix)))]
    {
        Ok(())
    }
}

pub(crate) fn advance_queue(queue: &mut Vec<String>, uris: &mut Vec<String>, current_uri: &str) {
    if uris.first().is_some_and(|uri| uri == current_uri) {
        uris.remove(0);
        if !queue.is_empty() {
            queue.remove(0);
        }
    }
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
