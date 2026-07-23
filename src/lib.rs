//! myx — a lean, beautiful terminal Spotify player.
//!
//! FE: the design-token system (noodle's visual language) ported to ratatui,
//! plus album-art-reactive theming with cross-fades.
//! Backend (`streaming` feature): a lean librespot engine — Connect device + tee'd
//! FFT visualizer + real track-change events.

pub mod anim;
pub mod color;
pub mod components;
pub mod cover;
pub mod gradient;
pub mod reactive;
pub mod theme;

#[cfg(feature = "streaming")]
pub mod audio;
#[cfg(feature = "streaming")]
pub mod engine;
#[cfg(feature = "streaming")]
pub mod webapi;
