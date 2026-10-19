//! Far Cry 3 audio: Ubisoft "DARE" sound objects (BAOs) packed in Dunia `.spk`
//! packages (`soundbinary\%08x.spk`) and streamed `.sbao` files
//! (`soundbinary\%08x.sbao`, all in `sound.fat`).
//!
//! # Containers
//!
//! * **SPK** (`"\x04KPS"`, little endian): `u32 count`, `count` BAO ids, then
//!   per BAO a `u32 size` followed by the BAO bytes padded to 4. The package is
//!   named after its root *event* id (`sndres0x001837ff` -> `001837ff.spk`).
//! * **BAO** (version `02 28 03 05` = package format 2, DARE v0x280305): a
//!   0x28-byte base header (`u32 version`, `u32 0x28`, 16-byte GUID, 8 zero
//!   bytes, `u32 class`, `u32 2`) and a class specific payload:
//!   * `0x1` event - what the game triggers (points at a header BAO),
//!   * `0x2` header - audio description (single sound, random, sequence, layers ...),
//!   * `0x3` memory audio - encoded audio data stored inside the `.spk`,
//!   * `0x4` project - not needed for playback,
//!   * `0x5` stream audio - the payload of a `.sbao` file,
//!   * `0x7` distance curve - volume by distance from the listener,
//!   * `0x9` positioning (in `2fffffff.spk`) - whether a sound is placed in
//!     the world (3D) and which distance curve it fades by; each playable
//!     header names one.
//!
//! # Codecs (stream type of a header BAO)
//!
//! `1` PCM s16le, `2` Ubisoft IMA ADPCM (v6 header), `3` Ubisoft IMA ADPCM
//! "seekable", `4` Ogg Vorbis. Multi-layer sounds (header type 6) and some
//! streams wrap their audio in a blocked multi-track container (version
//! `0x00100009`) which is de-interleaved into one stream per layer here.
//!
//! The layout was worked out from the game data together with vgmstream's
//! public `ubi_bao` / `ubi_sb_streamfile` / IMA documentation.

use std::collections::HashMap;

use crate::archive::Vfs;
use crate::hash::path_hash;

use anyhow::{bail, Context, Result};

/// Size of the BAO base header; class payloads start here.
pub const BAO_HEADER_SIZE: usize = 0x28;

pub const CLASS_EVENT: u8 = 0x1;
pub const CLASS_HEADER: u8 = 0x2;
pub const CLASS_MEMORY: u8 = 0x3;
pub const CLASS_PROJECT: u8 = 0x4;
pub const CLASS_STREAM: u8 = 0x5;
pub const CLASS_FALLOFF: u8 = 0x7;
pub const CLASS_POSITIONING: u8 = 0x9;

// ---------------------------------------------------------------------------
// little helpers

fn u32_at(b: &[u8], o: usize) -> Result<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .with_context(|| format!("read u32 at {o:#x} past end ({:#x})", b.len()))
}

fn f32_at(b: &[u8], o: usize) -> Result<f32> {
    u32_at(b, o).map(f32::from_bits)
}

fn i16_at(b: &[u8], o: usize) -> Result<i16> {
    b.get(o..o + 2)
        .map(|s| i16::from_le_bytes(s.try_into().unwrap()))
        .with_context(|| format!("read i16 at {o:#x} past end ({:#x})", b.len()))
}

/// Path of the package holding sound/event `id`.
pub fn spk_path(id: u32) -> String {
    format!("soundbinary\\{id:08x}.spk")
}

/// Path of the streamed audio file for resource `id`.
pub fn sbao_path(id: u32) -> String {
    format!("soundbinary\\{id:08x}.sbao")
}

/// Parses `soundbinary\xxxxxxxx.spk` / `.sbao` style names back to the id.
pub fn id_from_path(path: &str) -> Option<u32> {
    let file = path.rsplit(['\\', '/']).next()?;
    let stem = file.split('.').next()?;
    let stem = stem.strip_prefix("sndres0x").unwrap_or(stem);
    if stem.len() != 8 {
        return None;
    }
    u32::from_str_radix(stem, 16).ok()
}

// ---------------------------------------------------------------------------
// containers

/// One BAO (binary audio object).
#[derive(Clone, Copy, Debug)]
pub struct Bao<'a> {
    /// Id from the package index (header/event BAOs also carry it in their payload).
    pub id: u32,
    /// Byte 0: 1 = atomic, 2 = package.
    pub format: u8,
    /// Bytes 1..4 big endian, e.g. `0x280305`.
    pub version: u32,
    /// Top nibble of the class field (see `CLASS_*`).
    pub class: u8,
    /// Whole BAO including the base header.
    pub data: &'a [u8],
}

impl<'a> Bao<'a> {
    pub fn parse(id: u32, data: &'a [u8]) -> Result<Bao<'a>> {
        if data.len() < BAO_HEADER_SIZE {
            bail!("BAO {id:08x} too small ({} bytes)", data.len());
        }
        let format = data[0];
        let version = u32::from_be_bytes([0, data[1], data[2], data[3]]);
        if !(1..=2).contains(&format) || !(0x1B_0000..0x2C_0000).contains(&version) {
            bail!("BAO {id:08x}: not a BAO (format {format}, version {version:06x})");
        }
        if version >> 16 != 0x28 {
            bail!("BAO {id:08x}: unsupported DARE version {version:06x} (expected 28xxxx)");
        }
        let class_raw = u32_at(data, 0x20)?;
        if class_raw & 0x0FFF_FFFF != 0 {
            bail!("BAO {id:08x}: bad class field {class_raw:08x}");
        }
        Ok(Bao { id, format, version, class: (class_raw >> 28) as u8, data })
    }

    /// Class specific payload (after the 0x28-byte base header).
    pub fn payload(&self) -> &'a [u8] {
        &self.data[BAO_HEADER_SIZE..]
    }

    pub fn guid(&self) -> [u8; 16] {
        self.data[8..24].try_into().unwrap()
    }
}

/// A `.spk` package: a list of BAOs indexed by id.
#[derive(Clone, Debug)]
pub struct Spk<'a> {
    pub version: u8,
    pub baos: Vec<Bao<'a>>,
}

impl<'a> Spk<'a> {
    pub fn is_spk(data: &[u8]) -> bool {
        data.len() >= 8 && &data[1..4] == b"KPS" && matches!(data[0], 1 | 2 | 4)
    }

    pub fn parse(data: &'a [u8]) -> Result<Spk<'a>> {
        if !Self::is_spk(data) {
            bail!("not an SPK package");
        }
        let version = data[0];
        let count = u32_at(data, 4)? as usize;
        if count > 100_000 || 8 + count * 4 > data.len() {
            bail!("SPK: bad entry count {count}");
        }
        let ids: Vec<u32> = (0..count).map(|i| u32_at(data, 8 + i * 4)).collect::<Result<_>>()?;
        let mut pos = 8 + count * 4;
        let mut baos = Vec::with_capacity(count);
        for id in ids {
            if version == 1 {
                // v1 (Avatar/FC2) lists related ids before each BAO.
                let related = u32_at(data, pos)? as usize;
                pos += 4 + related * 4;
            }
            let size = u32_at(data, pos)? as usize;
            pos += 4;
            let end = pos + size;
            if end > data.len() {
                bail!("SPK: BAO {id:08x} overruns package ({end:#x} > {:#x})", data.len());
            }
            baos.push(Bao::parse(id, &data[pos..end])?);
            pos = (end + 3) & !3;
        }
        Ok(Spk { version, baos })
    }

    pub fn get(&self, id: u32) -> Option<&Bao<'a>> {
        self.baos.iter().find(|b| b.id == id)
    }
}

/// Splits a file into its BAOs: `.spk` packages yield many, a bare `.bao`/`.sbao` one.
pub fn parse_container(data: &[u8], id_hint: u32) -> Result<Vec<Bao<'_>>> {
    if Spk::is_spk(data) {
        Ok(Spk::parse(data)?.baos)
    } else {
        Ok(vec![Bao::parse(id_hint, data)?])
    }
}

// ---------------------------------------------------------------------------
// header / event descriptions

/// Audio codec (header "stream type" for DARE v0x28).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    Pcm16,
    UbiIma,
    UbiImaSeek,
    Ogg,
    Xma2,
    Psx,
    At3,
    Unknown(u32),
}

impl Codec {
    pub fn from_type(t: u32) -> Codec {
        match t {
            1 => Codec::Pcm16,
            2 => Codec::UbiIma,
            3 => Codec::UbiImaSeek,
            4 => Codec::Ogg,
            5 => Codec::Xma2,
            6 => Codec::Psx,
            7 => Codec::At3,
            x => Codec::Unknown(x),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Codec::Pcm16 => "pcm16",
            Codec::UbiIma => "ubi-ima",
            Codec::UbiImaSeek => "ubi-ima-seek",
            Codec::Ogg => "ogg",
            Codec::Xma2 => "xma2",
            Codec::Psx => "psx-adpcm",
            Codec::At3 => "atrac3",
            Codec::Unknown(_) => "unknown",
        }
    }
}

/// Where the encoded bytes of a header live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataSource {
    /// Memory BAO id (class 3, same `.spk`) or stream id (`.sbao`).
    pub resource_id: u32,
    /// True when the data is (at least partly) in `soundbinary\{resource_id}.sbao`.
    pub is_stream: bool,
    /// Bytes of the stream preloaded in a memory BAO with the same id (0 = none).
    pub prefetch_size: u32,
    /// Total encoded size (prefetch + stream, or memory size).
    pub size: u32,
    /// Data is stored right after the header table inside the header BAO itself.
    pub inline: bool,
}

/// Header type 1: a single sound.
#[derive(Clone, Debug)]
pub struct AudioHeader {
    pub id: u32,
    pub codec: Codec,
    pub channels: u32,
    pub sample_rate: u32,
    /// Samples per channel (loop start + loop body for looping sounds).
    pub num_samples: u32,
    pub looping: bool,
    pub loop_start: u32,
    pub source: DataSource,
}

#[derive(Clone, Debug)]
pub struct LayerInfo {
    pub codec: Codec,
    pub channels: u32,
    pub sample_rate: u32,
    pub num_samples: u32,
}

/// Header type 6: several tracks stored interleaved, played together.
#[derive(Clone, Debug)]
pub struct LayerHeader {
    pub id: u32,
    pub looping: bool,
    pub layers: Vec<LayerInfo>,
    pub source: DataSource,
}

/// Header payload, by header type.
#[derive(Clone, Debug)]
pub enum Header {
    /// Type 1.
    Audio(AudioHeader),
    /// Type 4: pick one child at random; `(child id, weight)`.
    Random { id: u32, children: Vec<(u32, f32)> },
    /// Type 5: children played back to back.
    Sequence { id: u32, children: Vec<u32>, looping: bool },
    /// Type 6.
    Layer(LayerHeader),
    /// Type 8.
    Silence { id: u32, duration: f32 },
    /// Type 3: plays the child whose switch value (e.g. surface material) is
    /// currently set for switch `group`; `(child id, value id)`.
    Switch { id: u32, group: u32, cases: Vec<(u32, u32)> },
    /// Types 2 (chain), 7 (parameter blend) and unknown. `refs` are the child ids.
    Other { id: u32, kind: u32, refs: Vec<u32> },
}

impl Header {
    pub fn id(&self) -> u32 {
        match self {
            Header::Audio(a) => a.id,
            Header::Layer(l) => l.id,
            Header::Random { id, .. }
            | Header::Sequence { id, .. }
            | Header::Silence { id, .. }
            | Header::Switch { id, .. }
            | Header::Other { id, .. } => *id,
        }
    }

    pub fn kind(&self) -> u32 {
        match self {
            Header::Audio(_) => 1,
            Header::Random { .. } => 4,
            Header::Sequence { .. } => 5,
            Header::Layer(_) => 6,
            Header::Silence { .. } => 8,
            Header::Switch { .. } => 3,
            Header::Other { kind, .. } => *kind,
        }
    }

    /// Ids of other header BAOs this one plays.
    pub fn children(&self) -> Vec<u32> {
        match self {
            Header::Random { children, .. } => children.iter().map(|c| c.0).collect(),
            Header::Sequence { children, .. } => children.clone(),
            Header::Switch { cases, .. } => cases.iter().map(|c| c.0).collect(),
            Header::Other { refs, .. } => refs.clone(),
            _ => Vec::new(),
        }
    }
}

// Offsets inside the header payload (relative to BAO+0x28) for DARE v0x2803xx.
const H_BASE: usize = 0xBC - BAO_HEADER_SIZE; // base header payload size
const H_ENTRY_RANDOM: usize = 0x10;
const H_ENTRY_SEQUENCE: usize = 0x14;
const H_ENTRY_LAYER: usize = 0x2C;

/// Parses a class 2 (header) BAO.
pub fn parse_header(bao: &Bao) -> Result<Header> {
    if bao.class != CLASS_HEADER {
        bail!("BAO {:08x} is class {} not a header", bao.id, bao.class);
    }
    let p = bao.payload();
    let id = u32_at(p, 0x00)?;
    let kind = u32_at(p, 0x04)?;
    Ok(match kind {
        1 => {
            let looping = u32_at(p, 0x48)? & 1 != 0;
            let n1 = u32_at(p, 0x64)?;
            let n2 = u32_at(p, 0x6C)?;
            let (num_samples, loop_start) = if looping { (n1 + n2, n1) } else { (n1, 0) };
            let prefetch_size = u32_at(p, 0x80)?;
            Header::Audio(AudioHeader {
                id,
                codec: Codec::from_type(u32_at(p, 0x74)?),
                channels: u32_at(p, 0x54)?,
                sample_rate: u32_at(p, 0x5C)?,
                num_samples,
                looping,
                loop_start,
                source: DataSource {
                    resource_id: u32_at(p, 0x38)?,
                    is_stream: u32_at(p, 0x3C)? & 1 != 0,
                    prefetch_size,
                    size: u32_at(p, 0x08)?,
                    inline: false,
                },
            })
        }
        4 => {
            let count = u32_at(p, 0x38)? as usize;
            if count > 4096 {
                bail!("random {id:08x}: bad count {count}");
            }
            let children = (0..count)
                .map(|i| {
                    let o = H_BASE + i * H_ENTRY_RANDOM;
                    Ok((u32_at(p, o)?, f32_at(p, o + 4)?))
                })
                .collect::<Result<_>>()?;
            Header::Random { id, children }
        }
        5 => {
            let count = u32_at(p, 0x48)? as usize;
            if count > 4096 {
                bail!("sequence {id:08x}: bad count {count}");
            }
            let children =
                (0..count).map(|i| u32_at(p, H_BASE + i * H_ENTRY_SEQUENCE)).collect::<Result<_>>()?;
            Header::Sequence { id, children, looping: u32_at(p, 0x3C)? == 0 }
        }
        6 => {
            let count = u32_at(p, 0x3C)? as usize;
            if count == 0 || count > 64 {
                bail!("layer {id:08x}: bad layer count {count}");
            }
            let cues = u32_at(p, 0x68)? as usize * 8 + u32_at(p, 0x6C)? as usize;
            let table = H_BASE + cues;
            let layers = (0..count)
                .map(|i| {
                    let o = table + i * H_ENTRY_LAYER;
                    Ok(LayerInfo {
                        sample_rate: u32_at(p, o)?,
                        channels: u32_at(p, o + 4)?,
                        codec: Codec::from_type(u32_at(p, o + 8)?),
                        num_samples: u32_at(p, o + 0x1C)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let is_stream = u32_at(p, 0x44)? & 1 != 0;
            let extra = u32_at(p, 0x60)? as usize;
            let prefetch_size = u32_at(p, 0x64)?;
            // memory data (or the prefetched head of a stream) follows the layer table
            let inline = bao.payload().len() > H_BASE + extra;
            Header::Layer(LayerHeader {
                id,
                looping: u32_at(p, 0x40)? & 1 != 0,
                layers,
                source: DataSource {
                    // Layered data (inline, prefetch or `.sbao`) is keyed by the header id itself.
                    resource_id: id,
                    is_stream,
                    prefetch_size,
                    size: u32_at(p, 0x58)?,
                    inline,
                },
            })
        }
        8 => Header::Silence { id, duration: f32_at(p, 0x38)? },
        2 => {
            // chain/"play list with config": u32 count at +0x48, ids after the base header
            let count = (u32_at(p, 0x48)? as usize).min(1024);
            let refs = (0..count).map(|i| u32_at(p, H_BASE + i * 4)).collect::<Result<_>>()?;
            Header::Other { id, kind, refs }
        }
        3 => {
            // switch: count at +0x38, switch group at +0x3C, entries (child id, switch value id)
            let count = (u32_at(p, 0x38)? as usize).min(1024);
            Header::Switch { id, group: u32_at(p, 0x3C)?, cases: pairs(p, H_BASE, count, 8)? }
        }
        7 => {
            // parameter-driven blend: count at +0x38, 0x1C-byte entries starting with the child id
            let count = (u32_at(p, 0x38)? as usize).min(1024);
            let refs = (0..count).map(|i| u32_at(p, H_BASE + i * 0x1C)).collect::<Result<_>>()?;
            Header::Other { id, kind, refs }
        }
        _ => Header::Other { id, kind, refs: Vec::new() },
    })
}

/// Reads `count` `(u32 child, u32 key)` pairs with the given stride.
fn pairs(p: &[u8], start: usize, count: usize, stride: usize) -> Result<Vec<(u32, u32)>> {
    (0..count)
        .map(|i| Ok((u32_at(p, start + i * stride)?, u32_at(p, start + i * stride + 4)?)))
        .collect()
}

/// How a blend child responds to a game parameter (engine RPM, speed...):
/// `kind` 0 is volume (dB), 1 pitch (semitones); `points` are `(parameter,
/// value)` pairs, joined by straight lines and held flat past the ends.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    pub kind: u32,
    pub param: u32,
    pub points: Vec<(f32, f32)>,
}

impl Curve {
    pub const VOLUME: u32 = 0;
    pub const PITCH: u32 = 1;

    pub fn at(&self, x: f32) -> f32 {
        let Some(&(x0, y0)) = self.points.first() else { return 0.0 };
        if x <= x0 {
            return y0;
        }
        for w in self.points.windows(2) {
            let ((xa, ya), (xb, yb)) = (w[0], w[1]);
            if x <= xb {
                return if xb > xa { ya + (yb - ya) * (x - xa) / (xb - xa) } else { yb };
            }
        }
        self.points.last().map_or(0.0, |p| p.1)
    }

    /// `kind:param:x,y;x,y...` as the manifest writes it.
    pub fn to_token(&self) -> String {
        let pts: Vec<String> = self.points.iter().map(|(x, y)| format!("{x},{y}")).collect();
        format!("{}:{:08x}:{}", self.kind, self.param, pts.join(";"))
    }

    pub fn from_token(t: &str) -> Option<Curve> {
        let mut it = t.splitn(3, ':');
        let kind = it.next()?.parse().ok()?;
        let param = u32::from_str_radix(it.next()?, 16).ok()?;
        let points = it
            .next()?
            .split(';')
            .map(|p| {
                let (x, y) = p.split_once(',')?;
                Some((x.parse().ok()?, y.parse().ok()?))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Curve { kind, param, points })
    }
}

/// A type 7 (parameter blend) header's children and their curves. From the
/// child table at the end of the base header: `count` (+0x38) 0x1C-byte
/// entries `(child id, curve count, curve table offset, ...)`, then 0x18-byte
/// curve entries `(kind, parameter id, point count, points offset, ..)` and
/// 0x10-byte points `(f32 x, f32 y, u32 shape, u32)`; offsets count from the
/// child table.
pub fn parse_blend(bao: &Bao) -> Result<Vec<(u32, Vec<Curve>)>> {
    let p = bao.payload();
    if u32_at(p, 4)? != 7 {
        bail!("not a blend header");
    }
    let count = (u32_at(p, 0x38)? as usize).min(64);
    (0..count)
        .map(|i| {
            let e = H_BASE + i * 0x1C;
            let (child, curves, table) = (u32_at(p, e)?, (u32_at(p, e + 4)? as usize).min(16), u32_at(p, e + 8)? as usize);
            let curves = (0..curves)
                .map(|k| {
                    let c = H_BASE + table + k * 0x18;
                    let (kind, param, n, at) = (u32_at(p, c)?, u32_at(p, c + 4)?, (u32_at(p, c + 8)? as usize).min(64), u32_at(p, c + 12)? as usize);
                    let points = (0..n)
                        .map(|j| Ok((f32_at(p, H_BASE + at + j * 16)?, f32_at(p, H_BASE + at + j * 16 + 4)?)))
                        .collect::<Result<Vec<_>>>()?;
                    Ok(Curve { kind, param, points })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((child, curves))
        })
        .collect()
}

/// A distance curve (class 7): `<Binary>`, the point count at +0x14, then
/// from +0x18 16-byte points `(f32 metres, f32 dB, u32 shape, u32)`.
pub fn parse_falloff(bao: &Bao) -> Result<Vec<(f32, f32)>> {
    let p = bao.payload();
    if bao.class != CLASS_FALLOFF || !p.starts_with(b"<Binary>") {
        bail!("BAO {:08x} is not a distance curve", bao.id);
    }
    let n = u32_at(p, 0x14)? as usize;
    if n == 0 || n > 64 {
        bail!("distance curve {:08x}: {n} points", bao.id);
    }
    (0..n).map(|k| Ok((f32_at(p, 0x18 + k * 16)?, f32_at(p, 0x1C + k * 16)?))).collect()
}

/// How a sound is placed (a class 9 BAO).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Positioning {
    /// Placed in the world (+0xB0); the others (menus, music, beds) play
    /// flat, not fading with distance.
    pub three_d: bool,
    /// Its distance curve (+0xC0, a class 7 id).
    pub curve: Option<u32>,
}

pub fn parse_positioning(bao: &Bao) -> Result<Positioning> {
    let p = bao.payload();
    if bao.class != CLASS_POSITIONING {
        bail!("BAO {:08x} is class {} not a positioning", bao.id, bao.class);
    }
    Ok(Positioning { three_d: u32_at(p, 0xB0)? != 0, curve: Some(u32_at(p, 0xC0)?).filter(|c| *c != 0 && *c != u32::MAX) })
}

/// The positioning (class 9 id) a playable header's sound plays with: an
/// audio header's at +0x88, a layered one's per layer (the last word of its
/// table entry).
pub fn header_positioning(bao: &Bao) -> Vec<u32> {
    let p = bao.payload();
    let at = |o: usize| u32_at(p, o).ok();
    match at(4) {
        Some(1) => at(0x88).into_iter().collect(),
        Some(6) => {
            let (Some(count), Some(cues), Some(labels)) = (at(0x3C), at(0x68), at(0x6C)) else { return Vec::new() };
            let table = H_BASE + cues as usize * 8 + labels as usize;
            (0..count.min(64) as usize).filter_map(|i| at(table + i * H_ENTRY_LAYER + 0x28)).collect()
        }
        _ => Vec::new(),
    }
}

/// How a sound fades with its distance from the listener.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fade {
    /// Its positioning isn't known.
    Unknown,
    /// Not placed in the world: no fade.
    Flat,
    /// By this distance curve (class 7 id).
    Curve(u32),
}

/// A header's own gain (dB), at +0x18 of every header type.
pub fn header_gain(bao: &Bao) -> f32 {
    f32_at(bao.payload(), 0x18).ok().filter(|g| g.is_finite() && g.abs() < 200.0).unwrap_or(0.0)
}

/// Byte range of inline layer data inside a layer header BAO.
fn layer_inline_data<'a>(bao: &Bao<'a>, len: usize) -> Result<&'a [u8]> {
    let p = bao.payload();
    let extra = u32_at(p, 0x60)? as usize;
    let start = H_BASE + extra;
    p.get(start..(start + len).min(p.len())).context("layer inline data out of range")
}

/// Class 1 event: what the game triggers by id.
///
/// Kinds: `1` play target, `2` stop the target event (e.g. end of an
/// automatic-fire loop), `4` play two targets, `6`/`10` push/pop a mixer
/// state (class 10 BAO in `2fffffff.spk`), `8` switch on a game state
/// (surface, environment...), `9` group: trigger every target together.
#[derive(Clone, Debug)]
pub struct Event {
    pub id: u32,
    pub kind: u32,
    /// Header (or event) ids this event plays.
    pub targets: Vec<u32>,
    pub volume: f32,
    /// Kind 8: `(switch group, [(target, value id)])`.
    pub switch: Option<(u32, Vec<(u32, u32)>)>,
}

impl Event {
    /// False for stop / mixer events that never start audio themselves.
    pub fn plays(&self) -> bool {
        !matches!(self.kind, 2 | 6 | 10)
    }
}

pub fn parse_event(bao: &Bao) -> Result<Event> {
    if bao.class != CLASS_EVENT {
        bail!("BAO {:08x} is class {} not an event", bao.id, bao.class);
    }
    let p = bao.payload();
    let id = u32_at(p, 0)?;
    let kind = u32_at(p, 4)?;
    let mut targets = Vec::new();
    let mut volume = 1.0;
    let mut switch = None;
    match kind {
        // 1 = play, 2 = stop/other event, 6/10 = start/stop of a looping group
        1 | 2 | 6 | 10 => {
            targets.push(u32_at(p, 8)?);
            if kind == 1 {
                volume = f32_at(p, 0x10)?;
            }
        }
        // play two objects (second is often a silence/delay)
        4 => {
            targets.push(u32_at(p, 8)?);
            targets.push(u32_at(p, 12)?);
        }
        // switch: group at +8, default at +0xC, count at +0x14, 12-byte (target, 0, value) entries at +0x48
        8 => {
            let group = u32_at(p, 8)?;
            let count = (u32_at(p, 0x14)? as usize).min(1024);
            let cases = (0..count)
                .map(|i| Ok((u32_at(p, 0x48 + i * 12)?, u32_at(p, 0x48 + i * 12 + 8)?)))
                .collect::<Result<Vec<_>>>()?;
            targets.push(u32_at(p, 12)?);
            targets.extend(cases.iter().map(|c| c.0));
            switch = Some((group, cases));
        }
        // group: count at +0xC, ids at +0x48
        9 => {
            let count = (u32_at(p, 0xC)? as usize).min(1024);
            for i in 0..count {
                targets.push(u32_at(p, 0x48 + i * 4)?);
            }
        }
        _ => {}
    }
    targets.retain(|&t| t != 0 && t != u32::MAX);
    targets.dedup();
    Ok(Event { id, kind, targets, volume, switch })
}

// ---------------------------------------------------------------------------
// multi-track ("layer") block de-interleaving

/// De-interleaves Ubisoft's blocked multi-track stream (layer header versions
/// 0x00040008/0x000B0008/0x000C0008/0x00100008/0x00100009) into one byte
/// stream per layer.
pub fn deinterleave_layers(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let version = u32_at(data, 0)?;
    let header_sizes_start = match version {
        0x0004_0008 | 0x000B_0008 | 0x000C_0008 | 0x0010_0008 => 0x1C,
        0x0010_0009 => 0x5C,
        v => bail!("unknown layer stream version {v:08x}"),
    };
    let layer_count = u32_at(data, 8)? as usize;
    if layer_count == 0 || layer_count > 64 {
        bail!("bad layer stream count {layer_count}");
    }
    let header_data_start = header_sizes_start + layer_count * 4;
    let block_sizes_start = 0x08;
    let block_data_start = 0x08 + layer_count * 4;

    let mut out: Vec<Vec<u8>> = vec![Vec::new(); layer_count];
    // header block: per-layer codec headers (e.g. Ogg identification/setup pages)
    let mut pos = header_data_start;
    for (i, layer) in out.iter_mut().enumerate() {
        let sz = u32_at(data, header_sizes_start + i * 4)? as usize;
        layer.extend_from_slice(data.get(pos..pos + sz).context("layer header data out of range")?);
        pos += sz;
    }
    let mut block_size = u32_at(data, 0x18)? as usize;
    while block_size != 0 && block_size != u32::MAX as usize && pos < data.len() {
        let next = u32_at(data, pos + 4)? as usize;
        let mut dpos = pos + block_data_start;
        for (i, layer) in out.iter_mut().enumerate() {
            let sz = u32_at(data, pos + block_sizes_start + i * 4)? as usize;
            let chunk = data.get(dpos..dpos + sz).with_context(|| {
                format!("layer block at {pos:#x} (layer {i}, {sz} bytes) out of range")
            })?;
            layer.extend_from_slice(chunk);
            dpos += sz;
        }
        pos += block_size;
        block_size = next;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// codecs

const IMA_STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80,
    88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544,
    598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749,
    3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635,
    13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const IMA_INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

#[derive(Clone, Copy, Default)]
struct ImaState {
    hist: i32,
    index: i32,
}

impl ImaState {
    #[inline]
    fn expand(&mut self, nibble: u8) -> i16 {
        let step = IMA_STEPS[self.index as usize];
        let mut delta = (((nibble & 7) as i32 * 2 + 1) * step) >> 3;
        if nibble & 8 != 0 {
            delta = -delta;
        }
        self.hist = (self.hist + delta).clamp(-32768, 32767);
        self.index = (self.index + IMA_INDEX[nibble as usize]).clamp(0, 88);
        self.hist as i16
    }
}

/// Parsed Ubisoft IMA (v6) stream header.
///
/// Layout (little endian): `u8 version (6)`, `u8 0`, 12 unknown bytes,
/// `i16 pcm_samples` (10), then per channel (2 slots) `i16 hist, u8 step, u8 0`,
/// `u32 ?`, `u32 flags` (2 = seekable), `u32 seek_size`, seek table, then
/// `pcm_samples` raw interleaved s16 frames, then 4-bit nibbles (high nibble
/// first; stereo packs L in the high and R in the low nibble).
///
/// The seek table is `u32 count` + `count + 1` pairs `(u32 sample, u32 offset)`
/// (offsets from the stream start, last pair = end). When present the nibble
/// data is cut into blocks at those points and every block starts with a
/// `i16 hist, u8 step, u8 0` header per channel.
#[derive(Clone, Debug)]
pub struct UbiImaHeader {
    pub version: u8,
    /// Samples per channel stored as raw PCM right after the header.
    pub pcm_samples: usize,
    /// Offset of the PCM samples.
    pub data_offset: usize,
    /// Initial predictor/step index per channel.
    pub hist: [i16; 2],
    pub step: [u8; 2],
    /// `(sample, byte offset)` block starts, including the final end marker.
    pub seek: Vec<(u32, u32)>,
}

pub fn parse_ubi_ima_header(data: &[u8]) -> Result<UbiImaHeader> {
    let version = *data.first().context("empty IMA stream")?;
    if !(3..=6).contains(&version) {
        bail!("unsupported Ubi IMA version {version}");
    }
    let base = 2;
    let pcm_samples = i16_at(data, base + 0x0C)?;
    if !(0..=64).contains(&pcm_samples) {
        bail!("Ubi IMA: implausible PCM preamble {pcm_samples}");
    }
    let hist = [i16_at(data, base + 0x0E)?, i16_at(data, base + 0x12)?];
    let step = [data[base + 0x10], data[base + 0x14]];
    let mut off = base + 0x0E + 0x08 + 0x04;
    let mut seek = Vec::new();
    if version == 6 {
        let seek_size = u32_at(data, off + 4)? as usize;
        let table = off + 8;
        if seek_size >= 4 {
            let count = u32_at(data, table)? as usize;
            if 4 + (count + 1) * 8 <= seek_size {
                for i in 0..=count {
                    seek.push((u32_at(data, table + 4 + i * 8)?, u32_at(data, table + 8 + i * 8)?));
                }
            }
        }
        off = table + seek_size;
    }
    Ok(UbiImaHeader { version, pcm_samples: pcm_samples as usize, data_offset: off, hist, step, seek })
}

/// Expands `count` frames of nibbles starting at `data[off..]` into `out`.
fn ima_nibbles(data: &[u8], off: usize, count: usize, st: &mut [ImaState; 2], channels: usize, out: &mut Vec<i16>) {
    let bytes = data.get(off..).unwrap_or(&[]);
    if channels == 1 {
        for j in 0..count.min(bytes.len() * 2) {
            let b = bytes[j / 2];
            out.push(st[0].expand(if j % 2 == 0 { b >> 4 } else { b & 0xF }));
        }
    } else {
        for &b in bytes.iter().take(count) {
            out.push(st[0].expand(b >> 4));
            out.push(st[1].expand(b & 0xF));
        }
    }
}

/// Decodes Ubisoft IMA ADPCM (BAO codecs 2 and 3, header v3..v6) to
/// interleaved s16. `num_samples` is per channel; decoding stops early if the
/// data runs out.
pub fn decode_ubi_ima(data: &[u8], channels: usize, num_samples: usize) -> Result<Vec<i16>> {
    if channels == 0 || channels > 2 {
        bail!("Ubi IMA: unsupported channel count {channels}");
    }
    let h = parse_ubi_ima_header(data)?;
    let mut st = [ImaState::default(); 2];
    for c in 0..channels {
        st[c] = ImaState { hist: h.hist[c] as i32, index: (h.step[c] as i8 as i32).clamp(0, 88) };
    }
    let mut out = Vec::with_capacity(num_samples * channels);
    let mut off = h.data_offset;
    let pre = h.pcm_samples.min(num_samples);
    for _ in 0..pre {
        for _ in 0..channels {
            out.push(i16_at(data, off)?);
            off += 2;
        }
    }
    off += (h.pcm_samples - pre) * 2 * channels;
    if h.seek.len() >= 2 {
        // seekable: independent blocks, each with its own predictor header
        for w in h.seek.windows(2) {
            let (s0, o0) = (w[0].0 as usize, w[0].1 as usize);
            let s1 = (w[1].0 as usize).min(num_samples);
            if s1 <= s0 || out.len() / channels != s0 {
                break;
            }
            for c in 0..channels {
                st[c].hist = i16_at(data, o0 + c * 4)? as i32;
                st[c].index = (*data.get(o0 + c * 4 + 2).context("IMA block header")? as i32).clamp(0, 88);
            }
            ima_nibbles(data, o0 + 4 * channels, s1 - s0, &mut st, channels, &mut out);
        }
    } else {
        ima_nibbles(data, off, num_samples - pre, &mut st, channels, &mut out);
    }
    // The encoder drops the final half byte when the nibble count is odd:
    // repeat the last frame so the length matches the header exactly.
    let want = num_samples * channels;
    if out.len() < want && want - out.len() <= 2 * channels && out.len() >= channels {
        let last: Vec<i16> = out[out.len() - channels..].to_vec();
        while out.len() < want {
            out.extend_from_slice(&last);
        }
    }
    Ok(out)
}

/// Summary of an Ogg Vorbis stream, from walking its pages.
#[derive(Clone, Debug, Default)]
pub struct OggInfo {
    pub channels: u8,
    pub sample_rate: u32,
    /// Granule position of the last page = total samples per channel.
    pub last_granule: u64,
    pub pages: usize,
    /// Pages whose CRC did not match.
    pub bad_crc: usize,
    /// Bytes after the last complete page.
    pub trailing: usize,
}

const fn ogg_crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04C1_1DB7 } else { r << 1 };
            j += 1;
        }
        t[i] = r;
        i += 1;
    }
    t
}
static OGG_CRC: [u32; 256] = ogg_crc_table();

/// Walks the pages of an Ogg Vorbis file, checking structure and CRCs.
pub fn ogg_info(data: &[u8]) -> Result<OggInfo> {
    let mut info = OggInfo::default();
    let mut pos = 0;
    while pos + 27 <= data.len() {
        if &data[pos..pos + 4] != b"OggS" {
            bail!("Ogg: lost sync at {pos:#x}");
        }
        let nseg = data[pos + 26] as usize;
        let body: usize = data.get(pos + 27..pos + 27 + nseg).context("Ogg: truncated segment table")?.iter().map(|&b| b as usize).sum();
        let len = 27 + nseg + body;
        let page = data.get(pos..pos + len).context("Ogg: truncated page")?;
        let stored = u32::from_le_bytes(page[22..26].try_into().unwrap());
        let mut crc = 0u32;
        for (i, &b) in page.iter().enumerate() {
            let b = if (22..26).contains(&i) { 0 } else { b };
            crc = (crc << 8) ^ OGG_CRC[((crc >> 24) as u8 ^ b) as usize];
        }
        if crc != stored {
            info.bad_crc += 1;
        }
        let granule = u64::from_le_bytes(page[6..14].try_into().unwrap());
        if granule != u64::MAX {
            info.last_granule = granule;
        }
        if info.pages == 0 {
            let p = &page[27 + nseg..];
            if p.len() < 16 || &p[..7] != b"\x01vorbis" {
                bail!("Ogg: first packet is not a Vorbis identification header");
            }
            info.channels = p[11];
            info.sample_rate = u32::from_le_bytes(p[12..16].try_into().unwrap());
        }
        info.pages += 1;
        pos += len;
    }
    info.trailing = data.len() - pos;
    if info.pages == 0 {
        bail!("Ogg: no pages");
    }
    Ok(info)
}

/// Converts s16le bytes to samples.
pub fn pcm16_from_bytes(data: &[u8]) -> Vec<i16> {
    data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
}

/// Wraps interleaved s16 PCM in a RIFF/WAVE container (Bevy's `wav` loader reads it).
pub fn pcm_to_wav(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&channels.to_le_bytes());
    w.extend_from_slice(&sample_rate.to_le_bytes());
    w.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
    w.extend_from_slice(&(channels * 2).to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

// ---------------------------------------------------------------------------
// decoding API

/// Playable audio.
#[derive(Clone, Debug)]
pub enum SoundData {
    /// Interleaved signed 16-bit PCM.
    Pcm { sample_rate: u32, channels: u16, samples: Vec<i16> },
    /// A complete Ogg Vorbis file (Bevy plays `.ogg` directly).
    Ogg(Vec<u8>),
}

impl SoundData {
    /// Bytes for Bevy's audio loaders: a `.wav` for PCM, the raw `.ogg` otherwise.
    pub fn to_file_bytes(&self) -> (Vec<u8>, &'static str) {
        match self {
            SoundData::Pcm { sample_rate, channels, samples } => {
                (pcm_to_wav(*sample_rate, *channels, samples), "wav")
            }
            SoundData::Ogg(b) => (b.clone(), "ogg"),
        }
    }
}

/// One decoded sound (a type 1 header, or one track of a type 6 layered header).
#[derive(Clone, Debug)]
pub struct DecodedSound {
    /// Header BAO id.
    pub header_id: u32,
    /// `(index, count)` for tracks of a layered sound (play them together).
    pub layer: Option<(u32, u32)>,
    pub codec: Codec,
    pub channels: u32,
    pub sample_rate: u32,
    /// Samples per channel according to the header.
    pub num_samples: u32,
    pub looping: bool,
    pub loop_start: u32,
    pub data: SoundData,
}

impl DecodedSound {
    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.num_samples as f32 / self.sample_rate as f32
        }
    }
}

/// Supplies files referenced by a package: streamed `.sbao` data or other `.spk`s.
pub trait SoundFiles {
    /// Returns the bytes of `soundbinary\{id:08x}.sbao` if available.
    fn sbao(&mut self, id: u32) -> Option<Vec<u8>>;
    /// Returns the bytes of `soundbinary\{id:08x}.spk` if available.
    fn spk(&mut self, _id: u32) -> Option<Vec<u8>> {
        None
    }
}

/// No external files (only sounds stored entirely in the package decode).
pub struct NoFiles;
impl SoundFiles for NoFiles {
    fn sbao(&mut self, _id: u32) -> Option<Vec<u8>> {
        None
    }
}

impl<F: FnMut(&str) -> Option<Vec<u8>>> SoundFiles for F {
    fn sbao(&mut self, id: u32) -> Option<Vec<u8>> {
        self(&sbao_path(id))
    }
    fn spk(&mut self, id: u32) -> Option<Vec<u8>> {
        self(&spk_path(id))
    }
}

/// Decodes every playable sound stored in a `.spk` (or atomic `.bao`) using
/// only the bytes given. Streamed sounds are skipped; use
/// [`decode_sound_with`] to supply `.sbao` files.
pub fn decode_sound(bytes: &[u8]) -> Result<Vec<DecodedSound>> {
    decode_sound_with(bytes, &mut NoFiles)
}

/// Like [`decode_sound`] but loads streamed data through `files`
/// (e.g. `&mut |path: &str| vfs.read(path).ok()`).
pub fn decode_sound_with(bytes: &[u8], files: &mut dyn SoundFiles) -> Result<Vec<DecodedSound>> {
    let baos = parse_container(bytes, 0)?;
    let mut out = Vec::new();
    let mut last_err = None;
    for bao in baos.iter().filter(|b| b.class == CLASS_HEADER) {
        match decode_header(bao, &baos, files) {
            Ok(mut v) => out.append(&mut v),
            Err(e) => last_err = Some(e.context(format!("header {:08x}", bao.id))),
        }
    }
    if out.is_empty() {
        if let Some(e) = last_err {
            return Err(e);
        }
    }
    Ok(out)
}

/// Collects the encoded bytes behind a data source.
pub fn load_source(
    source: &DataSource,
    header_bao: &Bao,
    package: &[Bao],
    files: &mut dyn SoundFiles,
) -> Result<Vec<u8>> {
    let memory_part = |len: usize| -> Result<Vec<u8>> {
        if source.inline {
            return Ok(layer_inline_data(header_bao, len)?.to_vec());
        }
        let m = package
            .iter()
            .find(|b| b.id == source.resource_id && b.class == CLASS_MEMORY)
            .with_context(|| format!("memory BAO {:08x} not in package", source.resource_id))?;
        let p = m.payload();
        Ok(p[..len.min(p.len())].to_vec())
    };
    if !source.is_stream {
        return memory_part(source.size as usize);
    }
    let mut data = Vec::with_capacity(source.size as usize);
    if source.prefetch_size > 0 {
        data = memory_part(source.prefetch_size as usize)?;
    }
    let need = source.size.saturating_sub(source.prefetch_size) as usize;
    if need > 0 {
        let sbao = files
            .sbao(source.resource_id)
            .with_context(|| format!("stream {} not available", sbao_path(source.resource_id)))?;
        let s = Bao::parse(source.resource_id, &sbao)?;
        let p = s.payload();
        data.extend_from_slice(&p[..need.min(p.len())]);
    }
    Ok(data)
}

fn decode_codec(codec: Codec, data: Vec<u8>, channels: u32, sample_rate: u32, num_samples: u32) -> Result<SoundData> {
    Ok(match codec {
        Codec::Ogg => {
            let start = data.windows(4).position(|w| w == b"OggS").context("no Ogg page in data")?;
            let mut d = data;
            d.drain(..start);
            SoundData::Ogg(d)
        }
        Codec::Pcm16 => {
            let mut samples = pcm16_from_bytes(&data);
            samples.truncate(num_samples as usize * channels as usize);
            SoundData::Pcm { sample_rate, channels: channels as u16, samples }
        }
        Codec::UbiIma | Codec::UbiImaSeek => SoundData::Pcm {
            sample_rate,
            channels: channels as u16,
            samples: decode_ubi_ima(&data, channels as usize, num_samples as usize)?,
        },
        c => bail!("codec {c:?} not supported"),
    })
}

/// Decodes a single header BAO (type 1 or 6) to playable sounds.
pub fn decode_header(bao: &Bao, package: &[Bao], files: &mut dyn SoundFiles) -> Result<Vec<DecodedSound>> {
    decode_header_layers(bao, package, files, None)
}

/// Like [`decode_header`]; for layered headers decodes only `only_layer` when given.
pub fn decode_header_layers(
    bao: &Bao,
    package: &[Bao],
    files: &mut dyn SoundFiles,
    only_layer: Option<u32>,
) -> Result<Vec<DecodedSound>> {
    match parse_header(bao)? {
        Header::Audio(a) => {
            let raw = load_source(&a.source, bao, package, files)?;
            let raw = unwrap_single_layer(raw)?;
            let data = decode_codec(a.codec, raw, a.channels, a.sample_rate, a.num_samples)?;
            Ok(vec![DecodedSound {
                header_id: a.id,
                layer: None,
                codec: a.codec,
                channels: a.channels,
                sample_rate: a.sample_rate,
                num_samples: a.num_samples,
                looping: a.looping,
                loop_start: a.loop_start,
                data,
            }])
        }
        Header::Layer(l) => {
            let raw = load_source(&l.source, bao, package, files)?;
            let tracks = deinterleave_layers(&raw)?;
            let n = l.layers.len() as u32;
            let mut out = Vec::new();
            for (i, (info, track)) in l.layers.iter().zip(tracks).enumerate() {
                if only_layer.is_some_and(|w| w != i as u32) {
                    continue;
                }
                let data = decode_codec(info.codec, track, info.channels, info.sample_rate, info.num_samples)
                    .with_context(|| format!("layer {i}"))?;
                out.push(DecodedSound {
                    header_id: l.id,
                    layer: Some((i as u32, n)),
                    codec: info.codec,
                    channels: info.channels,
                    sample_rate: info.sample_rate,
                    num_samples: info.num_samples,
                    looping: l.looping,
                    loop_start: 0,
                    data,
                });
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

/// Some single-track streams still use the multi-track block container.
fn unwrap_single_layer(raw: Vec<u8>) -> Result<Vec<u8>> {
    if raw.len() >= 12 && u32_at(&raw, 0)? == 0x0010_0009 && u32_at(&raw, 8)? == 1 {
        return Ok(deinterleave_layers(&raw)?.remove(0));
    }
    Ok(raw)
}

// ---------------------------------------------------------------------------
// whole-install API (for the converter / game)

/// One playable sound in the install: a type 1 header, or one track of a
/// type 6 layered header (tracks of the same header are meant to play together).
#[derive(Clone, Debug, PartialEq)]
pub struct SoundRef {
    pub header_id: u32,
    /// `(index, count)` for layered sounds.
    pub layer: Option<(u32, u32)>,
    /// Archive path of a package that holds the header, `soundbinary\xxxxxxxx.spk`.
    pub package: String,
    pub codec: Codec,
    pub channels: u32,
    pub sample_rate: u32,
    /// Samples per channel.
    pub num_samples: u32,
    pub looping: bool,
    pub loop_start: u32,
    /// Data comes (partly) from a `.sbao` stream file.
    pub streamed: bool,
}

impl SoundRef {
    /// Stable file stem: `001837fb`, or `0001712e_l0` for layer tracks.
    pub fn file_stem(&self) -> String {
        match self.layer {
            Some((i, _)) => format!("{:08x}_l{i}", self.header_id),
            None => format!("{:08x}", self.header_id),
        }
    }

    /// `"ogg"` or `"wav"`: the extension [`decode_to_file_bytes`] will produce.
    pub fn file_ext(&self) -> &'static str {
        if self.codec == Codec::Ogg {
            "ogg"
        } else {
            "wav"
        }
    }

    pub fn duration_secs(&self) -> f32 {
        self.num_samples as f32 / self.sample_rate.max(1) as f32
    }
}

/// A playable choice behind an event: header id (play all its layers) and
/// the relative probability of it being picked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Variant {
    pub header_id: u32,
    pub weight: f32,
}

/// Switch group the game sets to the surface underfoot or at a hit (values
/// from `logicmaterials.lib`, e.g. `0x00074CA9` sand).
pub const SURFACE_GROUP: u32 = 0x00074C89;
/// Footstep pace: walk, crouched walk, run (the first two share sounds).
pub const GAIT_GROUP: u32 = 0x004E4AF4;
pub const GAIT_VALUES: [u32; 3] = [0x004E4AF5, 0x004E4AF6, 0x004E4AF7];

/// Small slot (1..=48) for a surface switch value, 0 for anything else.
fn surface_slot(value: u32) -> u32 {
    match value {
        0x0007_4C8A..=0x0007_4CB7 => value - 0x0007_4C89,
        0x0009_F95E => 47,
        0x001B_0357 => 48,
        _ => 0,
    }
}

/// Every surface switch value with a slot.
pub fn surface_values() -> impl Iterator<Item = u32> {
    (0x0007_4C8Au32..=0x0007_4CB7).chain([0x0009_F95E, 0x001B_0357])
}

/// The manifest id of `event` played with the surface switch at `surface`
/// (a switch value, 0 = unset) and the gait at `gait` (0 = unset, 1..=3 =
/// [`GAIT_VALUES`]). Event ids fit in 24 bits, so the switch state packs
/// above them: surface slot in bits 24..30, gait in 30..32. The manifest
/// writes these for events whose sounds depend on the switches.
pub fn switched_event(event: u32, surface: u32, gait: u8) -> u32 {
    (event & 0x00FF_FFFF) | (surface_slot(surface) << 24) | ((gait.min(3) as u32) << 30)
}

/// The event/header graph of every `.spk` in the install.
#[derive(Default)]
pub struct SoundGraph {
    pub events: HashMap<u32, Event>,
    /// Every header BAO (playable or container).
    pub headers: HashMap<u32, Header>,
    /// BAO id -> package path that holds it (the package named after the id wins).
    pub package_of: HashMap<u32, String>,
    /// Ids that name a `soundbinary\xxxxxxxx.spk` (what the game loads as `sndres0x...`).
    pub packages: Vec<u32>,
    /// Blend (type 7) headers' children with their parameter curves.
    pub blends: HashMap<u32, Vec<(u32, Vec<Curve>)>>,
    /// Each header's own gain (dB), where it isn't 0.
    pub gains: HashMap<u32, f32>,
    /// Distance curves (class 7): volume (dB) by distance (m).
    pub falloffs: HashMap<u32, Vec<(f32, f32)>>,
    /// Positioning settings (class 9).
    pub positionings: HashMap<u32, Positioning>,
    /// Each playable header's positioning ids (one per layer for layered ones).
    pub placed: HashMap<u32, Vec<u32>>,
}

/// Every `soundbinary\*.spk` path known to the VFS (needs the filelists loaded).
pub fn spk_paths(vfs: &Vfs) -> Vec<String> {
    let mut v: Vec<String> = vfs
        .names
        .values()
        .filter(|n| n.starts_with("soundbinary\\") && n.ends_with(".spk"))
        .cloned()
        .collect();
    v.sort();
    v
}

impl SoundGraph {
    /// Parses every `.spk` in the install (~24k small files, a few seconds).
    pub fn build(vfs: &Vfs) -> SoundGraph {
        let mut g = SoundGraph::default();
        for path in spk_paths(vfs) {
            let Ok(bytes) = vfs.read(&path) else { continue };
            let Ok(spk) = Spk::parse(&bytes) else { continue };
            let own = id_from_path(&path);
            if let Some(id) = own {
                g.packages.push(id);
            }
            for b in &spk.baos {
                if own == Some(b.id) || !g.package_of.contains_key(&b.id) {
                    g.package_of.insert(b.id, path.clone());
                }
                match b.class {
                    CLASS_EVENT => {
                        if let Ok(e) = parse_event(b) {
                            g.events.entry(b.id).or_insert(e);
                        }
                    }
                    CLASS_HEADER => {
                        if let Ok(h) = parse_header(b) {
                            if !g.headers.contains_key(&b.id) {
                                if let Ok(blend) = parse_blend(b) {
                                    g.blends.insert(b.id, blend);
                                }
                                let gain = header_gain(b);
                                if gain != 0.0 {
                                    g.gains.insert(b.id, gain);
                                }
                                let placed = header_positioning(b);
                                if !placed.is_empty() {
                                    g.placed.insert(b.id, placed);
                                }
                            }
                            g.headers.entry(b.id).or_insert(h);
                        }
                    }
                    CLASS_FALLOFF => {
                        if let Ok(points) = parse_falloff(b) {
                            g.falloffs.entry(b.id).or_insert(points);
                        }
                    }
                    CLASS_POSITIONING => {
                        if let Ok(p) = parse_positioning(b) {
                            g.positionings.entry(b.id).or_insert(p);
                        }
                    }
                    _ => {}
                }
            }
        }
        g.packages.sort_unstable();
        g
    }

    /// How `header`'s sound (`layer` of a layered one) fades with distance.
    pub fn fade(&self, header: u32, layer: Option<u32>) -> Fade {
        let Some(placed) = self.placed.get(&header) else { return Fade::Unknown };
        let Some(p) = placed.get(layer.unwrap_or(0) as usize).and_then(|id| self.positionings.get(id)) else { return Fade::Unknown };
        match p.curve {
            _ if !p.three_d => Fade::Flat,
            Some(c) if self.falloffs.contains_key(&c) => Fade::Curve(c),
            _ => Fade::Unknown,
        }
    }

    /// Every playable sound (deduplicated by header id), sorted by id.
    pub fn sounds(&self) -> Vec<SoundRef> {
        let mut out = Vec::new();
        let mut ids: Vec<&u32> = self.headers.keys().collect();
        ids.sort_unstable();
        for id in ids {
            let package = self.package_of.get(id).cloned().unwrap_or_default();
            match &self.headers[id] {
                Header::Audio(a) => out.push(SoundRef {
                    header_id: a.id,
                    layer: None,
                    package,
                    codec: a.codec,
                    channels: a.channels,
                    sample_rate: a.sample_rate,
                    num_samples: a.num_samples,
                    looping: a.looping,
                    loop_start: a.loop_start,
                    streamed: a.source.is_stream,
                }),
                Header::Layer(l) => {
                    let n = l.layers.len() as u32;
                    for (i, info) in l.layers.iter().enumerate() {
                        out.push(SoundRef {
                            header_id: l.id,
                            layer: Some((i as u32, n)),
                            package: package.clone(),
                            codec: info.codec,
                            channels: info.channels,
                            sample_rate: info.sample_rate,
                            num_samples: info.num_samples,
                            looping: l.looping,
                            loop_start: 0,
                            streamed: l.source.is_stream,
                        });
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Flattens an event (or header) id into every playable header it can
    /// trigger (all voices merged, no switch state). See [`SoundGraph::voices`]
    /// for the structured form.
    pub fn resolve(&self, id: u32) -> Vec<Variant> {
        self.resolve_with(id, &HashMap::new())
    }

    /// Like [`SoundGraph::resolve`] but switch containers only follow the case
    /// selected in `switches` (switch group id -> value id) when the group is
    /// present, e.g. `{0x00074C89: 0x00074CA9}` = surface "Sand".
    pub fn resolve_with(&self, id: u32, switches: &HashMap<u32, u32>) -> Vec<Variant> {
        let mut merged: Vec<Variant> = Vec::new();
        for v in self.voices(id, switches).into_iter().flatten() {
            match merged.iter_mut().find(|m| m.header_id == v.header_id) {
                Some(m) => m.weight += v.weight,
                None => merged.push(v),
            }
        }
        merged
    }

    /// Structured resolution for playback: the outer list holds voices that
    /// start together (group events, chains, blends, sequences), each voice
    /// lists weighted alternatives (random containers, unresolved switches) of
    /// which one is picked. Every variant is a header id whose layers (if any)
    /// play together. Stop and mixer events yield nothing.
    pub fn voices(&self, id: u32, switches: &HashMap<u32, u32>) -> Vec<Vec<Variant>> {
        self.voices_rec(id, switches, 0)
    }

    fn voices_rec(&self, id: u32, sw: &HashMap<u32, u32>, depth: usize) -> Vec<Vec<Variant>> {
        if depth > 24 {
            return Vec::new();
        }
        if let Some(e) = self.events.get(&id) {
            if !e.plays() {
                return Vec::new();
            }
            if let Some((group, cases)) = &e.switch {
                let chosen: Vec<u32> = match sw.get(group) {
                    Some(v) => cases.iter().filter(|c| c.1 == *v).map(|c| c.0).collect(),
                    None => Vec::new(),
                };
                let targets = if chosen.is_empty() { e.targets.clone() } else { chosen };
                return vec![self.alternatives(&targets, None, sw, depth)];
            }
            return e
                .targets
                .iter()
                .filter(|&&t| t != id)
                .flat_map(|&t| self.voices_rec(t, sw, depth + 1))
                .collect();
        }
        let Some(h) = self.headers.get(&id) else { return Vec::new() };
        match h {
            Header::Audio(_) | Header::Layer(_) => vec![vec![Variant { header_id: id, weight: 1.0 }]],
            Header::Silence { .. } => Vec::new(),
            Header::Random { children, .. } => {
                let ids: Vec<u32> = children.iter().map(|c| c.0).collect();
                let weights: Vec<f32> = children.iter().map(|c| c.1.max(0.0)).collect();
                vec![self.alternatives(&ids, Some(&weights), sw, depth)]
            }
            Header::Switch { group, cases, .. } => {
                let chosen: Vec<u32> = match sw.get(group) {
                    Some(v) => cases.iter().filter(|c| c.1 == *v).map(|c| c.0).collect(),
                    None => Vec::new(),
                };
                if chosen.is_empty() {
                    let all: Vec<u32> = cases.iter().map(|c| c.0).collect();
                    vec![self.alternatives(&all, None, sw, depth)]
                } else {
                    chosen.iter().flat_map(|&c| self.voices_rec(c, sw, depth + 1)).collect()
                }
            }
            other => other
                .children()
                .into_iter()
                .filter(|&c| c != id)
                .flat_map(|c| self.voices_rec(c, sw, depth + 1))
                .collect(),
        }
    }

    /// One voice made of alternatives: each child's playable headers share the
    /// child's weight (equal weights when `weights` is None); duplicates merge.
    fn alternatives(&self, ids: &[u32], weights: Option<&[f32]>, sw: &HashMap<u32, u32>, depth: usize) -> Vec<Variant> {
        let total: f32 = weights.map(|w| w.iter().sum()).unwrap_or(ids.len() as f32);
        let mut out: Vec<Variant> = Vec::new();
        for (i, &c) in ids.iter().enumerate() {
            let w = weights.map(|w| w[i]).unwrap_or(1.0);
            let share = if total > 0.0 { w / total } else { 1.0 / ids.len().max(1) as f32 };
            let sub: Vec<Variant> = self.voices_rec(c, sw, depth + 1).into_iter().flatten().collect();
            let sub_total: f32 = sub.iter().map(|v| v.weight).sum();
            for v in sub {
                let weight = share * v.weight / sub_total.max(f32::MIN_POSITIVE);
                match out.iter_mut().find(|m| m.header_id == v.header_id) {
                    Some(m) => m.weight += weight,
                    None => out.push(Variant { header_id: v.header_id, weight }),
                }
            }
        }
        out
    }

    /// Every playable header `id` can reach with the gain (dB, summed along
    /// the way) and the blend curves it plays under; empty unless the way
    /// passes through a blend.
    pub fn voice_mods(&self, id: u32, sw: &HashMap<u32, u32>) -> Vec<(u32, f32, Vec<Curve>)> {
        let mut out = Vec::new();
        let mut curves = Vec::new();
        let mut blended = false;
        self.mods_rec(id, sw, 0, 0.0, &mut curves, &mut blended, &mut out);
        if !blended {
            out.clear();
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn mods_rec(&self, id: u32, sw: &HashMap<u32, u32>, depth: usize, gain: f32, curves: &mut Vec<Curve>, blended: &mut bool, out: &mut Vec<(u32, f32, Vec<Curve>)>) {
        if depth > 24 {
            return;
        }
        let pick = |group: &u32, cases: &[(u32, u32)]| -> Vec<u32> {
            match sw.get(group) {
                Some(v) if cases.iter().any(|c| c.1 == *v) => cases.iter().filter(|c| c.1 == *v).map(|c| c.0).collect(),
                _ => cases.iter().map(|c| c.0).collect(),
            }
        };
        if let Some(e) = self.events.get(&id) {
            if !e.plays() {
                return;
            }
            let targets = match &e.switch {
                Some((group, cases)) => pick(group, cases),
                None => e.targets.clone(),
            };
            for t in targets.into_iter().filter(|&t| t != id) {
                self.mods_rec(t, sw, depth + 1, gain, curves, blended, out);
            }
            return;
        }
        let Some(h) = self.headers.get(&id) else { return };
        let gain = gain + self.gains.get(&id).copied().unwrap_or(0.0);
        match h {
            Header::Audio(_) | Header::Layer(_) => {
                if !out.iter().any(|o: &(u32, f32, Vec<Curve>)| o.0 == id) {
                    out.push((id, gain, curves.clone()));
                }
            }
            Header::Switch { group, cases, .. } => {
                for c in pick(group, cases) {
                    self.mods_rec(c, sw, depth + 1, gain, curves, blended, out);
                }
            }
            _ => match self.blends.get(&id) {
                Some(children) => {
                    *blended = true;
                    for (child, own) in children {
                        let n = curves.len();
                        curves.extend(own.iter().cloned());
                        self.mods_rec(*child, sw, depth + 1, gain, curves, blended, out);
                        curves.truncate(n);
                    }
                }
                None => {
                    for c in h.children().into_iter().filter(|&c| c != id) {
                        self.mods_rec(c, sw, depth + 1, gain, curves, blended, out);
                    }
                }
            },
        }
    }

    /// The values each switch group under `id` is tested against.
    pub fn switch_values(&self, id: u32) -> HashMap<u32, std::collections::BTreeSet<u32>> {
        let mut out: HashMap<u32, std::collections::BTreeSet<u32>> = HashMap::new();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![(id, 0usize)];
        while let Some((id, depth)) = stack.pop() {
            if depth > 24 || !seen.insert(id) {
                continue;
            }
            if let Some(e) = self.events.get(&id) {
                if let Some((group, cases)) = &e.switch {
                    out.entry(*group).or_default().extend(cases.iter().map(|c| c.1));
                }
                stack.extend(e.targets.iter().map(|&t| (t, depth + 1)));
            } else if let Some(h) = self.headers.get(&id) {
                if let Header::Switch { group, cases, .. } = h {
                    out.entry(*group).or_default().extend(cases.iter().map(|c| c.1));
                }
                stack.extend(h.children().into_iter().map(|c| (c, depth + 1)));
            }
        }
        out
    }

    /// Switch groups that change what `id` plays.
    pub fn switch_groups(&self, id: u32) -> std::collections::HashSet<u32> {
        let mut out = std::collections::HashSet::new();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![(id, 0usize)];
        while let Some((id, depth)) = stack.pop() {
            if depth > 24 || !seen.insert(id) {
                continue;
            }
            if let Some(e) = self.events.get(&id) {
                if let Some((group, _)) = &e.switch {
                    out.insert(*group);
                }
                stack.extend(e.targets.iter().map(|&t| (t, depth + 1)));
            } else if let Some(h) = self.headers.get(&id) {
                if let Header::Switch { group, .. } = h {
                    out.insert(*group);
                }
                stack.extend(h.children().into_iter().map(|c| (c, depth + 1)));
            }
        }
        out
    }

    /// Plain-text manifest for a converted cache, one record per line:
    ///
    /// ```text
    /// sound <stem> <ext> <codec> <channels> <rate> <samples> <loop 0|1> <loop_start>
    /// event <id> <kind> <volume> <hdr>:<weight> <hdr>:<weight> | <hdr>:<weight> ...
    /// ```
    ///
    /// Voices are separated by `|` (they start together); inside a voice pick
    /// one alternative by weight. Layered headers expand to `<hdr>_l<i>` files.
    ///
    /// Events that depend on the surface or gait switches also get one record
    /// per switch state under [`switched_event`] ids; a state that plays the
    /// same as an earlier one is written as `same <id> <earlier id>`.
    ///
    /// Each sound ends with how it fades with distance: a curve id (written
    /// as `falloff <id> <metres>,<dB>;...`), `2d` for none, `-` unknown.
    pub fn manifest(&self) -> String {
        let mut s = String::from("# fc3 sound manifest v2\n");
        let mut curves = std::collections::BTreeSet::new();
        for r in self.sounds() {
            let fade = match self.fade(r.header_id, r.layer.map(|l| l.0)) {
                Fade::Unknown => "-".to_string(),
                Fade::Flat => "2d".to_string(),
                Fade::Curve(c) => {
                    curves.insert(c);
                    format!("{c:08x}")
                }
            };
            s += &format!(
                "sound {} {} {} {} {} {} {} {} {fade}\n",
                r.file_stem(),
                r.file_ext(),
                r.codec.name(),
                r.channels,
                r.sample_rate,
                r.num_samples,
                r.looping as u8,
                r.loop_start
            );
        }
        for c in curves {
            let pts: Vec<String> = self.falloffs[&c].iter().map(|(x, y)| format!("{x},{y}")).collect();
            s += &format!("falloff {c:08x} {}\n", pts.join(";"));
        }
        let mut ids: Vec<&u32> = self.events.keys().collect();
        ids.sort_unstable();
        let none = HashMap::new();
        for id in ids {
            let e = &self.events[id];
            let voices: Vec<String> = self
                .voices(*id, &none)
                .iter()
                .map(|alts| {
                    alts.iter().map(|v| format!("{:08x}:{:.4}", v.header_id, v.weight)).collect::<Vec<_>>().join(" ")
                })
                .collect();
            s += &format!("event {:08x} {} {} {}\n", id, e.kind, e.volume, voices.join(" | "));
            if e.plays() && *id <= 0x00FF_FFFF {
                s += &self.switched_records(*id, e);
                s += &self.blend_records(*id, e);
            }
        }
        s
    }

    /// For events that pass through blends (engines, tyres): each voice's
    /// gain and curves (`mod <event> <header> <gain dB> <curve>...`), and the
    /// event for every combination of the switch groups under it (`event
    /// <id>@<group>=<value>,<group>=<value>`, groups in order), with its own
    /// `mod` records.
    fn blend_records(&self, id: u32, e: &Event) -> String {
        let none = HashMap::new();
        if self.voice_mods(id, &none).is_empty() {
            return String::new();
        }
        let mods = |token: &str, sw: &HashMap<u32, u32>| -> String {
            self.voice_mods(id, sw)
                .iter()
                .map(|(h, gain, curves)| {
                    let c: Vec<String> = curves.iter().map(Curve::to_token).collect();
                    format!("mod {token} {h:08x} {gain} {}\n", c.join(" "))
                })
                .collect()
        };
        let mut out = mods(&format!("{id:08x}"), &none);
        let mut groups: Vec<(u32, Vec<u32>)> = self
            .switch_values(id)
            .into_iter()
            .filter(|(g, _)| *g != GAIT_GROUP)
            .map(|(g, v)| (g, v.into_iter().collect()))
            .collect();
        groups.sort_by_key(|g| g.0);
        // Every combination of the groups' values (a few hundred at most).
        let mut combos: Vec<Vec<(u32, u32)>> = vec![Vec::new()];
        for (group, values) in &groups {
            combos = combos.iter().flat_map(|c| values.iter().map(move |v| [c.clone(), vec![(*group, *v)]].concat())).collect();
            if combos.len() > 256 {
                return out;
            }
        }
        for combo in combos.into_iter().filter(|c| !c.is_empty()) {
            {
                let sw: HashMap<u32, u32> = combo.iter().copied().collect();
                let state: Vec<String> = combo.iter().map(|(g, v)| format!("{g:08x}={v:08x}")).collect();
                let token = format!("{id:08x}@{}", state.join(","));
                let voices: Vec<String> = self
                    .voices(id, &sw)
                    .iter()
                    .map(|alts| alts.iter().map(|v| format!("{:08x}:{:.4}", v.header_id, v.weight)).collect::<Vec<_>>().join(" "))
                    .collect();
                if voices.is_empty() {
                    continue;
                }
                out += &format!("event {token} {} {} {}\n", e.kind, e.volume, voices.join(" | "));
                out += &mods(&token, &sw);
            }
        }
        out
    }

    fn switched_records(&self, id: u32, e: &Event) -> String {
        let groups = self.switch_groups(id);
        let (surface, gait) = (groups.contains(&SURFACE_GROUP), groups.contains(&GAIT_GROUP));
        if !surface && !gait {
            return String::new();
        }
        let surfaces: Vec<u32> = if surface { std::iter::once(0).chain(surface_values()).collect() } else { vec![0] };
        let gaits: Vec<u8> = if gait { vec![0, 1, 2, 3] } else { vec![0] };
        let mut out = String::new();
        let mut written: Vec<(String, u32)> = Vec::new();
        for &sv in &surfaces {
            for &g in &gaits {
                if sv == 0 && g == 0 {
                    continue;
                }
                let mut sw = HashMap::new();
                if sv != 0 {
                    sw.insert(SURFACE_GROUP, sv);
                }
                if g != 0 {
                    sw.insert(GAIT_GROUP, GAIT_VALUES[g as usize - 1]);
                }
                let voices: Vec<String> = self
                    .voices(id, &sw)
                    .iter()
                    .map(|alts| alts.iter().map(|v| format!("{:08x}:{:.4}", v.header_id, v.weight)).collect::<Vec<_>>().join(" "))
                    .collect();
                if voices.is_empty() {
                    continue;
                }
                let body = voices.join(" | ");
                let sid = switched_event(id, sv, g);
                match written.iter().find(|(b, _)| *b == body) {
                    Some((_, first)) => out += &format!("same {sid:08x} {first:08x}\n"),
                    None => {
                        out += &format!("event {sid:08x} {} {} {body}\n", e.kind, e.volume);
                        written.push((body, sid));
                    }
                }
            }
        }
        out
    }
}

/// Every playable sound in the install (deduplicated by header id).
pub fn list_sounds(vfs: &Vfs) -> Vec<SoundRef> {
    SoundGraph::build(vfs).sounds()
}

struct VfsSoundFiles<'a>(&'a Vfs);
impl SoundFiles for VfsSoundFiles<'_> {
    fn sbao(&mut self, id: u32) -> Option<Vec<u8>> {
        self.0.read_hash(path_hash(&sbao_path(id))).ok()
    }
    fn spk(&mut self, id: u32) -> Option<Vec<u8>> {
        self.0.read_hash(path_hash(&spk_path(id))).ok()
    }
}

/// Decodes one sound to the bytes of a file Bevy can load: Ogg Vorbis
/// passthrough (`"ogg"`) or 16-bit PCM WAV (`"wav"`).
pub fn decode_to_file_bytes(vfs: &Vfs, sound: &SoundRef) -> Result<(Vec<u8>, &'static str)> {
    let d = decode_ref(vfs, sound)?;
    Ok(d.data.to_file_bytes())
}

/// Decodes one sound to PCM samples or Ogg bytes.
pub fn decode_ref(vfs: &Vfs, sound: &SoundRef) -> Result<DecodedSound> {
    let bytes = vfs.read(&sound.package)?;
    let baos = parse_container(&bytes, id_from_path(&sound.package).unwrap_or(0))?;
    let bao = baos
        .iter()
        .find(|b| b.id == sound.header_id && b.class == CLASS_HEADER)
        .with_context(|| format!("header {:08x} not in {}", sound.header_id, sound.package))?;
    let mut v = decode_header_layers(bao, &baos, &mut VfsSoundFiles(vfs), sound.layer.map(|l| l.0))?;
    if v.is_empty() {
        bail!("header {:08x} has no playable audio", sound.header_id);
    }
    Ok(v.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_interpolate_and_round_trip() {
        let c = Curve { kind: Curve::VOLUME, param: 0x0044_0256, points: vec![(0.0, -96.0), (1000.0, 0.0), (2000.0, -6.0)] };
        assert_eq!(c.at(-5.0), -96.0);
        assert_eq!(c.at(500.0), -48.0);
        assert_eq!(c.at(1500.0), -3.0);
        assert_eq!(c.at(9000.0), -6.0);
        assert_eq!(Curve::from_token(&c.to_token()), Some(c));
    }

    /// A BAO of `class` around `payload`.
    fn bao_bytes(class: u8, payload: &[u8]) -> Vec<u8> {
        let mut b = vec![2, 0x28, 0x03, 0x05];
        b.extend_from_slice(&0x28u32.to_le_bytes());
        b.extend_from_slice(&[0; 24]);
        b.extend_from_slice(&((class as u32) << 28).to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(payload);
        b
    }

    #[test]
    fn distance_curves_and_positioning() {
        let mut p = b"<Binary>".to_vec();
        p.resize(0x14, 0);
        p.extend_from_slice(&2u32.to_le_bytes());
        for (x, y) in [(0.0f32, -2.0f32), (200.0, -96.0)] {
            p.extend_from_slice(&x.to_le_bytes());
            p.extend_from_slice(&y.to_le_bytes());
            p.extend_from_slice(&[0; 8]);
        }
        let bytes = bao_bytes(CLASS_FALLOFF, &p);
        assert_eq!(parse_falloff(&Bao::parse(1, &bytes).unwrap()).unwrap(), vec![(0.0, -2.0), (200.0, -96.0)]);

        let mut p = b"<Binary>".to_vec();
        p.resize(0x11C, 0xFF);
        p[0xB0..0xB4].copy_from_slice(&1u32.to_le_bytes());
        p[0xC0..0xC4].copy_from_slice(&0x0045_6A81u32.to_le_bytes());
        let bytes = bao_bytes(CLASS_POSITIONING, &p);
        assert_eq!(parse_positioning(&Bao::parse(2, &bytes).unwrap()).unwrap(), Positioning { three_d: true, curve: Some(0x0045_6A81) });
        p[0xB0..0xB4].copy_from_slice(&0u32.to_le_bytes());
        p[0xC0..0xC4].copy_from_slice(&u32::MAX.to_le_bytes());
        let bytes = bao_bytes(CLASS_POSITIONING, &p);
        assert_eq!(parse_positioning(&Bao::parse(2, &bytes).unwrap()).unwrap(), Positioning { three_d: false, curve: None });
    }

    #[test]
    fn switched_ids_pack_above_the_event() {
        assert_eq!(switched_event(0x004E_4DC6, 0x0007_4CB1, 1), 0x684E_4DC6);
        assert_eq!(switched_event(0x004E_4DC6, 0, 3), 0xC04E_4DC6);
        assert_eq!(switched_event(0x004E_4DC6, 0x1234, 0), 0x004E_4DC6);
    }

    #[test]
    fn wav_header() {
        let w = pcm_to_wav(22050, 1, &[0, 1, -1]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 22050);
    }

    #[test]
    fn ids_from_paths() {
        assert_eq!(id_from_path("soundbinary\\001837ff.spk"), Some(0x001837ff));
        assert_eq!(id_from_path("sndres0x001837ff"), Some(0x001837ff));
        assert_eq!(spk_path(0x1837ff), "soundbinary\\001837ff.spk");
    }

    #[test]
    fn deinterleaves_two_layers() {
        // header block: version, config, 2 layers, 2 blocks, block header size,
        // sizes+headers size, first block size, 10 usable sizes, 2 header sizes
        let mut d = Vec::new();
        for v in [0x0010_0009u32, 0, 2, 2, 0x10, 8 + 3, 0x10 + 3] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&[0u8; 0x40]); // 0x1C..0x5C: per-layer usable sizes + reserved
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&1u32.to_le_bytes());
        d.extend_from_slice(b"AAB"); // layer headers: "AA" and "B"
        // block 1: marker, next block size, sizes (2, 1), data
        for v in [3u32, 0x10 + 2, 2, 1] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(b"aab");
        // block 2 (last): next = 0, sizes (1, 1)
        for v in [3u32, 0, 1, 1] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(b"ab");
        let l = deinterleave_layers(&d).unwrap();
        assert_eq!(l, vec![b"AAaaa".to_vec(), b"Bbb".to_vec()]);
    }

    #[test]
    fn ima_expand_matches_reference() {
        let mut s = ImaState { hist: 0, index: 0 };
        // nibble 7 at step 7: ((7*2+1)*7)>>3 = 13
        assert_eq!(s.expand(7), 13);
        assert_eq!(s.index, 8);
    }
}
