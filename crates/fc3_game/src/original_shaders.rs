//! Rendering world props with the game's own shaders.
//!
//! The converter translates every original D3D9 shader to WGSL
//! (`shaders/wgsl.pak`, see `docs/shaders.md`) and pairs each material with one
//! of the game's G-buffer pixel shaders and a matching static-mesh vertex
//! shader, validated by running them on the CPU (`fc3_formats::original`). The
//! cached material carries the pair, its constants and its textures; here the
//! pair runs as a Bevy material:
//!
//! * engine-wide registers (camera matrices, viewport, ...) come from one
//!   shared buffer updated every frame,
//! * the per-object `WorldMatrix` comes from Bevy's mesh transform,
//! * the pixel shader's G-buffer outputs (gamma albedo in target 0, packed
//!   world normal in target 1) are decoded into Bevy's PBR input, so Bevy
//!   keeps doing lighting, shadows and the atmosphere.
//!
//! Positions and normals are converted between Bevy (Y-up) and Dunia (Z-up)
//! space at the boundaries. Prepass and shadow pipelines keep Bevy's defaults.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use bevy::asset::io::Reader;
use bevy::asset::uuid::Uuid;
use bevy::asset::{AssetLoader, LoadContext, RenderAssetUsages};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::Shader;
use bevy::shader::ShaderRef;
use fc3_formats::cache::{self, mat_flags, CachedMaterial, ShaderEntry};
use fc3_formats::original::{
    h, names, ShaderSet, USAGE_BLEND_INDICES, USAGE_BLEND_WEIGHT, USAGE_COLOR, USAGE_FOG, USAGE_NORMAL, USAGE_POSITION, USAGE_PSIZE, USAGE_TEXCOORD,
};

use crate::player::PlayerCamera;

/// Asset source that serves cache textures without sRGB conversion (the
/// original shaders work on raw texel values).
pub const RAW_SOURCE: &str = "fc3raw";

pub struct OriginalShadersPlugin;

impl Plugin for OriginalShadersPlugin {
    fn build(&self, app: &mut App) {
        let globals = app.world_mut().resource_mut::<Assets<ShaderBuffer>>().add(ShaderBuffer::from(GlobalRegisters { f: [Vec4::ZERO; 256] }));
        let _ = GLOBALS.set(globals);
        bevy::asset::embedded_asset!(app, "original_prepass.wgsl");
        app.add_plugins(MaterialPlugin::<OriginalMaterial>::default())
            .register_asset_loader(OriginalMaterialLoader)
            .add_systems(Update, (toggle_original_shaders, attach_swaps, apply_original_materials, apply_terrain_materials, create_pair_shaders).chain())
            .add_systems(PostUpdate, update_globals.after(TransformSystems::Propagate));
    }
}

/// Whether props render with the original shaders (F4 toggles).
#[derive(Resource)]
pub struct OriginalShaders {
    pub enabled: bool,
}

/// Marks a static prop part that may use the original shaders.
#[derive(Component)]
pub struct PropPart;

/// Marks a skinned character part that may use the original shaders.
#[derive(Component)]
pub struct SkinnedPart;

/// A prop part that can switch between its standard and original material.
#[derive(Component)]
pub struct OriginalSwap {
    pub standard: Handle<StandardMaterial>,
    pub original: Handle<OriginalMaterial>,
}

/// A terrain chunk that can switch between the layered terrain material and
/// the game's own terrain shaders.
#[derive(Component)]
pub struct TerrainSwap {
    pub standard: Handle<crate::cache_assets::TerrainMaterial>,
    pub original: Handle<OriginalMaterial>,
}

// ------------------------------------------------------------------ library ---

/// The translated shaders: metadata (shared pairing helpers) plus the
/// compressed WGSL pack.
pub struct ShaderLibrary {
    set: ShaderSet,
    pack: Vec<u8>,
}

static LIBRARY: OnceLock<Option<Arc<ShaderLibrary>>> = OnceLock::new();
static GLOBALS: OnceLock<Handle<ShaderBuffer>> = OnceLock::new();

/// The library from the cache, loaded on first use (None if the cache has no shaders).
pub fn library() -> Option<Arc<ShaderLibrary>> {
    LIBRARY
        .get_or_init(|| match ShaderLibrary::load(&fc3_formats::cache::default_cache_root()) {
            Ok(l) => {
                info!("original shaders: {} translated", l.set.entries.len());
                Some(Arc::new(l))
            }
            Err(e) => {
                warn!("original shaders unavailable: {e:#}");
                None
            }
        })
        .clone()
}

impl ShaderLibrary {
    fn load(root: &Path) -> anyhow::Result<ShaderLibrary> {
        let index = std::fs::read_to_string(root.join(cache::SHADER_INDEX_REL))?;
        let pack = std::fs::read(root.join(cache::SHADER_PACK_REL))?;
        let entries: Vec<ShaderEntry> = index.lines().filter(|l| !l.starts_with('#')).filter_map(ShaderEntry::from_line).collect();
        Ok(ShaderLibrary { set: ShaderSet::new(entries), pack })
    }

    pub fn entry(&self, hash: u64) -> Option<&ShaderEntry> {
        self.set.entry(hash)
    }

    pub fn wgsl(&self, e: &ShaderEntry) -> anyhow::Result<String> {
        let blob =
            self.pack.get(e.offset as usize..e.offset as usize + e.size as usize).ok_or_else(|| anyhow::anyhow!("shader {:016x} outside the pack", e.hash))?;
        Ok(String::from_utf8(cache::decompress(blob)?)?)
    }

    fn global_reg(&self, name: &str) -> Option<u32> {
        self.set.global_reg(name)
    }
}

// ----------------------------------------------------------------- material ---

#[derive(Clone, Copy, ShaderType)]
pub struct D3dConstants {
    pub f: [Vec4; 256],
    pub i: [IVec4; 16],
    pub b: [UVec4; 4],
}

/// Pixel shader constant register the glue reads a material's dye from.
const DYE_REGISTER: usize = 255;

impl Default for D3dConstants {
    fn default() -> Self {
        D3dConstants { f: [Vec4::ZERO; 256], i: [IVec4::ZERO; 16], b: [UVec4::ZERO; 4] }
    }
}

#[derive(Clone, Copy, ShaderType)]
struct GlobalRegisters {
    f: [Vec4; 256],
}

/// Which translated vertex/pixel shader pair a material uses (0 = none).
#[derive(Clone, Copy, Hash, PartialEq, Eq, Default, Debug)]
pub struct OriginalKey {
    pub vs: u64,
    pub ps: u64,
    /// Render target 1 holds a usable world normal (else the mesh normal is used).
    pub normal: bool,
    /// Drawn on skinned meshes (the vertex shader reads a bone palette).
    pub skinned: bool,
    /// Drawn on terrain meshes (the vertex shader reads packed grid vertices).
    pub terrain: bool,
    /// The material's render state: no back-face culling, and the colour
    /// output's alpha cuts it out (D3D9 alpha test, done in the glue shader).
    pub double_sided: bool,
    pub alpha_test: bool,
    /// Self-lit (`Unlit` and `EffectLit` materials: signs, lamp glows,
    /// screens): the colour is shown as it is, without lighting.
    pub unlit: bool,
    /// Blended over what is behind (`Blended` and `Decal` overlays, lit
    /// effects), by the diffuse map's alpha.
    pub blended: bool,
}

/// A material rendered with a pair of the game's own shaders.
#[derive(Asset, AsBindGroup, TypePath, Clone)]
#[bind_group_data(OriginalKey)]
pub struct OriginalMaterial {
    #[uniform(0)]
    pub ps_constants: D3dConstants,
    #[storage(1, read_only)]
    pub globals: Handle<ShaderBuffer>,
    #[uniform(2)]
    pub vs_constants: D3dConstants,
    #[texture(16)]
    #[sampler(17)]
    pub s0: Option<Handle<Image>>,
    #[texture(18)]
    #[sampler(19)]
    pub s1: Option<Handle<Image>>,
    #[texture(20)]
    #[sampler(21)]
    pub s2: Option<Handle<Image>>,
    #[texture(22)]
    #[sampler(23)]
    pub s3: Option<Handle<Image>>,
    #[texture(24)]
    #[sampler(25)]
    pub s4: Option<Handle<Image>>,
    #[texture(26)]
    #[sampler(27)]
    pub s5: Option<Handle<Image>>,
    #[texture(28)]
    #[sampler(29)]
    pub s6: Option<Handle<Image>>,
    #[texture(30)]
    #[sampler(31)]
    pub s7: Option<Handle<Image>>,
    #[texture(32)]
    #[sampler(33)]
    pub s8: Option<Handle<Image>>,
    #[texture(34)]
    #[sampler(35)]
    pub s9: Option<Handle<Image>>,
    #[texture(36)]
    #[sampler(37)]
    pub s10: Option<Handle<Image>>,
    #[texture(38)]
    #[sampler(39)]
    pub s11: Option<Handle<Image>>,
    #[texture(40)]
    #[sampler(41)]
    pub s12: Option<Handle<Image>>,
    #[texture(42)]
    #[sampler(43)]
    pub s13: Option<Handle<Image>>,
    #[texture(44)]
    #[sampler(45)]
    pub s14: Option<Handle<Image>>,
    #[texture(46)]
    #[sampler(47)]
    pub s15: Option<Handle<Image>>,
    /// The world's horizon atlas, for shadows of distant land (`horizon`).
    #[texture(48)]
    #[sampler(49)]
    pub horizon: Option<Handle<Image>>,
    /// Cut-out and blended surfaces: the diffuse map, whose alpha cuts them
    /// out (or is their coverage) at the mesh UV times its tiling (`cutout`:
    /// xy tiling, z threshold, 0 when not cut out; w 1 when blended, with a
    /// threshold no alpha reaches, so overlays cast no shadow).
    #[texture(50)]
    #[sampler(51)]
    pub cutout_texture: Option<Handle<Image>>,
    #[uniform(52)]
    pub cutout: Vec4,
    pub key: OriginalKey,
}

impl From<&OriginalMaterial> for OriginalKey {
    fn from(m: &OriginalMaterial) -> OriginalKey {
        m.key
    }
}

impl OriginalMaterial {
    fn new(globals: Handle<ShaderBuffer>) -> OriginalMaterial {
        OriginalMaterial {
            ps_constants: D3dConstants::default(),
            globals,
            vs_constants: D3dConstants::default(),
            s0: None,
            s1: None,
            s2: None,
            s3: None,
            s4: None,
            s5: None,
            s6: None,
            s7: None,
            s8: None,
            s9: None,
            s10: None,
            s11: None,
            s12: None,
            s13: None,
            s14: None,
            s15: None,
            horizon: Some(crate::horizon::atlas()),
            cutout_texture: None,
            cutout: Vec4::ZERO,
            key: OriginalKey::default(),
        }
    }

    fn slot(&mut self, n: u32) -> Option<&mut Option<Handle<Image>>> {
        Some(match n {
            0 => &mut self.s0,
            1 => &mut self.s1,
            2 => &mut self.s2,
            3 => &mut self.s3,
            4 => &mut self.s4,
            5 => &mut self.s5,
            6 => &mut self.s6,
            7 => &mut self.s7,
            8 => &mut self.s8,
            9 => &mut self.s9,
            10 => &mut self.s10,
            11 => &mut self.s11,
            12 => &mut self.s12,
            13 => &mut self.s13,
            14 => &mut self.s14,
            15 => &mut self.s15,
            _ => return None,
        })
    }
}

/// The end of a mesh vertex entry point's inputs: the mesh tangent where there
/// is one, else any direction across the normal; then the tangent frame the
/// original shaders read as PSIZE (tangent) and FOG (binormal), Bevy space;
/// and the second UV set, else the first.
const TANGENT_INPUT: &str = "#ifdef D3D_TANGENT
    @location(6) a_tangent: vec4<f32>,
#endif
#ifdef D3D_UV1
    @location(7) a_uv1: vec2<f32>,
#endif
) -> GlueVarying {
#ifdef D3D_UV1
    let d3d_uv1 = a_uv1;
#else
    let d3d_uv1 = a_uv;
#endif
#ifdef D3D_TANGENT
    let d3d_tangent4 = a_tangent;
#else
    let d3d_side = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(a_normal.y) > 0.9);
    let d3d_tangent4 = vec4<f32>(normalize(cross(d3d_side, a_normal)), 1.0);
#endif
    let d3d_tangent = d3d_tangent4.xyz;
    let d3d_binormal = cross(a_normal, d3d_tangent) * select(1.0, -1.0, d3d_tangent4.w < 0.0);
";

/// Alpha below this cuts a cut-out surface away.
const CUTOUT_THRESHOLD: f32 = 0.5;

/// A cut-out material's diffuse tiling: `DiffuseTiling1` (u, v), or the
/// `Simple` family's `Tiling` (xy), else none.
fn cutout_tiling(params: &[(u32, Vec<u8>)]) -> [f32; 2] {
    fc3_formats::material::diffuse_mapping(params).0
}

/// Deterministic shader handles for a pair (created by `create_pair_shaders`).
fn pair_handles(key: OriginalKey) -> (Handle<Shader>, Handle<Shader>) {
    let flag = u64::from(key.normal)
        | u64::from(key.skinned) << 1
        | u64::from(key.terrain) << 2
        | u64::from(key.alpha_test) << 3
        | u64::from(key.unlit) << 4
        | u64::from(key.blended) << 5;
    let vs = Uuid::from_u64_pair(key.vs ^ 0x6663_335f_7673_0000 ^ flag, key.ps);
    let ps = Uuid::from_u64_pair(key.ps ^ 0x6663_335f_7073_0000 ^ flag, key.vs);
    (Handle::Uuid(vs, std::marker::PhantomData), Handle::Uuid(ps, std::marker::PhantomData))
}

impl Material for OriginalMaterial {
    fn alpha_mode(&self) -> AlphaMode {
        if self.cutout.w > 0.0 {
            AlphaMode::Blend
        } else if self.cutout.z > 0.0 {
            AlphaMode::Mask(self.cutout.z)
        } else {
            AlphaMode::Opaque
        }
    }

    fn depth_bias(&self) -> f32 {
        // Overlays sort just in front of the surface they cover (the depth
        // test's own bias is set in `specialize`).
        if self.cutout.w > 0.0 {
            crate::cache_assets::DECAL_DEPTH_BIAS
        } else {
            0.0
        }
    }

    fn prepass_fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("original_prepass.wgsl")).with_source("embedded").into()
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Prepass and shadow pipelines keep Bevy's default shaders.
        if descriptor.label.as_deref() == Some("prepass_pipeline") {
            return Ok(());
        }
        let mut attributes = vec![Mesh::ATTRIBUTE_POSITION.at_shader_location(0), Mesh::ATTRIBUTE_NORMAL.at_shader_location(1)];
        if !key.bind_group_data.terrain {
            attributes.push(Mesh::ATTRIBUTE_UV_0.at_shader_location(2));
            attributes.push(crate::cache_assets::ATTRIBUTE_D3D_COLOR.at_shader_location(3));
        }
        if key.bind_group_data.skinned {
            attributes.push(Mesh::ATTRIBUTE_JOINT_INDEX.at_shader_location(4));
            attributes.push(Mesh::ATTRIBUTE_JOINT_WEIGHT.at_shader_location(5));
        }
        // The tangent frame, where the mesh has one (else the glue makes one up).
        if !key.bind_group_data.terrain && layout.0.contains(Mesh::ATTRIBUTE_TANGENT) {
            attributes.push(Mesh::ATTRIBUTE_TANGENT.at_shader_location(6));
            descriptor.vertex.shader_defs.push("D3D_TANGENT".into());
        }
        // The second UV set, where the mesh has one (else the first stands in).
        if !key.bind_group_data.terrain && layout.0.contains(Mesh::ATTRIBUTE_UV_1) {
            attributes.push(Mesh::ATTRIBUTE_UV_1.at_shader_location(7));
            descriptor.vertex.shader_defs.push("D3D_UV1".into());
        }
        let vertex_layout = layout.0.get_layout(&attributes)?;
        descriptor.vertex.buffers = vec![vertex_layout];
        if key.bind_group_data.double_sided {
            descriptor.primitive.cull_mode = None;
        }
        // Overlays sit just in front of the surface they cover (as on the
        // standard material; `Material::depth_bias` only orders them).
        if key.bind_group_data.blended {
            if let Some(depth) = descriptor.depth_stencil.as_mut() {
                depth.bias.constant = crate::cache_assets::DECAL_DEPTH_BIAS as i32;
            }
        }
        let (vs, ps) = pair_handles(key.bind_group_data);
        descriptor.vertex.shader = vs;
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader = ps;
        }
        Ok(())
    }
}

fn repeat_sampler() -> ImageSampler {
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

#[derive(Default, TypePath)]
pub struct OriginalMaterialLoader;

impl AssetLoader for OriginalMaterialLoader {
    type Asset = OriginalMaterial;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(&self, reader: &mut dyn Reader, _: &(), ctx: &mut LoadContext<'_>) -> Result<OriginalMaterial, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let cm = CachedMaterial::from_bytes(&bytes).map_err(std::io::Error::other)?;
        let globals = GLOBALS.get().cloned().ok_or_else(|| std::io::Error::other("original shaders not initialised"))?;
        let mut mat = OriginalMaterial::new(globals);
        // The converter chose and validated the pair; just apply it. Loads
        // through the skinned source are for animated characters.
        let Some(o) = cm.original.as_ref() else { return Ok(mat) };
        // Blended overlays (dirt, stains, decals; lit effects such as foam):
        // the paired shader gives their colour, and the diffuse map's alpha
        // how much of it covers what is behind. FC3_NO_BLEND=1 (debug) leaves
        // them on the standard material.
        let blended = cm.flags & mat_flags::ALPHA_BLEND != 0;
        if blended && (cm.diffuse.is_empty() || std::env::var("FC3_NO_BLEND").is_ok()) {
            return Ok(mat);
        }
        // Cut-out surfaces (rags, tarps, fences): D3D9 tests the colour
        // output's alpha as render state, but the paired shader's alpha often
        // isn't coverage (gloss, or nothing), which cut whole surfaces away.
        // They are cut out by the diffuse map's alpha instead (in the glue
        // and the prepass); plants keep their own foliage material.
        let cut_out = cm.flags & mat_flags::ALPHA_TEST != 0 && !blended;
        // FC3_NO_CUTOUT=1 (debug): cut-out surfaces on the standard material.
        if cut_out && (cm.flags & mat_flags::VEGETATION != 0 || cm.diffuse.is_empty() || std::env::var("FC3_NO_CUTOUT").is_ok()) {
            return Ok(mat);
        }
        if cut_out || blended {
            let path = crate::cache_assets::cache_path(&cm.diffuse);
            mat.cutout_texture = Some(
                ctx.load_builder()
                    .with_settings(|s: &mut ImageLoaderSettings| {
                        s.is_srgb = true;
                        s.sampler = repeat_sampler();
                        s.asset_usage = RenderAssetUsages::RENDER_WORLD;
                    })
                    .load(path),
            );
            let tiling = cutout_tiling(&cm.params);
            // Overlays cast no shadow: their threshold is past any alpha, so
            // the shadow pass discards them whole.
            mat.cutout = if blended { Vec4::new(tiling[0], tiling[1], 2.0, 1.0) } else { Vec4::new(tiling[0], tiling[1], CUTOUT_THRESHOLD, 0.0) };
        }
        let skinned = ctx.path().source() == &bevy::asset::io::AssetSourceId::from(crate::cache_assets::SKINNED_SOURCE);
        let (vs, vs_constants) = if skinned { (o.skinned_vs, &o.skinned_vs_constants) } else { (o.vs, &o.vs_constants) };
        if vs == 0 {
            return Ok(mat);
        }
        // Older caches may contain a mean-colour-validated Unlit pair whose
        // texture lookup receives world position instead of the mesh UVs.
        if cm.shader == "Unlit" {
            let valid = library().is_some_and(|lib| {
                lib.entry(vs).zip(lib.entry(o.ps)).is_some_and(|(vs, ps)| {
                    fc3_formats::original::material_uv_compatible(&cm.shader, vs, ps)
                })
            });
            if !valid { return Ok(mat); }
        }
        for (r, v) in vs_constants {
            if (*r as usize) < 256 {
                mat.vs_constants.f[*r as usize] = Vec4::from_array(*v);
            }
        }
        for (r, v) in &o.ps_constants {
            if (*r as usize) < 256 {
                mat.ps_constants.f[*r as usize] = Vec4::from_array(*v);
            }
        }
        // A dyed material whose pixel shader never reads the dye (none of its
        // constants carry it) gets it applied by the glue, in c255 (beyond
        // the 224 a ps_3_0 shader can address); w = 1 switches it on.
        if let Some(dye) = fc3_formats::material::single_dye(&cm.shader, &cm.params) {
            let raw = fc3_formats::material::dye_param(&cm.shader, &cm.params).unwrap_or(dye);
            let carried = o.ps_constants.iter().any(|(_, v)| (0..3).all(|i| (v[i] - raw[i]).abs() < 1e-4));
            if !carried {
                mat.ps_constants.f[DYE_REGISTER] = Vec4::new(dye[0], dye[1], dye[2], 1.0);
            }
        }
        for (reg, rel, srgb) in &o.samplers {
            // Colour maps are sampled with sRGB decoding (as the game's sampler
            // state does); normal maps, masks and other data stay raw.
            let colour = *srgb;
            let path = if colour { crate::cache_assets::cache_path(rel) } else { format!("{RAW_SOURCE}://{rel}") };
            let handle = ctx
                .load_builder()
                .with_settings(move |s: &mut ImageLoaderSettings| {
                    s.is_srgb = colour;
                    s.sampler = repeat_sampler();
                    s.asset_usage = RenderAssetUsages::RENDER_WORLD;
                })
                .load(path);
            if let Some(slot) = mat.slot(*reg as u32) {
                *slot = Some(handle);
            }
        }
        mat.key = OriginalKey {
            vs,
            ps: o.ps,
            normal: o.use_normal,
            skinned,
            terrain: false,
            double_sided: cm.flags & mat_flags::DOUBLE_SIDED != 0,
            alpha_test: cut_out,
            unlit: cm.flags & mat_flags::EMISSIVE != 0,
            blended,
        };
        Ok(mat)
    }

    fn extensions(&self) -> &[&str] {
        &["fcmat"]
    }
}

// ------------------------------------------------------------------ terrain ---

/// One sector's layers on the game's own terrain shaders (None when the
/// cache has no usable terrain pair or the sector has no layers).
///
/// The pixel shader blends up to four detail layers by the atlas mask
/// (channel `k` drives layer `k`, as in `terrain_detail.wgsl`), modulated by
/// the colour atlas and the layer's distance noise; far away it fades to the
/// baked atlas diffuse. `quadrant` is the sector's place in its 2x2 atlas.
pub fn terrain_material(ctx: &mut LoadContext, table: &cache::TerrainLayerTable, layers: [u8; 4], atlas: u32, quadrant: Vec2) -> Option<OriginalMaterial> {
    use crate::cache_assets::load_texture;
    use fc3_formats::terrain::SECTOR_SIZE;

    let lib = library()?;
    let (vs, ps) = lib.set.terrain_pair()?;
    let mut mat = OriginalMaterial::new(GLOBALS.get().cloned()?);
    let vreg = |name: &str| ShaderSet::register(vs, name).map(|r| r as usize).filter(|r| *r < 256);
    let preg = |name: &str| ShaderSet::register(ps, name).map(|r| r as usize).filter(|r| *r < 252);
    // Vertex stage: no neighbour LOD tweening; atlas UVs for this quadrant
    // (u follows local Y, v local X, half the atlas per sector).
    if let Some(r) = vreg(names::CURRENT_TWEENING) {
        mat.vs_constants.f[r] = Vec4::X;
    }
    let uv_offset = quadrant * 0.5;
    if let Some(r) = vreg(names::SECTOR_UV_TRANSFORM) {
        mat.vs_constants.f[r] = Vec4::new(0.5, 0.5, uv_offset.x, uv_offset.y);
    }
    if let Some(r) = vreg(names::SECTOR_UV_MIN) {
        mat.vs_constants.f[r] = uv_offset.extend(0.0).extend(0.0);
    }

    // Pixel stage: per-layer textures and parameters.
    let sampler = |mat: &mut OriginalMaterial, name: &str, handle: Option<Handle<Image>>| {
        if let Some(b) = ps.bindings.iter().find(|b| b.sampler && b.name_hash == h(name)) {
            if let Some(slot) = mat.slot(b.register) {
                *slot = handle;
            }
        }
    };
    let mut any = false;
    for (k, &index) in layers.iter().enumerate() {
        let Some(layer) = table.layers.get(index as usize).filter(|l| !l.diffuse.is_empty()) else { continue };
        any = true;
        if let Some(r) = preg(names::MASK_CHANNEL_SELECTORS) {
            mat.ps_constants.f[r + k][k] = 1.0;
        }
        if let Some(r) = preg(names::DETAIL_UV_SCALING_GLOSS) {
            mat.ps_constants.f[r + k] = Vec4::new(layer.tiling / SECTOR_SIZE, layer.normal_tiling / SECTOR_SIZE, 0.0, layer.glossiness);
        }
        if let Some(r) = preg(names::NOISE_LEVELS) {
            mat.ps_constants.f[r][k] = layer.noise_level;
        }
        sampler(&mut mat, &names::detail_sampler(k, false), load_texture(ctx, &layer.diffuse, true, false));
        sampler(&mut mat, &names::detail_sampler(k, true), load_texture(ctx, &layer.normal, false, false));
    }
    if !any {
        return None;
    }
    if let Some(r) = preg(names::NOISE_TERRAIN_TRANSITION) {
        let [a, s, u] = table.noise_transition;
        mat.ps_constants.f[r] = Vec4::new(a, s, u, 0.0);
    }
    // Beyond ~250 m the layers fade into the baked atlas diffuse (fully by 400 m).
    if let Some(r) = preg(names::MATERIAL_LOD_PARAMS) {
        mat.ps_constants.f[r] = Vec4::new(-1.0 / 150.0, 400.0 / 150.0, 0.0, 0.0);
    }
    if let Some(r) = ShaderSet::register(ps, names::BLEND_DETAIL).filter(|r| *r < 16) {
        mat.ps_constants.b[r as usize / 4][r as usize % 4] = 1;
    }
    sampler(&mut mat, names::MASK_SAMPLER, load_texture(ctx, &cache::atlas_rel(atlas, "mask"), false, true));
    sampler(&mut mat, names::COLOR_SAMPLER, load_texture(ctx, &cache::atlas_rel(atlas, "color"), false, true));
    sampler(&mut mat, names::DIFFUSE_SAMPLER, load_texture(ctx, &cache::atlas_rel(atlas, "diffuse"), true, true));
    sampler(&mut mat, names::VISTA_NOISE_SAMPLER, load_texture(ctx, cache::TERRAIN_VISTA_NOISE_REL, false, false));
    sampler(&mut mat, names::PERLIN_NOISE_SAMPLER, load_texture(ctx, cache::TERRAIN_PERLIN_REL, false, false));
    mat.key = OriginalKey {
        vs: vs.hash,
        ps: ps.hash,
        normal: true,
        skinned: false,
        terrain: true,
        double_sided: false,
        alpha_test: false,
        unlit: false,
        blended: false,
    };
    Some(mat)
}

/// Sectors around the player drawn with the original near-terrain shader;
/// beyond, the batched layered terrain material takes over (the original
/// engine also switches to a cheaper vista shader at distance).
const ORIGINAL_TERRAIN_RADIUS: i32 = 5;

/// Puts each terrain chunk on the material matching the current setting.
fn apply_terrain_materials(
    settings: Res<OriginalShaders>,
    originals: Res<Assets<OriginalMaterial>>,
    mut commands: Commands,
    players: Query<&Transform, With<crate::player::Player>>,
    chunks: Query<(Entity, &TerrainSwap, &crate::terrain::TerrainChunk, Has<MeshMaterial3d<OriginalMaterial>>)>,
) {
    let here = players.single().ok().map(|t| fc3_formats::terrain::SectorCoord::containing(t.translation.x, -t.translation.z));
    for (e, swap, chunk, on_original) in &chunks {
        // One sector of hysteresis so chunks at the edge don't flicker between materials.
        let reach = ORIGINAL_TERRAIN_RADIUS + i32::from(on_original);
        let near = here.is_some_and(|h| (chunk.coord.x - h.x).abs().max((chunk.coord.y - h.y).abs()) <= reach);
        let want = settings.enabled && near && originals.contains(&swap.original);
        if want && !on_original {
            commands.entity(e).remove::<MeshMaterial3d<crate::cache_assets::TerrainMaterial>>().insert(MeshMaterial3d(swap.original.clone()));
        } else if !want && on_original {
            commands.entity(e).remove::<MeshMaterial3d<OriginalMaterial>>().insert(MeshMaterial3d(swap.standard.clone()));
        }
    }
}

// ------------------------------------------------------------------ shaders ---

/// Bevy (Y-up) <-> Dunia (Z-up): Dunia (x, y, z) = Bevy (x, -z, y).
const SPACE_CONSTANTS: &str = "
const DUNIA_FROM_BEVY = mat4x4<f32>(1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
const BEVY_FROM_DUNIA = mat4x4<f32>(1.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
struct D3dGlobals {
    f: array<vec4<f32>, 256>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var<storage, read> d3d_g: D3dGlobals;
";

/// Rebinds a translated module for the material bind group: constants at
/// `constants_binding`, engine globals from the shared buffer.
fn rebind(lib: &ShaderLibrary, e: &ShaderEntry, wgsl: &str, constants_binding: u32) -> String {
    let mut s = wgsl.replace(
        "@group(0) @binding(0) var<uniform> d3d_c: D3dConstants;",
        &format!("@group(#{{MATERIAL_BIND_GROUP}}) @binding({constants_binding}) var<uniform> d3d_c: D3dConstants;"),
    );
    s = s.replace("@group(0)", "@group(#{MATERIAL_BIND_GROUP})");
    for b in e.bindings.iter().filter(|b| !b.sampler && lib.set.engine.contains(&b.name_hash)) {
        if b.name_hash == h(names::WORLD_MATRIX) || b.name_hash == h(names::MESH_DECOMPRESSION) {
            continue;
        }
        for k in 0..b.rows.max(1) as u32 {
            let r = b.register + k;
            s = s.replace(&format!("d3d_c.f[{r}]"), &format!("d3d_g.f[{r}]"));
        }
    }
    s
}

fn free_locations(used: &HashSet<u32>, n: usize) -> Vec<u32> {
    (0..16u32).filter(|l| !used.contains(l)).take(n).collect()
}

/// Builds the vertex and fragment modules for a pair.
fn pair_sources(lib: &ShaderLibrary, key: OriginalKey) -> anyhow::Result<(String, String)> {
    let vs = lib.entry(key.vs).ok_or_else(|| anyhow::anyhow!("missing vertex shader"))?;
    let ps = lib.entry(key.ps).ok_or_else(|| anyhow::anyhow!("missing pixel shader"))?;
    let used: HashSet<u32> = ps.inputs.iter().filter_map(|i| i.location).collect();
    let free = free_locations(&used, 4);
    let needs_uv = key.alpha_test || key.blended;
    if free.len() < 3 || (needs_uv && free.len() < 4) {
        anyhow::bail!("no free varyings");
    }
    let (loc_world, loc_normal, loc_instance) = (free[0], free[1], free[2]);
    // Varyings: every VS output the PS reads, by semantic.
    let varyings: Vec<(u32, u32, u32)> = ps
        .inputs
        .iter()
        .filter_map(|i| {
            let o = vs.outputs.iter().find(|o| (o.usage, o.usage_index) == (i.usage, i.usage_index))?;
            Some((i.location?, o.register, i.register))
        })
        .collect();
    let mut fields = String::from("struct GlueVarying {\n    @builtin(position) position: vec4<f32>,\n");
    for (loc, _, ps_reg) in &varyings {
        fields += &format!("    @location({loc}) v{ps_reg}: vec4<f32>,\n");
    }
    fields += &format!(
        "    @location({loc_world}) bevy_world: vec4<f32>,\n    @location({loc_normal}) bevy_normal: vec4<f32>,\n    @location({loc_instance}) @interpolate(flat) instance: u32,\n"
    );
    if needs_uv {
        fields += &format!("    @location({}) bevy_uv: vec2<f32>,\n", free[3]);
    }
    fields += "}\n";

    // Vertex module.
    let mut vsrc = String::from(if key.skinned {
        "#import bevy_pbr::{mesh_functions, view_transformations, skinning}\n"
    } else {
        "#import bevy_pbr::{mesh_functions, view_transformations}\n"
    });
    vsrc += SPACE_CONSTANTS;
    vsrc += "var<private> d3d_world: array<vec4<f32>, 4>;\nvar<private> d3d_instance: u32;\n";
    if key.terrain {
        // Terrain vertices: the original shaders read a sector-local grid
        // position, heights as two 16-bit values in 1/64 m, and a packed normal.
        vsrc += "var<private> d3d_sector: vec4<f32>;\n";
        vsrc += "fn d3d_height_bytes(y: f32) -> vec4<f32> {\n    let h = y * 64.0;\n    let hi = floor(h / 256.0);\n    let lo = h - hi * 256.0;\n    return vec4<f32>(lo, hi, lo, hi);\n}\n";
    }
    let mut body = rebind(lib, vs, &lib.wgsl(vs)?, 2);
    // Shaders translated before outputs got their D3D9 default (see
    // `d3d9_wgsl::VS_OUTPUT_DEFAULT`) declare them bare.
    for io in &vs.outputs {
        body = body.replace(
            &format!("var<private> o{}: vec4<f32>;", io.register),
            &format!("var<private> o{}: vec4<f32> = {};", io.register, fc3_formats::d3d9_wgsl::VS_OUTPUT_DEFAULT),
        );
    }
    if let Some(wm) = vs.bindings.iter().find(|b| b.name_hash == h(names::WORLD_MATRIX)) {
        // Only the binding's own rows; the register after it belongs to another constant.
        for k in 0..(wm.rows.max(1) as u32).min(4) {
            body = body.replace(&format!("d3d_c.f[{}]", wm.register + k), &format!("d3d_world[{k}]"));
        }
    }
    if key.terrain {
        // The sector origin comes from the mesh transform.
        if let Some(r) = ShaderSet::register(vs, names::SECTOR_OFFSET) {
            body = body.replace(&format!("d3d_c.f[{r}]"), "d3d_sector");
        }
    }
    if key.skinned {
        // Skinned shaders index the bone palette relative to its first register;
        // those reads come from Bevy's animated joints (in Dunia space).
        let palette =
            vs.bindings.iter().find(|b| b.name_hash == h(names::BLEND_MATRICES)).ok_or_else(|| anyhow::anyhow!("skinned shader without a bone palette"))?;
        let base = palette.register;
        let hook = "fn d3d_crel(i: i32) -> vec4<f32> {\n";
        if !body.contains(hook) {
            anyhow::bail!("skinned shader without relative palette reads");
        }
        body = body.replace(hook, &format!("{hook}    if (i >= {base}i) {{ return d3d_bone_row(i - {base}i); }}\n"));
        vsrc += r#"fn d3d_bone_row(k: i32) -> vec4<f32> {
    let bone = u32(k / 3);
    let row = k % 3;
    let m = DUNIA_FROM_BEVY * skinning::skin_model(vec4<u32>(bone, 0u, 0u, 0u), vec4<f32>(1.0, 0.0, 0.0, 0.0), d3d_instance) * BEVY_FROM_DUNIA;
    return vec4<f32>(m[0][row], m[1][row], m[2][row], m[3][row]);
}
"#;
    }
    vsrc += &body;
    vsrc += &fields;
    if key.skinned {
        vsrc += "@vertex\nfn vertex(@builtin(instance_index) instance: u32, @location(0) a_position: vec3<f32>, @location(1) a_normal: vec3<f32>, @location(2) a_uv: vec2<f32>, @location(3) a_color: vec4<f32>, @location(4) a_joint_index: vec4<u32>, @location(5) a_joint_weight: vec4<f32>,\n";
        vsrc += TANGENT_INPUT;
        vsrc += "    d3d_instance = instance;\n";
        vsrc += "    let world_from_local = skinning::skin_model(a_joint_index, a_joint_weight, instance);\n";
        // The bone rows already place the vertex in the world.
        vsrc += "    d3d_world[0] = vec4<f32>(1.0, 0.0, 0.0, 0.0);\n    d3d_world[1] = vec4<f32>(0.0, 1.0, 0.0, 0.0);\n";
        vsrc += "    d3d_world[2] = vec4<f32>(0.0, 0.0, 1.0, 0.0);\n    d3d_world[3] = vec4<f32>(0.0, 0.0, 0.0, 1.0);\n";
    } else {
        if key.terrain {
            vsrc += "@vertex\nfn vertex(@builtin(instance_index) instance: u32, @location(0) a_position: vec3<f32>, @location(1) a_normal: vec3<f32>) -> GlueVarying {\n";
        } else {
            vsrc += "@vertex\nfn vertex(@builtin(instance_index) instance: u32, @location(0) a_position: vec3<f32>, @location(1) a_normal: vec3<f32>, @location(2) a_uv: vec2<f32>, @location(3) a_color: vec4<f32>,\n";
            vsrc += TANGENT_INPUT;
        }
        vsrc += "    d3d_instance = instance;\n";
        vsrc += "    let world_from_local = mesh_functions::get_world_from_local(instance);\n";
        vsrc += "    let w = DUNIA_FROM_BEVY * world_from_local * BEVY_FROM_DUNIA;\n";
        vsrc += "    for (var r = 0; r < 4; r = r + 1) {\n        d3d_world[r] = vec4<f32>(w[0][r], w[1][r], w[2][r], w[3][r]);\n    }\n";
        if key.terrain {
            vsrc += "    d3d_sector = vec4<f32>(w[3].x, w[3].y, 0.0, 0.0);\n";
        }
    }
    for i in &vs.inputs {
        let v = format!("v{}", i.register);
        let value = match i.usage {
            // Terrain: sector-local grid position with no neighbour LOD edge (4).
            USAGE_POSITION if key.terrain => "vec4<f32>(a_position.x, -a_position.z, 4.0, 4.0)".to_string(),
            USAGE_PSIZE if key.terrain => "d3d_height_bytes(a_position.y)".to_string(),
            USAGE_NORMAL if key.terrain => "vec4<f32>(a_normal.x, -a_normal.z, a_normal.x, -a_normal.z) * 0.5 + vec4<f32>(0.5)".to_string(),
            USAGE_POSITION => "vec4<f32>(a_position.x, -a_position.z, a_position.y, 1.0)".to_string(),
            USAGE_NORMAL => "vec4<f32>(vec3<f32>(a_normal.x, -a_normal.z, a_normal.y) * 0.5 + vec3<f32>(0.5), 1.0)".to_string(),
            // The game streams both UV sets as one four-component input
            // (they sit side by side in its vertex layout); materials pick
            // either half by their texture's UV group.
            USAGE_TEXCOORD if i.usage_index == 0 => "vec4<f32>(a_uv, d3d_uv1)".to_string(),
            USAGE_TEXCOORD => "vec4<f32>(a_uv, 0.0, 1.0)".to_string(),
            // FC3_ORIG_WHITE=1 (debug) ignores the mesh's vertex colours.
            USAGE_COLOR if std::env::var("FC3_ORIG_WHITE").is_ok() => "vec4<f32>(1.0)".to_string(),
            USAGE_COLOR => "a_color".to_string(),
            USAGE_BLEND_INDICES if key.skinned => "vec4<f32>(a_joint_index)".to_string(),
            // The game streams weights as a D3DCOLOR (bytes read back as
            // z, y, x, w) and indices as UBYTE4, so weight byte k pairs with
            // index byte k only after the same swizzle.
            USAGE_BLEND_WEIGHT if key.skinned => "a_joint_weight.zyxw".to_string(),
            // The tangent frame (see `TANGENT_INPUT`), packed like the normal.
            USAGE_PSIZE => "vec4<f32>(vec3<f32>(d3d_tangent.x, -d3d_tangent.z, d3d_tangent.y) * 0.5 + vec3<f32>(0.5), 1.0)".to_string(),
            USAGE_FOG => "vec4<f32>(vec3<f32>(d3d_binormal.x, -d3d_binormal.z, d3d_binormal.y) * 0.5 + vec3<f32>(0.5), 1.0)".to_string(),
            _ => "vec4<f32>(1.0)".to_string(),
        };
        vsrc += &format!("    {v} = {value};\n");
    }
    vsrc += "    d3d_main();\n    var out: GlueVarying;\n";
    let pos = vs.outputs.iter().find(|o| o.location.is_none()).ok_or_else(|| anyhow::anyhow!("vertex shader writes no position"))?;
    // Bevy's own clip position for the point, worked out as its depth prepass
    // and shadow passes do, so a surface lands on exactly the depth the
    // prepass laid down (the original shader's position differs by a hair,
    // which failed the depth test across whole walls once SSAO's prepass was
    // on). The game's clip depth also differed from Bevy's by more than a
    // decal's offset, which hid every decal drawn by the standard material
    // against these walls. `FC3_ORIG_POS` keeps the original shader's x, y
    // and w (with any vertex animation) with Bevy's depth, as before.
    if std::env::var("FC3_ORIG_POS").is_ok() && !key.terrain && !key.skinned {
        vsrc += "    let bevy_clip = view_transformations::position_world_to_clip((world_from_local * vec4<f32>(a_position, 1.0)).xyz);\n";
        vsrc += &format!("    out.position = vec4<f32>(o{0}.xy, bevy_clip.z / bevy_clip.w * o{0}.w, o{0}.w);\n", pos.register);
    } else {
        vsrc += "    out.position = view_transformations::position_world_to_clip(mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(a_position, 1.0)).xyz);\n";
    }
    for (_, vs_reg, ps_reg) in &varyings {
        vsrc += &format!("    out.v{ps_reg} = o{vs_reg};\n");
    }
    vsrc += "    out.bevy_world = world_from_local * vec4<f32>(a_position, 1.0);\n";
    vsrc += "    out.bevy_normal = vec4<f32>(normalize((world_from_local * vec4<f32>(a_normal, 0.0)).xyz), 0.0);\n";
    if needs_uv {
        vsrc += "    out.bevy_uv = a_uv;\n";
    }
    vsrc += "    out.instance = instance;\n    return out;\n}\n";

    // Fragment module.
    let mut fsrc = String::from(
        "#import bevy_pbr::{pbr_types, pbr_functions, mesh_bindings::mesh}\n#import fc3::horizon::{remove_hidden_sun, atlas_uv, beyond_cascades}\n",
    );
    fsrc += "@group(#{MATERIAL_BIND_GROUP}) @binding(48) var d3d_horizon: texture_2d<f32>;\n@group(#{MATERIAL_BIND_GROUP}) @binding(49) var d3d_horizon_sampler: sampler;\n";
    if needs_uv {
        fsrc += "@group(#{MATERIAL_BIND_GROUP}) @binding(50) var d3d_cutout: texture_2d<f32>;\n@group(#{MATERIAL_BIND_GROUP}) @binding(51) var d3d_cutout_sampler: sampler;\n@group(#{MATERIAL_BIND_GROUP}) @binding(52) var<uniform> d3d_cutout_params: vec4<f32>;\n";
    }
    fsrc += SPACE_CONSTANTS;
    fsrc += &format!(
        "const NORMAL_FROM_TARGET: bool = {};\nconst ALPHA_TEST: bool = {};\nconst UNLIT: bool = {};\nconst BLENDED: bool = {};\n",
        key.normal, key.alpha_test, key.unlit, key.blended
    );
    let pbody = rebind(lib, ps, &lib.wgsl(ps)?, 0);
    let uses_vpos = pbody.contains("var<private> d3d_vpos");
    let uses_vface = pbody.contains("var<private> d3d_vface");
    fsrc += &pbody;
    fsrc += &fields;
    fsrc += "@fragment\nfn fragment(in: GlueVarying, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {\n";
    for i in &ps.inputs {
        if varyings.iter().any(|(_, _, r)| *r == i.register) {
            fsrc += &format!("    v{0} = in.v{0};\n", i.register);
        }
    }
    if uses_vpos {
        fsrc += "    d3d_vpos = vec4<f32>(in.position.xy - vec2<f32>(0.5), 0.0, 1.0);\n";
    }
    if uses_vface {
        fsrc += "    d3d_vface = vec4<f32>(select(-1.0, 1.0, front));\n";
    }
    fsrc += "    d3d_main();\n";
    // FC3_ORIG_DEBUG=0..3 shows a raw render target instead of lighting.
    if let Some(n) = std::env::var("FC3_ORIG_DEBUG").ok().and_then(|v| v.parse::<u32>().ok()) {
        // 10 + k shows pixel shader input v<k> (fractional part, to make UVs visible).
        let target = if n < 4 && ps.color_outputs & (1 << n) != 0 {
            format!("oC{n}")
        } else if n >= 10 && ps.inputs.iter().any(|i| i.register == n - 10) {
            format!("fract(v{})", n - 10)
        } else {
            "vec4<f32>(1.0, 0.0, 1.0, 1.0)".into()
        };
        fsrc += &format!("    return vec4<f32>({target}.rgb, 1.0);\n}}\n");
        return Ok((vsrc, fsrc));
    }
    if key.alpha_test {
        // Cut out by the diffuse map's alpha (see `OriginalMaterial::cutout`).
        fsrc += "    if textureSample(d3d_cutout, d3d_cutout_sampler, in.bevy_uv * d3d_cutout_params.xy).a < d3d_cutout_params.z {\n        discard;\n    }\n";
    }
    // Blended: covers what is behind by the diffuse map's alpha.
    fsrc += if key.blended {
        "    let coverage = textureSample(d3d_cutout, d3d_cutout_sampler, in.bevy_uv * d3d_cutout_params.xy).a;\n"
    } else {
        "    let coverage = 1.0;\n"
    };
    // Target 2, where the shader writes it: the material's gloss (red) and
    // specular reflectance (blue) for the game's light passes.
    fsrc += &format!("    let target2 = {};\n", if ps.color_outputs & (1 << 2) != 0 { "oC2" } else { "vec4<f32>(-1.0)" });
    fsrc += r#"
    var pbr = pbr_types::pbr_input_new();
    // The material's dye, when the shader doesn't apply it (c255.w = 1).
    let dye = select(vec3<f32>(1.0), d3d_c.f[255].rgb, d3d_c.f[255].w > 0.5);
    let albedo = clamp(oC0.rgb * dye, vec3<f32>(0.0), vec3<f32>(1.0));
    pbr.material.base_color = vec4<f32>(albedo * albedo, coverage);
    // The material's gloss and reflectance (c254, from its parameters; w = 1
    // when given), else a matte default; per pixel, as its shader works them
    // out from its maps for the game's light passes (target 2: gloss g as a
    // Blinn-Phong exponent of 2^(13 g), and the reflectance), where it does.
    let surface = d3d_c.f[254];
    var roughness = select(0.75, surface.x, surface.w > 0.5);
    var reflectance = select(0.3, surface.y, surface.w > 0.5);
    if surface.w > 0.5 && all(target2.xz >= vec2<f32>(0.0)) && all(target2.xz <= vec2<f32>(1.0)) {
        roughness = sqrt(sqrt(2.0 / (exp2(13.0 * target2.x) + 2.0)));
        reflectance = sqrt(target2.z / 0.16);
    }
    pbr.material.perceptual_roughness = roughness;
    pbr.material.reflectance = vec3<f32>(reflectance);
    // The game reflects only a fraction of the surroundings (its ambient
    // specular); the sun's highlights stay whole.
    pbr.specular_occlusion = select(1.0, surface.z, surface.w > 0.5);
    // Fogged like the standard materials (the flags start out empty).
    pbr.material.flags |= pbr_types::STANDARD_MATERIAL_FLAGS_FOG_ENABLED_BIT;
    if BLENDED {
        pbr.material.flags |= pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_BLEND;
    }
    // Target 1 normally holds the world normal packed as n * 0.5 + 0.5, often
    // scaled so its largest component is 1 (length 1 to sqrt(3)). Some shader
    // families store other data there, so the decoded normal is only used when
    // its length fits and it faces the same way as the surface.
    let packed = oC1.xyz * 2.0 - vec3<f32>(1.0);
    // Two-sided surfaces light their back faces from behind.
    let geometric = normalize(in.bevy_normal.xyz) * select(-1.0, 1.0, front);
    var n = geometric;
    let len = length(packed);
    if NORMAL_FROM_TARGET && len > 0.8 && len < 1.8 {
        let nd = packed / len;
        let candidate = normalize(vec3<f32>(nd.x, nd.z, -nd.y));
        if dot(candidate, geometric) > 0.2 {
            n = candidate;
        }
    }
    pbr.frag_coord = in.position;
    pbr.world_position = in.bevy_world;
    pbr.world_normal = n;
    pbr.N = n;
    pbr.V = pbr_functions::calculate_view(in.bevy_world, false);
    pbr.flags = mesh[in.instance].flags;
    var color = pbr.material.base_color;
    if !UNLIT {
        color = pbr_functions::apply_pbr_lighting(pbr);
        // Shadows of distant land beyond the shadow cascades.
        if beyond_cascades(in.bevy_world) {
            let horizon = textureSampleLevel(d3d_horizon, d3d_horizon_sampler, atlas_uv(in.bevy_world.xyz), 0.0).rg;
            color = remove_hidden_sun(color, pbr, horizon);
        }
    }
    color = pbr_functions::main_pass_post_lighting_processing(pbr, color);
    return color;
}
"#;
    Ok((vsrc, fsrc))
}

/// Creates the shader assets for pairs used by newly loaded materials.
fn create_pair_shaders(
    mut events: MessageReader<AssetEvent<OriginalMaterial>>,
    materials: Res<Assets<OriginalMaterial>>,
    mut shaders: ResMut<Assets<Shader>>,
    mut done: Local<HashSet<OriginalKey>>,
) {
    for ev in events.read() {
        let AssetEvent::Added { id } = ev else { continue };
        let Some(m) = materials.get(*id) else { continue };
        let key = m.key;
        if key.vs == 0 || !done.insert(key) {
            continue;
        }
        let Some(lib) = library() else { continue };
        match pair_sources(&lib, key) {
            Ok((vsrc, fsrc)) => {
                // FC3_ORIG_DUMP=<dir> writes the generated modules for inspection.
                if let Ok(dir) = std::env::var("FC3_ORIG_DUMP") {
                    let dir = std::path::Path::new(&dir);
                    let _ = std::fs::write(dir.join(format!("{:016x}_{:016x}.vs.wgsl", key.vs, key.ps)), &vsrc);
                    let _ = std::fs::write(dir.join(format!("{:016x}_{:016x}.ps.wgsl", key.vs, key.ps)), &fsrc);
                }
                let (vh, fh) = pair_handles(key);
                let _ = shaders.insert(vh.id(), Shader::from_wgsl(vsrc, format!("fc3://original/{:016x}_{:016x}.vs.wgsl", key.vs, key.ps)));
                let _ = shaders.insert(fh.id(), Shader::from_wgsl(fsrc, format!("fc3://original/{:016x}_{:016x}.ps.wgsl", key.vs, key.ps)));
            }
            Err(e) => warn!("original shader pair {:016x}/{:016x}: {e:#}", key.vs, key.ps),
        }
    }
}

// --------------------------------------------------------------- runtime ---

fn toggle_original_shaders(keys: Res<ButtonInput<KeyCode>>, mut settings: ResMut<OriginalShaders>, mut hud: ResMut<crate::hud::HudState>) {
    if keys.just_pressed(KeyCode::F4) {
        settings.enabled = !settings.enabled;
        hud.notify(if settings.enabled { "Original shaders on".to_string() } else { "Original shaders off".to_string() });
    }
}

/// Gives prop parts their original material (loaded from the same cached
/// material file as their standard one) while the setting is on.
#[allow(clippy::type_complexity)]
fn attach_swaps(
    settings: Res<OriginalShaders>,
    server: Res<AssetServer>,
    mut commands: Commands,
    parts: Query<
        (Entity, &MeshMaterial3d<StandardMaterial>, Has<SkinnedPart>),
        (Or<(With<PropPart>, With<SkinnedPart>)>, Without<OriginalSwap>, Without<crate::tattoo::NoOriginal>),
    >,
) {
    if !settings.enabled {
        return;
    }
    for (e, standard, skinned) in parts.iter().take(2000) {
        let Some(path) = server.get_path(&standard.0) else { continue };
        // Skinned parts load through their own source, which selects the
        // skinned vertex shader and vertex layout.
        let path = if skinned { path.clone_owned().with_source(crate::cache_assets::SKINNED_SOURCE) } else { path.clone_owned() };
        let original: Handle<OriginalMaterial> = server.load(path);
        commands.entity(e).insert(OriginalSwap { standard: standard.0.clone(), original });
    }
}

/// How near the eye (m) props use the original shaders; beyond, the standard
/// materials stand in. Each original material draws on its own (they don't
/// batch as the standard ones do), so drawing every prop with them halved the
/// frame rate round busy places, while far props are too small for the
/// difference to show. A few metres of hysteresis stop parts flickering.
const ORIGINAL_PROP_RANGE: f32 = 150.0;
const ORIGINAL_PROP_SLACK: f32 = 25.0;

/// Puts each prop part on the material matching the current setting and its
/// distance, once its original material has loaded (parts without a usable
/// pair stay standard). Checked a few times a second.
#[allow(clippy::type_complexity)]
fn apply_original_materials(
    settings: Res<OriginalShaders>,
    originals: Res<Assets<OriginalMaterial>>,
    mut commands: Commands,
    parts: Query<(Entity, &OriginalSwap, &GlobalTransform, Has<MeshMaterial3d<OriginalMaterial>>)>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    time: Res<Time<Real>>,
    mut next_stats: Local<f32>,
    mut next_check: Local<f32>,
) {
    if time.elapsed_secs() < *next_check && !settings.is_changed() {
        return;
    }
    *next_check = time.elapsed_secs() + 0.5;
    let eye = cameras.single().map(|c| c.translation()).unwrap_or(Vec3::splat(f32::MAX));
    // FC3_ORIG_RANGE=m (tests) overrides the range.
    let range = std::env::var("FC3_ORIG_RANGE").ok().and_then(|v| v.parse().ok()).unwrap_or(ORIGINAL_PROP_RANGE);
    // FC3_ORIG_STATS=1 logs coverage every few seconds.
    if std::env::var("FC3_ORIG_STATS").is_ok() && time.elapsed_secs() > *next_stats {
        *next_stats = time.elapsed_secs() + 5.0;
        let near = parts.iter().filter(|(_, _, t, _)| t.translation().distance(eye) < range).count();
        let usable =
            parts.iter().filter(|(_, s, t, _)| t.translation().distance(eye) < range && originals.get(&s.original).is_some_and(|m| m.key.vs != 0)).count();
        let on = parts.iter().filter(|(_, _, _, on)| *on).count();
        info!("original shaders: {on} prop parts on them; {usable} of the {near} within {range} m can be");
    }
    for (e, swap, t, on_original) in &parts {
        let usable = originals.get(&swap.original).is_some_and(|m| m.key.vs != 0);
        let reach = range + if on_original { ORIGINAL_PROP_SLACK } else { 0.0 };
        let want = settings.enabled && usable && t.translation().distance(eye) < reach;
        if want && !on_original {
            commands.entity(e).remove::<MeshMaterial3d<StandardMaterial>>().insert(MeshMaterial3d(swap.original.clone()));
        } else if !want && on_original {
            commands.entity(e).remove::<MeshMaterial3d<OriginalMaterial>>().insert(MeshMaterial3d(swap.standard.clone()));
        }
    }
}

/// Fills the engine-wide registers from the camera, in Dunia space.
fn update_globals(
    cameras: Query<(&GlobalTransform, &Projection, &Camera), With<PlayerCamera>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    settings: Res<OriginalShaders>,
    time: Res<Time>,
    weather: Res<crate::clouds::Weather>,
) {
    if !settings.enabled {
        return;
    }
    let Some(lib) = library() else { return };
    let Some(handle) = GLOBALS.get() else { return };
    let Ok((gt, projection, camera)) = cameras.single() else { return };
    let bevy_from_dunia = Mat4::from_cols(Vec4::X, Vec4::new(0.0, 0.0, -1.0, 0.0), Vec4::Y, Vec4::W);
    let clip_from_view = projection.get_clip_from_view();
    let view_from_world = gt.to_matrix().inverse() * bevy_from_dunia;
    let rotation_only = Mat4::from_quat(gt.rotation()).inverse() * bevy_from_dunia;
    let mut g = GlobalRegisters { f: [Vec4::ZERO; 256] };
    let mut put_matrix = |name: &str, m: Mat4| {
        if let Some(r) = lib.global_reg(name) {
            for i in 0..4 {
                if (r as usize + i) < 256 {
                    g.f[r as usize + i] = m.row(i);
                }
            }
        }
    };
    put_matrix(names::VIEW_PROJECTION, clip_from_view * view_from_world);
    put_matrix(names::VIEW_ROT_PROJECTION, clip_from_view * rotation_only);
    put_matrix(names::PROJECTION, clip_from_view);
    put_matrix(names::VIEW, view_from_world);
    put_matrix(names::INV_PROJECTION, clip_from_view.inverse());
    put_matrix(names::INV_PROJECTION_DEPTH, clip_from_view.inverse());
    put_matrix(names::INV_VIEW, view_from_world.inverse());
    let to_dunia = |v: Vec3| Vec3::new(v.x, -v.z, v.y);
    let fov = match projection {
        Projection::Perspective(p) => p.fov.to_degrees(),
        _ => 60.0,
    };
    let size = camera.logical_viewport_size().unwrap_or(Vec2::new(1280.0, 720.0));
    let mut put = |name: &str, v: Vec4| {
        if let Some(r) = lib.global_reg(name) {
            if (r as usize) < 256 {
                g.f[r as usize] = v;
            }
        }
    };
    put(names::CAMERA_POSITION, to_dunia(gt.translation()).extend(1.0));
    put(names::CAMERA_DIRECTION, to_dunia(gt.forward().into()).extend(fov));
    put(names::CAMERA_RIGHT, to_dunia(gt.right().into()).extend(0.0));
    put(names::CAMERA_UP, to_dunia(gt.up().into()).extend(0.0));
    put(names::VIEWPORT_SIZE, Vec4::new(size.x, size.y, 1.0 / size.x, 1.0 / size.y));
    put(names::CAMERA_DISTANCES, Vec4::new(0.1, 4000.0, 1.0 / 0.1, 1.0 / 4000.0));
    put(names::AMBIENT_MULTIPLIER, Vec4::ONE);
    put(names::BLOOM_ADAPTATION, Vec4::ONE);
    // Wind sway: four out-of-phase oscillators per horizontal axis (Dunia X, Y),
    // following the weather's wind; shaders weight them per vertex.
    let wind = weather.wind();
    let (dir, speed) = (Vec2::new(wind.x, -wind.z).normalize_or(Vec2::X), wind.length());
    let t = time.elapsed_secs_wrapped();
    let amp = 0.04 + speed * 0.025;
    let wave = |f: f32, p: f32| (t * f + p).sin() * 0.7 + (t * f * 2.3 + p * 1.7).sin() * 0.3;
    let sway = Vec4::new(wave(1.1, 0.0), wave(1.7, 1.3), wave(2.6, 2.1), wave(4.1, 0.7)) * amp + Vec4::splat(amp * 0.6);
    put(names::WIND_SIM_X, sway * dir.x);
    put(names::WIND_SIM_Y, sway * dir.y);
    if std::env::var("FC3_ORIG_DUMP").is_ok() {
        static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            for name in [names::VIEW_ROT_PROJECTION, names::VIEW_PROJECTION, names::CAMERA_POSITION, names::VIEWPORT_SIZE] {
                let r = lib.global_reg(name);
                info!("global {name} -> {r:?} = {:?}", r.map(|r| g.f[r as usize]));
            }
            for name in [names::WIND_SIM_X, names::WIND_SIM_Y] {
                let r = lib.global_reg(name);
                info!("global {name} -> {r:?} = {:?}", r.map(|r| g.f[r as usize]));
            }
        }
    }
    if let Some(mut buffer) = buffers.get_mut(handle) {
        buffer.set_data(g);
    }
}
