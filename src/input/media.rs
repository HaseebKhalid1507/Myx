//! OS media-key input (souvlaki media controls).

use crate::*;

pub(crate) fn consume_media_event<T>(
    event: Result<T, flume::RecvError>,
    open: &mut bool,
) -> Option<T> {
    match event {
        Ok(event) => Some(event),
        Err(_) => {
            *open = false;
            None
        }
    }
}

pub(crate) fn handle_media_control_event(
    app: &mut App,
    ev: MediaControlEvent,
    radio_tx: &flume::Sender<Result<Radio, String>>,
) {
    match ev {
        MediaControlEvent::Next => {
            app.engine_do(|e| {
                let _ = e.next();
            });
        }
        MediaControlEvent::Previous => {
            app.engine_do(|e| {
                let _ = e.prev();
            });
        }
        MediaControlEvent::Toggle => {
            if app.transport.playback_started {
                app.engine_do(|e| { let _ = e.toggle(); });
            } else if app.session.reclaimed {
                // Resume the reclaimed server-side context (full queue intact).
                app.engine_do(|e| { let _ = e.play(); });
                app.transport.playback_started = true;
            } else {
                // No live session — resume the persisted source (context/radio/liked).
                resume_source(app, radio_tx);
                app.transport.playback_started = true;
            }
        }
        MediaControlEvent::Play => {
            if app.transport.playback_started {
                app.engine_do(|e| { let _ = e.play(); });
            } else if app.session.reclaimed {
                // Resume the reclaimed server-side context (full queue intact).
                app.engine_do(|e| { let _ = e.play(); });
                app.transport.playback_started = true;
            } else {
                // No live session — resume the persisted source (context/radio/liked).
                resume_source(app, radio_tx);
                app.transport.playback_started = true;
            }
        }
        MediaControlEvent::Pause => {
            app.engine_do(|e| {
                let _ = e.pause();
            });
        }
        MediaControlEvent::Stop => {
            app.engine_do(|e| e.stop());
        }
        MediaControlEvent::Seek(direction) => match direction {
            SeekDirection::Backward => app.playback.seek_step(-5_000),
            SeekDirection::Forward => app.playback.seek_step(5_000),
        },
        MediaControlEvent::SeekBy(direction, duration) => match direction {
            SeekDirection::Backward => app.playback.seek_step(-(duration.as_millis() as i64)),
            SeekDirection::Forward => app.playback.seek_step(duration.as_millis() as i64),
        },
        MediaControlEvent::SetPosition(MediaPosition(duration)) => {
            let engine = app.svc.engine.clone();
            let mut do_seek = |p: u32| {
                if let Some(e) = engine.as_ref() {
                    let _ = e.seek(p);
                }
            };
            app.playback
                .seek_to(&mut do_seek, duration.as_millis() as u32);
        }
        _ => {}
    }
}
