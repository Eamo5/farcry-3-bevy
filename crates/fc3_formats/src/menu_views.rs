//! The menus' 3D item views: the model the engine draws for a shop item
//! (`shoppingitems.lib`'s `arkDisplayArchetype`) or a crafting recipe
//! (`craftingrecipe.lib`'s `arkMenuArchetype`) where the page's `3D_*`
//! clips stand, turned as the item says (`DisplayDefaultAngles`) and swaying
//! (`fRotationSpeed`, `fRotationAmplitude`).

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const REL: &str = "ui/menu_views.txt";
const MAGIC: &str = "fc3-menu-views 2";

/// An item's view: its model, its turn (Dunia angles, degrees) and its
/// sway (how fast, in turns a second, and how far, in turns).
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub model: String,
    pub angles: [f32; 3],
    pub speed: f32,
    pub amplitude: f32,
}

/// The shop's items' views by their id (`disId`); its subcategories' (the
/// item each shows for itself, `bDisplayAsDefaultItem`, as it shows it
/// there); and the recipes' by the recipe's id (its archetype's entity
/// name, as the crafting tables have it).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MenuViews {
    pub shop: HashMap<u32, View>,
    pub categories: HashMap<u32, View>,
    pub recipes: HashMap<String, View>,
}

fn uint(o: &Object, name: &str) -> Option<u64> {
    let b = o.field(name)?;
    if b.is_empty() || b.len() > 8 {
        return None;
    }
    Some(b.iter().rev().fold(0u64, |v, &x| (v << 8) | x as u64))
}

/// The library's prototypes: by id, their entity's name and the model they
/// draw (their model selector's, else their graphic component's).
fn prototypes(library: &Object) -> HashMap<u64, (String, Option<String>)> {
    let mut out = HashMap::new();
    library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") {
            return;
        }
        let (Some(id), Some(entity)) = (uint(o, "disLibItemId"), o.child("Entity")) else { return };
        let mut model = None;
        entity.walk(&mut |c, _| {
            if model.is_none() {
                model = c.str_h(0xC230_E653).or_else(|| c.str_h(0x3182_766C)).filter(|m| m.to_ascii_lowercase().ends_with(".xbg"));
            }
        });
        out.insert(id, (entity.str("hidName").unwrap_or_default(), model.map(|m| m.to_ascii_lowercase())));
    });
    out
}

/// An entry's sway, and a view of a model with it.
fn view(e: &Object, model: &str, angles: [f32; 3]) -> View {
    let num = |n: &str| e.f32(n).filter(|v| v.is_finite()).unwrap_or(0.0);
    View { model: model.to_string(), angles, speed: num("fRotationSpeed"), amplitude: num("fRotationAmplitude") }
}

pub fn from_libs(shop_items: &Object, recipes: &Object, library: &Object) -> MenuViews {
    let protos = prototypes(library);
    let model_of = |e: &Object, field: &str| uint(e, field).filter(|&a| a != 0).and_then(|a| protos.get(&a)?.1.clone());
    let mut out = MenuViews::default();
    for e in &shop_items.children {
        let (Some(id), Some(model)) = (uint(e, "disId"), model_of(e, "arkDisplayArchetype")) else { continue };
        let angles = e.child("DefaultPositionOnScreen").and_then(|p| p.vec3("DisplayDefaultAngles")).unwrap_or([0.0; 3]);
        if id < u16::MAX as u64 {
            out.shop.insert(id as u32, view(e, &model, angles));
        }
        let category = e.child("CategoryPositionOnScreen").filter(|c| c.bool("bDisplayAsDefaultItem") == Some(true));
        if let (Some(c), Some(sub)) = (category, uint(e, "shpscataSubCategory1").filter(|&s| s < 255)) {
            out.categories.insert(sub as u32, view(e, &model, c.vec3("DisplayDefaultCategoryAngles").unwrap_or([0.0; 3])));
        }
    }
    for e in &recipes.children {
        let Some(model) = model_of(e, "arkMenuArchetype") else { continue };
        let Some((name, _)) = uint(e, "arkArchetype").and_then(|a| protos.get(&a)).filter(|(n, _)| !n.is_empty()) else { continue };
        out.recipes.insert(name.clone(), view(e, &model, [0.0; 3]));
    }
    out
}

impl MenuViews {
    pub fn to_text(&self) -> String {
        let line = |kind: &str, key: &str, v: &View| {
            let [x, y, z] = v.angles;
            format!("{kind}\t{key}\t{}\t{x},{y},{z}\t{}\t{}\n", v.model, v.speed, v.amplitude)
        };
        let mut shop: Vec<_> = self.shop.iter().collect();
        shop.sort_by_key(|(k, _)| **k);
        let mut categories: Vec<_> = self.categories.iter().collect();
        categories.sort_by_key(|(k, _)| **k);
        let mut recipes: Vec<_> = self.recipes.iter().collect();
        recipes.sort_by(|a, b| a.0.cmp(b.0));
        let mut s = format!("{MAGIC}\n");
        s += &shop.iter().map(|(k, v)| line("shop", &k.to_string(), v)).collect::<String>();
        s += &categories.iter().map(|(k, v)| line("category", &k.to_string(), v)).collect::<String>();
        s += &recipes.iter().map(|(k, v)| line("recipe", k, v)).collect::<String>();
        s
    }

    pub fn is_current(text: &str) -> bool {
        text.lines().next() == Some(MAGIC)
    }

    pub fn from_text(text: &str) -> Result<MenuViews> {
        let mut lines = text.lines();
        if lines.next() != Some(MAGIC) {
            bail!("not a menu views table");
        }
        let mut out = MenuViews::default();
        for l in lines {
            let f: Vec<&str> = l.split('\t').collect();
            let [kind, key, model, angles, speed, amplitude] = f[..] else { continue };
            let a: Vec<f32> = angles.split(',').filter_map(|x| x.parse().ok()).collect();
            let (Ok(speed), Ok(amplitude), &[x, y, z]) = (speed.parse(), amplitude.parse(), &a[..]) else { continue };
            let v = View { model: model.to_string(), angles: [x, y, z], speed, amplitude };
            match kind {
                "shop" => {
                    if let Ok(id) = key.parse() {
                        out.shop.insert(id, v);
                    }
                }
                "category" => {
                    if let Ok(id) = key.parse() {
                        out.categories.insert(id, v);
                    }
                }
                "recipe" => {
                    out.recipes.insert(key.to_string(), v);
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_go_through_text() {
        let mut v = MenuViews::default();
        v.shop.insert(15, View { model: r"graphics\weapons\kimber.xbg".into(), angles: [15.7, -4.7, 97.31], speed: 0.2, amplitude: 0.03 });
        v.categories.insert(148, View { model: r"graphics\weapons\kimber.xbg".into(), angles: [15.8, -2.0, 114.51], speed: 0.2, amplitude: 0.03 });
        v.recipes
            .insert("FC3_Interactive:Syringe".into(), View { model: r"graphics\props\syringe.xbg".into(), angles: [0.0; 3], speed: 0.08, amplitude: 0.05 });
        let text = v.to_text();
        assert!(MenuViews::is_current(&text));
        assert_eq!(MenuViews::from_text(&text).unwrap(), v);
    }
}
