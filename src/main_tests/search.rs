//! The `/` search prompt: the tui-textarea editor behind it and the cursor
//! glyph placement in the header line.

use crate::ui::split_at_cursor;
use crate::*;
use crossterm::event::KeyEvent;

fn state() -> SearchState {
    SearchState {
        input_mode: true,
        input: Default::default(),
        searching: false,
        search_results: Vec::new(),
    }
}

fn key(s: &mut SearchState, code: KeyCode, mods: KeyModifiers) {
    s.input.input(KeyEvent::new(code, mods));
}

fn type_str(s: &mut SearchState, text: &str) {
    for c in text.chars() {
        key(s, KeyCode::Char(c), KeyModifiers::empty());
    }
}

// -------------------------------------------------------------- editing

#[test]
fn typing_updates_the_query() {
    let mut s = state();
    type_str(&mut s, "radiohead");
    assert_eq!(s.query(), "radiohead");
    assert_eq!(s.input.cursor(), (0, 9));
}

#[test]
fn left_arrow_then_typing_inserts_mid_string() {
    let mut s = state();
    type_str(&mut s, "abd");
    key(&mut s, KeyCode::Left, KeyModifiers::empty());
    type_str(&mut s, "c");
    assert_eq!(s.query(), "abcd");
    assert_eq!(s.input.cursor(), (0, 3), "cursor sits after the insert");
}

#[test]
fn ctrl_w_deletes_the_word_before_the_cursor() {
    let mut s = state();
    type_str(&mut s, "boards of canada");
    key(&mut s, KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert_eq!(s.query(), "boards of ");
}

#[test]
fn backspace_still_deletes() {
    let mut s = state();
    type_str(&mut s, "ab");
    key(&mut s, KeyCode::Backspace, KeyModifiers::empty());
    assert_eq!(s.query(), "a");
}

// ---------------------------------------------------------- prompt contract

#[test]
fn esc_preserves_the_query_content() {
    // key.rs intercepts Esc (exit input mode) without touching the editor —
    // the buffer only resets on the next `/`. Mirror that sequence here.
    let mut s = state();
    type_str(&mut s, "aphex twin");
    s.input_mode = false; // the Esc branch, minus the App plumbing
    assert_eq!(s.query(), "aphex twin");
    s.clear(); // the `/` branch
    assert_eq!(s.query(), "");
    assert_eq!(s.input.cursor(), (0, 0));
}

#[test]
fn enter_submits_the_trimmed_query() {
    // key.rs intercepts Enter and reads `query().trim()` — no newline ever
    // reaches the editor.
    let mut s = state();
    type_str(&mut s, "  ok computer  ");
    assert_eq!(s.query().trim(), "ok computer");
}

#[test]
fn a_pasted_newline_cannot_leak_past_the_accessor() {
    let mut s = state();
    s.input.insert_str("first\nsecond");
    assert_eq!(s.query(), "first", "query() is first-line only");
}

// ------------------------------------------------------------ cursor glyph

#[test]
fn the_cursor_glyph_splits_mid_string() {
    assert_eq!(split_at_cursor("abcd", 2), ("ab", "cd"));
}

#[test]
fn the_cursor_glyph_lands_at_the_ends() {
    assert_eq!(split_at_cursor("abcd", 0), ("", "abcd"));
    assert_eq!(split_at_cursor("abcd", 4), ("abcd", ""));
    assert_eq!(split_at_cursor("", 0), ("", ""));
}

#[test]
fn the_cursor_glyph_clamps_past_the_end() {
    assert_eq!(split_at_cursor("ab", 99), ("ab", ""));
}

#[test]
fn the_cursor_glyph_splits_on_char_boundaries() {
    // col is a char index, not bytes — multibyte text must not panic.
    assert_eq!(split_at_cursor("héllo", 2), ("hé", "llo"));
}

#[test]
fn the_glyph_follows_real_editing() {
    let mut s = state();
    type_str(&mut s, "abd");
    key(&mut s, KeyCode::Left, KeyModifiers::empty());
    let (before, after) = split_at_cursor(s.query(), s.input.cursor().1);
    assert_eq!((before, after), ("ab", "d"));
}
