//! The player's weapons' handling from the entity library: each
//! `WeaponProperties:FC3/<name>` prototype's `CWeaponProperties` (damage per
//! bullet, fire rate, magazine, reload and draw times, pellets, silence),
//! joined to its `weapons:FC3/<name>` prototype's model. The cache keeps
//! them as text ([`STATS_REL`]); the game matches them to its weapons by
//! model. With them go the attachments the shop sells for each weapon
//! (`shoppingitems.lib`: an attachment's name starts with its weapon's).

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const STATS_REL: &str = "weapons/stats.txt";
const MAGIC: &str = "fc3-weapons 2";

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
    library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") {
            return;
        }
        let Some(e) = o.child("Entity") else { return };
        let Some(name) = e.str("hidName") else { return };
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
        out.push(WeaponStats {
            name,
            model: model.clone(),
            damage: get(fire.and_then(|f| f.child("BaseDamage")).map(|d| d.as_ref()), "fLevel"),
            fire_rate: get(rate, "iFireRate"),
            magazine: get(ammo, "iAmmoInClip") as u32,
            reload: get(reloading, "fReloadTimeNormal"),
            draw: get(Some(&common), "fDrawTime"),
            holster: get(Some(&common), "fHolsterTime"),
            pellets: get(fire.map(|f| f.as_ref()), "iBulletsShot").max(1.0) as u32,
            silent: common.bool("bIsSilent").unwrap_or(false),
            attachments: Vec::new(),
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
        s += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{attachments}\n",
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
        };
        assert_eq!(from_text(&to_text(&[w.clone()])).unwrap(), vec![w.clone()]);
        assert_eq!(model_key(&w.model), model_key(r"Graphics\Weapons\ak74m\ak74m.xbg"));
    }
}
