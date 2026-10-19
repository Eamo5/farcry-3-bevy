//! The player's weapons' handling from the entity library: each
//! `WeaponProperties:FC3/<name>` prototype's `CWeaponProperties` (damage per
//! bullet, fire rate, magazine, reload and draw times, pellets, silence),
//! joined to its `weapons:FC3/<name>` prototype's model. The cache keeps
//! them as text ([`STATS_REL`]); the game matches them to its weapons by
//! model. With them go the attachments the shop sells for each weapon
//! (`shoppingitems.lib`: an attachment's name starts with its weapon's).
//! A weapon that fires a projectile (the bow's arrow) takes the
//! projectile's damage, and a weapon's damage falls off with range as its
//! `RangeMultipliers` (the normal difficulty's) say. Its first-person
//! `BulletSpread`s give how far shots stray, by stance. Its `Particles`
//! name its muzzle's effects (first and third person, and the smoke after),
//! and a suppressor gadget its own muzzle.

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const STATS_REL: &str = "weapons/stats.txt";
const MAGIC: &str = "fc3-weapons 12";
/// The form before the flame's mesh (read the same, without it).
const MAGIC_11: &str = "fc3-weapons 11";

/// Whether a cached table is in this version's form.
pub fn is_current(text: &str) -> bool {
    text.lines().next().map(str::trim) == Some(MAGIC)
}

/// The shop's attachment kinds (`selAttachmentType`).
pub const ATTACH_SIGHT: u32 = 1;
pub const ATTACH_BARREL: u32 = 2;
pub const ATTACH_SCOPE: u32 = 4;
pub const ATTACH_MAGAZINE: u32 = 5;

/// An attachment the shop sells for a weapon.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AttachmentOffer {
    /// [`ATTACH_SIGHT`], ...
    pub kind: u32,
    /// The shop's name for it, the weapon's taken off ("reflex A").
    pub name: String,
    pub cost: u32,
    /// Its name and description in the shop page (string ids) and its icon
    /// there (`sShopIconName`: a frame of the page's icons).
    pub name_id: u32,
    pub description_id: u32,
    pub icon: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WeaponStats {
    /// The prototype's name after `FC3/`.
    pub name: String,
    /// The weapon's model (lower case; the third-person one, as placed).
    pub model: String,
    /// Damage a bullet (or pellet) does.
    pub damage: f32,
    /// Rounds a minute.
    pub fire_rate: f32,
    pub magazine: u32,
    /// Seconds to reload a magazine, to draw the weapon and to put it away.
    pub reload: f32,
    pub draw: f32,
    pub holster: f32,
    /// Pellets a shot fires.
    pub pellets: u32,
    pub silent: bool,
    pub attachments: Vec<AttachmentOffer>,
    /// Damage over range: (from, to metres, multiplier at each end).
    pub ranges: Vec<[f32; 4]>,
    pub spread: Option<Spread>,
    pub aim: Option<Aim>,
    /// The attachments it takes, by the game's key ("acog", "reflexa",
    /// "silencerassault", "magupgrade"...).
    pub gadgets: Vec<Gadget>,
    /// Its muzzle's effects (`psMuzzleParticlesId`, `..._3rd` and
    /// `psBarrelSmokeMuzzleParticlesId`; empty when none).
    pub muzzle: String,
    pub muzzle_3p: String,
    pub barrel_smoke: String,
    /// A flamethrower's flame, if it is one.
    pub flame: Option<Flame>,
}

/// A flamethrower's flame (`FireStrategyProperties`): the effects its nozzle
/// plays (alight and idle, shooting, stopping; first person and third) and
/// those it sends down the stream (`FlameMesh`'s, `FireballEffects`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Flame {
    pub tip_idle: String,
    pub tip_shoot: String,
    pub tip_stop: String,
    pub tip_shoot_3p: String,
    pub tip_stop_3p: String,
    pub stream: Vec<StreamEffect>,
    /// Its flame mesh, if it draws one.
    pub rope: Option<FlameRope>,
}

/// A flame's mesh (`FlameMesh`): a tube round its rope, `ring` vertices
/// round, the rope's knots sent from the nozzle `segment` metres apart at
/// `speed` (m/s) for `length` metres. Up to `straight[0]` metres the rope
/// keeps straight ahead of the gun, from `straight[1]` it goes its own way
/// (in between it blends). The tube is as wide as `grow` (to so many metres,
/// from one width to another) and `shrink` (from so many metres to its end)
/// say, and isn't drawn nearer the nozzle than `cutoff`. The flame gets to
/// full strength in `grow_time` seconds. Its texture has `frames` frames
/// side by side, the next every `frame_time` seconds, tiled every `tiling`
/// metres along the rope.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlameRope {
    pub texture: String,
    pub frames: u32,
    pub frame_time: f32,
    pub tiling: f32,
    pub ring: u32,
    pub ring_angle: f32,
    pub length: f32,
    pub speed: f32,
    pub segment: f32,
    pub cutoff: f32,
    pub straight: [f32; 2],
    pub grow_time: f32,
    pub grow: [f32; 3],
    pub shrink: [f32; 3],
}

impl FlameRope {
    /// The tube's width so far along the rope (m), easing between its
    /// widths.
    pub fn width(&self, d: f32) -> f32 {
        let ease = |a: f32, b: f32, t: f32| {
            let t = t.clamp(0.0, 1.0);
            a + (b - a) * t * t * (3.0 - 2.0 * t)
        };
        let ([grow_to, g0, g1], [shrink_from, s0, s1]) = (self.grow, self.shrink);
        if d < grow_to {
            ease(g0, g1, d / grow_to.max(1e-3))
        } else if d < shrink_from {
            ease(g1, s0, (d - grow_to) / (shrink_from - grow_to).max(1e-3))
        } else {
            ease(s0, s1, (d - shrink_from) / (self.length - shrink_from).max(1e-3))
        }
    }

    fn to_text(&self) -> String {
        let n = [
            self.frames as f32,
            self.frame_time,
            self.tiling,
            self.ring as f32,
            self.ring_angle,
            self.length,
            self.speed,
            self.segment,
            self.cutoff,
            self.straight[0],
            self.straight[1],
            self.grow_time,
            self.grow[0],
            self.grow[1],
            self.grow[2],
            self.shrink[0],
            self.shrink[1],
            self.shrink[2],
        ];
        format!("{},{}", self.texture, n.map(|v| v.to_string()).join(","))
    }

    fn from_text(s: &str) -> Option<FlameRope> {
        let (texture, rest) = s.split_once(',')?;
        let n: Vec<f32> = rest.split(',').map(|v| v.parse().ok()).collect::<Option<_>>()?;
        let n: [f32; 18] = n.try_into().ok()?;
        Some(FlameRope {
            texture: texture.to_string(),
            frames: n[0] as u32,
            frame_time: n[1],
            tiling: n[2],
            ring: n[3] as u32,
            ring_angle: n[4],
            length: n[5],
            speed: n[6],
            segment: n[7],
            cutoff: n[8],
            straight: [n[9], n[10]],
            grow_time: n[11],
            grow: [n[12], n[13], n[14]],
            shrink: [n[15], n[16], n[17]],
        })
    }

    /// From a flame's `FlameMesh`, if it has a texture and a length.
    fn read(m: &Object) -> Option<FlameRope> {
        let texture = m.str_h(ROPE_TEXTURE).map(|s| s.trim_end_matches('\0').to_string()).filter(|s| !s.is_empty())?;
        let f = |name: &str| float(m, name).unwrap_or(0.0);
        let ease = |group: &str, to: f32| {
            let g = m.child(group);
            [to, g.and_then(|g| float(g, "fValueA")).unwrap_or(0.1), g.and_then(|g| float(g, "fValueB")).unwrap_or(0.1)]
        };
        let rope = FlameRope {
            texture,
            frames: f("fTextureFrames").max(1.0) as u32,
            frame_time: f("fTextureChangeTime"),
            tiling: f("fTeselation"),
            ring: uint(m, "iRingNVertex").unwrap_or(5) as u32,
            ring_angle: f("fRingStartAngle"),
            length: f("fSize"),
            speed: f("fSpeed"),
            segment: f("fSegmentLength"),
            cutoff: f("fMeshRingCutoffDistance"),
            straight: [f("fFlameRestitutionStartDistance"), f("fFlameRestitutionEndDistance")],
            grow_time: f("fFlameStrengthGrowthTime"),
            grow: ease("sizeGrowInterpolation", f("fSizeGrowInterpolationDist")),
            shrink: ease("sizeShrinkInterpolation", f("fSizeShrinkInterpolationDist")),
        };
        (rope.length > 0.0 && rope.speed > 0.0 && rope.segment > 0.0).then_some(rope)
    }
}

/// An effect sent down a flame's stream: one every so often (s), going so
/// fast (m/s) so far (m); first person and third. The wait between them
/// is so many times as long for how fast the aim turns (deg/s, its
/// `arkSpawnTimeAngularSpeedRatioCurve`'s points).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StreamEffect {
    pub effect: String,
    pub effect_3p: String,
    pub every: f32,
    pub speed: f32,
    pub distance: f32,
    pub turning: Vec<[f32; 2]>,
}

impl StreamEffect {
    /// The wait's multiplier when the aim turns so fast (deg/s), between
    /// its curve's points (1 without one).
    pub fn turning_ratio(&self, turn: f32) -> f32 {
        let c = &self.turning;
        match c.iter().position(|p| p[0] > turn) {
            None => c.last().map_or(1.0, |p| p[1]),
            Some(0) => c[0][1],
            Some(i) => {
                let (a, b) = (c[i - 1], c[i]);
                a[1] + (b[1] - a[1]) * (turn - a[0]) / (b[0] - a[0]).max(1e-6)
            }
        }
    }
}

impl Flame {
    fn to_text(&self) -> String {
        let tips = [&self.tip_idle, &self.tip_shoot, &self.tip_stop, &self.tip_shoot_3p, &self.tip_stop_3p].map(|s| s.as_str()).join(",");
        let stream = self
            .stream
            .iter()
            .map(|e| {
                let turning = e.turning.iter().map(|p| format!("{}:{}", p[0], p[1])).collect::<Vec<_>>().join("/");
                format!("{},{},{},{},{},{turning}", e.effect, e.effect_3p, e.every, e.speed, e.distance)
            })
            .collect::<Vec<_>>()
            .join(";");
        let rope = self.rope.as_ref().map(|r| r.to_text()).unwrap_or_default();
        format!("{tips}|{stream}|{rope}")
    }

    fn from_text(s: &str) -> Option<Flame> {
        let mut parts = s.splitn(3, '|');
        let (tips, stream, rope) = (parts.next()?, parts.next()?, parts.next().and_then(FlameRope::from_text));
        let t: Vec<&str> = tips.split(',').collect();
        let tip = |i: usize| t.get(i).map(|s| s.to_string()).unwrap_or_default();
        let stream = stream
            .split(';')
            .filter_map(|e| {
                let f: Vec<&str> = e.split(',').collect();
                let n = |i: usize| f.get(i).and_then(|v| v.parse().ok());
                let turning = f
                    .get(5)
                    .map(|c| c.split('/').filter_map(|p| p.split_once(':')).filter_map(|(x, y)| Some([x.parse().ok()?, y.parse().ok()?])).collect())
                    .unwrap_or_default();
                Some(StreamEffect { effect: f.first()?.to_string(), effect_3p: f.get(1)?.to_string(), every: n(2)?, speed: n(3)?, distance: n(4)?, turning })
            })
            .collect();
        Some(Flame { tip_idle: tip(0), tip_shoot: tip(1), tip_stop: tip(2), tip_shoot_3p: tip(3), tip_stop_3p: tip(4), stream, rope })
    }

    /// From a weapon's `FireStrategyProperties`, if it has a flame (its
    /// curves among `curves`, by library id).
    fn read(fire: &Object, curves: &HashMap<u64, Vec<[f32; 2]>>) -> Option<Flame> {
        let mesh = fire.child("FlameMesh");
        let tips = fire.child("TipEffects").and_then(|t| t.child("Effects"));
        if mesh.is_none() && tips.is_none() {
            return None;
        }
        let mut flame = Flame::default();
        for t in tips.map(|t| t.children.as_slice()).unwrap_or_default() {
            let name = effect_name(t, PS_NAME);
            if name.is_empty() {
                continue;
            }
            let first = t.bool("bFirstPerson").unwrap_or(true);
            // (`selType`: 0 alight and idle, 1 shooting, 4 stopping.)
            let slot = match (uint(t, "selType").unwrap_or(99), first) {
                (0, true) => &mut flame.tip_idle,
                (1, true) => &mut flame.tip_shoot,
                (4, true) => &mut flame.tip_stop,
                (1, false) => &mut flame.tip_shoot_3p,
                (4, false) => &mut flame.tip_stop_3p,
                _ => continue,
            };
            if slot.is_empty() {
                *slot = name;
            }
        }
        let launched = |o: &Object, every: &str, speed: &str, distance: &str| {
            let effect = effect_name(o, PS_NAME);
            (!effect.is_empty() && !o.bool("bRemove").unwrap_or(false)).then(|| StreamEffect {
                effect_3p: effect_name(o, PS_3P_NAME),
                effect,
                every: float(o, every).unwrap_or(0.0),
                speed: float(o, speed).unwrap_or(0.0),
                distance: float(o, distance).unwrap_or(0.0),
                turning: uint(o, "arkSpawnTimeAngularSpeedRatioCurve").and_then(|id| curves.get(&id)).cloned().unwrap_or_default(),
            })
        };
        if let Some(m) = mesh {
            flame.stream.extend(launched(m, "fPSSpawnTime", "fSpeed", "fSize"));
            flame.rope = FlameRope::read(m);
        }
        for e in fire.child("FireballEffects").and_then(|f| f.child("Effects")).map(|e| e.children.as_slice()).unwrap_or_default() {
            flame.stream.extend(launched(e, "fSpawnTime", "fSpeed", "fDistance"));
        }
        Some(flame)
    }
}

/// The names of a flame's effects (beside their ids): first person, third.
const PS_NAME: u32 = 0x93D2_AFB5;
const PS_3P_NAME: u32 = 0xA9BA_1966;
/// The name of a flame mesh's texture (beside its id).
const ROPE_TEXTURE: u32 = 0x3924_E150;

/// The names of the effects a weapon's `Particles` give (beside their ids).
const MUZZLE_NAME: u32 = 0xB171_F78F;
const MUZZLE_3P_NAME: u32 = 0x4C22_2D81;
const BARREL_SMOKE_NAME: u32 = 0x81F9_EF25;

/// An effect's name from a field, if it names one (not the placeholder, an
/// effect of nothing).
fn effect_name(o: &Object, field: u32) -> String {
    o.str_h(field).map(|s| s.trim_end_matches('\0').to_string()).filter(|s| s.contains('.') && !s.starts_with("fc3_placeholder")).unwrap_or_default()
}

/// An attachment as one weapon takes it: its key, the magazine it brings
/// (0 if none), and its gadget's modifiers (`CWeaponAttachmentComponent`):
/// degrees off the aimed FOV, the damage and the shot's noise (radius)
/// times so much, the spread on the move times so much, the reload times
/// so much, seconds more for the sights to come up, and the aimed turn
/// times so much (pad across, up and down, mouse).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Gadget {
    pub key: String,
    pub magazine: u32,
    pub fov: f32,
    pub damage: f32,
    pub noise: f32,
    pub moving_spread: f32,
    pub reload: f32,
    pub sight_time: f32,
    pub look: [f32; 3],
    /// Its own muzzle effect (a suppressor's), if any.
    pub muzzle: String,
}

impl Gadget {
    fn to_text(&self) -> String {
        let n = [self.fov, self.damage, self.noise, self.moving_spread, self.reload, self.sight_time, self.look[0], self.look[1], self.look[2]];
        format!("{}:{}:{}:{}", self.key, self.magazine, n.map(|v| v.to_string()).join(":"), self.muzzle)
    }

    fn from_text(s: &str) -> Option<Gadget> {
        let p: Vec<&str> = s.split(':').collect();
        let key = p.first()?.to_string();
        let magazine = p.get(1)?.parse().ok()?;
        let v: Vec<f32> = p.get(2..11)?.iter().filter_map(|x| x.parse().ok()).collect();
        let muzzle = p.get(11).map(|m| m.to_string()).unwrap_or_default();
        (v.len() == 9).then(|| Gadget {
            muzzle,
            key,
            magazine,
            fov: v[0],
            damage: v[1],
            noise: v[2],
            moving_spread: v[3],
            reload: v[4],
            sight_time: v[5],
            look: [v[6], v[7], v[8]],
        })
    }
}

/// A gadget prototype's modifiers (its key and magazine come from the
/// weapon).
fn gadget_mods(component: &Object) -> Option<Gadget> {
    let m = component.child("modifiers")?;
    let f = |o: Option<&std::sync::Arc<Object>>, n: &str, d: f32| o.and_then(|o| float(o, n)).unwrap_or(d);
    let me = Some(m);
    // The muzzle's sound stim (type 1): how far the shot is heard.
    let mut noise = 1.0;
    if let Some(stim) = m.child("MuzzleStim") {
        for s in stim.children_named("StimModifiers") {
            if uint(s, "selType") == Some(1) {
                noise = float(s, "fRadiusMultiplier").unwrap_or(1.0);
            }
        }
    }
    Some(Gadget {
        key: String::new(),
        magazine: 0,
        fov: f(m.child("fovModifiers"), "fAdditionalIronsightFOVDegrees", 0.0),
        damage: f(m.child("baseDamagePrimary"), "diceRollMultiplier", 1.0),
        noise,
        moving_spread: f(m.child("bulletSpreadMultipliers"), "fBulletSpread_movementModifier", 1.0),
        reload: f(me, "fReloadTimeMultiplier", 1.0),
        sight_time: f(me, "fIronsightTransitionTimeAdd", 0.0),
        look: [
            f(me, "fLookSpeedMultiplierIronsight", 1.0),
            f(me, "fVerticalLookSpeedMultiplierIronsight", 1.0),
            f(me, "fLookSpeedMultiplierIronsightMouse", 1.0),
        ],
        muzzle: effect_name(m, MUZZLE_NAME),
    })
}

/// A weapon's attachments (its common properties' `Attachments`), with
/// their gadgets' modifiers by library item id.
fn gadgets(common: &Object, mods: &HashMap<u64, Gadget>) -> Vec<Gadget> {
    let mut out = Vec::new();
    common.walk(&mut |o, _| {
        if !o.is("Attachments") {
            return;
        }
        let Some(id) = uint(o, "arkAttachmentArchetypeName").filter(|id| *id != 0) else { return };
        let Some(key) = o.str_h(0xF2E5_88CD).filter(|k| !k.is_empty() && k != "none") else { return };
        let Some(m) = mods.get(&id) else { return };
        out.push(Gadget { key: key.to_ascii_lowercase(), magazine: uint(o, "magazineUpgradeReplacement").unwrap_or(0) as u32, ..m.clone() });
    });
    out
}

/// Aiming down the sights (`IronSight`) and the help a pad gets
/// (`ShootCorrectionHelpers`): the view's FOV aimed (degrees), the seconds
/// the sights take to come up, how much slower the player moves and turns
/// aimed (with a pad, with the mouse, and up and down besides), whether
/// aiming locks on to a target near the sights (and how fast, degrees a
/// second, how wide a cylinder, and the distances it fades out between),
/// and how shots bend toward a target from the hip and aimed (strength,
/// cylinder factor, farthest distance).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Aim {
    pub fov: f32,
    pub transition: f32,
    pub move_speed: f32,
    pub look_pad: f32,
    pub look_mouse: f32,
    pub look_vertical: f32,
    pub lock_on: bool,
    pub lock_speed: f32,
    pub lock_cylinder: f32,
    pub lock_fade: (f32, f32),
    pub correction: [[f32; 3]; 2],
}

impl Aim {
    fn to_text(&self) -> String {
        let [h, i] = self.correction;
        [
            self.fov,
            self.transition,
            self.move_speed,
            self.look_pad,
            self.look_mouse,
            self.look_vertical,
            f32::from(u8::from(self.lock_on)),
            self.lock_speed,
            self.lock_cylinder,
            self.lock_fade.0,
            self.lock_fade.1,
            h[0],
            h[1],
            h[2],
            i[0],
            i[1],
            i[2],
        ]
        .map(|v| v.to_string())
        .join(",")
    }

    fn from_text(s: &str) -> Option<Aim> {
        let v: Vec<f32> = s.split(',').filter_map(|x| x.parse().ok()).collect();
        (v.len() == 17).then(|| Aim {
            fov: v[0],
            transition: v[1],
            move_speed: v[2],
            look_pad: v[3],
            look_mouse: v[4],
            look_vertical: v[5],
            lock_on: v[6] != 0.0,
            lock_speed: v[7],
            lock_cylinder: v[8],
            lock_fade: (v[9], v[10]),
            correction: [[v[11], v[12], v[13]], [v[14], v[15], v[16]]],
        })
    }
}

/// A weapon's aiming, from its common properties.
fn aim(common: &Object) -> Option<Aim> {
    let sight = common.child("IronSight")?;
    if !sight.bool("bCanIronsight").unwrap_or(false) {
        return None;
    }
    let f = |n: &str| float(sight, n).unwrap_or(0.0);
    let helpers = common.child("ShootCorrectionHelpers");
    let correction = |n: &str| {
        helpers.and_then(|h| h.child(n)).map_or([0.0; 3], |c| {
            let f = |n: &str| float(c, n).unwrap_or(0.0);
            [f("fShootCorrectionStrength"), f("fShootCorrectionCylinderFactor"), f("fShootCorrectionHelperMaxDistance")]
        })
    };
    Some(Aim {
        fov: f("fIronsightFOVDegrees"),
        transition: f("fIronsightTransitionTime"),
        move_speed: f("fMoveSpeedFactor"),
        look_pad: f("fLookSensitivityFactor"),
        look_mouse: f("fLookSensitivityFactorMouse"),
        look_vertical: f("fVerticalLookSensitivityFactor"),
        lock_on: sight.bool("bIronsightAimingHelperEnabled").unwrap_or(false),
        lock_speed: f("fAimingHelperLockOnSpeed"),
        lock_cylinder: f("fSnapAimCylinderFactor"),
        lock_fade: (f("fAimingHelperFadeOutStartDistance"), f("fAimingHelperFadeOutStopDistance")),
        correction: [correction("ShootCorrectionHelper"), correction("ShootCorrectionHelperIronsight")],
    })
    .filter(|a| a.fov > 0.0)
}

/// How far a weapon's shots stray (degrees off the aim), from the base
/// to the most after `shots` shots in a row, by stance; shrinking back at
/// `contraction` degrees a second, and `moving` times as much on the move.
/// A shotgun's pellets each stray `pellets` (yaw, pitch) about the shot.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Spread {
    pub hip: [f32; 2],
    pub aimed: [f32; 2],
    pub crouched: [f32; 2],
    pub crouched_aimed: [f32; 2],
    pub jumping: [f32; 2],
    pub shots: f32,
    pub contraction: f32,
    pub moving: f32,
    pub pellets: Option<[f32; 2]>,
}

impl Spread {
    fn to_text(&self) -> String {
        let p = self.pellets.unwrap_or([-1.0, -1.0]);
        [self.hip, self.aimed, self.crouched, self.crouched_aimed, self.jumping, [self.shots, self.contraction], [self.moving, 0.0], p]
            .iter()
            .flat_map(|v| v.iter())
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }

    fn from_text(s: &str) -> Option<Spread> {
        let v: Vec<f32> = s.split(',').filter_map(|x| x.parse().ok()).collect();
        (v.len() == 16).then(|| Spread {
            hip: [v[0], v[1]],
            aimed: [v[2], v[3]],
            crouched: [v[4], v[5]],
            crouched_aimed: [v[6], v[7]],
            jumping: [v[8], v[9]],
            shots: v[10],
            contraction: v[11],
            moving: v[12],
            pellets: (v[14] >= 0.0).then_some([v[14], v[15]]),
        })
    }
}

/// The first-person spread table under a weapon's properties.
fn spread(props: &Object, fire: Option<&Object>) -> Option<Spread> {
    let mut first = None;
    props.walk(&mut |o, _| {
        if first.is_none() && o.is("FirstPerson") && o.child("BulletSpread").is_some() {
            first = Some(o);
        }
    });
    let first = first?;
    let pair =
        |n: &str| -> [f32; 2] { first.child(n).map_or([0.0; 2], |b| [float(b, "fBaseSpread").unwrap_or(0.0), float(b, "fMaxBulletSpread").unwrap_or(0.0)]) };
    let angles = fire
        .filter(|f| f.bool("bUseAngleSpread").unwrap_or(false))
        .map(|f| [float(f, "fAngleYawBulletSpread").unwrap_or(0.0), float(f, "fAnglePitchBulletSpread").unwrap_or(0.0)]);
    Some(Spread {
        hip: pair("BulletSpread"),
        aimed: pair("BulletSpread_ironSight"),
        crouched: pair("BulletSpread_crouch"),
        crouched_aimed: pair("BulletSpread_crouch_ironSight"),
        jumping: pair("fBulletSpread_jump"),
        shots: float(first, "fMax_spread_bullet_number").unwrap_or(1.0).max(1.0),
        contraction: float(first, "fShooting_contraction_speed").unwrap_or(10.0),
        moving: float(first, "fBulletSpread_movement_modifier").filter(|m| *m > 0.0).unwrap_or(1.0),
        pellets: angles,
    })
}

impl WeaponStats {
    /// The damage multiplier at a distance (whole before the first band,
    /// the last band's end beyond the last).
    pub fn range_scale(&self, distance: f32) -> f32 {
        let mut scale = 1.0;
        for &[from, to, start, end] in &self.ranges {
            if distance < from {
                break;
            }
            scale = if distance >= to || to <= from { end } else { start + (end - start) * (distance - from) / (to - from) };
        }
        scale
    }
}

fn float(o: &Object, name: &str) -> Option<f32> {
    // (Some counts are stored as floats, some as integers.)
    let b = o.field(name)?;
    match b.len() {
        4 => {
            let v = f32::from_le_bytes(b.try_into().ok()?);
            let i = u32::from_le_bytes(b.try_into().ok()?);
            // A float's bits read as a small integer are an integer.
            Some(if i < 0x0010_0000 { i as f32 } else { v })
        }
        1 => Some(b[0] as f32),
        _ => None,
    }
}

fn uint(o: &Object, name: &str) -> Option<u64> {
    let b = o.field(name)?;
    (!b.is_empty() && b.len() <= 8).then(|| b.iter().rev().fold(0u64, |v, &x| (v << 8) | x as u64))
}

/// The player's weapons from the entity library, with the attachments the
/// shop's items (if given) sell for them.
pub fn weapon_stats(library: &Object, shop_items: Option<&Object>) -> Vec<WeaponStats> {
    let mut models: HashMap<String, String> = HashMap::new();
    let mut props: Vec<(String, Object)> = Vec::new();
    // Library item id -> weapon name, for the shop's weapons.
    let mut by_item: HashMap<u64, String> = HashMap::new();
    // Library item id -> a projectile's damage.
    let mut projectiles: HashMap<u64, f32> = HashMap::new();
    // Library item id -> an attachment gadget's modifiers.
    let mut gadget_by_id: HashMap<u64, Gadget> = HashMap::new();
    // Library item id -> a curve's points (`Curves:`).
    let mut curves: HashMap<u64, Vec<[f32; 2]>> = HashMap::new();
    library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") {
            return;
        }
        let Some(e) = o.child("Entity") else { return };
        let Some(name) = e.str("hidName") else { return };
        if let (true, Some(id)) = (name.starts_with("Curves:"), uint(o, "disLibItemId")) {
            let knots = e.child("curveCurve").and_then(|c| c.child("Knots")).map(|k| k.children.as_slice()).unwrap_or_default();
            let point = |k: &Object| {
                let v = k.field("Value")?;
                Some([f32::from_le_bytes(v.get(0..4)?.try_into().ok()?), f32::from_le_bytes(v.get(4..8)?.try_into().ok()?)])
            };
            curves.insert(id, knots.iter().filter_map(|k| point(k)).collect());
        }
        if let Some(id) = uint(o, "disLibItemId") {
            let mut damage = None;
            e.walk(&mut |c, _| {
                if damage.is_none() && c.is("BaseDamage") {
                    damage = float(c, "fLevel").filter(|d| *d > 0.0);
                }
            });
            if let Some(d) = damage {
                projectiles.insert(id, d);
            }
            if name.starts_with("gadgets:") {
                if let Some(g) = e.child("Components").and_then(|c| c.child("CWeaponAttachmentComponent")).and_then(|c| gadget_mods(c)) {
                    gadget_by_id.insert(id, g);
                }
            }
        }
        if let (Some(n), Some(id)) = (name.strip_prefix("weapons:FC3/").filter(|n| !n.contains('.')), uint(o, "disLibItemId")) {
            by_item.insert(id, n.to_ascii_lowercase());
        }
        if let Some(n) = name.strip_prefix("weapons:FC3/").filter(|n| !n.contains('.')) {
            let mut model = None;
            e.walk(&mut |c, _| {
                if model.is_none() {
                    model = c.str_h(0xC230_E653).filter(|m| m.to_ascii_lowercase().ends_with(".xbg"));
                }
            });
            if let Some(m) = model {
                models.insert(n.to_ascii_lowercase(), m.to_ascii_lowercase());
            }
        } else if let Some(n) = name.strip_prefix("WeaponProperties:FC3/").filter(|n| !n.contains('.')) {
            if let Some(p) = e.child("Components").and_then(|c| c.child("CWeaponProperties")) {
                props.push((n.to_string(), p.as_ref().clone()));
            }
        }
    });
    let mut out = Vec::new();
    for (name, p) in props {
        let Some(model) = models.get(&name.to_ascii_lowercase()) else { continue };
        let Some(common) = p.child("CommonProperties") else { continue };
        let fire = p.child("FireStrategyProperties");
        let get = |o: Option<&Object>, f: &str| o.and_then(|o| float(o, f)).unwrap_or(0.0);
        let reloading = common.child("Reloading").map(|r| r.as_ref());
        let rate = common.child("FireRate").map(|r| r.as_ref());
        let ammo = common.child("Ammo").map(|r| r.as_ref());
        let mut damage = get(fire.and_then(|f| f.child("BaseDamage")).map(|d| d.as_ref()), "fLevel");
        if damage <= 0.0 {
            damage = fire.and_then(|f| uint(f, "arkProjectileArchetype")).and_then(|id| projectiles.get(&id)).copied().unwrap_or(0.0);
        }
        let ranges = fire
            .and_then(|f| f.child("RangeMultipliers"))
            .and_then(|r| r.child("RangeMultiplierExperimented"))
            .and_then(|r| r.child("RangeMultiplierDifficulty"))
            .map(|r| {
                r.children_named("RangeMultiplier")
                    .filter_map(|m| {
                        let f = |n: &str| float(m, n).unwrap_or(0.0);
                        let band = [f("fMinRange"), f("fMaxRange"), f("fStartMultiplier"), f("fEndMultiplier")];
                        (band[1] > band[0] && band[2] > 0.0).then_some(band)
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(WeaponStats {
            name,
            model: model.clone(),
            damage,
            fire_rate: get(rate, "iFireRate"),
            magazine: get(ammo, "iAmmoInClip") as u32,
            reload: get(reloading, "fReloadTimeNormal"),
            draw: get(Some(&common), "fDrawTime"),
            holster: get(Some(&common), "fHolsterTime"),
            pellets: get(fire.map(|f| f.as_ref()), "iBulletsShot").max(1.0) as u32,
            silent: common.bool("bIsSilent").unwrap_or(false),
            attachments: Vec::new(),
            ranges,
            spread: spread(&p, fire.map(|f| f.as_ref())),
            aim: aim(&common),
            gadgets: Vec::new(),
            muzzle: String::new(),
            muzzle_3p: String::new(),
            barrel_smoke: String::new(),
            flame: None,
        });
        let mut flame = None;
        p.walk(&mut |c, _| {
            if flame.is_none() && c.is("FireStrategyProperties") {
                flame = Flame::read(c, &curves);
            }
        });
        if let Some(w) = out.last_mut() {
            w.flame = flame;
        }
        let mut particles = None;
        p.walk(&mut |c, _| {
            if particles.is_none() && c.is("Particles") && c.field_h(MUZZLE_NAME).is_some() {
                particles = Some(c);
            }
        });
        if let (Some(w), Some(fx)) = (out.last_mut(), particles) {
            w.muzzle = effect_name(fx, MUZZLE_NAME);
            w.muzzle_3p = effect_name(fx, MUZZLE_3P_NAME);
            w.barrel_smoke = effect_name(fx, BARREL_SMOKE_NAME);
        }
        let mut found = gadgets(&common, &gadget_by_id);
        found.dedup_by(|a, b| a.key == b.key);
        if let Some(w) = out.last_mut() {
            w.gadgets = found;
        }
    }
    if let Some(items) = shop_items {
        // The shop's weapons by their item name ("Assault rifles/P416"),
        // then each attachment under the longest weapon name it starts with.
        let mut weapons: Vec<(String, String)> = Vec::new();
        let mut offers: Vec<(String, AttachmentOffer)> = Vec::new();
        for e in &items.children {
            let Some(key) = e.str("text_hidName") else { continue };
            let kind = uint(e, "shpitpeItemType").unwrap_or(0) & 0xFF;
            let sold = uint(e, "shpcatCategory").unwrap_or(0xFF) & 0xFF != 0xFF;
            if kind == 66 {
                if let Some(w) = uint(e, "arkArchetype").and_then(|a| by_item.get(&a)) {
                    weapons.push((key, w.clone()));
                }
            } else if kind == 67 && sold {
                let attach = uint(e, "selAttachmentType").unwrap_or(0) as u32;
                let cost = uint(e, "iCost").unwrap_or(0) as u32;
                let name_id = uint(e, "locidItemNameId").unwrap_or(0) as u32;
                let description_id = uint(e, "locidDescriptionId").unwrap_or(0) as u32;
                let icon = e.str("sShopIconName").filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_graphic())).unwrap_or_default();
                offers.push((key, AttachmentOffer { kind: attach, name: String::new(), cost, name_id, description_id, icon }));
            }
        }
        weapons.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        for (key, mut offer) in offers {
            let Some((wkey, weapon)) = weapons.iter().find(|(k, _)| key.starts_with(&format!("{k} "))) else { continue };
            offer.name = key[wkey.len() + 1..].to_string();
            if let Some(s) = out.iter_mut().find(|s| s.name.eq_ignore_ascii_case(weapon)) {
                s.attachments.push(offer);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn to_text(stats: &[WeaponStats]) -> String {
    let mut s = format!("{MAGIC}\n");
    for w in stats {
        let attachments = w
            .attachments
            .iter()
            .map(|a| format!("{}:{}:{}:{}:{}:{}", a.kind, a.cost, a.name_id, a.description_id, a.icon, a.name))
            .collect::<Vec<_>>()
            .join(";");
        let ranges = w.ranges.iter().map(|r| r.map(|v| v.to_string()).join(":")).collect::<Vec<_>>().join(";");
        let spread = w.spread.map(|s| s.to_text()).unwrap_or_default();
        let aim = w.aim.map(|a| a.to_text()).unwrap_or_default();
        let gadgets = w.gadgets.iter().map(|g| g.to_text()).collect::<Vec<_>>().join(";");
        let flame = w.flame.as_ref().map(|f| f.to_text()).unwrap_or_default();
        s += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{attachments}\t{ranges}\t{spread}\t{aim}\t{gadgets}\t{}\t{}\t{}\t{flame}\n",
            w.name,
            w.model,
            w.damage,
            w.fire_rate,
            w.magazine,
            w.reload,
            w.draw,
            w.holster,
            w.pellets,
            u8::from(w.silent),
            w.muzzle,
            w.muzzle_3p,
            w.barrel_smoke
        );
    }
    s
}

pub fn from_text(text: &str) -> Result<Vec<WeaponStats>> {
    let mut lines = text.lines();
    if !matches!(lines.next().map(str::trim), Some(MAGIC | MAGIC_11)) {
        bail!("not a weapon stats table");
    }
    Ok(lines
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 10 {
                return None;
            }
            let n = |i: usize| f[i].parse::<f32>().unwrap_or(0.0);
            Some(WeaponStats {
                name: f[0].into(),
                model: f[1].into(),
                damage: n(2),
                fire_rate: n(3),
                magazine: n(4) as u32,
                reload: n(5),
                draw: n(6),
                holster: n(7),
                pellets: n(8) as u32,
                silent: f[9] == "1",
                attachments: f
                    .get(10)
                    .map(|a| {
                        a.split(';')
                            .filter_map(|o| {
                                let mut p = o.splitn(6, ':');
                                let (kind, cost) = (p.next()?.parse().ok()?, p.next()?.parse().ok()?);
                                let (name_id, description_id) = (p.next()?.parse().ok()?, p.next()?.parse().ok()?);
                                let icon = p.next()?.to_string();
                                Some(AttachmentOffer { kind, cost, name_id, description_id, icon, name: p.next()?.to_string() })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                spread: f.get(12).and_then(|s| Spread::from_text(s)),
                aim: f.get(13).and_then(|s| Aim::from_text(s)),
                gadgets: f.get(14).map(|g| g.split(';').filter_map(Gadget::from_text).collect()).unwrap_or_default(),
                muzzle: f.get(15).map(|s| s.to_string()).unwrap_or_default(),
                muzzle_3p: f.get(16).map(|s| s.to_string()).unwrap_or_default(),
                barrel_smoke: f.get(17).map(|s| s.to_string()).unwrap_or_default(),
                flame: f.get(18).and_then(|s| Flame::from_text(s)),
                ranges: f
                    .get(11)
                    .map(|r| {
                        r.split(';')
                            .filter_map(|b| {
                                let v: Vec<f32> = b.split(':').filter_map(|x| x.parse().ok()).collect();
                                (v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// A weapon model's path for matching: lower case, the third-person
/// model's `_tp` suffix taken off.
pub fn model_key(path: &str) -> String {
    let p = path.to_ascii_lowercase().replace('/', "\\");
    p.strip_suffix("_tp.xbg").map(|b| format!("{b}.xbg")).unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_waits_by_how_fast_the_aim_turns() {
        let s = StreamEffect { turning: vec![[0.0, 2.5], [180.0, 0.3], [180.0, 0.3], [500.0, 0.1]], ..Default::default() };
        assert_eq!(s.turning_ratio(0.0), 2.5);
        assert!((s.turning_ratio(90.0) - 1.4).abs() < 1e-5);
        assert!((s.turning_ratio(340.0) - 0.2).abs() < 1e-5);
        assert_eq!(s.turning_ratio(900.0), 0.1);
        assert_eq!(StreamEffect::default().turning_ratio(50.0), 1.0);
    }

    #[test]
    fn round_trips_and_matches_models() {
        let w = WeaponStats {
            name: "AK47".into(),
            model: r"graphics\weapons\ak74m\ak74m_tp.xbg".into(),
            damage: 40.0,
            fire_rate: 600.0,
            magazine: 30,
            reload: 2.4,
            draw: 0.85,
            holster: 0.3,
            pellets: 1,
            silent: false,
            attachments: vec![AttachmentOffer {
                kind: ATTACH_SIGHT,
                name: "reflex A".into(),
                cost: 300,
                name_id: 82897,
                description_id: 82912,
                icon: "ReflexSight".into(),
            }],
            ranges: vec![[0.0, 25.0, 1.0, 1.0], [25.0, 40.0, 1.0, 0.7]],
            spread: Some(Spread {
                hip: [2.0, 3.75],
                aimed: [0.2, 1.0],
                crouched: [1.7, 3.75],
                crouched_aimed: [0.2, 1.0],
                jumping: [4.0, 4.0],
                shots: 3.0,
                contraction: 10.0,
                moving: 1.4,
                pellets: Some([1.0, 1.0]),
            }),
            aim: Some(Aim {
                fov: 65.0,
                transition: 0.35,
                move_speed: 0.4,
                look_pad: 0.3,
                look_mouse: 0.7,
                look_vertical: 1.0,
                lock_on: true,
                lock_speed: 45.0,
                lock_cylinder: 1.0,
                lock_fade: (40.0, 40.1),
                correction: [[3.0, 1.0, 15.0], [2.0, 0.75, 25.0]],
            }),
            muzzle: "fx.ak.muzzle".into(),
            muzzle_3p: "fx.ak.muzzle_3p".into(),
            barrel_smoke: "fx.ak.after".into(),
            flame: Some(Flame {
                tip_idle: "fx.flame.idle".into(),
                tip_shoot: "fx.flame.shoot".into(),
                tip_stop: String::new(),
                tip_shoot_3p: "fx.flame.shoot_3rd".into(),
                tip_stop_3p: "fx.flame.stop_3rd".into(),
                stream: vec![StreamEffect {
                    effect: "fx.flame.ball".into(),
                    effect_3p: "fx.flame.ball_3rd".into(),
                    every: 0.01,
                    speed: 20.0,
                    distance: 40.0,
                    turning: vec![[0.0, 2.5], [180.0, 0.3]],
                }],
                rope: Some(FlameRope {
                    texture: "graphics\\fx\\flame.xbt".into(),
                    frames: 4,
                    frame_time: 0.03,
                    tiling: 1.0,
                    ring: 5,
                    ring_angle: 0.0,
                    length: 15.0,
                    speed: 15.0,
                    segment: 0.5,
                    cutoff: 1.5,
                    straight: [2.0, 4.0],
                    grow_time: 1.0,
                    grow: [8.0, 0.075, 0.2],
                    shrink: [10.0, 0.2, 0.075],
                }),
            }),
            gadgets: vec![
                Gadget { key: "acog".into(), fov: -30.0, damage: 1.0, noise: 1.0, moving_spread: 1.0, reload: 1.0, look: [1.0; 3], ..Default::default() },
                Gadget {
                    key: "silencerassault".into(),
                    damage: 1.0,
                    noise: 0.02,
                    moving_spread: 1.0,
                    reload: 1.0,
                    look: [1.0; 3],
                    muzzle: "fx.silencer.muzzle".into(),
                    ..Default::default()
                },
                Gadget {
                    key: "magupgrade".into(),
                    magazine: 40,
                    damage: 1.0,
                    noise: 1.0,
                    moving_spread: 1.0,
                    reload: 1.0,
                    look: [1.0; 3],
                    ..Default::default()
                },
            ],
        };
        assert_eq!(from_text(&to_text(&[w.clone()])).unwrap(), vec![w.clone()]);
        assert_eq!(w.range_scale(10.0), 1.0);
        assert!((w.range_scale(32.5) - 0.85).abs() < 1e-5);
        assert_eq!(w.range_scale(100.0), 0.7);
        assert_eq!(model_key(&w.model), model_key(r"Graphics\Weapons\ak74m\ak74m.xbg"));
    }
}
