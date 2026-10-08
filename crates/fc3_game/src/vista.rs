//! The forest far away: beyond the sectors whose trees are loaded as models
//! (`vegetation`), every tree stands as its billboard card, as in the game
//! (each tree resource's `*_vistabill.xbg`: a flat card cut from a sheet of
//! tree images, with rounded normals). The converter lists the world's trees
//! with their cards (`cache::VistaForest`); here they are merged into a few
//! meshes per 8x8-sector block and card sheet. The vertex shader turns each
//! card to face the eye and folds away the cards of sectors whose trees are
//! loaded, so the two meet without overlap (`vista.wgsl`).

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::camera::primitives::Aabb;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use fc3_formats::cache::{self, CachedModel, VistaForest};
use fc3_formats::terrain::SectorCoord;

use crate::cache_assets::cache_path;
use crate::terrain::StreamingAnchor;

pub struct VistaPlugin;

impl Plugin for VistaPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "vista.wgsl");
        app.add_plugins(MaterialPlugin::<VistaMaterial>::default())
            .add_systems(Startup, load_forest)
            .add_systems(Update, (build_forest, follow_player));
    }
}

pub type VistaMaterial = ExtendedMaterial<StandardMaterial, VistaExt>;

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct VistaExt {
    /// xy: the sector the loaded trees centre on, z: how many sectors round
    /// it have their trees loaded (their cards fold away), w: unused.
    #[uniform(100)]
    pub range: Vec4,
    /// The world's horizon atlas, for shadows of distant land (`horizon`).
    #[texture(101)]
    #[sampler(102)]
    pub horizon: Handle<Image>,
}

impl MaterialExtension for VistaExt {
    fn vertex_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("vista.wgsl")).with_source("embedded").into()
    }

    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("vista.wgsl")).with_source("embedded").into()
    }

    // Far away only: no depth prepass, no shadows (the cards turn with the eye).
    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }
}

/// Sectors per merged block side.
const BLOCK: i32 = 8;

/// One card: its geometry (Bevy-space card plane: x across, y up) and material.
struct Card {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    material: Handle<StandardMaterial>,
}

#[derive(Resource)]
struct Forest {
    trees: VistaForest,
    cards: Vec<Option<Card>>,
    built: bool,
    materials: Vec<Handle<VistaMaterial>>,
}

fn load_forest(mut commands: Commands, server: Res<AssetServer>) {
    // FC3_NO_VISTA=1 (debug): no far forest.
    if std::env::var("FC3_NO_VISTA").is_ok() {
        return;
    }
    let root = cache::default_cache_root();
    let Some(trees) = std::fs::read(root.join(cache::VISTA_REL)).ok().and_then(|b| VistaForest::from_bytes(&b).ok()) else {
        return;
    };
    let cards = trees
        .cards
        .iter()
        .map(|rel| {
            let model = CachedModel::from_bytes(&std::fs::read(root.join(rel)).ok()?).ok()?;
            let part = model.lods.first()?.parts.first()?;
            let material = model.materials.get(part.material as usize)?;
            Some(Card {
                positions: part.positions.clone(),
                normals: part.normals.clone(),
                uvs: part.uvs.clone(),
                indices: part.indices.clone(),
                material: server.load(cache_path(material)),
            })
        })
        .collect();
    info!("vista: {} trees on {} cards", trees.trees.len(), trees.cards.len());
    commands.insert_resource(Forest { trees, cards, built: false, materials: Vec::new() });
}

/// Builds the merged card meshes once the cards' materials have loaded.
fn build_forest(
    forest: Option<ResMut<Forest>>,
    standard: Res<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VistaMaterial>>,
    mut commands: Commands,
) {
    let Some(mut forest) = forest else { return };
    if forest.built || forest.cards.iter().flatten().any(|c| standard.get(&c.material).is_none()) {
        return;
    }
    forest.built = true;
    // One vista material per card sheet.
    let mut by_sheet: HashMap<AssetId<StandardMaterial>, Handle<VistaMaterial>> = HashMap::new();
    let horizon = crate::horizon::atlas();
    for card in forest.cards.iter().flatten() {
        by_sheet.entry(card.material.id()).or_insert_with(|| {
            let mut base = standard.get(&card.material).cloned().unwrap_or_default();
            base.cull_mode = None;
            base.double_sided = true;
            if !matches!(base.alpha_mode, AlphaMode::Mask(_)) {
                base.alpha_mode = AlphaMode::Mask(0.5);
            }
            materials.add(VistaMaterial { base, extension: VistaExt { range: Vec4::new(0.0, 0.0, -1.0, 0.0), horizon: horizon.clone() } })
        });
    }
    // Trees grouped by block and sheet.
    #[derive(Default)]
    struct Group {
        positions: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
        uvs: Vec<[f32; 2]>,
        corners: Vec<[f32; 2]>,
        indices: Vec<u32>,
        min: Vec3,
        max: Vec3,
    }
    let mut groups: HashMap<((i32, i32), AssetId<StandardMaterial>), Group> = HashMap::new();
    for (k, t) in forest.trees.trees.iter().enumerate() {
        let Some(Some(card)) = forest.cards.get(t.card as usize) else { continue };
        let c = SectorCoord::containing(t.pos[0], -t.pos[2]);
        let g = groups.entry(((c.x.div_euclid(BLOCK), c.y.div_euclid(BLOCK)), card.material.id())).or_insert_with(|| Group {
            min: Vec3::splat(f32::MAX),
            max: Vec3::splat(f32::MIN),
            ..default()
        });
        let base = g.positions.len() as u32;
        // Every other tree mirrored, for variety.
        let flip = if k % 2 == 0 { 1.0 } else { -1.0 };
        let reach = card.positions.iter().map(|p| p[0].abs().max(p[1].abs())).fold(0.0f32, f32::max) * t.scale;
        for (i, p) in card.positions.iter().enumerate() {
            g.positions.push(t.pos);
            g.corners.push([p[0] * t.scale * flip, p[1] * t.scale]);
            let n = card.normals.get(i).copied().unwrap_or([0.0, 0.0, 1.0]);
            g.normals.push([n[0] * flip, n[1], n[2]]);
            g.uvs.push(card.uvs.get(i).copied().unwrap_or_default());
        }
        g.indices.extend(card.indices.iter().map(|i| base + i));
        let at = Vec3::from_array(t.pos);
        g.min = g.min.min(at - Vec3::splat(reach));
        g.max = g.max.max(at + Vec3::splat(reach));
    }
    let n = groups.len();
    for ((_, sheet), g) in groups {
        let Some(material) = by_sheet.get(&sheet) else { continue };
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, g.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, g.normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, g.uvs);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, g.corners);
        mesh.insert_indices(Indices::U32(g.indices));
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material.clone()),
            Transform::IDENTITY,
            // The positions are the trees' bases; the cards reach round them.
            Aabb::from_min_max(g.min, g.max),
            NotShadowCaster,
            NotShadowReceiver,
            Name::new("vista forest"),
        ));
    }
    forest.materials = by_sheet.into_values().collect();
    info!("vista: {n} card meshes");
}

/// Folds away the cards where the trees are loaded as models (the same
/// sectors `vegetation` streams round the anchor).
fn follow_player(
    forest: Option<Res<Forest>>,
    settings: Res<crate::settings::Settings>,
    anchor: Query<&GlobalTransform, With<StreamingAnchor>>,
    mut last: Local<Option<Vec4>>,
    mut materials: ResMut<Assets<VistaMaterial>>,
) {
    let (Some(forest), Ok(anchor)) = (forest, anchor.single()) else { return };
    let p = anchor.translation();
    let c = SectorCoord::containing(p.x, -p.z);
    let range = Vec4::new(c.x as f32, c.y as f32, crate::vegetation::far_radius(&settings) as f32, 0.0);
    if *last == Some(range) && !forest.is_changed() {
        return;
    }
    *last = Some(range);
    for h in &forest.materials {
        if let Some(mut m) = materials.get_mut(h) {
            m.extension.range = range;
        }
    }
}
