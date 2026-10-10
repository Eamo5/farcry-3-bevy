//! Bevy asset loaders for the converted cache (`fc3://...`).
//!
//! Each `.fcm`/`.fct`/`.fcs` file loads as one asset whose meshes, materials
//! and textures are labeled sub-assets or tracked dependencies, so
//! `AssetServer::is_loaded_with_dependencies` tells streaming code when a
//! whole model or sector is ready to show.

use std::sync::Arc;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, RenderAssetUsages};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::Face;
use fc3_formats::cache::{
    self, mat_flags, CachedBone, CachedEntity, CachedMaterial, CachedModel, CachedPart, CachedSector, CachedTerrain, Placement, TerrainLayerTable,
};
use fc3_formats::terrain::{SectorCoord, TerrainSector, GRID, SECTOR_SIZE};
use serde::{Deserialize, Serialize};

/// Asset source name for the converted cache.
pub const SOURCE: &str = "fc3";
/// Same cache folder, used for skinned character models so they never share
/// an asset (and its skinned vertex layout) with static props.
pub const SKINNED_SOURCE: &str = "fc3skin";

pub fn cache_path(rel: &str) -> String {
    format!("{SOURCE}://{rel}")
}

pub fn skinned_cache_path(rel: &str) -> String {
    format!("{SKINNED_SOURCE}://{rel}")
}

/// True once an asset and its dependency tree have finished loading, whether
/// or not every dependency succeeded (missing pieces are simply skipped).
pub fn settled(server: &AssetServer, id: impl Into<bevy::asset::UntypedAssetId>) -> bool {
    use bevy::asset::RecursiveDependencyLoadState as S;
    matches!(server.recursive_dependency_load_state(id), S::Loaded | S::Failed(_))
}

/// The asset itself is done loading (or failed), whatever its dependencies.
pub fn done(server: &AssetServer, id: impl Into<bevy::asset::UntypedAssetId>) -> bool {
    matches!(server.load_state(id.into()), bevy::asset::LoadState::Loaded | bevy::asset::LoadState::Failed(_))
}

/// Seconds streaming waits for an asset's dependencies once the asset itself
/// is in, before showing it anyway.
const DEPENDENCY_GRACE: f32 = 4.0;

/// Whether streamed content can be shown: its dependencies (textures) have
/// settled, or the asset itself has been in for a few seconds. A deep
/// dependency can sit in `Loading` indefinitely, which used to hold whole
/// sectors back; `since` remembers when the asset came in.
pub fn ready(server: &AssetServer, id: impl Into<bevy::asset::UntypedAssetId>, since: &mut Option<f32>, now: f32) -> bool {
    let id = id.into();
    if !done(server, id) {
        return false;
    }
    let first = *since.get_or_insert(now);
    settled(server, id) || now - first >= DEPENDENCY_GRACE
}

/// Whether an asset can be used: loaded with its dependencies, or itself
/// loaded for a few seconds with some dependency (a texture) still loading.
/// Like `ready`, for callers without a place to keep the time: one stuck
/// texture load held vehicles, animals or arms back for good.
pub fn usable(server: &AssetServer, id: impl Into<bevy::asset::UntypedAssetId>) -> bool {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Instant;
    static FIRST_DONE: Mutex<Option<HashMap<bevy::asset::UntypedAssetId, Instant>>> = Mutex::new(None);
    let id = id.into();
    if server.is_loaded_with_dependencies(id) {
        return true;
    }
    if !done(server, id) {
        return false;
    }
    let mut map = FIRST_DONE.lock().unwrap();
    let first = *map.get_or_insert_with(HashMap::new).entry(id).or_insert_with(Instant::now);
    let stale = first.elapsed().as_secs_f32() >= DEPENDENCY_GRACE;
    if stale && first.elapsed().as_secs_f32() < DEPENDENCY_GRACE + 0.1 {
        debug!("{:?}: dependencies still loading after {DEPENDENCY_GRACE} s; using it anyway", server.get_path(id));
    }
    stale
}

pub struct CacheAssetsPlugin;

impl Plugin for CacheAssetsPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<Fc3Model>()
            .init_asset::<Fc3Terrain>()
            .init_asset::<Fc3Sector>()
            .init_asset::<Fc3Skeleton>()
            .init_asset::<Fc3Clip>()
            .register_asset_loader(SkeletonLoader)
            .register_asset_loader(ClipLoader)
            .register_asset_loader(MaterialLoader)
            .register_asset_loader(ModelLoader)
            .register_asset_loader(TerrainLoader)
            .register_asset_loader(SectorLoader)
            .add_plugins(MaterialPlugin::<TerrainMaterial>::default())
            .add_systems(Update, flow_materials);
        bevy::asset::embedded_asset!(app, "terrain_detail.wgsl");
    }
}

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

async fn read_all(reader: &mut dyn Reader) -> Result<Vec<u8>, std::io::Error> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

pub(crate) fn repeat_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    })
}

pub(crate) fn clamp_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    })
}

pub(crate) fn load_texture(ctx: &mut LoadContext, rel: &str, srgb: bool, clamp: bool) -> Option<Handle<Image>> {
    if rel.is_empty() {
        return None;
    }
    Some(
        ctx.load_builder()
            .with_settings(move |s: &mut ImageLoaderSettings| {
                s.is_srgb = srgb;
                s.sampler = if clamp { clamp_sampler() } else { repeat_sampler() };
                s.asset_usage = RenderAssetUsages::RENDER_WORLD;
            })
            .load(cache_path(rel)),
    )
}

// ----------------------------------------------------------------- model ---

#[derive(Clone)]
pub struct ModelPart {
    pub mesh: Handle<Mesh>,
    /// The model's own material for this part's slot.
    pub material: Handle<StandardMaterial>,
    /// Material slot index, for per-placement material sets.
    pub slot: u32,
    pub flags: u32,
    /// A part that turns about its own node (vehicle wheels).
    pub pivot: Option<fc3_formats::cache::PartPivot>,
}

impl ModelPart {
    /// Whether a weapon part shows when used this way: some guns carry a
    /// mounted and a handheld set of pieces (spade grips or a stock).
    pub fn shown(&self, mounted: bool) -> bool {
        match self.pivot.as_ref().map(|p| p.name.as_str()) {
            Some("PIECES_MOUNTED") => mounted,
            Some("PIECES_HANDHELD") => !mounted,
            _ => true,
        }
    }
}

#[derive(Clone)]
pub struct ModelLod {
    pub distance: f32,
    pub parts: Vec<ModelPart>,
}

/// Triangle soup for physics, in model space.
pub struct CollisionMesh {
    pub positions: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
    /// From the game's own collision (`cache::collision_rel`) rather than
    /// the render mesh.
    pub from_havok: bool,
}

/// Whether the model whose cache file is named by `hash` has collision of
/// the game's own (the converter's index, read once). `FC3_RENDER_COLLISION`
/// ignores it, for comparing with the render-derived meshes.
fn has_havok_collision(hash: u64) -> bool {
    static INDEX: std::sync::LazyLock<std::collections::HashSet<u64>> = std::sync::LazyLock::new(|| {
        if std::env::var_os("FC3_RENDER_COLLISION").is_some() {
            info!("FC3_RENDER_COLLISION: colliders from the render meshes only");
            return Default::default();
        }
        let path = cache::default_cache_root().join(cache::COLLISION_INDEX_REL);
        let index = std::fs::read_to_string(&path).map(|t| cache::collision_index(&t)).unwrap_or_default();
        info!("{} models with the game's own collision", index.len());
        index
    });
    INDEX.contains(&hash)
}

#[derive(Asset, TypePath)]
pub struct Fc3Model {
    pub lods: Vec<ModelLod>,
    pub aabb_min: Vec3,
    pub aabb_max: Vec3,
    pub collision: Option<Arc<CollisionMesh>>,
    pub bones: Vec<CachedBone>,
    /// True when every LOD-0 part is foliage (no collision, cheaper shadows).
    pub is_vegetation: bool,
    /// Present when loaded with `ModelSettings { skinned: true }`: one inverse bind per bone.
    pub inverse_bindposes: Option<Handle<SkinnedMeshInverseBindposes>>,
}

/// Loader settings for `.fcm`. Characters opt into skinning so world props
/// don't pay for joint attributes they never use.
#[derive(Serialize, Deserialize, Default, Clone, Copy)]
pub struct ModelSettings {
    pub skinned: bool,
}

impl Fc3Model {
    pub fn radius(&self) -> f32 {
        ((self.aabb_max - self.aabb_min) * 0.5).length()
    }

    pub fn center(&self) -> Vec3 {
        (self.aabb_max + self.aabb_min) * 0.5
    }
}

#[derive(Default, TypePath)]
pub struct ModelLoader;

impl AssetLoader for ModelLoader {
    type Asset = Fc3Model;
    type Settings = ModelSettings;
    type Error = std::io::Error;

    async fn load(&self, reader: &mut dyn Reader, settings: &ModelSettings, ctx: &mut LoadContext<'_>) -> Result<Fc3Model, Self::Error> {
        let bytes = read_all(reader).await?;
        let m = CachedModel::from_bytes(&bytes).map_err(io_err)?;
        // Materials are shared files, so every model using one gets the same handle.
        let materials: Vec<Handle<StandardMaterial>> = m.materials.iter().map(|rel| ctx.load(cache_path(rel))).collect();
        let mut lods = Vec::with_capacity(m.lods.len());
        let mut bytes = 0;
        for (li, lod) in m.lods.iter().enumerate() {
            let mut parts = Vec::with_capacity(lod.parts.len());
            for (pi, p) in lod.parts.iter().enumerate() {
                let flags = m.material_flags.get(p.material as usize).copied().unwrap_or(0);
                let mesh = build_mesh(p, settings.skinned);
                bytes += mesh.get_vertex_buffer_size() + mesh.get_index_buffer_bytes().map_or(0, |b| b.len());
                let mesh = ctx.add_labeled_asset(format!("lod{li}_part{pi}"), mesh);
                let material = materials.get(p.material as usize).cloned().unwrap_or_default();
                parts.push(ModelPart { mesh, material, slot: p.material, flags, pivot: p.pivot.clone() });
            }
            lods.push(ModelLod { distance: lod.distance, parts });
        }
        let is_vegetation = lods.first().is_some_and(|l| !l.parts.is_empty() && l.parts.iter().all(|p| p.flags & mat_flags::VEGETATION != 0));
        // The game's own collision where the model ships one (static props
        // only: characters don't use it); else the render-derived mesh.
        let hash = ctx.path().path().file_stem().and_then(|s| s.to_str()).and_then(|s| u64::from_str_radix(s, 16).ok());
        let havok = match hash.filter(|&h| !settings.skinned && has_havok_collision(h)) {
            Some(h) => match ctx
                .read_asset_bytes(cache_path(&cache::collision_rel_of_hash(h)))
                .await
                .map_err(io_err)
                .and_then(|b| cache::CachedCollision::from_bytes(&b).map_err(io_err))
            {
                // Made only of the no-impact material (plant pickups, kelp,
                // volumes) where the render mesh has nothing solid: the file
                // doesn't say these block characters (their shapes look like
                // targets to aim at), so they stay walk-through.
                Ok(c) => Some(c).filter(|c| !c.indices.is_empty() && !(c.no_impact && m.collision_indices.is_empty())),
                Err(e) => {
                    warn!("{}: its collision didn't load ({e}); using the render mesh", ctx.path());
                    None
                }
            },
            None => None,
        };
        let collision = match havok {
            Some(c) => Some(Arc::new(CollisionMesh {
                positions: c.positions.iter().map(|p| Vec3::from_array(*p)).collect(),
                triangles: c.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect(),
                from_havok: true,
            })),
            None => (!m.collision_indices.is_empty()).then(|| {
                Arc::new(CollisionMesh {
                    positions: m.collision_positions.iter().map(|p| Vec3::from_array(*p)).collect(),
                    triangles: m.collision_indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect(),
                    from_havok: false,
                })
            }),
        };
        let inverse_bindposes = (settings.skinned && !m.bones.is_empty()).then(|| {
            let mats: Vec<Mat4> = m.bones.iter().map(|b| Mat4::from_cols_array(&b.inverse_bind)).collect();
            ctx.add_labeled_asset("inverse_bindposes".to_string(), SkinnedMeshInverseBindposes::from(mats))
        });
        // Its meshes go to the GPU as it comes in; wait for room this frame.
        crate::upload_budget::take(bytes).await;
        Ok(Fc3Model {
            lods,
            aabb_min: Vec3::from_array(m.aabb_min),
            aabb_max: Vec3::from_array(m.aabb_max),
            collision,
            bones: m.bones,
            is_vegetation,
            inverse_bindposes,
        })
    }

    fn extensions(&self) -> &[&str] {
        &["fcm"]
    }
}

#[derive(Default, TypePath)]
pub struct MaterialLoader;

impl AssetLoader for MaterialLoader {
    type Asset = StandardMaterial;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(&self, reader: &mut dyn Reader, _: &(), ctx: &mut LoadContext<'_>) -> Result<StandardMaterial, Self::Error> {
        let bytes = read_all(reader).await?;
        let cm = CachedMaterial::from_bytes(&bytes).map_err(io_err)?;
        if let Some(flow) = Flow::of(&cm) {
            FLOWING.lock().unwrap().insert(ctx.path().to_string(), flow);
        }
        Ok(build_material(ctx, &cm))
    }

    fn extensions(&self) -> &[&str] {
        &["fcmat"]
    }
}

/// A lit effect's first layer: its tiling and how fast it scrolls (UV units a
/// second), from the material's `(tile u, tile v, speed u, speed v)`.
#[derive(Clone, Copy, Debug)]
pub struct Flow {
    pub tile: Vec2,
    pub speed: Vec2,
}

impl Flow {
    const LAYER: u32 = 0x23BE_852F;

    fn of(cm: &CachedMaterial) -> Option<Flow> {
        if cm.shader != "EffectLit" {
            return None;
        }
        let (_, raw) = cm.params.iter().find(|(h, _)| *h == Self::LAYER)?;
        let f = fc3_formats::original::param_floats(raw);
        (f.len() >= 4).then(|| Flow { tile: Vec2::new(f[0], f[1]), speed: Vec2::new(f[2], f[3]) })
    }
}

/// Flowing materials by asset path, noted as they load.
pub static FLOWING: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Flow>>> = std::sync::LazyLock::new(Default::default);

/// Scrolls the lit effects' maps (waterfalls run, foam drifts).
pub fn flow_materials(
    time: Res<Time>,
    server: Res<AssetServer>,
    mut events: MessageReader<AssetEvent<StandardMaterial>>,
    mut flowing: Local<Vec<(AssetId<StandardMaterial>, Flow)>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for ev in events.read() {
        if let AssetEvent::LoadedWithDependencies { id } | AssetEvent::Added { id } = ev {
            let flow = server.get_path(*id).and_then(|p| FLOWING.lock().unwrap().get(&p.to_string()).copied());
            if let Some(flow) = flow.filter(|_| !flowing.iter().any(|(f, _)| f == id)) {
                flowing.push((*id, flow));
                debug!("flowing material {id:?}: {flow:?} ({} now)", flowing.len());
            }
        }
    }
    let t = time.elapsed_secs();
    flowing.retain(|(id, flow)| {
        let Some(mut m) = materials.get_mut(*id) else { return false };
        let offset = (flow.speed * t).fract_gl();
        m.uv_transform = bevy::math::Affine2::from_scale_angle_translation(flow.tile, 0.0, offset);
        true
    });
}

fn unlit_blend(shader: &str, params: &[(u32, Vec<u8>)]) -> Option<AlphaMode> {
    if shader != "Unlit" { return None; }
    let bytes = &params.iter().find(|(id, _)| *id == fc3_formats::hash::name_hash("BlendingType"))?.1;
    // Installed engine/shaders/materialdescriptors/unlit.xml defines these
    // values. Screen, multiply-add and ZMask need dedicated implementations.
    match u32::from_le_bytes(bytes.as_slice().try_into().ok()?) {
        0 => Some(AlphaMode::Opaque),
        1 => Some(AlphaMode::Blend),
        2 => Some(AlphaMode::Add),
        4 => Some(AlphaMode::Premultiplied),
        _ => None,
    }
}

fn build_material(ctx: &mut LoadContext, cm: &CachedMaterial) -> StandardMaterial {
    if cm.flags & mat_flags::PLACEHOLDER != 0 {
        return StandardMaterial { base_color: Color::srgb(cm.base_color[0], cm.base_color[1], cm.base_color[2]), perceptual_roughness: 0.9, ..default() };
    }
    let diffuse = load_texture(ctx, &cm.diffuse, true, false);
    // Lit effects add their light (foam on a waterfall, spray): their maps
    // carry no alpha.
    let alpha_mode = if let Some(mode) = unlit_blend(&cm.shader, &cm.params).filter(|_| cm.flags & mat_flags::ALPHA_TEST == 0) {
        mode
    } else if cm.shader == "EffectLit" && cm.flags & mat_flags::ALPHA_TEST == 0 {
        AlphaMode::Add
    } else if cm.flags & mat_flags::ALPHA_TEST != 0 {
        AlphaMode::Mask(0.4)
    } else if cm.flags & (mat_flags::ALPHA_BLEND | mat_flags::WATER) != 0 {
        AlphaMode::Blend
    } else {
        AlphaMode::Opaque
    };
    let double_sided = cm.flags & mat_flags::DOUBLE_SIDED != 0;
    // Generic, hair and blended materials dye their map with `DiffuseColor1`
    // and `Simple` ones with `DiffuseColor` (a white map made brown, a grey
    // one dark; some brighten up to twice); metal ones darken by it (see
    // `single_dye`).
    let dye = fc3_formats::material::single_dye(&cm.shader, &cm.params).unwrap_or([1.0; 3]);
    // The colour map's tiling, as the original shaders apply it (left as it
    // is for maps on the second UV set, which not every mesh carries).
    let (tiling, uv_set) = fc3_formats::material::diffuse_mapping(&cm.params);
    let uv_transform = if uv_set == 0 && tiling != [1.0, 1.0] { bevy::math::Affine2::from_scale(Vec2::from(tiling)) } else { bevy::math::Affine2::IDENTITY };
    StandardMaterial {
        uv_transform,
        base_color: if diffuse.is_some() {
            Color::srgba(cm.base_color[0] * dye[0], cm.base_color[1] * dye[1], cm.base_color[2] * dye[2], cm.base_color[3])
        } else {
            Color::srgb(0.55, 0.55, 0.55)
        },
        base_color_texture: diffuse,
        // (Not the material's own gloss, `original::SURFACE_REGISTER`: the
        // standard material reflects all of the sky, where the game's ambient
        // specular is a fraction, so glossy leaves came out silvered.)
        perceptual_roughness: cm.roughness,
        reflectance: 0.35,
        // Light through leaves, for vegetation (`original::VEGETATION_SSS_REGISTER`;
        // the foliage material takes it from here, see `vegetation.rs`).
        diffuse_transmission: cm
            .original
            .as_ref()
            .and_then(|o| o.ps_constants.iter().find(|(r, _)| *r as usize == fc3_formats::original::VEGETATION_SSS_REGISTER))
            .map_or(0.0, |(_, v)| v[0]),
        alpha_mode,
        double_sided,
        cull_mode: if double_sided { None } else { Some(Face::Back) },
        unlit: cm.shader == "Unlit" || cm.flags & mat_flags::EMISSIVE != 0,
        // Overlays (dirt, stains, decals) lie on their surface: drawn a hair
        // nearer, so they win against it even when it's drawn by the original
        // shaders, whose positions differ in the last bits.
        depth_bias: if cm.flags & mat_flags::ALPHA_BLEND != 0 { DECAL_DEPTH_BIAS } else { 0.0 },
        ..default()
    }
}

/// Constant depth bias for overlays (Depth32 units: about 2 mm at 6 m).
pub(crate) const DECAL_DEPTH_BIAS: f32 = 4000.0;

fn build_mesh(p: &CachedPart, skinned: bool) -> Mesh {
    let n = p.positions.len();
    let usages = if std::env::var_os("FC3_VIEW_MESH_DEBUG").is_some() {
        RenderAssetUsages::default()
    } else { RenderAssetUsages::RENDER_WORLD };
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, usages);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, p.positions.clone());
    if p.uvs.len() == n {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, p.uvs.clone());
    } else {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32, 0.0]; n]);
    }
    // The second UV set (masks and dirt mapped uniquely over a tiling first
    // set), for the original shaders.
    if p.uvs1.len() == n {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, p.uvs1.clone());
    }
    mesh.insert_indices(Indices::U32(p.indices.clone()));
    if p.normals.len() == n {
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, p.normals.clone());
    } else {
        mesh.compute_normals();
    }
    // The tangent frame (the original shaders read it for normal maps).
    if p.tangents.len() == n {
        mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, p.tangents.clone());
    }
    if skinned && p.joints.len() == n && p.weights.len() == n {
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, bevy::mesh::VertexAttributeValues::Uint16x4(p.joints.clone()));
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, p.weights.clone());
    }
    // The original vertex colours (blend weights and baked occlusion for the
    // game's own shaders); a custom attribute so standard materials ignore it.
    let colors: Vec<[u8; 4]> =
        if p.colors.len() == n { p.colors.iter().map(|c| c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)).collect() } else { vec![[255; 4]; n] };
    mesh.insert_attribute(ATTRIBUTE_D3D_COLOR, bevy::mesh::VertexAttributeValues::Unorm8x4(colors));
    mesh
}

/// Original per-vertex colour, read by the translated original shaders.
pub const ATTRIBUTE_D3D_COLOR: bevy::mesh::MeshVertexAttribute =
    bevy::mesh::MeshVertexAttribute::new("D3dColor", 0x0fc3_c010, bevy::render::render_resource::VertexFormat::Unorm8x4);

// ------------------------------------------------------ skeleton / clips ---

/// A parsed `.skeleton` (bone names, parents, procedural helper rules).
#[derive(Asset, TypePath)]
pub struct Fc3Skeleton(pub Arc<fc3_formats::anim::Skeleton>);

/// A parsed `.mab` animation clip.
#[derive(Asset, TypePath)]
pub struct Fc3Clip(pub Arc<fc3_formats::anim::Clip>);

#[derive(Default, TypePath)]
pub struct SkeletonLoader;

impl AssetLoader for SkeletonLoader {
    type Asset = Fc3Skeleton;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(&self, reader: &mut dyn Reader, _: &(), _ctx: &mut LoadContext<'_>) -> Result<Fc3Skeleton, Self::Error> {
        let bytes = read_all(reader).await?;
        Ok(Fc3Skeleton(Arc::new(fc3_formats::anim::parse_skeleton(&bytes).map_err(io_err)?)))
    }

    fn extensions(&self) -> &[&str] {
        &["skeleton"]
    }
}

#[derive(Default, TypePath)]
pub struct ClipLoader;

impl AssetLoader for ClipLoader {
    type Asset = Fc3Clip;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(&self, reader: &mut dyn Reader, _: &(), ctx: &mut LoadContext<'_>) -> Result<Fc3Clip, Self::Error> {
        let bytes = read_all(reader).await?;
        // A clip kept in segments: the list of them, joined here.
        if let Some(list) = bytes.strip_prefix(fc3_formats::anim::SEGMENTS_MAGIC.as_bytes()) {
            let mut segments = Vec::new();
            for rel in String::from_utf8_lossy(list).lines().map(str::trim).filter(|l| !l.is_empty()) {
                let seg = ctx.read_asset_bytes(cache_path(rel)).await.map_err(io_err)?;
                segments.push(fc3_formats::anim::parse_clip(&seg).map_err(io_err)?);
            }
            let clip = fc3_formats::anim::stitch(segments).ok_or_else(|| io_err("no segments"))?;
            return Ok(Fc3Clip(Arc::new(clip)));
        }
        Ok(Fc3Clip(Arc::new(fc3_formats::anim::parse_clip(&bytes).map_err(io_err)?)))
    }

    fn extensions(&self) -> &[&str] {
        &["mab"]
    }
}

// --------------------------------------------------------------- terrain ---

pub const TERRAIN_LODS: usize = 3;

/// The terrain's atlas material, with procedural close-range detail
/// (`terrain_detail.wgsl`).
pub type TerrainMaterial = bevy::pbr::ExtendedMaterial<StandardMaterial, TerrainDetail>;

/// Per-sector terrain layer data for `terrain_detail.wgsl`: up to four tiling
/// ground textures (with normal maps) blended by the atlas mask.
///
/// Bindless when the platform supports it, so all terrain chunks batch; the
/// index table (binding 150) covers bindless indices 100..=113.
#[derive(Asset, bevy::render::render_resource::AsBindGroup, TypePath, Debug, Clone, Default)]
#[data(100, TerrainParams, binding_array(151))]
#[bindless(index_table(range(100..114), binding(150)))]
pub struct TerrainDetail {
    pub params: TerrainParams,
    #[texture(101)]
    #[sampler(102)]
    pub mask: Option<Handle<Image>>,
    #[texture(103)]
    pub color: Option<Handle<Image>>,
    #[texture(104)]
    #[sampler(105)]
    pub diffuse0: Option<Handle<Image>>,
    #[texture(106)]
    pub diffuse1: Option<Handle<Image>>,
    #[texture(107)]
    pub diffuse2: Option<Handle<Image>>,
    #[texture(108)]
    pub diffuse3: Option<Handle<Image>>,
    #[texture(109)]
    pub normal0: Option<Handle<Image>>,
    #[texture(110)]
    pub normal1: Option<Handle<Image>>,
    #[texture(111)]
    pub normal2: Option<Handle<Image>>,
    #[texture(112)]
    pub normal3: Option<Handle<Image>>,
    /// The sector's horizon map (see `horizon_image`).
    #[texture(113)]
    pub horizon: Option<Handle<Image>>,
}

#[derive(Clone, Copy, Debug, Default, bevy::render::render_resource::ShaderType)]
pub struct TerrainParams {
    /// Per slot: texture repeats per metre.
    pub scale: Vec4,
    /// Per slot: projection (0 = along X, 1 = along Z, 2 = top-down, 3 = triplanar), -1 if unused.
    pub mode: Vec4,
    /// Per slot: 1 when the slot has a normal map.
    pub has_normal: Vec4,
    /// x: 1 when layer textures are bound, y: 1 when the horizon map is.
    pub flags: Vec4,
}

impl From<&TerrainDetail> for TerrainParams {
    fn from(d: &TerrainDetail) -> TerrainParams {
        d.params
    }
}

impl TerrainDetail {
    /// Builds the layer bindings for one sector from the world's layer table.
    fn for_sector(ctx: &mut LoadContext, table: &TerrainLayerTable, layers: [u8; 4], atlas: u32) -> TerrainDetail {
        let mut d = TerrainDetail { params: TerrainParams { mode: Vec4::splat(-1.0), ..default() }, ..default() };
        let mut any = false;
        for (slot, &index) in layers.iter().enumerate() {
            let Some(layer) = table.layers.get(index as usize) else { continue };
            let Some(diffuse) = load_texture(ctx, &layer.diffuse, true, false) else { continue };
            let normal = load_texture(ctx, &layer.normal, false, false);
            d.params.scale[slot] = layer.tiling / SECTOR_SIZE;
            // Dunia axes: X stays X, Y (north) is Bevy -Z, Z (up) is Bevy Y.
            d.params.mode[slot] = if layer.name.contains("Cliff") { 3.0 } else { layer.axis.min(2) as f32 };
            d.params.has_normal[slot] = if normal.is_some() { 1.0 } else { 0.0 };
            let (dst, nst) = match slot {
                0 => (&mut d.diffuse0, &mut d.normal0),
                1 => (&mut d.diffuse1, &mut d.normal1),
                2 => (&mut d.diffuse2, &mut d.normal2),
                _ => (&mut d.diffuse3, &mut d.normal3),
            };
            *dst = Some(diffuse);
            *nst = normal;
            any = true;
        }
        if any {
            d.mask = load_texture(ctx, &cache::atlas_rel(atlas, "mask"), false, true);
            d.color = load_texture(ctx, &cache::atlas_rel(atlas, "color"), false, true);
            if d.mask.is_some() && d.color.is_some() {
                d.params.flags.x = 1.0;
            }
        }
        d
    }
}

impl bevy::pbr::MaterialExtension for TerrainDetail {
    fn fragment_shader() -> bevy::shader::ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("terrain_detail.wgsl")).with_source("embedded").into()
    }
}

#[derive(Asset, TypePath)]
pub struct Fc3Terrain {
    pub sector: Arc<TerrainSector>,
    /// The coarsest mesh (4 m grid), all a sector at the edge of view needs;
    /// the finer ones are built as the viewer nears (`terrain::spawn_terrain`).
    pub coarse: Handle<Mesh>,
    pub material: Handle<TerrainMaterial>,
    /// The same layers on the game's own terrain shaders (when available).
    pub original: Option<Handle<crate::original_shaders::OriginalMaterial>>,
}

#[derive(Default, TypePath)]
pub struct TerrainLoader;

impl AssetLoader for TerrainLoader {
    type Asset = Fc3Terrain;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(&self, reader: &mut dyn Reader, _: &(), ctx: &mut LoadContext<'_>) -> Result<Fc3Terrain, Self::Error> {
        let bytes = read_all(reader).await?;
        let t = CachedTerrain::from_bytes(&bytes).map_err(io_err)?;
        let table = match ctx.read_asset_bytes(cache_path(cache::TERRAIN_LAYERS_REL)).await {
            Ok(b) => TerrainLayerTable::from_text(&String::from_utf8_lossy(&b)),
            Err(_) => TerrainLayerTable::default(),
        };
        let sector = Arc::new(t.to_sector());
        let c = SectorCoord::from_id(t.sector_id);
        let coarse = ctx.add_labeled_asset(format!("lod{}", TERRAIN_LODS - 1), terrain_mesh(&sector, c, 1 << (TERRAIN_LODS - 1)));
        let atlas = c.atlas_coord().id();
        let diffuse = load_texture(ctx, &cache::atlas_rel(atlas, "diffuse"), true, true);
        let mut detail = TerrainDetail::for_sector(ctx, &table, t.layers, atlas);
        // FC3_NO_HORIZON=1 (debug): no shadows from distant land.
        let horizon = if std::env::var("FC3_NO_HORIZON").is_ok() { None } else { ctx.read_asset_bytes(cache_path(&cache::shadow_rel(t.sector_id))).await.ok() };
        if let Some(image) = horizon.and_then(|b| horizon_image(&b)) {
            detail.horizon = Some(ctx.add_labeled_asset("horizon".to_string(), image));
            detail.params.flags.y = 1.0;
        }
        let quadrant = Vec2::new((c.x - c.atlas_coord().x) as f32, (c.y - c.atlas_coord().y) as f32);
        let original =
            crate::original_shaders::terrain_material(ctx, &table, t.layers, atlas, quadrant).map(|m| ctx.add_labeled_asset("original".to_string(), m));
        let material = ctx.add_labeled_asset(
            "material".to_string(),
            TerrainMaterial {
                base: StandardMaterial {
                    base_color: if diffuse.is_some() { Color::WHITE } else { Color::srgb(0.35, 0.45, 0.25) },
                    base_color_texture: diffuse,
                    perceptual_roughness: 0.95,
                    reflectance: 0.2,
                    ..default()
                },
                extension: detail,
            },
        );
        Ok(Fc3Terrain { sector, coarse, material, original })
    }

    fn extensions(&self) -> &[&str] {
        &["fct"]
    }
}

/// A sector's horizon map (`sd<id>_shadow`, a 64x64 G16R16 DDS) as an RG8
/// image: how high the land rises toward where the sun sets (red) and rises
/// (green) along its path, as 0..1 for -90..90 degrees. Like the atlas it is
/// transposed: u runs north, v east, a texel per metre.
fn horizon_image(dds: &[u8]) -> Option<Image> {
    const N: usize = 64;
    let px = dds.get(128..128 + N * N * 4).filter(|_| dds.starts_with(b"DDS "))?;
    // The high byte of each 16-bit channel.
    let data: Vec<u8> = px.chunks_exact(4).flat_map(|t| [t[1], t[3]]).collect();
    let mut image = Image::new(
        bevy::render::render_resource::Extent3d { width: N as u32, height: N as u32, depth_or_array_layers: 1 },
        bevy::render::render_resource::TextureDimension::D2,
        data,
        bevy::render::render_resource::TextureFormat::Rg8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = clamp_sampler();
    Some(image)
}

/// Builds a sector mesh in sector-local Bevy space (origin at the SW corner).
pub fn terrain_mesh(s: &TerrainSector, c: SectorCoord, step: usize) -> Mesh {
    let n = (GRID - 1) / step + 1;
    let atlas = c.atlas_coord();
    let (ax, ay) = ((c.x - atlas.x) as f32, (c.y - atlas.y) as f32);
    let mut positions = Vec::with_capacity(n * n + 4 * n);
    let mut normals = Vec::with_capacity(n * n + 4 * n);
    let mut uvs = Vec::with_capacity(n * n + 4 * n);
    for row in 0..n {
        for col in 0..n {
            let (gx, gy) = (col * step, row * step);
            positions.push([gx as f32, s.height(gx, gy), -(gy as f32)]);
            let nd = s.normal(gx, gy);
            normals.push([nd[0], nd[2], -nd[1]]);
            // The atlas covers 2x2 sectors: quadrants by sector parity (southern
            // sectors in the top half), each quadrant transposed (rows follow +X).
            let u = (ax * SECTOR_SIZE + gy as f32) / (2.0 * SECTOR_SIZE);
            let v = (ay * SECTOR_SIZE + gx as f32) / (2.0 * SECTOR_SIZE);
            uvs.push([u, v]);
        }
    }
    let idx = |col: usize, row: usize| (row * n + col) as u32;
    let mut indices = Vec::with_capacity((n - 1) * (n - 1) * 6 + 24 * n);
    let cells = fc3_formats::terrain::CELLS;
    // A coarse cell is left open if any cell it covers opens into a cave.
    let open = |col: usize, row: usize| {
        !s.holes.is_empty()
            && (row * step..(row + 1) * step).any(|r| (col * step..(col + 1) * step).any(|c| s.holes[r.min(cells - 1) * cells + c.min(cells - 1)]))
    };
    for row in 0..n - 1 {
        for col in 0..n - 1 {
            if open(col, row) {
                continue;
            }
            let (a, b, c2, d) = (idx(col, row), idx(col + 1, row), idx(col + 1, row + 1), idx(col, row + 1));
            indices.extend_from_slice(&[a, b, c2, a, c2, d]);
        }
    }
    // Skirts hide cracks where neighbouring sectors use different LODs.
    let skirt = 2.0 + 2.0 * step as f32;
    let edges: [Vec<(usize, usize)>; 4] = [
        (0..n).map(|i| (i, 0)).collect(),
        (0..n).map(|i| (n - 1, i)).collect(),
        (0..n).rev().map(|i| (i, n - 1)).collect(),
        (0..n).rev().map(|i| (0, i)).collect(),
    ];
    for edge in edges {
        let base = positions.len() as u32;
        for &(col, row) in &edge {
            let i = row * n + col;
            let mut p = positions[i];
            p[1] -= skirt;
            positions.push(p);
            normals.push(normals[i]);
            uvs.push(uvs[i]);
        }
        for k in 0..edge.len() - 1 {
            let (a, b) = (idx(edge[k].0, edge[k].1), idx(edge[k + 1].0, edge[k + 1].1));
            let (a2, b2) = (base + k as u32, base + k as u32 + 1);
            indices.extend_from_slice(&[a, a2, b2, a, b2, b]);
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

// ---------------------------------------------------------------- sector ---

#[derive(Asset, TypePath)]
pub struct Fc3Sector {
    pub id: u32,
    /// Parallel to the cached model list; models only mission layers use
    /// aren't loaded (default handles) until a layer shows them.
    pub models: Vec<Handle<Fc3Model>>,
    /// Cache paths of the models, parallel to `models`.
    pub model_paths: Vec<String>,
    /// The world as it always is: placements and entities on the default
    /// mission layer.
    pub placements: Vec<Placement>,
    pub entities: Vec<CachedEntity>,
    /// What other mission layers add while a mission has them on.
    pub layers: std::collections::HashMap<u64, LayerContent>,
    /// Per-placement material replacements, indexed by model slot.
    pub material_sets: Vec<Vec<Handle<StandardMaterial>>>,
}

/// A mission layer's share of a sector: its entities, and placements whose
/// entity indices point into them (model indices into the sector's lists).
#[derive(Default)]
pub struct LayerContent {
    pub entities: Vec<CachedEntity>,
    pub placements: Vec<Placement>,
}

impl Fc3Sector {
    /// Material for one part of a placement, honouring the placement's material set.
    pub fn part_material(&self, p: &Placement, part: &ModelPart) -> Handle<StandardMaterial> {
        if p.material_set != fc3_formats::cache::NO_MATERIAL_SET {
            if let Some(m) = self.material_sets.get(p.material_set as usize).and_then(|set| set.get(part.slot as usize)) {
                return m.clone();
            }
        }
        part.material.clone()
    }
}

#[derive(Default, TypePath)]
pub struct SectorLoader;

impl AssetLoader for SectorLoader {
    type Asset = Fc3Sector;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(&self, reader: &mut dyn Reader, _: &(), ctx: &mut LoadContext<'_>) -> Result<Fc3Sector, Self::Error> {
        let bytes = read_all(reader).await?;
        let s = CachedSector::from_bytes(&bytes).map_err(io_err)?;
        // Split the default layer from the others; each keeps its own entity
        // list, and placements are renumbered into it.
        let mut entities = Vec::new();
        let mut layers: std::collections::HashMap<u64, LayerContent> = std::collections::HashMap::new();
        let mut moved = Vec::with_capacity(s.entities.len());
        for e in s.entities {
            if e.mission_layer == fc3_formats::world::DEFAULT_MISSION_LAYER {
                moved.push((None, entities.len() as u32));
                entities.push(e);
            } else {
                let layer = layers.entry(e.mission_layer).or_default();
                moved.push((Some(e.mission_layer), layer.entities.len() as u32));
                layer.entities.push(e);
            }
        }
        let mut placements = Vec::new();
        let mut standing_models = vec![false; s.models.len()];
        for mut p in s.placements {
            let Some(&(layer, index)) = moved.get(p.entity as usize) else { continue };
            p.entity = index;
            match layer {
                None => {
                    if let Some(used) = standing_models.get_mut(p.model as usize) {
                        *used = true;
                    }
                    placements.push(p);
                }
                Some(l) => layers.entry(l).or_default().placements.push(p),
            }
        }
        let models = s.models.iter().zip(&standing_models).map(|(m, &used)| if used { ctx.load(cache_path(m)) } else { Handle::default() }).collect();
        let material_sets = s.material_sets.iter().map(|set| set.iter().map(|rel| ctx.load(cache_path(rel))).collect()).collect();
        Ok(Fc3Sector { id: s.sector_id, models, model_paths: s.models, placements, entities, layers, material_sets })
    }

    fn extensions(&self) -> &[&str] {
        &["fcs"]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires installed Far Cry 3 archives"]
    fn installed_godray_uses_authored_additive_unlit_blending() {
        let vfs = fc3_formats::Vfs::open_install(&fc3_formats::find_install().unwrap(), None).unwrap();
        let material = fc3_formats::material::parse(&vfs.read(r"graphics\_materials\MADESLONGCHAMPS-M-0708200756257742.material.bin").unwrap()).unwrap();
        assert_eq!(super::unlit_blend(&material.shader, &material.params), Some(bevy::prelude::AlphaMode::Add));
        assert_eq!(super::unlit_blend("Generic_FC3", &material.params), None);
    }
    use super::*;

    /// The horizon map keeps each 16-bit channel's high byte, in texel order.
    #[test]
    fn horizon_image_keeps_high_bytes() {
        let mut dds = b"DDS ".to_vec();
        dds.resize(128, 0);
        for i in 0..64 * 64 {
            let (r, g) = (i as u16 * 7, 0xFFFF - i as u16);
            dds.extend_from_slice(&r.to_le_bytes());
            dds.extend_from_slice(&g.to_le_bytes());
        }
        let image = horizon_image(&dds).unwrap();
        let data = image.data.unwrap();
        assert_eq!(data.len(), 64 * 64 * 2);
        assert_eq!(&data[2 * 100..2 * 100 + 2], &[((100u16 * 7) >> 8) as u8, ((0xFFFF - 100u16) >> 8) as u8]);
        assert!(horizon_image(&dds[..1000]).is_none());
    }
}
