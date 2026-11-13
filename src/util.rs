//! Small pure helpers shared by the UI and the workers.
//!
//! Everything here is dependency-light and side-effect free, so it can be
//! unit-tested without a terminal, a network, or an audio device.

use ratatui::layout::Rect;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Truncate to `max` characters, replacing the tail with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

/// Wrap `s` into rows no wider than `width` terminal columns, as evenly as
/// possible: a line that needs two rows becomes two of about the same width,
/// not a full row and a stray word. Centred lyrics read much better that way.
///
/// At most `max_rows` rows come back; when the text needs more, the last one
/// ends in `…`. Widths are display columns, so double-width (CJK) text fits,
/// and a word wider than a row — or a run of CJK, which has no spaces — breaks
/// between characters.
pub fn wrap_balanced(s: &str, width: usize, max_rows: usize) -> Vec<String> {
    if width == 0 || max_rows == 0 {
        return Vec::new();
    }
    let atoms = wrap_atoms(s, width);
    let total: usize = atoms
        .iter()
        .enumerate()
        .map(|(i, a)| a.width + usize::from(i > 0 && a.space_before))
        .sum();
    if total <= width {
        return vec![join_atoms(&atoms)];
    }
    let full = pack_atoms(&atoms, width);
    if full.len() > max_rows {
        // Too long even at full width: keep the first rows and say so.
        let mut rows: Vec<String> = full[..max_rows - 1].iter().map(|r| join_atoms(r)).collect();
        let rest: Vec<Atom> = full[max_rows - 1..].concat();
        rows.push(ellipsize(&join_atoms(&rest), width));
        return rows;
    }
    // Narrowest width that still needs no more rows than full width does.
    // Greedy row count only falls as the width grows, so bisect.
    let rows = full.len();
    let (mut lo, mut hi) = (total.div_ceil(rows).max(1), width);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if pack_atoms(&atoms, mid).len() <= rows {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    pack_atoms(&atoms, lo)
        .iter()
        .map(|r| join_atoms(r))
        .collect()
}

/// A piece `wrap_balanced` never splits: a word, or one character of a word
/// that has to break inside itself.
#[derive(Clone)]
struct Atom<'a> {
    text: &'a str,
    width: usize,
    /// Joined to the previous atom by a space (between words), not glued on.
    space_before: bool,
}

fn wrap_atoms(s: &str, width: usize) -> Vec<Atom<'_>> {
    let mut atoms = Vec::new();
    for word in s.split_whitespace() {
        let w = word.width();
        // CJK has no spaces to break at, so each wide character is its own atom.
        if w <= width && !word.chars().any(|c| c.width().unwrap_or(0) > 1) {
            atoms.push(Atom {
                text: word,
                width: w,
                space_before: true,
            });
            continue;
        }
        for (i, (at, c)) in word.char_indices().enumerate() {
            atoms.push(Atom {
                text: &word[at..at + c.len_utf8()],
                width: c.width().unwrap_or(0),
                space_before: i == 0,
            });
        }
    }
    atoms
}

/// First-fit rows of at most `width` columns. Never returns an empty row, so
/// an atom wider than `width` (a lone wide char in a 1-column box) still lands.
fn pack_atoms<'a>(atoms: &[Atom<'a>], width: usize) -> Vec<Vec<Atom<'a>>> {
    let mut rows: Vec<Vec<Atom>> = Vec::new();
    let mut row: Vec<Atom> = Vec::new();
    let mut used = 0;
    for a in atoms {
        let gap = usize::from(!row.is_empty() && a.space_before);
        if !row.is_empty() && used + gap + a.width > width {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        used += usize::from(!row.is_empty() && a.space_before) + a.width;
        row.push(a.clone());
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn join_atoms(atoms: &[Atom]) -> String {
    let mut out = String::new();
    for (i, a) in atoms.iter().enumerate() {
        if i > 0 && a.space_before {
            out.push(' ');
        }
        out.push_str(a.text);
    }
    out
}

/// `s` cut to `width` columns including a trailing `…`, which it always gets.
fn ellipsize(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// Format milliseconds as `m:ss`.
pub fn fmt_ms(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Convert a 0..=100 percentage to librespot's 0..=65535 volume range.
pub fn vol_u16(pct: u8) -> u16 {
    (pct as u32 * 65535 / 100) as u16
}

/// Vertically center a `height`-row rect inside `area`.
pub fn center_v(area: Rect, height: u16) -> Rect {
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x: area.x,
        y,
        width: area.width,
        height: height.min(area.height),
    }
}

/// Convert a `spotify:kind:id` URI to an open.spotify.com link.
pub fn uri_to_url(uri: &str) -> String {
    let mut p = uri.split(':');
    p.next();
    let kind = p.next().unwrap_or("");
    let id = p.next().unwrap_or("");
    format!("https://open.spotify.com/{kind}/{id}")
}

/// Pull the id out of a `spotify:track:<id>` URI.
pub fn track_id_from_uri(uri: &str) -> Option<String> {
    let mut parts = uri.split(':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("spotify"), Some("track"), Some(id)) => Some(id.to_string()),
        _ => None,
    }
}

/// Percent-encode a string for use in a query component.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
