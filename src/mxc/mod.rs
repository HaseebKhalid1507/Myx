//! MXC — the Myx Color Protocol.
//!
//! Myx derives a 16-token semantic palette from album art on every track change.
//! Without MXC that palette dies inside the process. MXC makes it a **published
//! local resource**: Myx opens a Unix socket, writes newline-delimited JSON, and
//! any process that wants album-reactive color subscribes. Myx has zero
//! knowledge of its consumers.
//!
//! The protocol is deliberately small: **one socket, one message shape, full
//! state every time, snapshot on connect.**
//!
//! Spec: `~/Jawz/notes/tech/myx-color-protocol.md` (v0.1.0).
//!
//! This module is the *protocol* half — pure data types and pure color math,
//! no I/O. The publisher (`UnixListener`, fan-out, dedupe) is a separate
//! concern so that these types stay trivially portable to consumers.
//!
//! ## Layout
//!
//! - [`wire`] — the serde types that define the byte-level contract.
//! - [`contrast`] — WCAG relative luminance and the `on_*` foreground picker.
//!
//! ## Why the contrast math lives here
//!
//! Every surveyed media-theming project reimplements luminance clamping, and
//! most do it wrong (see spec §3.3). Publishing `is_dark` and `contrast`
//! *once, correctly* means no consumer has to. That is a protocol
//! responsibility, not a consumer one.

pub mod contrast;
pub mod wire;

pub use contrast::Contrast;
pub use wire::{ByeReason, Colors, Hex, Message, Origin, OriginKind};

use std::path::PathBuf;

/// Protocol major version. Bumped only for breaking changes — removing or
/// renaming a color token, changing hex format, or changing framing.
///
/// Additive changes (new optional envelope fields, new [`OriginKind`] values,
/// new `on_*` keys) do NOT bump this. Consumers ignore what they don't know.
pub const PROTOCOL_VERSION: u32 = 1;

/// Socket path: `$XDG_RUNTIME_DIR/myx/theme.sock`.
///
/// Falls back to `/tmp/myx-$UID/theme.sock` when `XDG_RUNTIME_DIR` is unset
/// (bare TTY logins, some minimal containers). The fallback is uid-scoped so
/// two users on one box never collide.
pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: `getuid` is always safe — it cannot fail and touches no
            // memory we own.
            let uid = unsafe { libc_getuid() };
            PathBuf::from(format!("/tmp/myx-{uid}"))
        });
    dir.join("myx").join("theme.sock")
}

/// Minimal `getuid` shim so the fallback path doesn't pull in a `libc`
/// dependency for one call.
#[cfg(unix)]
unsafe fn libc_getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    getuid()
}

#[cfg(not(unix))]
unsafe fn libc_getuid() -> u32 {
    0
}

/// Unix epoch milliseconds, for the `ts` envelope field.
///
/// Saturates to `0` if the clock is before the epoch rather than panicking —
/// a bad clock must never take down the player.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_honours_xdg_runtime_dir() {
        // Not using std::env::set_var here (unsound under parallel tests);
        // instead assert the shape of whatever the environment yields.
        let p = socket_path();
        assert!(
            p.ends_with("myx/theme.sock"),
            "socket must always terminate in myx/theme.sock, got {p:?}"
        );
        assert!(p.is_absolute(), "socket path must be absolute, got {p:?}");
    }

    #[test]
    fn now_ms_is_plausible() {
        // Sanity floor: 2020-01-01. Guards against a unit mix-up (secs vs ms).
        assert!(now_ms() > 1_577_836_800_000);
    }
}
