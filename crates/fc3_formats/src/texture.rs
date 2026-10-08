//! XBT textures: a small "TBX\0" header wrapping a standard DDS file.

use anyhow::{bail, Result};

pub struct Xbt<'a> {
    /// Path of the companion `_mip0.xbt` holding the top mip level, if any.
    pub mip0_path: Option<String>,
    pub dds: &'a [u8],
}

pub fn parse(data: &[u8]) -> Result<Xbt<'_>> {
    if data.len() < 16 || &data[0..4] != b"TBX\0" {
        bail!("not an XBT");
    }
    let header_size = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    if header_size > data.len() || &data[header_size..header_size + 4] != b"DDS " {
        bail!("XBT: DDS payload not found at {header_size:#x}");
    }
    let path_bytes = &data[0x20.min(header_size)..header_size];
    let end = path_bytes.iter().position(|&c| c == 0).unwrap_or(path_bytes.len());
    let mip0 = String::from_utf8_lossy(&path_bytes[..end]).into_owned();
    Ok(Xbt { mip0_path: (!mip0.is_empty()).then_some(mip0), dds: &data[header_size..] })
}

/// Basic DDS header info.
#[derive(Debug, Clone, Copy)]
pub struct DdsInfo {
    pub width: u32,
    pub height: u32,
    pub mips: u32,
    pub fourcc: [u8; 4],
}

pub fn dds_info(dds: &[u8]) -> Option<DdsInfo> {
    if dds.len() < 128 || &dds[0..4] != b"DDS " {
        return None;
    }
    let rd = |o: usize| u32::from_le_bytes(dds[o..o + 4].try_into().unwrap());
    Some(DdsInfo { height: rd(12), width: rd(16), mips: rd(28).max(1), fourcc: dds[84..88].try_into().unwrap() })
}

/// Builds a single DDS from an XBT, prepending the separately stored top mip
/// (`*_mip0.xbt`) when available. Returns the DDS bytes.
pub fn assemble_dds(main_xbt: &[u8], mip0_xbt: Option<&[u8]>) -> Result<Vec<u8>> {
    let main = parse(main_xbt)?;
    let Some(mip0_bytes) = mip0_xbt else { return Ok(main.dds.to_vec()) };
    let top = parse(mip0_bytes)?;
    let (Some(mi), Some(ti)) = (dds_info(main.dds), dds_info(top.dds)) else {
        return Ok(main.dds.to_vec());
    };
    // Both must share the pixel format and the top level must be exactly 2x.
    if mi.fourcc != ti.fourcc || ti.width != mi.width * 2 || ti.height != mi.height * 2 || main.dds.len() < 128 {
        return Ok(main.dds.to_vec());
    }
    let header_len = dds_header_len(main.dds);
    let top_header_len = dds_header_len(top.dds);
    let mut out = Vec::with_capacity(main.dds.len() + top.dds.len());
    out.extend_from_slice(&main.dds[..header_len]);
    let put = |out: &mut Vec<u8>, o: usize, v: u32| out[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut out, 12, ti.height);
    put(&mut out, 16, ti.width);
    put(&mut out, 20, u32::from_le_bytes(top.dds[20..24].try_into().unwrap()));
    put(&mut out, 28, mi.mips + 1);
    out.extend_from_slice(&top.dds[top_header_len..]);
    out.extend_from_slice(&main.dds[header_len..]);
    Ok(out)
}

fn dds_header_len(dds: &[u8]) -> usize {
    // A "DX10" FourCC adds an extended header.
    if dds.len() >= 88 && &dds[84..88] == b"DX10" {
        148
    } else {
        128
    }
}

/// Decodes one texel of the top mip of a BC1 (DXT1) DDS. Returns RGB.
pub fn bc1_texel(dds: &[u8], x: u32, y: u32) -> Option<[u8; 3]> {
    let info = dds_info(dds)?;
    if &info.fourcc != b"DXT1" || x >= info.width || y >= info.height {
        return None;
    }
    let blocks_w = info.width.div_ceil(4) as usize;
    let block = (y as usize / 4) * blocks_w + (x as usize / 4);
    let o = 128 + block * 8;
    let b = dds.get(o..o + 8)?;
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let rgb = |c: u16| -> [i32; 3] {
        let r = ((c >> 11) & 31) as i32;
        let g = ((c >> 5) & 63) as i32;
        let bl = (c & 31) as i32;
        [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (bl << 3) | (bl >> 2)]
    };
    let (p0, p1) = (rgb(c0), rgb(c1));
    let palette: [[i32; 3]; 4] = if c0 > c1 {
        [p0, p1, std::array::from_fn(|k| (2 * p0[k] + p1[k]) / 3), std::array::from_fn(|k| (p0[k] + 2 * p1[k]) / 3)]
    } else {
        [p0, p1, std::array::from_fn(|k| (p0[k] + p1[k]) / 2), [0, 0, 0]]
    };
    let bits = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    let idx = ((bits >> (2 * ((y % 4) * 4 + (x % 4)))) & 3) as usize;
    let c = palette[idx];
    Some([c[0] as u8, c[1] as u8, c[2] as u8])
}

fn bc1_palette(b: &[u8], punch_alpha: bool) -> [[f32; 4]; 4] {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let rgb = |c: u16| -> [f32; 4] {
        let r = ((c >> 11) & 31) as f32 / 31.0;
        let g = ((c >> 5) & 63) as f32 / 63.0;
        let bl = (c & 31) as f32 / 31.0;
        [r, g, bl, 1.0]
    };
    let (p0, p1) = (rgb(c0), rgb(c1));
    let mix = |a: f32, x: [f32; 4], y: [f32; 4]| -> [f32; 4] { std::array::from_fn(|k| x[k] * (1.0 - a) + y[k] * a) };
    if c0 > c1 || !punch_alpha {
        [p0, p1, mix(1.0 / 3.0, p0, p1), mix(2.0 / 3.0, p0, p1)]
    } else {
        [p0, p1, mix(0.5, p0, p1), [0.0, 0.0, 0.0, 0.0]]
    }
}

/// Eight interpolated values from a BC3/BC4 alpha block.
fn bc4_values(b: &[u8]) -> [f32; 8] {
    let (a0, a1) = (b[0] as f32 / 255.0, b[1] as f32 / 255.0);
    let mut v = [a0, a1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    if b[0] > b[1] {
        for i in 1..7 {
            v[i + 1] = (a0 * (7 - i) as f32 + a1 * i as f32) / 7.0;
        }
    } else {
        for i in 1..5 {
            v[i + 1] = (a0 * (5 - i) as f32 + a1 * i as f32) / 5.0;
        }
        v[6] = 0.0;
        v[7] = 1.0;
    }
    v
}

fn bc4_texel(b: &[u8], t: usize) -> f32 {
    let bits = u64::from_le_bytes([b[2], b[3], b[4], b[5], b[6], b[7], 0, 0]);
    bc4_values(b)[((bits >> (3 * t)) & 7) as usize]
}

/// Average colour (RGBA, 0..1, as stored) of a DDS, from its smallest mip
/// level that holds at least one block. Supports BC1-5 and 32-bit RGBA/BGRA.
pub fn mean_color(dds: &[u8]) -> Option<[f32; 4]> {
    let info = dds_info(dds)?;
    let fourcc = info.fourcc;
    let block_bytes = match &fourcc {
        b"DXT1" | b"ATI1" | b"BC4U" => 8,
        b"DXT2" | b"DXT3" | b"DXT4" | b"DXT5" | b"ATI2" | b"BC5U" => 16,
        [0, 0, 0, 0] => 0,
        _ => return None,
    };
    let rd = |o: usize| u32::from_le_bytes(dds[o..o + 4].try_into().unwrap());
    let bits = rd(88);
    // Byte size of a mip level.
    let level_size = |w: u32, h: u32| -> usize {
        if block_bytes > 0 {
            (w.div_ceil(4).max(1) * h.div_ceil(4).max(1)) as usize * block_bytes
        } else {
            (w * h * bits / 8) as usize
        }
    };
    let (mut w, mut h, mut offset) = (info.width.max(1), info.height.max(1), 128usize);
    for _ in 1..info.mips {
        if block_bytes > 0 && (w <= 4 || h <= 4) {
            break;
        }
        offset += level_size(w, h);
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    let data = dds.get(offset..offset + level_size(w, h))?;
    let mut sum = [0.0f64; 4];
    let mut n = 0.0f64;
    if block_bytes == 0 {
        if bits != 32 {
            return None;
        }
        let (rm, gm, bm, am) = (rd(92), rd(96), rd(100), rd(104));
        let chan = |px: u32, mask: u32| if mask == 0 { 1.0 } else { ((px & mask) >> mask.trailing_zeros()) as f64 / (mask >> mask.trailing_zeros()) as f64 };
        for px in data.chunks_exact(4) {
            let p = u32::from_le_bytes(px.try_into().unwrap());
            for (k, m) in [rm, gm, bm, am].into_iter().enumerate() {
                sum[k] += chan(p, m);
            }
            n += 1.0;
        }
    } else {
        for b in data.chunks_exact(block_bytes) {
            for t in 0..16usize {
                let texel: [f32; 4] = match &fourcc {
                    b"DXT1" => {
                        let idx = (u32::from_le_bytes([b[4], b[5], b[6], b[7]]) >> (2 * t)) & 3;
                        bc1_palette(b, true)[idx as usize]
                    }
                    b"ATI1" | b"BC4U" => {
                        let v = bc4_texel(b, t);
                        [v, v, v, 1.0]
                    }
                    b"ATI2" | b"BC5U" => [bc4_texel(&b[0..8], t), bc4_texel(&b[8..16], t), 1.0, 1.0],
                    _ => {
                        let c = &b[8..16];
                        let idx = (u32::from_le_bytes([c[4], c[5], c[6], c[7]]) >> (2 * t)) & 3;
                        let mut px = bc1_palette(c, false)[idx as usize];
                        px[3] = if &fourcc == b"DXT2" || &fourcc == b"DXT3" {
                            ((u64::from_le_bytes(b[0..8].try_into().unwrap()) >> (4 * t)) & 15) as f32 / 15.0
                        } else {
                            bc4_texel(&b[0..8], t)
                        };
                        px
                    }
                };
                for k in 0..4 {
                    sum[k] += texel[k] as f64;
                }
                n += 1.0;
            }
        }
    }
    if n == 0.0 {
        return None;
    }
    Some(std::array::from_fn(|k| (sum[k] / n) as f32))
}

#[cfg(test)]
mod mean_tests {
    use super::*;

    #[test]
    fn mean_of_solid_bc1() {
        let mut dds = vec![0u8; 128];
        dds[0..4].copy_from_slice(b"DDS ");
        dds[12..16].copy_from_slice(&4u32.to_le_bytes());
        dds[16..20].copy_from_slice(&4u32.to_le_bytes());
        dds[28..32].copy_from_slice(&1u32.to_le_bytes());
        dds[84..88].copy_from_slice(b"DXT1");
        // c0 = pure red (0xF800), c1 = black, all texels index 0.
        dds.extend_from_slice(&[0x00, 0xF8, 0x00, 0x00, 0, 0, 0, 0]);
        let m = mean_color(&dds).unwrap();
        assert!((m[0] - 1.0).abs() < 1e-6 && m[1] == 0.0 && m[3] == 1.0, "{m:?}");
    }
}
