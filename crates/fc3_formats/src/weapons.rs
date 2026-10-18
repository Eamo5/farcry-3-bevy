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
//! `BulletSpread`s give how far shots stray, by stance.

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const STATS_REL: &str = "weapons/stats.txt";
const MAGIC: &str = "fc3-weapons 8";

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
}

impl Gadget {
    fn to_text(&self) -> String {
        let n = [self.fov, self.damage, self.noise, self.moving_spread, self.reload, self.sight_time, self.look[0], self.look[1], self.look[2]];
        format!("{}:{}:{}", self.key, self.magazine, n.map(|v| v.to_string()).join(":"))
    }

    fn from_text(s: &str) -> Option<Gadget> {
        let mut p = s.split(':');
        let key = p.next()?.to_string();
        let magazine = p.next()?.parse().ok()?;
        let v: Vec<f32> = p.filter_map(|x| x.parse().ok()).collect();
        (v.len() == 9).then(|| Gadget {
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
    library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") {
            return;
        }
        let Some(e) = o.child("Entity") else { return };
        let Some(name) = e.str("hidName") else { return };
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
        });
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
        s += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{attachments}\t{ranges}\t{spread}\t{aim}\t{gadgets}\n",
            w.name,
            w.model,
            w.damage,
            w.fire_rate,
            w.magazine,
            w.reload,
            w.draw,
            w.holster,
            w.pellets,
            u8::from(w.silent)
        );
    }
    s
}

pub fn from_text(text: &str) -> Result<Vec<WeaponStats>> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
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
            gadgets: vec![
                Gadget { key: "acog".into(), fov: -30.0, damage: 1.0, noise: 1.0, moving_spread: 1.0, reload: 1.0, look: [1.0; 3], ..Default::default() },
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
