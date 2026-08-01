//! Mouse input: clicks, drags, and wheel scrolls, resolved against the hit
//! rects the renderer left in `FrameOut`.

use crate::*;

/// Mouse input. Mirrors `handle_key`: returns `true` when the app should quit.
pub(crate) fn handle_mouse(
    app: &mut App,
    out: &FrameOut,
    m: crossterm::event::MouseEvent,
    chans: &UiChannels,
) -> bool {
    if matches!(
        m.kind,
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left)
    ) {
        let is_down = matches!(m.kind, MouseEventKind::Down(MouseButton::Left));
        let mut consumed = false;
        // Drag the sidebar scrollbar (2-col grab target) to scroll.
        if let Some(sb) = out.hits.scroll {
            if m.column + 1 >= sb.x
                && m.column <= sb.x
                && m.row >= sb.y
                && m.row < sb.y + sb.height
                && sb.height > 0
            {
                consumed = true;
                let total = out.hits.scroll_len;
                if total > 1 {
                    let denom = sb.height.saturating_sub(1).max(1) as f32;
                    let frac = (m.row - sb.y) as f32 / denom;
                    let sel = (frac * (total - 1) as f32).round() as usize;
                    app.browse.selected = sel.min(total - 1);
                    app.normalize_selection();
                }
            }
        }
        // Click/drag the volume meter bars to set volume.
        if !consumed {
            if let Some(vr) = out.hits.vol {
                if m.row == vr.y && m.column >= vr.x && m.column < vr.x + vr.width && vr.width > 0 {
                    consumed = true;
                    let offset = (m.column - vr.x) as u32;
                    let vol = (((offset + 1) * 100) / vr.width as u32).min(100) as u8;
                    app.transport.volume = vol;
                    let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
                }
            }
        }
        // Otherwise an initial click on the progress bar seeks.
        if !consumed && is_down {
            if let Some(bar) = out.hits.bar {
                if m.row == bar.y
                    && m.column >= bar.x
                    && m.column < bar.x + bar.width
                    && bar.width > 0
                {
                    if let Some(dur) = app.playback.now.as_ref().map(|n| n.duration_ms) {
                        let frac = (m.column - bar.x) as f32 / bar.width as f32;
                        app.playback
                            .seek_to(&app.svc.engine, (frac * dur as f32) as u32);
                    }
                }
            }
        }
        // View-tab click -> switch the right pane.
        if !consumed && is_down {
            let hit = out
                .hits
                .tabs
                .iter()
                .find(|(_, r)| m.row == r.y && m.column >= r.x && m.column < r.x + r.width)
                .map(|(v, _)| *v);
            if let Some(v) = hit {
                app.view.mode = v;
                consumed = true;
            }
        }
        // Library click -> select; double-click (same row <400ms) -> activate.
        if !consumed && is_down {
            if let Some(lr) = out.hits.lib {
                if m.column >= lr.x
                    && m.column < lr.x + lr.width
                    && m.row >= lr.y
                    && m.row < lr.y + lr.height
                {
                    let idx = out.lib_offset + (m.row - lr.y) as usize;
                    let selectable = app
                        .cur_items()
                        .get(idx)
                        .map(|it| !it.is_header)
                        .unwrap_or(false);
                    if selectable {
                        app.browse.selected = idx;
                        let now = Instant::now();
                        let dbl = app
                            .session
                            .last_click
                            .map(|(r0, t0)| {
                                r0 == m.row && now.duration_since(t0) < Duration::from_millis(400)
                            })
                            .unwrap_or(false);
                        if dbl {
                            app.session.last_click = None;
                            let quit =
                                handle_key(app, KeyCode::Enter, KeyModifiers::empty(), chans);
                            if quit {
                                return true;
                            }
                        } else {
                            app.session.last_click = Some((m.row, now));
                        }
                    }
                }
            }
        }
    }
    // Scroll wheel → volume (anywhere in the window).
    if matches!(
        m.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        match m.kind {
            MouseEventKind::ScrollUp => {
                app.transport.volume = (app.transport.volume + 5).min(100);
                let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
            }
            MouseEventKind::ScrollDown => {
                app.transport.volume = app.transport.volume.saturating_sub(5);
                let _ = app.svc.engine.set_volume(vol_u16(app.transport.volume));
            }
            _ => {}
        }
    }
    false
}
