//! Decoder for RAD Game Tools' Bink 1 video files (signature `BIK`, revisions `b` to `k`), written in
//! plain Rust: video to planar YUV 4:2:0 plus alpha when the file has it, and both Bink audio variants
//! (DCT and DFT) to interleaved `f32` samples.
//!
//! The container and both codecs are implemented from their public descriptions (the MultimediaWiki
//! "Bink Container", "Bink Video" and "Bink Audio" pages) and checked against FFmpeg's decoder, whose
//! video output this crate reproduces exactly. The constant tables in `tables.rs` (coefficient scan
//! order, the sixteen Huffman code sets, run patterns and dequantisation matrices) are the format's own,
//! taken from FFmpeg's `binkdata.h`.
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! let data: std::sync::Arc<[u8]> = std::fs::read("intro.bik")?.into();
//! let mut bink = fc3_bink::Bink::new(data)?;
//! for i in 0..bink.frame_count() {
//!     let rgba = bink.decode_frame(i)?.rgba();
//! }
//! let samples = bink.decode_audio(0)?;
//! # Ok(())
//! # }
//! ```

mod audio;
mod bits;
mod fft;
#[rustfmt::skip]
mod tables;
mod video;

use std::sync::Arc;

use anyhow::{ensure, Context, Result};

pub use audio::AudioDecoder;
pub use video::{Frame, Plane, BLOCK_KIND_NAMES};

const FLAG_ALPHA: u32 = 0x0010_0000;
const FLAG_GREY: u32 = 0x0002_0000;
const AUDIO_STEREO: u16 = 0x2000;
const AUDIO_DCT: u16 = 0x1000;

/// The transform an audio track is coded with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    /// A DCT-III per channel (newer files).
    Dct,
    /// An inverse real DFT over the interleaved channels (older files).
    Rdft,
}

#[derive(Clone, Debug)]
pub struct AudioTrack {
    /// The id the file gives the track (often a language index).
    pub id: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub codec: AudioCodec,
}

#[derive(Clone, Copy)]
struct FrameEntry {
    start: usize,
    end: usize,
    keyframe: bool,
}

/// An open Bink file. Video decodes in order; asking for an earlier frame than the last one decoded
/// starts over from the first frame, so looping is just asking for frame 0 again.
pub struct Bink {
    data: Arc<[u8]>,
    revision: u8,
    width: u32,
    height: u32,
    frame_rate: (u32, u32),
    has_alpha: bool,
    tracks: Vec<AudioTrack>,
    frames: Vec<FrameEntry>,
    video: video::VideoDecoder,
    /// The frame the video decoder will decode next.
    next: u32,
    /// The frame the video decoder holds, if the last decode succeeded.
    current: Option<u32>,
}

impl Bink {
    /// Parses the header and frame index of a Bink file held in memory.
    pub fn new(data: Arc<[u8]>) -> Result<Self> {
        let base = header_offset(&data);
        let mut r = Reader { data: &data, pos: base };
        let tag: [u8; 4] = r.bytes()?;
        ensure!(&tag[..3] == b"BIK", "not a Bink 1 file");
        let revision = tag[3];
        ensure!((b'b'..=b'k').contains(&revision), "unsupported Bink revision '{}'", revision.escape_ascii());
        let file_size = r.u32()? as usize + 8;
        let frame_count = r.u32()?;
        ensure!(frame_count <= 1_000_000, "implausible frame count {frame_count}");
        r.skip(8);
        let (width, height) = (r.u32()?, r.u32()?);
        ensure!((1..=7680).contains(&width) && (1..=4800).contains(&height), "implausible frame size {width}x{height}");
        let frame_rate = (r.u32()?, r.u32()?);
        ensure!(frame_rate.0 != 0 && frame_rate.1 != 0, "invalid frame rate {}/{}", frame_rate.0, frame_rate.1);
        let flags = r.u32()?;
        let track_count = r.u32()? as usize;
        ensure!(track_count <= 256, "implausible audio track count {track_count}");
        if revision == b'k' {
            r.skip(4);
        }
        // Each track's largest decoded packet size, unused here.
        r.skip(4 * track_count);
        let mut tracks = Vec::with_capacity(track_count);
        for _ in 0..track_count {
            let sample_rate = u32::from(r.u16()?);
            let track_flags = r.u16()?;
            let channels = if track_flags & AUDIO_STEREO != 0 { 2 } else { 1 };
            let codec = if track_flags & AUDIO_DCT != 0 { AudioCodec::Dct } else { AudioCodec::Rdft };
            tracks.push(AudioTrack { id: 0, sample_rate, channels, codec });
        }
        for track in &mut tracks {
            track.id = r.u32()?;
        }

        // Frame offsets, the lowest bit marking keyframes; the last frame runs to the end of the file.
        let mut frames = Vec::with_capacity(frame_count as usize);
        let mut next = if frame_count > 0 { r.u32()? } else { 0 };
        for i in 0..frame_count {
            let pos = next;
            next = if i + 1 == frame_count { file_size as u32 } else { r.u32()? };
            let (start, end) = ((pos & !1) as usize, (next & !1) as usize);
            ensure!(end > start, "frame index entry {i} is not increasing");
            frames.push(FrameEntry { start: base + start, end: base + end, keyframe: i == 0 || pos & 1 != 0 });
        }

        let has_alpha = flags & FLAG_ALPHA != 0;
        let video = video::VideoDecoder::new(revision, width, height, has_alpha, flags & FLAG_GREY != 0);
        Ok(Self { data, revision, width, height, frame_rate, has_alpha, tracks, frames, video, next: 0, current: None })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Frames per second as a fraction, numerator first.
    pub fn frame_rate(&self) -> (u32, u32) {
        self.frame_rate
    }

    pub fn frame_count(&self) -> u32 {
        self.frames.len() as u32
    }

    /// The revision letter of the format, `b` to `k`.
    pub fn revision(&self) -> char {
        self.revision as char
    }

    pub fn has_alpha(&self) -> bool {
        self.has_alpha
    }

    /// Whether the frame index marks frame `index` as a keyframe.
    pub fn is_keyframe(&self, index: u32) -> bool {
        self.frames.get(index as usize).is_some_and(|f| f.keyframe)
    }

    pub fn audio_tracks(&self) -> &[AudioTrack] {
        &self.tracks
    }

    /// Decodes frame `index`. Frames after the last one decoded are reached by decoding the ones in
    /// between; earlier frames by starting over from frame 0. Asking for the current frame again is free.
    /// After an error the frame before it stays available through [`frame`](Self::frame) and decoding
    /// can carry on with the next frame.
    pub fn decode_frame(&mut self, index: u32) -> Result<&Frame> {
        ensure!(index < self.frame_count(), "frame {index} is past the last frame ({} frames)", self.frame_count());
        if self.current == Some(index) {
            return Ok(self.video.frame());
        }
        if index < self.next {
            self.next = 0;
            self.video.restart();
        }
        while self.next <= index {
            let i = self.next;
            self.next += 1;
            self.current = None;
            let (_, packet) = split_frame(&self.data, self.frames[i as usize], self.tracks.len(), None).with_context(|| format!("frame {i}"))?;
            self.video.decode(packet).with_context(|| format!("frame {i}"))?;
        }
        self.current = Some(index);
        Ok(self.video.frame())
    }

    /// Decodes the frame after the last one decoded, going back to frame 0 after the last frame.
    pub fn next_frame(&mut self) -> Result<&Frame> {
        let index = if self.next >= self.frame_count() { 0 } else { self.next };
        self.decode_frame(index)
    }

    /// The most recently decoded frame (all zero before the first).
    pub fn frame(&self) -> &Frame {
        self.video.frame()
    }

    /// How many blocks of each kind the video decoder has decoded, indexed like [`BLOCK_KIND_NAMES`], then
    /// at `10 + kind` how many scaled blocks were of each kind. Revision `b` is not counted.
    pub fn block_counts(&self) -> &[u64; 20] {
        &self.video.counts
    }

    /// The packet track `track` has in frame `index`, if any.
    pub fn audio_packet(&self, index: u32, track: usize) -> Result<Option<&[u8]>> {
        ensure!(track < self.tracks.len(), "no audio track {track}");
        let entry = *self.frames.get(index as usize).with_context(|| format!("no frame {index}"))?;
        Ok(split_frame(&self.data, entry, self.tracks.len(), Some(track)).with_context(|| format!("frame {index}"))?.0)
    }

    /// A decoder for track `track`, to be fed [`audio_packet`](Self::audio_packet)s in frame order.
    pub fn audio_decoder(&self, track: usize) -> Result<AudioDecoder> {
        let info = self.tracks.get(track).with_context(|| format!("no audio track {track}"))?;
        AudioDecoder::new(info, self.revision)
    }

    /// Decodes a whole audio track to interleaved samples at the track's rate. Damaged packets are
    /// skipped, as the reference decoder does.
    pub fn decode_audio(&self, track: usize) -> Result<Vec<f32>> {
        let mut decoder = self.audio_decoder(track)?;
        let mut out = Vec::new();
        for i in 0..self.frame_count() {
            if let Ok(Some(packet)) = self.audio_packet(i, track) {
                let _ = decoder.decode_packet(packet, &mut out);
            }
        }
        Ok(out)
    }
}

/// Splits a frame into the packet of audio track `wanted` (when present) and the video packet that
/// follows the audio. Frames running past the end of a truncated file are cut short.
fn split_frame(data: &[u8], entry: FrameEntry, tracks: usize, wanted: Option<usize>) -> Result<(Option<&[u8]>, &[u8])> {
    ensure!(entry.start < data.len(), "frame data lies past the end of the file");
    let mut rest = &data[entry.start..entry.end.min(data.len())];
    let mut found = None;
    for track in 0..tracks {
        ensure!(rest.len() >= 4, "frame truncated in its audio packets");
        let size = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        ensure!(size <= rest.len() - 4, "audio packet of {size} bytes overruns its frame");
        // Packets under four bytes carry no audio.
        if Some(track) == wanted && size >= 4 {
            found = Some(&rest[4..4 + size]);
        }
        rest = &rest[4 + size..];
    }
    Ok((found, rest))
}

/// Some files wrap the Bink stream in 512-byte SMUSH blocks; the Bink header starts after them.
fn header_offset(data: &[u8]) -> usize {
    if data.starts_with(b"SMUS") {
        let mut off = 512;
        while off + 4 <= data.len() {
            if &data[off..off + 3] == b"BIK" {
                return off;
            }
            off += 512;
        }
    }
    0
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let bytes = self.data.get(self.pos..self.pos + N).context("Bink header is truncated")?;
        self.pos += N;
        Ok(bytes.try_into().unwrap())
    }

    fn u32(&mut self) -> Result<u32> {
        self.bytes().map(u32::from_le_bytes)
    }

    fn u16(&mut self) -> Result<u16> {
        self.bytes().map(u16::from_le_bytes)
    }

    fn skip(&mut self, n: usize) {
        self.pos += n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A revision `k` file whose frames fill every plane with one value each (the plane fill flag only
    /// that revision has), with an alpha plane.
    fn filled_file(frames: &[[u8; 4]]) -> Vec<u8> {
        let header_len = 48 + 4 * frames.len();
        // Each frame: 32 skipped bits, alpha, 32 skipped bits, then Y, V and U, each plane a set fill bit
        // and the 8-bit value, padded to 32 bits.
        let frame_len = 4 * 6;
        let mut file = Vec::new();
        file.extend_from_slice(b"BIKk");
        file.extend_from_slice(&((header_len + frame_len * frames.len() - 8) as u32).to_le_bytes());
        for v in [frames.len() as u32, frame_len as u32, frames.len() as u32, 20, 12, 30, 1, FLAG_ALPHA, 0, 0] {
            file.extend_from_slice(&v.to_le_bytes());
        }
        for i in 0..frames.len() {
            let pos = (header_len + i * frame_len) as u32 | (i == 0) as u32;
            file.extend_from_slice(&pos.to_le_bytes());
        }
        for &[a, y, u, v] in frames {
            for word in [0, 1 | u32::from(a) << 1, 0, 1 | u32::from(y) << 1, 1 | u32::from(v) << 1, 1 | u32::from(u) << 1] {
                file.extend_from_slice(&word.to_le_bytes());
            }
        }
        file
    }

    fn assert_filled(frame: &Frame, [a, y, u, v]: [u8; 4]) {
        for (plane, value) in [(frame.alpha().unwrap(), a), (frame.y(), y), (frame.u(), u), (frame.v(), v)] {
            for row in 0..plane.height() {
                assert!(plane.row(row).iter().all(|&s| s == value), "row {row} is not {value}");
            }
        }
    }

    #[test]
    fn decodes_and_loops_revision_k_fills() {
        let values = [[255, 16, 128, 128], [7, 235, 100, 200], [128, 81, 90, 240]];
        let mut bink = Bink::new(filled_file(&values).into()).unwrap();
        assert_eq!((bink.width(), bink.height(), bink.frame_count(), bink.revision()), (20, 12, 3, 'k'));
        assert!(bink.has_alpha() && bink.is_keyframe(0) && !bink.is_keyframe(1));
        for round in 0..2 {
            for (i, &v) in values.iter().enumerate() {
                let frame = if round == 0 { bink.decode_frame(i as u32).unwrap() } else { bink.next_frame().unwrap() };
                assert_filled(frame, v);
            }
        }
        // Going back starts over from the first frame.
        assert_filled(bink.decode_frame(1).unwrap(), values[1]);

        let frame = bink.decode_frame(0).unwrap();
        assert!(frame.full_range());
        let rgba = frame.rgba();
        assert_eq!(rgba.len(), 20 * 12 * 4);
        assert_eq!(&rgba[..4], &[16, 16, 16, 255]);
    }

    #[test]
    fn rejects_other_formats() {
        assert!(Bink::new(b"KB2a\0\0\0\0".to_vec().into()).is_err());
        assert!(Bink::new(b"BIK".to_vec().into()).is_err());
    }
}
