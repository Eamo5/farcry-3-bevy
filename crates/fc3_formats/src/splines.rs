//! Spline primitives: the roads and rivers laid along splines in the world
//! sectors (`SplinePrimitiveEntity`), drawn as ribbons with their material's
//! `Road` or `WaterRiver` shader.
//!
//! The primitive component (`E2FD401E`) holds the material path and a spline
//! object whose fields are, already tessellated: cross-sections of three
//! points each (the centre, then the two edges, so the ribbon follows the
//! ground's slope across it; Dunia space); per point, the distance along the
//! whole road or river (repeated for a section's three points, carrying the
//! texture on across sector pieces); per section, a blend weight; and the
//! half-width.

use crate::fcb::Object;
use crate::reader::Reader;
use anyhow::{bail, Result};

/// Field hashes.
mod h {
    pub const COMPONENT: u32 = 0xE2FD401E;
    pub const MATERIAL: u32 = 0xEF034902;
    pub const DRAW_DISTANCE: u32 = 0x56FF0AED;
    pub const POINTS: u32 = 0x546E67DF;
    pub const DISTANCES: u32 = 0xE0387957;
    pub const WEIGHTS: u32 = 0xB115A242;
    pub const HALF_WIDTH: u32 = 0x96F35D9E;
}

/// One cross-section of the ribbon.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Section {
    /// Centre, then the two edges (Dunia space, Z up).
    pub points: [[f32; 3]; 3],
    /// Distance along the whole road or river (metres).
    pub distance: f32,
    pub weight: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SplinePrimitive {
    /// `graphics\_materials\<name>.material.bin`.
    pub material: String,
    pub sections: Vec<Section>,
    pub half_width: f32,
    /// How far away it is drawn (metres).
    pub draw_distance: f32,
}

fn f32s(b: &[u8]) -> Vec<f32> {
    let n = b.get(..4).map_or(0, |c| u32::from_le_bytes(c.try_into().unwrap()) as usize);
    b.get(4..).unwrap_or(&[]).chunks_exact(4).take(n).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn f32_at(o: &Object, hash: u32) -> Option<f32> {
    o.field_h(hash).filter(|b| b.len() >= 4).map(|b| f32::from_le_bytes(b[..4].try_into().unwrap()))
}

/// The spline primitive in an entity's `Components`, if it has one.
pub fn parse(components: &Object) -> Option<SplinePrimitive> {
    let c = components.children.iter().find(|c| c.name_hash == h::COMPONENT)?;
    let material = c.str_h(h::MATERIAL).filter(|s| !s.is_empty())?;
    let spline = c.children.iter().flat_map(|s| s.children.iter()).find(|s| s.field_h(h::POINTS).is_some())?;
    let b = spline.field_h(h::POINTS)?;
    let n = u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as usize;
    let points: Vec<[f32; 3]> = b.get(4..)?.chunks_exact(12).take(n).map(|p| [0, 4, 8].map(|o| f32::from_le_bytes(p[o..o + 4].try_into().unwrap()))).collect();
    if points.len() < 6 || points.len() % 3 != 0 {
        return None;
    }
    let distances = spline.field_h(h::DISTANCES).map(f32s).unwrap_or_default();
    let weights = spline.field_h(h::WEIGHTS).map(f32s).unwrap_or_default();
    let sections = points
        .chunks_exact(3)
        .enumerate()
        .map(|(i, p)| Section { points: [p[0], p[1], p[2]], distance: distances.get(i * 3).copied().unwrap_or(0.0), weight: weights.get(i).copied().unwrap_or(1.0) })
        .collect();
    Some(SplinePrimitive {
        material,
        sections,
        half_width: f32_at(spline, h::HALF_WIDTH).unwrap_or(2.0),
        draw_distance: f32_at(c, h::DRAW_DISTANCE).unwrap_or(300.0),
    })
}

impl SplinePrimitive {
    pub fn to_bytes(&self, out: &mut Vec<u8>) {
        let s = self.material.as_bytes();
        out.extend_from_slice(&(s.len() as u16).to_le_bytes());
        out.extend_from_slice(s);
        out.extend_from_slice(&self.half_width.to_le_bytes());
        out.extend_from_slice(&self.draw_distance.to_le_bytes());
        out.extend_from_slice(&(self.sections.len() as u32).to_le_bytes());
        for sec in &self.sections {
            for p in sec.points {
                for v in p {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
            out.extend_from_slice(&sec.distance.to_le_bytes());
            out.extend_from_slice(&sec.weight.to_le_bytes());
        }
    }

    pub fn read(r: &mut Reader) -> Result<SplinePrimitive> {
        let len = r.u16()? as usize;
        let material = String::from_utf8_lossy(r.bytes(len)?).into_owned();
        let half_width = r.f32()?;
        let draw_distance = r.f32()?;
        let n = r.u32()? as usize;
        if n > 100_000 {
            bail!("spline with {n} sections");
        }
        let mut sections = Vec::with_capacity(n);
        for _ in 0..n {
            let mut points = [[0.0; 3]; 3];
            for p in &mut points {
                *p = [r.f32()?, r.f32()?, r.f32()?];
            }
            sections.push(Section { points, distance: r.f32()?, weight: r.f32()? });
        }
        Ok(SplinePrimitive { material, sections, half_width, draw_distance })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let s = SplinePrimitive {
            material: r"graphics\_materials\road.material.bin".into(),
            sections: vec![
                Section { points: [[0.0, 0.0, 1.0], [-2.0, 0.0, 1.5], [2.0, 0.0, 0.5]], distance: 0.0, weight: 0.5 },
                Section { points: [[0.0, 3.0, 1.0], [-2.0, 3.0, 1.5], [2.0, 3.0, 0.5]], distance: 3.0, weight: 1.0 },
            ],
            half_width: 2.0,
            draw_distance: 300.0,
        };
        let mut b = Vec::new();
        s.to_bytes(&mut b);
        assert_eq!(SplinePrimitive::read(&mut Reader::new(&b)).unwrap(), s);
    }
}
