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
            rest = rest[end + 1..].trim_start();
            if stamps.is_empty() {
                break; // not a timestamp tag (e.g. metadata) — bail
            }
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
    let cs: u32 = format!("{cs:0<3}")[..3].parse().unwrap_or(0);
    Some((mm * 60 + ss) * 1000 + cs)
}
