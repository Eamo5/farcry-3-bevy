//! Shadows of distant land beyond the shadow cascades, from the game's
//! horizon maps (see `docs/terrain.md`). The terrain samples its sector's own
//! map; trees and props sample the world-wide atlas the converter joins from
//! them (`cache::HorizonAtlas`, 4 m texels). The test itself is the shader
//! module `fc3::horizon` (`horizon.wgsl`).

use std::sync::OnceLock;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

pub struct HorizonPlugin;

impl Plugin for HorizonPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "horizon.wgsl");
        app.add_systems(PreStartup, load_atlas);
    }
}

/// The world-wide horizon atlas (a neutral 1x1 image without one).
static ATLAS: OnceLock<Handle<Image>> = OnceLock::new();

/// Keeps the shader module loaded so other shaders can import it.
#[derive(Resource)]
#[allow(dead_code)]
struct HorizonShader(Handle<Shader>);

pub fn atlas() -> Handle<Image> {
    ATLAS.get().cloned().unwrap_or_default()
}

fn load_atlas(mut commands: Commands, server: Res<AssetServer>, mut images: ResMut<Assets<Image>>) {
    let shader = server.load(bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("horizon.wgsl")).with_source("embedded"));
    commands.insert_resource(HorizonShader(shader));
    let path = fc3_formats::cache::default_cache_root().join(fc3_formats::cache::HORIZON_ATLAS_REL);
    // FC3_NO_HORIZON=1 (debug): no shadows from distant land.
    let atlas = std::env::var("FC3_NO_HORIZON")
        .is_err()
        .then(|| std::fs::read(path).ok().and_then(|b| fc3_formats::cache::HorizonAtlas::from_bytes(&b)))
        .flatten();
    let (size, data) = match atlas {
        Some(a) => (a.size, a.data),
        None => (1, vec![128, 128]),
    };
    let mut image = Image::new(
        Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rg8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = crate::cache_assets::clamp_sampler();
    let _ = ATLAS.set(images.add(image));
}
