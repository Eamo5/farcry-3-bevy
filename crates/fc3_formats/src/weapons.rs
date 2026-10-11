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
const MAGIC: &str = "fc3-weapons 4";

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
        });
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
                offers.push((key, AttachmentOffer { kind: attach, name: String::new(), cost }));
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
        let attachments = w.attachments.iter().map(|a| format!("{}:{}:{}", a.kind, a.cost, a.name)).collect::<Vec<_>>().join(";");
        let ranges = w.ranges.iter().map(|r| r.map(|v| v.to_string()).join(":")).collect::<Vec<_>>().join(";");
        let spread = w.spread.map(|s| s.to_text()).unwrap_or_default();
        s += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{attachments}\t{ranges}\t{spread}\n",
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
                                let mut p = o.splitn(3, ':');
                                Some(AttachmentOffer { kind: p.next()?.parse().ok()?, cost: p.next()?.parse().ok()?, name: p.next()?.to_string() })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                spread: f.get(12).and_then(|s| Spread::from_text(s)),
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
            attachments: vec![AttachmentOffer { kind: ATTACH_SIGHT, name: "reflex A".into(), cost: 300 }],
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
        };
        assert_eq!(from_text(&to_text(&[w.clone()])).unwrap(), vec![w.clone()]);
        assert_eq!(w.range_scale(10.0), 1.0);
        assert!((w.range_scale(32.5) - 0.85).abs() < 1e-5);
        assert_eq!(w.range_scale(100.0), 0.7);
        assert_eq!(model_key(&w.model), model_key(r"Graphics\Weapons\ak74m\ak74m.xbg"));
    }
}
