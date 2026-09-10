use crate::*;

#[test]
fn a_cover_palette_keeps_the_configured_transparent_background() {
    // `derive_theme` knows nothing of the config and always comes back opaque;
    // the flag set at startup has to survive every track change.
    let transparent = Theme {
        transparent: true,
        ..TOKYONIGHT
    };
    let mut state = ThemeState {
        displayed: transparent,
        target: transparent,
        fade: None,
    };

    state.start_fade(myx::theme::GRUVBOX);

    assert!(state.target.transparent);
    assert!(state.fade.as_ref().is_some_and(|f| f.current().transparent));
}

#[test]
fn flipping_transparency_mid_fade_sticks_when_the_fade_lands() {
    // The running fade still carries the old flag; without restarting it, the
    // next frame would paint the background right back.
    let mut state = ThemeState {
        displayed: TOKYONIGHT,
        target: TOKYONIGHT,
        fade: None,
    };
    state.start_fade(myx::theme::GRUVBOX);

    state.set_transparent(true);

    assert!(state.displayed.transparent);
    assert!(state
        .fade
        .as_ref()
        .is_some_and(|f| f.current().transparent && f.target().transparent));
}
