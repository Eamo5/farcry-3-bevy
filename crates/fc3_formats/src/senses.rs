//! How the AI notices the player, from the object templates' sensory
//! systems (`aisensorysystem:*`): for each of the player's postures
//! (stealthy, normal, in action) and each of the AI's states (unaware,
//! aware, focused on him), the farthest it can notice him and how long
//! that takes, from a shortest time near to a longest one far, and which of
//! its three view volumes (tight, medium, wide) it looks through; how fast
//! a partly filled acquisition drains in each state; the grass it sees
//! through close by; the "sixth sense" that notices anyone very close
//! whichever way it faces; and how it sees a body, a grenade or a thrown
//! rock. The cache keeps them as text ([`SENSES_REL`]).

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const SENSES_REL: &str = "characters/senses.txt";
const MAGIC: &str = "fc3-senses 3";

/// The player's postures, in the tables' order.
pub const POSTURES: [&str; 3] = ["Stealth", "Normal", "Action"];
/// The AI's states, in the tables' order.
pub const LEVELS: [&str; 3] = ["Unaware", "Aware", "Focus"];

/// Noticing at one posture and state: nothing beyond `max_dist`; within it,
/// `min_time` at `near` metres or closer rising to `max_time` at `far` or
/// beyond.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Transition {
    pub max_dist: f32,
    pub near: f32,
    pub far: f32,
    pub min_time: f32,
    pub max_time: f32,
}

impl Transition {
    /// Seconds to notice the player at a distance, or `None` beyond reach.
    pub fn time(&self, distance: f32) -> Option<f32> {
        if distance > self.max_dist {
            return None;
        }
        let f = if self.far > self.near { ((distance - self.near) / (self.far - self.near)).clamp(0.0, 1.0) } else { 0.0 };
        Some(self.min_time + (self.max_time - self.min_time) * f)
    }
}

/// A view volume (`TightFOV`, `MediumFOV`, `WideFOV`): seen up to `depth`
/// metres ahead; to each side as far as `lateral` and up or down as far as
/// `height`, each given at the eye, halfway out and at full depth (and
/// straight between).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Fov {
    pub depth: f32,
    pub lateral: [f32; 3],
    pub height: [f32; 3],
}

impl Fov {
    /// Whether a point `ahead` metres in front, `side` metres to a side and
    /// `up` metres above or below the eye is in view.
    pub fn contains(&self, ahead: f32, side: f32, up: f32) -> bool {
        if !(0.0..=self.depth).contains(&ahead) || self.depth <= 0.0 {
            return false;
        }
        let f = ahead / self.depth * 2.0;
        let at = |v: &[f32; 3]| if f < 1.0 { v[0] + (v[1] - v[0]) * f } else { v[1] + (v[2] - v[1]) * (f - 1.0) };
        side.abs() <= at(&self.lateral) && up.abs() <= at(&self.height)
    }
}

/// Seeing a thing (a body, a grenade, a rock): through a view volume, how
/// far and how soon.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stim {
    pub fov: u8,
    pub sight: Transition,
}

/// The things it sees, in [`Senses::stims`]' order.
pub const STIMS: [&str; 3] = ["CorpseDetection", "GrenadeDetection", "RockDetection"];
pub const CORPSE: usize = 0;
pub const GRENADE: usize = 1;
pub const ROCK: usize = 2;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Senses {
    /// The template's name ("aisensorysystem:default.default").
    pub name: String,
    /// By posture, then state.
    pub table: [[Transition; 3]; 3],
    /// Anyone this near is noticed within this time, whichever way it faces.
    pub sixth_sense: (f32, f32),
    /// The tight, medium and wide view volumes, and which one each posture
    /// and state looks through.
    pub fov: [Fov; 3],
    pub fov_of: [[u8; 3]; 3],
    /// Seconds a full acquisition takes to drain in each state.
    pub cooldown: [f32; 3],
    /// Grass this near does not hide anyone from it.
    pub grass_ignored: f32,
    /// Seeing a body, a grenade and a rock ([`STIMS`]); a rock is only
    /// seen moving at least `rock_speed` m/s.
    pub stims: [Stim; 3],
    pub rock_speed: f32,
}

impl Senses {
    pub fn get(&self, posture: usize, level: usize) -> &Transition {
        &self.table[posture.min(2)][level.min(2)]
    }

    /// The view volume at a posture and state.
    pub fn view(&self, posture: usize, level: usize) -> &Fov {
        &self.fov[(self.fov_of[posture.min(2)][level.min(2)] as usize).min(2)]
    }

    /// Seconds to see a thing ([`STIMS`]) at a place relative to the eye
    /// (`ahead`, `side`, `up`), or `None` if it cannot.
    pub fn sees(&self, stim: usize, ahead: f32, side: f32, up: f32) -> Option<f32> {
        let s = self.stims.get(stim)?;
        let distance = (ahead * ahead + side * side + up * up).sqrt();
        self.fov[(s.fov as usize).min(2)].contains(ahead, side, up).then(|| s.sight.time(distance)).flatten()
    }
}

/// Whether a cached table is in this version's form.
pub fn is_current(text: &str) -> bool {
    text.lines().next().map(str::trim) == Some(MAGIC)
}

fn float(o: &Object, name: &str) -> Option<f32> {
    let b = o.field(name)?;
    match b.len() {
        4 => Some(f32::from_le_bytes(b.try_into().ok()?)),
        // (A zero is stored as a single byte.)
        1 => Some(b[0] as f32),
        _ => None,
    }
}

fn int(o: &Object, name: &str) -> Option<u32> {
    let b = o.field(name)?;
    match b.len() {
        4 => Some(u32::from_le_bytes(b.try_into().ok()?)),
        1 => Some(b[0] as u32),
        _ => None,
    }
}

/// The three view volumes in the order `selFOVType` counts them.
const FOVS: [&str; 3] = ["TightFOV", "MediumFOV", "WideFOV"];

/// Every sensory system among the object templates.
pub fn senses(templates: &Object) -> Vec<Senses> {
    let mut out = Vec::new();
    templates.walk(&mut |o, _| {
        let Some(name) = o.str("FullName").filter(|n| n.starts_with("aisensorysystem:")) else { return };
        let Some(detection) = o.child("Detection") else { return };
        let mut s = Senses { name, ..Default::default() };
        for (p, posture) in POSTURES.iter().enumerate() {
            let Some(posture) = detection.child(posture) else { continue };
            for (l, level) in LEVELS.iter().enumerate() {
                let Some(level) = posture.child(level) else { continue };
                let f = |o: &Object, n: &str| float(o, n).unwrap_or(0.0);
                s.fov_of[p][l] = int(level, "selFOVType").unwrap_or(0).min(2) as u8;
                let t = level.child("Transition");
                s.table[p][l] = Transition {
                    max_dist: f(level, "MaxDist"),
                    near: t.map_or(0.0, |t| f(t, "MinDistance")),
                    far: t.map_or(0.0, |t| f(t, "MaxDistance")),
                    min_time: t.map_or(0.0, |t| f(t, "MinTime")),
                    max_time: t.map_or(0.0, |t| f(t, "MaxTime")),
                };
            }
        }
        if let Some(six) = o.child("SixthSense") {
            s.sixth_sense = (float(six, "SixthSenseMaxDist").unwrap_or(0.0), float(six, "SixthSenseMaxTime").unwrap_or(0.0));
        }
        if let Some(fov) = o.child("FOV") {
            for (i, name) in FOVS.iter().enumerate() {
                let Some(v) = fov.child(name) else { continue };
                let f = |n: &str| float(v, n).unwrap_or(0.0);
                s.fov[i] = Fov {
                    depth: f("MaxDepth"),
                    lateral: [f("LateralBase"), f("LateralMidRatio") * f("LateralMax"), f("LateralMax")],
                    height: [f("HeightBase"), f("HeightMidRatio") * f("HeightMax"), f("HeightMax")],
                };
            }
        }
        if let Some(c) = detection.child("AcquisitionCooldown") {
            for (l, level) in LEVELS.iter().enumerate() {
                s.cooldown[l] = float(c, level).unwrap_or(0.0);
            }
        }
        s.grass_ignored = o.child("Vegetation").and_then(|v| float(v, "GrassNullifyDetectionRadius")).unwrap_or(0.0);
        for (i, name) in STIMS.iter().enumerate() {
            let Some(d) = o.child(name) else { continue };
            let f = |o: &Object, n: &str| float(o, n).unwrap_or(0.0);
            let t = d.child("Transition");
            s.stims[i] = Stim {
                fov: int(d, "selFOVType").unwrap_or(0).min(2) as u8,
                sight: Transition {
                    max_dist: f(d, "MaxDist"),
                    near: t.map_or(0.0, |t| f(t, "MinDistance")),
                    far: t.map_or(0.0, |t| f(t, "MaxDistance")),
                    min_time: t.map_or(0.0, |t| f(t, "MinTime")),
                    max_time: t.map_or(0.0, |t| f(t, "MaxTime")),
                },
            };
        }
        s.rock_speed = float(o, "RockMinSpeed").unwrap_or(0.0);
        out.push(s);
    });
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn to_text(all: &[Senses]) -> String {
    let mut s = format!("{MAGIC}\n");
    for x in all {
        let cells: Vec<String> = x.table.iter().flatten().map(|t| format!("{},{},{},{},{}", t.max_dist, t.near, t.far, t.min_time, t.max_time)).collect();
        let fovs: Vec<String> =
            x.fov.iter().map(|v| [v.depth].iter().chain(&v.lateral).chain(&v.height).map(|n| n.to_string()).collect::<Vec<_>>().join(",")).collect();
        let fov_of: String = x.fov_of.iter().flatten().map(|v| v.to_string()).collect();
        let cooldown = x.cooldown.map(|v| v.to_string()).join(",");
        let stims: Vec<String> = x
            .stims
            .iter()
            .map(|v| format!("{},{},{},{},{},{}", v.fov, v.sight.max_dist, v.sight.near, v.sight.far, v.sight.min_time, v.sight.max_time))
            .collect();
        s += &format!(
            "{}\t{},{}\t{}\t{}\t{fov_of}\t{cooldown}\t{}\t{}\t{}\n",
            x.name,
            x.sixth_sense.0,
            x.sixth_sense.1,
            cells.join(";"),
            fovs.join(";"),
            x.grass_ignored,
            stims.join(";"),
            x.rock_speed
        );
    }
    s
}

pub fn from_text(text: &str) -> Result<Vec<Senses>> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not a senses table");
    }
    let mut out = Vec::new();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 9 {
            continue;
        }
        let nums = |s: &str| s.split(',').map(|v| v.parse::<f32>().unwrap_or(0.0)).collect::<Vec<_>>();
        let six = nums(f[1]);
        let mut s = Senses { name: f[0].into(), sixth_sense: (six.first().copied().unwrap_or(0.0), six.get(1).copied().unwrap_or(0.0)), ..Default::default() };
        for (i, cell) in f[2].split(';').take(9).enumerate() {
            let v = nums(cell);
            if v.len() == 5 {
                s.table[i / 3][i % 3] = Transition { max_dist: v[0], near: v[1], far: v[2], min_time: v[3], max_time: v[4] };
            }
        }
        for (i, fov) in f[3].split(';').take(3).enumerate() {
            let v = nums(fov);
            if v.len() == 7 {
                s.fov[i] = Fov { depth: v[0], lateral: [v[1], v[2], v[3]], height: [v[4], v[5], v[6]] };
            }
        }
        for (i, c) in f[4].bytes().take(9).enumerate() {
            s.fov_of[i / 3][i % 3] = c.saturating_sub(b'0').min(2);
        }
        for (i, v) in nums(f[5]).into_iter().take(3).enumerate() {
            s.cooldown[i] = v;
        }
        s.grass_ignored = f[6].parse().unwrap_or(0.0);
        for (i, stim) in f[7].split(';').take(3).enumerate() {
            let v = nums(stim);
            if v.len() == 6 {
                s.stims[i] = Stim { fov: (v[0] as u8).min(2), sight: Transition { max_dist: v[1], near: v[2], far: v[3], min_time: v[4], max_time: v[5] } };
            }
        }
        s.rock_speed = f[8].parse().unwrap_or(0.0);
        out.push(s);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_times_by_distance() {
        let mut s = Senses { name: "aisensorysystem:default.default".into(), sixth_sense: (5.0, 2.0), ..Default::default() };
        s.table[1][0] = Transition { max_dist: 55.0, near: 7.0, far: 55.0, min_time: 0.25, max_time: 4.0 };
        s.fov[0] = Fov { depth: 30.0, lateral: [2.0, 8.05, 11.5], height: [1.0, 4.95, 9.0] };
        s.fov_of[2][2] = 2;
        s.cooldown = [2.0, 4.0, 10.0];
        s.grass_ignored = 3.0;
        s.stims[CORPSE] = Stim { fov: 0, sight: Transition { max_dist: 35.0, near: 5.0, far: 35.0, min_time: 0.25, max_time: 1.0 } };
        s.rock_speed = 5.0;
        assert_eq!(from_text(&to_text(&[s.clone()])).unwrap(), vec![s.clone()]);
        // A body ahead within the tight volume is seen in its time; beside or
        // beyond it, not.
        assert_eq!(s.sees(CORPSE, 20.0, 0.0, 0.0), Some(0.625));
        assert_eq!(s.sees(CORPSE, 5.0, 8.0, 0.0), None);
        assert_eq!(s.sees(GRENADE, 5.0, 0.0, 0.0), None);
        assert!(is_current(&to_text(&[])));
        // The tight volume: narrow at the eye, widening out to its depth.
        let v = s.view(1, 0);
        assert!(v.contains(1.0, 2.0, 0.0) && !v.contains(1.0, 3.0, 0.0));
        assert!(v.contains(15.0, 8.0, 0.0) && !v.contains(15.0, 8.2, 0.0));
        assert!(v.contains(30.0, 11.5, 0.0) && !v.contains(30.5, 0.0, 0.0) && !v.contains(-1.0, 0.0, 0.0));
        assert!(!v.contains(15.0, 0.0, 5.5));
        let t = s.get(1, 0);
        assert_eq!(t.time(3.0), Some(0.25));
        assert!((t.time(31.0).unwrap() - 2.125).abs() < 1e-4);
        assert_eq!(t.time(55.0), Some(4.0));
        assert_eq!(t.time(56.0), None);
    }
}
