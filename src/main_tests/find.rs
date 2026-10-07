//! `f`, find in the list on screen: which rows match, which can take the
//! cursor, and how a find belongs to (and lapses with) its list.

use crate::*;

fn track(name: &str, artist: &str) -> LibItem {
    LibItem::track(
        name.to_string(),
        artist.to_string(),
        format!("spotify:track:{name}"),
    )
}

fn playlist() -> Vec<LibItem> {
    vec![
        LibItem::play(
            "▶︎ Play House Party".to_string(),
            "spotify:playlist:x".to_string(),
        ),
        LibItem::header("Tracks"),
        track("One More Time", "Daft Punk"),
        track("Music Sounds Better With You", "Stardust"),
        track("Around the World", "Daft Punk"),
        track("Finally", "Kings of Tomorrow"),
    ]
}

// ------------------------------------------------------------------ matching

#[test]
fn every_word_must_appear_in_the_title_or_the_artist_in_any_order() {
    let one_more_time = track("One More Time", "Daft Punk");
    assert!(find_matches(&one_more_time, "daft one"));
    assert!(find_matches(&one_more_time, "TIME"));
    assert!(find_matches(&one_more_time, "punk  more"));
    assert!(!find_matches(&one_more_time, "daft world"));
}

#[test]
fn headers_and_play_rows_are_never_found() {
    let rows = playlist();
    // "house" is in the play row's name; it still isn't a match.
    assert!(!find_matches(&rows[0], "house"));
    assert!(!find_matches(&rows[1], "tracks"));
}

#[test]
fn shown_rows_are_every_row_until_a_query_narrows_them() {
    let rows = playlist();
    assert_eq!(shown_rows(&rows, None), [0, 1, 2, 3, 4, 5]);
    assert_eq!(shown_rows(&rows, Some("daft")), [2, 4]);
    assert_eq!(
        shown_rows(&rows, Some("nothing like this")),
        [] as [usize; 0]
    );
}

#[test]
fn the_cursor_skips_headers_and_hidden_rows() {
    let rows = playlist();
    assert!(!row_selectable(&rows, 1, None), "header");
    assert!(row_selectable(&rows, 0, None), "the play row, unfiltered");
    assert!(row_selectable(&rows, 3, None));
    assert!(
        !row_selectable(&rows, 3, Some("daft")),
        "hidden by the find"
    );
    assert!(row_selectable(&rows, 4, Some("daft")));
    assert!(!row_selectable(&rows, 99, None), "past the end");
}

// --------------------------------------------------------------- belonging

fn typed(f: &mut FindState, text: &str) {
    for c in text.chars() {
        f.input.input(crossterm::event::KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::empty(),
        ));
    }
}

const LIKED: ListKey = ListKey::Section(Section::Liked);

#[test]
fn a_find_applies_only_to_its_own_list_and_only_once_typed() {
    let mut f = FindState::default();
    f.open(LIKED);
    assert_eq!(f.query_for(&LIKED), None, "nothing typed yet");
    typed(&mut f, "  daft ");
    assert_eq!(f.query_for(&LIKED), Some("daft"));
    assert_eq!(f.query_for(&ListKey::Search), None);
    let page = ListKey::Page(1, "spotify:playlist:x".to_string());
    assert_eq!(f.query_for(&page), None);
}

#[test]
fn reopening_on_the_same_list_edits_the_query_and_elsewhere_starts_empty() {
    let mut f = FindState::default();
    f.open(LIKED);
    typed(&mut f, "daft");
    f.typing = false;
    f.open(LIKED);
    assert!(f.typing);
    assert_eq!(f.query(), "daft");
    f.open(ListKey::Search);
    assert_eq!(f.query(), "");
    assert_eq!(f.list, Some(ListKey::Search));
}

#[test]
fn leaving_the_list_forgets_the_find_so_coming_back_shows_everything() {
    let mut f = FindState::default();
    f.open(LIKED);
    typed(&mut f, "daft");
    f.forget_unless(&LIKED);
    assert_eq!(f.query_for(&LIKED), Some("daft"), "still on its list");
    f.forget_unless(&ListKey::Search);
    assert_eq!(f.query_for(&LIKED), None, "gone, not just hidden");
    assert!(!f.typing);
}

#[test]
fn the_same_playlist_open_at_another_depth_is_another_list() {
    // A page is its place in the drill-in stack, not just its URI.
    let a = ListKey::Page(1, "spotify:playlist:x".to_string());
    let b = ListKey::Page(2, "spotify:playlist:x".to_string());
    let mut f = FindState::default();
    f.open(a.clone());
    typed(&mut f, "daft");
    assert_eq!(f.query_for(&b), None);
    assert_eq!(f.query_for(&a), Some("daft"));
}
