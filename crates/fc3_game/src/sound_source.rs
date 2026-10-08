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

/// A sound effect: its samples, or the file for the long ones.
#[derive(Asset, TypePath, Clone)]
pub enum Sfx {
    Decoded { samples: Arc<[f32]>, channels: ChannelCount, rate: SampleRate },
    Compressed(Arc<[u8]>),
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

    async fn load(&self, reader: &mut dyn Reader, _settings: &(), _context: &mut LoadContext<'_>) -> Result<Sfx, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let bytes: Arc<[u8]> = bytes.into();
        let mut d = decoder(&bytes).map_err(std::io::Error::other)?;
        let (channels, rate) = (d.channels(), d.sample_rate());
        let mut samples = Vec::new();
        for s in d.by_ref() {
            if samples.len() == MAX_DECODED {
                return Ok(Sfx::Compressed(bytes));
            }
            samples.push(s);
        }
        Ok(Sfx::Decoded { samples: samples.into(), channels, rate })
    }

    fn extensions(&self) -> &[&str] {
        &["ogg", "wav"]
    }
}

impl Decodable for Sfx {
    type Decoder = SfxDecoder;

    fn decoder(&self) -> SfxDecoder {
        match self {
            Sfx::Decoded { samples, channels, rate } => SfxDecoder::Decoded { samples: samples.clone(), pos: 0, channels: *channels, rate: *rate },
            Sfx::Compressed(bytes) => SfxDecoder::Compressed(Box::new(decoder(bytes).expect("decoded when loaded"))),
        }
    }
}

pub enum SfxDecoder {
    Decoded { samples: Arc<[f32]>, pos: usize, channels: ChannelCount, rate: SampleRate },
    Compressed(Box<rodio::Decoder<Cursor<Arc<[u8]>>>>),
}

impl Iterator for SfxDecoder {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        match self {
            SfxDecoder::Decoded { samples, pos, .. } => {
                let s = samples.get(*pos).copied();
                *pos += 1;
                s
            }
            SfxDecoder::Compressed(d) => d.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            SfxDecoder::Decoded { samples, pos, .. } => {
                let n = samples.len().saturating_sub(*pos);
                (n, Some(n))
            }
            SfxDecoder::Compressed(d) => d.size_hint(),
        }
    }
}

impl Source for SfxDecoder {
    fn current_span_len(&self) -> Option<usize> {
        match self {
            // One span, as rodio's own sample buffer does it.
            SfxDecoder::Decoded { samples, pos, .. } => Some(if *pos >= samples.len() { 0 } else { samples.len() }),
            SfxDecoder::Compressed(d) => d.current_span_len(),
        }
    }

    fn channels(&self) -> ChannelCount {
        match self {
            SfxDecoder::Decoded { channels, .. } => *channels,
            SfxDecoder::Compressed(d) => d.channels(),
        }
    }

    fn sample_rate(&self) -> SampleRate {
        match self {
            SfxDecoder::Decoded { rate, .. } => *rate,
            SfxDecoder::Compressed(d) => d.sample_rate(),
        }
    }

    fn total_duration(&self) -> Option<Duration> {
        match self {
            SfxDecoder::Decoded { samples, channels, rate, .. } => {
                let frames = samples.len() as u64 / channels.get() as u64;
                Some(Duration::from_secs_f64(frames as f64 / rate.get() as f64))
            }
            SfxDecoder::Compressed(d) => d.total_duration(),
        }
    }

    fn try_seek(&mut self, to: Duration) -> Result<(), SeekError> {
        match self {
            SfxDecoder::Decoded { samples, pos, channels, rate } => {
                let frame = (to.as_secs_f64() * rate.get() as f64) as usize;
                *pos = (frame * channels.get() as usize).min(samples.len());
                Ok(())
            }
            SfxDecoder::Compressed(d) => d.try_seek(to),
        }
    }
}
