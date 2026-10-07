//! The interactive ten-band equalizer overlay.

use super::*;
use crate::*;

const NORMAL_MIN_WIDTH: u16 = 62;
const NORMAL_MIN_HEIGHT: u16 = 16;
const NOW_PLAYING_BOTTOM_ROWS: u16 = 9;
const NOW_PLAYING_TOP_INSET: u16 = 3;
const MAX_ART_HEIGHT: u16 = 14;
const ART_METADATA_ROWS: u16 = 4;

pub(crate) fn render_equalizer_overlay(
    f: &mut Frame,
    app: &App,
    out: &mut FrameOut,
    theme: Theme,
    area: Rect,
) {
    let Some(overlay) = app.view.equalizer.as_ref() else {
        return;
    };
    out.hits.eq_toggle = None;
    out.hits.eq_presets.clear();
    out.hits.eq_bands.clear();
    let rect = equalizer_rect(area, app.view.mode == RightView::NowPlaying);

    f.render_widget(Clear, rect);
    f.render_widget(Block::default().style(theme.element()), rect);
    let inner = rect.inner(Margin::new(2, 1));
    if inner.width == 0 || inner.height == 0 {
        force_area(f, rect);
        return;
    }

    render_header(f, app, out, theme, inner);
    if rect.width >= NORMAL_MIN_WIDTH && rect.height >= NORMAL_MIN_HEIGHT {
        render_presets(f, app, out, theme, inner);
        render_bands(f, app, out, theme, inner);
    } else {
        render_compact(f, app, out, theme, inner, overlay.selected_band);
    }
    force_area(f, rect);
}

/// Prefer the free area below the complete cover/metadata group, replacing the
/// visualizer while the editor is open. The constants mirror the Now Playing
/// layout without coupling this feature to its renderer; that keeps the two
/// independently mergeable. Short panes naturally select the compact controls.
fn equalizer_rect(area: Rect, reserve_now_playing: bool) -> Rect {
    let region = if reserve_now_playing {
        let top_height = area
            .height
            .saturating_sub(NOW_PLAYING_BOTTOM_ROWS)
            .saturating_sub(NOW_PLAYING_TOP_INSET);
        let art_height = top_height
            .saturating_sub(ART_METADATA_ROWS)
            .clamp(3, MAX_ART_HEIGHT);
        let group_height = art_height.saturating_add(ART_METADATA_ROWS);
        let group_y = area
            .y
            .saturating_add(NOW_PLAYING_TOP_INSET)
            .saturating_add(top_height.saturating_sub(group_height) / 2);
        let below_y = group_y.saturating_add(group_height).min(area.bottom());
        Rect::new(area.x, below_y, area.width, area.bottom() - below_y)
    } else {
        area
    };

    let width = (region.width.saturating_mul(9) / 10)
        .clamp(1, 100)
        .min(region.width);
    let height = (region.height.saturating_mul(9) / 10)
        .clamp(1, 24)
        .min(region.height);
    Rect::new(
        region.x + region.width.saturating_sub(width) / 2,
        region.y + region.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn render_header(f: &mut Frame, app: &App, out: &mut FrameOut, theme: Theme, area: Rect) {
    let row = Rect::new(area.x, area.y, area.width, 1);
    f.render_widget(Paragraph::new("EQUALIZER").style(theme.heading()), row);

    let toggle = if app.transport.equalizer.enabled {
        " ON "
    } else {
        " BYPASS "
    };
    let toggle_width = toggle.chars().count() as u16;
    let toggle_x = row.right().saturating_sub(toggle_width).max(row.x);
    let toggle_rect = Rect::new(toggle_x, row.y, row.right().saturating_sub(toggle_x), 1);
    let toggle_style = if app.transport.equalizer.enabled {
        Style::default()
            .bg(theme.success.into())
            .fg(theme.background.into())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .bg(theme.border_subtle.into())
            .fg(theme.text.into())
            .add_modifier(Modifier::BOLD)
    };
    f.render_widget(Paragraph::new(toggle).style(toggle_style), toggle_rect);
    out.hits.eq_toggle = Some(toggle_rect);

    let preset = EqualizerPreset::from_gains(&app.transport.equalizer.gains_db)
        .map(EqualizerPreset::label)
        .unwrap_or("Custom");
    let preamp = app.transport.equalizer.auto_preamp_db();
    let info = format!("{preset}  ·  AUTO {preamp:+.1} dB");
    let info_right = toggle_rect.x.saturating_sub(2);
    let info_x = row.x.saturating_add(11).min(info_right);
    let info_width = info
        .chars()
        .count()
        .min(info_right.saturating_sub(info_x) as usize) as u16;
    if info_width > 0 {
        f.render_widget(
            Paragraph::new(truncate(&info, info_width as usize))
                .style(theme.muted())
                .alignment(Alignment::Right),
            Rect::new(info_x, row.y, info_right.saturating_sub(info_x), 1),
        );
    }
}

fn render_presets(f: &mut Frame, app: &App, out: &mut FrameOut, theme: Theme, area: Rect) {
    if area.height < 3 {
        return;
    }
    let current = EqualizerPreset::from_gains(&app.transport.equalizer.gains_db);
    let widths: Vec<u16> = EqualizerPreset::ALL
        .iter()
        .map(|preset| preset.short_label().chars().count() as u16 + 2)
        .collect();
    let total = widths.iter().sum::<u16>() + EqualizerPreset::ALL.len() as u16 - 1;
    let mut x = area.x + area.width.saturating_sub(total) / 2;
    for (preset, width) in EqualizerPreset::ALL.into_iter().zip(widths) {
        let rect = Rect::new(x, area.y + 2, width, 1);
        let active = current == Some(preset);
        let style = if active {
            Style::default()
                .bg(theme.primary.into())
                .fg(theme.background.into())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .bg(theme.border_dimmest.into())
                .fg(theme.text_muted.into())
        };
        f.render_widget(
            Paragraph::new(format!(" {} ", preset.short_label())).style(style),
            rect,
        );
        out.hits.eq_presets.push((preset, rect));
        x = x.saturating_add(width + 1);
    }
}

fn render_bands(f: &mut Frame, app: &App, out: &mut FrameOut, theme: Theme, area: Rect) {
    let selected = app
        .view
        .equalizer
        .as_ref()
        .map_or(0, |overlay| overlay.selected_band.min(NUM_EQ_BANDS - 1));
    let track_top = area.y + 5;
    let hint_y = area.bottom().saturating_sub(1);
    let label_y = hint_y.saturating_sub(2);
    let track_height = label_y.saturating_sub(track_top).max(1);
    let column_width = (area.width / NUM_EQ_BANDS as u16).max(1);
    let content_width = column_width * NUM_EQ_BANDS as u16;
    let start_x = area.x + area.width.saturating_sub(content_width) / 2;

    for (band, &frequency) in EQ_FREQUENCIES_HZ.iter().enumerate() {
        let column = Rect::new(
            start_x + band as u16 * column_width,
            track_top,
            column_width,
            track_height,
        );
        let gain = app.transport.equalizer.gains_db[band];
        f.render_widget(
            Paragraph::new(format!("{gain:+}"))
                .alignment(Alignment::Center)
                .style(if band == selected {
                    theme.heading()
                } else {
                    theme.muted()
                }),
            Rect::new(column.x, track_top.saturating_sub(1), column.width, 1),
        );

        let rail_x = column.x + column.width / 2;
        let knob_y = gain_to_row(gain, track_top, track_height);
        let zero_y = gain_to_row(0, track_top, track_height);
        for y in track_top..track_top.saturating_add(track_height) {
            let between = y >= knob_y.min(zero_y) && y <= knob_y.max(zero_y);
            let color = if between {
                if band == selected {
                    theme.primary
                } else {
                    theme.success
                }
            } else {
                theme.border_dimmest
            };
            f.render_widget(
                Paragraph::new("│").style(Style::default().fg(color.into())),
                Rect::new(rail_x, y, 1, 1),
            );
        }
        let knob_width = 3.min(column.width);
        let knob_x = rail_x.saturating_sub(knob_width / 2).max(column.x);
        f.render_widget(
            Paragraph::new("━".repeat(knob_width as usize)).style(
                Style::default()
                    .fg(if band == selected {
                        theme.accent.into()
                    } else {
                        theme.text.into()
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            Rect::new(knob_x, knob_y, knob_width, 1),
        );
        f.render_widget(
            Paragraph::new(format_frequency(frequency))
                .style(if band == selected {
                    theme.heading()
                } else {
                    theme.muted()
                })
                .alignment(Alignment::Center),
            Rect::new(column.x, label_y, column.width, 1),
        );
        out.hits.eq_bands.push(EqualizerBandHit {
            band,
            rect: column,
            vertical: true,
        });
    }

    render_hints(f, theme, Rect::new(area.x, hint_y, area.width, 1));
}

fn render_compact(
    f: &mut Frame,
    app: &App,
    out: &mut FrameOut,
    theme: Theme,
    area: Rect,
    selected_band: usize,
) {
    if area.height < 3 {
        return;
    }
    let band = selected_band.min(NUM_EQ_BANDS - 1);
    let preset = EqualizerPreset::from_gains(&app.transport.equalizer.gains_db)
        .map(EqualizerPreset::label)
        .unwrap_or("Custom");
    f.render_widget(
        Paragraph::new(format!("Preset: {preset}  ·  Tab to change")).style(theme.muted()),
        Rect::new(area.x, area.y + 2, area.width, 1),
    );
    if area.height < 7 {
        return;
    }
    let gain = app.transport.equalizer.gains_db[band];
    let title_y = area.y + 4;
    f.render_widget(
        Paragraph::new(format!(
            "{} Hz  {gain:+} dB",
            format_frequency(EQ_FREQUENCIES_HZ[band])
        ))
        .style(theme.heading())
        .alignment(Alignment::Center),
        Rect::new(area.x, title_y, area.width, 1),
    );

    let track_width = area.width.saturating_sub(8).max(1);
    let track = Rect::new(
        area.x + area.width.saturating_sub(track_width) / 2,
        title_y + 2,
        track_width,
        1,
    );
    let knob = gain_to_column(gain, track.x, track.width);
    for x in track.x..track.right() {
        let color = if x <= knob {
            theme.primary
        } else {
            theme.border_dimmest
        };
        f.render_widget(
            Paragraph::new("─").style(Style::default().fg(color.into())),
            Rect::new(x, track.y, 1, 1),
        );
    }
    f.render_widget(
        Paragraph::new("◆").style(Style::default().fg(theme.accent.into())),
        Rect::new(knob, track.y, 1, 1),
    );
    out.hits.eq_bands.push(EqualizerBandHit {
        band,
        rect: track,
        vertical: false,
    });
    if area.height >= 9 {
        render_hints(
            f,
            theme,
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
        );
    }
}

fn render_hints(f: &mut Frame, theme: Theme, area: Rect) {
    f.render_widget(
        Paragraph::new("Tab preset  ←→ band  ↑↓ gain  Space bypass  e/Esc close")
            .style(theme.muted())
            .alignment(Alignment::Center),
        area,
    );
}

fn gain_to_row(gain: i8, top: u16, height: u16) -> u16 {
    if height <= 1 {
        return top;
    }
    let range = i32::from(MAX_EQ_GAIN_DB - MIN_EQ_GAIN_DB);
    let from_top = i32::from(MAX_EQ_GAIN_DB - gain.clamp(MIN_EQ_GAIN_DB, MAX_EQ_GAIN_DB));
    top + ((from_top * i32::from(height - 1) + range / 2) / range) as u16
}

fn gain_to_column(gain: i8, left: u16, width: u16) -> u16 {
    if width <= 1 {
        return left;
    }
    let range = i32::from(MAX_EQ_GAIN_DB - MIN_EQ_GAIN_DB);
    let offset = i32::from(gain.clamp(MIN_EQ_GAIN_DB, MAX_EQ_GAIN_DB) - MIN_EQ_GAIN_DB);
    left + ((offset * i32::from(width - 1) + range / 2) / range) as u16
}

fn format_frequency(frequency: f64) -> String {
    if frequency >= 1_000.0 {
        format!("{}k", (frequency / 1_000.0) as u16)
    } else {
        (frequency as u16).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_positions_cover_both_ends_and_the_midpoint() {
        assert_eq!(gain_to_row(12, 5, 25), 5);
        assert_eq!(gain_to_row(0, 5, 25), 17);
        assert_eq!(gain_to_row(-12, 5, 25), 29);
        assert_eq!(gain_to_column(-12, 10, 25), 10);
        assert_eq!(gain_to_column(0, 10, 25), 22);
        assert_eq!(gain_to_column(12, 10, 25), 34);
    }

    #[test]
    fn frequency_labels_stay_compact() {
        assert_eq!(format_frequency(31.0), "31");
        assert_eq!(format_frequency(1_000.0), "1k");
        assert_eq!(format_frequency(16_000.0), "16k");
    }

    #[test]
    fn equalizer_sits_below_the_cover_when_the_pane_has_room() {
        let pane = Rect::new(80, 4, 180, 54);
        let art = Rect::new(154, 19, 32, 14);
        let equalizer = equalizer_rect(pane, true);

        assert!(!equalizer.intersects(art));
        assert!(equalizer.y >= art.bottom() + ART_METADATA_ROWS);
        assert!(equalizer.width >= NORMAL_MIN_WIDTH);
        assert!(equalizer.height >= NORMAL_MIN_HEIGHT);
    }

    #[test]
    fn compact_equalizer_still_avoids_art_on_a_small_pane() {
        let pane = Rect::new(0, 0, 40, 25);
        let art = Rect::new(10, 3, 20, 9);
        let equalizer = equalizer_rect(pane, true);

        assert!(!equalizer.intersects(art));
        assert!(equalizer.width > 0);
        assert!(equalizer.height > 0);
    }
}
