//! Facial animation: pose-weight clips (`.dpax`), facial pose definitions
//! (`.dpdx`, the face rig) and the table characters name their rig by
//! (`facialposedefinition.lib`).
//!
//! Faces are driven by poses, not by per-bone tracks. A `.dpdx` holds one
//! character's face bones: each bone's rest transform and, for every pose that
//! moves it, a rotation and a translation at full weight. A `.dpax` holds one
//! weight curve (0..=1) per pose, each named by the pose's hash. Every frame the
//! engine samples the playing clips into one weight per pose (layers add up),
//! then sets each rig bone's local transform to
//!
//! * rotation `rest ⊗ s₁ ⊗ s₂ ⊗ …`, where `sᵢ = slerp(identity, qᵢ, wᵢ)` and the
//!   poses are taken in index order;
//! * translation `rest + Σ wᵢ·tᵢ`.
//!
//! Rig bones are matched to skeleton bones and XBG nodes by name hash (CRC-32),
//! and the rig's rest transforms replace the model's bind transforms for them.
//!
//! Like `anim`, everything returned here is in Bevy space (see that module for
//! the conversion). The byte layouts are in `docs/animation.md`.

use anyhow::{bail, ensure, Context, Result};

use crate::anim::{mat_from_trs, quat_mul, quat_normalize, quat_to_bevy, vec_to_bevy, Mat4, Node, Quat, Vec3};
use crate::fcb::{self, Object};
use crate::hash::crc32;
use crate::reader::Reader;

/// The table of facial pose definitions that `CFacialPoseComponent` refers to.
pub const DEFINITION_LIBRARY: &str = r"generated\databases\generic\facialposedefinition.lib";

/// Archive path of the lip-sync clip the engine plays with a voice line: the
/// line's sound id as eight lower-case hex digits.
pub fn voice_clip_path(sound_id: u32) -> String {
    format!("graphics\\characters\\_common\\facial\\generated\\{sound_id:08x}.dpax")
}

const CLIP_VERSION: u16 = 2;
const RIG_VERSION: u16 = 1;
/// A stored weight of 255 is full weight.
const WEIGHT_STEP: f32 = 1.0 / 255.0;
/// Packed pose rotation components are `(v - 511) / 722`.
const ROTATION_STEP: f32 = 1.0 / 722.0;
/// Pose translations are `i16 * 4 / 32767` metres.
const TRANSLATION_STEP: f32 = 4.0 / 32767.0;
/// The engine skips poses whose weight is this small.
const MIN_WEIGHT: f32 = 2e-6;

// ===========================================================================
// .dpax: pose weight clips
// ===========================================================================

/// How one channel is stored in one block (two bits per channel in the block's mask).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    /// One byte per frame.
    Raw,
    /// No bytes: zero for the whole block.
    Zero,
    /// One byte for the whole block.
    Constant,
    /// Some frames stored; the runs left out between them are interpolated.
    Sparse,
}

impl ChannelKind {
    fn from_bits(bits: u8) -> ChannelKind {
        match bits & 3 {
            0 => ChannelKind::Raw,
            1 => ChannelKind::Zero,
            2 => ChannelKind::Constant,
            _ => ChannelKind::Sparse,
        }
    }
}

/// How one block of frames was stored, kept for inspection.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceBlock {
    pub first_frame: usize,
    pub frame_count: usize,
    /// One per channel.
    pub kinds: Vec<ChannelKind>,
    /// Block size in the file, header and mask included.
    pub bytes: usize,
}

/// A decoded `.dpax` clip: one weight curve per pose.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceClip {
    /// Frames per second.
    pub frame_rate: f32,
    /// Seconds; the last frame is at `duration * frame_rate`.
    pub duration: f32,
    pub frame_count: usize,
    /// The pose name hash each channel drives (looked up in `FaceRig::poses`).
    pub channels: Vec<u32>,
    /// Weights at each frame, channel-major: `weights[channel * frame_count + frame]`.
    /// Frames the file leaves out are already interpolated.
    pub weights: Vec<f32>,
    pub blocks: Vec<FaceBlock>,
}

/// Parses a `.dpax` clip.
pub fn parse_clip(data: &[u8]) -> Result<FaceClip> {
    let mut r = Reader::new(data);
    let version = r.u16()?;
    ensure!(r.bytes(2)? == b"Ap" && version == CLIP_VERSION, "not a facial clip (.dpax)");
    let size = r.u32()? as usize;
    ensure!(size <= data.len(), "clip claims {size} bytes, has {}", data.len());
    let info_at = r.u32()? as usize;
    let table_at = r.u32()? as usize;

    let mut r = Reader::at(data, info_at);
    let _self_offset = r.u32()?;
    let _info_size = r.u32()?;
    let hashes_at = r.u32()? as usize;
    let frame_rate = r.f32()?;
    let duration = r.f32()?;
    let frame_count = r.u16()? as usize;
    let channel_count = r.u16()? as usize;
    let block_frames = r.u8()? as usize;
    ensure!(block_frames > 0, "clip with zero frames per block");

    let mut r = Reader::at(data, hashes_at);
    let channels = (0..channel_count).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;

    let block_count = frame_count.div_ceil(block_frames);
    let mut weights = vec![0.0f32; channel_count * frame_count];
    let mut blocks = Vec::with_capacity(block_count);
    let mut column = vec![0.0f32; block_frames];
    for b in 0..block_count {
        let at = Reader::at(data, table_at + 4 * b).u32()? as usize;
        let mut r = Reader::at(data, at);
        let _self_offset = r.u32()?;
        let bytes = r.u32()? as usize;
        let mask_at = r.u32()? as usize;
        let data_at = r.u32()? as usize;
        let first_frame = r.u16()? as usize;
        let count = r.u16()? as usize;
        let end = at + bytes;
        ensure!(first_frame == b * block_frames, "block {b} starts at frame {first_frame}");
        ensure!(count >= 1 && count <= block_frames && first_frame + count <= frame_count, "block {b} has {count} frames");
        ensure!(end <= data.len() && mask_at + channel_count.div_ceil(4) <= data_at && data_at <= end, "block {b} runs past the file");
        let kinds: Vec<ChannelKind> = (0..channel_count).map(|c| ChannelKind::from_bits(data[mask_at + c / 4] >> (2 * (c % 4)))).collect();
        let body = &data[data_at..end];
        let mut pos = 0;
        for (c, kind) in kinds.iter().enumerate() {
            let column = &mut column[..count];
            let rest = body.get(pos..).unwrap_or(&[]);
            pos += match kind {
                ChannelKind::Raw => {
                    ensure!(rest.len() >= count, "block {b} channel {c}: raw frames past the block end");
                    column.iter_mut().zip(rest).for_each(|(o, &v)| *o = v as f32);
                    count
                }
                ChannelKind::Zero => {
                    column.fill(0.0);
                    0
                }
                ChannelKind::Constant => {
                    column.fill(*rest.first().with_context(|| format!("block {b} channel {c}: constant past the block end"))? as f32);
                    1
                }
                ChannelKind::Sparse => decode_sparse(rest, column).with_context(|| format!("block {b} channel {c}"))?,
            };
            let out = &mut weights[c * frame_count + first_frame..][..count];
            out.iter_mut().zip(column.iter()).for_each(|(o, &v)| *o = v * WEIGHT_STEP);
        }
        ensure!(pos == body.len(), "block {b}: channels use {pos} of {} bytes", body.len());
        blocks.push(FaceBlock { first_frame, frame_count: count, kinds, bytes });
    }
    Ok(FaceClip { frame_rate, duration, frame_count, channels, weights, blocks })
}

/// Decodes one sparse channel into `out` (one value per block frame, stored
/// units) and returns the bytes it used.
///
/// Layout: `u8 runs, u8 stored, (u8 after, u8 len) * runs, u8 values[stored]`.
/// Frame `after` is stored and the `len` frames following it are not; they are
/// interpolated linearly between frames `after` and `after + len + 1`. Every
/// other frame is stored, in order. The first frame of a block is always stored.
fn decode_sparse(d: &[u8], out: &mut [f32]) -> Result<usize> {
    let n = out.len();
    ensure!(d.len() >= 2, "sparse channel past the block end");
    let (runs, stored) = (d[0] as usize, d[1] as usize);
    let head = 2 + 2 * runs;
    ensure!(d.len() >= head + stored, "sparse channel past the block end");
    let gaps: Vec<(usize, usize)> = (0..runs).map(|i| (d[2 + 2 * i] as usize, d[3 + 2 * i] as usize)).collect();
    let mut skipped = vec![false; n];
    let mut earliest = 0;
    for &(after, len) in &gaps {
        ensure!(after >= earliest && after + len + 1 < n, "run after frame {after} ({len} frames) doesn't fit the block");
        skipped[after + 1..=after + len].fill(true);
        earliest = after + len + 1;
    }
    let kept = skipped.iter().filter(|s| !**s).count();
    ensure!(kept == stored, "sparse channel stores {stored} values for {kept} frames");
    let mut values = d[head..head + stored].iter();
    for (o, _) in out.iter_mut().zip(&skipped).filter(|(_, s)| !**s) {
        *o = *values.next().unwrap() as f32;
    }
    for &(after, len) in &gaps {
        let (a, b) = (out[after], out[after + len + 1]);
        for k in 1..=len {
            out[after + k] = a + (b - a) * k as f32 / (len + 1) as f32;
        }
    }
    Ok(head + stored)
}

impl FaceClip {
    /// Frame position at time `t`: the engine clamps `t` to the clip and
    /// multiplies by the frame rate.
    pub fn frame_at(&self, t: f32) -> f32 {
        (t.clamp(0.0, self.duration.max(0.0)) * self.frame_rate).clamp(0.0, self.frame_count.saturating_sub(1) as f32)
    }

    /// Weight of one channel at time `t`, interpolated linearly between frames.
    pub fn channel_weight(&self, channel: usize, t: f32) -> f32 {
        let n = self.frame_count;
        if n == 0 || channel >= self.channels.len() {
            return 0.0;
        }
        let f = self.frame_at(t);
        let f0 = f.floor() as usize;
        let f1 = (f0 + 1).min(n - 1);
        let curve = &self.weights[channel * n..][..n];
        curve[f0] + (curve[f1] - curve[f0]) * (f - f0 as f32)
    }

    /// All channel weights at time `t`, in channel order.
    pub fn sample(&self, t: f32) -> Vec<f32> {
        (0..self.channels.len()).map(|c| self.channel_weight(c, t)).collect()
    }

    /// Adds this clip's weights at `t`, scaled by `blend`, to a rig's pose
    /// weights. `map` comes from [`FaceRig::channel_map`]. Channels the rig
    /// lacks are skipped. Several clips played at once add up, as in the engine.
    pub fn add_weights(&self, t: f32, blend: f32, map: &[Option<usize>], weights: &mut [f32]) {
        if blend == 0.0 {
            return;
        }
        for (c, pose) in map.iter().enumerate() {
            if let Some(w) = pose.and_then(|p| weights.get_mut(p)) {
                *w += self.channel_weight(c, t) * blend;
            }
        }
    }
}

// ===========================================================================
// .dpdx: the face rig
// ===========================================================================

/// What one pose does to one bone at full weight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BonePose {
    /// Index into [`FaceRig::poses`].
    pub pose: u16,
    /// Applied after the rest rotation, in the bone's own frame.
    pub rotation: Quat,
    /// Added to the rest translation (parent space, metres).
    pub translation: Vec3,
}

/// One face bone of a rig.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceBone {
    /// CRC-32 of the bone name, as in skeletons and XBG nodes.
    pub name_hash: u32,
    /// Rest transform, local to the parent bone.
    pub rotation: Quat,
    pub translation: Vec3,
    /// The poses that move this bone, in ascending pose order.
    pub poses: Vec<BonePose>,
}

/// A decoded `.dpdx` facial pose definition.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceRig {
    /// Pose name hashes; a pose's position here is its index.
    pub poses: Vec<u32>,
    pub bones: Vec<FaceBone>,
}

/// Unpacks a pose rotation (Dunia space, `[x, y, z, w]`).
///
/// Three 10-bit fields (bits 0-9, 10-19, 20-29) hold `(v - 511) / 722`. The top
/// two bits pick the component left out, which is `sqrt(1 - a² - b² - c²)`: 0
/// leaves out `w`, 1 `x`, 2 `y` and 3 `z`. The three fields are the components
/// that follow it in `x, y, z, w` order, wrapping around.
pub fn unpack_pose_rotation(packed: u32) -> Quat {
    let field = |i: u32| (((packed >> (10 * i)) & 0x3FF) as i32 - 511) as f32 * ROTATION_STEP;
    let (a, b, c) = (field(0), field(1), field(2));
    let implicit = ((packed >> 30) as usize + 3) % 4;
    let mut q = [0.0; 4];
    q[implicit] = (1.0 - a * a - b * b - c * c).max(0.0).sqrt();
    q[(implicit + 1) % 4] = a;
    q[(implicit + 2) % 4] = b;
    q[(implicit + 3) % 4] = c;
    q
}

/// Parses a `.dpdx` facial pose definition.
pub fn parse_rig(data: &[u8]) -> Result<FaceRig> {
    let mut r = Reader::new(data);
    let version = r.u16()?;
    ensure!(r.bytes(2)? == b"Dp" && version == RIG_VERSION, "not a facial pose definition (.dpdx)");
    let _size = r.u32()?;
    let bones_at = r.u32()? as usize;
    let _entries_at = r.u32()?;
    let poses_at = r.u32()? as usize;
    let bone_count = r.u16()? as usize;
    let _entry_count = r.u16()?;
    let pose_count = r.u32()? as usize;

    let mut r = Reader::at(data, poses_at);
    let poses = (0..pose_count).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
    let mut bones = Vec::with_capacity(bone_count);
    for i in 0..bone_count {
        let mut r = Reader::at(data, bones_at + 48 * i);
        let rotation = quat_to_bevy(r.vec4()?);
        let translation = vec_to_bevy(r.vec3()?);
        let name_hash = r.u32()?;
        let entries_at = r.u32()? as usize;
        let count = r.u16()? as usize;
        let mut r = Reader::at(data, entries_at);
        let mut bone_poses = Vec::with_capacity(count);
        for _ in 0..count {
            let packed = r.u32()?;
            let t = [r.i16()?, r.i16()?, r.i16()?];
            let pose = r.u16()?;
            if pose as usize >= pose_count {
                bail!("bone {i} refers to pose {pose} of {pose_count}");
            }
            bone_poses.push(BonePose {
                pose,
                rotation: quat_to_bevy(unpack_pose_rotation(packed)),
                translation: vec_to_bevy(t.map(|v| v as f32 * TRANSLATION_STEP)),
            });
        }
        bones.push(FaceBone { name_hash, rotation, translation, poses: bone_poses });
    }
    Ok(FaceRig { poses, bones })
}

/// `slerp(identity, q, w)` along the shorter arc, computed the way the engine
/// does it (from the angle between `q` and identity), then normalised.
fn partial_rotation(q: Quat, w: f32) -> Quat {
    let d = q[3];
    if d <= -1.0 || d >= 1.0 {
        return q;
    }
    let angle = d.abs().acos();
    if angle <= 1e-6 {
        return q;
    }
    let s = angle.sin();
    let keep = ((1.0 - w) * angle).sin() / s;
    let take = (w * angle).sin() / s * if d < 0.0 { -1.0 } else { 1.0 };
    quat_normalize([q[0] * take, q[1] * take, q[2] * take, keep + q[3] * take])
}

impl FaceRig {
    /// Index of the pose with this name hash.
    pub fn pose_index(&self, hash: u32) -> Option<usize> {
        self.poses.iter().position(|&p| p == hash)
    }

    /// The rig pose each channel of `clip` drives (`None`: this rig lacks it).
    pub fn channel_map(&self, clip: &FaceClip) -> Vec<Option<usize>> {
        clip.channels.iter().map(|&h| self.pose_index(h)).collect()
    }

    /// All pose weights at zero (the face at rest).
    pub fn zero_weights(&self) -> Vec<f32> {
        vec![0.0; self.poses.len()]
    }

    /// Pose weights of a single clip at time `t`.
    pub fn clip_weights(&self, clip: &FaceClip, t: f32) -> Vec<f32> {
        let mut w = self.zero_weights();
        clip.add_weights(t, 1.0, &self.channel_map(clip), &mut w);
        w
    }

    /// Local `(translation, rotation)` of bone `bone` for these pose weights.
    pub fn bone_local(&self, bone: usize, weights: &[f32]) -> (Vec3, Quat) {
        let b = &self.bones[bone];
        let mut rotation = b.rotation;
        let mut translation = b.translation;
        for p in &b.poses {
            let w = weights.get(p.pose as usize).copied().unwrap_or(0.0);
            if w.abs() <= MIN_WEIGHT {
                continue;
            }
            for (t, d) in translation.iter_mut().zip(p.translation) {
                *t += d * w;
            }
            rotation = quat_mul(rotation, partial_rotation(p.rotation, w));
        }
        (translation, quat_normalize(rotation))
    }

    /// Sets the local matrices of the `nodes` that are bones of this rig
    /// (matched by name hash) to the posed face transforms, keeping each node's
    /// scale. Other nodes are left alone. `locals` is what
    /// `anim::pose_locals` returns, so the face goes on top of a body pose.
    pub fn apply(&self, nodes: &[Node], weights: &[f32], locals: &mut [Mat4]) {
        for (bi, bone) in self.bones.iter().enumerate() {
            let Some(ni) = nodes.iter().position(|n| n.name_hash == bone.name_hash) else { continue };
            let (t, q) = self.bone_local(bi, weights);
            locals[ni] = mat_from_trs(t, q, nodes[ni].scale);
        }
    }
}

// ===========================================================================
// Which rig a character uses
// ===========================================================================

/// One entry of [`DEFINITION_LIBRARY`].
#[derive(Clone, Debug, PartialEq)]
pub struct FaceDefinition {
    /// CRC-32 of `name`; what `CFacialPoseComponent` refers to.
    pub key: u32,
    pub name: String,
    /// Archive path of the `.dpdx`.
    pub rig: String,
}

/// The definition's `.dpdx` path, as text (the field's name isn't known).
const RIG_PATH_FIELD: u32 = 0xCC97_9B17;

/// Parses the facial pose definition table ([`DEFINITION_LIBRARY`], an FCB file).
pub fn parse_definitions(bytes: &[u8]) -> Result<Vec<FaceDefinition>> {
    let start = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
    let root = fcb::parse(&bytes[start..])?;
    let mut out = Vec::new();
    for e in &root.children {
        let Some(name) = e.str("hidName") else { continue };
        let key = e.u64("hidKey").map(|k| k as u32).unwrap_or_else(|| crc32(name.as_bytes()));
        let rig = e.str_h(RIG_PATH_FIELD).unwrap_or_default();
        out.push(FaceDefinition { key, name, rig });
    }
    Ok(out)
}

/// The rig choice in a character's `CFacialPoseComponent`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FaceChoice {
    /// Definition key used when no head-specific entry applies.
    pub default: Option<u32>,
    /// `(CRC-32 of a head tag, definition key)`. Graphic kit head parts carry
    /// tags such as `head02`, so a kit with several heads gets the rig that fits
    /// the head it picked.
    pub by_head: Vec<(u32, u32)>,
}

impl FaceChoice {
    /// Reads a `CFacialPoseComponent` object.
    pub fn from_component(c: &Object) -> FaceChoice {
        let key = |o: &Object, field: &str| o.u64(field).map(|k| k as u32).filter(|&k| k != u32::MAX);
        let by_head = c
            .child("MercKitFacialPoseDefinitions")
            .map(|list| {
                list.children_named("FacialPoseDefinition")
                    .filter_map(|d| Some((d.u32("sHeadTag").filter(|&t| t != u32::MAX)?, key(d.as_ref(), "facialposedefinitionFacialDefinition")?)))
                    .collect()
            })
            .unwrap_or_default();
        FaceChoice { default: key(c, "facialposedefinitionFacialPoseDefinition"), by_head }
    }

    /// The definition key for a character whose head part carries these kit
    /// tags (any case), falling back to the default.
    pub fn definition(&self, head_tags: &[&str]) -> Option<u32> {
        head_tags
            .iter()
            .find_map(|t| {
                let h = crc32(t.to_ascii_lowercase().as_bytes());
                self.by_head.iter().find(|(tag, _)| *tag == h).map(|&(_, key)| key)
            })
            .or(self.default)
    }
}

// ===========================================================================
// The cache's copy
// ===========================================================================

/// Every face rig and facial clip, packed by archive path hash.
pub const PAK_REL: &str = "facial/faces.pak";
/// Definition key -> the path hash of its rig, one `key hash` (hex) per line.
pub const DEFINITIONS_REL: &str = "facial/definitions.txt";

const PAK_MAGIC: &[u8; 4] = b"FPAK";
const PAK_ENTRY: usize = 16;

/// Packs files by path hash: `"FPAK"`, `u32` count, the entries sorted by
/// hash (`u64` hash, `u32` offset, `u32` length), then the files.
pub fn write_pak(files: &mut [(u64, Vec<u8>)]) -> Vec<u8> {
    files.sort_by_key(|f| f.0);
    let head = 8 + files.len() * PAK_ENTRY;
    let mut out = Vec::with_capacity(head + files.iter().map(|f| f.1.len()).sum::<usize>());
    out.extend_from_slice(PAK_MAGIC);
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    let mut at = head as u32;
    for (hash, bytes) in files.iter() {
        out.extend_from_slice(&hash.to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        at += bytes.len() as u32;
    }
    for (_, bytes) in files.iter() {
        out.extend_from_slice(bytes);
    }
    out
}

/// A pack's table: where each file is.
#[derive(Clone, Debug, Default)]
pub struct PakIndex {
    entries: Vec<(u64, u32, u32)>,
}

impl PakIndex {
    /// Bytes of the header to read for `count` (from its first 8 bytes).
    pub fn header_len(first8: &[u8]) -> Result<usize> {
        ensure!(first8.len() >= 8 && &first8[..4] == PAK_MAGIC, "not a facial pack");
        Ok(8 + u32::from_le_bytes(first8[4..8].try_into().unwrap()) as usize * PAK_ENTRY)
    }

    pub fn parse(header: &[u8]) -> Result<PakIndex> {
        let len = Self::header_len(header)?;
        ensure!(header.len() >= len, "facial pack header cut short");
        let entries = header[8..len]
            .chunks_exact(PAK_ENTRY)
            .map(|e| {
                let u = |i: usize| u32::from_le_bytes(e[i..i + 4].try_into().unwrap());
                (u64::from_le_bytes(e[..8].try_into().unwrap()), u(8), u(12))
            })
            .collect();
        Ok(PakIndex { entries })
    }

    /// (offset, length) of a file by path hash.
    pub fn find(&self, hash: u64) -> Option<(u32, u32)> {
        let i = self.entries.binary_search_by_key(&hash, |e| e.0).ok()?;
        Some((self.entries[i].1, self.entries[i].2))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Graphic kit id -> the face definitions its characters use: `kit default
/// [head tag CRC:key...]` (hex; the default `-` when none) per line.
pub const KIT_FACES_REL: &str = "facial/kit_faces.txt";
/// The table's first line.
pub const KIT_FACES_HEADER: &str = "# kit faces 2";

/// Which face definitions each graphic kit's characters use (by the tags of
/// the head they wear): the entity library's archetypes that carry both a
/// `CGraphicKitComponent` and a `CFacialPoseComponent` (the first archetype
/// of a kit decides).
pub fn kit_faces(library: &Object) -> Vec<(u64, FaceChoice)> {
    let mut out: Vec<(u64, FaceChoice)> = Vec::new();
    library.walk(&mut |o, _| {
        let comps = o.child("Components").map(|c| c.as_ref()).unwrap_or(o);
        let Some(kit) = comps.child("CGraphicKitComponent").and_then(|c| c.u64("kitidGraphicKit")).filter(|&k| k != 0 && k != u64::MAX) else { return };
        let Some(choice) = comps.child("CFacialPoseComponent").map(|c| FaceChoice::from_component(c)) else { return };
        if (choice.default.is_some() || !choice.by_head.is_empty()) && !out.iter().any(|(k, _)| *k == kit) {
            out.push((kit, choice));
        }
    });
    out.sort_by_key(|(k, _)| *k);
    out
}

pub fn kit_faces_to_text(table: &[(u64, FaceChoice)]) -> String {
    let mut s = format!("{KIT_FACES_HEADER}\n");
    for (k, c) in table {
        s += &format!("{k:016x} {}", c.default.map_or("-".to_string(), |d| format!("{d:08x}")));
        for (tag, key) in &c.by_head {
            s += &format!(" {tag:08x}:{key:08x}");
        }
        s.push('\n');
    }
    s
}

pub fn kit_faces_from_text(text: &str) -> std::collections::HashMap<u64, FaceChoice> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let kit = u64::from_str_radix(it.next()?, 16).ok()?;
            let default = it.next().and_then(|d| u32::from_str_radix(d, 16).ok());
            let by_head = it
                .filter_map(|p| {
                    let (t, k) = p.split_once(':')?;
                    Some((u32::from_str_radix(t, 16).ok()?, u32::from_str_radix(k, 16).ok()?))
                })
                .collect();
            Some((kit, FaceChoice { default, by_head }))
        })
        .collect()
}

/// The definitions table in the cache's text form.
pub fn definitions_to_text(defs: &[(u32, u64)]) -> String {
    defs.iter().map(|(k, h)| format!("{k:08x} {h:016x}\n")).collect()
}

pub fn definitions_from_text(text: &str) -> std::collections::HashMap<u32, u64> {
    text.lines()
        .filter_map(|l| {
            let (k, h) = l.split_once(' ')?;
            Some((u32::from_str_radix(k, 16).ok()?, u64::from_str_radix(h.trim(), 16).ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pak_round_trip() {
        let mut files = vec![(9u64, vec![1, 2, 3]), (4u64, vec![7])];
        let pak = write_pak(&mut files);
        let index = PakIndex::parse(&pak).unwrap();
        assert_eq!(PakIndex::header_len(&pak[..8]).unwrap(), 8 + 2 * 16);
        let (at, len) = index.find(9).unwrap();
        assert_eq!(&pak[at as usize..(at + len) as usize], &[1, 2, 3]);
        let (at, len) = index.find(4).unwrap();
        assert_eq!(&pak[at as usize..(at + len) as usize], &[7]);
        assert!(index.find(5).is_none());
        let defs = definitions_from_text(&definitions_to_text(&[(0xABCD, 0x1234_5678_9ABC)]));
        assert_eq!(defs[&0xABCD], 0x1234_5678_9ABC);
    }
    use crate::anim::{quat_slerp, QUAT_IDENTITY};
    use std::sync::Arc;

    /// Builds a `.dpax`: per block, the 2-bit kinds and the channel bytes.
    fn clip_bytes(rate: f32, frames: u16, block_frames: u8, hashes: &[u32], blocks: &[(&[u8], &[u8])]) -> Vec<u8> {
        let n = hashes.len();
        let info = 0x10;
        let hashes_at = info + 0x1C;
        let table = hashes_at + 4 * n;
        let mut d = vec![0u8; table + 4 * blocks.len()];
        let put = |d: &mut Vec<u8>, at: usize, b: &[u8]| d[at..at + b.len()].copy_from_slice(b);
        put(&mut d, 0, &2u16.to_le_bytes());
        put(&mut d, 2, b"Ap");
        put(&mut d, 8, &(info as u32).to_le_bytes());
        put(&mut d, 12, &(table as u32).to_le_bytes());
        put(&mut d, info, &(info as u32).to_le_bytes());
        put(&mut d, info + 4, &((table - info) as u32).to_le_bytes());
        put(&mut d, info + 8, &(hashes_at as u32).to_le_bytes());
        put(&mut d, info + 12, &rate.to_le_bytes());
        put(&mut d, info + 16, &((frames - 1) as f32 / rate).to_le_bytes());
        put(&mut d, info + 20, &frames.to_le_bytes());
        put(&mut d, info + 22, &(n as u16).to_le_bytes());
        d[info + 24] = block_frames;
        for (i, h) in hashes.iter().enumerate() {
            put(&mut d, hashes_at + 4 * i, &h.to_le_bytes());
        }
        for (b, (kinds, body)) in blocks.iter().enumerate() {
            while d.len() % 4 != 0 {
                d.push(0);
            }
            let at = d.len();
            put(&mut d, table + 4 * b, &(at as u32).to_le_bytes());
            let mut mask = vec![0u8; n.div_ceil(4)];
            for (c, k) in kinds.iter().enumerate() {
                mask[c / 4] |= k << (2 * (c % 4));
            }
            let first = b * block_frames as usize;
            let count = (frames as usize - first).min(block_frames as usize) as u16;
            let mask_at = at + 20;
            let data_at = mask_at + mask.len();
            let size = data_at + body.len() - at;
            for v in [at as u32, size as u32, mask_at as u32, data_at as u32] {
                d.extend_from_slice(&v.to_le_bytes());
            }
            d.extend_from_slice(&(first as u16).to_le_bytes());
            d.extend_from_slice(&count.to_le_bytes());
            d.extend_from_slice(&mask);
            d.extend_from_slice(body);
        }
        d.extend_from_slice(&[0; 4]);
        let len = d.len() as u32;
        put(&mut d, 4, &len.to_le_bytes());
        d
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn engine_constants() {
        // The values the engine multiplies by, bit for bit.
        assert_eq!(ROTATION_STEP.to_bits(), 0x3AB5_8A48);
        assert_eq!(TRANSLATION_STEP.to_bits(), 0x3900_0100);
        assert_eq!(WEIGHT_STEP.to_bits(), 0x3B80_8081);
    }

    #[test]
    fn decodes_every_channel_kind() {
        // Two blocks of four frames plus one of two (ten frames).
        // Channels: raw, zero, constant, sparse.
        let block0: &[u8] = &[10, 20, 30, 40, /* constant */ 99, /* sparse: 1 run after frame 0, 2 frames */ 1, 2, 0, 2, 0, 255];
        let block1: &[u8] = &[50, 60, 70, 80, 7, /* sparse, no runs */ 0, 4, 1, 2, 3, 4];
        let block2: &[u8] = &[90, 100, 255];
        let d = clip_bytes(30.0, 10, 4, &[0xA, 0xB, 0xC, 0xD], &[(&[0, 1, 2, 3], block0), (&[0, 1, 2, 3], block1), (&[0, 1, 1, 2], block2)]);
        let c = parse_clip(&d).unwrap();
        assert_eq!((c.frame_count, c.channels.len(), c.blocks.len()), (10, 4, 3));
        assert_eq!(c.blocks[2].kinds, vec![ChannelKind::Raw, ChannelKind::Zero, ChannelKind::Zero, ChannelKind::Constant]);
        let at = |ch: usize, f: usize| c.weights[ch * 10 + f] * 255.0;
        let raw: Vec<f32> = (0..10).map(|f| at(0, f)).collect();
        assert!(raw.iter().zip([10, 20, 30, 40, 50, 60, 70, 80, 90, 100]).all(|(&a, b)| close(a, b as f32)));
        assert!((0..10).all(|f| at(1, f) == 0.0));
        assert!((0..4).all(|f| close(at(2, f), 99.0)) && (4..8).all(|f| close(at(2, f), 7.0)) && (8..10).all(|f| at(2, f) == 0.0));
        // Sparse: frames 0 and 3 stored (0, 255), frames 1 and 2 interpolated.
        assert!(close(at(3, 0), 0.0) && close(at(3, 1), 85.0) && close(at(3, 2), 170.0) && close(at(3, 3), 255.0));
        assert!(close(at(3, 4), 1.0) && close(at(3, 7), 4.0) && close(at(3, 8), 255.0));
    }

    #[test]
    fn samples_between_frames_and_across_blocks() {
        let d = clip_bytes(10.0, 6, 4, &[1], &[(&[0], &[0, 51, 102, 153]), (&[0], &[255, 255])]);
        let c = parse_clip(&d).unwrap();
        assert!(close(c.duration, 0.5));
        assert!(close(c.channel_weight(0, 0.0), 0.0));
        assert!(close(c.channel_weight(0, 0.15), 0.3)); // halfway between 0.2 and 0.4
        assert!(close(c.channel_weight(0, 0.35), 0.8)); // last frame of block 0 to first of block 1
        assert!(close(c.channel_weight(0, 9.0), 1.0)); // clamped to the end
        assert!(close(c.channel_weight(0, -1.0), 0.0));
        let mut w = vec![0.25, 0.0];
        c.add_weights(0.1, 0.5, &[Some(1)], &mut w);
        assert!(close(w[0], 0.25) && close(w[1], 0.1));
    }

    #[test]
    fn rejects_inconsistent_clips() {
        assert!(parse_clip(b"not a clip at all, not even close......").is_err());
        // Sparse channel claiming three stored values for a four-frame block with a two-frame run.
        let d = clip_bytes(30.0, 4, 4, &[1], &[(&[3], &[1, 3, 0, 2, 0, 0, 0])]);
        assert!(parse_clip(&d).is_err());
        // A run that leaves no stored frame after it.
        let d = clip_bytes(30.0, 4, 4, &[1], &[(&[3], &[1, 1, 0, 3, 0])]);
        assert!(parse_clip(&d).is_err());
        // Leftover bytes.
        let d = clip_bytes(30.0, 2, 4, &[1], &[(&[2], &[5, 6])]);
        assert!(parse_clip(&d).is_err());
    }

    #[test]
    fn unpacks_pose_rotations() {
        let pack = |top: u32, a: u32, b: u32, c: u32| top << 30 | c << 20 | b << 10 | a;
        assert_eq!(unpack_pose_rotation(pack(0, 511, 511, 511)), [0.0, 0.0, 0.0, 1.0]);
        let q = unpack_pose_rotation(pack(0, 511 + 361, 511, 511));
        assert!(close(q[0], 0.5) && close(q[3], 0.75f32.sqrt()));
        // Top bits 1: x left out, fields are y, z, w.
        let q = unpack_pose_rotation(pack(1, 511, 511, 511 + 361));
        assert!(close(q[3], 0.5) && close(q[0], 0.75f32.sqrt()) && q[1] == 0.0);
        // Top bits 3: z left out, fields are w, x, y.
        let q = unpack_pose_rotation(pack(3, 511 - 361, 511, 511));
        assert!(close(q[3], -0.5) && close(q[2], 0.75f32.sqrt()));
    }

    /// One-bone rig: rest at the origin, pose 0 turns 90° about Dunia Z and moves
    /// 4/32767 * 100 m along Dunia X, pose 1 isn't used by the bone.
    fn rig_bytes() -> Vec<u8> {
        let mut d = vec![0u8; 0x20];
        d[0..2].copy_from_slice(&1u16.to_le_bytes());
        d[2..4].copy_from_slice(b"Dp");
        let (bones, entries, poses) = (0x20u32, 0x50u32, 0x5Cu32);
        for (i, v) in [bones, entries, poses].iter().enumerate() {
            d[8 + 4 * i..12 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        d[0x14..0x16].copy_from_slice(&1u16.to_le_bytes());
        d[0x16..0x18].copy_from_slice(&1u16.to_le_bytes());
        d[0x18..0x1C].copy_from_slice(&2u32.to_le_bytes());
        for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.1] {
            d.extend_from_slice(&f.to_le_bytes());
        }
        d.extend_from_slice(&0xBEEFu32.to_le_bytes());
        d.extend_from_slice(&entries.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&[0; 10]);
        // sin 45° = 0.7071 -> 511 + 510.5; use 1021 (the largest value the encoder writes).
        let packed = (511 + 510u32) << 20 | 511 << 10 | 511;
        d.extend_from_slice(&packed.to_le_bytes());
        for v in [100i16, 0, 0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&0u16.to_le_bytes());
        d.extend_from_slice(&0x1111u32.to_le_bytes());
        d.extend_from_slice(&0x2222u32.to_le_bytes());
        let len = d.len() as u32;
        d[4..8].copy_from_slice(&len.to_le_bytes());
        d
    }

    #[test]
    fn poses_a_bone() {
        let rig = parse_rig(&rig_bytes()).unwrap();
        assert_eq!(rig.poses, vec![0x1111, 0x2222]);
        assert_eq!(rig.bones.len(), 1);
        let b = &rig.bones[0];
        assert_eq!((b.name_hash, b.poses.len()), (0xBEEF, 1));
        // Dunia (0, 0, 0.1) is Bevy (0, 0.1, 0).
        assert!(close(b.translation[1], 0.1) && close(b.translation[2], 0.0));
        let step = 400.0 / 32767.0;

        let (t, q) = rig.bone_local(0, &[0.0, 1.0]);
        assert!(close(t[0], 0.0) && close(t[1], 0.1));
        assert_eq!(q, quat_normalize(b.rotation));

        let (t, q) = rig.bone_local(0, &[1.0, 0.0]);
        assert!(close(t[0], step) && close(t[1], 0.1));
        let full = quat_normalize(b.poses[0].rotation);
        assert!((0..4).all(|k| close(q[k], full[k])));
        // Dunia Z is Bevy Y: about 90° around +Y.
        assert!((q[1] - q[3]).abs() < 0.01 && q[1] > 0.7 && q[0] == 0.0 && q[2] == 0.0);

        let (t, q) = rig.bone_local(0, &[0.5, 0.0]);
        assert!(close(t[0], step * 0.5));
        let half = quat_slerp(QUAT_IDENTITY, full, 0.5);
        assert!((0..4).all(|k| (q[k] - half[k]).abs() < 1e-4));

        // Weights this small are skipped.
        assert_eq!(rig.bone_local(0, &[1e-6, 0.0]).0, b.translation);
    }

    #[test]
    fn partial_rotation_takes_the_short_way() {
        let q = [0.0, 0.0, 0.8f32.sqrt(), -0.2f32.sqrt()];
        let p = partial_rotation(q, 0.5);
        let expect = quat_slerp(QUAT_IDENTITY, q, 0.5);
        assert!((0..4).all(|k| (p[k] - expect[k]).abs() < 1e-5));
        assert_eq!(partial_rotation(q, 1.0).map(|v| (v * 1e4).round()), [-q[0], -q[1], -q[2], -q[3]].map(|v| (v * 1e4).round()));
    }

    #[test]
    fn kit_faces_round_trip() {
        let heads = FaceChoice { default: Some(0x1234_5678), by_head: vec![(crc32(b"head02"), 0xBA2A_1067)] };
        let table = vec![(0xA773_2017_0000_0200, heads.clone()), (7, FaceChoice { default: Some(0xFFFF_0000), by_head: vec![] })];
        let back = kit_faces_from_text(&kit_faces_to_text(&table));
        assert_eq!(back.len(), 2);
        assert_eq!(back[&0xA773_2017_0000_0200], heads);
        assert_eq!(back[&7].default, Some(0xFFFF_0000));
        // A villager wearing the second head takes its rig; any other, the default.
        assert_eq!(back[&0xA773_2017_0000_0200].definition(&["head02"]), Some(0xBA2A_1067));
        assert_eq!(back[&0xA773_2017_0000_0200].definition(&["head01"]), Some(0x1234_5678));
    }

    #[test]
    fn maps_channels_and_applies_to_nodes() {
        let rig = parse_rig(&rig_bytes()).unwrap();
        let d = clip_bytes(30.0, 2, 64, &[0x9999, 0x1111], &[(&[2, 2], &[255, 255])]);
        let clip = parse_clip(&d).unwrap();
        assert_eq!(rig.channel_map(&clip), vec![None, Some(0)]);
        assert_eq!(rig.clip_weights(&clip, 0.0), vec![1.0, 0.0]);
        let node = |name_hash, parent| Node {
            name: String::new(),
            name_hash,
            parent,
            first_child: -1,
            next_sibling: -1,
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            bind_index: -1,
        };
        let nodes = [node(1, -1), node(0xBEEF, 0)];
        let mut locals = vec![crate::anim::MAT4_IDENTITY; 2];
        rig.apply(&nodes, &[1.0, 0.0], &mut locals);
        assert_eq!(locals[0], crate::anim::MAT4_IDENTITY);
        assert!(close(locals[1][13], 0.1) && close(locals[1][12], 400.0 / 32767.0));
    }

    #[test]
    fn picks_the_rig_for_a_head() {
        let field = |name: &str, v: Vec<u8>| (crate::hash::name_hash(name), v);
        let entry = |tag: u32, key: u64| {
            Arc::new(Object {
                name_hash: crate::hash::name_hash("FacialPoseDefinition"),
                fields: vec![field("sHeadTag", tag.to_le_bytes().to_vec()), field("facialposedefinitionFacialDefinition", key.to_le_bytes().to_vec())],
                children: vec![],
            })
        };
        let list = Arc::new(Object {
            name_hash: crate::hash::name_hash("MercKitFacialPoseDefinitions"),
            fields: vec![],
            children: vec![entry(crc32(b"head02"), 0x22), entry(u32::MAX, u32::MAX as u64)],
        });
        let comp = Object {
            name_hash: crate::hash::name_hash("CFacialPoseComponent"),
            fields: vec![field("facialposedefinitionFacialPoseDefinition", 0x11u64.to_le_bytes().to_vec())],
            children: vec![list],
        };
        let choice = FaceChoice::from_component(&comp);
        assert_eq!(choice, FaceChoice { default: Some(0x11), by_head: vec![(crc32(b"head02"), 0x22)] });
        assert_eq!(choice.definition(&["Head02"]), Some(0x22));
        assert_eq!(choice.definition(&["head01"]), Some(0x11));
        assert_eq!(choice.definition(&[]), Some(0x11));
    }

    #[test]
    fn voice_clip_paths() {
        assert_eq!(voice_clip_path(0x001B_82D9), "graphics\\characters\\_common\\facial\\generated\\001b82d9.dpax");
    }
}
