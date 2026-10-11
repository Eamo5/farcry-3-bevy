//! How the AI notices the player, from the object templates' sensory
//! systems (`aisensorysystem:*`): for each of the player's postures
//! (stealthy, normal, in action) and each of the AI's states (unaware,
//! aware, focused on him), the farthest it can notice him and how long
//! that takes, from a shortest time near to a longest one far; and the
//! "sixth sense" that notices anyone very close whichever way it faces.
//! The cache keeps them as text ([`SENSES_REL`]).

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const SENSES_REL: &str = "characters/senses.txt";
const MAGIC: &str = "fc3-senses 1";

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

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Senses {
    /// The template's name ("aisensorysystem:default.default").
    pub name: String,
    /// By posture, then state.
    pub table: [[Transition; 3]; 3],
    /// Anyone this near is noticed within this time, whichever way it faces.
    pub sixth_sense: (f32, f32),
}

impl Senses {
    pub fn get(&self, posture: usize, level: usize) -> &Transition {
        &self.table[posture.min(2)][level.min(2)]
    }
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
        out.push(s);
    });
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn to_text(all: &[Senses]) -> String {
    let mut s = format!("{MAGIC}\n");
    for x in all {
        let cells: Vec<String> = x.table.iter().flatten().map(|t| format!("{},{},{},{},{}", t.max_dist, t.near, t.far, t.min_time, t.max_time)).collect();
        s += &format!("{}\t{},{}\t{}\n", x.name, x.sixth_sense.0, x.sixth_sense.1, cells.join(";"));
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
        if f.len() < 3 {
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
        assert_eq!(from_text(&to_text(&[s.clone()])).unwrap(), vec![s.clone()]);
        let t = s.get(1, 0);
        assert_eq!(t.time(3.0), Some(0.25));
        assert!((t.time(31.0).unwrap() - 2.125).abs() < 1e-4);
        assert_eq!(t.time(55.0), Some(4.0));
        assert_eq!(t.time(56.0), None);
    }
}
