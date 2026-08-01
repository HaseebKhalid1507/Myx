//! The binary's unit tests, kept in the crate (not `tests/`) because they
//! exercise items that are private to `main.rs`.

mod nav;
mod playlist;

/// Live-API tests, `#[ignore]`d so `cargo test` stays offline:
///
///     cargo test --bin myx -- --ignored --nocapture
///
/// They catch Spotify changing an endpoint out from under the artist page,
/// which is how the 403/400 pair that emptied it went unnoticed.
mod live;
