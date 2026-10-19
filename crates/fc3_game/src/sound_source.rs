//! Sounds decoded as they load.
//!
//! Bevy's own audio source keeps the file's bytes and builds a decoder when
//! playback starts, on the main thread; for Ogg Vorbis that parses the
//! codebooks twice over, and a few sounds starting in one frame (sound points
//! coming into earshot, a burst of fire) cost 5-9 ms. Here the loader (on the
//! loading threads) decodes each clip to samples up front, so starting one
//! only wraps the samples. Long ones (ambient beds) stay compressed and decode
//! as they play.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use bevy::asset::{io::Reader, AssetLoader, LoadContext};
use bevy::audio::{AddAudioSource, ChannelCount, Decodable, SampleRate, Source};
use bevy::prelude::*;
use rodio::source::SeekError;

/// Clips decoding to more samples than this (about 12 MB) stay compressed.
const MAX_DECODED: usize = 3 << 20;

pub struct SoundSourcePlugin;

impl Plugin for SoundSourcePlugin {
    fn build(&self, app: &mut App) {
        app.add_audio_source::<Sfx>().register_asset_loader(SfxLoader);
    }
}

/// A sound effect: its samples, or the file for the long ones; and the
/// mixer's low-pass slots it plays through (see `mixer::filters_of`).
#[derive(Asset, TypePath, Clone)]
pub enum Sfx {
    Decoded { samples: Arc<[f32]>, channels: ChannelCount, rate: SampleRate, filters: Arc<[u8]> },
    Compressed(Arc<[u8]>, Arc<[u8]>),
}

fn decoder(bytes: &Arc<[u8]>) -> Result<rodio::Decoder<Cursor<Arc<[u8]>>>, rodio::decoder::DecoderError> {
    rodio::Decoder::builder().with_byte_len(bytes.len() as u64).with_data(Cursor::new(bytes.clone())).build()
}

#[derive(Default, TypePath)]
struct SfxLoader;

impl AssetLoader for SfxLoader {
    type Asset = Sfx;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(&self, reader: &mut dyn Reader, _settings: &(), context: &mut LoadContext<'_>) -> Result<Sfx, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let bytes: Arc<[u8]> = bytes.into();
        let filters = crate::mixer::filters_of(context.path().path().file_stem().and_then(|s| s.to_str()).unwrap_or_default());
        let mut d = decoder(&bytes).map_err(std::io::Error::other)?;
        let (channels, rate) = (d.channels(), d.sample_rate());
        let mut samples = Vec::new();
        for s in d.by_ref() {
            if samples.len() == MAX_DECODED {
                return Ok(Sfx::Compressed(bytes, filters));
            }
            samples.push(s);
        }
        Ok(Sfx::Decoded { samples: samples.into(), channels, rate, filters })
    }

    fn extensions(&self) -> &[&str] {
        &["ogg", "wav"]
    }
}

impl Decodable for Sfx {
    type Decoder = SfxDecoder;

    fn decoder(&self) -> SfxDecoder {
        let (inner, filters) = match self {
            Sfx::Decoded { samples, channels, rate, filters } => {
                (Inner::Decoded { samples: samples.clone(), pos: 0, channels: *channels, rate: *rate }, filters.clone())
            }
            Sfx::Compressed(bytes, filters) => (Inner::Compressed(Box::new(decoder(bytes).expect("decoded when loaded"))), filters.clone()),
        };
        let (channels, rate) = (inner.channels().get() as usize, inner.sample_rate().get() as f32);
        SfxDecoder { inner, filter: LowPass { slots: filters, state: [[0.0; 2]; 8], a: 1.0, countdown: 0, channel: 0, channels: channels.clamp(1, 8), rate } }
    }
}

/// The mixer's low-pass on a playing sound: two one-pole stages per
/// channel at the lowest cutoff of its slots (checked every 256 samples).
struct LowPass {
    slots: Arc<[u8]>,
    state: [[f32; 2]; 8],
    /// The stages' coefficient (1: open).
    a: f32,
    countdown: u32,
    channel: usize,
    channels: usize,
    rate: f32,
}

impl LowPass {
    #[inline]
    fn apply(&mut self, x: f32) -> f32 {
        if self.slots.is_empty() {
            return x;
        }
        if self.countdown == 0 {
            self.countdown = 256;
            self.a = crate::mixer::cutoff(&self.slots).map_or(1.0, |hz| 1.0 - (-std::f32::consts::TAU * hz / self.rate.max(1.0)).exp());
        }
        self.countdown -= 1;
        let s = &mut self.state[self.channel];
        self.channel = (self.channel + 1) % self.channels;
        if self.a >= 0.999 {
            *s = [x, x];
            return x;
        }
        s[0] += self.a * (x - s[0]);
        s[1] += self.a * (s[0] - s[1]);
        s[1]
    }
}

pub struct SfxDecoder {
    inner: Inner,
    filter: LowPass,
}

impl Iterator for SfxDecoder {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        let x = self.inner.next()?;
        Some(self.filter.apply(x))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl Source for SfxDecoder {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, to: Duration) -> Result<(), SeekError> {
        self.filter.channel = 0;
        self.inner.try_seek(to)
    }
}

enum Inner {
    Decoded { samples: Arc<[f32]>, pos: usize, channels: ChannelCount, rate: SampleRate },
    Compressed(Box<rodio::Decoder<Cursor<Arc<[u8]>>>>),
}

impl Iterator for Inner {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        match self {
            Inner::Decoded { samples, pos, .. } => {
                let s = samples.get(*pos).copied();
                *pos += 1;
                s
            }
            Inner::Compressed(d) => d.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Inner::Decoded { samples, pos, .. } => {
                let n = samples.len().saturating_sub(*pos);
                (n, Some(n))
            }
            Inner::Compressed(d) => d.size_hint(),
        }
    }
}

impl Source for Inner {
    fn current_span_len(&self) -> Option<usize> {
        match self {
            // One span, as rodio's own sample buffer does it.
            Inner::Decoded { samples, pos, .. } => Some(if *pos >= samples.len() { 0 } else { samples.len() }),
            Inner::Compressed(d) => d.current_span_len(),
        }
    }

    fn channels(&self) -> ChannelCount {
        match self {
            Inner::Decoded { channels, .. } => *channels,
            Inner::Compressed(d) => d.channels(),
        }
    }

    fn sample_rate(&self) -> SampleRate {
        match self {
            Inner::Decoded { rate, .. } => *rate,
            Inner::Compressed(d) => d.sample_rate(),
        }
    }

    fn total_duration(&self) -> Option<Duration> {
        match self {
            Inner::Decoded { samples, channels, rate, .. } => {
                let frames = samples.len() as u64 / channels.get() as u64;
                Some(Duration::from_secs_f64(frames as f64 / rate.get() as f64))
            }
            Inner::Compressed(d) => d.total_duration(),
        }
    }

    fn try_seek(&mut self, to: Duration) -> Result<(), SeekError> {
        match self {
            Inner::Decoded { samples, pos, channels, rate } => {
                let frame = (to.as_secs_f64() * rate.get() as f64) as usize;
                *pos = (frame * channels.get() as usize).min(samples.len());
                Ok(())
            }
            Inner::Compressed(d) => d.try_seek(to),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_low_passes_the_sounds_below_its_unit() {
        // A bus with one low-pass unit; a state turning it on at 800 Hz.
        let mut mix = crate::mixer::MixData::default();
        mix.read("bus", "00007000 00006000 0 0");
        mix.read("fx", "00007001 00007000 0 20000");
        mix.read("mixfx", "00000200 00007001:0:1:0:0 00007001:1:800:0:0");
        mix.read("mixevent", "00000020 +00000200");
        let tone = |filter: &mut LowPass| -> f32 {
            let samples: Vec<f32> = (0..4800).map(|i| (i as f32 * std::f32::consts::TAU * 5000.0 / 48000.0).sin()).map(|x| filter.apply(x)).collect();
            (samples[2400..].iter().map(|s| s * s).sum::<f32>() / 2400.0).sqrt()
        };
        let filter = || LowPass { slots: Arc::from([0u8]), state: [[0.0; 2]; 8], a: 1.0, countdown: 0, channel: 0, channels: 1, rate: 48000.0 };
        let open = tone(&mut filter());
        let mut mixer = crate::mixer::Mixer::default();
        mixer.apply(&mix, 0x20);
        mixer.advance(&mix, 0.1);
        let muffled = tone(&mut filter());
        // A 5 kHz tone through two 800 Hz poles: well under a tenth.
        assert!(open > 0.6 && muffled < open * 0.1, "{open} {muffled}");
    }
}
