//! `R`, repeat (#68): off → all → one like Spotify's button, said on the
//! status line, and kept for the next load even when the player can't be told.
//!
//! Needs `--features test-support` (CI's `--all-features` has it).

#![cfg(feature = "test-support")]

use super::sizes::{chans, test_app};
use crate::*;

fn press_r(app: &mut App, mods: KeyModifiers) {
    handle_key(app, KeyCode::Char('R'), mods, &chans());
}

#[test]
fn r_steps_off_all_one_and_back() {
    let mut app = test_app();
    assert_eq!(app.svc.engine.repeat_mode(), Repeat::Off);
    // Shift+R as legacy terminals and Windows report it: the capital, with
    // SHIFT set. Caps Lock + r arrives as the bare capital.
    for (mods, want) in [
        (KeyModifiers::SHIFT, Repeat::All),
        (KeyModifiers::empty(), Repeat::One),
        (KeyModifiers::SHIFT, Repeat::Off),
    ] {
        press_r(&mut app, mods);
        assert_eq!(app.svc.engine.repeat_mode(), want);
    }
}

#[test]
fn r_says_what_repeat_is_now() {
    let mut app = test_app();
    for want in [Repeat::All, Repeat::One, Repeat::Off] {
        app.status.clear();
        press_r(&mut app, KeyModifiers::SHIFT);
        assert!(
            app.status.starts_with(&format!("repeat {}", want.label())),
            "{want:?}: {:?}",
            app.status
        );
    }
}

/// A detached engine has no player to tell — like a dropped connection. The
/// failure is shown, not swallowed, and the mode still holds for the next load.
#[test]
fn r_shows_why_the_player_did_not_get_it() {
    let mut app = test_app();
    press_r(&mut app, KeyModifiers::SHIFT);
    assert_eq!(app.svc.engine.repeat_mode(), Repeat::All);
    assert!(app.status.contains("not connected"), "{:?}", app.status);
}

#[test]
fn r_works_with_the_library_hidden() {
    let mut app = test_app();
    app.view.zen = true;
    app.settle_view();
    press_r(&mut app, KeyModifiers::SHIFT);
    assert_eq!(app.svc.engine.repeat_mode(), Repeat::All);
}

#[test]
fn each_repeat_status_names_its_mode() {
    for mode in [Repeat::Off, Repeat::All, Repeat::One] {
        assert!(repeat_status(mode).starts_with(&format!("repeat {}", mode.label())));
    }
}
