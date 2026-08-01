//! Characterization tests for `myx::util`.
//!
//! These lock in the behavior the helpers have TODAY, quirks included.
//! If one of these fails, someone changed behavior — deliberately or not.

use myx::util::{center_v, fmt_ms, track_id_from_uri, truncate, uri_to_url, urlencode, vol_u16};
use ratatui::layout::Rect;

// ---------------------------------------------------------------- truncate

#[test]
fn truncate_leaves_short_strings_alone() {
    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate("hello", 5), "hello"); // exactly max is untouched
}

#[test]
fn truncate_cuts_and_appends_ellipsis() {
    assert_eq!(truncate("hello", 4), "hel…");
    assert_eq!(truncate("abcdef", 3), "ab…");
}

#[test]
fn truncate_result_has_max_chars_when_cut() {
    let out = truncate("abcdefghij", 4);
    assert_eq!(out.chars().count(), 4);
    assert_eq!(out, "abc…");
}

#[test]
fn truncate_empty_string() {
    assert_eq!(truncate("", 0), "");
    assert_eq!(truncate("", 5), "");
}

#[test]
fn truncate_max_zero_quirk_returns_ellipsis_longer_than_max() {
    // QUIRK: max = 0 yields a 1-char string, which is longer than the limit.
    assert_eq!(truncate("a", 0), "…");
    assert_eq!(truncate("anything", 0), "…");
}

#[test]
fn truncate_max_one_quirk_drops_all_content() {
    // QUIRK: max = 1 keeps zero characters of the input.
    assert_eq!(truncate("abc", 1), "…");
}

#[test]
fn truncate_counts_chars_not_bytes() {
    // 5 chars, 10 bytes — fits under max 5, so it is returned whole.
    assert_eq!(truncate("héllo", 5), "héllo");
    // Cyrillic: 6 chars / 12 bytes.
    assert_eq!(truncate("привет", 6), "привет");
    assert_eq!(truncate("привет", 3), "пр…");
}

#[test]
fn truncate_never_splits_a_multibyte_char() {
    let out = truncate("日本語のテキスト", 4);
    assert_eq!(out, "日本語…");
    assert!(std::str::from_utf8(out.as_bytes()).is_ok());
}

#[test]
fn truncate_counts_scalar_values_not_grapheme_clusters() {
    // QUIRK: "é" as e + U+0301 is 2 chars, so combining marks can be severed.
    let decomposed = "e\u{0301}llo"; // 5 chars
    assert_eq!(truncate(decomposed, 5), decomposed);
    assert_eq!(truncate(decomposed, 2), "e…"); // the combining accent is dropped
                                               // A ZWJ emoji family is many scalar values, not one.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"; // 5 chars
    assert_eq!(truncate(family, 5), family);
    assert_eq!(truncate(family, 2), "\u{1F468}…");
}

// ------------------------------------------------------------------ fmt_ms

#[test]
fn fmt_ms_zero() {
    assert_eq!(fmt_ms(0), "0:00");
}

#[test]
fn fmt_ms_truncates_sub_second() {
    assert_eq!(fmt_ms(999), "0:00");
    assert_eq!(fmt_ms(1_000), "0:01");
    assert_eq!(fmt_ms(1_999), "0:01");
}

#[test]
fn fmt_ms_pads_seconds() {
    assert_eq!(fmt_ms(9_000), "0:09");
    assert_eq!(fmt_ms(59_000), "0:59");
    assert_eq!(fmt_ms(59_999), "0:59");
    assert_eq!(fmt_ms(60_000), "1:00");
    assert_eq!(fmt_ms(61_000), "1:01");
}

#[test]
fn fmt_ms_typical_track_length() {
    assert_eq!(fmt_ms(213_000), "3:33");
    assert_eq!(fmt_ms(3_600_000), "60:00"); // QUIRK: no hour field, minutes just grow
}

#[test]
fn fmt_ms_large_values() {
    assert_eq!(fmt_ms(86_400_000), "1440:00"); // 24h
    assert_eq!(fmt_ms(u32::MAX), "71582:47");
}

// ----------------------------------------------------------------- vol_u16

#[test]
fn vol_u16_boundaries() {
    assert_eq!(vol_u16(0), 0);
    assert_eq!(vol_u16(100), 65535);
}

#[test]
fn vol_u16_midpoints() {
    assert_eq!(vol_u16(50), 32767); // integer division, so not 32768
    assert_eq!(vol_u16(1), 655); // 655.35 floored
    assert_eq!(vol_u16(25), 16383);
    assert_eq!(vol_u16(75), 49151);
    assert_eq!(vol_u16(99), 64879);
}

#[test]
fn vol_u16_is_monotonic_over_the_valid_range() {
    let mut prev = 0u16;
    for pct in 0..=100u8 {
        let v = vol_u16(pct);
        assert!(v >= prev, "vol_u16({pct}) = {v} went backwards from {prev}");
        prev = v;
    }
}

#[test]
fn vol_u16_above_100_wraps_quirk() {
    // QUIRK: nothing clamps `pct`; the `as u16` cast wraps modulo 65536.
    assert_eq!(vol_u16(101), 654); // 66190 - 65536
    assert_eq!(vol_u16(255), 36042);
}

// ----------------------------------------------------------------- center_v

#[test]
fn center_v_centers_inside_a_taller_area() {
    let area = Rect::new(2, 0, 20, 10);
    let r = center_v(area, 4);
    assert_eq!(r, Rect::new(2, 3, 20, 4));
}

#[test]
fn center_v_preserves_x_and_width() {
    let area = Rect::new(7, 3, 33, 9);
    let r = center_v(area, 3);
    assert_eq!(r.x, 7);
    assert_eq!(r.width, 33);
    assert_eq!(r.y, 6);
    assert_eq!(r.height, 3);
}

#[test]
fn center_v_odd_slack_rounds_the_top_gap_down() {
    let area = Rect::new(0, 0, 10, 10);
    let r = center_v(area, 5); // slack 5 -> top gap 2, bottom gap 3
    assert_eq!(r.y, 2);
    assert_eq!(r.height, 5);
}

#[test]
fn center_v_clamps_height_to_the_area() {
    let area = Rect::new(1, 5, 10, 2);
    let r = center_v(area, 10);
    assert_eq!(r, Rect::new(1, 5, 10, 2)); // saturating_sub keeps y at area.y
}

#[test]
fn center_v_zero_height_area() {
    let area = Rect::new(0, 4, 10, 0);
    let r = center_v(area, 3);
    assert_eq!(r, Rect::new(0, 4, 10, 0));
}

#[test]
fn center_v_zero_requested_height() {
    let area = Rect::new(0, 0, 10, 10);
    let r = center_v(area, 0);
    assert_eq!(r, Rect::new(0, 5, 10, 0));
}

#[test]
fn center_v_exact_fit() {
    let area = Rect::new(0, 2, 10, 6);
    assert_eq!(center_v(area, 6), Rect::new(0, 2, 10, 6));
}

// ---------------------------------------------------------------- uri_to_url

#[test]
fn uri_to_url_track() {
    assert_eq!(
        uri_to_url("spotify:track:4cOdK2wGLETKBW3PvgPWqT"),
        "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT"
    );
}

#[test]
fn uri_to_url_other_kinds() {
    assert_eq!(
        uri_to_url("spotify:album:1DFixLWuPkv3KT3TnV35m3"),
        "https://open.spotify.com/album/1DFixLWuPkv3KT3TnV35m3"
    );
    assert_eq!(
        uri_to_url("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"),
        "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M"
    );
    assert_eq!(
        uri_to_url("spotify:artist:0OdUWJ0sBjDrqHygGUXeCF"),
        "https://open.spotify.com/artist/0OdUWJ0sBjDrqHygGUXeCF"
    );
}

#[test]
fn uri_to_url_ignores_trailing_segments() {
    assert_eq!(
        uri_to_url("spotify:user:someone:playlist:abc"),
        "https://open.spotify.com/user/someone"
    );
}

#[test]
fn uri_to_url_does_not_validate_the_scheme_quirk() {
    // QUIRK: the first segment is discarded without ever being checked.
    assert_eq!(uri_to_url("a:b:c"), "https://open.spotify.com/b/c");
    assert_eq!(
        uri_to_url("http://example.com"),
        "https://open.spotify.com///example.com/"
    );
}

#[test]
fn uri_to_url_empty_and_malformed() {
    assert_eq!(uri_to_url(""), "https://open.spotify.com//");
    assert_eq!(uri_to_url("nonsense"), "https://open.spotify.com//");
    assert_eq!(uri_to_url("spotify"), "https://open.spotify.com//");
    assert_eq!(
        uri_to_url("spotify:track"),
        "https://open.spotify.com/track/"
    );
    assert_eq!(uri_to_url("::"), "https://open.spotify.com//");
}

// --------------------------------------------------------- track_id_from_uri

#[test]
fn track_id_from_uri_happy_path() {
    assert_eq!(
        track_id_from_uri("spotify:track:4cOdK2wGLETKBW3PvgPWqT"),
        Some("4cOdK2wGLETKBW3PvgPWqT".to_string())
    );
}

#[test]
fn track_id_from_uri_rejects_other_kinds() {
    assert_eq!(track_id_from_uri("spotify:album:abc"), None);
    assert_eq!(track_id_from_uri("spotify:episode:abc"), None);
    assert_eq!(track_id_from_uri("spotify:artist:abc"), None);
}

#[test]
fn track_id_from_uri_rejects_non_spotify_uris() {
    assert_eq!(track_id_from_uri(""), None);
    assert_eq!(track_id_from_uri("track:abc"), None);
    assert_eq!(
        track_id_from_uri("https://open.spotify.com/track/abc"),
        None
    );
    assert_eq!(track_id_from_uri("Spotify:track:abc"), None); // case sensitive
    assert_eq!(track_id_from_uri("spotify:track"), None); // missing id segment
}

#[test]
fn track_id_from_uri_ignores_extra_segments() {
    assert_eq!(
        track_id_from_uri("spotify:track:abc:extra"),
        Some("abc".to_string())
    );
}

#[test]
fn track_id_from_uri_accepts_empty_id_quirk() {
    // QUIRK: an empty third segment is a "valid" id.
    assert_eq!(track_id_from_uri("spotify:track:"), Some(String::new()));
}

// ---------------------------------------------------------------- urlencode

#[test]
fn urlencode_passes_unreserved_chars_through() {
    assert_eq!(urlencode("abcXYZ0189-_.~"), "abcXYZ0189-_.~");
}

#[test]
fn urlencode_empty() {
    assert_eq!(urlencode(""), "");
}

#[test]
fn urlencode_space_and_punctuation() {
    assert_eq!(urlencode("hello world"), "hello%20world");
    assert_eq!(urlencode("a+b"), "a%2Bb");
    assert_eq!(urlencode("a/b?c=d&e"), "a%2Fb%3Fc%3Dd%26e");
    assert_eq!(urlencode("!*'()"), "%21%2A%27%28%29");
}

#[test]
fn urlencode_uses_uppercase_hex() {
    assert_eq!(urlencode(":"), "%3A");
    assert_eq!(urlencode("\u{7f}"), "%7F");
}

#[test]
fn urlencode_encodes_utf8_byte_by_byte() {
    assert_eq!(urlencode("é"), "%C3%A9");
    assert_eq!(urlencode("日本"), "%E6%97%A5%E6%9C%AC");
    assert_eq!(urlencode("\u{1F600}"), "%F0%9F%98%80");
}

#[test]
fn urlencode_control_chars() {
    assert_eq!(urlencode("\n\t"), "%0A%09");
    assert_eq!(urlencode("\0"), "%00");
}

#[test]
fn urlencode_realistic_search_query() {
    assert_eq!(
        urlencode("Sigur Rós - Hoppípolla"),
        "Sigur%20R%C3%B3s%20-%20Hopp%C3%ADpolla"
    );
}
