use crate::*;
use ratatui::Terminal;

// -------------------------------------------------------- scroll_offset

/// Walk the cursor from `from` to `to` one row at a time, as the arrow keys
/// do, threading the offset through — the sticky viewport only makes sense
/// as a sequence. Returns the offset the walk ends on.
fn walk(mut offset: usize, from: usize, to: usize, cap: usize, total: usize) -> usize {
    let step = if to >= from { 1isize } else { -1 };
    let mut sel = from as isize;
    while sel != to as isize {
        sel += step;
        offset = scroll_offset(offset, sel as usize, cap, total, 3);
    }
    offset
}

#[test]
fn short_lists_never_scroll() {
    assert_eq!(scroll_offset(0, 4, 10, 5, 3), 0);
    assert_eq!(scroll_offset(0, 9, 10, 10, 3), 0, "total == cap still fits");
}

#[test]
fn top_of_a_long_list_stays_put() {
    // Nothing to reveal above row 0, so no margin is owed there.
    for sel in 0..=4 {
        assert_eq!(scroll_offset(0, sel, 8, 50, 3), 0, "sel={sel}");
    }
}

#[test]
fn scrolling_down_keeps_the_margin_below_the_cursor() {
    // cap=8, margin=3: the cursor may reach row 4, then the list moves.
    let offset = walk(0, 0, 20, 8, 50);
    assert_eq!(20 - offset, 4, "3 rows stay visible below the cursor");
}

#[test]
fn scrolling_back_up_keeps_the_margin_above_the_cursor() {
    let offset = walk(0, 0, 30, 8, 50);
    let offset = walk(offset, 30, 10, 8, 50);
    assert_eq!(10 - offset, 3, "3 rows stay visible above the cursor");
}

#[test]
fn the_cursor_moves_inside_the_viewport_before_the_list_does() {
    // The whole point of the sticky offset: a short move well inside the
    // window must not scroll at all.
    let offset = walk(0, 0, 30, 20, 100);
    assert_eq!(scroll_offset(offset, 20, 20, 100, 3), offset);
}

#[test]
fn the_end_of_the_list_is_reachable() {
    // The last row must still be selectable — the bottom margin cannot push
    // the viewport past the end.
    assert_eq!(walk(0, 0, 49, 8, 50), 42, "clamped to total - cap");
}

#[test]
fn the_cursor_is_always_inside_the_viewport() {
    // The invariant the renderer depends on: `selected` must be one of the
    // rows actually drawn — whatever the previous offset was, and however
    // absurd the configured margin is.
    for margin in [0usize, 3, 99] {
        for total in [1usize, 5, 40] {
            for cap in 1..12usize {
                for prev in 0..total {
                    for sel in 0..total {
                        let off = scroll_offset(prev, sel, cap, total, margin);
                        let shown = cap.min(total);
                        assert!(off <= total.saturating_sub(cap), "{margin}/{total}/{cap}");
                        assert!(
                            sel >= off && sel < off + shown,
                            "{margin}/{total}/{cap}/{prev}/{sel}"
                        );
                    }
                }
            }
        }
    }
}

// --------------------------------------------------------- render_loading

/// The rendered rows, trailing blanks trimmed, paired with their y.
fn loading_rows(w: u16, h: u16) -> Vec<(u16, String)> {
    use ratatui::backend::TestBackend;
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    term.draw(|f| render_loading(f, "connecting to Spotify", 0))
        .expect("draw");
    let buf = term.backend().buffer().clone();
    (0..h)
        .map(|y| {
            let line: String = (0..w).map(|x| buf[(x, y)].symbol()).collect();
            (y, line)
        })
        .filter(|(_, line)| !line.trim().is_empty())
        .collect()
}

#[test]
fn the_loading_screen_names_what_it_is_waiting_on() {
    let text = loading_rows(40, 12)
        .into_iter()
        .map(|(_, l)| l)
        .collect::<String>();
    assert!(text.contains("connecting to Spotify"), "{text}");
    assert!(text.contains(SPINNER[0]), "spinner missing");
    assert!(text.contains('\u{FF2D}'), "wordmark missing");
}

#[test]
fn the_loading_screen_is_centred_on_both_axes() {
    let (w, h) = (41u16, 13u16);
    let rows = loading_rows(w, h);
    assert_eq!(rows.len(), 2, "expected a wordmark row and a spinner row");
    for (y, line) in &rows {
        // Measured from the content's midpoint, not its margins: the
        // fullwidth wordmark leaves a blank continuation cell per letter.
        let first = line.chars().take_while(|c| *c == ' ').count();
        let last = line.trim_end().chars().count();
        let centre = (first + last) / 2;
        assert!(
            centre.abs_diff(w as usize / 2) <= 1,
            "row {y} centres at {centre}, screen centre {} ({line:?})",
            w / 2
        );
    }
    // Two rows of content, so the block straddles the middle of the screen.
    let mid = (rows[0].0 + rows[1].0) / 2;
    assert!(
        mid.abs_diff(h / 2) <= 1,
        "block sits at {mid}, screen mid {}",
        h / 2
    );
}

// ------------------------------------------------------------ retry_delay

#[test]
fn short_backoffs_are_waited_out_long_ones_are_not() {
    assert!(retry_delay(Some(1)).is_some());
    assert!(retry_delay(Some(5)).is_some());
    assert!(
        retry_delay(None).is_some(),
        "no header means a token bounce"
    );
    // Spotify answers with hours once a development-mode quota is spent;
    // sleeping on that froze a drill-in for minutes and failed anyway.
    assert!(retry_delay(Some(60)).is_none());
    assert!(retry_delay(Some(76_854)).is_none());
}

// ---------------------------------------------------------- scrub_target

#[test]
fn a_scrub_step_moves_by_the_step_size() {
    assert_eq!(scrub_target(10_000, 200_000, 5_000), 15_000);
    assert_eq!(scrub_target(10_000, 200_000, -5_000), 5_000);
}

#[test]
fn a_scrub_cannot_leave_the_track() {
    // Held keys walk into both walls; neither may underflow or overshoot.
    assert_eq!(scrub_target(2_000, 200_000, -5_000), 0);
    assert_eq!(scrub_target(198_000, 200_000, 5_000), 200_000);
    assert_eq!(scrub_target(0, 0, -5_000), 0);
}

// -------------------------------------------------------- drives_library

#[test]
fn zen_ignores_the_keys_that_only_move_the_hidden_library() {
    for code in [
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Enter,
        KeyCode::Char('j'),
        KeyCode::Char('/'),
        KeyCode::Char('o'),
    ] {
        assert!(drives_library(code), "{code:?} only drives the library");
    }
}

#[test]
fn zen_keeps_playback_and_the_right_hand_pane() {
    for code in [
        KeyCode::Char(' '),
        KeyCode::Char('n'),
        KeyCode::Char('b'),
        KeyCode::Char('s'),
        KeyCode::Char('z'),
        KeyCode::Char('q'),
        KeyCode::Left,
        KeyCode::Right,
        // Retargets onto the playing track rather than going quiet.
        KeyCode::Char('a'),
    ] {
        assert!(!drives_library(code), "{code:?} must still work in zen");
    }
}

// ---------------------------------------------------------- ArtRepaint

#[test]
fn a_forced_repaint_blanks_the_box_before_redrawing_it() {
    // The wipe is the whole mechanism: re-encoding is byte-identical for
    // sixel and iTerm2, so a blank frame is the only thing the diff will
    // emit — and it takes the overlay's leftovers with it.
    assert_eq!(ArtRepaint::Wipe.advance(), ArtRepaint::Draw);
    assert_eq!(ArtRepaint::Draw.advance(), ArtRepaint::Idle);
    assert_eq!(ArtRepaint::Idle.advance(), ArtRepaint::Idle);
}

// ----------------------------------------------------------- should_draw

#[test]
fn input_redraws_at_the_next_terminal_refresh() {
    // What made a held arrow key scroll in jerks: the cursor had moved but
    // the frame was held back until the idle tick came round.
    assert!(should_draw(true, false, MIN_FRAME));
    assert!(!should_draw(
        true,
        false,
        MIN_FRAME - Duration::from_millis(1)
    ));
}

#[test]
fn an_untouched_screen_redraws_rarely() {
    assert!(!should_draw(false, false, ANIM_FRAME));
    assert!(should_draw(false, false, IDLE_REDRAW));
}

#[test]
fn animation_redraws_between_the_two() {
    assert!(should_draw(false, true, ANIM_FRAME));
    assert!(!should_draw(false, true, MIN_FRAME));
}

#[test]
fn a_fade_is_long_enough_to_be_smooth_at_the_animation_rate() {
    // Smoothness has to come from duration, not frame rate: every present
    // recomposes the viewport and the inline cover shimmers at 60fps.
    let steps = FADE_MS / ANIM_FRAME.as_millis() as u64;
    assert!(steps >= 30, "only {steps} steps of recolour");
}
