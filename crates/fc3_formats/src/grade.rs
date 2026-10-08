//! The game's tone mapping and colour grade, as the converter reads them off
//! the installed game for the cache.
//!
//! Dunia ends each frame with one pass that adds the bloom to the scene, maps
//! the HDR colour through a filmic curve into a gamma-like encoding, and looks
//! the result up in a colour cube (a 32³ volume texture named by the world's
//! environment, `fileColorRemapVolumeMapTexture`). Nothing of the pass is
//! copied: the converter runs the game's own pixel shader on the CPU
//! (`d3d9_interp`) with the bloom off and a stand-in cube that returns its
//! coordinate, and tabulates what comes out for a range of grey inputs. The
//! game uses the cube as stored.

use std::collections::HashMap;

use anyhow::{bail, ensure, Context, Result};

use crate::d3d9::{ShaderObject, Stage};
use crate::d3d9_interp::{run, Machine};
use crate::hash::crc32;

/// Entries in the curve table.
pub const CURVE_LEN: usize = 256;

/// The grade: the tone curve (per channel, from exposed HDR to the cube's
/// coordinate) and the colour cube.
#[derive(Debug, Clone, PartialEq)]
pub struct Grade {
    /// The curve reaches its top at this input; entry `i` is the response
    /// at `x_max * (i / (CURVE_LEN - 1))²`.
    pub x_max: f32,
    pub curve: Vec<f32>,
    /// Cube edge; texels RGBA, red fastest, then green, then blue.
    pub size: u32,
    pub cube: Vec<[u8; 4]>,
}

const MAGIC: &[u8; 4] = b"FCGR";
const VERSION: u32 = 1;

impl Grade {
    /// The curve's response at `x` (linear between table entries).
    pub fn curve_at(&self, x: f32) -> f32 {
        let t = (x / self.x_max).clamp(0.0, 1.0).sqrt() * (self.curve.len() - 1) as f32;
        let i = (t as usize).min(self.curve.len() - 2);
        let f = t - i as f32;
        self.curve[i] * (1.0 - f) + self.curve[i + 1] * f
    }

    /// The cube's colour (0..1, display-referred) for a curve output, with
    /// trilinear filtering between texel centres as the game samples it
    /// (0 and 1 land on the first and last texels).
    pub fn cube_at(&self, c: [f32; 3]) -> [f32; 3] {
        let n = self.size as usize;
        let s = (n - 1) as f32;
        let p = c.map(|v| v.clamp(0.0, 1.0) * s);
        let i0 = p.map(|v| (v as usize).min(n - 1));
        let i1 = i0.map(|v| (v + 1).min(n - 1));
        let f = [p[0] - i0[0] as f32, p[1] - i0[1] as f32, p[2] - i0[2] as f32];
        let mut out = [0.0f32; 3];
        for (z, wz) in [(i0[2], 1.0 - f[2]), (i1[2], f[2])] {
            for (y, wy) in [(i0[1], 1.0 - f[1]), (i1[1], f[1])] {
                for (x, wx) in [(i0[0], 1.0 - f[0]), (i1[0], f[0])] {
                    let t = self.cube[(z * n + y) * n + x];
                    for k in 0..3 {
                        out[k] += t[k] as f32 / 255.0 * wx * wy * wz;
                    }
                }
            }
        }
        out
    }

    /// What the game shows (display-referred, 0..1) for an exposed HDR colour.
    pub fn display(&self, hdr: [f32; 3]) -> [f32; 3] {
        self.cube_at(hdr.map(|v| self.curve_at(v)))
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.curve.len() * 4 + self.cube.len() * 4);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&self.x_max.to_le_bytes());
        out.extend_from_slice(&(self.curve.len() as u32).to_le_bytes());
        for v in &self.curve {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.size.to_le_bytes());
        for t in &self.cube {
            out.extend_from_slice(t);
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Result<Grade> {
        let u32_at = |o: usize| -> Result<u32> { Ok(u32::from_le_bytes(b.get(o..o + 4).context("grade truncated")?.try_into().unwrap())) };
        ensure!(b.get(0..4) == Some(MAGIC.as_slice()), "not a grade file");
        ensure!(u32_at(4)? == VERSION, "grade version {} (want {VERSION})", u32_at(4)?);
        let x_max = f32::from_bits(u32_at(8)?);
        let len = u32_at(12)? as usize;
        ensure!(len >= 2, "grade curve too short");
        let curve = (0..len).map(|i| u32_at(16 + i * 4).map(f32::from_bits)).collect::<Result<Vec<_>>>()?;
        let at = 16 + len * 4;
        let size = u32_at(at)?;
        let n = (size as usize).pow(3);
        ensure!(size >= 2, "grade cube too small");
        let data = b.get(at + 4..at + 4 + n * 4).context("grade cube truncated")?;
        let cube = data.chunks_exact(4).map(|t| [t[0], t[1], t[2], t[3]]).collect();
        Ok(Grade { x_max, curve, size, cube })
    }
}

/// Reads a colour cube (an uncompressed 32-bit volume DDS) into RGBA texels.
pub fn cube_from_dds(dds: &[u8]) -> Result<(u32, Vec<[u8; 4]>)> {
    ensure!(dds.len() >= 128 && &dds[0..4] == b"DDS ", "not a DDS");
    let rd = |o: usize| u32::from_le_bytes(dds[o..o + 4].try_into().unwrap());
    let (height, width, depth) = (rd(12), rd(16), rd(24));
    let (bits, masks) = (rd(88), [rd(92), rd(96), rd(100), rd(104)]);
    ensure!(width == height && height == depth && width >= 2, "colour cube is {width}x{height}x{depth}");
    ensure!(bits == 32 && masks[..3].iter().all(|&m| m != 0), "colour cube is not 32-bit RGB");
    let n = (width as usize).pow(3);
    let data = dds.get(128..128 + n * 4).context("colour cube truncated")?;
    let channel = |px: u32, mask: u32| ((px & mask) >> mask.trailing_zeros()) as u8;
    let cube = data
        .chunks_exact(4)
        .map(|t| {
            let px = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
            [channel(px, masks[0]), channel(px, masks[1]), channel(px, masks[2]), if masks[3] != 0 { channel(px, masks[3]) } else { 255 }]
        })
        .collect();
    Ok((width, cube))
}

/// The final pass's samplers and constants, by name.
const CUBE: &str = "ColorRemap0VolumeSampler";
const BLOOM: &str = "CombinedBloomSampler";
const BLOOM_PARAMS: &str = "BloomParams";

/// One variant of the final pass, reduced to what a grey input does.
struct Variant<'a> {
    obj: &'a ShaderObject,
    cube: u32,
    bloom: u32,
}

impl Variant<'_> {
    /// The cube coordinate the pass looks up for a grey scene value `x`.
    fn lookup(&self, x: f32) -> Option<f32> {
        let sample = |s: u32, c: [f32; 4]| -> [f32; 4] {
            if s == self.cube {
                [c[0], c[1], c[2], 1.0]
            } else if s == self.bloom {
                [0.0; 4]
            } else {
                [x, x, x, 1.0]
            }
        };
        let mut m = Machine::new(&sample);
        if let Err(e) = run(&self.obj.program, &mut m) {
            if std::env::var("FC3_GRADE_DEBUG").is_ok() {
                eprintln!("grade: run failed: {e:#}");
            }
            return None;
        }
        let o = m.oc[0];
        // A grey input stays grey up to the lookup.
        ((o[0] - o[1]).abs() < 1e-5 && (o[0] - o[2]).abs() < 1e-5 && o[0].is_finite()).then_some(o[0])
    }
}

/// Tabulates the game's tone curve from its final-pass pixel shaders: of the
/// plain variants (scene, bloom and cube only), those whose response tops
/// out (the tone-mapped ones), the curve most of them share.
pub fn curve_from_shaders(objects: &[ShaderObject]) -> Result<(f32, Vec<f32>)> {
    let (cube_h, bloom_h, params_h) = (crc32(CUBE.as_bytes()), crc32(BLOOM.as_bytes()), crc32(BLOOM_PARAMS.as_bytes()));
    let mut curves: HashMap<Vec<i32>, (usize, f32, Vec<f32>)> = HashMap::new();
    for obj in objects.iter().filter(|o| o.program.stage == Stage::Pixel) {
        let samplers: Vec<_> = obj.bindings.iter().filter(|b| b.is_sampler()).collect();
        let reg = |h: u32| samplers.iter().find(|b| b.name_hash == h).map(|b| b.first_register());
        let (Some(cube), Some(bloom)) = (reg(cube_h), reg(bloom_h)) else { continue };
        if samplers.len() != 3 || obj.bindings.iter().any(|b| !b.is_sampler() && b.name_hash != params_h) {
            continue;
        }
        let v = Variant { obj, cube, bloom };
        if std::env::var("FC3_GRADE_DEBUG").is_ok() {
            eprintln!("grade: candidate {} instrs: {:?} {:?} {:?}", obj.program.instrs.len(), v.lookup(0.0), v.lookup(1.0), v.lookup(4096.0));
        }
        // The tone-mapped variants reach the top of the cube and stay there
        // (past about 28 the game's curve dips a little again; such values
        // are left at the top); the others only take a square root and keep
        // rising.
        let (Some(lo), Some(a), Some(b)) = (v.lookup(0.0), v.lookup(8.0), v.lookup(16.0)) else { continue };
        let top = a.max(b);
        if (a - b).abs() > 1e-5 || top - lo < 0.5 {
            continue;
        }
        let u = |x: f32| v.lookup(x).map(|o| ((o - lo) / (top - lo)).clamp(0.0, 1.0));
        // Where it tops out.
        let mut x_max = 1.0f32;
        while x_max < 8.0 && u(x_max).is_some_and(|y| y < 0.9999) {
            x_max *= 2.0;
        }
        let (mut a, mut b) = (x_max / 2.0, x_max);
        for _ in 0..24 {
            let m = (a + b) / 2.0;
            if u(m).is_some_and(|y| y < 0.9999) {
                a = m;
            } else {
                b = m;
            }
        }
        let x_max = b;
        let Some(table) = (0..CURVE_LEN).map(|i| u(x_max * (i as f32 / (CURVE_LEN - 1) as f32).powi(2))).collect::<Option<Vec<f32>>>() else { continue };
        let key: Vec<i32> = [0.05f32, 0.18, 0.5, 1.0, 2.0].iter().filter_map(|&x| u(x)).map(|y| (y * 1e4).round() as i32).collect();
        curves.entry(key).or_insert((0, x_max, table)).0 += 1;
    }
    let Some((_, (_, x_max, table))) = curves.into_iter().max_by_key(|(_, (n, _, _))| *n) else {
        bail!("no tone-mapping pass among the shaders");
    };
    Ok((x_max, table))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_samples() {
        let size = 2;
        let cube = (0..8u32).map(|i| [((i & 1) * 255) as u8, (((i >> 1) & 1) * 255) as u8, (((i >> 2) & 1) * 255) as u8, 255]).collect();
        let g = Grade { x_max: 4.0, curve: (0..CURVE_LEN).map(|i| i as f32 / (CURVE_LEN - 1) as f32).collect(), size, cube };
        let back = Grade::from_bytes(&g.to_bytes()).unwrap();
        assert_eq!(back, g);
        // The identity cube returns its coordinate; the curve is sqrt(x / 4).
        let c = g.cube_at([0.25, 0.5, 1.0]);
        assert!((c[0] - 0.25).abs() < 1e-5 && (c[1] - 0.5).abs() < 1e-5 && (c[2] - 1.0).abs() < 1e-5);
        assert!((g.curve_at(1.0) - 0.5).abs() < 1e-3);
        assert!((g.display([1.0, 1.0, 1.0])[0] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn reads_a_volume_dds() {
        let mut dds = vec![0u8; 128];
        dds[0..4].copy_from_slice(b"DDS ");
        let put = |d: &mut Vec<u8>, o: usize, v: u32| d[o..o + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut dds, 12, 2);
        put(&mut dds, 16, 2);
        put(&mut dds, 24, 2);
        put(&mut dds, 88, 32);
        put(&mut dds, 92, 0x00ff_0000);
        put(&mut dds, 96, 0x0000_ff00);
        put(&mut dds, 100, 0x0000_00ff);
        put(&mut dds, 104, 0xff00_0000);
        // BGRA bytes: the second texel pure red.
        for i in 0..8 {
            dds.extend_from_slice(if i == 1 { &[0, 0, 255, 255] } else { &[0, 0, 0, 255] });
        }
        let (size, cube) = cube_from_dds(&dds).unwrap();
        assert_eq!(size, 2);
        assert_eq!(cube[1], [255, 0, 0, 255]);
        assert_eq!(cube[0], [0, 0, 0, 255]);
    }
}
