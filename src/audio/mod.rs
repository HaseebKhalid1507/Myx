//! Audio engine internals (streaming feature).

pub mod equalizer;
pub mod visualizer;

pub use equalizer::{
    EqualizerPreset, EqualizerSettings, EQ_FREQUENCIES_HZ, MAX_EQ_GAIN_DB, MIN_EQ_GAIN_DB,
    NUM_EQ_BANDS,
};
pub use visualizer::{VisBands, VisualizationSink, NUM_BANDS};
