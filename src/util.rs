//! Small pure helpers shared by the UI and the workers.
//!
//! Everything here is dependency-light and side-effect free, so it can be
//! unit-tested without a terminal, a network, or an audio device.

use ratatui::layout::Rect;

/// Truncate to `max` characters, replacing the tail with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    } else {
        s.to_string()
    }
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
