//! The audio output device: whether there is one to play through, and what
//! stands in when there isn't.
//!
//! librespot's rodio sink opens the default output device with an `unwrap`, on
//! a player thread. A machine with no usable device — typically PipeWire with
//! no ALSA plugin, so ALSA has nothing to play to (#53, #17) — got a panic,
//! and Myx's panic hook took the whole UI down with it.

use std::time::Duration;

use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;

/// What to tell someone with no audio output: why, and the usual fix.
pub const NO_AUDIO_OUTPUT: &str = "\
no audio output device, so there's nothing to play through.

On Linux myx plays through ALSA. With PipeWire, install your distribution's
PipeWire ALSA plugin (PulseAudio isn't needed):
  Arch, Debian, Ubuntu, Fedora:  pipewire-alsa
  Void:   alsa-pipewire, then link 50-pipewire.conf and 99-pipewire-default.conf
          from /usr/share/alsa/alsa.conf.d into /etc/alsa/conf.d
  NixOS:  services.pipewire.alsa.enable = true;
`aplay -L` should then list a default device. See the README's Audio section.";

/// Whether the default output device can be opened the way librespot's rodio
/// sink opens it: the default host's default device, and its default config.
pub fn output_available() -> bool {
    use cpal::traits::{DeviceTrait, HostTrait};
    cpal::default_host()
        .default_output_device()
        .is_some_and(|device| device.default_output_config().is_ok())
}

/// Plays nothing, in real time. Stands in for the device when it's gone, so
/// Myx keeps running (and says why there's no sound) instead of crashing.
///
/// It waits as long as each packet would take to play, like a device would:
/// a sink that returned at once would let the player race through tracks at
/// decoding speed.
#[derive(Debug, Default)]
pub struct SilentSink;

/// librespot decodes to interleaved stereo at 44.1 kHz.
const SAMPLES_PER_SECOND: f64 = 2.0 * 44_100.0;

impl SilentSink {
    /// How long `packet` takes to play.
    pub fn duration_of(packet: &AudioPacket) -> Duration {
        match packet {
            AudioPacket::Samples(samples) => {
                Duration::from_secs_f64(samples.len() as f64 / SAMPLES_PER_SECOND)
            }
            AudioPacket::Raw(_) => Duration::ZERO,
        }
    }
}

impl Sink for SilentSink {
    fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        std::thread::sleep(Self::duration_of(&packet));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_sink_takes_as_long_as_the_audio() {
        // A tenth of a second of stereo at 44.1 kHz.
        let packet = AudioPacket::Samples(vec![0.0; 8_820]);
        assert_eq!(SilentSink::duration_of(&packet), Duration::from_millis(100));
        let mut converter = Converter::new(None);
        let t = std::time::Instant::now();
        SilentSink.write(packet, &mut converter).expect("write");
        assert!(
            t.elapsed() >= Duration::from_millis(95),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn the_message_names_the_fix_for_each_family() {
        for needle in [
            "pipewire-alsa",
            "alsa-pipewire",
            "99-pipewire-default.conf",
            "aplay -L",
        ] {
            assert!(NO_AUDIO_OUTPUT.contains(needle), "{needle}");
        }
    }
}
