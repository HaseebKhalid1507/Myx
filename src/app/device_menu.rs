//! The devices menu (`d`): the account's Spotify Connect devices, to move
//! playback between them — including onto this myx, and off it.
//!
//! Its list is the Web API's (`GET /me/player/devices`, asked when the menu
//! opens and every few seconds while it stays open), kept live by the cluster
//! push: which device is active, and their volumes. The push alone isn't
//! trusted for who's there — whether it carries every device on the account
//! isn't documented — so it stands in only until the Web API has answered, or
//! when it can't.

use crate::*;

/// How often the open menu asks the Web API for the list again: a phone that
/// wakes up shows up, one that sleeps drops off. Well under the rate limit,
/// and only while the menu is on screen.
pub(crate) const DEVICES_REFRESH: Duration = Duration::from_secs(5);

/// The devices menu's own state. Everything it lists is in `DevicesState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DevicesMenu {
    /// The device under the cursor, by id: the list reorders and refreshes
    /// under it, and the cursor stays on the same device.
    pub(crate) selected: String,
    /// When the list was last asked for.
    pub(crate) asked_at: Instant,
    /// What went wrong asking, until an answer comes.
    pub(crate) error: Option<String>,
}

/// The menu's rows: the Web API's devices (or, until it answers, the push's),
/// with the push's live volumes and the active device marked, and this myx
/// always among them. This myx first, then the devices that can be driven,
/// then those that can't, each by name.
pub(crate) fn device_rows(
    listed: Option<&[Device]>,
    pushed: &[Device],
    active: Option<&str>,
    here_id: &str,
    here: (&str, u8),
) -> Vec<Device> {
    let base = match listed {
        Some(listed) if !listed.is_empty() => listed,
        _ => pushed,
    };
    let mut rows: Vec<Device> = base
        .iter()
        .map(|d| {
            let mut d = d.clone();
            if let Some(live) = pushed.iter().find(|p| p.id == d.id) {
                d.volume = live.volume;
                // Either side saying it can't be driven is enough.
                d.controllable &= live.controllable;
            }
            d.is_active = active == Some(d.id.as_str());
            d.is_this = !here_id.is_empty() && d.id == here_id;
            if d.is_this {
                // Its own volume and name are known here, and current.
                d.name = here.0.to_string();
                d.volume = Some(here.1);
                d.controllable = true;
            }
            d
        })
        .collect();
    if !rows.iter().any(|d| d.is_this) {
        rows.push(Device {
            id: here_id.to_string(),
            name: here.0.to_string(),
            kind: DeviceKind::Computer,
            volume: Some(here.1),
            is_active: active.is_some_and(|a| a == here_id),
            controllable: true,
            is_this: true,
        });
    }
    rows.sort_by_cached_key(|d| {
        (
            !d.is_this,
            !d.controllable,
            d.name.to_lowercase(),
            d.id.clone(),
        )
    });
    rows
}

/// What picking a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pick {
    /// It's playing there already: just close.
    AlreadyThere,
    /// Pull playback onto this myx from the device playing it.
    TransferHere,
    /// Nothing plays anywhere: play what's on screen, here.
    ResumeHere,
    /// Move playback to that device.
    TransferTo { device_id: String, name: String },
    /// It can't be driven from here.
    Refused(String),
}

/// What picking `row` does. `elsewhere`: another device holds playback (remote
/// mode). `here_loaded`: this myx's player has something loaded, playing or
/// paused.
pub(crate) fn device_pick(row: &Device, elsewhere: bool, here_loaded: bool) -> Pick {
    if row.is_this {
        if elsewhere {
            Pick::TransferHere
        } else if row.is_active || here_loaded {
            Pick::AlreadyThere
        } else {
            Pick::ResumeHere
        }
    } else if row.is_active {
        Pick::AlreadyThere
    } else if !row.controllable {
        Pick::Refused(format!(
            "{} can't be controlled from here",
            device_label(&row.name)
        ))
    } else {
        Pick::TransferTo {
            device_id: row.id.clone(),
            name: device_label(&row.name).to_string(),
        }
    }
}

/// The row `delta` pickable rows from `from`, staying put at either end.
/// Greyed rows are passed over; there is always one row the cursor can be on
/// (this myx).
pub(crate) fn step_pickable(rows: &[Device], from: usize, delta: isize) -> usize {
    let mut i = from as isize;
    loop {
        i += delta.signum();
        let Ok(at) = usize::try_from(i) else {
            return from;
        };
        match rows.get(at) {
            None => return from,
            Some(d) if d.controllable => return at,
            Some(_) => {}
        }
    }
}

impl App {
    /// The devices menu's rows, as they stand.
    pub(crate) fn device_rows(&self) -> Vec<Device> {
        let d = &self.devices;
        let here_volume = self.transport.volume;
        device_rows(
            d.listed.as_deref(),
            &d.pushed,
            d.active.as_deref(),
            &d.here_id,
            (d.here_name.as_str(), here_volume),
        )
    }

    /// The row under the menu's cursor: its device, else the active one, else
    /// this myx.
    pub(crate) fn device_cursor(&self, rows: &[Device]) -> usize {
        let selected = self.devices.menu.as_ref().map(|m| m.selected.as_str());
        rows.iter()
            .position(|d| Some(d.id.as_str()) == selected)
            .or_else(|| rows.iter().position(|d| d.is_active && d.controllable))
            .or_else(|| rows.iter().position(|d| d.is_this))
            .unwrap_or(0)
    }

    /// `d`: open the menu on the device that's playing, and ask for the list.
    pub(crate) fn open_devices(&mut self) {
        let selected = self.devices.active.clone().unwrap_or_default();
        self.devices.menu = Some(DevicesMenu {
            selected,
            asked_at: Instant::now(),
            error: None,
        });
        let _ = self.svc.player.send(PlayerRequest::Devices);
    }

    /// Ask for the list again if the open menu's is getting old.
    pub(crate) fn refresh_devices_if_due(&mut self, now: Instant) {
        let Some(menu) = self.devices.menu.as_mut() else {
            return;
        };
        if now.duration_since(menu.asked_at) >= DEVICES_REFRESH {
            menu.asked_at = now;
            let _ = self.svc.player.send(PlayerRequest::Devices);
        }
    }

    /// Is a popup over the screen? It covers the cover's pixels, which have
    /// to be sent again once it closes.
    pub(crate) fn popup_open(&self) -> bool {
        self.view.actions.is_some() || self.devices.menu.is_some()
    }
}

/// Take the Web API's device list.
pub(crate) fn apply_device_list(app: &mut App, listed: Result<Vec<Device>, String>) {
    match listed {
        Ok(listed) => {
            // As fresh as any push: the list says which device is active too.
            app.devices.active = listed.iter().find(|d| d.is_active).map(|d| d.id.clone());
            app.devices.listed = Some(listed);
            if let Some(menu) = app.devices.menu.as_mut() {
                menu.error = None;
            }
        }
        Err(e) => {
            liblog(format!("devices: couldn't list them: {e}"));
            app.status = format!("couldn't list devices: {e}");
            if let Some(menu) = app.devices.menu.as_mut() {
                menu.error = Some(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str, kind: DeviceKind) -> Device {
        Device {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            volume: Some(50),
            is_active: false,
            controllable: true,
            is_this: false,
        }
    }

    const HERE: &str = "here";

    fn rows(listed: Option<&[Device]>, pushed: &[Device], active: Option<&str>) -> Vec<Device> {
        device_rows(listed, pushed, active, HERE, ("myx · avante", 80))
    }

    fn names(rows: &[Device]) -> Vec<&str> {
        rows.iter().map(|d| d.name.as_str()).collect()
    }

    #[test]
    fn this_myx_comes_first_then_what_can_be_driven_then_the_rest_by_name() {
        let mut sonos = device("s", "sonos", DeviceKind::Speaker);
        sonos.controllable = false;
        let listed = [
            sonos,
            device("t", "Living Room TV", DeviceKind::Tv),
            device("p", "Pixel 7", DeviceKind::Phone),
            device("a", "avante", DeviceKind::Computer),
        ];
        let rows = rows(Some(&listed[..]), &[], Some("p"));
        assert_eq!(
            names(&rows),
            [
                "myx · avante",
                "avante",
                "Living Room TV",
                "Pixel 7",
                "sonos"
            ]
        );
        assert!(rows[0].is_this && rows[0].controllable);
        assert_eq!(rows[0].volume, Some(80), "this myx's own volume");
        assert!(rows[3].is_active, "the active one is marked");
        assert_eq!(rows.iter().filter(|d| d.is_active).count(), 1);
    }

    #[test]
    fn this_myx_listed_by_the_web_api_appears_once_under_its_own_name() {
        let listed = [
            device(HERE, "an old name", DeviceKind::Computer),
            device("p", "Pixel 7", DeviceKind::Phone),
        ];
        let rows = rows(Some(&listed[..]), &[], Some(HERE));
        assert_eq!(names(&rows), ["myx · avante", "Pixel 7"]);
        assert!(rows[0].is_this && rows[0].is_active);
    }

    #[test]
    fn the_push_keeps_volumes_live_and_can_grey_a_device_out() {
        let listed = [device("p", "Pixel 7", DeviceKind::Phone)];
        let mut live = device("p", "Pixel 7", DeviceKind::Phone);
        live.volume = Some(12);
        live.controllable = false;
        let rows = rows(Some(&listed[..]), &[live], None);
        let pixel = rows.iter().find(|d| d.id == "p").unwrap();
        assert_eq!(pixel.volume, Some(12));
        assert!(!pixel.controllable);
    }

    #[test]
    fn until_the_web_api_answers_the_push_lists_them() {
        let pushed = [device("p", "Pixel 7", DeviceKind::Phone)];
        assert_eq!(
            names(&rows(None, &pushed, None)),
            ["myx · avante", "Pixel 7"]
        );
        // An empty answer is no better than none.
        assert_eq!(
            names(&rows(Some(&[][..]), &pushed, None)),
            ["myx · avante", "Pixel 7"]
        );
        // Nothing anywhere: this myx, alone.
        assert_eq!(names(&rows(None, &[], None)), ["myx · avante"]);
    }

    #[test]
    fn picking_a_row_moves_playback_or_says_why_not() {
        let mut pixel = device("p", "Pixel 7", DeviceKind::Phone);
        let mut here = device(HERE, "myx", DeviceKind::Computer);
        here.is_this = true;
        assert_eq!(
            device_pick(&pixel, false, true),
            Pick::TransferTo {
                device_id: "p".into(),
                name: "Pixel 7".into()
            }
        );
        // This myx: pulled over from the device playing, else resumed here.
        assert_eq!(device_pick(&here, true, false), Pick::TransferHere);
        assert_eq!(device_pick(&here, false, false), Pick::ResumeHere);
        assert_eq!(device_pick(&here, false, true), Pick::AlreadyThere);
        here.is_active = true;
        assert_eq!(device_pick(&here, false, false), Pick::AlreadyThere);
        pixel.is_active = true;
        assert_eq!(device_pick(&pixel, true, false), Pick::AlreadyThere);
        let mut sonos = device("s", "Sonos", DeviceKind::Speaker);
        sonos.controllable = false;
        assert!(
            matches!(device_pick(&sonos, true, false), Pick::Refused(why) if why.contains("Sonos"))
        );
    }

    #[test]
    fn the_cursor_passes_over_greyed_rows_and_stops_at_the_ends() {
        let mut greyed = device("g", "g", DeviceKind::Other);
        greyed.controllable = false;
        let rows = [
            device("a", "a", DeviceKind::Computer),
            greyed.clone(),
            device("b", "b", DeviceKind::Phone),
            greyed,
        ];
        assert_eq!(step_pickable(&rows, 0, 1), 2);
        assert_eq!(step_pickable(&rows, 2, 1), 2, "only greyed rows below");
        assert_eq!(step_pickable(&rows, 2, -1), 0);
        assert_eq!(step_pickable(&rows, 0, -1), 0);
    }
}
