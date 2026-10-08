//! Converted-cache location and coordinate conventions.
//!
//! Dunia is right-handed Z-up; Bevy is right-handed Y-up. We map
//! Dunia (x, y, z) to Bevy (x, z, -y), so Dunia north (+Y) is Bevy -Z.

use std::collections::HashSet;
use std::path::PathBuf;

use bevy::prelude::*;
use fc3_formats::cache::Manifest;

/// What the converted cache contains (from its manifest).
#[derive(Resource, Clone, Default)]
pub struct CacheInfo {
    pub root: PathBuf,
    pub terrain: HashSet<u32>,
    pub objects: HashSet<u32>,
    pub vegetation: HashSet<u32>,
}

impl CacheInfo {
    pub fn load(root: PathBuf) -> anyhow::Result<CacheInfo> {
        let m = Manifest::load(&root)?;
        anyhow::ensure!(root.join(fc3_formats::shop::WEAPONS_REL).is_file(), "weapon shop table missing; re-run the converter");
        let ingredients = std::fs::read_to_string(root.join(fc3_formats::progression::RECIPES_REL))?;
        fc3_formats::progression::Crafting::from_text(&ingredients)?;
        Ok(CacheInfo {
            root,
            terrain: m.sectors_with_terrain.into_iter().collect(),
            objects: m.sectors_with_objects.into_iter().collect(),
            vegetation: m.sectors_with_vegetation.into_iter().collect(),
        })
    }
}

#[inline]
pub fn to_bevy(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[0], p[2], -p[1])
}

#[inline]
pub fn to_dunia(v: Vec3) -> [f32; 3] {
    [v.x, -v.z, v.y]
}
