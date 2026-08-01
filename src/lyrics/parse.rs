//! LRC parsing — pure, no I/O.

/// Parse LRC `[mm:ss.xx] text` lines into sorted (ms, text) pairs.
pub fn parse_lrc(lrc: &str) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = Vec::new();
    for line in lrc.lines() {
        // A line may carry multiple timestamps; collect them, then the trailing text.
        let mut rest = line;
        let mut stamps: Vec<u32> = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            let tag = &rest[1..end];
            if let Some(ms) = parse_lrc_stamp(tag) {
                stamps.push(ms);
            }
            // Keep consuming bracket groups even if this one was not a
            // timestamp. Bailing here used to discard the rest of the line, so
            // a metadata tag or a malformed stamp sitting in front of a valid
            // one ("[ar:X][00:01.00]words") swallowed the whole lyric. Lines
            // that yield no stamps at all are still dropped below.
            rest = rest[end + 1..].trim_start();
        }
        let text = rest.trim().to_string();
        for ms in stamps {
            out.push((ms, text.clone()));
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

/// Parse a single `mm:ss.xx` (or `mm:ss`) LRC timestamp tag into milliseconds.
pub fn parse_lrc_stamp(tag: &str) -> Option<u32> {
    // mm:ss.xx or mm:ss
    let (mm, rest) = tag.split_once(':')?;
    let mm: u32 = mm.parse().ok()?;
    let (ss, cs) = match rest.split_once('.') {
        Some((s, c)) => (s.parse::<u32>().ok()?, c),
        None => (rest.parse::<u32>().ok()?, "0"),
    };
    // Reject a non-numeric fraction outright. LRC text is fetched from lrclib,
    // a community-editable database, so this is untrusted remote input.
    //
    // This guard closes two bugs at once:
    //   1. The byte-slice below used to split multi-byte characters and panic.
    //      `format!("{cs:0<3}")` pads by *chars* but `[..3]` indexes *bytes*, so
    //      a fraction like "a日" produced "a日0" and slicing at byte 3 landed
    //      inside '日'. With `panic = "abort"` that killed the whole player, and
    //      because the payload rides on a track it reproduced on every replay.
    //   2. Garbage such as ".xx" silently parsed as .000 instead of being
    //      rejected, quietly desyncing a line by up to 999ms.
    if !cs.is_empty() && !cs.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let cs: u32 = if cs.is_empty() {
        0
    } else {
        // ASCII-only by the guard above, so this slice is char-boundary safe.
        format!("{:0<3}", &cs[..cs.len().min(3)]).parse().ok()?
    };
    // Checked arithmetic: a hostile stamp like "[99999999:00]" overflowed u32,
    // panicking in debug builds and silently wrapping in release.
    mm.checked_mul(60)?
        .checked_add(ss)?
        .checked_mul(1000)?
        .checked_add(cs)
}
