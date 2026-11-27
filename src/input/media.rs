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
    // Like the keys, these drive whichever device holds playback.
    match ev {
        MediaControlEvent::Next => app.next(),
        MediaControlEvent::Previous => app.prev(),
        MediaControlEvent::Toggle => app.play_pause(radio_tx),
        MediaControlEvent::Play => app.play(radio_tx),
        MediaControlEvent::Pause => app.pause(),
        MediaControlEvent::Stop => app.stop(),
        MediaControlEvent::Seek(direction) => match direction {
            SeekDirection::Backward => app.playback.seek_step(-5_000),
            SeekDirection::Forward => app.playback.seek_step(5_000),
        },
        MediaControlEvent::SeekBy(direction, duration) => match direction {
            SeekDirection::Backward => app.playback.seek_step(-(duration.as_millis() as i64)),
            SeekDirection::Forward => app.playback.seek_step(duration.as_millis() as i64),
        },
        MediaControlEvent::SetPosition(MediaPosition(duration)) => {
            app.seek_to(duration.as_millis() as u32);
        }
        _ => {}
    }
}
