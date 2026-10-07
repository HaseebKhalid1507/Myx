//! Real-time ten-band graphic equalizer.
//!
//! The equalizer sits in front of the visualizer and the real audio backend, so
//! the spectrum on screen is the spectrum the listener actually hears. The
//! audio thread only checks a tiny shared settings snapshot between packets;
//! it never waits for the UI and allocates nothing while processing samples.

use std::f64::consts::{PI, SQRT_2};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use rustfft::num_complex::Complex;
use serde::{Deserialize, Serialize};

pub const NUM_EQ_BANDS: usize = 10;
pub const EQ_FREQUENCIES_HZ: [f64; NUM_EQ_BANDS] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];
pub const MIN_EQ_GAIN_DB: i8 = -12;
pub const MAX_EQ_GAIN_DB: i8 = 12;

/// Persistable equalizer controls. Derived values such as the automatic
/// preamp are intentionally omitted and recomputed from the curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqualizerSettings {
    pub enabled: bool,
    pub gains_db: [i8; NUM_EQ_BANDS],
}

impl EqualizerSettings {
    pub fn normalized(mut self) -> Self {
        for gain in &mut self.gains_db {
            *gain = (*gain).clamp(MIN_EQ_GAIN_DB, MAX_EQ_GAIN_DB);
        }
        self
    }

    pub fn is_flat(self) -> bool {
        self.gains_db.iter().all(|&gain| gain == 0)
    }

    pub fn set_band(&mut self, band: usize, gain_db: i8) {
        if let Some(gain) = self.gains_db.get_mut(band) {
            *gain = gain_db.clamp(MIN_EQ_GAIN_DB, MAX_EQ_GAIN_DB);
        }
    }

    /// The automatic preamp applied before the filters, in dB. This is zero
    /// for a bypassed/flat curve and negative for a curve whose combined
    /// response rises above unity.
    pub fn auto_preamp_db(self) -> f64 {
        let settings = self.normalized();
        if !settings.enabled || settings.is_flat() {
            return 0.0;
        }
        let coeffs = coefficients(settings, SAMPLE_RATE);
        -peak_response_db(&coeffs, SAMPLE_RATE).max(0.0)
    }
}

impl Default for EqualizerSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            gains_db: [0; NUM_EQ_BANDS],
        }
    }
}

/// Built-in curves. A manually edited curve is represented in the UI as
/// `Custom`; it is deliberately not a preset because its values are the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EqualizerPreset {
    Flat,
    BassBoost,
    Rock,
    Jazz,
    Vocal,
    Electronic,
    TrebleBoost,
}

impl EqualizerPreset {
    pub const ALL: [Self; 7] = [
        Self::Flat,
        Self::BassBoost,
        Self::Rock,
        Self::Jazz,
        Self::Vocal,
        Self::Electronic,
        Self::TrebleBoost,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Flat => "Flat",
            Self::BassBoost => "Bass Boost",
            Self::Rock => "Rock",
            Self::Jazz => "Jazz",
            Self::Vocal => "Vocal",
            Self::Electronic => "Electronic",
            Self::TrebleBoost => "Treble Boost",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::BassBoost => "Bass",
            Self::Electronic => "Electro",
            Self::TrebleBoost => "Treble",
            other => other.label(),
        }
    }

    pub const fn gains_db(self) -> [i8; NUM_EQ_BANDS] {
        match self {
            Self::Flat => [0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            Self::BassBoost => [6, 6, 5, 4, 2, 0, -1, -2, -2, -2],
            Self::Rock => [4, 3, 2, 0, -2, -1, 1, 3, 4, 4],
            Self::Jazz => [3, 2, 1, 2, -1, -1, 0, 1, 2, 3],
            Self::Vocal => [-3, -2, -1, 1, 3, 4, 3, 1, -1, -2],
            Self::Electronic => [4, 3, 1, 0, -2, 1, 2, 3, 4, 3],
            Self::TrebleBoost => [-2, -2, -2, -1, 0, 2, 4, 5, 6, 6],
        }
    }

    pub fn from_gains(gains_db: &[i8; NUM_EQ_BANDS]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|preset| preset.gains_db() == *gains_db)
    }
}

pub(crate) type EqualizerControl = Arc<Mutex<EqualizerSettings>>;

pub(crate) fn shared_equalizer() -> EqualizerControl {
    Arc::new(Mutex::new(EqualizerSettings::default()))
}

const SAMPLE_RATE: f64 = 44_100.0;
const HEADROOM_RAMP_FRAMES: usize = 882; // 20ms at 44.1kHz
const RESPONSE_POINTS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Coefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coefficients {
    const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
}

#[derive(Debug, Default, Clone, Copy)]
struct FilterState {
    z1: f64,
    z2: f64,
}

impl FilterState {
    #[inline]
    fn process(&mut self, input: f64, coeffs: Coefficients) -> f64 {
        // Transposed direct form II: two delay values and good numerical
        // behaviour when ten sections are cascaded.
        let output = coeffs.b0 * input + self.z1;
        self.z1 = coeffs.b1 * input - coeffs.a1 * output + self.z2;
        self.z2 = coeffs.b2 * input - coeffs.a2 * output;
        output
    }
}

struct FilterBank {
    settings: EqualizerSettings,
    coeffs: [Coefficients; NUM_EQ_BANDS],
    states: [[FilterState; NUM_EQ_BANDS]; 2],
    preamp: f64,
    target_preamp: f64,
    preamp_step: f64,
    ramp_frames: usize,
}

impl FilterBank {
    fn new(settings: EqualizerSettings) -> Self {
        let settings = settings.normalized();
        let target_preamp = db_to_linear(settings.auto_preamp_db());
        Self {
            settings,
            coeffs: coefficients(settings, SAMPLE_RATE),
            states: [[FilterState::default(); NUM_EQ_BANDS]; 2],
            preamp: target_preamp,
            target_preamp,
            preamp_step: 0.0,
            ramp_frames: 0,
        }
    }

    fn update(&mut self, settings: EqualizerSettings) {
        let settings = settings.normalized();
        if settings == self.settings {
            return;
        }
        self.settings = settings;
        self.coeffs = coefficients(settings, SAMPLE_RATE);
        self.target_preamp = db_to_linear(settings.auto_preamp_db());
        if !settings.enabled || settings.is_flat() {
            self.reset_states();
            // Bypass really is unity. Starting the next enabled curve from
            // unity also makes the headroom transition do what the ear expects.
            self.preamp = 1.0;
            self.target_preamp = 1.0;
            self.preamp_step = 0.0;
            self.ramp_frames = 0;
        } else {
            // More attenuation has to land with the boosted coefficients or a
            // preset change could clip during the ramp. Returning towards
            // unity may be smoothed safely.
            if self.target_preamp < self.preamp {
                self.preamp = self.target_preamp;
                self.preamp_step = 0.0;
                self.ramp_frames = 0;
            } else {
                self.preamp_step = (self.target_preamp - self.preamp) / HEADROOM_RAMP_FRAMES as f64;
                self.ramp_frames = HEADROOM_RAMP_FRAMES;
            }
        }
    }

    fn process_interleaved(&mut self, samples: &mut [f64]) {
        if !self.settings.enabled || self.settings.is_flat() {
            return;
        }
        for frame in samples.chunks_mut(2) {
            if self.ramp_frames > 0 {
                self.preamp += self.preamp_step;
                self.ramp_frames -= 1;
                if self.ramp_frames == 0 {
                    self.preamp = self.target_preamp;
                }
            }
            for (channel, sample) in frame.iter_mut().enumerate() {
                let mut value = *sample * self.preamp;
                for (state, &coeffs) in self.states[channel].iter_mut().zip(self.coeffs.iter()) {
                    value = state.process(value, coeffs);
                }
                *sample = value;
            }
        }
    }

    fn reset_states(&mut self) {
        self.states = [[FilterState::default(); NUM_EQ_BANDS]; 2];
    }
}

/// A transforming sink: unlike the visualizer tee, this intentionally replaces
/// sample packets before forwarding them to the next sink.
pub(crate) struct EqualizerSink {
    inner: Box<dyn Sink>,
    control: EqualizerControl,
    filters: FilterBank,
}

impl EqualizerSink {
    pub(crate) fn new(inner: Box<dyn Sink>, control: EqualizerControl) -> Self {
        let settings = *control.lock().unwrap_or_else(PoisonError::into_inner);
        Self {
            inner,
            control,
            filters: FilterBank::new(settings),
        }
    }

    fn refresh_settings(&mut self) {
        let settings = match self.control.try_lock() {
            Ok(settings) => *settings,
            Err(TryLockError::WouldBlock) => return,
            Err(TryLockError::Poisoned(error)) => *error.into_inner(),
        };
        self.filters.update(settings);
    }
}

impl Sink for EqualizerSink {
    fn start(&mut self) -> SinkResult<()> {
        self.filters.reset_states();
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.filters.reset_states();
        self.inner.stop()
    }

    fn write(&mut self, mut packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        self.refresh_settings();
        if let AudioPacket::Samples(samples) = &mut packet {
            self.filters.process_interleaved(samples);
        }
        self.inner.write(packet, converter)
    }
}

fn coefficients(settings: EqualizerSettings, sample_rate: f64) -> [Coefficients; NUM_EQ_BANDS] {
    std::array::from_fn(|band| {
        let gain = settings.gains_db[band];
        if gain == 0 {
            Coefficients::IDENTITY
        } else {
            peaking_coefficients(EQ_FREQUENCIES_HZ[band], f64::from(gain), sample_rate)
        }
    })
}

/// Robert Bristow-Johnson Audio EQ Cookbook peaking-EQ coefficients, normalized
/// by a0 so the hot path needs five multiplies per section.
fn peaking_coefficients(frequency: f64, gain_db: f64, sample_rate: f64) -> Coefficients {
    let amplitude = 10.0_f64.powf(gain_db / 40.0);
    let omega = 2.0 * PI * frequency / sample_rate;
    let alpha = omega.sin() / (2.0 * SQRT_2);
    let cos = omega.cos();
    let a0 = 1.0 + alpha / amplitude;
    Coefficients {
        b0: (1.0 + alpha * amplitude) / a0,
        b1: (-2.0 * cos) / a0,
        b2: (1.0 - alpha * amplitude) / a0,
        a1: (-2.0 * cos) / a0,
        a2: (1.0 - alpha / amplitude) / a0,
    }
}

fn peak_response_db(coeffs: &[Coefficients; NUM_EQ_BANDS], sample_rate: f64) -> f64 {
    let max_frequency = 20_000.0_f64.min(sample_rate * 0.49);
    let ratio = max_frequency / 20.0;
    (0..RESPONSE_POINTS)
        .map(|point| {
            let t = point as f64 / (RESPONSE_POINTS - 1) as f64;
            let frequency = 20.0 * ratio.powf(t);
            cascade_response_db(coeffs, frequency, sample_rate)
        })
        .fold(f64::NEG_INFINITY, f64::max)
}

fn cascade_response_db(
    coeffs: &[Coefficients; NUM_EQ_BANDS],
    frequency: f64,
    sample_rate: f64,
) -> f64 {
    let omega = 2.0 * PI * frequency / sample_rate;
    let z1 = Complex::from_polar(1.0, -omega);
    let z2 = Complex::from_polar(1.0, -2.0 * omega);
    coeffs
        .iter()
        .map(|c| {
            let numerator = c.b0 + c.b1 * z1 + c.b2 * z2;
            let denominator = 1.0 + c.a1 * z1 + c.a2 * z2;
            20.0 * (numerator / denominator).norm().log10()
        })
        .sum()
}

fn db_to_linear(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_active_flat_and_normalization_clamps_saved_values() {
        let default = EqualizerSettings::default();
        assert!(default.enabled);
        assert!(default.is_flat());

        let normalized = EqualizerSettings {
            enabled: true,
            gains_db: [-30, 30, 0, 0, 0, 0, 0, 0, 0, 0],
        }
        .normalized();
        assert_eq!(normalized.gains_db[0], MIN_EQ_GAIN_DB);
        assert_eq!(normalized.gains_db[1], MAX_EQ_GAIN_DB);
    }

    #[test]
    fn presets_are_valid_unique_curves_and_recognize_exact_matches() {
        for (index, preset) in EqualizerPreset::ALL.iter().enumerate() {
            let gains = preset.gains_db();
            assert!(gains
                .iter()
                .all(|gain| (MIN_EQ_GAIN_DB..=MAX_EQ_GAIN_DB).contains(gain)));
            assert_eq!(EqualizerPreset::from_gains(&gains), Some(*preset));
            for other in &EqualizerPreset::ALL[index + 1..] {
                assert_ne!(gains, other.gains_db());
            }
        }
    }

    #[test]
    fn flat_and_bypass_are_exact_passthroughs() {
        let input = [0.25, -0.5, 0.75, -1.0];
        for settings in [
            EqualizerSettings::default(),
            EqualizerSettings {
                enabled: false,
                gains_db: EqualizerPreset::BassBoost.gains_db(),
            },
        ] {
            let mut samples = input;
            FilterBank::new(settings).process_interleaved(&mut samples);
            assert_eq!(samples, input);
        }
    }

    #[test]
    fn automatic_preamp_cancels_the_sampled_peak_response() {
        for preset in EqualizerPreset::ALL {
            let settings = EqualizerSettings {
                enabled: true,
                gains_db: preset.gains_db(),
            };
            let coeffs = coefficients(settings, SAMPLE_RATE);
            let compensated = peak_response_db(&coeffs, SAMPLE_RATE) + settings.auto_preamp_db();
            assert!(compensated <= 1e-9, "{}: {compensated}", preset.label());
        }
    }

    #[test]
    fn stereo_channels_keep_independent_filter_history() {
        let settings = EqualizerSettings {
            enabled: true,
            gains_db: EqualizerPreset::BassBoost.gains_db(),
        };
        let mut filters = FilterBank::new(settings);
        let mut samples = vec![0.0; 4_096];
        samples[0] = 0.5; // left-channel impulse; right stays silent
        filters.process_interleaved(&mut samples);
        assert!(samples.iter().step_by(2).any(|sample| sample.abs() > 0.0));
        assert!(samples
            .iter()
            .skip(1)
            .step_by(2)
            .all(|sample| *sample == 0.0));
        assert!(samples.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn bass_and_treble_presets_tilt_the_expected_ends() {
        let response = |preset: EqualizerPreset, frequency| {
            let settings = EqualizerSettings {
                enabled: true,
                gains_db: preset.gains_db(),
            };
            cascade_response_db(&coefficients(settings, SAMPLE_RATE), frequency, SAMPLE_RATE)
        };
        assert!(
            response(EqualizerPreset::BassBoost, 62.0)
                - response(EqualizerPreset::BassBoost, 4_000.0)
                > 6.0
        );
        assert!(
            response(EqualizerPreset::TrebleBoost, 8_000.0)
                - response(EqualizerPreset::TrebleBoost, 125.0)
                > 6.0
        );
    }
}
