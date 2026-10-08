//! Weapon stock from the installed game's shopping database, joined to its
//! entity-library prototypes. Kept separately from the story's name lookup so
//! the host can honor item/category ids, prices and tutorial shop restrictions.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use crate::fcb::Object;

pub const ITEMS_LIB: &str = r"generated\databases\generic\shoppingitems.lib";
pub const WEAPONS_REL: &str = "story/shop_weapons.txt";
const FORMAT: &str = "fc3shop 1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Weapon {
    pub id: u32,
    /// The weapon-family category used by `OverrideShopMenu`.
    pub category: u32,
    pub cost: u32,
    pub sort: u32,
    pub name_id: u32,
    pub unlock_script: String,
    pub unlock_mission: String,
    pub archetype: String,
    pub model: String,
}

impl Weapon {
    /// Signature/DLC variants use a dotted prototype name and can share the
    /// base weapon's model. A model alone does not make them the same item.
    pub fn is_base(&self) -> bool {
        !self.archetype.contains('.')
    }

    /// The shopping database's `TowerN` script frees this item after N radio
    /// towers, independently of whether it can already be bought for money.
    pub fn tower_requirement(&self) -> Option<usize> {
        let number = self.unlock_script.strip_prefix("Tower")?;
        if number.is_empty() || !number.bytes().all(|c| c.is_ascii_digit()) { return None }
        number.parse().ok().filter(|&n| n > 0)
    }
}

fn uint(o: &Object, name: &str) -> Option<u64> {
    let b = o.field(name)?;
    if b.is_empty() || b.len() > 8 { return None }
    Some(b.iter().rev().fold(0u64, |v, &x| (v << 8) | x as u64))
}

/// Unset strings are sometimes a single zero byte rather than a string.
fn text(o: &Object, name: &str) -> String {
    o.str(name).filter(|s| !s.is_empty() && s.chars().all(|c| !c.is_control()) && !s.contains('\u{fffd}')).unwrap_or_default()
}

pub fn weapons(items: &Object, library: &Object) -> Vec<Weapon> {
    let mut prototypes = HashMap::new();
    library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") { return }
        let Some(id) = uint(o, "disLibItemId") else { return };
        let Some(entity) = o.child("Entity") else { return };
        let Some(archetype) = entity.str("hidName").filter(|a| a.starts_with("weapons:")) else { return };
        let mut model = None;
        entity.walk(&mut |c, _| {
            if model.is_none() {
                model = c.str_h(0xC230_E653).filter(|m| m.to_ascii_lowercase().ends_with(".xbg"));
            }
        });
        if let Some(model) = model {
            prototypes.insert(id, (archetype, model.to_ascii_lowercase()));
        }
    });
    let mut out = Vec::new();
    for item in &items.children {
        let Some((archetype, model)) = uint(item, "arkArchetype").and_then(|id| prototypes.get(&id)) else { continue };
        let (Some(id), Some(category), Some(cost)) = (uint(item, "disId"), uint(item, "shpscataSubCategory1"), uint(item, "iCost")) else { continue };
        if id >= u16::MAX as u64 || category >= u8::MAX as u64 || cost >= u32::MAX as u64 || item.bool("bIsLootItem") == Some(true) { continue }
        out.push(Weapon {
            id: id as u32, category: category as u32, cost: cost as u32,
            sort: uint(item, "iSortId").unwrap_or(0) as u32,
            name_id: uint(item, "locidItemNameId").unwrap_or(0) as u32,
            unlock_script: text(item, "UnlockFromScript"), unlock_mission: text(item, "UnlockOnMissionComplete"),
            archetype: archetype.clone(), model: model.clone(),
        });
    }
    out.sort_by_key(|w| (w.category, w.sort, w.id));
    out
}

pub fn to_text(weapons: &[Weapon]) -> String {
    let mut out = format!("{FORMAT}\n");
    for w in weapons {
        out += &format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n", w.id, w.category, w.cost, w.sort, w.name_id,
            w.unlock_script, w.unlock_mission, w.archetype, w.model);
    }
    out
}

pub fn from_text(text: &str) -> Result<Vec<Weapon>> {
    let mut lines = text.lines();
    if lines.next() != Some(FORMAT) { bail!("not a current weapon shop table") }
    lines.filter(|l| !l.is_empty()).map(|l| {
        let fields: Vec<_> = l.split('\t').collect();
        if fields.len() != 9 { bail!("malformed weapon shop entry") }
        let number = |i: usize| fields[i].parse::<u32>().context("invalid weapon shop number");
        Ok(Weapon {
            id: number(0)?, category: number(1)?, cost: number(2)?, sort: number(3)?, name_id: number(4)?,
            unlock_script: fields[5].into(), unlock_mission: fields[6].into(), archetype: fields[7].into(), model: fields[8].into(),
        })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn object(name: &str, fields: Vec<(&str, Vec<u8>)>, children: Vec<Object>) -> Object {
        Object { name_hash: crate::hash::name_hash(name), fields: fields.into_iter().map(|(n, b)| (crate::hash::name_hash(n), b)).collect(),
            children: children.into_iter().map(Arc::new).collect() }
    }

    #[test]
    fn shopping_ids_and_prices_join_to_weapon_prototypes() {
        let model = "graphics\\test\\pistol.xbg";
        let mut component = Object::default();
        component.fields.push((0xC230_E653, model.as_bytes().to_vec()));
        let entity = object("Entity", vec![("hidName", b"weapons:TestPistol".to_vec())], vec![component]);
        let prototype = object("EntityPrototype", vec![("disLibItemId", 1234u64.to_le_bytes().to_vec())], vec![entity]);
        let item = object("Item", vec![
            ("arkArchetype", 1234u64.to_le_bytes().to_vec()), ("disId", 42u16.to_le_bytes().to_vec()),
            ("shpscataSubCategory1", vec![7]), ("iCost", 75u32.to_le_bytes().to_vec()),
            ("locidItemNameId", 999u32.to_le_bytes().to_vec()), ("UnlockFromScript", b"TestTower".to_vec()),
            ("UnlockOnMissionComplete", vec![0]),
        ], Vec::new());
        let mut invalid = item.clone();
        invalid.fields.retain(|(h, _)| *h != crate::hash::name_hash("arkArchetype"));
        let stock = weapons(&object("lib", Vec::new(), vec![item, invalid]), &object("lib", Vec::new(), vec![prototype]));
        assert_eq!(stock.len(), 1);
        assert_eq!((stock[0].id, stock[0].category, stock[0].cost), (42, 7, 75));
        assert_eq!((stock[0].archetype.as_str(), stock[0].model.as_str()), ("weapons:TestPistol", model));
        assert_eq!(stock[0].unlock_script, "TestTower");
        assert!(stock[0].unlock_mission.is_empty());
        assert_eq!(from_text(&to_text(&stock)).unwrap(), stock);
    }

    #[test]
    fn malformed_or_outdated_catalogs_are_rejected() {
        assert!(from_text("fc3shop 0\n").is_err());
        assert!(from_text("fc3shop 1\n1\t2\n").is_err());
        assert!(from_text("fc3shop 1\na\t2\t3\t4\t5\t\t\tweapon\tmodel\n").is_err());
        assert!(from_text("fc3shop 1\n").unwrap().is_empty());
    }

    #[test]
    fn only_positive_tower_scripts_define_a_free_weapon_threshold() {
        let mut weapon = from_text("fc3shop 1\n1\t2\t3\t4\t5\t\t\tweapon\tmodel\n").unwrap().remove(0);
        for (script, requirement) in [("Tower1", Some(1)), ("Tower15", Some(15)), ("Tower99", Some(99)),
            ("", None), ("Tower", None), ("Tower0", None), ("Tower-1", None), ("Tower+1", None), ("Tower1_extra", None), ("Mission1", None)] {
            weapon.unlock_script = script.into();
            assert_eq!(weapon.tower_requirement(), requirement, "{script}");
        }
    }
}
