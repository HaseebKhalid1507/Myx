//! The spectrum bars drawn under the album art.

use crate::*;

pub(crate) fn render_visualizer(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let active = app
        .svc
        .engine
        .bands
        .try_lock()
        .map(|g| g.is_active)
        .unwrap_or(false);
    if !active {
        return;
    }
    let Ok(guard) = app.svc.engine.bands.try_lock() else {
        return;
    };
    let values: [f32; NUM_BANDS] = guard.values;
    let peak = guard.peak_envelope.max(1e-6);
    drop(guard);

    // Cap the spectrum to a centered band — full-pane bars are too tall/wide.
    let vh = ((area.height as u32 * 3 / 5) as u16)
        .clamp(6, 14)
        .min(area.height);
    let vw = ((area.width as u32 * 9 / 10) as u16)
        .clamp(24, 80)
        .min(area.width);
    let vrect = Rect {
        x: area.x + area.width.saturating_sub(vw) / 2,
        y: area.y + area.height.saturating_sub(vh) / 2,
        width: vw,
        height: vh,
    };
    let w = vrect.width as usize;
    let h = vrect.height as usize;
    if w == 0 || h == 0 {
        return;
    }

    // 1. Box-average the bands into each column (anti-aliasing vs. single-pick).
    let mut cols = vec![0.0f32; w];
    for (x, c) in cols.iter_mut().enumerate() {
        let lo = x * NUM_BANDS / w;
        let hi = (((x + 1) * NUM_BANDS / w).max(lo + 1)).min(NUM_BANDS);
        let sum: f32 = values[lo..hi].iter().sum();
        let v = sum / (hi - lo) as f32;
        // Perceptual curve so quiet detail stays visible.
        *c = (v / peak).sqrt().clamp(0.0, 1.0);
    }

    // 2. Spatial smoothing — a couple of weighted passes so the envelope flows
    //    instead of spiking. This is what kills the "chopped" look.
    for _ in 0..2 {
        let src = cols.clone();
        for x in 0..w {
            let l = src[x.saturating_sub(1)];
            let r = src[(x + 1).min(w - 1)];
            cols[x] = l * 0.25 + src[x] * 0.5 + r * 0.25;
        }
    }

    // 3. Render with an eighth-block sub-cell tip and a vertical color gradient
    //    (info at the base → primary → accent at the peaks) for a smooth wash.
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let stops = [theme.info, theme.primary, theme.accent];

    let mut lines: Vec<Line> = Vec::with_capacity(h);
    for row in 0..h {
        let from_bottom = (h - 1 - row) as f32;
        let vfrac = if h > 1 {
            from_bottom / (h - 1) as f32
        } else {
            0.0
        };
        let color: ratatui::style::Color = gradient::interpolate(&stops, vfrac).into();
        let mut spans: Vec<Span> = Vec::with_capacity(w);
        for &v in &cols {
            let filled = v * h as f32 - from_bottom;
            let ch = if filled >= 1.0 {
                '█'
            } else if filled <= 0.0 {
                ' '
            } else {
                LEVELS[((filled * 8.0) as usize).clamp(1, 8) - 1]
            };
            if ch == ' ' {
                spans.push(Span::raw(" "));
            } else {
                spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), vrect);
}
