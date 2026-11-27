//! Input while the devices menu (`d`) is open.

use crate::*;

/// Handle a key while the devices menu is open.
pub(crate) fn handle_devices_key(
    app: &mut App,
    code: KeyCode,
    radio_tx: &flume::Sender<Result<Radio, String>>,
) {
    let rows = app.device_rows();
    let at = app.device_cursor(&rows);
    let delta = match code {
        KeyCode::Esc | KeyCode::Char('d') | KeyCode::Char('q') => {
            app.devices.menu = None;
            return;
        }
        KeyCode::Up | KeyCode::Char('k') => -1,
        KeyCode::Down | KeyCode::Char('j') => 1,
        KeyCode::Enter => {
            if let Some(row) = rows.get(at) {
                pick_device(app, row, radio_tx);
            }
            return;
        }
        _ => return,
    };
    let to = step_pickable(&rows, at, delta);
    if let (Some(menu), Some(row)) = (app.devices.menu.as_mut(), rows.get(to)) {
        menu.selected = row.id.clone();
    }
}

/// Move playback to `row`'s device. The menu closes once playback is on its
/// way; the push that follows shows it arriving.
fn pick_device(app: &mut App, row: &Device, radio_tx: &flume::Sender<Result<Radio, String>>) {
    let elsewhere = app.devices.remote.is_some();
    match device_pick(row, elsewhere, app.transport.playback_started) {
        Pick::AlreadyThere => {
            app.status = if row.is_this {
                "playback's already here".to_string()
            } else {
                format!("playback's already on {}", device_label(&row.name))
            };
        }
        Pick::TransferHere => {
            app.status = "moving playback here…".to_string();
            if let Err(e) = app.svc.engine.transfer_here() {
                app.status = format!("couldn't move playback here: {e:#}");
                return;
            }
        }
        Pick::ResumeHere => app.play(radio_tx),
        Pick::TransferTo { device_id, name } => {
            app.status = format!("moving playback to {name}…");
            let _ = app.svc.player.send(PlayerRequest::Command {
                device_id,
                device_name: name,
                command: PlayerCommand::Transfer,
            });
        }
        Pick::Refused(why) => {
            // The cursor can't land on such a row, but the list can change
            // under it.
            app.status = why;
            return;
        }
    }
    app.devices.menu = None;
}
