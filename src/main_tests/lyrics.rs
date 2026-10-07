//! The lyrics pane's layout: wrapped lines, the current one held mid-pane.

use crate::ui::*;

fn lyrics(lines: &[&str]) -> Vec<(u32, String)> {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| (i as u32 * 1000, l.to_string()))
        .collect()
}

/// The lyric line each row belongs to, top to bottom.
fn owners(rows: &[LyricRow]) -> Vec<usize> {
    rows.iter().map(|r| r.line).collect()
}

const SHORT: &str = "Nadie sabe lo que va a pasar";
const LONG: &str = "Y de ahora en adelante, toda' las decisiones de mi vida las voy a hacer pensando en mí y solamente en mí";

#[test]
fn short_lines_keep_one_row_each_with_the_current_one_mid_pane() {
    // What the pane did before wrapping: line `cur` on row height / 2.
    let ly = lyrics(&[SHORT; 30]);
    let rows = lyric_rows(&ly, 15, 60, 11);
    assert_eq!(owners(&rows), (10..21).collect::<Vec<_>>());
    assert_eq!(rows[5].line, 15);
}

#[test]
fn a_long_line_wraps_instead_of_being_cut() {
    let ly = lyrics(&[SHORT, LONG, SHORT]);
    let rows = lyric_rows(&ly, 0, 60, 10);
    let long: Vec<&str> = rows
        .iter()
        .filter(|r| r.line == 1)
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(long.len(), 2, "{rows:?}");
    assert_eq!(long.join(" "), LONG);
    assert!(rows.iter().all(|r| !r.text.contains('…')), "{rows:?}");
}

#[test]
fn wrapped_neighbours_do_not_move_the_current_line() {
    // Every line around the current one wraps to two rows at this width; the
    // current line must still start on the middle row.
    let mut lines = vec![LONG; 21];
    lines[10] = SHORT;
    let ly = lyrics(&lines);
    for height in [7, 10, 11, 20] {
        let rows = lyric_rows(&ly, 10, 60, height);
        assert_eq!(rows.len(), height);
        let first = rows
            .iter()
            .position(|r| r.line == 10)
            .expect("current shown");
        assert_eq!(first, height / 2, "height {height}: {:?}", owners(&rows));
    }
}

#[test]
fn a_wrapped_current_line_is_centred_as_a_block() {
    let mut lines = vec![SHORT; 21];
    lines[10] = LONG;
    let ly = lyrics(&lines);
    let rows = lyric_rows(&ly, 10, 60, 11);
    // Two rows for the current line, on rows 4 and 5 of 0..=10.
    assert_eq!(owners(&rows)[4..6], [10, 10]);
    assert_eq!(rows.len(), 11);
}

#[test]
fn near_the_start_the_current_line_sits_higher() {
    let ly = lyrics(&[SHORT; 30]);
    let rows = lyric_rows(&ly, 0, 60, 11);
    assert_eq!(rows[0].line, 0);
    assert_eq!(rows.len(), 11);
}

#[test]
fn the_last_lines_leave_the_rest_of_the_pane_empty() {
    let ly = lyrics(&[SHORT; 5]);
    let rows = lyric_rows(&ly, 4, 60, 11);
    assert_eq!(owners(&rows), [0, 1, 2, 3, 4]);
}

#[test]
fn a_pane_too_short_for_the_current_line_shows_its_start() {
    let ly = lyrics(&[SHORT, LONG, SHORT]);
    let rows = lyric_rows(&ly, 1, 40, 2);
    assert_eq!(owners(&rows), [1, 1]);
    assert!(LONG.starts_with(rows[0].text.as_str()));
}

#[test]
fn an_instrumental_gap_shows_a_note() {
    let ly = lyrics(&[SHORT, "", SHORT]);
    let rows = lyric_rows(&ly, 1, 40, 3);
    assert_eq!(rows[1].text, "♪︎");
}

#[test]
fn no_room_or_no_lyrics_draws_nothing() {
    assert!(lyric_rows(&lyrics(&[SHORT]), 0, 0, 5).is_empty());
    assert!(lyric_rows(&lyrics(&[SHORT]), 0, 40, 0).is_empty());
    assert!(lyric_rows(&[], 0, 40, 5).is_empty());
}
