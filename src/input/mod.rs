//! The input layer.
//!
//! One-way dependency, and the one that runs the other way from `ui`: everything
//! here takes a terminal or media-key event and turns it into a mutation of
//! `App` plus sends on the `UiChannels` senders. Nothing here draws — it only
//! reads `FrameOut` for the hit rects the renderer left behind. One module per
//! input source, so the file to open is the one named after where the event
//! came from.

mod actions;
mod key;
mod media;
mod mouse;

pub(crate) use actions::*;
pub(crate) use key::*;
pub(crate) use media::*;
pub(crate) use mouse::*;
