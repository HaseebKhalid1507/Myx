//! `mxc_demo` — an external process that recolors itself live from MXC.
//!
//! The real implementation lives in `mxc_demo_support/imp.rs`. This file exists
//! only to keep the demo off non-Unix platforms.
//!
//! MXC is an `AF_UNIX` protocol, so the demo cannot exist on Windows — and
//! Cargo has no way to target-gate an `[[example]]`. Without this split,
//! `cargo test` and `cargo clippy --all-targets` would fail to compile on
//! Windows even though the player itself builds there fine.
//!
//! The support directory has no `main.rs`, so Cargo does not auto-discover it
//! as a second example. That is deliberate — it lets `dump_theme`, `libcheck`,
//! and `radiocheck` keep being discovered normally, which setting
//! `autoexamples = false` would have quietly broken.
//!
//! ```text
//! cargo run --example mxc_demo                 # $XDG_RUNTIME_DIR/myx/theme.sock
//! cargo run --example mxc_demo /tmp/my.sock    # explicit path
//! cargo run --example mxc_demo -- --fake       # no Myx required
//! ```

#[cfg(unix)]
#[path = "mxc_demo_support/imp.rs"]
mod imp;

#[cfg(unix)]
fn main() {
    imp::main();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("mxc_demo: MXC is Unix-only — it needs an AF_UNIX socket.");
    std::process::exit(1);
}
