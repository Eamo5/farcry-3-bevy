//! Trigger volumes, from the world data: proximity triggers (boxes or
//! spheres around the entity, `vectorSize`, turned with it) and 2D regions
//! (`hidShapePoints` polygons extruded over `fHeight`, or without limit when
//! it is negative). The story tests the player against the triggers scripts
//! watch; the host adds what it knows of other entities.

use crate::data::Entry;

/// Restrictions scripts can change while a proximity trigger is active.
#[derive(Debug, Clone, Copy, Default)]
pub struct UseRules {
    pub usable: Option<bool>,
    pub inside: Option<bool>,
    pub look_at: Option<bool>,
    pub angle: Option<bool>,
}

/// Whether the player can use this proximity trigger from their current
/// capsule and view. The caller checks layer, enabled state and listeners.
pub fn can_use(e: &Entry, rules: UseRules, player: [f32; 3], eye: [f32; 3], look: [f32; 3]) -> bool {
    let Some(prox) = e.component("CProximityTriggerComponent") else { return false };
    if !rules.usable.unwrap_or_else(|| prox.bool("bUsable").unwrap_or(false)) {
        return false;
    }
    let restrictions = prox.child("UseRestrictions");
    let flag = |name| restrictions.and_then(|r| r.bool(name)).unwrap_or(false);
    let inside = rules.inside.unwrap_or_else(|| flag("bIsInsideToUse"));
    let reach = if inside { 0.0 } else { 1.0 };
    let Some(shape) = shape_of(e) else { return false };
    if !shape.holds(player, BODY_RADIUS + reach, BODY_HALF_HEIGHT) {
        return false;
    }
    if rules.look_at.unwrap_or_else(|| flag("bLookAtCheck")) {
        let to = [e.pos[0] - eye[0], e.pos[1] - eye[1], e.pos[2] - eye[2]];
        let distance = to.iter().map(|v| v * v).sum::<f32>().sqrt();
        let length = look.iter().map(|v| v * v).sum::<f32>().sqrt();
        if distance > 0.3 && (length < 1e-6 || to.iter().zip(look).map(|(a, b)| a * b).sum::<f32>() / (distance * length) < 0.6) {
            return false;
        }
    }
    if rules.angle.unwrap_or_else(|| flag("bAngleCheck")) {
        let length = (look[0] * look[0] + look[1] * look[1]).sqrt();
        let (s, c) = e.angles[2].to_radians().sin_cos();
        if length < 1e-6 || (-s * look[0] + c * look[1]) / length < 0.6 {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// Centre, half sizes and yaw (degrees, about Dunia Z).
    Box {
        centre: [f32; 3],
        half: [f32; 3],
        yaw: f32,
    },
    Sphere {
        centre: [f32; 3],
        radius: f32,
    },
    /// A polygon in X/Y between two heights.
    Prism {
        points: Vec<[f32; 2]>,
        z_min: f32,
        z_max: f32,
    },
}

impl Shape {
    pub fn contains(&self, p: [f32; 3]) -> bool {
        match self {
            Shape::Box { centre, half, yaw } => {
                let d = [p[0] - centre[0], p[1] - centre[1], p[2] - centre[2]];
                let (s, c) = (-yaw.to_radians()).sin_cos();
                let local = [d[0] * c - d[1] * s, d[0] * s + d[1] * c, d[2]];
                (0..3).all(|i| local[i].abs() <= half[i])
            }
            Shape::Sphere { centre, radius } => {
                let d: f32 = (0..3).map(|i| (p[i] - centre[i]).powi(2)).sum();
                d <= radius * radius
            }
            Shape::Prism { points, z_min, z_max } => {
                if p[2] < *z_min || p[2] > *z_max {
                    return false;
                }
                // Even-odd ray test.
                let mut inside = false;
                let n = points.len();
                for i in 0..n {
                    let (a, b) = (points[i], points[(i + n - 1) % n]);
                    if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                        inside = !inside;
                    }
                }
                inside
            }
        }
    }
}

impl Shape {
    /// Whether a body round `p` (a capsule: radius `r`, half height `h`)
    /// touches the volume, as the engine's physics overlap tests it.
    pub fn holds(&self, p: [f32; 3], r: f32, h: f32) -> bool {
        match self {
            Shape::Box { centre, half, yaw } => {
                let d = [p[0] - centre[0], p[1] - centre[1], p[2] - centre[2]];
                let (s, c) = (-yaw.to_radians()).sin_cos();
                let local = [d[0] * c - d[1] * s, d[0] * s + d[1] * c, d[2]];
                local[0].abs() <= half[0] + r && local[1].abs() <= half[1] + r && local[2].abs() <= half[2] + h
            }
            Shape::Sphere { centre, radius } => {
                let d: f32 = (0..3).map(|i| (p[i] - centre[i]).powi(2)).sum();
                d <= (radius + r) * (radius + r)
            }
            Shape::Prism { points, z_min, z_max } => {
                if p[2] < *z_min - h || p[2] > *z_max + h {
                    return false;
                }
                if self.contains([p[0], p[1], p[2].clamp(*z_min, *z_max)]) {
                    return true;
                }
                // Within reach of an edge.
                let n = points.len();
                (0..n).any(|i| {
                    let (a, b) = (points[i], points[(i + 1) % n]);
                    let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
                    let len2 = (ex * ex + ey * ey).max(1e-6);
                    let t = (((p[0] - a[0]) * ex + (p[1] - a[1]) * ey) / len2).clamp(0.0, 1.0);
                    let (dx, dy) = (a[0] + ex * t - p[0], a[1] + ey * t - p[1]);
                    dx * dx + dy * dy <= r * r
                })
            }
        }
    }

    /// Like [`Shape::holds`] anywhere along the way from `a` to `b` (a body
    /// crossing a thin trigger between two frames).
    pub fn holds_along(&self, a: [f32; 3], b: [f32; 3], r: f32, h: f32) -> bool {
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let steps = ((len / 0.15).ceil() as usize).clamp(1, 40);
        (0..=steps).any(|i| {
            let t = i as f32 / steps as f32;
            self.holds([a[0] + d[0] * t, a[1] + d[1] * t, a[2] + d[2] * t], r, h)
        })
    }
}

/// The player's and characters' bodies, as triggers test them.
pub const BODY_RADIUS: f32 = 0.4;
pub const BODY_HALF_HEIGHT: f32 = 0.9;

/// A trigger entity's volume.
pub fn shape_of(e: &Entry) -> Option<Shape> {
    if let Some(points) = e.fields.field("hidShapePoints") {
        // u32 count, then the corners (x, y, z).
        let n = points.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize).unwrap_or(0);
        let pts: Vec<[f32; 2]> = points
            .get(4..)
            .unwrap_or_default()
            .chunks_exact(12)
            .take(n)
            .map(|c| [f32::from_le_bytes(c[0..4].try_into().unwrap()), f32::from_le_bytes(c[4..8].try_into().unwrap())])
            .collect();
        if pts.len() >= 3 {
            let height = e.fields.f32("fHeight").unwrap_or(-1.0);
            let (z_min, z_max) = if height < 0.0 {
                (f32::MIN, f32::MAX)
            } else {
                let min = e.fields.vec3("hidBBoxMin").map(|b| b[2]).unwrap_or(e.pos[2]);
                (min, min + height)
            };
            return Some(Shape::Prism { points: pts, z_min, z_max });
        }
    }
    let prox = e.component("CProximityTriggerComponent")?;
    let size = prox.vec3("vectorSize").unwrap_or([2.0; 3]);
    let sphere = e.component("CTriggerComponent").and_then(|t| t.bool("bIsSphere")).unwrap_or(false);
    Some(if sphere { Shape::Sphere { centre: e.pos, radius: size[0] } } else { Shape::Box { centre: e.pos, half: size.map(|s| s * 0.5), yaw: e.angles[2] } })
}

/// Whether a trigger starts enabled.
pub fn starts_enabled(e: &Entry) -> bool {
    e.component("CProximityTriggerComponent")
        .or_else(|| e.component("CTrigger2DComponent"))
        .or_else(|| e.component("CLookAtTriggerComponent"))
        .and_then(|c| c.bool("bEnabled"))
        .unwrap_or(true)
}

/// The authored collision filter applies to both the host's overlaps and
/// characters tested from their live positions. Mission-boundary regions
/// marked `PlayerOnly` must not report NPCs leaving or being streamed out.
pub fn allows_entity(e: &Entry, id: u64) -> bool {
    if id == crate::PLAYER {
        return true;
    }
    if let Some(region) = e.component("CTrigger2DComponent") {
        // The region's filter string (the original field's name is unknown).
        if region.str_h(0xB3C7_40E6).is_some_and(|s| s.eq_ignore_ascii_case("PlayerOnly")) {
            return false;
        }
    }
    e.component("CProximityTriggerComponent").is_none_or(|c| c.bool("bAllowTriggerByNonPlayers").unwrap_or(false))
}

/// Whether the player sees a look-at trigger standing at `at`: within its
/// range, and inside its share of the view's half angle around where the
/// player looks.
pub fn looked_at(e: &Entry, at: [f32; 3], eye: [f32; 3], look: [f32; 3], fov_deg: f32) -> bool {
    let Some(c) = e.component("CLookAtTriggerComponent") else { return false };
    let range = c.f32("fRange").unwrap_or(10.0);
    let share = c.f32("fFOVPercentage").unwrap_or(1.0).clamp(0.05, 1.0);
    let to = [at[0] - eye[0], at[1] - eye[1], at[2] - eye[2]];
    let d = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
    if d > range {
        return false;
    }
    if d < 0.3 {
        return true;
    }
    let cos = (to[0] * look[0] + to[1] * look[1] + to[2] * look[2]) / d;
    cos >= (fov_deg.to_radians() * 0.5 * share).cos()
}

/// The character a look-at trigger rides on, and its offset from the
/// character's feet: the game's are placed on a character where it spawns
/// (head, chest, hips: within a metre across, up to its height above), and
/// follow it.
pub fn carrier_of(trigger: &Entry, entities: &std::collections::HashMap<u64, Entry>) -> Option<(u64, [f32; 3])> {
    trigger.component("CLookAtTriggerComponent")?;
    let p = trigger.pos;
    entities
        .values()
        .filter(|c| c.has("CPawn"))
        .map(|c| (c, [p[0] - c.pos[0], p[1] - c.pos[1], p[2] - c.pos[2]]))
        .filter(|(_, d)| d[0].hypot(d[1]) < 1.0 && (-0.5..2.5).contains(&d[2]))
        .min_by(|a, b| a.1[0].hypot(a.1[1]).total_cmp(&b.1[0].hypot(b.1[1])))
        .map(|(c, d)| (c.id, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        let b = Shape::Box { centre: [0.0; 3], half: [5.0, 1.0, 2.0], yaw: 90.0 };
        // Turned a quarter: long along Y.
        assert!(b.contains([0.5, 4.0, 0.0]));
        assert!(!b.contains([4.0, 0.5, 0.0]));
        let p = Shape::Prism { points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]], z_min: f32::MIN, z_max: f32::MAX };
        assert!(p.contains([5.0, 5.0, 1000.0]));
        assert!(!p.contains([11.0, 5.0, 0.0]));
    }
}
