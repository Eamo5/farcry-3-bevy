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

/// A sound effect: its samples, or the file for the long ones; the mixer's
/// low-pass slots it plays through (see `mixer::filters_of`) and its sends
/// to the reverb (`mixer::sends_of`); and whether its reverb rings on once
/// it ends (not when looped: its `#loop` asset).
#[derive(Asset, TypePath, Clone)]
pub enum Sfx {
    Decoded { samples: Arc<[f32]>, channels: ChannelCount, rate: SampleRate, filters: Arc<[u8]>, sends: Arc<[u8]>, tail: bool },
    Compressed { bytes: Arc<[u8]>, filters: Arc<[u8]>, sends: Arc<[u8]>, tail: bool },
}

impl Sfx {
    /// The same sound, its reverb cut as it ends (to be looped).
    fn looped(&self) -> Sfx {
        let mut s = self.clone();
        match &mut s {
            Sfx::Decoded { tail, .. } | Sfx::Compressed { tail, .. } => *tail = false,
        }
        s
    }
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
        let stem = context.path().path().file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        let (filters, sends) = (crate::mixer::filters_of(&stem), crate::mixer::sends_of(&stem));
        let mut d = decoder(&bytes).map_err(std::io::Error::other)?;
        let (channels, rate) = (d.channels(), d.sample_rate());
        let mut samples = Vec::new();
        let mut compressed = false;
        for s in d.by_ref() {
            if samples.len() == MAX_DECODED {
                compressed = true;
                break;
            }
            samples.push(s);
        }
        let sfx = if compressed {
            Sfx::Compressed { bytes, filters, sends, tail: true }
        } else {
            Sfx::Decoded { samples: samples.into(), channels, rate, filters, sends, tail: true }
        };
        // (And `#loop`: the same, its reverb cut as it ends.)
        context.add_labeled_asset("loop".to_string(), sfx.looped());
        Ok(sfx)
    }

    fn extensions(&self) -> &[&str] {
        &["ogg", "wav"]
    }
}

impl Decodable for Sfx {
    type Decoder = SfxDecoder;

    fn decoder(&self) -> SfxDecoder {
        let (inner, filters, sends, tail) = match self {
            Sfx::Decoded { samples, channels, rate, filters, sends, tail } => {
                (Inner::Decoded { samples: samples.clone(), pos: 0, channels: *channels, rate: *rate }, filters.clone(), sends.clone(), *tail)
            }
            Sfx::Compressed { bytes, filters, sends, tail } => {
                (Inner::Compressed(Box::new(decoder(bytes).expect("decoded when loaded"))), filters.clone(), sends.clone(), *tail)
            }
        };
        let (channels, rate) = (inner.channels().get() as usize, inner.sample_rate().get() as f32);
        SfxDecoder {
            reverb: Reverb::new(sends, tail, channels.clamp(1, 8), rate),
            inner,
            filter: LowPass {
                slots: filters,
                state: [[0.0; 2]; 8],
                high: [0.0; 8],
                a: 1.0,
                b: 0.0,
                countdown: 0,
                channel: 0,
                channels: channels.clamp(1, 8),
                rate,
            },
        }
    }
}

/// The mixer's filters on a playing sound: two one-pole low-pass stages
/// per channel at the lowest low-pass cutoff of its slots, then a one-pole
/// high-pass at the highest high-pass cutoff (checked every 256 samples).
struct LowPass {
    slots: Arc<[u8]>,
    state: [[f32; 2]; 8],
    /// The high-pass's own low-pass state, per channel.
    high: [f32; 8],
    /// The low-pass stages' coefficient (1: open), the high-pass's (0: off).
    a: f32,
    b: f32,
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
            let coefficient = |hz: f32| 1.0 - (-std::f32::consts::TAU * hz / self.rate.max(1.0)).exp();
            let (low, high) = crate::mixer::cutoffs(&self.slots);
            self.a = low.map_or(1.0, coefficient);
            self.b = high.map_or(0.0, coefficient);
        }
        self.countdown -= 1;
        let ch = self.channel;
        self.channel = (self.channel + 1) % self.channels;
        let s = &mut self.state[ch];
        let y = if self.a >= 0.999 {
            *s = [x, x];
            x
        } else {
            s[0] += self.a * (x - s[0]);
            s[1] += self.a * (s[0] - s[1]);
            s[1]
        };
        // (The high-pass: what's left over a low-pass at its cutoff.)
        let h = &mut self.high[ch];
        *h += self.b * (y - *h);
        if self.b > 0.0 {
            y - *h
        } else {
            y
        }
    }
}

/// The early reflections: delays (times the preset's) and gains.
const EARLY: [(f32, f32); 4] = [(1.0, 0.6), (1.7, 0.45), (2.3, 0.33), (3.1, 0.25)];
/// The tail's combs and all-passes (ms; Freeverb's, at 44.1 kHz).
const COMBS_MS: [f32; 4] = [25.3, 26.9, 29.0, 30.7];
const ALLPASS_MS: [f32; 2] = [12.6, 10.0];
/// The longest delay before the reflections (ms), and the longest tail
/// rung out after a sound ends (s).
const MAX_DELAY_MS: f32 = 400.0;
const MAX_TAIL: f32 = 6.0;

/// The mixer's reverb on a playing sound (see `mixer::reverb`): its first
/// channel, at its buses' send, through a few early reflections and a tail
/// (four damped combs into two all-passes), added to each channel; once the
/// sound ends, the tail rung out (unless it's looped). The preset is read
/// every 256 frames; with none on, or no send, it does nothing.
pub(crate) struct Reverb {
    sends: Arc<[u8]>,
    tail: bool,
    channels: usize,
    channel: usize,
    rate: f32,
    countdown: u32,
    on: bool,
    /// Send times the tail's and the reflections' levels; the delays
    /// (frames); each comb's feedback, the damping (a one-pole's
    /// coefficient), the all-passes' gain, what keeps the combs' sum
    /// near the input's level; the decay (s).
    gain: f32,
    early_gain: f32,
    early_delay: usize,
    late_delay: usize,
    feedback: [f32; 4],
    damp: f32,
    diffusion: f32,
    norm: f32,
    decay: f32,
    /// The delay line before the reflections, the combs (line, place, the
    /// damping's state) and the all-passes; made the first time it's on.
    pre: Vec<f32>,
    pre_pos: usize,
    combs: Vec<(Vec<f32>, usize, f32)>,
    allpasses: Vec<(Vec<f32>, usize)>,
    /// This frame's reverb, whether it has rung at all, and the frames of
    /// tail left once the sound ends (and the loudest of the last stretch).
    wet: f32,
    rung: bool,
    left: Option<usize>,
    peak: f32,
    quiet: usize,
}

impl Reverb {
    pub(crate) fn new(sends: Arc<[u8]>, tail: bool, channels: usize, rate: f32) -> Reverb {
        // (Its lines made now, as it's made, for a sound that sends at all:
        // made on the audio thread when it first rings, they glitched it.)
        let len = |ms: f32| ((ms / 1000.0) * rate.max(1.0)) as usize + 1;
        let sending = !sends.is_empty();
        let (pre, combs, allpasses) = if sending {
            (
                vec![0.0; len(MAX_DELAY_MS * 2.0 + 10.0)],
                COMBS_MS.iter().map(|&ms| (vec![0.0; len(ms)], 0, 0.0)).collect(),
                ALLPASS_MS.iter().map(|&ms| (vec![0.0; len(ms)], 0)).collect(),
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
        Reverb {
            sends,
            tail,
            channels,
            channel: 0,
            rate: rate.max(1.0),
            countdown: 0,
            on: false,
            gain: 0.0,
            early_gain: 0.0,
            early_delay: 1,
            late_delay: 1,
            feedback: [0.0; 4],
            damp: 1.0,
            diffusion: 0.5,
            norm: 1.0,
            decay: 1.0,
            pre,
            pre_pos: 0,
            combs,
            allpasses,
            wet: 0.0,
            rung: false,
            left: None,
            peak: 0.0,
            quiet: 0,
        }
    }

    /// Takes the preset and the send as they are now.
    fn refresh(&mut self, preset: Option<[f32; crate::mixer::REVERB_PARAMS]>, send: f32) {
        let Some(p) = preset.filter(|_| send > 1e-4) else {
            self.on = false;
            return;
        };
        self.on = true;
        let db = |v: f32| 10f32.powf(v.clamp(-96.0, 24.0) / 20.0);
        (self.gain, self.early_gain) = (send * db(p[2]), send * db(p[12]));
        let frames = |ms: f32| ((ms.clamp(0.0, MAX_DELAY_MS) / 1000.0) * self.rate) as usize;
        (self.early_delay, self.late_delay) = (frames(p[1]).max(1), frames(p[7]).max(1));
        self.decay = p[4].clamp(0.1, 20.0);
        // (Each comb loses 60 dB over the decay.)
        for (f, ms) in self.feedback.iter_mut().zip(COMBS_MS) {
            *f = 10f32.powf(-3.0 * ms / 1000.0 / self.decay);
        }
        let g = self.feedback.iter().sum::<f32>() / 4.0;
        self.norm = 0.5 * (1.0 - g * g).max(1e-4).sqrt();
        self.damp = 1.0 - (-std::f32::consts::TAU * p[6].clamp(500.0, 20_000.0) / self.rate).exp();
        self.diffusion = (p[9] / 100.0).clamp(0.0, 1.0) * 0.7;
        // (One that sends nowhere has no lines: it stays off.)
        if self.pre.is_empty() {
            self.on = false;
        }
    }

    /// One frame's reverb, its first channel `x` going in.
    fn frame(&mut self, x: f32) -> f32 {
        let n = self.pre.len();
        self.pre[self.pre_pos] = x;
        let pre = &self.pre;
        let tap = |d: usize| pre[(self.pre_pos + n - d.min(n - 1)) % n];
        let early: f32 = EARLY.iter().map(|&(k, g)| tap((self.early_delay as f32 * k) as usize) * g).sum();
        let input = tap(self.early_delay + self.late_delay);
        self.pre_pos = (self.pre_pos + 1) % n;
        let mut late = 0.0;
        for ((line, pos, state), feedback) in self.combs.iter_mut().zip(self.feedback) {
            let y = line[*pos];
            *state += self.damp * (y - *state);
            line[*pos] = input + *state * feedback;
            *pos = (*pos + 1) % line.len();
            late += y;
        }
        late *= self.norm;
        for (line, pos) in &mut self.allpasses {
            let b = line[*pos];
            line[*pos] = late + b * self.diffusion;
            *pos = (*pos + 1) % line.len();
            late = b - late;
        }
        early * self.early_gain + late * self.gain
    }

    /// The reverb to add to the sound's next sample `y`.
    #[inline]
    fn add(&mut self, y: f32) -> f32 {
        if self.channel == 0 {
            if self.countdown == 0 {
                self.countdown = 256;
                self.refresh(crate::mixer::reverb(), crate::mixer::send(&self.sends));
            }
            self.countdown -= 1;
            self.wet = if self.on {
                self.rung = true;
                self.frame(y)
            } else {
                0.0
            };
        }
        self.channel = (self.channel + 1) % self.channels;
        self.wet
    }

    /// The tail's next sample once the sound has ended, until it has rung
    /// out (or isn't due: a loop, a sound it never rang for).
    fn ring(&mut self) -> Option<f32> {
        if !(self.tail && self.rung && self.on) {
            return None;
        }
        if self.channel == 0 {
            let mut left = self.left.unwrap_or(((self.decay.min(MAX_TAIL) * self.rate) as usize) + self.early_delay * 4 + self.late_delay);
            if left == 0 {
                return None;
            }
            left -= 1;
            self.wet = self.frame(0.0);
            // (Gone quiet over a stretch: done.)
            self.peak = self.peak.max(self.wet.abs());
            self.quiet += 1;
            if self.quiet == 2048 {
                if self.peak < 1e-4 {
                    left = 0;
                }
                (self.peak, self.quiet) = (0.0, 0);
            }
            self.left = Some(left);
        }
        self.channel = (self.channel + 1) % self.channels;
        Some(self.wet)
    }

    fn ringing(&self) -> bool {
        self.left.is_some()
    }
}

pub struct SfxDecoder {
    inner: Inner,
    filter: LowPass,
    reverb: Reverb,
}

impl Iterator for SfxDecoder {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        match self.inner.next() {
            Some(x) => {
                let y = self.filter.apply(x);
                Some(y + self.reverb.add(y))
            }
            None => self.reverb.ring(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl Source for SfxDecoder {
    fn current_span_len(&self) -> Option<usize> {
        // (Its tail rings for as long as it takes.)
        if self.reverb.ringing() {
            None
        } else {
            self.inner.current_span_len()
        }
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
        (self.reverb.channel, self.reverb.left) = (0, None);
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
    fn a_state_filters_the_sounds_below_its_units() {
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
        let filter =
            || LowPass { slots: Arc::from([0u8]), state: [[0.0; 2]; 8], high: [0.0; 8], a: 1.0, b: 0.0, countdown: 0, channel: 0, channels: 1, rate: 48000.0 };
        let open = tone(&mut filter());
        let mut mixer = crate::mixer::Mixer::default();
        mixer.apply(&mix, 0x20);
        mixer.advance(&mix, 0.1);
        let muffled = tone(&mut filter());
        // A 5 kHz tone through two 800 Hz poles: well under a tenth.
        assert!(open > 0.6 && muffled < open * 0.1, "{open} {muffled}");

        // A high-pass unit (the same slot, now of kind 2) on at 1 kHz: a
        // 100 Hz hum all but goes.
        let mut mix = crate::mixer::MixData::default();
        mix.read("bus", "00007000 00006000 0 0");
        mix.read("fx", "00007002 00007000 2 150");
        mix.read("mixfx", "00000201 00007002:0:1:0:0 00007002:1:1000:0:0");
        mix.read("mixevent", "00000021 +00000201");
        let hum = |filter: &mut LowPass| -> f32 {
            let samples: Vec<f32> = (0..9600).map(|i| (i as f32 * std::f32::consts::TAU * 100.0 / 48000.0).sin()).map(|x| filter.apply(x)).collect();
            (samples[4800..].iter().map(|s| s * s).sum::<f32>() / 4800.0).sqrt()
        };
        let mut mixer = crate::mixer::Mixer::default();
        mixer.apply(&mix, 0x21);
        mixer.advance(&mix, 0.1);
        let thinned = hum(&mut filter());
        assert!(thinned < 0.2, "{thinned}");
    }

    #[test]
    fn a_reverb_rings_on_for_its_decay() {
        // A cave's preset: reflections 5 ms on at -2 dB, the tail at -2 dB
        // and decaying over 2 s, at a send of 0 dB.
        let mut p = [0.0f32; crate::mixer::REVERB_PARAMS];
        (p[0], p[1], p[2], p[4], p[6], p[7], p[9], p[12]) = (1.0, 5.0, -2.0, 2.0, 9000.0, 20.0, 100.0, -2.0);
        let rate = 48000.0;
        // (Sending through one slot: one that sends nowhere has no lines.)
        let mut r = Reverb::new(Arc::from([0u8]), true, 1, rate);
        r.refresh(Some(p), 1.0);
        // White noise for a second, then nothing.
        let mut seed = 12345u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        };
        let input: Vec<f32> = (0..48000).map(|_| noise() * 0.5).collect();
        let mut wet: Vec<f32> = input.iter().map(|&x| r.frame(x)).collect();
        wet.extend((0..(rate as usize * 3)).map(|_| r.frame(0.0)));
        let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
        let (dry, during) = (rms(&input[24000..]), rms(&wet[24000..48000]));
        // As loud as the dry sound, give or take (the tail at -2 dB, the
        // reflections beside it).
        assert!(during > dry * 0.3 && during < dry * 2.5, "{dry} {during}");
        // A decay of 2 s: 30 dB down about a second after the noise stops,
        // and well under 60 dB down only after the full two.
        let at = |s: f32| rms(&wet[48000 + (s * rate) as usize..][..4800]);
        let (start, one, three) = (at(0.05), at(1.0), at(2.5));
        let db = |a: f32, b: f32| 20.0 * (b / a).log10();
        assert!((-45.0..-18.0).contains(&db(start, one)), "{}", db(start, one));
        assert!(db(start, three) < -50.0, "{}", db(start, three));
    }
}
