//! Bink 1 video.
//!
//! Every plane of a frame (the optional alpha plane, luma, then the two half-resolution chroma planes)
//! is a grid of 8x8 blocks, each of one of ten kinds: kept from the previous frame, copied from it with an
//! offset (optionally plus a residue or a DCT-coded difference), DCT coded, filled with one colour, two
//! colours following a bit pattern, colour runs along one of sixteen scan orders, raw pixels, or any of the
//! intra kinds at half resolution doubled up to 16x16.
//!
//! The per-block values are not interleaved with the blocks. Each kind of value (block kinds, colours,
//! patterns, motion offsets, DC levels, run lengths) has its own "bundle", refilled in chunks at the start of
//! every block row and mostly Huffman coded with one of sixteen fixed code sets whose symbol order the plane
//! header chooses. Only the DCT coefficients, the residues and a few flags are read inline as the blocks are
//! walked. Revision `b` predates this layout: its bundles hold fixed-width values, its blocks are coded in
//! place in a single frame buffer, and it has its own quantisers.

use std::num::Wrapping;

use anyhow::{bail, ensure, Result};

use crate::bits::BitReader;
use crate::tables::{
    B_INTER_SEED, B_INTRA_SEED, B_QUANT_DEN, B_QUANT_NUM, B_RUN_BITS, B_SCALE, INTER_QUANT, INTRA_QUANT, PATTERNS, SCAN, TREE_CODES, TREE_LENGTHS,
};

const SKIP: u8 = 0;
const SCALED: u8 = 1;
const MOTION: u8 = 2;
const RUN: u8 = 3;
const RESIDUE: u8 = 4;
const INTRA: u8 = 5;
const FILL: u8 = 6;
const INTER: u8 = 7;
const PATTERN: u8 = 8;
const RAW: u8 = 9;

/// Names of the block kinds, indexed like [`Bink::block_counts`](crate::Bink::block_counts).
pub const BLOCK_KIND_NAMES: [&str; 10] = ["skip", "scaled", "motion", "run", "residue", "intra", "fill", "inter", "pattern", "raw"];

/// One 8-bit image plane. Rows are [`stride`](Self::stride) bytes apart and the buffer extends to whole
/// 16x16 block pairs; only the first [`width`](Self::width) bytes of the first [`height`](Self::height)
/// rows are picture.
///
/// The stride matches the one FFmpeg gives the same frame and the buffer starts out zero as there, so a
/// motion vector near the edge of the plane is accepted or refused exactly as FFmpeg does.
#[derive(Clone)]
pub struct Plane {
    data: Vec<u8>,
    stride: usize,
    width: usize,
    height: usize,
    blocks_w: usize,
    blocks_h: usize,
}

impl Plane {
    fn new(width: usize, height: usize, blocks_w: usize, blocks_h: usize, stride: usize, value: u8) -> Self {
        debug_assert!(stride >= blocks_w.next_multiple_of(2) * 8);
        let rows = blocks_h.next_multiple_of(2) * 8;
        Self { data: vec![value; stride * rows], stride, width, height, blocks_w, blocks_h }
    }

    /// The whole buffer, `stride` bytes per row.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Visible width in samples.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Visible height in rows.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The visible samples of row `y`.
    pub fn row(&self, y: usize) -> &[u8] {
        &self.data[y * self.stride..y * self.stride + self.width]
    }
}

/// A decoded picture: planar YUV 4:2:0 with an optional full-resolution alpha plane.
#[derive(Clone)]
pub struct Frame {
    width: u32,
    height: u32,
    full_range: bool,
    /// Y, U (Cb) and V (Cr).
    planes: [Plane; 3],
    alpha: Option<Plane>,
}

impl Frame {
    /// A frame of zeros, as FFmpeg's buffers start out; grey-scale files get neutral chroma instead.
    fn new(width: u32, height: u32, full_range: bool, alpha: bool, grey: bool) -> Self {
        let (w, h) = (width as usize, height as usize);
        // FFmpeg pads Bink frames to a multiple of 32 pixels, then widens them until every plane's stride
        // is a multiple of 64 bytes.
        let mut padded = w.next_multiple_of(32);
        while padded % 64 != 0 || padded.div_ceil(2) % 64 != 0 {
            padded += padded.isolate_lowest_one();
        }
        let luma = || Plane::new(w, h, (w + 7) >> 3, (h + 7) >> 3, padded, 0);
        let chroma = || Plane::new(w.div_ceil(2), h.div_ceil(2), (w + 15) >> 4, (h + 15) >> 4, padded.div_ceil(2), if grey { 128 } else { 0 });
        Self { width, height, full_range, planes: [luma(), chroma(), chroma()], alpha: alpha.then(luma) }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Luma.
    pub fn y(&self) -> &Plane {
        &self.planes[0]
    }

    /// Blue-difference chroma at half resolution in both directions.
    pub fn u(&self) -> &Plane {
        &self.planes[1]
    }

    /// Red-difference chroma at half resolution in both directions.
    pub fn v(&self) -> &Plane {
        &self.planes[2]
    }

    /// The alpha plane, at full resolution, when the file has one.
    pub fn alpha(&self) -> Option<&Plane> {
        self.alpha.as_ref()
    }

    /// Whether the samples use the full 0-255 range (revision `k`) rather than the studio 16-235 range.
    pub fn full_range(&self) -> bool {
        self.full_range
    }

    /// Converts to tightly packed RGBA8 (`width * height * 4` bytes) with the BT.601 matrix and the
    /// file's range, each chroma sample covering a 2x2 block of pixels, as FFmpeg converts these files.
    /// Alpha is opaque unless the file has an alpha plane. FFmpeg's own SIMD and C converters round
    /// differently from each other; this lands within 3 levels of both.
    pub fn to_rgba(&self, out: &mut [u8]) {
        let (w, h) = (self.width as usize, self.height as usize);
        assert!(out.len() >= w * h * 4, "RGBA buffer holds {} bytes, {w}x{h} needs {}", out.len(), w * h * 4);
        // 16.16 fixed-point BT.601 coefficients (FFmpeg's), with the studio range expansion folded in.
        let (black, ky, kr, kgu, kgv, kb) = if self.full_range { (0, 65536, 91881, 22554, 46802, 116130) } else { (16, 76309, 104597, 25675, 53279, 132201) };
        let clamp = |v: i32| (v >> 16).clamp(0, 255) as u8;
        for (y, dst) in out.chunks_exact_mut(w * 4).take(h).enumerate() {
            let (luma, cb, cr) = (self.planes[0].row(y), self.planes[1].row(y / 2), self.planes[2].row(y / 2));
            for ((pair, luma), (&u, &v)) in dst.chunks_mut(8).zip(luma.chunks(2)).zip(cb.iter().zip(cr)) {
                let (u, v) = (u as i32 - 128, v as i32 - 128);
                let (r, g, b) = (kr * v + 32768, 32768 - kgu * u - kgv * v, kb * u + 32768);
                for (px, &l) in pair.chunks_exact_mut(4).zip(luma) {
                    let l = (l as i32 - black) * ky;
                    px.copy_from_slice(&[clamp(l + r), clamp(l + g), clamp(l + b), 255]);
                }
            }
            if let Some(alpha) = &self.alpha {
                for (px, &a) in dst.chunks_exact_mut(4).zip(alpha.row(y)) {
                    px[3] = a;
                }
            }
        }
    }

    /// [`to_rgba`](Self::to_rgba) into a new buffer.
    pub fn rgba(&self) -> Vec<u8> {
        let mut out = vec![0; self.width as usize * self.height as usize * 4];
        self.to_rgba(&mut out);
        out
    }
}

/// One of the sixteen fixed code sets with the symbol order a plane header chose for it, as a lookup
/// table on the next 7 bits: each entry holds the code length in its high nibble and the symbol in its
/// low nibble.
#[derive(Clone)]
struct Tree {
    lut: [u8; 128],
}

impl Tree {
    const IDENTITY: Tree = Tree::build(0, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);

    const fn build(set: usize, syms: [u8; 16]) -> Tree {
        let mut lut = [0u8; 128];
        let mut i = 0;
        while i < 16 {
            let len = TREE_LENGTHS[set][i];
            let mut code = TREE_CODES[set][i] as usize;
            while code < 128 {
                lut[code] = len << 4 | syms[i];
                code += 1 << len;
            }
            i += 1;
        }
        Tree { lut }
    }

    /// Reads a code set number and the order of its symbols: either a short list of the first few symbols
    /// with the rest following in ascending order, or up to four rounds of bit-driven merging that shuffle
    /// the ascending order.
    fn read(br: &mut BitReader) -> Tree {
        let set = br.read(4) as usize;
        if set == 0 {
            return Tree::IDENTITY;
        }
        let mut syms: [u8; 16] = std::array::from_fn(|i| i as u8);
        if br.bit() {
            let count = br.read(3) as usize + 1;
            let mut listed = [false; 16];
            for sym in &mut syms[..count] {
                *sym = br.read(4) as u8;
                listed[*sym as usize] = true;
            }
            let rest = (0..16u8).filter(|&s| !listed[s as usize]);
            for (slot, sym) in syms[count..].iter_mut().zip(rest) {
                *slot = sym;
            }
        } else {
            let rounds = br.read(2) + 1;
            for round in 0..rounds {
                let half = 1usize << round;
                let mut merged = [0u8; 16];
                for start in (0..16).step_by(half * 2) {
                    merge(br, &syms[start..start + half * 2], &mut merged[start..start + half * 2]);
                }
                syms = merged;
            }
        }
        Tree::build(set, syms)
    }

    #[inline(always)]
    fn decode(&self, br: &mut BitReader) -> u8 {
        let entry = self.lut[br.peek(7) as usize];
        br.skip(u32::from(entry >> 4));
        entry & 15
    }
}

/// Merges the two halves of `src` into `dst`, a bit per element choosing which half it comes from until
/// one half runs out.
fn merge(br: &mut BitReader, src: &[u8], dst: &mut [u8]) {
    let half = src.len() / 2;
    let (a, b) = src.split_at(half);
    let (mut i, mut j) = (0, 0);
    for out in dst {
        if j == half || (i < half && !br.bit()) {
            *out = a[i];
            i += 1;
        } else {
            *out = b[j];
            j += 1;
        }
    }
}

/// A plane's stream of one kind of block value.
struct Bundle<T> {
    /// Width in bits of each chunk's value count.
    len: u32,
    tree: Tree,
    data: Vec<T>,
    /// Values decoded by the last refill.
    filled: usize,
    /// Values the blocks have consumed since then.
    pos: usize,
    /// A zero count ends the bundle for the rest of the plane.
    done: bool,
}

impl<T: Copy + Default> Bundle<T> {
    fn new(capacity: usize) -> Self {
        Self { len: 0, tree: Tree::IDENTITY, data: vec![T::default(); capacity], filled: 0, pos: 0, done: false }
    }

    fn reset(&mut self, len: u32) {
        self.len = len;
        self.filled = 0;
        self.pos = 0;
        self.done = false;
    }

    /// A refill only happens once the blocks have used every decoded value. Returns the number of values
    /// in the new chunk, if there is one; revision `k` scrambles block type counts with `xor`.
    fn chunk(&mut self, br: &mut BitReader, xor: u32) -> Result<Option<usize>> {
        if self.done || self.filled > self.pos {
            return Ok(None);
        }
        let mut count = br.read(self.len);
        if count != 0 {
            count ^= xor;
        }
        if count == 0 {
            self.done = true;
            return Ok(None);
        }
        ensure!(count as usize <= self.data.len(), "bundle chunk of {count} values is too long");
        self.filled = count as usize;
        self.pos = 0;
        Ok(Some(count as usize))
    }

    #[inline(always)]
    fn next(&mut self) -> T {
        let v = self.data.get(self.pos).copied().unwrap_or_default();
        self.pos += 1;
        v
    }
}

/// Number of bits needed for counts of up to `n + 511` values.
fn count_bits(n: usize) -> u32 {
    usize::BITS - (n + 511).leading_zeros()
}

/// The bundles of revisions after `b`.
struct Bundles {
    block_types: Bundle<u8>,
    sub_types: Bundle<u8>,
    colors: Bundle<u8>,
    patterns: Bundle<u8>,
    x_off: Bundle<i8>,
    y_off: Bundle<i8>,
    intra_dc: Bundle<i16>,
    inter_dc: Bundle<i16>,
    runs: Bundle<u8>,
    /// Colours code their high nibble with one of sixteen trees chosen by the previous high nibble.
    color_high: [Tree; 16],
    color_last: u8,
}

impl Bundles {
    fn new(max_width: usize) -> Self {
        let capacity = (1usize << count_bits(max_width.div_ceil(8) * 64)).max(256) + 64;
        Self {
            block_types: Bundle::new(capacity),
            sub_types: Bundle::new(capacity),
            colors: Bundle::new(capacity),
            patterns: Bundle::new(capacity),
            x_off: Bundle::new(capacity),
            y_off: Bundle::new(capacity),
            intra_dc: Bundle::new(capacity),
            inter_dc: Bundle::new(capacity),
            runs: Bundle::new(capacity),
            color_high: std::array::from_fn(|_| Tree::IDENTITY),
            color_last: 0,
        }
    }

    /// Reads a plane header: the chunk count widths follow from the plane size, then the trees.
    fn start_plane(&mut self, br: &mut BitReader, width: usize, blocks_w: usize) {
        let width = width.max(8).next_multiple_of(8);
        let per_row = count_bits(width >> 3);
        self.block_types.reset(per_row);
        self.sub_types.reset(count_bits(width >> 4));
        self.colors.reset(count_bits(blocks_w * 64));
        self.patterns.reset(count_bits(blocks_w * 8));
        self.x_off.reset(per_row);
        self.y_off.reset(per_row);
        self.intra_dc.reset(per_row);
        self.inter_dc.reset(per_row);
        self.runs.reset(count_bits(blocks_w * 48));

        self.block_types.tree = Tree::read(br);
        self.sub_types.tree = Tree::read(br);
        for tree in &mut self.color_high {
            *tree = Tree::read(br);
        }
        self.color_last = 0;
        self.colors.tree = Tree::read(br);
        self.patterns.tree = Tree::read(br);
        self.x_off.tree = Tree::read(br);
        self.y_off.tree = Tree::read(br);
        self.runs.tree = Tree::read(br);
    }

    /// Tops up every bundle that has run dry, at the start of a block row.
    fn refill(&mut self, br: &mut BitReader, revision: u8) -> Result<()> {
        let xor = if revision == b'k' { 0xBB } else { 0 };
        refill_block_types(&mut self.block_types, br, xor)?;
        refill_block_types(&mut self.sub_types, br, xor)?;
        self.refill_colors(br, revision < b'i')?;
        if let Some(n) = self.patterns.chunk(br, 0)? {
            let tree = &self.patterns.tree;
            for v in &mut self.patterns.data[..n] {
                let low = tree.decode(br);
                *v = low | tree.decode(br) << 4;
            }
        }
        refill_motion(&mut self.x_off, br)?;
        refill_motion(&mut self.y_off, br)?;
        refill_dcs(&mut self.intra_dc, br, false)?;
        refill_dcs(&mut self.inter_dc, br, true)?;
        if let Some(n) = self.runs.chunk(br, 0)? {
            if br.bit() {
                let v = br.read(4) as u8;
                self.runs.data[..n].fill(v);
            } else {
                let tree = &self.runs.tree;
                for v in &mut self.runs.data[..n] {
                    *v = tree.decode(br);
                }
            }
        }
        Ok(())
    }

    fn refill_colors(&mut self, br: &mut BitReader, signed: bool) -> Result<()> {
        let Some(n) = self.colors.chunk(br, 0)? else { return Ok(()) };
        let mut read = |br: &mut BitReader| {
            self.color_last = self.color_high[self.color_last as usize].decode(br);
            let v = self.color_last << 4 | self.colors.tree.decode(br);
            // Before revision `i` colours were stored as sign and magnitude around 128.
            if signed {
                let magnitude = i32::from(v & 0x7F);
                (if v & 0x80 != 0 { 128 - magnitude } else { 128 + magnitude }) as u8
            } else {
                v
            }
        };
        if br.bit() {
            let v = read(br);
            self.colors.data[..n].fill(v);
        } else {
            for i in 0..n {
                let v = read(br);
                self.colors.data[i] = v;
            }
        }
        Ok(())
    }
}

/// Block types: either one repeated value, or Huffman symbols where 12 to 15 repeat the previous type
/// 4, 8, 12 or 32 times.
fn refill_block_types(b: &mut Bundle<u8>, br: &mut BitReader, xor: u32) -> Result<()> {
    let Some(n) = b.chunk(br, xor)? else { return Ok(()) };
    if br.bit() {
        let v = br.read(4) as u8;
        b.data[..n].fill(v);
        return Ok(());
    }
    let (mut i, mut last) = (0, 0);
    while i < n {
        let v = b.tree.decode(br);
        if v < 12 {
            last = v;
            b.data[i] = v;
            i += 1;
        } else {
            let run = [4, 8, 12, 32][v as usize - 12];
            ensure!(n - i >= run, "block type run overflows its chunk");
            b.data[i..i + run].fill(last);
            i += run;
        }
    }
    Ok(())
}

fn refill_motion(b: &mut Bundle<i8>, br: &mut BitReader) -> Result<()> {
    let Some(n) = b.chunk(br, 0)? else { return Ok(()) };
    let signed = |br: &mut BitReader, v: u8| if v != 0 && br.bit() { -(v as i8) } else { v as i8 };
    if br.bit() {
        let v = br.read(4) as u8;
        let v = signed(br, v);
        b.data[..n].fill(v);
    } else {
        for i in 0..n {
            let v = b.tree.decode(br);
            b.data[i] = signed(br, v);
        }
    }
    Ok(())
}

/// DC levels: an 11-bit start value (10 bits and a sign for inter blocks), then groups of up to eight
/// deltas sharing one bit width.
fn refill_dcs(b: &mut Bundle<i16>, br: &mut BitReader, has_sign: bool) -> Result<()> {
    let Some(n) = b.chunk(br, 0)? else { return Ok(()) };
    let mut v = br.read(11 - has_sign as u32) as i32;
    if v != 0 && has_sign && br.bit() {
        v = -v;
    }
    b.data[0] = v as i16;
    let mut i = 1;
    while i < n {
        let group = (n - i).min(8);
        let width = br.read(4);
        for slot in &mut b.data[i..i + group] {
            if width != 0 {
                let delta = br.read(width) as i32;
                v += if delta != 0 && br.bit() { -delta } else { delta };
                ensure!((-32768..=32767).contains(&v), "DC level {v} out of range");
            }
            *slot = v as i16;
        }
        i += group;
    }
    Ok(())
}

/// The list walk shared by DCT coefficients and residues. Coefficients are visited in [`SCAN`] order in
/// groups: entries are a single coefficient (mode 3), a group of four (mode 2), or one of the three big
/// initial groups of four plus sixteen more (mode 0, which becomes mode 1 once its first four are split
/// off). A set bit on an entry splits it; coefficients of a split group either become significant at the
/// current bit level or join the front of the list to be revisited at lower levels.
struct ScanList {
    coef: [u8; 128],
    mode: [u8; 128],
    start: usize,
    end: usize,
}

impl ScanList {
    fn new(initial: &[(u8, u8)]) -> Self {
        let mut list = Self { coef: [0; 128], mode: [0; 128], start: 64, end: 64 };
        for &(coef, mode) in initial {
            list.coef[list.end] = coef;
            list.mode[list.end] = mode;
            list.end += 1;
        }
        list
    }

    /// One pass at the current bit level. `significant` reads the value of each coefficient that becomes
    /// significant; returning false stops the walk, and so does `pass`.
    #[inline(always)]
    fn pass(&mut self, br: &mut BitReader, mut significant: impl FnMut(&mut BitReader, u8) -> bool) -> bool {
        let mut pos = self.start;
        while pos < self.end {
            let (coef, mode) = (self.coef[pos], self.mode[pos]);
            if (coef == 0 && mode == 0) || !br.bit() {
                pos += 1;
                continue;
            }
            match mode {
                0 | 2 => {
                    if mode == 0 {
                        self.coef[pos] = coef + 4;
                        self.mode[pos] = 1;
                    } else {
                        self.coef[pos] = 0;
                        self.mode[pos] = 0;
                        pos += 1;
                    }
                    for c in coef..coef + 4 {
                        if br.bit() {
                            self.start -= 1;
                            self.coef[self.start] = c;
                            self.mode[self.start] = 3;
                        } else if !significant(br, c) {
                            return false;
                        }
                    }
                }
                1 => {
                    self.mode[pos] = 2;
                    for k in 1..4 {
                        self.coef[self.end] = coef + 4 * k;
                        self.mode[self.end] = 2;
                        self.end += 1;
                    }
                }
                _ => {
                    self.coef[pos] = 0;
                    self.mode[pos] = 0;
                    pos += 1;
                    if !significant(br, coef) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// Reads the AC coefficients of a DCT block into `coefs` (natural order; the caller has stored the DC
/// level) and dequantises them with matrix `quant`, read after the coefficients unless the caller passes
/// it. Returns the number of AC coefficients present.
fn read_dct(br: &mut BitReader, coefs: &mut [i32; 64], quant: Option<u32>, matrices: &[[i32; 64]; 16]) -> Result<usize> {
    let mut found = [0u8; 64];
    let mut count = 0;
    let mut list = ScanList::new(&[(4, 0), (24, 0), (44, 0), (1, 3), (2, 3), (3, 3)]);
    // A coefficient becoming significant at level `bits` has its top bit there and its lower bits inline.
    for bits in (0..br.read(4)).rev() {
        list.pass(br, |br, c| {
            let magnitude = if bits == 0 { 1 } else { (br.read(bits) | 1 << bits) as i32 };
            coefs[SCAN[c as usize] as usize] = if br.bit() { -magnitude } else { magnitude };
            found[count] = c;
            count += 1;
            true
        });
    }
    let quant = quant.unwrap_or_else(|| br.read(4));
    ensure!(quant < 16, "quantiser {quant} out of range");
    let matrix = &matrices[quant as usize];
    coefs[0] = coefs[0].wrapping_mul(matrix[0]) >> 11;
    for &c in &found[..count] {
        let at = SCAN[c as usize] as usize;
        coefs[at] = coefs[at].wrapping_mul(matrix[c as usize]) >> 11;
    }
    Ok(count)
}

/// Reads a residue block: bit planes from the top down, each first refining the coefficients already
/// significant and then finding new ones. At most `masks + 1` bits of magnitude are coded in total.
fn read_residue(br: &mut BitReader, block: &mut [i16; 64], mut masks: i32) {
    let mut found = [0u8; 64];
    let mut count = 0;
    let mut list = ScanList::new(&[(4, 0), (24, 0), (44, 0), (0, 2)]);
    let mut mask = 1i16 << br.read(3);
    while mask != 0 {
        for &at in &found[..count] {
            if !br.bit() {
                continue;
            }
            let v = &mut block[at as usize];
            *v = if *v < 0 { *v - mask } else { *v + mask };
            masks -= 1;
            if masks < 0 {
                return;
            }
        }
        let more = list.pass(br, |br, c| {
            let at = SCAN[c as usize];
            found[count] = at;
            count += 1;
            block[at as usize] = if br.bit() { -mask } else { mask };
            masks -= 1;
            masks >= 0
        });
        if !more {
            return;
        }
        mask >>= 1;
    }
}

/// The format's integer 8-point inverse DCT, applied down the columns and then along the rows.
#[inline(always)]
fn idct8(s: [Wrapping<i32>; 8]) -> [Wrapping<i32>; 8] {
    const A1: Wrapping<i32> = Wrapping(2896);
    const A2: Wrapping<i32> = Wrapping(2217);
    const A3: Wrapping<i32> = Wrapping(3784);
    const A4: Wrapping<i32> = Wrapping(-5352);
    let a0 = s[0] + s[4];
    let a1 = s[0] - s[4];
    let a2 = s[2] + s[6];
    let a3 = (A1 * (s[2] - s[6])) >> 11;
    let a4 = s[5] + s[3];
    let a5 = s[5] - s[3];
    let a6 = s[1] + s[7];
    let a7 = s[1] - s[7];
    let b0 = a4 + a6;
    let b1 = (A3 * (a5 + a7)) >> 11;
    let b2 = ((A4 * a5) >> 11) - b0 + b1;
    let b3 = ((A1 * (a6 - a4)) >> 11) - b2;
    let b4 = ((A2 * a7) >> 11) + b3 - b1;
    [a0 + a2 + b0, a1 + a3 - a2 + b2, a1 - a3 + a2 + b3, a0 - a2 - b4, a0 - a2 + b4, a1 - a3 + a2 - b3, a1 + a3 - a2 - b2, a0 + a2 - b0]
}

/// Inverse transform of a dequantised block to pixel values (or differences), before wrapping to 8 bits.
fn idct(coefs: &[i32; 64]) -> [i32; 64] {
    let mut tmp = [Wrapping(0i32); 64];
    for c in 0..8 {
        let col: [Wrapping<i32>; 8] = std::array::from_fn(|r| Wrapping(coefs[r * 8 + c]));
        // A column with no AC terms transforms to its DC level.
        let out = if col[1..].iter().all(|v| v.0 == 0) { [col[0]; 8] } else { idct8(col) };
        for r in 0..8 {
            tmp[r * 8 + c] = out[r];
        }
    }
    let mut out = [0i32; 64];
    for r in 0..8 {
        let row = idct8(tmp[r * 8..r * 8 + 8].try_into().unwrap());
        for c in 0..8 {
            out[r * 8 + c] = (row[c] + Wrapping(0x7F)).0 >> 8;
        }
    }
    out
}

/// Pixels of a DCT block laid out 8 to a row. A block with only a DC level is flat.
fn dct_pixels(coefs: &[i32; 64], ac_count: usize) -> [i32; 64] {
    if ac_count == 0 {
        [coefs[0].wrapping_add(0x7F) >> 8; 64]
    } else {
        idct(coefs)
    }
}

fn put_block(dst: &mut [u8], off: usize, stride: usize, block: &[u8; 64]) {
    for (r, src) in block.chunks_exact(8).enumerate() {
        dst[off + r * stride..off + r * stride + 8].copy_from_slice(src);
    }
}

fn fill_block(dst: &mut [u8], off: usize, stride: usize, v: u8) {
    for r in 0..8 {
        dst[off + r * stride..off + r * stride + 8].fill(v);
    }
}

fn copy_block(dst: &mut [u8], src: &[u8], dst_off: usize, src_off: usize, stride: usize) {
    for r in 0..8 {
        dst[dst_off + r * stride..dst_off + r * stride + 8].copy_from_slice(&src[src_off + r * stride..src_off + r * stride + 8]);
    }
}

/// Doubles an 8x8 block to 16x16.
fn scale_block(dst: &mut [u8], off: usize, stride: usize, block: &[u8; 64]) {
    for (r, src) in block.chunks_exact(8).enumerate() {
        let top = off + 2 * r * stride;
        for (c, &v) in src.iter().enumerate() {
            dst[top + 2 * c] = v;
            dst[top + 2 * c + 1] = v;
        }
        dst.copy_within(top..top + 16, top + stride);
    }
}

fn put_pixels(dst: &mut [u8], off: usize, stride: usize, pixels: &[i32; 64]) {
    for (r, src) in pixels.chunks_exact(8).enumerate() {
        for (d, &s) in dst[off + r * stride..off + r * stride + 8].iter_mut().zip(src) {
            *d = s as u8;
        }
    }
}

fn add_pixels(dst: &mut [u8], off: usize, stride: usize, pixels: &[i32; 64]) {
    for (r, src) in pixels.chunks_exact(8).enumerate() {
        for (d, &s) in dst[off + r * stride..off + r * stride + 8].iter_mut().zip(src) {
            *d = d.wrapping_add(s as u8);
        }
    }
}

fn add_residue(dst: &mut [u8], off: usize, stride: usize, residue: &[i16; 64]) {
    for (r, src) in residue.chunks_exact(8).enumerate() {
        for (d, &s) in dst[off + r * stride..off + r * stride + 8].iter_mut().zip(src) {
            *d = d.wrapping_add(s as u8);
        }
    }
}

/// Two colours chosen per pixel by eight bit rows from the pattern bundle.
fn pattern_block(colors: &mut Bundle<u8>, patterns: &mut Bundle<u8>) -> [u8; 64] {
    let pair = [colors.next(), colors.next()];
    let mut block = [0u8; 64];
    for row in block.chunks_exact_mut(8) {
        let mut bits = patterns.next();
        for px in row {
            *px = pair[(bits & 1) as usize];
            bits >>= 1;
        }
    }
    block
}

/// Runs of colours (single repeated or listed) along one of the sixteen scan patterns.
fn run_block(br: &mut BitReader, b: &mut Bundles) -> Result<[u8; 64]> {
    let scan = &PATTERNS[br.read(4) as usize];
    let mut block = [0u8; 64];
    let mut i = 0;
    while i < 63 {
        let run = b.runs.next() as usize + 1;
        ensure!(i + run <= 64, "run block overflows");
        if br.bit() {
            let v = b.colors.next();
            for &p in &scan[i..i + run] {
                block[p as usize] = v;
            }
        } else {
            for &p in &scan[i..i + run] {
                block[p as usize] = b.colors.next();
            }
        }
        i += run;
    }
    if i == 63 {
        block[scan[63] as usize] = b.colors.next();
    }
    Ok(block)
}

/// Offset in the previous frame of a motion block's source, refusing vectors that leave the plane as
/// the reference decoder does.
#[inline]
fn motion_source(b: &mut Bundles, off: usize, stride: usize, last_block: usize) -> Result<usize> {
    let (x, y) = (b.x_off.next() as isize, b.y_off.next() as isize);
    let src = off as isize + x + y * stride as isize;
    ensure!(src >= 0 && src as usize <= last_block, "motion vector ({x}, {y}) points outside the previous frame");
    Ok(src as usize)
}

/// Decodes one plane of revisions after `b` into `dst`, with `prev` holding the same plane of the
/// previous frame. `width` is the plane's nominal width, which sets the chunk count widths.
fn decode_plane(br: &mut BitReader, b: &mut Bundles, revision: u8, dst: &mut Plane, prev: &Plane, width: usize, counts: &mut [u64; 20]) -> Result<()> {
    let (blocks_w, blocks_h, stride) = (dst.blocks_w, dst.blocks_h, dst.stride);
    if revision == b'k' && br.bit() {
        let v = br.read(8) as u8;
        dst.data.fill(v);
        br.align32();
        return Ok(());
    }
    b.start_plane(br, width, blocks_w);
    let last_block = (blocks_h - 1) * 8 * stride + (blocks_w - 1) * 8;
    let (dst, prev) = (&mut dst.data[..], &prev.data[..]);
    for by in 0..blocks_h {
        b.refill(br, revision)?;
        let mut bx = 0;
        while bx < blocks_w {
            let off = by * 8 * stride + bx * 8;
            let kind = b.block_types.next();
            // The other three quarters of a 16x16 block come up as scaled blocks too; skip past them.
            if kind == SCALED && (bx | by) & 1 != 0 {
                bx += 2;
                continue;
            }
            match kind {
                SKIP => copy_block(dst, prev, off, off, stride),
                SCALED => {
                    let sub = b.sub_types.next();
                    let block = match sub {
                        RUN => run_block(br, b)?,
                        INTRA => {
                            let mut coefs = [0i32; 64];
                            coefs[0] = i32::from(b.intra_dc.next());
                            let ac = read_dct(br, &mut coefs, None, &INTRA_QUANT)?;
                            dct_pixels(&coefs, ac).map(|v| v as u8)
                        }
                        FILL => [b.colors.next(); 64],
                        PATTERN => pattern_block(&mut b.colors, &mut b.patterns),
                        RAW => std::array::from_fn(|_| b.colors.next()),
                        _ => bail!("unknown scaled block type {sub}"),
                    };
                    counts[10 + sub as usize] += 1;
                    scale_block(dst, off, stride, &block);
                    bx += 1;
                }
                MOTION => {
                    let src = motion_source(b, off, stride, last_block)?;
                    copy_block(dst, prev, off, src, stride);
                }
                RUN => {
                    let block = run_block(br, b)?;
                    put_block(dst, off, stride, &block);
                }
                RESIDUE => {
                    let src = motion_source(b, off, stride, last_block)?;
                    copy_block(dst, prev, off, src, stride);
                    let mut residue = [0i16; 64];
                    let masks = br.read(7) as i32;
                    read_residue(br, &mut residue, masks);
                    add_residue(dst, off, stride, &residue);
                }
                INTRA => {
                    let mut coefs = [0i32; 64];
                    coefs[0] = i32::from(b.intra_dc.next());
                    let ac = read_dct(br, &mut coefs, None, &INTRA_QUANT)?;
                    put_pixels(dst, off, stride, &dct_pixels(&coefs, ac));
                }
                FILL => fill_block(dst, off, stride, b.colors.next()),
                INTER => {
                    let src = motion_source(b, off, stride, last_block)?;
                    copy_block(dst, prev, off, src, stride);
                    let mut coefs = [0i32; 64];
                    coefs[0] = i32::from(b.inter_dc.next());
                    let ac = read_dct(br, &mut coefs, None, &INTER_QUANT)?;
                    add_pixels(dst, off, stride, &dct_pixels(&coefs, ac));
                }
                PATTERN => {
                    let block = pattern_block(&mut b.colors, &mut b.patterns);
                    put_block(dst, off, stride, &block);
                }
                RAW => {
                    let block = std::array::from_fn(|_| b.colors.next());
                    put_block(dst, off, stride, &block);
                }
                _ => bail!("unknown block type {kind}"),
            }
            counts[kind as usize] += 1;
            bx += 1;
        }
    }
    br.align32();
    Ok(())
}

/// Revision `b` bundles, in the order they are refilled.
const OLD_TYPES: usize = 0;
const OLD_COLORS: usize = 1;
const OLD_PATTERN: usize = 2;
const OLD_X: usize = 3;
const OLD_Y: usize = 4;
const OLD_INTRA_DC: usize = 5;
const OLD_INTER_DC: usize = 6;
const OLD_INTRA_Q: usize = 7;
const OLD_INTER_Q: usize = 8;
const OLD_INTER_COEFS: usize = 9;
/// Bit width of each revision `b` bundle's values, and whether they are offset to be signed.
const OLD_FIELDS: [(u32, bool); 10] = [(4, false), (8, false), (8, false), (5, true), (5, true), (11, false), (11, true), (4, false), (4, false), (7, false)];

/// Revision `b` state: fixed-width bundles and quantisers derived from seed weights.
struct OldBundles {
    bundles: [Bundle<i16>; 10],
    intra_quant: Box<[[i32; 64]; 16]>,
    inter_quant: Box<[[i32; 64]; 16]>,
}

impl OldBundles {
    fn new() -> Self {
        let mut inverse_scan = [0usize; 64];
        for (i, &s) in SCAN.iter().enumerate() {
            inverse_scan[s as usize] = i;
        }
        let matrices = |seed: &[u8; 64]| {
            let mut m = Box::new([[0i32; 64]; 16]);
            for (q, matrix) in m.iter_mut().enumerate() {
                for i in 0..64 {
                    let v = i64::from(seed[i]) * i64::from(B_SCALE[i]) * i64::from(B_QUANT_NUM[q]) / (i64::from(B_QUANT_DEN[q]) << 18);
                    matrix[inverse_scan[i]] = v as i32;
                }
            }
            m
        };
        Self { bundles: std::array::from_fn(|_| Bundle::new(1 << 13)), intra_quant: matrices(&B_INTRA_SEED), inter_quant: matrices(&B_INTER_SEED) }
    }

    fn refill(&mut self, br: &mut BitReader) -> Result<()> {
        for (b, &(bits, signed)) in self.bundles.iter_mut().zip(&OLD_FIELDS) {
            if let Some(n) = b.chunk(br, 0)? {
                let bias = if signed { 1 << (bits - 1) } else { 0 };
                for v in &mut b.data[..n] {
                    *v = (br.read(bits) as i32 - bias) as i16;
                }
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn next(&mut self, bundle: usize) -> i32 {
        i32::from(self.bundles[bundle].next())
    }

    fn color(&mut self) -> u8 {
        self.next(OLD_COLORS) as u8
    }

    /// Copies a block from elsewhere in the frame being decoded. Vectors leaving the plane are ignored,
    /// leaving the block as it was.
    fn motion(&mut self, dst: &mut [u8], off: usize, stride: usize, last_block: usize, y_bias: isize) {
        let x = self.next(OLD_X) as isize;
        let y = self.next(OLD_Y) as isize + y_bias;
        let src = off as isize + x + y * stride as isize;
        if src >= 0 && src as usize <= last_block {
            let src = src as usize;
            let mut block = [0u8; 64];
            for (r, row) in block.chunks_exact_mut(8).enumerate() {
                row.copy_from_slice(&dst[src + r * stride..src + r * stride + 8]);
            }
            put_block(dst, off, stride, &block);
        }
    }
}

/// Decodes one plane of a revision `b` frame in place.
fn decode_plane_old(br: &mut BitReader, ob: &mut OldBundles, plane: &mut Plane, key: bool) -> Result<()> {
    let (blocks_w, blocks_h, stride) = (plane.blocks_w, plane.blocks_h, plane.stride);
    for b in &mut ob.bundles {
        b.reset(13);
    }
    let last_block = (blocks_h - 1) * 8 * stride + (blocks_w - 1) * 8;
    // Motion in the first frame refers to rows above the block.
    let y_bias = if key { -15 } else { 0 };
    let dst = &mut plane.data[..];
    for by in 0..blocks_h {
        ob.refill(br)?;
        for bx in 0..blocks_w {
            let off = by * 8 * stride + bx * 8;
            match ob.next(OLD_TYPES) {
                0 => {}
                1 => {
                    let scan = &PATTERNS[br.read(4) as usize];
                    let mut block = [0u8; 64];
                    let mut i = 0;
                    while i < 63 {
                        let repeat = br.bit();
                        let run = br.read(u32::from(B_RUN_BITS[i])) as usize + 1;
                        ensure!(i + run <= 64, "run block overflows");
                        if repeat {
                            let v = ob.color();
                            for &p in &scan[i..i + run] {
                                block[p as usize] = v;
                            }
                        } else {
                            for &p in &scan[i..i + run] {
                                block[p as usize] = ob.color();
                            }
                        }
                        i += run;
                    }
                    if i == 63 {
                        block[scan[63] as usize] = ob.color();
                    }
                    put_block(dst, off, stride, &block);
                }
                2 => {
                    let mut coefs = [0i32; 64];
                    coefs[0] = ob.next(OLD_INTRA_DC);
                    let quant = ob.next(OLD_INTRA_Q) as u32;
                    let ac = read_dct(br, &mut coefs, Some(quant), &ob.intra_quant)?;
                    put_pixels(dst, off, stride, &dct_pixels(&coefs, ac));
                }
                3 => {
                    ob.motion(dst, off, stride, last_block, y_bias);
                    let mut residue = [0i16; 64];
                    let masks = ob.next(OLD_INTER_COEFS);
                    read_residue(br, &mut residue, masks);
                    add_residue(dst, off, stride, &residue);
                }
                4 => {
                    ob.motion(dst, off, stride, last_block, y_bias);
                    let mut coefs = [0i32; 64];
                    coefs[0] = ob.next(OLD_INTER_DC);
                    let quant = ob.next(OLD_INTER_Q) as u32;
                    let ac = read_dct(br, &mut coefs, Some(quant), &ob.inter_quant)?;
                    add_pixels(dst, off, stride, &dct_pixels(&coefs, ac));
                }
                5 => fill_block(dst, off, stride, ob.color()),
                6 => {
                    let pair = [ob.color(), ob.color()];
                    let mut block = [0u8; 64];
                    for row in block.chunks_exact_mut(8) {
                        let mut bits = ob.next(OLD_PATTERN);
                        for px in row {
                            *px = pair[(bits & 1) as usize];
                            bits >>= 1;
                        }
                    }
                    put_block(dst, off, stride, &block);
                }
                7 => ob.motion(dst, off, stride, last_block, y_bias),
                8 => {
                    let block = std::array::from_fn(|_| ob.color());
                    put_block(dst, off, stride, &block);
                }
                kind => bail!("unknown block type {kind}"),
            }
        }
    }
    br.align32();
    Ok(())
}

/// Decoder state for one video stream.
pub(crate) struct VideoDecoder {
    revision: u8,
    width: u32,
    /// The most recently decoded frame.
    front: Frame,
    /// The frame being decoded; later revisions decode into a second buffer while referring to the first.
    back: Frame,
    bundles: Bundles,
    old: Option<OldBundles>,
    /// Frames decoded since the start, which tells revision `b` whether it is on its first frame.
    decoded: u32,
    /// How many blocks of each kind have been decoded, then how many scaled blocks of each sub-kind.
    pub counts: [u64; 20],
}

impl VideoDecoder {
    pub fn new(revision: u8, width: u32, height: u32, alpha: bool, grey: bool) -> Self {
        let frame = Frame::new(width, height, revision == b'k', alpha, grey);
        Self {
            revision,
            width,
            back: frame.clone(),
            front: frame,
            bundles: Bundles::new(width as usize),
            old: (revision == b'b').then(OldBundles::new),
            decoded: 0,
            counts: [0; 20],
        }
    }

    pub fn frame(&self) -> &Frame {
        &self.front
    }

    /// Forgets the position in the stream so the next frame decoded is treated as the first.
    pub fn restart(&mut self) {
        self.decoded = 0;
    }

    /// Decodes one video packet. On failure the previous frame stays current.
    pub fn decode(&mut self, packet: &[u8]) -> Result<()> {
        let mut br = BitReader::new(packet);
        let revision = self.revision;
        let width = self.width as usize;
        if revision >= b'i' && self.front.alpha.is_some() {
            br.skip(32);
        }
        if self.old.is_some() {
            if let Some(alpha) = &mut self.front.alpha {
                // Revision `b` decodes in place, so the alpha plane refers to a copy of itself.
                let prev = alpha.clone();
                decode_plane(&mut br, &mut self.bundles, revision, alpha, &prev, width, &mut self.counts)?;
            }
        } else if let (Some(dst), Some(prev)) = (&mut self.back.alpha, &self.front.alpha) {
            decode_plane(&mut br, &mut self.bundles, revision, dst, prev, width, &mut self.counts)?;
        }
        if revision >= b'i' {
            br.skip(32);
        }
        self.decoded += 1;
        // From revision `h` the red-difference plane is coded before the blue-difference one.
        let order = if revision >= b'h' { [0, 2, 1] } else { [0, 1, 2] };
        for (i, &plane) in order.iter().enumerate() {
            let plane_width = if i == 0 { width } else { width >> 1 };
            match &mut self.old {
                Some(old) => decode_plane_old(&mut br, old, &mut self.front.planes[plane], self.decoded == 1)?,
                None => {
                    let (dst, prev) = (&mut self.back.planes[plane], &self.front.planes[plane]);
                    decode_plane(&mut br, &mut self.bundles, revision, dst, prev, plane_width, &mut self.counts)?;
                }
            }
            // Grey-scale files stop after the luma plane.
            if br.left() <= 0 {
                break;
            }
        }
        if self.old.is_none() {
            std::mem::swap(&mut self.front, &mut self.back);
        }
        Ok(())
    }
}
