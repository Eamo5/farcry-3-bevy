//! Bink audio.
//!
//! Each block codes the transform coefficients of one window per channel: the first two as floats, the rest
//! as small integers in variable-width runs, scaled by a per-band quantiser on a WMA-like critical band
//! layout. The window is either a DCT-III of each channel, or (in older files) an inverse real DFT of the
//! channels already interleaved. Consecutive windows overlap by a sixteenth and are cross-faded linearly.

use anyhow::{ensure, Result};

use crate::bits::BitReader;
use crate::fft::{Dct3, RealInverse};
use crate::{AudioCodec, AudioTrack};

/// Upper edges in Hz of the critical bands that group the coefficients for quantisation (the WMA table).
const CRITICAL_FREQS: [u32; 25] =
    [100, 200, 300, 400, 510, 630, 770, 920, 1080, 1270, 1480, 1720, 2000, 2320, 2700, 3150, 3700, 4400, 5300, 6400, 7700, 9500, 12000, 15500, 24500];

/// Lengths, in groups of eight coefficients, of the long runs that share one bit width.
const RUN_GROUPS: [usize; 16] = [2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16, 32, 64];

enum Transform {
    Dct(Dct3),
    Rdft(RealInverse),
}

/// Incremental decoder for one audio track. Feed it the track's packets in frame order (see
/// [`Bink::audio_packet`](crate::Bink::audio_packet)) and it appends interleaved samples.
pub struct AudioDecoder {
    transform: Transform,
    /// Revision `b` codes the first coefficients as raw floats and uses fixed runs.
    old: bool,
    channels: usize,
    /// Channels coded separately: all of them for the DCT, one interleaved stream for the DFT.
    coded: usize,
    window: usize,
    overlap: usize,
    /// First coefficient of each critical band, then the window length.
    bands: Vec<usize>,
    quant_table: [f32; 96],
    root: f32,
    coeffs: Vec<f32>,
    spec_re: Vec<f32>,
    spec_im: Vec<f32>,
    /// The current window of each coded channel.
    out: Vec<Vec<f32>>,
    /// The tail of each coded channel's previous window, faded into the next one.
    previous: Vec<Vec<f32>>,
    first: bool,
    /// Channels are coded in blocks of up to two; this is the first channel of the next block.
    next_channel: usize,
}

impl AudioDecoder {
    pub fn new(track: &AudioTrack, revision: u8) -> Result<Self> {
        let channels = track.channels as usize;
        let rdft = track.codec == AudioCodec::Rdft;
        ensure!((1..=if rdft { 2 } else { 6 }).contains(&channels), "unsupported audio channel count {channels}");
        ensure!(track.sample_rate > 0, "audio sample rate is zero");
        let old = revision == b'b';
        let mut sample_rate = track.sample_rate;
        let mut bits = if sample_rate < 22050 {
            9
        } else if sample_rate < 44100 {
            10
        } else {
            11
        };
        let coded = if rdft {
            // The DFT variant transforms the channels interleaved, as one stream at a multiple of the rate.
            sample_rate *= channels as u32;
            if !old {
                bits += channels.ilog2();
            }
            1
        } else {
            channels
        };
        let window = 1usize << bits;
        let overlap = window / 16;
        let half_rate = (sample_rate as usize).div_ceil(2);
        let root = if rdft { 2.0 / ((window as f64).sqrt() * 32768.0) } else { window as f64 / ((window as f64).sqrt() * 32768.0) } as f32;
        let quant_table = std::array::from_fn(|i| (i as f32 * 0.152_891_65_f32).exp() * root);
        let band_count = (1..25).find(|&n| half_rate <= CRITICAL_FREQS[n - 1] as usize).unwrap_or(25);
        let mut bands = vec![2];
        bands.extend((1..band_count).map(|i| (CRITICAL_FREQS[i - 1] as usize * window / half_rate) & !1));
        bands.push(window);
        let transform = if rdft { Transform::Rdft(RealInverse::new(window)) } else { Transform::Dct(Dct3::new(window)) };
        Ok(Self {
            transform,
            old,
            channels,
            coded,
            window,
            overlap,
            bands,
            quant_table,
            root,
            coeffs: vec![0.0; window],
            spec_re: vec![0.0; window / 2 + 1],
            spec_im: vec![0.0; window / 2 + 1],
            out: vec![vec![0.0; window]; coded],
            previous: vec![vec![0.0; overlap]; coded],
            first: true,
            next_channel: 0,
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Forgets the previous window, as at the start of the stream.
    pub fn reset(&mut self) {
        self.first = true;
        self.next_channel = 0;
    }

    /// Decodes one packet, appending its samples to `out` interleaved. A packet that turns out damaged is
    /// abandoned; the samples of its intact blocks have already been appended.
    pub fn decode_packet(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<()> {
        ensure!(packet.len() >= 4, "audio packet of {} bytes is too short", packet.len());
        let mut br = BitReader::new(packet);
        // The packet starts with its decoded size in bytes.
        br.skip(32);
        while br.left() > 0 {
            let first = self.next_channel;
            let count = (self.coded - first).min(2);
            if let Err(e) = self.decode_block(&mut br, first, count) {
                self.next_channel = 0;
                return Err(e);
            }
            br.align32();
            self.next_channel += 2;
            if self.next_channel >= self.coded {
                self.next_channel = 0;
                self.emit(out);
            }
        }
        Ok(())
    }

    fn emit(&self, out: &mut Vec<f32>) {
        let length = self.window - self.overlap;
        if self.coded == 1 {
            out.extend_from_slice(&self.out[0][..length]);
        } else {
            out.reserve(length * self.channels);
            for i in 0..length {
                out.extend(self.out.iter().map(|ch| ch[i]));
            }
        }
    }

    /// Decodes the windows of coded channels `first..first + count` and cross-fades them with the
    /// previous ones.
    fn decode_block(&mut self, br: &mut BitReader, first: usize, count: usize) -> Result<()> {
        if matches!(self.transform, Transform::Dct(_)) {
            br.skip(2);
        }
        let band_count = self.bands.len() - 1;
        for ch in first..first + count {
            let coeffs = &mut self.coeffs;
            if self.old {
                ensure!(br.left() >= 64, "audio block truncated");
                coeffs[0] = f32::from_bits(br.read(32)) * self.root;
                coeffs[1] = f32::from_bits(br.read(32)) * self.root;
            } else {
                ensure!(br.left() >= 58, "audio block truncated");
                coeffs[0] = read_float(br) * self.root;
                coeffs[1] = read_float(br) * self.root;
            }
            ensure!(br.left() >= band_count as isize * 8, "audio block truncated");
            let mut quant = [0.0f32; 25];
            for q in &mut quant[..band_count] {
                *q = self.quant_table[(br.read(8) as usize).min(95)];
            }

            let (mut band, mut q) = (0, quant[0]);
            let mut i = 2;
            while i < self.window {
                let end = if self.old {
                    i + 16
                } else if br.bit() {
                    i + RUN_GROUPS[br.read(4) as usize] * 8
                } else {
                    i + 8
                };
                let end = end.min(self.window);
                let width = br.read(4);
                if width == 0 {
                    coeffs[i..end].fill(0.0);
                    i = end;
                    while self.bands[band] < i {
                        q = quant[band];
                        band += 1;
                    }
                } else {
                    while i < end {
                        if self.bands[band] == i {
                            q = quant[band];
                            band += 1;
                        }
                        let v = br.read(width);
                        coeffs[i] = match v {
                            0 => 0.0,
                            _ if br.bit() => -q * v as f32,
                            _ => q * v as f32,
                        };
                        i += 1;
                    }
                }
            }

            let out = &mut self.out[ch];
            match &mut self.transform {
                Transform::Dct(dct) => {
                    coeffs[0] *= 2.0;
                    dct.run(coeffs, out, 2.0 / self.window as f32);
                }
                Transform::Rdft(rdft) => {
                    let half = self.window / 2;
                    self.spec_re[0] = coeffs[0];
                    self.spec_im[0] = 0.0;
                    for k in 1..half {
                        self.spec_re[k] = coeffs[2 * k];
                        self.spec_im[k] = -coeffs[2 * k + 1];
                    }
                    self.spec_re[half] = coeffs[1];
                    self.spec_im[half] = 0.0;
                    rdft.run(&self.spec_re, &self.spec_im, out);
                    for v in out.iter_mut() {
                        *v *= 0.5;
                    }
                }
            }
        }

        let fade = self.overlap * count;
        for (n, ch) in (first..first + count).enumerate() {
            let (out, previous) = (&mut self.out[ch], &mut self.previous[ch]);
            if !self.first {
                for (i, (v, &p)) in out[..self.overlap].iter_mut().zip(previous.iter()).enumerate() {
                    let j = n + i * count;
                    *v = (p * (fade - j) as f32 + *v * j as f32) / fade as f32;
                }
            }
            previous.copy_from_slice(&out[self.window - self.overlap..]);
        }
        self.first = false;
        Ok(())
    }
}

/// A float coded as a 5-bit exponent, a 23-bit mantissa and a sign bit.
fn read_float(br: &mut BitReader) -> f32 {
    let exponent = br.read(5) as i32 - 23;
    let mantissa = br.read(23) as f32;
    let v = mantissa * f32::from_bits(((127 + exponent) as u32) << 23);
    if br.bit() {
        -v
    } else {
        v
    }
}
