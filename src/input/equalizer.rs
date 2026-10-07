//! Input for the equalizer modal.

use crate::*;

pub(crate) fn handle_equalizer_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Char('e') => app.view.equalizer = None,
        KeyCode::Left | KeyCode::Char('h') => move_band(app, -1),
        KeyCode::Right | KeyCode::Char('l') => move_band(app, 1),
        KeyCode::Up | KeyCode::Char('k') => adjust_selected_band(app, 1),
        KeyCode::Down | KeyCode::Char('j') => adjust_selected_band(app, -1),
        KeyCode::Char('0') => set_selected_band(app, 0),
        KeyCode::Tab => cycle_preset(app, 1),
        KeyCode::BackTab => cycle_preset(app, -1),
        KeyCode::Char('r') => apply_preset(app, EqualizerPreset::Flat),
        KeyCode::Char(' ') => {
            let mut settings = app.transport.equalizer;
            settings.enabled = !settings.enabled;
            apply_settings(app, settings);
        }
        _ => {}
    }
}

pub(crate) fn handle_equalizer_mouse(
    app: &mut App,
    out: &FrameOut,
    event: crossterm::event::MouseEvent,
) {
    if !matches!(
        event.kind,
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
    ) {
        return;
    }

    let point_in = |rect: Rect| {
        event.column >= rect.x
            && event.column < rect.right()
            && event.row >= rect.y
            && event.row < rect.bottom()
    };

    if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
        if out.hits.eq_toggle.is_some_and(point_in) {
            let mut settings = app.transport.equalizer;
            settings.enabled = !settings.enabled;
            apply_settings(app, settings);
            return;
        }
        if let Some(preset) = out
            .hits
            .eq_presets
            .iter()
            .find(|(_, rect)| point_in(*rect))
            .map(|(preset, _)| *preset)
        {
            apply_preset(app, preset);
            return;
        }
    }

    if let Some(hit) = out.hits.eq_bands.iter().find(|hit| point_in(hit.rect)) {
        if let Some(overlay) = app.view.equalizer.as_mut() {
            overlay.selected_band = hit.band;
        }
        let gain = gain_from_pointer(*hit, event.column, event.row);
        set_band(app, hit.band, gain);
    }
}

fn apply_settings(app: &mut App, settings: EqualizerSettings) {
    let settings = settings.normalized();
    app.transport.equalizer = settings;
    app.svc.engine.set_equalizer(settings);
}

fn apply_preset(app: &mut App, preset: EqualizerPreset) {
    let mut settings = app.transport.equalizer;
    settings.gains_db = preset.gains_db();
    apply_settings(app, settings);
}

fn cycle_preset(app: &mut App, delta: isize) {
    let current = EqualizerPreset::from_gains(&app.transport.equalizer.gains_db)
        .and_then(|preset| EqualizerPreset::ALL.iter().position(|item| *item == preset));
    let len = EqualizerPreset::ALL.len() as isize;
    let next = match current {
        Some(index) => (index as isize + delta).rem_euclid(len) as usize,
        None if delta < 0 => EqualizerPreset::ALL.len() - 1,
        None => 0,
    };
    apply_preset(app, EqualizerPreset::ALL[next]);
}

fn move_band(app: &mut App, delta: isize) {
    if let Some(overlay) = app.view.equalizer.as_mut() {
        overlay.selected_band =
            (overlay.selected_band as isize + delta).rem_euclid(NUM_EQ_BANDS as isize) as usize;
    }
}

fn adjust_selected_band(app: &mut App, delta: i8) {
    let Some(band) = app
        .view
        .equalizer
        .as_ref()
        .map(|overlay| overlay.selected_band)
    else {
        return;
    };
    let gain = app.transport.equalizer.gains_db[band].saturating_add(delta);
    set_band(app, band, gain);
}

fn set_selected_band(app: &mut App, gain: i8) {
    let Some(band) = app
        .view
        .equalizer
        .as_ref()
        .map(|overlay| overlay.selected_band)
    else {
        return;
    };
    set_band(app, band, gain);
}

fn set_band(app: &mut App, band: usize, gain: i8) {
    let mut settings = app.transport.equalizer;
    settings.set_band(band, gain);
    apply_settings(app, settings);
}

pub(crate) fn gain_from_pointer(hit: EqualizerBandHit, column: u16, row: u16) -> i8 {
    let (offset, span) = if hit.vertical {
        (
            row.saturating_sub(hit.rect.y) as i32,
            hit.rect.height.saturating_sub(1) as i32,
        )
    } else {
        (
            column.saturating_sub(hit.rect.x) as i32,
            hit.rect.width.saturating_sub(1) as i32,
        )
    };
    if span == 0 {
        return 0;
    }
    let range = i32::from(MAX_EQ_GAIN_DB - MIN_EQ_GAIN_DB);
    let step = (offset * range + span / 2) / span;
    let gain = if hit.vertical {
        i32::from(MAX_EQ_GAIN_DB) - step
    } else {
        i32::from(MIN_EQ_GAIN_DB) + step
    };
    gain.clamp(i32::from(MIN_EQ_GAIN_DB), i32::from(MAX_EQ_GAIN_DB)) as i8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_pointer_maps_top_middle_and_bottom_to_gain_range() {
        let hit = EqualizerBandHit {
            band: 0,
            rect: Rect::new(4, 10, 3, 25),
            vertical: true,
        };
        assert_eq!(gain_from_pointer(hit, 5, 10), 12);
        assert_eq!(gain_from_pointer(hit, 5, 22), 0);
        assert_eq!(gain_from_pointer(hit, 5, 34), -12);
    }

    #[test]
    fn horizontal_pointer_maps_left_middle_and_right_to_gain_range() {
        let hit = EqualizerBandHit {
            band: 3,
            rect: Rect::new(10, 2, 25, 1),
            vertical: false,
        };
        assert_eq!(gain_from_pointer(hit, 10, 2), -12);
        assert_eq!(gain_from_pointer(hit, 22, 2), 0);
        assert_eq!(gain_from_pointer(hit, 34, 2), 12);
    }
}
