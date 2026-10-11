//! The help a pad gets aiming at enemies and animals, by difficulty, from
//! the player parameters among the object templates
//! (`playerparamsservice:players.playerparamsservice`): each target's
//! cylinder (widening with distance) that aiming snaps onto and shots bend
//! toward (`EnemyAimingHelpers`), and the billboards round a target within
//! which the view follows it as it moves (`FollowHelpers`). The cache keeps
//! them as text ([`ASSIST_REL`]).

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const ASSIST_REL: &str = "characters/assist.txt";
const MAGIC: &str = "fc3-assist 1";
const SERVICE: &str = "playerparamsservice:players.playerparamsservice";

/// The difficulties in the data's order (casual, normal, hard, infamous).
const LEVELS: [&str; 4] = ["Casual", "Experimented", "Hardcore", "Infamous"];

/// Snapping onto a target and bending shots toward it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Aiming {
    /// The cylinder round a target: its radius and how far it reaches above
    /// the head and below the feet, each a base plus so much a metre away;
    /// an animal's scaled by `animal`.
    pub radius: (f32, f32),
    pub height: (f32, f32),
    pub animal: (f32, f32),
    /// A nearer target only takes over from the one nearest the sights if
    /// this far from it.
    pub proximity: f32,
    /// Seconds the sights keep following a target they snapped onto, and
    /// fade off it; and the stick's own control coming back.
    pub follow: f32,
    pub fade: f32,
    pub stick_blend: f32,
    /// Aimed within this of the head, the sights snap to it; else to the
    /// body between this far below the head and this far above the torso.
    pub head_radius: f32,
    pub below_head: f32,
    pub above_torso: f32,
    /// The snap holds while within the cylinder scaled by these.
    pub keep: (f32, f32),
    /// Shots bend: the weapon's strength and cylinder scaled by these, and
    /// up or down only so much (animals, people).
    pub correction: (f32, f32),
    pub correction_vertical: (f32, f32),
}

/// The view following a target.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Follow {
    /// Half width, and reach above and below the head, of the inner and
    /// outer billboards round a target.
    pub inner: [f32; 3],
    pub outer: [f32; 3],
    /// Distances: none before the first, rising to full at the second, full
    /// to the third, gone at the fourth.
    pub distances: [f32; 4],
    /// How much of the target's movement across the view the view follows
    /// (yaw, pitch), how slowly that comes on (seconds), and how much while
    /// sprinting.
    pub intensity: (f32, f32),
    pub damping: f32,
    pub sprint: f32,
    /// How much a turn of the stick is eased round a target within this
    /// distance (yaw, pitch).
    pub stick: Option<(f32, f32, f32)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Assist {
    pub aiming: [Aiming; 4],
    pub follow: [Follow; 4],
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

/// The pad's aiming help from the templates.
pub fn assist(templates: &Object) -> Option<Assist> {
    let mut out = None;
    templates.walk(&mut |o, _| {
        if out.is_some() || o.str("FullName").as_deref() != Some(SERVICE) {
            return;
        }
        let mut a = Assist::default();
        let helpers = o.child("EnemyAimingHelpers");
        let follows = o.child("FollowHelpers");
        for (i, level) in LEVELS.iter().enumerate() {
            if let Some(h) = helpers.and_then(|h| h.child(&format!("AimingHelper{level}"))) {
                let f = |n: &str| float(h, n).unwrap_or(0.0);
                a.aiming[i] = Aiming {
                    radius: (f("fCylinderRadius"), f("fCylinderRadiusFactor")),
                    height: (f("fCylinderHeight"), f("fCylinderHeightFactor")),
                    animal: (f("fAnimalCylinderRadiusMultiplier"), f("fAnimalCylinderHeightMultiplier")),
                    proximity: f("fMinDistanceForProximityOverride"),
                    follow: f("fLockOnFollowTime"),
                    fade: f("fLockOnFadeOutTime"),
                    stick_blend: f("fLockOnStickRemovalBlendTime"),
                    head_radius: f("fLockOnHeadRadius"),
                    below_head: f("fLockOnMinDistanceBelowHead"),
                    above_torso: f("fLockOnMinDistanceAboveTorso"),
                    keep: (f("fLockOnRadiusFactor"), f("fLockOnHeightFactor")),
                    correction: (f("fShootCorrectionHelperStrengthMultiplier"), f("fShootCorrectionHelperCylinderMultiplier")),
                    correction_vertical: (f("fShootCorrectionHelperAnimalVerticalStrength"), f("fShootCorrectionHelperPawnVerticalStrength")),
                };
            }
            if let Some(h) = follows.and_then(|h| h.child(&format!("FollowHelper{level}"))) {
                let f = |n: &str| float(h, n).unwrap_or(0.0);
                a.follow[i] = Follow {
                    // (The game spells the inner one's "above" with a double i.)
                    inner: [f("fInnerBillboardX"), f("fIinnerBillboardYAbove"), f("fInnerBillboardYBelow")],
                    outer: [f("fOuterBillboardX"), f("fOuterBillboardYAbove"), f("fOuterBillboardYBelow")],
                    distances: [f("fMinDistForInfluence"), f("fMinDistForMaxInfluence"), f("fMaxDistForMaxInfluence"), f("fMaxDistForInfluence")],
                    intensity: (f("fMaxYawIntensity"), f("fMaxPitchIntensity")),
                    damping: f("fInfluenceDamping"),
                    sprint: f("fSprintFactor"),
                    stick: h
                        .field("bUseRightStickAssist")
                        .is_some_and(|b| b.first().copied().unwrap_or(0) != 0)
                        .then(|| (f("fMaxDistForRightStickAssist"), f("fYawRightStickAssist"), f("fPitchRightStickAssist"))),
                };
            }
        }
        out = Some(a);
    });
    out
}

impl Aiming {
    fn numbers(&self) -> Vec<f32> {
        vec![
            self.radius.0,
            self.radius.1,
            self.height.0,
            self.height.1,
            self.animal.0,
            self.animal.1,
            self.proximity,
            self.follow,
            self.fade,
            self.stick_blend,
            self.head_radius,
            self.below_head,
            self.above_torso,
            self.keep.0,
            self.keep.1,
            self.correction.0,
            self.correction.1,
            self.correction_vertical.0,
            self.correction_vertical.1,
        ]
    }

    fn from_numbers(v: &[f32]) -> Option<Aiming> {
        (v.len() == 19).then(|| Aiming {
            radius: (v[0], v[1]),
            height: (v[2], v[3]),
            animal: (v[4], v[5]),
            proximity: v[6],
            follow: v[7],
            fade: v[8],
            stick_blend: v[9],
            head_radius: v[10],
            below_head: v[11],
            above_torso: v[12],
            keep: (v[13], v[14]),
            correction: (v[15], v[16]),
            correction_vertical: (v[17], v[18]),
        })
    }

    /// The cylinder's radius and reach above and below at a distance, for a
    /// person or an animal.
    pub fn cylinder(&self, distance: f32, animal: bool) -> (f32, f32) {
        let (r, h) = (self.radius.0 + self.radius.1 * distance, self.height.0 + self.height.1 * distance);
        if animal {
            (r * self.animal.0, h * self.animal.1)
        } else {
            (r, h)
        }
    }
}

impl Follow {
    fn numbers(&self) -> Vec<f32> {
        let stick = self.stick.map_or([-1.0, 0.0, 0.0], |(d, y, p)| [d, y, p]);
        let mut v = Vec::new();
        v.extend(self.inner);
        v.extend(self.outer);
        v.extend(self.distances);
        v.extend([self.intensity.0, self.intensity.1, self.damping, self.sprint]);
        v.extend(stick);
        v
    }

    fn from_numbers(v: &[f32]) -> Option<Follow> {
        (v.len() == 17).then(|| Follow {
            inner: [v[0], v[1], v[2]],
            outer: [v[3], v[4], v[5]],
            distances: [v[6], v[7], v[8], v[9]],
            intensity: (v[10], v[11]),
            damping: v[12],
            sprint: v[13],
            stick: (v[14] >= 0.0).then_some((v[14], v[15], v[16])),
        })
    }

    /// How much of its help reaches a target this far off (0 to 1).
    pub fn reach(&self, distance: f32) -> f32 {
        let [a, b, c, d] = self.distances;
        if distance < a || distance > d {
            0.0
        } else if distance < b {
            (distance - a) / (b - a).max(1e-3)
        } else if distance <= c {
            1.0
        } else {
            1.0 - (distance - c) / (d - c).max(1e-3)
        }
    }
}

pub fn to_text(a: &Assist) -> String {
    let join = |v: Vec<f32>| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
    let mut s = format!("{MAGIC}\n");
    for i in 0..4 {
        s += &format!("aiming\t{i}\t{}\n", join(a.aiming[i].numbers()));
        s += &format!("follow\t{i}\t{}\n", join(a.follow[i].numbers()));
    }
    s
}

pub fn from_text(text: &str) -> Result<Assist> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not an aiming help table");
    }
    let mut a = Assist::default();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        let (Some(kind), Some(i), Some(v)) = (f.first(), f.get(1).and_then(|i| i.parse::<usize>().ok()).filter(|i| *i < 4), f.get(2)) else { continue };
        let v: Vec<f32> = v.split(',').filter_map(|x| x.parse().ok()).collect();
        match *kind {
            "aiming" => a.aiming[i] = Aiming::from_numbers(&v).unwrap_or_default(),
            "follow" => a.follow[i] = Follow::from_numbers(&v).unwrap_or_default(),
            _ => {}
        }
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_widens_with_distance() {
        let mut a = Assist::default();
        a.aiming[1] = Aiming { radius: (0.75, 0.12), height: (0.5, 0.06), animal: (1.0, 1.0), keep: (1.0, 1.0), ..Default::default() };
        a.aiming[2] = Aiming { radius: (0.25, 0.08), height: (0.4, 0.04), animal: (0.75, 0.75), ..Default::default() };
        a.follow[1] = Follow { distances: [0.0, 3.0, 20.0, 30.0], intensity: (0.6, 0.6), stick: Some((25.0, 0.6, 0.6)), ..Default::default() };
        assert_eq!(from_text(&to_text(&a)).unwrap(), a);
        let (r, h) = a.aiming[1].cylinder(10.0, false);
        assert!((r - 1.95).abs() < 1e-5 && (h - 1.1).abs() < 1e-5);
        assert!((a.aiming[2].cylinder(10.0, true).0 - 0.7875).abs() < 1e-5);
        let f = a.follow[1];
        assert_eq!((f.reach(1.5), f.reach(10.0), f.reach(25.0), f.reach(31.0)), (0.5, 1.0, 0.5, 0.0));
    }
}
