//! Vehicle patrol routes: the world's vehicle patrol markers and the AI path
//! shapes they drive along.
//!
//! A path shape (`AIPathShape_*`, a `CBasicShapeEntity`) stores its points in
//! one blob field: a `u32` count followed by that many `[f32; 3]` world
//! positions (Dunia space). A patrol marker refers to its shape by entity id
//! somewhere in its components.

use crate::fcb::Object;

/// Blob field of a shape entity holding its points.
pub const SHAPE_POINTS: u32 = 0x4073_DD31;

/// A route in Bevy space (Y up).
#[derive(Clone, Debug, PartialEq)]
pub struct PatrolRoute {
    pub points: Vec<[f32; 3]>,
    /// Drives round and round (else back and forth).
    pub looped: bool,
}

/// Points of a path shape's blob, in Dunia space.
pub fn shape_points(blob: &[u8]) -> Vec<[f32; 3]> {
    let Some(count) = blob.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize) else { return Vec::new() };
    if blob.len() < 4 + count * 12 {
        return Vec::new();
    }
    (0..count)
        .map(|i| {
            let f = |k: usize| f32::from_le_bytes(blob[4 + i * 12 + k * 4..8 + i * 12 + k * 4].try_into().unwrap());
            [f(0), f(1), f(2)]
        })
        .collect()
}

pub fn is_path_shape(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("aipathshape")
}

pub fn is_patrol_marker(name: &str) -> bool {
    name.to_ascii_lowercase().contains("patrolpathvehicle")
}

/// Every 64-bit value in an object tree (a marker's candidate path ids).
pub fn u64_values(o: &Object) -> Vec<u64> {
    let mut out = Vec::new();
    o.walk(&mut |o, _| {
        for (_, v) in &o.fields {
            if v.len() == 8 {
                out.push(u64::from_le_bytes(v[..8].try_into().unwrap()));
            }
        }
    });
    out
}

/// A route whose ends meet (within `gap` metres) loops.
pub fn route(points_bevy: Vec<[f32; 3]>, gap: f32) -> Option<PatrolRoute> {
    if points_bevy.len() < 2 {
        return None;
    }
    let (a, b) = (points_bevy[0], points_bevy[points_bevy.len() - 1]);
    let d = ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    Some(PatrolRoute { points: points_bevy, looped: d < gap })
}

/// One line per route: `route <loop 0|1> x,y,z x,y,z ...`.
pub fn to_text(routes: &[PatrolRoute]) -> String {
    let mut out = String::new();
    for r in routes {
        out += &format!("route {}", u8::from(r.looped));
        for p in &r.points {
            out += &format!(" {:.2},{:.2},{:.2}", p[0], p[1], p[2]);
        }
        out.push('\n');
    }
    out
}

pub fn from_text(text: &str) -> Vec<PatrolRoute> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.strip_prefix("route ")?.split(' ');
            let looped = it.next()? == "1";
            let points = it
                .filter_map(|p| {
                    let v: Vec<f32> = p.split(',').filter_map(|x| x.parse().ok()).collect();
                    (v.len() == 3).then(|| [v[0], v[1], v[2]])
                })
                .collect::<Vec<_>>();
            (points.len() >= 2).then_some(PatrolRoute { points, looped })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_shape_points() {
        let mut blob = 2u32.to_le_bytes().to_vec();
        for v in [1.0f32, 2.0, 3.0, -4.0, 5.5, 6.0] {
            blob.extend(v.to_le_bytes());
        }
        assert_eq!(shape_points(&blob), vec![[1.0, 2.0, 3.0], [-4.0, 5.5, 6.0]]);
        assert!(shape_points(&blob[..10]).is_empty());
    }

    #[test]
    fn routes_round_trip_and_loop() {
        let open = route(vec![[0.0, 0.0, 0.0], [50.0, 1.0, 0.0]], 15.0).unwrap();
        let closed = route(vec![[0.0, 0.0, 0.0], [50.0, 1.0, 0.0], [5.0, 0.0, 5.0]], 15.0).unwrap();
        assert!(!open.looped && closed.looped);
        assert_eq!(from_text(&to_text(&[open.clone(), closed.clone()])), vec![open, closed]);
    }
}
