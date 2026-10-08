//! The baked world map: a top-down picture of the whole island for the
//! in-game map and minimap, north up, 2 m per pixel.
//!
//! The converter shades it from the terrain's baked colour atlas, its heights
//! (hill shading, cliffs) and the sea, then stores it as a BC1 DDS with mips.

use crate::terrain::{SECTOR_SIZE, WORLD_SECTORS};

/// Map width and height in pixels.
pub const MAP_SIZE: u32 = 5120;
/// Ground metres per map pixel.
pub const METRES_PER_PIXEL: f32 = 2.0;
/// The map spans -HALF_EXTENT..HALF_EXTENT metres on both Dunia axes.
pub const HALF_EXTENT: f32 = WORLD_SECTORS as f32 * SECTOR_SIZE / 2.0;
/// Sea level (Dunia Z, metres).
pub const SEA_LEVEL: f32 = 100.0;

/// The far south-east corner holds the developers' flat test areas, which
/// aren't part of the islands (Dunia x, y in metres).
pub fn in_test_area(x: f32, y: f32) -> bool {
    x >= 2048.0 && y <= -2048.0
}

/// Map position (pixels, fractional; origin top-left, north up) of a Dunia (x, y).
pub fn to_map(x: f32, y: f32) -> (f32, f32) {
    ((x + HALF_EXTENT) / METRES_PER_PIXEL, (HALF_EXTENT - y) / METRES_PER_PIXEL)
}

/// Dunia (x, y) at a map position in pixels.
pub fn from_map(px: f32, py: f32) -> (f32, f32) {
    (px * METRES_PER_PIXEL - HALF_EXTENT, HALF_EXTENT - py * METRES_PER_PIXEL)
}

/// Colour of one map pixel from the ground colour, height and surface normal
/// (Dunia space, Z up). Land gets hill shading; water is tinted by depth.
pub fn shade(ground: [u8; 3], height: f32, normal: [f32; 3]) -> [u8; 3] {
    let light = {
        let l = [-0.55f32, 0.55, 0.62];
        let n = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
        [l[0] / n, l[1] / n, l[2] / n]
    };
    let lambert = (normal[0] * light[0] + normal[1] * light[1] + normal[2] * light[2]).max(0.0);
    let flat = light[2];
    // Relative to flat ground, so plains keep their colour and slopes facing
    // away from the light darken.
    let hill = (0.95 + 0.6 * (lambert - flat)).clamp(0.45, 1.3);
    let steep = (1.0 - normal[2]).clamp(0.0, 1.0);
    let mut c = ground.map(|v| v as f32 / 255.0);
    // Cliffs read as grey rock.
    for ch in &mut c {
        *ch = *ch * (1.0 - steep * 0.5) + 0.45 * steep * 0.5;
    }
    let land = c.map(|v| (v * hill).clamp(0.0, 1.0));
    if height >= SEA_LEVEL {
        return land.map(|v| (v * 255.0).round() as u8);
    }
    let depth = SEA_LEVEL - height;
    let shallow = [0.25f32, 0.62, 0.66];
    let deep = [0.05f32, 0.2, 0.38];
    let t = (depth / 30.0).clamp(0.0, 1.0);
    let water: [f32; 3] = std::array::from_fn(|k| shallow[k] * (1.0 - t) + deep[k] * t);
    // A little of the sea floor shows through in the shallows.
    let see = (1.0 - depth / 6.0).clamp(0.0, 0.5);
    std::array::from_fn(|k| ((water[k] * (1.0 - see) + land[k] * see) * 255.0).round() as u8)
}

/// Colour of open sea with no terrain data.
pub const OPEN_SEA: [u8; 3] = [13, 51, 97];

/// Encodes RGBA8 pixels (`w` x `h`, both multiples of 4) as a BC1 DDS with a
/// full mip chain (box-filtered).
pub fn encode_dds(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    assert!(w % 4 == 0 && h % 4 == 0 && rgba.len() == (w * h * 4) as usize);
    let mut levels = vec![(w, h, rgba.to_vec())];
    while let Some((lw, lh, data)) = levels.last() {
        if *lw <= 4 || *lh <= 4 {
            break;
        }
        let (nw, nh) = (lw / 2, lh / 2);
        let mut next = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh as usize {
            for x in 0..nw as usize {
                for ch in 0..4 {
                    let at = |dx: usize, dy: usize| data[((y * 2 + dy) * *lw as usize + x * 2 + dx) * 4 + ch] as u32;
                    next[(y * nw as usize + x) * 4 + ch] = ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8;
                }
            }
        }
        levels.push((nw, nh, next));
    }
    let mut out = dds_header(w, h, levels.len() as u32);
    for (lw, lh, data) in &levels {
        out.extend_from_slice(&encode_bc1(data, *lw, *lh));
    }
    out
}

fn dds_header(w: u32, h: u32, mips: u32) -> Vec<u8> {
    let mut d = Vec::with_capacity(128);
    let mut u = |v: u32| d.extend_from_slice(&v.to_le_bytes());
    u(u32::from_le_bytes(*b"DDS "));
    u(124);
    // CAPS | HEIGHT | WIDTH | PIXELFORMAT | MIPMAPCOUNT | LINEARSIZE
    u(0x1 | 0x2 | 0x4 | 0x1000 | 0x20000 | 0x80000);
    u(h);
    u(w);
    u(w.div_ceil(4) * h.div_ceil(4) * 8);
    u(0);
    u(mips);
    for _ in 0..11 {
        u(0);
    }
    // Pixel format: FourCC DXT1.
    u(32);
    u(0x4);
    u(u32::from_le_bytes(*b"DXT1"));
    for _ in 0..5 {
        u(0);
    }
    // TEXTURE | COMPLEX | MIPMAP
    u(0x1000 | 0x8 | 0x400000);
    for _ in 0..4 {
        u(0);
    }
    d
}

fn to565(c: [f32; 3]) -> u16 {
    let r = (c[0].clamp(0.0, 255.0) * 31.0 / 255.0).round() as u16;
    let g = (c[1].clamp(0.0, 255.0) * 63.0 / 255.0).round() as u16;
    let b = (c[2].clamp(0.0, 255.0) * 31.0 / 255.0).round() as u16;
    (r << 11) | (g << 5) | b
}

fn from565(c: u16) -> [f32; 3] {
    let r = ((c >> 11) & 31) as f32;
    let g = ((c >> 5) & 63) as f32;
    let b = (c & 31) as f32;
    [r * 255.0 / 31.0, g * 255.0 / 63.0, b * 255.0 / 31.0]
}

/// BC1 blocks for one image level (opaque, four-colour mode). Endpoints are
/// the extremes of the block's colours along their principal axis.
pub fn encode_bc1(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (bw, bh) = (w.div_ceil(4) as usize, h.div_ceil(4) as usize);
    let mut out = Vec::with_capacity(bw * bh * 8);
    for by in 0..bh {
        for bx in 0..bw {
            let mut px = [[0f32; 3]; 16];
            for (i, p) in px.iter_mut().enumerate() {
                let x = (bx * 4 + i % 4).min(w as usize - 1);
                let y = (by * 4 + i / 4).min(h as usize - 1);
                let o = (y * w as usize + x) * 4;
                *p = [rgba[o] as f32, rgba[o + 1] as f32, rgba[o + 2] as f32];
            }
            out.extend_from_slice(&encode_block(&px));
        }
    }
    out
}

fn encode_block(px: &[[f32; 3]; 16]) -> [u8; 8] {
    let mean: [f32; 3] = std::array::from_fn(|k| px.iter().map(|p| p[k]).sum::<f32>() / 16.0);
    let mut cov = [[0f32; 3]; 3];
    for p in px {
        let d: [f32; 3] = std::array::from_fn(|k| p[k] - mean[k]);
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    let mut axis = [1.0f32, 1.0, 1.0];
    for _ in 0..6 {
        let n: [f32; 3] = std::array::from_fn(|i| cov[i][0] * axis[0] + cov[i][1] * axis[1] + cov[i][2] * axis[2]);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len < 1e-6 {
            break;
        }
        axis = n.map(|v| v / len);
    }
    let proj = |p: &[f32; 3]| (p[0] - mean[0]) * axis[0] + (p[1] - mean[1]) * axis[1] + (p[2] - mean[2]) * axis[2];
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for p in px {
        let t = proj(p);
        lo = lo.min(t);
        hi = hi.max(t);
    }
    let end = |t: f32| -> [f32; 3] { std::array::from_fn(|k| mean[k] + axis[k] * t) };
    let (mut c0, mut c1) = (to565(end(hi)), to565(end(lo)));
    if c0 < c1 {
        std::mem::swap(&mut c0, &mut c1);
    }
    let mut block = [0u8; 8];
    block[0..2].copy_from_slice(&c0.to_le_bytes());
    block[2..4].copy_from_slice(&c1.to_le_bytes());
    if c0 == c1 {
        return block;
    }
    let (p0, p1) = (from565(c0), from565(c1));
    let palette: [[f32; 3]; 4] = [
        p0,
        p1,
        std::array::from_fn(|k| (2.0 * p0[k] + p1[k]) / 3.0),
        std::array::from_fn(|k| (p0[k] + 2.0 * p1[k]) / 3.0),
    ];
    let mut bits = 0u32;
    for (i, p) in px.iter().enumerate() {
        let best = (0..4)
            .min_by(|&a, &b| {
                let d = |c: &[f32; 3]| (0..3).map(|k| (c[k] - p[k]).powi(2)).sum::<f32>();
                d(&palette[a]).total_cmp(&d(&palette[b]))
            })
            .unwrap_or(0);
        bits |= (best as u32) << (2 * i);
    }
    block[4..8].copy_from_slice(&bits.to_le_bytes());
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_roundtrip() {
        let (px, py) = to_map(-290.7, 1719.4);
        let (x, y) = from_map(px, py);
        assert!((x + 290.7).abs() < 1e-3 && (y - 1719.4).abs() < 1e-3);
        assert_eq!(to_map(-HALF_EXTENT, HALF_EXTENT), (0.0, 0.0));
    }

    #[test]
    fn bc1_roundtrip_is_close() {
        let mut img = vec![0u8; 8 * 8 * 4];
        for y in 0..8 {
            for x in 0..8 {
                let o = (y * 8 + x) * 4;
                img[o..o + 4].copy_from_slice(&[(x * 30) as u8, (x * 20 + y / 4 * 40) as u8, 90, 255]);
            }
        }
        let dds = encode_dds(&img, 8, 8);
        let info = crate::texture::dds_info(&dds).unwrap();
        assert_eq!((info.width, info.height, &info.fourcc), (8, 8, b"DXT1"));
        assert_eq!(info.mips, 2);
        for (x, y) in [(0u32, 0u32), (7, 7), (3, 5)] {
            let c = crate::texture::bc1_texel(&dds, x, y).unwrap();
            let want = [(x * 30) as i32, (x * 20 + y / 4 * 40) as i32, 90];
            for k in 0..3 {
                assert!((c[k] as i32 - want[k]).abs() <= 24, "{x},{y}: {c:?} vs {want:?}");
            }
        }
    }

    #[test]
    fn shading() {
        let up = [0.0, 0.0, 1.0];
        assert_eq!(shade([100, 120, 80], 150.0, up), [95, 114, 76]);
        let sea = shade([100, 120, 80], 50.0, up);
        assert!(sea[2] > sea[0]);
    }
}
