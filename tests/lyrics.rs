//! Characterization tests for `myx::lyrics::parse`.
//!
//! These lock in TODAY's LRC parsing behavior, quirks included. Where a former
//! quirk was a crash reachable from remote input, the test now locks the fix.

use myx::lyrics::parse::{parse_lrc, parse_lrc_stamp};

// ----------------------------------------------------------- parse_lrc_stamp

#[test]
fn stamp_mm_ss_without_fraction() {
    assert_eq!(parse_lrc_stamp("00:00"), Some(0));
    assert_eq!(parse_lrc_stamp("01:02"), Some(62_000));
    assert_eq!(parse_lrc_stamp("10:30"), Some(630_000));
}

#[test]
fn stamp_two_digit_fraction_is_centiseconds() {
    assert_eq!(parse_lrc_stamp("00:00.50"), Some(500));
    assert_eq!(parse_lrc_stamp("01:02.34"), Some(62_340));
    assert_eq!(parse_lrc_stamp("00:01.99"), Some(1_990));
}

#[test]
fn stamp_three_digit_fraction_is_milliseconds() {
    assert_eq!(parse_lrc_stamp("00:00.500"), Some(500));
    assert_eq!(parse_lrc_stamp("01:02.345"), Some(62_345));
    assert_eq!(parse_lrc_stamp("00:00.001"), Some(1));
}

#[test]
fn stamp_one_digit_fraction_is_left_padded_to_deciseconds() {
    // "5" -> "500" ms, i.e. the fraction is left-aligned then zero-filled.
    assert_eq!(parse_lrc_stamp("00:00.5"), Some(500));
    assert_eq!(parse_lrc_stamp("00:00.0"), Some(0));
    assert_eq!(parse_lrc_stamp("00:10.9"), Some(10_900));
}

#[test]
fn stamp_longer_fraction_is_cut_to_three_digits_quirk() {
    // QUIRK: extra fraction digits are silently truncated, not rounded.
    assert_eq!(parse_lrc_stamp("02:03.4567"), Some(123_456));
    assert_eq!(parse_lrc_stamp("00:00.9999"), Some(999));
}

#[test]
fn stamp_empty_fraction_is_zero_quirk() {
    // QUIRK: a trailing dot with nothing after it parses fine as .000
    assert_eq!(parse_lrc_stamp("01:02."), Some(62_000));
}

#[test]
fn stamp_rejects_garbage() {
    assert_eq!(parse_lrc_stamp(""), None);
    assert_eq!(parse_lrc_stamp("nope"), None);
    assert_eq!(parse_lrc_stamp("ar:Some Artist"), None);
    assert_eq!(parse_lrc_stamp("ti:Some Title"), None);
    assert_eq!(parse_lrc_stamp("00"), None); // no colon
    assert_eq!(parse_lrc_stamp("00:ss"), None);
    assert_eq!(parse_lrc_stamp("-1:00"), None); // u32 parse
    assert_eq!(parse_lrc_stamp("00:-1"), None);
    assert_eq!(parse_lrc_stamp(" 00:00"), None); // no trimming
}

#[test]
fn stamp_non_numeric_fraction_is_rejected() {
    // Previously `unwrap_or(0)` degraded junk to .000, silently desyncing the
    // line by up to 999ms. A fraction that is not all ASCII digits is not a
    // timestamp, so the stamp is now rejected outright.
    assert_eq!(parse_lrc_stamp("00:01.xx"), None);
    assert_eq!(parse_lrc_stamp("00:01.ab0"), None);
    assert_eq!(parse_lrc_stamp("00:01.5a"), None);
    assert_eq!(parse_lrc_stamp("00:01.-1"), None);
}

#[test]
fn stamp_does_not_bound_minutes_or_seconds_quirk() {
    // QUIRK: no range checks — 99 seconds is accepted as 99 seconds.
    assert_eq!(parse_lrc_stamp("00:99"), Some(99_000));
    assert_eq!(parse_lrc_stamp("999:00"), Some(59_940_000));
}

#[test]
fn stamp_absurd_minutes_do_not_overflow() {
    // `(mm * 60 + ss) * 1000` was unchecked: "99999999:00" overflowed u32,
    // panicking in debug builds and silently wrapping in release. Now `None`.
    assert_eq!(parse_lrc_stamp("99999999:00"), None);
    assert_eq!(parse_lrc_stamp("71582789:00"), None);
    assert_eq!(parse_lrc_stamp("4294967295:00"), None);
    // The largest stamp that still fits is unaffected.
    assert_eq!(parse_lrc_stamp("71582:47.295"), Some(4_294_967_295));
}

#[test]
fn stamp_multi_colon_tag_uses_only_the_first_split() {
    // "00:01:02" -> mm=00, rest="01:02" which does not parse as u32.
    assert_eq!(parse_lrc_stamp("00:01:02"), None);
}

// ------------------------------------------------------------------ parse_lrc

#[test]
fn parse_lrc_empty_input() {
    assert_eq!(parse_lrc(""), Vec::<(u32, String)>::new());
    assert_eq!(parse_lrc("\n\n\n"), Vec::<(u32, String)>::new());
}

#[test]
fn parse_lrc_basic_song() {
    let lrc = "[00:12.00]Line one\n[00:15.50]Line two\n[01:00.00]Line three";
    assert_eq!(
        parse_lrc(lrc),
        vec![
            (12_000, "Line one".to_string()),
            (15_500, "Line two".to_string()),
            (60_000, "Line three".to_string()),
        ]
    );
}

#[test]
fn parse_lrc_sorts_by_timestamp() {
    let lrc = "[00:30.00]third\n[00:10.00]first\n[00:20.00]second";
    let out = parse_lrc(lrc);
    assert_eq!(
        out,
        vec![
            (10_000, "first".to_string()),
            (20_000, "second".to_string()),
            (30_000, "third".to_string()),
        ]
    );
}

#[test]
fn parse_lrc_equal_timestamps_keep_source_order() {
    let lrc = "[00:05.00]alpha\n[00:05.00]beta";
    assert_eq!(
        parse_lrc(lrc),
        vec![
            (5_000, "alpha".to_string()),
            (5_000, "beta".to_string()),
        ]
    );
}

#[test]
fn parse_lrc_multiple_stamps_on_one_line_duplicate_the_text() {
    let lrc = "[00:01.00][00:31.00][01:01.00]chorus";
    assert_eq!(
        parse_lrc(lrc),
        vec![
            (1_000, "chorus".to_string()),
            (31_000, "chorus".to_string()),
            (61_000, "chorus".to_string()),
        ]
    );
}

#[test]
fn parse_lrc_trims_whitespace_around_text() {
    let lrc = "[00:01.00]   spaced out   ";
    assert_eq!(parse_lrc(lrc), vec![(1_000, "spaced out".to_string())]);
}

#[test]
fn parse_lrc_keeps_empty_text_lines() {
    // Instrumental gaps are real LRC content and are preserved as "".
    let lrc = "[00:01.00]\n[00:02.00]sing";
    assert_eq!(
        parse_lrc(lrc),
        vec![(1_000, String::new()), (2_000, "sing".to_string())]
    );
}

#[test]
fn parse_lrc_skips_metadata_only_lines() {
    let lrc = "[ar:Artist]\n[ti:Title]\n[00:01.00]words";
    assert_eq!(parse_lrc(lrc), vec![(1_000, "words".to_string())]);
}

#[test]
fn parse_lrc_metadata_before_a_stamp_swallows_the_whole_line_quirk() {
    // QUIRK: once a leading tag fails to parse the loop bails, so a valid
    // timestamp that follows a metadata tag on the SAME line is dropped.
    assert_eq!(parse_lrc("[ar:Artist][00:01.00]words"), Vec::<(u32, String)>::new());
}

#[test]
fn parse_lrc_metadata_after_a_stamp_is_ignored() {
    // The mirror case works: the stamp already landed, so parsing continues.
    assert_eq!(
        parse_lrc("[00:01.00][ar:Artist]words"),
        vec![(1_000, "words".to_string())]
    );
}

#[test]
fn parse_lrc_lines_without_any_stamp_are_dropped() {
    let lrc = "just some plain text\n[00:01.00]real line\nmore plain text";
    assert_eq!(parse_lrc(lrc), vec![(1_000, "real line".to_string())]);
}

#[test]
fn parse_lrc_unterminated_bracket_is_dropped() {
    assert_eq!(parse_lrc("[00:01.00 missing brace"), Vec::<(u32, String)>::new());
    assert_eq!(parse_lrc("["), Vec::<(u32, String)>::new());
}

#[test]
fn parse_lrc_empty_tag() {
    assert_eq!(parse_lrc("[]text"), Vec::<(u32, String)>::new());
}

#[test]
fn parse_lrc_handles_crlf() {
    let lrc = "[00:01.00]one\r\n[00:02.00]two\r\n";
    assert_eq!(
        parse_lrc(lrc),
        vec![(1_000, "one".to_string()), (2_000, "two".to_string())]
    );
}

#[test]
fn parse_lrc_text_may_contain_brackets_after_the_stamps() {
    assert_eq!(
        parse_lrc("[00:01.00]ooh [x3] yeah"),
        vec![(1_000, "ooh [x3] yeah".to_string())]
    );
}

#[test]
fn parse_lrc_unicode_text_survives_intact() {
    assert_eq!(
        parse_lrc("[00:01.00]君の名は — 日本語 🎵"),
        vec![(1_000, "君の名は — 日本語 🎵".to_string())]
    );
}

#[test]
fn parse_lrc_mixed_fraction_widths_in_one_file() {
    let lrc = "[00:01.5]a\n[00:02.25]b\n[00:03.125]c\n[00:04]d";
    assert_eq!(
        parse_lrc(lrc),
        vec![
            (1_500, "a".to_string()),
            (2_250, "b".to_string()),
            (3_125, "c".to_string()),
            (4_000, "d".to_string()),
        ]
    );
}

#[test]
fn parse_lrc_realistic_file_with_header_block() {
    let lrc = "\
[ar:Radiohead]
[ti:Everything In Its Right Place]
[al:Kid A]
[length:04:11]

[00:00.00]
[00:22.10]Everything
[00:26.40]Everything
[00:30.80]Everything in its right place
[00:38.20]Yesterday I woke up sucking a lemon
";
    let out = parse_lrc(lrc);
    assert_eq!(out.len(), 5);
    assert_eq!(out[0], (0, String::new()));
    assert_eq!(out[1], (22_100, "Everything".to_string()));
    assert_eq!(out[4], (38_200, "Yesterday I woke up sucking a lemon".to_string()));
}

#[test]
fn stamp_multibyte_fraction_is_rejected_not_panic() {
    // REGRESSION: the fraction was sliced with a BYTE range `[..3]` after a
    // char-width pad, so a multibyte fraction sliced through a UTF-8 boundary
    // and panicked. With `panic = "abort"` that aborted the whole player, and
    // since the payload rides on a track it reproduced on every replay.
    assert_eq!(parse_lrc_stamp("00:00.ab\u{65e5}"), None);
    assert_eq!(parse_lrc_stamp("00:00.a\u{65e5}"), None);
    assert_eq!(parse_lrc_stamp("00:00.\u{65e5}\u{65e5}\u{65e5}"), None);
    assert_eq!(parse_lrc_stamp("01:02.\u{1f600}"), None);
}

/// The crash had to be unreachable through the real entry point, not just the
/// helper — `parse_lrc` is what the lyrics fetch actually calls.
#[test]
fn parse_lrc_survives_a_poisoned_multibyte_fraction() {
    let out = parse_lrc("[00:00.a\u{65e5}]cursed\n[00:01.00]fine");
    assert_eq!(out, vec![(1000, "fine".to_string())]);
}

#[test]
fn parse_lrc_survives_an_overflowing_stamp() {
    let out = parse_lrc("[99999999:00]nope\n[00:02.00]yes");
    assert_eq!(out, vec![(2000, "yes".to_string())]);
}
