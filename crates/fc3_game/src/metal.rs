//! Metal materials with their mask: the game's "Metal" shader keeps metalness
//! and gloss in a mask texture that plain materials ignore, which leaves
//! guns looking like matt plastic lit by the sky. Parts marked
//! [`MaskedMetal`] get a material that reads the mask (see `metal.wgsl`).

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageLoaderSettings;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use fc3_formats::cache::CachedMaterial;

use crate::cache_assets::cache_path;
use crate::data::CacheInfo;

pub struct MetalPlugin;

impl Plugin for MetalPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "metal.wgsl");
        app.add_plugins(MaterialPlugin::<MetalMaterial>::default()).add_systems(Update, upgrade_parts);
    }
}

pub type MetalMaterial = ExtendedMaterial<StandardMaterial, MetalExt>;

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct MetalExt {
    #[texture(100)]
    #[sampler(101)]
    pub mask: Handle<Image>,
}

impl MaterialExtension for MetalExt {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("metal.wgsl")).with_source("embedded").into()
    }
}

/// A mesh part whose material should read its metal mask, if it has one.
#[derive(Component)]
pub struct MaskedMetal;

/// The original material's mask texture parameter.
const MASK_TEXTURE: u32 = 0x260A3CFE;

/// Swaps marked parts to a mask-reading material once their own material has
/// loaded; parts without a metal mask keep what they have.
fn upgrade_parts(
    info: Option<Res<CacheInfo>>,
    server: Res<AssetServer>,
    standard: Res<Assets<StandardMaterial>>,
    mut metals: ResMut<Assets<MetalMaterial>>,
    mut done: Local<HashMap<AssetId<StandardMaterial>, Option<Handle<MetalMaterial>>>>,
    parts: Query<(Entity, &MeshMaterial3d<StandardMaterial>), With<MaskedMetal>>,
    mut commands: Commands,
) {
    let Some(info) = info else { return };
    if std::env::var("FC3_NO_METAL").is_ok() {
        return;
    }
    for (e, mat) in &parts {
        let id = mat.0.id();
        if !done.contains_key(&id) {
            let Some(base) = standard.get(id) else { continue };
            let upgraded = server.get_path(id).and_then(|path| {
                let bytes = std::fs::read(info.root.join(path.path())).ok()?;
                let cm = CachedMaterial::from_bytes(&bytes).ok()?;
                if !cm.shader.to_ascii_lowercase().starts_with("metal") {
                    return None;
                }
                let (_, rel) = cm.textures.iter().find(|(h, _)| *h == MASK_TEXTURE)?;
                let rel = rel.clone();
                let mask = server
                    .load_builder()
                    .with_settings(|s: &mut ImageLoaderSettings| {
                        s.is_srgb = false;
                        s.sampler = crate::cache_assets::repeat_sampler();
                        s.asset_usage = RenderAssetUsages::RENDER_WORLD;
                    })
                    .load(cache_path(&rel));
                Some(metals.add(MetalMaterial { base: base.clone(), extension: MetalExt { mask } }))
            });
            done.insert(id, upgraded);
        }
        commands.entity(e).remove::<MaskedMetal>();
        if let Some(Some(m)) = done.get(&id) {
            commands.entity(e).remove::<MeshMaterial3d<StandardMaterial>>().insert(MeshMaterial3d(m.clone()));
        }
    }
}
