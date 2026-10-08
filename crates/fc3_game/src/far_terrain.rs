//! The land beyond the streamed terrain: one coarse mesh of the islands (a
//! vertex every 32 m, coloured from the baked ground atlas; see
//! `cache::FarTerrain`), so the far shores, ridges and the other island stay
//! in view. It is cut away wherever the streamed sectors lie
//! (`far_terrain.wgsl`), so the two never overlap.

use bevy::asset::RenderAssetUsages;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use fc3_formats::cache::{self, FarTerrain};
use fc3_formats::terrain::SectorCoord;

use crate::terrain::StreamingAnchor;

pub struct FarTerrainPlugin;

impl Plugin for FarTerrainPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "far_terrain.wgsl");
        app.add_plugins(MaterialPlugin::<FarMaterial>::default()).add_systems(Startup, spawn_far_terrain).add_systems(Update, follow_player);
    }
}

pub type FarMaterial = ExtendedMaterial<StandardMaterial, FarExt>;

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct FarExt {
    /// xy: the sector the streamed terrain centres on, z: its radius in
    /// sectors (cut away within), w: unused.
    #[uniform(100)]
    pub range: Vec4,
    /// The world's horizon atlas, for shadows of distant land (`horizon`).
    #[texture(101)]
    #[sampler(102)]
    pub horizon: Handle<Image>,
}

impl MaterialExtension for FarExt {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("far_terrain.wgsl")).with_source("embedded").into()
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }
}

#[derive(Resource)]
struct Far(Handle<FarMaterial>);

fn spawn_far_terrain(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<FarMaterial>>) {
    // FC3_NO_FAR=1 (debug): no far land.
    if std::env::var("FC3_NO_FAR").is_ok() {
        return;
    }
    let path = cache::default_cache_root().join(cache::FAR_TERRAIN_REL);
    let Some(far) = std::fs::read(path).ok().and_then(|b| FarTerrain::from_bytes(&b)) else { return };
    let n = far.size as usize;
    let half = n as f32 * FarTerrain::SPACING / 2.0;
    let at = |i: usize, j: usize| far.heights[j.min(n - 1) * n + i.min(n - 1)];
    let mut positions = Vec::with_capacity(n * n);
    let mut normals = Vec::with_capacity(n * n);
    let mut colours = Vec::with_capacity(n * n);
    for j in 0..n {
        for i in 0..n {
            let (x, y) = (i as f32 * FarTerrain::SPACING - half, j as f32 * FarTerrain::SPACING - half);
            positions.push([x, at(i, j), -y]);
            // Slopes from the neighbours (Dunia x east, y north = Bevy -z).
            let dx = (at(i + 1, j) - at(i.saturating_sub(1), j)) / (2.0 * FarTerrain::SPACING);
            let dy = (at(i, j + 1) - at(i, j.saturating_sub(1))) / (2.0 * FarTerrain::SPACING);
            normals.push(Vec3::new(-dx, 1.0, dy).normalize().to_array());
            let c = far.colours[j * n + i];
            colours.push(Color::srgb_u8(c[0], c[1], c[2]).to_linear().to_f32_array());
        }
    }
    // Only cells with land at a corner (the rest lies under the sea).
    let mut indices = Vec::new();
    for j in 0..n - 1 {
        for i in 0..n - 1 {
            let corners = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
            if corners.iter().all(|&(a, b)| at(a, b) <= FarTerrain::SEA_FLOOR) {
                continue;
            }
            let k = |(a, b): (usize, usize)| (b * n + a) as u32;
            let [a, b, c, d] = corners.map(k);
            // Wound to face up (Bevy's -z is north).
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colours);
    mesh.insert_indices(Indices::U32(indices));
    let material = materials.add(FarMaterial {
        base: StandardMaterial { base_color: Color::WHITE, perceptual_roughness: 0.95, reflectance: 0.2, ..default() },
        extension: FarExt { range: Vec4::new(0.0, 0.0, -1.0, 0.0), horizon: crate::horizon::atlas() },
    });
    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(material.clone()),
        Transform::IDENTITY,
        NotShadowCaster,
        NotShadowReceiver,
        Name::new("far terrain"),
    ));
    commands.insert_resource(Far(material));
}

/// Cuts the coarse land away where the terrain is streamed (the same
/// sectors `terrain` keeps round the anchor).
fn follow_player(
    far: Option<Res<Far>>,
    settings: Res<crate::settings::Settings>,
    anchor: Query<&GlobalTransform, With<StreamingAnchor>>,
    mut last: Local<Option<Vec4>>,
    mut materials: ResMut<Assets<FarMaterial>>,
) {
    let (Some(far), Ok(anchor)) = (far, anchor.single()) else { return };
    let p = anchor.translation();
    let c = SectorCoord::containing(p.x, -p.z);
    let range = Vec4::new(c.x as f32, c.y as f32, crate::terrain::terrain_radius(&settings) as f32, 0.0);
    if *last == Some(range) {
        return;
    }
    *last = Some(range);
    if let Some(mut m) = materials.get_mut(&far.0) {
        m.extension.range = range;
    }
}
