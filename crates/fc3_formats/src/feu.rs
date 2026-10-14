//! The game's interface movies (`.feu`): Flash movies (SWF 8, ActionScript
//! 2) with the signature `UEF` where SWF has `FWS`. Their bitmaps and fonts
//! live outside the file, named by the game's own tags: an external image
//! (246: id, the authored width and height, the texture's path as
//! `UI\...\TX_name.png`, shipped as `.xbt`) and an external font (251: id,
//! the `.ffd` font file, its face name, per-language variants). Tags 248 and
//! 249 carry the movie's id and a version.
//!
//! [`to_swf`] makes a standard SWF of one: the signature put back, each
//! external image embedded as a lossless bitmap, each external font as a
//! `DefineFont3` (traced by `crate::ffd`), the game's other tags left out, and
//! its calls on the engine (Flash Lite's `FSCommand2`) made calls of the
//! global function [`ENGINE_CALL`] (`crate::avm1`), so any Flash player can
//! run it.
//!
//! Many images are packed into shared textures (`UI\SuperTextures\...`,
//! "atlases"). Such an image keeps its own authored size, and the shapes
//! that fill with it reach the part of the atlas that is theirs through the
//! fill's matrix: the engine stretches the whole atlas over the image's
//! authored size. A Flash player can't, so the atlas is embedded once, at
//! its own size, and those fills are pointed at it with their matrices
//! scaled from the authored size to the atlas's.

use anyhow::{bail, Context, Result};
use std::io::Write;

pub const EXTERNAL_IMAGE: u16 = 246;
pub const MOVIE_ID: u16 = 248;
pub const MOVIE_VERSION: u16 = 249;
pub const EXTERNAL_FONT: u16 = 251;
const END: u16 = 0;
const DEFINE_SPRITE: u16 = 39;
const DEFINE_SHAPE: u16 = 2;
const DEFINE_SHAPE2: u16 = 22;
const DEFINE_SHAPE3: u16 = 32;
const DEFINE_SHAPE4: u16 = 83;
const DEFINE_BITS_LOSSLESS2: u16 = 36;
const DEFINE_FONT3: u16 = 75;
const DO_ACTION: u16 = 12;
const DO_INIT_ACTION: u16 = 59;
const EXPORT_ASSETS: u16 = 56;
const IMPORT_ASSETS: u16 = 57;
const IMPORT_ASSETS2: u16 = 71;
/// The first id of the copies of a movie's exported sprites (see
/// `export_copies`): above every movie's own (the most any has is ~1000).
const EXPORT_COPY_BASE: u16 = 0x8000;

/// The global function a converted movie calls the engine through, with the
/// command and its arguments (what `FSCommand2` took): the host defines it.
pub const ENGINE_CALL: &str = "__fireEngineCall";

/// A bitmap a movie takes from the game's textures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalImage {
    pub id: u16,
    pub width: u16,
    pub height: u16,
    /// As the movie names it (`UI\...\TX_name.png`).
    pub path: String,
}

impl ExternalImage {
    /// Whether its texture is shared with other images (an atlas).
    pub fn in_atlas(&self) -> bool {
        self.path.to_ascii_lowercase().contains("supertextures")
    }

    /// The archive path of its texture (`ui\...\tx_name.xbt`).
    pub fn texture(&self) -> String {
        let lower = self.path.to_ascii_lowercase();
        let stem = lower.rsplit_once('.').map_or(lower.as_str(), |(s, _)| s);
        format!("{stem}.xbt")
    }
}

/// A font a movie takes from the game's font files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalFont {
    pub id: u16,
    /// The `.ffd` file (`UI\Common\fonts\...`).
    pub file: String,
    pub face: String,
}

pub fn is_feu(data: &[u8]) -> bool {
    data.len() > 8 && &data[..3] == b"UEF"
}

/// Marks the cache's movies as made by this version of [`to_swf`].
pub const MOVIES_STAMP_REL: &str = "ui/stamp-movies-export-copies-first.txt";

/// Where a movie lives in the cache as standard SWF (`ui/.../name.swf`), by
/// its archive path or by the path another movie imports it by (which may be
/// in any case, with either slash, doubled).
pub fn movie_rel(path: &str) -> String {
    let rel: Vec<&str> = path.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    let rel = rel.join("/").to_ascii_lowercase();
    match rel.strip_suffix(".feu") {
        Some(stem) => format!("{stem}.swf"),
        None => rel,
    }
}

/// Where the tags start (after the header's frame rectangle, rate and count).
fn tags_start(data: &[u8]) -> Result<usize> {
    if data.len() < 9 {
        bail!("too short for a movie header");
    }
    let nbits = (data[8] >> 3) as usize;
    let start = 8 + (5 + 4 * nbits).div_ceil(8) + 4;
    if start > data.len() {
        bail!("truncated movie header");
    }
    Ok(start)
}

/// One tag: its code and body.
struct Tag<'a> {
    code: u16,
    body: &'a [u8],
}

/// An ExportAssets tag's (id, name) pairs.
fn parse_exports(body: &[u8]) -> Option<Vec<(u16, &[u8])>> {
    let n = u16::from_le_bytes(body.get(..2)?.try_into().ok()?);
    let mut off = 2;
    let mut out = Vec::new();
    for _ in 0..n {
        let id = u16::from_le_bytes(body.get(off..off + 2)?.try_into().ok()?);
        let len = body.get(off + 2..)?.iter().position(|&b| b == 0)?;
        out.push((id, &body[off + 2..off + 2 + len]));
        off += 2 + len + 1;
    }
    Some(out)
}

/// For a movie that imports, the sprites it exports again under new ids,
/// and their names exported for those, to go before its first import.
///
/// A clip imported from a library (`lib.feu`'s grid tile) attaches by name
/// in the library's own symbols. Ruffle gives those the importer's exports
/// as the library loads (`import_exports_of_importer`), so only exports met
/// before the import, and only at ids the library doesn't use: the
/// rucksack's item icons (`TX_...`, exported after its imports, ids from
/// 173) never reached lib, and a tile's `attachMovie` of one found nothing.
/// A copy holds the sprite's frames (not the definitions in it, made once
/// by the original), so the name attaches the same thing in the library;
/// in the movie itself the original's export, met later, keeps the name.
fn export_copies(tags: &[Tag<'_>]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if !tags.iter().any(|t| matches!(t.code, IMPORT_ASSETS | IMPORT_ASSETS2)) {
        return Ok(out);
    }
    let exports: Vec<(u16, &[u8])> = tags.iter().filter(|t| t.code == EXPORT_ASSETS).filter_map(|t| parse_exports(t.body)).flatten().collect();
    let mut copies: Vec<(u16, &[u8])> = Vec::new();
    for t in tags.iter().filter(|t| t.code == DEFINE_SPRITE && t.body.len() >= 4) {
        let id = u16::from_le_bytes([t.body[0], t.body[1]]);
        let names: Vec<&[u8]> = exports.iter().filter(|e| e.0 == id).map(|e| e.1).collect();
        if names.is_empty() {
            continue;
        }
        let copy = EXPORT_COPY_BASE + copies.len() as u16;
        let mut body = copy.to_le_bytes().to_vec();
        body.extend_from_slice(&t.body[2..4]);
        for inner in read_tags(t.body, 4, t.body.len())?.into_iter().filter(|t| FRAME_TAGS.contains(&t.code)) {
            match inner.code {
                DO_ACTION => write_tag(&mut body, DO_ACTION, &crate::avm1::replace_fscommand2(inner.body, ENGINE_CALL)?),
                code => write_tag(&mut body, code, inner.body),
            }
        }
        write_tag(&mut out, DEFINE_SPRITE, &body);
        copies.extend(names.into_iter().map(|n| (copy, n)));
    }
    if !copies.is_empty() {
        let mut body = (copies.len() as u16).to_le_bytes().to_vec();
        for (id, name) in copies {
            body.extend_from_slice(&id.to_le_bytes());
            body.extend_from_slice(name);
            body.push(0);
        }
        write_tag(&mut out, EXPORT_ASSETS, &body);
    }
    Ok(out)
}

/// A sprite's tags that make its frames (no definitions): End, ShowFrame,
/// PlaceObject 1-3, RemoveObject 1-2, DoAction, StartSound 1-2, the sound
/// stream's, FrameLabel.
const FRAME_TAGS: [u16; 14] = [END, 1, 4, 5, 12, 15, 18, 19, 26, 28, 43, 45, 70, 89];

/// The tags from `off` up to and including the next End (or `end`).
fn read_tags(data: &[u8], mut off: usize, end: usize) -> Result<Vec<Tag<'_>>> {
    let mut out = Vec::new();
    while off + 2 <= end {
        let head = u16::from_le_bytes([data[off], data[off + 1]]);
        off += 2;
        let (code, mut len) = (head >> 6, (head & 0x3f) as usize);
        if len == 0x3f {
            let l = data.get(off..off + 4).context("truncated tag length")?;
            len = u32::from_le_bytes(l.try_into().unwrap()) as usize;
            off += 4;
        }
        let body = data.get(off..off + len).with_context(|| format!("tag {code} runs past the movie"))?;
        off += len;
        out.push(Tag { code, body });
        if code == END {
            break;
        }
    }
    Ok(out)
}

fn cstr(b: &[u8]) -> Option<(String, &[u8])> {
    let n = b.iter().position(|&c| c == 0)?;
    Some((String::from_utf8_lossy(&b[..n]).into_owned(), &b[n + 1..]))
}

fn parse_image(body: &[u8]) -> Option<ExternalImage> {
    let u16_at = |o: usize| body.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let (path, _) = cstr(body.get(6..)?)?;
    Some(ExternalImage { id: u16_at(0)?, width: u16_at(2)?, height: u16_at(4)?, path })
}

fn parse_font(body: &[u8]) -> Option<ExternalFont> {
    let id = u16::from_le_bytes([*body.first()?, *body.get(1)?]);
    let (file, rest) = cstr(body.get(2..)?)?;
    let (face, _) = cstr(rest)?;
    Some(ExternalFont { id, file, face })
}

/// Every tag of a movie, sprites' own included, in file order.
fn all_tags(data: &[u8]) -> Result<Vec<Tag<'_>>> {
    fn walk<'a>(data: &'a [u8], tags: Vec<Tag<'a>>, out: &mut Vec<Tag<'a>>) -> Result<()> {
        for t in tags {
            if t.code == DEFINE_SPRITE && t.body.len() >= 4 {
                let inner = read_tags(t.body, 4, t.body.len())?;
                walk(data, inner, out)?;
            }
            out.push(t);
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(data, read_tags(data, tags_start(data)?, data.len())?, &mut out)?;
    Ok(out)
}

/// The bitmaps a movie takes from the game's textures.
pub fn external_images(feu: &[u8]) -> Result<Vec<ExternalImage>> {
    Ok(all_tags(feu)?.iter().filter(|t| t.code == EXTERNAL_IMAGE).filter_map(|t| parse_image(t.body)).collect())
}

/// The fonts a movie takes from the game's font files.
pub fn external_fonts(feu: &[u8]) -> Result<Vec<ExternalFont>> {
    Ok(all_tags(feu)?.iter().filter(|t| t.code == EXTERNAL_FONT).filter_map(|t| parse_font(t.body)).collect())
}

fn write_tag(out: &mut Vec<u8>, code: u16, body: &[u8]) {
    if body.len() < 0x3f {
        out.extend_from_slice(&((code << 6) | body.len() as u16).to_le_bytes());
    } else {
        out.extend_from_slice(&((code << 6) | 0x3f).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    }
    out.extend_from_slice(body);
}

/// RGBA rows at the authored size: a texture padded out to a power of two
/// is cropped, one of another size is resampled.
fn fit(rgba: &[u8], w: u32, h: u32, to_w: u32, to_h: u32) -> Vec<u8> {
    if (w, h) == (to_w, to_h) {
        return rgba.to_vec();
    }
    let padded = w >= to_w && h >= to_h && w == to_w.next_power_of_two() && h == to_h.next_power_of_two();
    let mut out = vec![0u8; (to_w * to_h * 4) as usize];
    for y in 0..to_h {
        for x in 0..to_w {
            let (sx, sy) = if padded { (x, y) } else { ((x * w / to_w.max(1)).min(w - 1), (y * h / to_h.max(1)).min(h - 1)) };
            let s = ((sy * w + sx) * 4) as usize;
            let d = ((y * to_w + x) * 4) as usize;
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    out
}

/// A lossless bitmap (`DefineBitsLossless2`, 32-bit premultiplied ARGB).
fn bitmap_tag(id: u16, w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut argb = Vec::with_capacity(rgba.len());
    for p in rgba.chunks_exact(4) {
        let a = p[3] as u32;
        let pre = |c: u8| ((c as u32 * a + 127) / 255) as u8;
        argb.extend_from_slice(&[p[3], pre(p[0]), pre(p[1]), pre(p[2])]);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&argb)?;
    let mut body = Vec::with_capacity(argb.len() / 4 + 16);
    body.extend_from_slice(&id.to_le_bytes());
    body.push(5);
    body.extend_from_slice(&(w as u16).to_le_bytes());
    body.extend_from_slice(&(h as u16).to_le_bytes());
    body.extend_from_slice(&z.finish()?);
    Ok(body)
}

/// SWF's bit-packed fields, most significant bit first.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, bit: 0 }
    }

    fn ub(&mut self, n: u32) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos).context("shape runs past its tag")?;
            v = (v << 1) | ((byte >> (7 - self.bit)) & 1) as u32;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.pos += 1;
            }
        }
        Ok(v)
    }

    fn sb(&mut self, n: u32) -> Result<i32> {
        let v = self.ub(n)?;
        Ok(if n > 0 && n < 32 && v & (1 << (n - 1)) != 0 { (v | !((1u32 << n) - 1)) as i32 } else { v as i32 })
    }

    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.pos += 1;
        }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.align();
        let b = self.data.get(self.pos..self.pos + n).context("shape runs past its tag")?;
        self.pos += n;
        Ok(b)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
}

#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    acc: u8,
    bits: u32,
}

impl BitWriter {
    fn ub(&mut self, n: u32, v: u32) {
        for i in (0..n).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1) as u8;
            self.bits += 1;
            if self.bits == 8 {
                self.out.push(self.acc);
                self.acc = 0;
                self.bits = 0;
            }
        }
    }

    fn sb(&mut self, n: u32, v: i32) {
        self.ub(n, v as u32);
    }

    fn align(&mut self) {
        if self.bits != 0 {
            self.out.push(self.acc << (8 - self.bits));
            self.acc = 0;
            self.bits = 0;
        }
    }

    fn bytes(&mut self, b: &[u8]) {
        self.align();
        self.out.extend_from_slice(b);
    }
}

/// Bits a signed field needs for these values.
fn signed_bits(values: &[i32]) -> u32 {
    values
        .iter()
        .map(|&v| {
            if v == 0 {
                0
            } else if v > 0 {
                33 - v.leading_zeros()
            } else {
                33 - (!v).leading_zeros()
            }
        })
        .max()
        .unwrap_or(0)
}

/// A fill's matrix (scale and skew 16.16, translation in twips).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Matrix {
    scale: Option<(i32, i32)>,
    skew: Option<(i32, i32)>,
    translate: (i32, i32),
}

impl Matrix {
    fn read(r: &mut BitReader) -> Result<Self> {
        r.align();
        let scale = if r.ub(1)? == 1 {
            let n = r.ub(5)?;
            Some((r.sb(n)?, r.sb(n)?))
        } else {
            None
        };
        let skew = if r.ub(1)? == 1 {
            let n = r.ub(5)?;
            Some((r.sb(n)?, r.sb(n)?))
        } else {
            None
        };
        let n = r.ub(5)?;
        let translate = (r.sb(n)?, r.sb(n)?);
        r.align();
        Ok(Matrix { scale, skew, translate })
    }

    fn write(&self, w: &mut BitWriter) {
        w.align();
        for pair in [self.scale, self.skew] {
            match pair {
                Some((a, b)) => {
                    let n = signed_bits(&[a, b]);
                    w.ub(1, 1);
                    w.ub(5, n);
                    w.sb(n, a);
                    w.sb(n, b);
                }
                None => w.ub(1, 0),
            }
        }
        let n = signed_bits(&[self.translate.0, self.translate.1]);
        w.ub(5, n);
        w.sb(n, self.translate.0);
        w.sb(n, self.translate.1);
        w.align();
    }

    /// The same mapping for a bitmap whose x and y are scaled by `fx` and
    /// `fy` (bitmap x goes through scale x and skew 0, y through skew 1 and
    /// scale y).
    fn rescaled(&self, fx: f64, fy: f64) -> Self {
        let (sx, sy) = self.scale.unwrap_or((1 << 16, 1 << 16));
        let s = |v: i32, f: f64| (v as f64 * f).round() as i32;
        Matrix { scale: Some((s(sx, fx), s(sy, fy))), skew: self.skew.map(|(r0, r1)| (s(r0, fx), s(r1, fy))), translate: self.translate }
    }
}

/// Where fills of an image in an atlas go instead: the atlas's id, and the
/// image's authored size over the atlas's.
type AtlasFills = std::collections::HashMap<u16, (u16, f64, f64)>;

/// What a shape's bitmap fill becomes (bitmap id and matrix; `None`: as it
/// is).
type FillMap<'a> = dyn FnMut(u16, Matrix) -> Option<(u16, Matrix)> + 'a;

/// Copies a shape's fill style, a bitmap fill as `fill` has it.
/// (`version`: 1 to 4, `DefineShape` to `DefineShape4`.)
fn copy_fill_style(r: &mut BitReader, w: &mut BitWriter, version: u8, fill: &mut FillMap, changed: &mut bool) -> Result<()> {
    let kind = r.u8()?;
    w.bytes(&[kind]);
    let color = if version >= 3 { 4 } else { 3 };
    match kind {
        0x00 => w.bytes(r.bytes(color)?),
        0x10 | 0x12 | 0x13 => {
            Matrix::read(r)?.write(w);
            let flags = r.u8()?;
            w.bytes(&[flags]);
            w.bytes(r.bytes((flags & 0x0f) as usize * (1 + color))?);
            if kind == 0x13 {
                w.bytes(r.bytes(2)?);
            }
        }
        0x40..=0x43 => {
            let id = r.u16()?;
            let m = Matrix::read(r)?;
            let (id, m) = fill(id, m).inspect(|_| *changed = true).unwrap_or((id, m));
            w.bytes(&id.to_le_bytes());
            m.write(w);
        }
        k => bail!("unknown fill style {k:#x}"),
    }
    Ok(())
}

fn copy_styles(r: &mut BitReader, w: &mut BitWriter, version: u8, fill: &mut FillMap, changed: &mut bool) -> Result<()> {
    let count = |r: &mut BitReader, w: &mut BitWriter| -> Result<usize> {
        let n = r.u8()?;
        w.bytes(&[n]);
        if n == 0xff && version >= 2 {
            let n = r.u16()?;
            w.bytes(&n.to_le_bytes());
            return Ok(n as usize);
        }
        Ok(n as usize)
    };
    for _ in 0..count(r, w)? {
        copy_fill_style(r, w, version, fill, changed)?;
    }
    for _ in 0..count(r, w)? {
        w.bytes(r.bytes(2)?);
        if version == 4 {
            let flags = r.bytes(2)?;
            w.bytes(flags);
            if (flags[0] >> 4) & 3 == 2 {
                w.bytes(r.bytes(2)?);
            }
            if flags[0] & 0x08 != 0 {
                copy_fill_style(r, w, version, fill, changed)?;
            } else {
                w.bytes(r.bytes(4)?);
            }
        } else {
            w.bytes(r.bytes(if version >= 3 { 4 } else { 3 })?);
        }
    }
    Ok(())
}

/// A `DefineShape`..`DefineShape4` body with its bitmap fills as `fill` has
/// them, or `None` when it changes none.
fn shape_with_fills(code: u16, body: &[u8], fill: &mut FillMap) -> Result<Option<Vec<u8>>> {
    let version = match code {
        DEFINE_SHAPE => 1,
        DEFINE_SHAPE2 => 2,
        DEFINE_SHAPE3 => 3,
        _ => 4,
    };
    let mut r = BitReader::new(body);
    let skip_rect = |r: &mut BitReader| -> Result<()> {
        let n = r.ub(5)?;
        r.ub(4 * n)?;
        r.align();
        Ok(())
    };
    r.u16()?;
    skip_rect(&mut r)?;
    if version == 4 {
        skip_rect(&mut r)?;
        r.u8()?;
    }
    let mut w = BitWriter::default();
    w.bytes(&body[..r.pos]);
    let mut changed = false;
    copy_styles(&mut r, &mut w, version, fill, &mut changed)?;
    let (mut fill_bits, mut line_bits) = (r.ub(4)?, r.ub(4)?);
    w.ub(4, fill_bits);
    w.ub(4, line_bits);
    loop {
        if r.ub(1)? == 0 {
            w.ub(1, 0);
            let flags = r.ub(5)?;
            w.ub(5, flags);
            if flags == 0 {
                break;
            }
            if flags & 1 != 0 {
                let n = r.ub(5)?;
                let (dx, dy) = (r.sb(n)?, r.sb(n)?);
                w.ub(5, n);
                w.sb(n, dx);
                w.sb(n, dy);
            }
            for (bit, n) in [(2, fill_bits), (4, fill_bits), (8, line_bits)] {
                if flags & bit != 0 {
                    let v = r.ub(n)?;
                    w.ub(n, v);
                }
            }
            if flags & 16 != 0 {
                r.align();
                copy_styles(&mut r, &mut w, version, fill, &mut changed)?;
                (fill_bits, line_bits) = (r.ub(4)?, r.ub(4)?);
                w.ub(4, fill_bits);
                w.ub(4, line_bits);
            }
        } else {
            w.ub(1, 1);
            let straight = r.ub(1)?;
            let nb = r.ub(4)?;
            w.ub(1, straight);
            w.ub(4, nb);
            let n = nb + 2;
            let fields = if straight == 1 {
                let general = r.ub(1)?;
                w.ub(1, general);
                if general == 1 {
                    2
                } else {
                    let vertical = r.ub(1)?;
                    w.ub(1, vertical);
                    1
                }
            } else {
                4
            };
            for _ in 0..fields {
                let v = r.sb(n)?;
                w.sb(n, v);
            }
        }
    }
    w.align();
    // (Anything after the records, kept.)
    r.align();
    w.bytes(&body[r.pos.min(body.len())..]);
    Ok(changed.then_some(w.out))
}

/// The atlases a movie's images are packed in: each embedded once, under
/// the first image's id.
#[derive(Default)]
struct Atlases {
    /// Atlas texture → its id and size.
    by_texture: std::collections::HashMap<String, (u16, u32, u32)>,
    fills: AtlasFills,
}

/// Gives an external image's pixels (width, height, RGBA rows).
pub type ImageSource<'a> = dyn FnMut(&ExternalImage) -> Option<(u32, u32, Vec<u8>)> + 'a;
/// Gives the body of a `DefineFont3` for an external font (see
/// `crate::ffd::TracedFont::define_font3`).
pub type FontSource<'a> = dyn FnMut(&ExternalFont) -> Option<Vec<u8>> + 'a;

/// A standard SWF of a movie. An external image `image` can't give is left
/// transparent at its size; an external font `font` can't give is left out
/// (its text falls back to a device font).
pub fn to_swf(
    feu: &[u8],
    mut image: impl FnMut(&ExternalImage) -> Option<(u32, u32, Vec<u8>)>,
    mut font: impl FnMut(&ExternalFont) -> Option<Vec<u8>>,
) -> Result<Vec<u8>> {
    if !is_feu(feu) {
        bail!("not an interface movie (no UEF signature)");
    }
    fn rewrite(
        tags: Vec<Tag<'_>>,
        out: &mut Vec<u8>,
        image: &mut ImageSource,
        font: &mut FontSource,
        atlases: &mut Atlases,
        copies: &mut Vec<u8>,
    ) -> Result<()> {
        for t in tags {
            match t.code {
                EXTERNAL_IMAGE => {
                    let Some(ext) = parse_image(t.body) else { continue };
                    let (w, h) = (ext.width.max(1) as u32, ext.height.max(1) as u32);
                    // An image in an atlas met before: its fills go to the
                    // atlas, and its own id holds a transparent pixel.
                    if let Some(&(id, aw, ah)) = atlases.by_texture.get(&ext.texture()).filter(|_| ext.in_atlas()) {
                        atlases.fills.insert(ext.id, (id, w as f64 / aw as f64, h as f64 / ah as f64));
                        write_tag(out, DEFINE_BITS_LOSSLESS2, &bitmap_tag(ext.id, 1, 1, &[0; 4])?);
                        continue;
                    }
                    let rgba = match image(&ext) {
                        Some((tw, th, px)) if px.len() == (tw * th * 4) as usize && tw > 0 && th > 0 => {
                            if ext.in_atlas() {
                                atlases.by_texture.insert(ext.texture(), (ext.id, tw, th));
                                atlases.fills.insert(ext.id, (ext.id, w as f64 / tw as f64, h as f64 / th as f64));
                                write_tag(out, DEFINE_BITS_LOSSLESS2, &bitmap_tag(ext.id, tw, th, &px)?);
                                continue;
                            }
                            fit(&px, tw, th, w, h)
                        }
                        _ => vec![0u8; (w * h * 4) as usize],
                    };
                    write_tag(out, DEFINE_BITS_LOSSLESS2, &bitmap_tag(ext.id, w, h, &rgba)?);
                }
                DEFINE_SHAPE | DEFINE_SHAPE2 | DEFINE_SHAPE3 | DEFINE_SHAPE4 if !atlases.fills.is_empty() => {
                    let fills = &atlases.fills;
                    let mut to_atlas = |id: u16, m: Matrix| fills.get(&id).map(|&(to, fx, fy)| (to, m.rescaled(fx, fy)));
                    match shape_with_fills(t.code, t.body, &mut to_atlas)? {
                        Some(body) => write_tag(out, t.code, &body),
                        None => write_tag(out, t.code, t.body),
                    }
                }
                EXTERNAL_FONT => {
                    if let Some(body) = parse_font(t.body).and_then(|f| font(&f)) {
                        write_tag(out, DEFINE_FONT3, &body);
                    }
                }
                MOVIE_ID | MOVIE_VERSION => {}
                DO_ACTION => write_tag(out, DO_ACTION, &crate::avm1::replace_fscommand2(t.body, ENGINE_CALL)?),
                DO_INIT_ACTION if t.body.len() >= 2 => {
                    let mut body = t.body[..2].to_vec();
                    body.extend(crate::avm1::replace_fscommand2(&t.body[2..], ENGINE_CALL)?);
                    write_tag(out, DO_INIT_ACTION, &body);
                }
                DEFINE_SPRITE if t.body.len() >= 4 => {
                    let mut body = t.body[..4].to_vec();
                    rewrite(read_tags(t.body, 4, t.body.len())?, &mut body, image, font, atlases, &mut Vec::new())?;
                    write_tag(out, DEFINE_SPRITE, &body);
                }
                // The exported sprites' copies first (`export_copies`).
                IMPORT_ASSETS | IMPORT_ASSETS2 => {
                    out.append(copies);
                    write_tag(out, t.code, t.body);
                }
                code => write_tag(out, code, t.body),
            }
        }
        Ok(())
    }
    let start = tags_start(feu)?;
    let mut out = Vec::with_capacity(feu.len() * 2);
    out.extend_from_slice(b"FWS");
    out.extend_from_slice(&feu[3..start]);
    let tags = read_tags(feu, start, feu.len())?;
    let mut copies = export_copies(&tags)?;
    rewrite(tags, &mut out, &mut image, &mut font, &mut Atlases::default(), &mut copies)?;
    let len = out.len() as u32;
    out[4..8].copy_from_slice(&len.to_le_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(code: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_tag(&mut out, code, body);
        out
    }

    #[test]
    fn imports_and_archive_paths_name_the_same_cached_movie() {
        assert_eq!(movie_rel(r"ui\menus\shared.feu"), "ui/menus/shared.swf");
        assert_eq!(movie_rel(r"UI\Menus\Shared.feu"), "ui/menus/shared.swf");
        assert_eq!(movie_rel(r"UI\\Menus\\Shared.feu"), "ui/menus/shared.swf");
        assert_eq!(movie_rel("UI/Menus/Shared.feu"), "ui/menus/shared.swf");
    }

    /// A movie that imports has each sprite it exports written again under
    /// a high id, without its definitions, and exported, before the import;
    /// one that doesn't import is left as it is.
    #[test]
    fn an_importing_movie_exports_copies_of_its_sprites() {
        let movie = |import: bool| {
            let mut feu = b"UEF\x08\0\0\0\0".to_vec();
            feu.extend_from_slice(&[0x00, 0x00, 0x18, 0x01, 0x00]);
            if import {
                feu.extend(tag(IMPORT_ASSETS, b"UI\\Common\\lib.feu\0\x01\0\x05\0Tile\0"));
            }
            let mut image = vec![9, 0, 1, 0, 1, 0];
            image.extend_from_slice(b"UI\\Common\\Textures\\TX_Icon.png\0");
            let mut sprite = vec![7, 0, 1, 0];
            sprite.extend(tag(EXTERNAL_IMAGE, &image));
            sprite.extend(tag(1, &[]));
            sprite.extend(tag(END, &[]));
            feu.extend(tag(DEFINE_SPRITE, &sprite));
            feu.extend(tag(EXPORT_ASSETS, b"\x01\0\x07\0TX_Icon\0"));
            feu.extend(tag(END, &[]));
            let len = feu.len() as u32;
            feu[4..8].copy_from_slice(&len.to_le_bytes());
            to_swf(&feu, |_| None, |_| None).unwrap()
        };
        let swf = movie(true);
        let tags = read_tags(&swf, tags_start(&swf).unwrap(), swf.len()).unwrap();
        assert_eq!(tags.iter().map(|t| t.code).collect::<Vec<_>>(), vec![DEFINE_SPRITE, EXPORT_ASSETS, IMPORT_ASSETS, DEFINE_SPRITE, EXPORT_ASSETS, END]);
        assert_eq!(&tags[0].body[..2], &EXPORT_COPY_BASE.to_le_bytes());
        assert_eq!(&tags[3].body[..2], &[7, 0]);
        // (The copy without the definition in the sprite.)
        let frames = |body: &[u8]| read_tags(body, 4, body.len()).unwrap().iter().map(|t| t.code).collect::<Vec<_>>();
        assert_eq!(frames(tags[0].body), vec![1, END]);
        assert_eq!(frames(tags[3].body), vec![DEFINE_BITS_LOSSLESS2, 1, END]);
        assert_eq!(parse_exports(tags[1].body).unwrap(), vec![(EXPORT_COPY_BASE, &b"TX_Icon"[..])]);
        assert_eq!(parse_exports(tags[4].body).unwrap(), vec![(7, &b"TX_Icon"[..])]);
        let swf = movie(false);
        let tags = read_tags(&swf, tags_start(&swf).unwrap(), swf.len()).unwrap();
        assert_eq!(tags.iter().map(|t| t.code).collect::<Vec<_>>(), vec![DEFINE_SPRITE, EXPORT_ASSETS, END]);
        assert_eq!(parse_exports(tags[1].body).unwrap(), vec![(7, &b"TX_Icon"[..])]);
    }

    /// A movie of a sprite holding an external image comes out a standard
    /// SWF: the signature put back, the image embedded at its authored size
    /// (a power-of-two texture cropped), the game's tags gone, lengths right.
    #[test]
    fn a_movie_becomes_a_standard_swf() {
        let mut image = Vec::new();
        image.extend_from_slice(&7u16.to_le_bytes());
        image.extend_from_slice(&3u16.to_le_bytes());
        image.extend_from_slice(&2u16.to_le_bytes());
        image.extend_from_slice(b"UI\\Common\\Textures\\TX_Test.png\0\0\0");
        let mut sprite = vec![9, 0, 1, 0];
        sprite.extend(tag(EXTERNAL_IMAGE, &image));
        sprite.extend(tag(1, &[]));
        sprite.extend(tag(END, &[]));
        // Header: signature, version, length, a 5-bit-wide empty rectangle,
        // frame rate and count.
        let mut feu = b"UEF\x08\0\0\0\0".to_vec();
        feu.extend_from_slice(&[0x00, 0x00, 0x18, 0x01, 0x00]);
        feu.extend(tag(MOVIE_VERSION, &[2, 0, 0, 0]));
        feu.extend(tag(DEFINE_SPRITE, &sprite));
        feu.extend(tag(END, &[]));
        let len = feu.len() as u32;
        feu[4..8].copy_from_slice(&len.to_le_bytes());
        assert!(is_feu(&feu));
        let found = external_images(&feu).unwrap();
        assert_eq!(found, vec![ExternalImage { id: 7, width: 3, height: 2, path: r"UI\Common\Textures\TX_Test.png".into() }]);
        assert_eq!(found[0].texture(), r"ui\common\textures\tx_test.xbt");
        // A 4x2 texture, its right column red (padding).
        let mut px = vec![255u8; 4 * 2 * 4];
        for y in 0..2 {
            px[(y * 4 + 3) * 4..(y * 4 + 3) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
        let swf = to_swf(&feu, |_| Some((4, 2, px.clone())), |_| None).unwrap();
        assert_eq!(&swf[..4], b"FWS\x08");
        assert_eq!(u32::from_le_bytes(swf[4..8].try_into().unwrap()) as usize, swf.len());
        let tags = read_tags(&swf, tags_start(&swf).unwrap(), swf.len()).unwrap();
        assert_eq!(tags.iter().map(|t| t.code).collect::<Vec<_>>(), vec![DEFINE_SPRITE, END]);
        let inner = read_tags(tags[0].body, 4, tags[0].body.len()).unwrap();
        assert_eq!(inner.iter().map(|t| t.code).collect::<Vec<_>>(), vec![DEFINE_BITS_LOSSLESS2, 1, END]);
        let bits = inner[0].body;
        assert_eq!(
            (u16::from_le_bytes([bits[0], bits[1]]), bits[2], u16::from_le_bytes([bits[3], bits[4]]), u16::from_le_bytes([bits[5], bits[6]])),
            (7, 5, 3, 2)
        );
        let mut argb = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&bits[7..]), &mut argb).unwrap();
        // Cropped: no red.
        assert_eq!(argb.len(), 3 * 2 * 4);
        assert!(argb.chunks_exact(4).all(|p| p == [255, 255, 255, 255]));
    }

    const ONE: i32 = 1 << 16;

    fn bitmap_fill(w: &mut BitWriter, id: u16, m: Matrix) {
        w.bytes(&[0x41]);
        w.bytes(&id.to_le_bytes());
        m.write(w);
    }

    /// A `DefineShape3` filled with bitmap 8, with every kind of record,
    /// and new styles part way filling with bitmap 9.
    fn test_shape() -> Vec<u8> {
        let mut w = BitWriter::default();
        w.bytes(&20u16.to_le_bytes());
        w.ub(5, 9);
        for v in [-160, 160, -200, 200] {
            w.sb(9, v);
        }
        w.align();
        // Fills: the bitmap at 16x (a 32 px image in a 512 px atlas), a
        // solid; one line.
        w.bytes(&[2]);
        bitmap_fill(&mut w, 8, Matrix { scale: Some((16 * ONE, 16 * ONE)), skew: None, translate: (-5000, 3000) });
        w.bytes(&[0x00, 1, 2, 3, 255]);
        w.bytes(&[1, 20, 0, 9, 9, 9, 255]);
        w.ub(4, 2);
        w.ub(4, 1);
        // Move, fill 0 and line 1; a general line, a vertical one, a curve.
        w.ub(1, 0);
        w.ub(5, 0b01011);
        w.ub(5, 9);
        w.sb(9, -160);
        w.sb(9, -200);
        w.ub(2, 1);
        w.ub(1, 1);
        w.ub(1, 1);
        w.ub(1, 1);
        w.ub(4, 7);
        w.ub(1, 1);
        w.sb(9, 200);
        w.sb(9, -3);
        w.ub(1, 1);
        w.ub(1, 1);
        w.ub(4, 7);
        w.ub(1, 0);
        w.ub(1, 1);
        w.sb(9, 120);
        w.ub(1, 1);
        w.ub(1, 0);
        w.ub(4, 3);
        for v in [10, -10, 7, 6] {
            w.sb(5, v);
        }
        // New styles: bitmap 9 (16 px) turned a quarter, no lines; fill 1.
        w.ub(1, 0);
        w.ub(5, 0b10100);
        w.ub(2, 1);
        w.align();
        w.bytes(&[1]);
        bitmap_fill(&mut w, 9, Matrix { scale: Some((0, 0)), skew: Some((32 * ONE, -32 * ONE)), translate: (40, 0) });
        w.bytes(&[0]);
        w.ub(4, 1);
        w.ub(4, 0);
        w.ub(1, 1);
        w.ub(1, 1);
        w.ub(4, 2);
        w.ub(1, 0);
        w.ub(1, 0);
        w.sb(4, -5);
        w.ub(1, 0);
        w.ub(5, 0);
        w.align();
        w.out
    }

    fn fills_of(body: &[u8]) -> Vec<(u16, Matrix)> {
        let mut seen = Vec::new();
        shape_with_fills(DEFINE_SHAPE3, body, &mut |id, m| {
            seen.push((id, m));
            None
        })
        .unwrap();
        seen
    }

    /// A shape comes through its records unchanged when its fills are; an
    /// atlas image's fills are pointed at the atlas, their matrices scaled
    /// from the image's size to the atlas's (here 1/16 and 1/32: a pixel of
    /// the atlas a pixel on screen).
    #[test]
    fn atlas_fills_point_at_the_atlas() {
        let shape = test_shape();
        assert_eq!(shape_with_fills(DEFINE_SHAPE3, &shape, &mut |_, _| None).unwrap(), None);
        let same = shape_with_fills(DEFINE_SHAPE3, &shape, &mut |id, m| Some((id, m))).unwrap().unwrap();
        assert_eq!(same, shape);
        let before = fills_of(&shape);
        assert_eq!(before.iter().map(|f| f.0).collect::<Vec<_>>(), vec![8, 9]);
        let atlas: AtlasFills = [(8, (8, 32.0 / 512.0, 32.0 / 512.0)), (9, (8, 16.0 / 512.0, 16.0 / 512.0))].into();
        let fixed = shape_with_fills(DEFINE_SHAPE3, &shape, &mut |id, m| atlas.get(&id).map(|&(to, fx, fy)| (to, m.rescaled(fx, fy)))).unwrap().unwrap();
        let after = fills_of(&fixed);
        assert_eq!(after[0], (8, Matrix { scale: Some((ONE, ONE)), skew: None, translate: (-5000, 3000) }));
        assert_eq!(after[1], (8, Matrix { scale: Some((0, 0)), skew: Some((ONE, -ONE)), translate: (40, 0) }));
        // Only the fills changed: the same again with the old ones put back.
        let back = shape_with_fills(DEFINE_SHAPE3, &fixed, &mut |_, _| None).unwrap();
        assert_eq!(back, None);
        let mut old = before.into_iter();
        let restored = shape_with_fills(DEFINE_SHAPE3, &fixed, &mut |_, _| old.next()).unwrap().unwrap();
        assert_eq!(restored, shape);
    }

    /// An atlas is embedded once, at its own size, under its first image's
    /// id; later images of it are a transparent pixel, and fills of them go
    /// to the atlas.
    #[test]
    fn an_atlas_is_embedded_once() {
        let image = |id: u16, size: u16| {
            let mut b = Vec::new();
            b.extend_from_slice(&id.to_le_bytes());
            b.extend_from_slice(&size.to_le_bytes());
            b.extend_from_slice(&size.to_le_bytes());
            b.extend_from_slice(b"UI\\SuperTextures\\H_Test_0.png\0");
            b
        };
        let mut feu = b"UEF\x08\0\0\0\0".to_vec();
        feu.extend_from_slice(&[0x00, 0x00, 0x18, 0x01, 0x00]);
        feu.extend(tag(EXTERNAL_IMAGE, &image(8, 32)));
        feu.extend(tag(EXTERNAL_IMAGE, &image(9, 16)));
        feu.extend(tag(DEFINE_SHAPE3, &test_shape()));
        feu.extend(tag(END, &[]));
        let len = feu.len() as u32;
        feu[4..8].copy_from_slice(&len.to_le_bytes());
        let mut asked = 0;
        let swf = to_swf(
            &feu,
            |_| {
                asked += 1;
                Some((512, 512, vec![128; 512 * 512 * 4]))
            },
            |_| None,
        )
        .unwrap();
        assert_eq!(asked, 1);
        let tags = read_tags(&swf, tags_start(&swf).unwrap(), swf.len()).unwrap();
        assert_eq!(tags.iter().map(|t| t.code).collect::<Vec<_>>(), vec![DEFINE_BITS_LOSSLESS2, DEFINE_BITS_LOSSLESS2, DEFINE_SHAPE3, END]);
        let size = |b: &[u8]| (u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[3], b[4]]), u16::from_le_bytes([b[5], b[6]]));
        assert_eq!(size(tags[0].body), (8, 512, 512));
        assert_eq!(size(tags[1].body), (9, 1, 1));
        let fills = fills_of(tags[2].body);
        assert_eq!(fills.iter().map(|f| (f.0, f.1.scale)).collect::<Vec<_>>(), vec![(8, Some((ONE, ONE))), (8, Some((0, 0)))]);
    }
}
