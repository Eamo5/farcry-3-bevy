//! The world's light probes, Far Cry 3's "Deferred Radiance Transfer
//! Volumes" (GDC 2012, Gilabert and Stefanov). Each world sector has one
//! `lightprobes_<sector>` entity whose `CLightProbesComponent` holds about
//! seventy probes laid out over the ground (8 m apart, and over the trees'
//! bounds), each baked with:
//!
//! * a radiance transfer matrix per colour channel: for each of four basis
//!   directions (three tilted 35 degrees up, a third of a turn apart, and
//!   one straight down) the light bounced to the probe from that side, as a
//!   second-order spherical harmonic (L00, then L1 along Dunia y, z and x)
//!   that the sun's or the sky's own harmonic is dotted with;
//! * the sky's visibility in each basis direction (the down one always 0).
//!
//! `radianceTransferProbes_V4` is a probe count times 16, then per probe 64
//! signed 16-bit fixed-point values (1.0 = 256): the three matrices (red,
//! green, blue; each basis's four coefficients in turn), 12 of static
//! irradiance (left at 0 in the shipped world) and the four visibilities.
//! Open sky reads about 1619, 2π of solid angle. `positions` is a count and
//! the probes' Dunia positions; `grid` (a 32 × 32 × 4 index of the nearest
//! probe over the sector's bounds) is left: the runtime finds the nearest
//! itself, across sectors.

use crate::fcb::Object;

/// The fixed point's one.
pub const ONE: f32 = 256.0;
/// The sky visibility of a basis direction under open sky.
pub const OPEN_SKY: f32 = 1619.0;

/// The basis directions (Dunia, z up), as the game's ambient shading has
/// them.
pub const BASIS: [[f32; 3]; 4] = [[-0.408248, -0.707107, 0.57735], [-0.408248, 0.707107, 0.57735], [0.816497, 0.0, 0.57735], [0.0, 0.0, -1.0]];

/// A probe: where it stands (Bevy space), its transfer (channel, basis,
/// coefficient) and its sky visibility in the three upward directions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Probe {
    pub pos: [f32; 3],
    pub transfer: [[[i16; 4]; 4]; 3],
    pub sky: [i16; 3],
}

/// Where the cache keeps a sector's probes.
pub fn cache_rel(sector: u32) -> String {
    format!("probes/{sector}.bin")
}

const MAGIC: &[u8; 4] = b"FC3P";
const PROBE_BYTES: usize = 12 + 96 + 6;

/// The probes of a sector's entities (its whole FCB tree).
pub fn from_sector(root: &Object) -> Vec<Probe> {
    let mut out = Vec::new();
    root.walk(&mut |o, _| {
        if !o.is("CLightProbesComponent") {
            return;
        }
        let (Some(pos), Some(v4)) = (o.field("positions"), o.field("radianceTransferProbes_V4")) else { return };
        out.extend(parse(pos, v4));
    });
    out
}

/// The probes from a component's `positions` and `radianceTransferProbes_V4`.
pub fn parse(positions: &[u8], v4: &[u8]) -> Vec<Probe> {
    let u32_at = |b: &[u8], i: usize| b.get(i..i + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()));
    let (Some(n), Some(n16)) = (u32_at(positions, 0), u32_at(v4, 0)) else { return Vec::new() };
    let n = n as usize;
    if n16 as usize != n * 16 || positions.len() < 4 + n * 12 || v4.len() < 4 + n * 128 {
        return Vec::new();
    }
    let f = |i: usize| f32::from_le_bytes(positions[i..i + 4].try_into().unwrap());
    (0..n)
        .map(|k| {
            let p = 4 + k * 12;
            let q = 4 + k * 128;
            let s = |i: usize| i16::from_le_bytes(v4[q + i * 2..q + i * 2 + 2].try_into().unwrap());
            Probe {
                pos: crate::world::pos_to_bevy([f(p), f(p + 4), f(p + 8)]),
                transfer: std::array::from_fn(|c| std::array::from_fn(|b| std::array::from_fn(|i| s(c * 16 + b * 4 + i)))),
                sky: [s(60), s(61), s(62)],
            }
        })
        .collect()
}

pub fn to_bytes(probes: &[Probe]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + probes.len() * PROBE_BYTES);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(probes.len() as u32).to_le_bytes());
    for p in probes {
        for v in p.pos {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in p.transfer.iter().flatten().flatten().chain(&p.sky) {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

pub fn from_bytes(b: &[u8]) -> Option<Vec<Probe>> {
    if b.get(..4)? != MAGIC {
        return None;
    }
    let n = u32::from_le_bytes(b.get(4..8)?.try_into().ok()?) as usize;
    if b.len() != 8 + n * PROBE_BYTES {
        return None;
    }
    Some(
        (0..n)
            .map(|k| {
                let q = 8 + k * PROBE_BYTES;
                let f = |i: usize| f32::from_le_bytes(b[q + i * 4..q + i * 4 + 4].try_into().unwrap());
                let s = |i: usize| i16::from_le_bytes(b[q + 12 + i * 2..q + 14 + i * 2].try_into().unwrap());
                Probe {
                    pos: [f(0), f(1), f(2)],
                    transfer: std::array::from_fn(|c| std::array::from_fn(|bi| std::array::from_fn(|i| s(c * 16 + bi * 4 + i)))),
                    sky: [s(48), s(49), s(50)],
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A component's two fields read into probes and back through the cache.
    #[test]
    fn probes_read_and_round_trip() {
        let mut positions = 1u32.to_le_bytes().to_vec();
        for v in [-252.0f32, -764.0, 206.5] {
            positions.extend_from_slice(&v.to_le_bytes());
        }
        let mut v4 = 16u32.to_le_bytes().to_vec();
        for i in 0..64i16 {
            let v = match i {
                60..=62 => 1600 + i,
                63 => 0,
                48..=59 => 0,
                _ => i - 10,
            };
            v4.extend_from_slice(&v.to_le_bytes());
        }
        let probes = parse(&positions, &v4);
        assert_eq!(probes.len(), 1);
        let p = probes[0];
        // Dunia (x, y, z) is Bevy (x, z, -y).
        assert_eq!(p.pos, [-252.0, 206.5, 764.0]);
        assert_eq!(p.transfer[0][0], [-10, -9, -8, -7]);
        assert_eq!(p.transfer[2][3], [34, 35, 36, 37]);
        assert_eq!(p.sky, [1660, 1661, 1662]);
        assert_eq!(from_bytes(&to_bytes(&probes)), Some(probes));
        // Not a probe count's worth: nothing.
        assert!(parse(&positions, &v4[..100]).is_empty());
    }
}
