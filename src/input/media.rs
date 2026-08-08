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
            let _ = app.svc.engine.next();
        }
        MediaControlEvent::Previous => {
            let _ = app.svc.engine.prev();
        }
        MediaControlEvent::Toggle => {
            if app.transport.playback_started {
                let _ = app.svc.engine.toggle();
            } else {
                // Resume the persisted source (context/radio/liked).
                resume_source(app, radio_tx);
                app.transport.playback_started = true;
            }
        }
        MediaControlEvent::Play => {
            if app.transport.playback_started {
                let _ = app.svc.engine.play();
            } else {
                // Resume the persisted source (context/radio/liked).
                resume_source(app, radio_tx);
                app.transport.playback_started = true;
            }
        }
        MediaControlEvent::Pause => {
            let _ = app.svc.engine.pause();
        }
        MediaControlEvent::Stop => {
            app.svc.engine.stop();
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
            app.playback
                .seek_to(&app.svc.engine, duration.as_millis() as u32);
        }
        _ => {}
    }
}
