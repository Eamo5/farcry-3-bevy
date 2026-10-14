//! The interface movies' fonts (`.ffd`): a glyph table over distance-field
//! atlas pages (`<name>_<n>.png`, shipped as `.xbt`, the field in alpha,
//! the glyph's edge at one half).
//!
//! Layout (little-endian): byte 3 is the page count; at 0x30 the face name
//! (its length in a byte, NUL included); then the glyph count (u16), 4 bytes
//! per glyph and 4 more (unused here), each glyph's character (u16), the
//! ascent and descent (u16, summing to the em of 20480) and a u16, each
//! glyph's advance (u16, in 1/20480 em), a u16 per glyph and two bytes
//! (unused), the pages' paths (NUL-terminated), and then a 27-byte record per
//! glyph: character (u16), page (u8), its rectangle on the page (u0, v0, u1,
//! v1 as f32), and its cell's offset from the pen and the line's top and its
//! size (i16 each, in eighths of a page pixel). Three bytes close the file.
//!
//! [`TracedFont`] traces each glyph's edge into outlines, and
//! [`TracedFont::define_font3`] writes them as a SWF font (`DefineFont3`), so
//! a Flash player draws the game's own faces.

use anyhow::{bail, ensure, Context, Result};

/// Units per em in `DefineFont3` (and the file's advances).
pub const EM: f32 = 20480.0;
/// Where the distance field crosses the glyph's edge.
const EDGE: f32 = 0.5;
/// Samples per page pixel when tracing.
const SUPERSAMPLE: usize = 4;
/// Points closer than this (page pixels) to the line through their
/// neighbours are dropped.
const SIMPLIFY: f32 = 0.06;

#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    pub code: u16,
    /// In 1/20480 em.
    pub advance: u16,
    pub page: u8,
    /// u0, v0, u1, v1 on its page.
    pub uv: [f32; 4],
    /// Offset from the pen (x) and the line's top (y), and size, in eighths
    /// of a page pixel.
    pub cell: [i16; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Font {
    pub face: String,
    pub ascent: u16,
    pub descent: u16,
    /// As the file names them (`UI\...\name_1.png`).
    pub pages: Vec<String>,
    pub glyphs: Vec<Glyph>,
}

impl Font {
    /// The archive path of a page's texture (`ui\...\name_1.xbt`).
    pub fn page_texture(&self, page: usize) -> Option<String> {
        let lower = self.pages.get(page)?.to_ascii_lowercase();
        let stem = lower.rsplit_once('.').map_or(lower.as_str(), |(s, _)| s);
        Some(format!("{stem}.xbt"))
    }
}

pub fn parse(d: &[u8]) -> Result<Font> {
    let u16_at = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).context("font file truncated");
    ensure!(d.len() > 0x31, "too short for a font");
    let pages = d[3] as usize;
    let name_len = d[0x30] as usize;
    let name = d.get(0x31..0x31 + name_len).context("font name truncated")?;
    let face = String::from_utf8_lossy(name.split(|&c| c == 0).next().unwrap_or(name)).into_owned();
    let mut o = 0x31 + name_len;
    let count = u16_at(o)? as usize;
    o += 2 + 4 * count + 4;
    let chars: Vec<u16> = (0..count).map(|i| u16_at(o + 2 * i)).collect::<Result<_>>()?;
    o += 2 * count;
    let (ascent, descent) = (u16_at(o)?, u16_at(o + 2)?);
    o += 6;
    let advances: Vec<u16> = (0..count).map(|i| u16_at(o + 2 * i)).collect::<Result<_>>()?;
    o += 4 * count + 2;
    let mut paths = Vec::new();
    for _ in 0..pages {
        let n = d.get(o..).and_then(|r| r.iter().position(|&c| c == 0)).context("page path truncated")?;
        paths.push(String::from_utf8_lossy(&d[o..o + n]).into_owned());
        o += n + 1;
    }
    if o + 27 * count + 3 != d.len() {
        bail!("glyph records at {o:#x} don't end the file ({count} glyphs, {} bytes)", d.len());
    }
    let mut glyphs = Vec::with_capacity(count);
    for i in 0..count {
        let r = &d[o + 27 * i..o + 27 * (i + 1)];
        let f = |k: usize| f32::from_le_bytes(r[3 + 4 * k..7 + 4 * k].try_into().unwrap());
        let s = |k: usize| i16::from_le_bytes([r[19 + 2 * k], r[20 + 2 * k]]);
        let code = u16::from_le_bytes([r[0], r[1]]);
        // The advance table runs in the character list's order.
        let advance = chars.iter().position(|&c| c == code).map_or(0, |k| advances[k]);
        glyphs.push(Glyph { code, advance, page: r[2], uv: [f(0), f(1), f(2), f(3)], cell: [s(0), s(1), s(2), s(3)] });
    }
    glyphs.sort_by_key(|g| g.code);
    glyphs.dedup_by_key(|g| g.code);
    Ok(Font { face, ascent, descent, pages: paths, glyphs })
}

/// A page's distance field: width, height, one value (0..1) per pixel.
pub struct Page {
    pub width: u32,
    pub height: u32,
    pub field: Vec<f32>,
}

impl Page {
    /// From RGBA rows (the field in alpha).
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Self {
        Page { width, height, field: rgba.chunks_exact(4).map(|p| p[3] as f32 / 255.0).collect() }
    }

    /// Bilinear between pixel centres, nothing outside `clip` (x0, y0, x1, y1).
    fn sample(&self, x: f32, y: f32, clip: [f32; 4]) -> f32 {
        if x < clip[0] || y < clip[1] || x > clip[2] || y > clip[3] {
            return 0.0;
        }
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let at = |px: f32, py: f32| {
            let (px, py) = (px.clamp(0.0, self.width as f32 - 1.0) as usize, py.clamp(0.0, self.height as f32 - 1.0) as usize);
            self.field.get(py * self.width as usize + px).copied().unwrap_or(0.0)
        };
        let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1.0, y0) * tx;
        let bottom = at(x0, y0 + 1.0) * (1.0 - tx) + at(x0 + 1.0, y0 + 1.0) * tx;
        top * (1.0 - ty) + bottom * ty
    }
}

/// Closed contours around the samples at or above `edge` in a `w` x `h`
/// grid (row-major), as points in grid units, the inside on the left (y
/// down): outlines and holes wind opposite ways. Outside the grid is empty.
pub fn trace(field: &[f32], w: usize, h: usize, edge: f32) -> Vec<Vec<[f32; 2]>> {
    // The grid with a ring of empty samples, so every contour closes.
    let (pw, ph) = (w + 2, h + 2);
    let v = |x: usize, y: usize| if x == 0 || y == 0 || x > w || y > h { 0.0 } else { field[(y - 1) * w + (x - 1)] };
    // A crossing is named by the grid edge it lies on: horizontal edges
    // (x, y)-(x+1, y) first, then vertical ones (x, y)-(x, y+1).
    let horizontal = |x: usize, y: usize| y * pw + x;
    let vertical = |x: usize, y: usize| pw * ph + y * pw + x;
    let mut point = vec![[0f32; 2]; 2 * pw * ph];
    let mut next = vec![usize::MAX; 2 * pw * ph];
    for y in 0..ph - 1 {
        for x in 0..pw - 1 {
            let c = [v(x, y), v(x + 1, y), v(x + 1, y + 1), v(x, y + 1)];
            let inside = |i: usize| c[i] >= edge;
            let case = (0..4).filter(|&i| inside(i)).fold(0, |m, i| m | (1 << i));
            if case == 0 || case == 15 {
                continue;
            }
            let corner = [[x, y], [x + 1, y], [x + 1, y + 1], [x, y + 1]];
            // Sides: top, right, bottom, left (corners i to i + 1).
            let side = [horizontal(x, y), vertical(x + 1, y), horizontal(x, y + 1), vertical(x, y)];
            for s in 0..4 {
                let (i, j) = (s, (s + 1) % 4);
                if inside(i) != inside(j) {
                    let t = (edge - c[i]) / (c[j] - c[i]);
                    let (a, b) = (corner[i], corner[j]);
                    point[side[s]] = [a[0] as f32 + (b[0] as f32 - a[0] as f32) * t, a[1] as f32 + (b[1] as f32 - a[1] as f32) * t];
                }
            }
            let joined = c.iter().sum::<f32>() / 4.0 >= edge;
            let links: &[(usize, usize)] = match case {
                1 => &[(3, 0)],
                2 => &[(0, 1)],
                3 => &[(3, 1)],
                4 => &[(1, 2)],
                5 if joined => &[(1, 0), (3, 2)],
                5 => &[(3, 0), (1, 2)],
                6 => &[(0, 2)],
                7 => &[(3, 2)],
                8 => &[(2, 3)],
                9 => &[(2, 0)],
                10 if joined => &[(0, 3), (2, 1)],
                10 => &[(0, 1), (2, 3)],
                11 => &[(2, 1)],
                12 => &[(1, 3)],
                13 => &[(1, 0)],
                _ => &[(0, 3)],
            };
            for &(a, b) in links {
                next[side[a]] = side[b];
            }
        }
    }
    let mut seen = vec![false; next.len()];
    let mut loops = Vec::new();
    for start in 0..next.len() {
        if next[start] == usize::MAX || seen[start] {
            continue;
        }
        let mut contour = Vec::new();
        let mut at = start;
        while at != usize::MAX && !seen[at] {
            seen[at] = true;
            let [px, py] = point[at];
            contour.push([px - 1.0, py - 1.0]);
            at = next[at];
        }
        if contour.len() > 2 {
            loops.push(contour);
        }
    }
    loops
}

/// Drops points that lie within `tolerance` of the line from the last kept
/// point to the next one.
fn simplify(contour: &[[f32; 2]], tolerance: f32) -> Vec<[f32; 2]> {
    let n = contour.len();
    let mut out = vec![contour[0]];
    for i in 1..n {
        let (a, b, c) = (*out.last().unwrap(), contour[i], contour[(i + 1) % n]);
        let (dx, dy) = (c[0] - a[0], c[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt();
        let off = if len > 1e-6 { ((b[0] - a[0]) * dy - (b[1] - a[1]) * dx).abs() / len } else { 0.0 };
        if off >= tolerance {
            out.push(b);
        }
    }
    if out.len() >= 3 {
        out
    } else {
        contour.to_vec()
    }
}

/// A font's glyphs as outlines, in em units (20480) from the pen and the
/// baseline (y down).
pub struct TracedFont {
    pub face: String,
    pub ascent: u16,
    pub descent: u16,
    /// Code, advance, contours.
    pub glyphs: Vec<(u16, u16, Vec<Vec<[i32; 2]>>)>,
}

/// A glyph's contours in page pixels from its cell's origin (the pen and
/// the line's top).
fn glyph_contours(g: &Glyph, page: &Page) -> Vec<Vec<[f32; 2]>> {
    let (pw, ph) = (page.width as f32, page.height as f32);
    let rect = [g.uv[0] * pw, g.uv[1] * ph, g.uv[2] * pw, g.uv[3] * ph];
    let (rw, rh) = (rect[2] - rect[0], rect[3] - rect[1]);
    if rw <= 0.0 || rh <= 0.0 {
        return Vec::new();
    }
    let (x0, y0) = (rect[0].floor() - 1.0, rect[1].floor() - 1.0);
    let step = 1.0 / SUPERSAMPLE as f32;
    let (w, h) = (((rect[2].ceil() + 1.0 - x0) / step) as usize + 1, ((rect[3].ceil() + 1.0 - y0) / step) as usize + 1);
    let mut field = Vec::with_capacity(w * h);
    for j in 0..h {
        for i in 0..w {
            field.push(page.sample(x0 + i as f32 * step, y0 + j as f32 * step, rect));
        }
    }
    // Cell units are eighths of a page pixel; the rectangle can be a little
    // off the cell's size, so scale to it.
    let (sx, sy) = (g.cell[2] as f32 / 8.0 / rw, g.cell[3] as f32 / 8.0 / rh);
    trace(&field, w, h, EDGE)
        .iter()
        .map(|c| {
            simplify(c, SIMPLIFY * SUPERSAMPLE as f32)
                .iter()
                .map(|&[gx, gy]| {
                    let (px, py) = (x0 + gx * step - rect[0], y0 + gy * step - rect[1]);
                    [g.cell[0] as f32 / 8.0 + px * sx, g.cell[1] as f32 / 8.0 + py * sy]
                })
                .collect()
        })
        .collect()
}

impl TracedFont {
    /// Traces every glyph on the pages given (by page index). The pages'
    /// scale is found from the glyphs that sit on the baseline.
    pub fn trace(font: &Font, pages: &[Option<Page>]) -> Result<Self> {
        let contours: Vec<Vec<Vec<[f32; 2]>>> =
            font.glyphs.iter().map(|g| pages.get(g.page as usize).and_then(Option::as_ref).map_or_else(Vec::new, |p| glyph_contours(g, p))).collect();
        // The baseline: where flat-bottomed capitals and lower case end.
        let mut bottoms: Vec<f32> = font
            .glyphs
            .iter()
            .zip(&contours)
            .filter(|(g, c)| b"HEILTxzvw".contains(&(g.code.min(255) as u8)) && g.code < 128 && !c.is_empty())
            .map(|(_, c)| c.iter().flatten().map(|p| p[1]).fold(f32::MIN, f32::max))
            .collect();
        ensure!(!bottoms.is_empty(), "{}: no glyph to find the baseline by", font.face);
        bottoms.sort_by(f32::total_cmp);
        let baseline = bottoms[bottoms.len() / 2];
        let em_px = baseline / (font.ascent as f32 / EM);
        ensure!(em_px > 1.0, "{}: baseline {baseline} px", font.face);
        let units = |p: [f32; 2]| [(p[0] / em_px * EM).round() as i32, ((p[1] - baseline) / em_px * EM).round() as i32];
        let glyphs = font
            .glyphs
            .iter()
            .zip(contours)
            .map(|(g, cs)| {
                let cs = cs
                    .into_iter()
                    .map(|c| {
                        let mut pts: Vec<[i32; 2]> = c.into_iter().map(units).collect();
                        pts.dedup();
                        while pts.len() > 1 && pts.first() == pts.last() {
                            pts.pop();
                        }
                        pts
                    })
                    .filter(|c| c.len() >= 3)
                    .collect();
                (g.code, g.advance, cs)
            })
            .collect();
        Ok(TracedFont { face: font.face.clone(), ascent: font.ascent, descent: font.descent, glyphs })
    }

    /// The body of a `DefineFont3` tag (75) for this font as character `id`.
    pub fn define_font3(&self, id: u16, name: &str) -> Vec<u8> {
        let shapes: Vec<Vec<u8>> = self.glyphs.iter().map(|(_, _, cs)| glyph_shape(cs)).collect();
        let n = shapes.len();
        let total: usize = shapes.iter().map(Vec::len).sum();
        let wide = 4 * (n + 1) + total > 0xffff;
        let entry = if wide { 4 } else { 2 };
        let mut out = Vec::with_capacity(total + 32 + n * 16);
        out.extend_from_slice(&id.to_le_bytes());
        // Has layout, wide codes (and offsets), bold by name.
        let bold = name.to_ascii_lowercase().contains("bold");
        out.push(0x80 | 0x04 | if wide { 0x08 } else { 0 } | bold as u8);
        out.push(1); // Latin
        let name = name.as_bytes();
        out.push(name.len().min(255) as u8);
        out.extend_from_slice(&name[..name.len().min(255)]);
        out.extend_from_slice(&(n as u16).to_le_bytes());
        let mut offset = entry * (n + 1);
        let mut offsets = Vec::with_capacity(entry * (n + 1));
        for s in &shapes {
            push_offset(&mut offsets, offset, wide);
            offset += s.len();
        }
        push_offset(&mut offsets, offset, wide); // the code table
        out.extend_from_slice(&offsets);
        for s in &shapes {
            out.extend_from_slice(s);
        }
        for (code, _, _) in &self.glyphs {
            out.extend_from_slice(&code.to_le_bytes());
        }
        out.extend_from_slice(&self.ascent.to_le_bytes());
        out.extend_from_slice(&self.descent.to_le_bytes());
        out.extend_from_slice(&0i16.to_le_bytes()); // leading
        for (_, advance, _) in &self.glyphs {
            out.extend_from_slice(&(*advance).min(i16::MAX as u16).to_le_bytes());
        }
        for (_, _, cs) in &self.glyphs {
            let mut b = Bits::default();
            let (mut lo, mut hi) = ([i32::MAX; 2], [i32::MIN; 2]);
            for p in cs.iter().flatten() {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
            if lo[0] > hi[0] {
                (lo, hi) = ([0; 2], [0; 2]);
            }
            b.rect(lo[0], hi[0], lo[1], hi[1]);
            out.extend_from_slice(&b.finish());
        }
        out.extend_from_slice(&0u16.to_le_bytes()); // no kerning
        out
    }
}

fn push_offset(out: &mut Vec<u8>, offset: usize, wide: bool) {
    if wide {
        out.extend_from_slice(&(offset as u32).to_le_bytes());
    } else {
        out.extend_from_slice(&(offset as u16).to_le_bytes());
    }
}

/// A glyph's SHAPE: one fill style (FillStyle0 = 1 on the first move), each
/// contour a move and straight edges back to its start.
fn glyph_shape(contours: &[Vec<[i32; 2]>]) -> Vec<u8> {
    let mut b = Bits::default();
    b.ub(4, 1); // fill bits
    b.ub(4, 0); // line bits
    let mut first = true;
    for c in contours {
        // Style change: move, and the fill on the first.
        b.ub(1, 0);
        b.ub(1, 0); // new styles
        b.ub(1, 0); // line style
        b.ub(1, 0); // fill style 1
        b.ub(1, first as u32); // fill style 0
        b.ub(1, 1); // move to
        let (mx, my) = (c[0][0], c[0][1]);
        let bits = sbits(mx).max(sbits(my));
        b.ub(5, bits);
        b.sb(bits, mx);
        b.sb(bits, my);
        if first {
            b.ub(1, 1);
            first = false;
        }
        let mut at = c[0];
        for p in c[1..].iter().chain(std::iter::once(&c[0])) {
            let (dx, dy) = (p[0] - at[0], p[1] - at[1]);
            if dx == 0 && dy == 0 {
                continue;
            }
            let bits = sbits(dx).max(sbits(dy)).max(2);
            b.ub(1, 1); // edge
            b.ub(1, 1); // straight
            b.ub(4, bits - 2);
            if dx != 0 && dy != 0 {
                b.ub(1, 1); // general line
                b.sb(bits, dx);
                b.sb(bits, dy);
            } else {
                b.ub(1, 0);
                b.ub(1, (dx == 0) as u32); // vertical
                b.sb(bits, if dx == 0 { dy } else { dx });
            }
            at = *p;
        }
    }
    b.ub(6, 0); // end of shape
    b.finish()
}

/// Bits needed for a signed value.
fn sbits(v: i32) -> u32 {
    if v == 0 {
        return 1;
    }
    let mag = if v < 0 { !v } else { v } as u32;
    33 - mag.leading_zeros()
}

/// A big-endian bit writer (SWF's bit fields).
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    used: u32,
}

impl Bits {
    fn ub(&mut self, n: u32, v: u32) {
        for i in (0..n).rev() {
            if self.used % 8 == 0 {
                self.bytes.push(0);
            }
            let bit = (v >> i) & 1;
            *self.bytes.last_mut().unwrap() |= (bit as u8) << (7 - self.used % 8);
            self.used += 1;
        }
    }

    fn sb(&mut self, n: u32, v: i32) {
        self.ub(n, (v as u32) & if n >= 32 { u32::MAX } else { (1 << n) - 1 });
    }

    fn rect(&mut self, x0: i32, x1: i32, y0: i32, y1: i32) {
        let n = [x0, x1, y0, y1].iter().map(|&v| sbits(v)).max().unwrap();
        self.ub(5, n);
        for v in [x0, x1, y0, y1] {
            self.sb(n, v);
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A font file of two glyphs on one page (values made up).
    fn file() -> Vec<u8> {
        let mut d = vec![7, 0x40, 0x0b, 1, 2, 0, 32, 0];
        d.resize(0x30, 0);
        let name = b"Made Up\0";
        d.push(name.len() as u8);
        d.extend_from_slice(name);
        d.extend_from_slice(&2u16.to_le_bytes());
        d.extend_from_slice(&[0; 4 * 2 + 4]);
        for c in [0x49u16, 0x2e] {
            d.extend_from_slice(&c.to_le_bytes());
        }
        d.extend_from_slice(&16000u16.to_le_bytes());
        d.extend_from_slice(&4480u16.to_le_bytes());
        d.extend_from_slice(&0u16.to_le_bytes());
        for a in [6000u16, 5000] {
            d.extend_from_slice(&a.to_le_bytes());
        }
        d.extend_from_slice(&[8, 0, 8, 0, 0, 0]);
        d.extend_from_slice(b"UI\\Fonts\\Made_Up_1.png\0");
        for (code, uv, cell) in [(0x2eu16, [0.5f32, 0.0, 0.75, 0.25], [-8i16, 80, 64, 64]), (0x49, [0.0, 0.0, 0.5, 1.0], [-8, 0, 128, 256])] {
            d.extend_from_slice(&code.to_le_bytes());
            d.push(0);
            for f in uv {
                d.extend_from_slice(&f.to_le_bytes());
            }
            for s in cell {
                d.extend_from_slice(&s.to_le_bytes());
            }
        }
        d.extend_from_slice(&[1, 6, 8]);
        d
    }

    #[test]
    fn a_font_file_gives_its_glyphs_with_their_advances() {
        let f = parse(&file()).unwrap();
        assert_eq!(f.face, "Made Up");
        assert_eq!((f.ascent, f.descent), (16000, 4480));
        assert_eq!(f.page_texture(0).as_deref(), Some("ui\\fonts\\made_up_1.xbt"));
        assert_eq!(f.glyphs.len(), 2);
        // Sorted by character; advances follow the character list.
        assert_eq!((f.glyphs[0].code, f.glyphs[0].advance, f.glyphs[0].cell), (0x2e, 5000, [-8, 80, 64, 64]));
        assert_eq!((f.glyphs[1].code, f.glyphs[1].advance, f.glyphs[1].uv[2]), (0x49, 6000, 0.5));
    }

    /// Signed area (positive: clockwise on screen, y down).
    fn area(c: &[[f32; 2]]) -> f32 {
        (0..c.len()).map(|i| (c[i][0] * c[(i + 1) % c.len()][1] - c[(i + 1) % c.len()][0] * c[i][1]) / 2.0).sum()
    }

    #[test]
    fn a_ring_traces_to_an_outline_and_a_hole_wound_opposite_ways() {
        let (w, h) = (24, 24);
        let field: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 - 11.5, (i / w) as f32 - 11.5);
                let r = (x * x + y * y).sqrt();
                (0.5 - (r - 7.0).abs() / 6.0).clamp(0.0, 1.0)
            })
            .collect();
        let loops = trace(&field, w, h, 0.25);
        assert_eq!(loops.len(), 2);
        let (a, b) = (area(&loops[0]), area(&loops[1]));
        assert!(a * b < 0.0, "outline and hole wind the same way: {a} {b}");
        let (outer, inner) = if a.abs() > b.abs() { (a, b) } else { (b, a) };
        // Radii 8.5 and 5.5.
        assert!((outer.abs() - std::f32::consts::PI * 8.5 * 8.5).abs() < 8.0, "{outer}");
        assert!((inner.abs() - std::f32::consts::PI * 5.5 * 5.5).abs() < 8.0, "{inner}");
    }

    #[test]
    fn a_traced_bar_sits_on_the_baseline_in_em_units() {
        let font = parse(&file()).unwrap();
        // One 32 x 32 page: an "I" bar in the left half's cell, 2..14 px
        // across, 4..28 px down (the field ramping over a pixel either side).
        let (w, h) = (32u32, 32u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..16 {
                let d = [x as f32 + 0.5 - 2.0, 14.0 - (x as f32 + 0.5), y as f32 + 0.5 - 4.0, 28.0 - (y as f32 + 0.5)].into_iter().fold(f32::MAX, f32::min);
                rgba[((y * w + x) * 4 + 3) as usize] = ((0.5 + d * 0.25).clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
        let traced = TracedFont::trace(&font, &[Some(Page::from_rgba(w, h, &rgba))]).unwrap();
        let (code, advance, contours) = &traced.glyphs[1];
        assert_eq!((*code, *advance), (0x49, 6000));
        assert_eq!(contours.len(), 1);
        let ys: Vec<i32> = contours[0].iter().map(|p| p[1]).collect();
        // Its bottom is the baseline (28 px down, so 28 px is the ascent and
        // the em 35.84 px); its top 24 px above.
        assert!(ys.iter().max().unwrap().abs() <= 60, "{ys:?}");
        assert!((ys.iter().min().unwrap() + 13714).abs() <= 120, "{ys:?}");
        let body = traced.define_font3(7, "Made Up");
        assert_eq!(&body[..2], &7u16.to_le_bytes());
        assert_eq!(body[2] & 0x84, 0x84);
        assert_eq!(&body[4..4 + 1 + 7][1..], b"Made Up");
    }
}
