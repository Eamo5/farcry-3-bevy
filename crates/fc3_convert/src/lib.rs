//! One-time conversion of an installed Far Cry 3 into the Bevy-ready cache.
//!
//! Everything is derived from the user's own installation and written to a
//! local cache directory (see `fc3_formats::cache`). Conversion is parallel,
//! incremental (existing outputs are kept unless `force` is set) and every
//! file is written atomically, so an interrupted run can simply be resumed.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use fc3_formats::cache::{
    self, mat_flags, CachedEntity, CachedLod, CachedMaterial, CachedModel, CachedPart, CachedSector, CachedTerrain, Manifest, Placement, NO_MATERIAL_SET,
};
use fc3_formats::material::{self as fmat, TexRole};
use fc3_formats::mesh::{self as fmesh, MeshData};
use fc3_formats::terrain::{self as ft, SectorCoord};
use fc3_formats::world as fw;
use fc3_formats::Vfs;
use rayon::prelude::*;

pub const GB: u64 = 1024 * 1024 * 1024;

/// The untextured "grey placeholder" material some world meshes still reference.
const PLACEHOLDER_MATERIAL: &str = r"graphics\_materials\lpcantin-m-20110323153738.material.bin";
/// A real rock material used in place of the placeholder on natural rock meshes.
/// Mesh paths of volumes and blockout pieces that are never drawn.
const NOT_DRAWN: [&str; 5] = ["interiorvolume", "interriorvolume", "fakecollisionbox", r"\leveldesign\legokit\", r"\nature\caverns"];

/// Model names of collision shapes that are never drawn (they keep their
/// collision, unlike `NOT_DRAWN`).
const COLLISION_ONLY: [&str; 2] = ["invisibleplayercol", "collisiononly"];

/// The other grey placeholder (a debug checker), on some rock formations.
const CHECKER_PLACEHOLDER_MATERIAL: &str = r"graphics\_materials\lpcantin-m-2010032950932047.material.bin";
const ROCK_FALLBACK_MATERIAL: &str = r"graphics\_materials\njinnah-m-20120404173103.material.bin";
/// Stands in for the material of `ZMask*` slots: the surface inside a boat's
/// hull that only keeps the sea from being drawn there (never visible).
const WATER_MASK_MATERIAL: &str = r"fc3bevy\water_mask.material.bin";

#[derive(Clone, Debug)]
pub struct Options {
    pub data_dir: PathBuf,
    pub filelists: Option<PathBuf>,
    pub cache_root: PathBuf,
    pub world: String,
    pub force: bool,
    /// Refuse to start (and stop) if free space on the cache drive would fall below this.
    pub min_free_bytes: u64,
    /// Restrict terrain/sector conversion to these sectors (testing).
    pub only_sectors: Option<Vec<u32>>,
}

impl Options {
    pub fn new(data_dir: PathBuf, cache_root: PathBuf) -> Self {
        Options { data_dir, filelists: None, cache_root, world: "fc3_main".into(), force: false, min_free_bytes: 5 * GB, only_sectors: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Scan,
    Terrain,
    Sectors,
    Models,
    Materials,
    Textures,
    Animations,
    Audio,
    Shaders,
    Story,
    Finish,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Scan => "Scanning game data",
            Stage::Terrain => "Converting terrain",
            Stage::Sectors => "Converting world sectors",
            Stage::Models => "Converting models",
            Stage::Materials => "Converting materials",
            Stage::Textures => "Converting textures",
            Stage::Animations => "Extracting animations",
            Stage::Audio => "Decoding audio",
            Stage::Shaders => "Translating shaders",
            Stage::Story => "Gathering the story",
            Stage::Finish => "Finishing",
        }
    }

    /// Rough share of total conversion time, for a single overall progress bar.
    pub fn weight(self) -> (f32, f32) {
        match self {
            Stage::Scan => (0.0, 0.03),
            Stage::Terrain => (0.03, 0.25),
            Stage::Sectors => (0.25, 0.4),
            Stage::Models => (0.4, 0.75),
            Stage::Materials => (0.75, 0.78),
            Stage::Textures => (0.78, 0.9),
            Stage::Animations => (0.9, 0.92),
            Stage::Audio => (0.92, 0.97),
            Stage::Shaders => (0.97, 0.985),
            Stage::Story => (0.985, 0.99),
            Stage::Finish => (0.99, 1.0),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub stage: Stage,
    pub done: usize,
    pub total: usize,
    pub bytes_written: u64,
}

impl Progress {
    /// Overall completion in 0..1 across all stages.
    pub fn overall(&self) -> f32 {
        let (a, b) = self.stage.weight();
        let f = if self.total > 0 { self.done as f32 / self.total as f32 } else { 0.0 };
        a + (b - a) * f.clamp(0.0, 1.0)
    }
}

/// Shared state for reporting and cancellation.
pub struct Context_ {
    pub cancel: AtomicBool,
    done: AtomicUsize,
    bytes: AtomicU64,
    errors: Mutex<Vec<String>>,
}

impl Default for Context_ {
    fn default() -> Self {
        Context_ { cancel: AtomicBool::new(false), done: AtomicUsize::new(0), bytes: AtomicU64::new(0), errors: Mutex::new(Vec::new()) }
    }
}

pub type Reporter<'a> = &'a (dyn Fn(Progress) + Sync);

/// Free bytes on the volume holding `path` (or its nearest existing ancestor).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    let canon = std::fs::canonicalize(&p).ok()?;
    let canon = canon.to_string_lossy().trim_start_matches(r"\\?\").to_ascii_lowercase();
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| canon.starts_with(&d.mount_point().to_string_lossy().to_ascii_lowercase()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

/// Writes via a temp file + rename so readers never see partial files.
fn write_atomic(root: &Path, rel: &str, bytes: &[u8], ctx: &Context_) -> Result<()> {
    let path = root.join(rel);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("renaming {}", path.display()))?;
    ctx.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
    Ok(())
}

fn exists(root: &Path, rel: &str) -> bool {
    root.join(rel).exists()
}

/// A converted material plus the original texture paths it references.
struct MaterialInfo {
    cached: CachedMaterial,
    textures: Vec<String>,
    /// Usable texture parameters: name hash and original texture path.
    tex_paths: Vec<(u32, String)>,
}

struct Job<'a> {
    opts: &'a Options,
    vfs: &'a Vfs,
    ctx: &'a Context_,
    report: Reporter<'a>,
    /// Model path -> its own per-slot material paths (lowercase `.material.bin`).
    model_materials: Mutex<HashMap<String, Arc<Vec<String>>>>,
    materials: Mutex<HashMap<String, Arc<MaterialInfo>>>,
    /// Parsed original shader objects, by archive hash.
    shader_objects: Mutex<HashMap<u64, Option<Arc<fc3_formats::d3d9::ShaderObject>>>>,
    /// Average texture colours, by original path.
    texture_means: Mutex<HashMap<String, Option<[f32; 4]>>>,
    /// Model bounds (Bevy space), by original path.
    model_bounds: Mutex<HashMap<String, Option<([f32; 3], [f32; 3])>>>,
    /// The particle library's looping effect lights, by effect name.
    effect_lights: std::sync::OnceLock<HashMap<String, Vec<fc3_formats::lights::EffectLight>>>,
    /// Each mesh's nodes in model space (Bevy space), by lower-case name.
    node_positions: Mutex<HashMap<String, Arc<HashMap<String, [f32; 3]>>>>,
}

impl Job<'_> {
    /// A model's bounding box (Bevy space), read once per model.
    fn model_bounds(&self, path: &str) -> Option<([f32; 3], [f32; 3])> {
        if let Some(b) = self.model_bounds.lock().unwrap().get(path) {
            return *b;
        }
        let b = self.vfs.read(path).ok().and_then(|bytes| fmesh::decode(&bytes, 1).ok()).map(|m| (m.bbox_min, m.bbox_max));
        self.model_bounds.lock().unwrap().insert(path.to_string(), b);
        b
    }

    fn root(&self) -> &Path {
        &self.opts.cache_root
    }

    fn error(&self, msg: String) {
        let mut e = self.ctx.errors.lock().unwrap();
        if e.len() < 2000 {
            e.push(msg);
        }
    }

    fn should_write(&self, rel: &str) -> bool {
        self.opts.force || !exists(self.root(), rel)
    }

    /// Runs `f` over `items` in parallel with progress and periodic disk checks.
    fn run<T: Sync, R: Send>(&self, stage: Stage, items: &[T], f: impl Fn(&T) -> Option<R> + Sync + Send) -> Result<Vec<R>> {
        self.ctx.done.store(0, Ordering::Relaxed);
        let total = items.len();
        (self.report)(Progress { stage, done: 0, total, bytes_written: self.ctx.bytes.load(Ordering::Relaxed) });
        let low_disk = AtomicBool::new(false);
        let out: Vec<R> = items
            .par_iter()
            .filter_map(|item| {
                if self.ctx.cancel.load(Ordering::Relaxed) || low_disk.load(Ordering::Relaxed) {
                    return None;
                }
                let r = f(item);
                let done = self.ctx.done.fetch_add(1, Ordering::Relaxed) + 1;
                if done % 128 == 0 || done == total {
                    if done % 4096 == 0 && free_space(self.root()).is_some_and(|free| free < self.opts.min_free_bytes) {
                        low_disk.store(true, Ordering::Relaxed);
                    }
                    (self.report)(Progress { stage, done, total, bytes_written: self.ctx.bytes.load(Ordering::Relaxed) });
                }
                r
            })
            .collect();
        if low_disk.load(Ordering::Relaxed) {
            bail!("stopped: free space on the cache drive fell below {:.1} GB", self.opts.min_free_bytes as f64 / GB as f64);
        }
        if self.ctx.cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        Ok(out)
    }

    /// The model's own per-slot materials, with placeholder substitution applied.
    /// A model's slot materials as it names them (no placeholder fallback).
    fn model_materials_raw(&self, model: &str) -> Vec<String> {
        self.vfs
            .read(&model.to_ascii_lowercase())
            .ok()
            .and_then(|b| fmesh::read_material_refs(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|r| material_bin_path(&r.path))
            .collect()
    }

    fn model_materials(&self, model: &str) -> Arc<Vec<String>> {
        let key = model.to_ascii_lowercase();
        if let Some(v) = self.model_materials.lock().unwrap().get(&key) {
            return v.clone();
        }
        let mats: Vec<String> = self
            .vfs
            .read(&key)
            .ok()
            .and_then(|b| fmesh::read_material_refs(&b).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                if r.name.to_ascii_lowercase().starts_with("zmask") {
                    WATER_MASK_MATERIAL.to_string()
                } else {
                    substitute_placeholder(&key, &material_bin_path(&r.path))
                }
            })
            .collect();
        let v = Arc::new(mats);
        self.model_materials.lock().unwrap().insert(key, v.clone());
        v
    }

    fn shader_object(&self, hash: u64) -> Option<Arc<fc3_formats::d3d9::ShaderObject>> {
        if let Some(o) = self.shader_objects.lock().unwrap().get(&hash) {
            return o.clone();
        }
        let o = self.vfs.read_hash(hash).ok().and_then(|b| fc3_formats::d3d9::parse_object(&b).ok()).map(Arc::new);
        self.shader_objects.lock().unwrap().insert(hash, o.clone());
        o
    }

    fn effect_lights(&self) -> &HashMap<String, Vec<fc3_formats::lights::EffectLight>> {
        self.effect_lights.get_or_init(|| match self.vfs.read(fc3_formats::lights::PARTICLE_LIBRARY) {
            Ok(b) => fc3_formats::lights::effect_lights(&String::from_utf8_lossy(&b)),
            Err(_) => HashMap::new(),
        })
    }

    /// Where each node of a mesh sits in the model (Bevy space), by
    /// lower-case name (the bones effects play at).
    fn node_positions(&self, mesh: &str) -> Arc<HashMap<String, [f32; 3]>> {
        if let Some(n) = self.node_positions.lock().unwrap().get(mesh) {
            return n.clone();
        }
        use fc3_formats::anim::{quat_mul, quat_rotate, QUAT_IDENTITY};
        let nodes = self.vfs.read(mesh).ok().and_then(|b| fc3_formats::anim::parse_nodes(&b).ok()).unwrap_or_default();
        let mut placed: Vec<([f32; 4], [f32; 3])> = Vec::with_capacity(nodes.len());
        for (i, n) in nodes.iter().enumerate() {
            // Parents come first; one that doesn't counts as a root.
            let parent = usize::try_from(n.parent).ok().filter(|&p| p < i).map_or((QUAT_IDENTITY, [0.0; 3]), |p| placed[p]);
            let t = quat_rotate(parent.0, n.translation);
            placed.push((quat_mul(parent.0, n.rotation), [parent.1[0] + t[0], parent.1[1] + t[1], parent.1[2] + t[2]]));
        }
        let out = Arc::new(nodes.iter().zip(&placed).map(|(n, p)| (n.name.to_ascii_lowercase(), p.1)).collect());
        self.node_positions.lock().unwrap().insert(mesh.to_string(), Arc::clone(&out));
        out
    }

    fn texture_mean(&self, path: &str) -> Option<[f32; 4]> {
        if let Some(m) = self.texture_means.lock().unwrap().get(path) {
            return *m;
        }
        let m = self.vfs.read(path).ok().and_then(|b| fc3_formats::texture::parse(&b).ok().and_then(|x| fc3_formats::texture::mean_color(x.dds)));
        self.texture_means.lock().unwrap().insert(path.to_string(), m);
        m
    }

    /// Pairs a material with a validated original shader pair, with the
    /// engine's stand-ins for textures it leaves out if it needs them (the
    /// textures added: sampler parameter, game path).
    fn original_binding(&self, set: &fc3_formats::original::ShaderSet, info: &MaterialInfo) -> Option<(cache::OriginalBinding, Vec<(u32, String)>)> {
        // A material without textures can still pair, with what the engine
        // binds for all of them; the grey placeholder stays as it is, and
        // water and what isn't drawn don't take a pair.
        if info.cached.flags & (mat_flags::PLACEHOLDER | mat_flags::WATER | mat_flags::NO_DRAW) != 0 {
            return None;
        }
        let means: HashMap<u32, [f32; 4]> = info.tex_paths.iter().filter_map(|(h, p)| Some((*h, self.texture_mean(p)?))).collect();
        let sample = fc3_formats::original::MaterialSample { material: &info.cached, texture_means: &means };
        fc3_formats::original::pair_material_or_replaced(set, &sample, &|h| self.shader_object(h), 96, &|p| self.texture_mean(p))
    }

    fn material(&self, bin: &str) -> Arc<MaterialInfo> {
        if let Some(m) = self.materials.lock().unwrap().get(bin) {
            return m.clone();
        }
        let info = Arc::new(build_material(self, bin));
        self.materials.lock().unwrap().insert(bin.to_string(), info.clone());
        info
    }
}

/// Opens the game archives for conversion.
pub fn open_vfs(opts: &Options) -> Result<Arc<Vfs>> {
    Vfs::open_install(&opts.data_dir, opts.filelists.as_deref())
}

/// Rough output size, used for the free-space check before starting.
pub fn estimate_bytes(vfs: &Vfs) -> u64 {
    let mut total = 0u64;
    for (h, name) in &vfs.names {
        let mult = if name.ends_with(".xbg") {
            3
        } else if name.ends_with(".xbt") || name.ends_with(".sdat") || name.ends_with(".data.fcb") {
            1
        } else {
            0
        };
        if mult > 0 {
            if let Some(loc) = vfs.index.get(h) {
                total += loc.entry.size() as u64 * mult;
            }
        }
    }
    total
}

/// Total size of the files under `dir`.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

/// Converts everything the game needs. Returns the manifest written last.
pub fn convert(opts: &Options, ctx: &Context_, report: Reporter) -> Result<Manifest> {
    std::fs::create_dir_all(&opts.cache_root).with_context(|| format!("creating cache dir {}", opts.cache_root.display()))?;
    report(Progress { stage: Stage::Scan, done: 0, total: 1, bytes_written: 0 });
    let vfs = open_vfs(opts)?;
    // Files already in the cache are overwritten in place, so they don't count.
    let needed = estimate_bytes(&vfs).saturating_sub(dir_size(&opts.cache_root));
    if let Some(free) = free_space(&opts.cache_root) {
        if free < needed + opts.min_free_bytes {
            bail!(
                "not enough disk space on the cache drive: {:.1} GB free, conversion needs about {:.1} GB and must leave {:.1} GB free. \
                 Free some space or set FC3_CACHE to another location.",
                free as f64 / GB as f64,
                needed as f64 / GB as f64,
                opts.min_free_bytes as f64 / GB as f64
            );
        }
    }
    // A cache written by an older format version is rebuilt from scratch.
    let mut opts = opts.clone();
    if opts.cache_root.join(Manifest::FILE).exists() && Manifest::load(&opts.cache_root).is_err() {
        opts.force = true;
    }
    let opts = &opts;
    let job = Job {
        opts,
        vfs: &vfs,
        ctx,
        report,
        model_materials: Mutex::new(HashMap::new()),
        materials: Mutex::new(HashMap::new()),
        shader_objects: Mutex::new(HashMap::new()),
        texture_means: Mutex::new(HashMap::new()),
        model_bounds: Mutex::new(HashMap::new()),
        effect_lights: std::sync::OnceLock::new(),
        node_positions: Mutex::new(HashMap::new()),
    };

    // ---- terrain
    let sectors: Vec<u32> = match &opts.only_sectors {
        Some(s) => s.clone(),
        None => (0..(ft::WORLD_SECTORS * ft::WORLD_SECTORS) as u32).collect(),
    };
    let mut terrain_ids: Vec<u32> = job.run(Stage::Terrain, &sectors, |&id| match convert_terrain(&job, id) {
        Ok(r) => r,
        Err(e) => {
            job.error(format!("terrain {id}: {e:#}"));
            None
        }
    })?;
    let atlas_ids: BTreeSet<u32> = terrain_ids.iter().map(|&id| SectorCoord::from_id(id).atlas_coord().id()).collect();
    let atlas_ids: Vec<u32> = atlas_ids.into_iter().collect();
    job.run(Stage::Terrain, &atlas_ids, |&id| {
        let c = SectorCoord::from_id(id);
        for kind in ["diffuse", "color", "mask"] {
            let rel = cache::atlas_rel(id, kind);
            if job.should_write(&rel) {
                if let Err(e) = convert_texture_to(&job, &c.atlas_path(&opts.world, kind), &rel) {
                    job.error(format!("atlas {id} {kind}: {e:#}"));
                }
            }
        }
        Some(())
    })?;

    // The whole world's land, coarse, for beyond the streamed terrain.
    if opts.only_sectors.is_none() && job.should_write(cache::FAR_TERRAIN_REL) {
        let mut far = cache::FarTerrainBuilder::default();
        let mut atlases: HashMap<u32, Option<Vec<u8>>> = HashMap::new();
        let sectors: HashMap<u32, cache::CachedTerrain> = terrain_ids
            .iter()
            .filter_map(|&id| Some((id, cache::CachedTerrain::from_bytes(&std::fs::read(job.root().join(cache::terrain_rel(id))).ok()?).ok()?)))
            .collect();
        for id in island_sectors(&sectors) {
            let Some(t) = sectors.get(&id) else { continue };
            let coord = SectorCoord::from_id(id);
            let atlas = atlases
                .entry(coord.atlas_coord().id())
                .or_insert_with(|| {
                    job.vfs.read(&coord.atlas_path(&opts.world, "diffuse")).ok().and_then(|b| fc3_formats::texture::parse(&b).ok().map(|x| x.dds.to_vec()))
                })
                .clone();
            far.put(&t, |lx, ly| {
                let dds = atlas.as_deref()?;
                let size = fc3_formats::texture::dds_info(dds)?.width;
                let (row, col) = fc3_formats::vegetation::atlas_pixel(coord, lx, ly, size);
                fc3_formats::texture::bc1_texel(dds, col, row)
            });
        }
        if let Err(e) = write_atomic(job.root(), cache::FAR_TERRAIN_REL, &far.0.to_bytes(), job.ctx) {
            job.error(format!("far terrain: {e:#}"));
        }
    }

    // The sectors' horizon maps joined into one world-wide image (for far
    // trees and props).
    if opts.only_sectors.is_none() && job.should_write(cache::HORIZON_ATLAS_REL) {
        let mut atlas = cache::HorizonAtlas::new();
        let mut n = 0;
        for &id in &terrain_ids {
            if let Ok(b) = std::fs::read(job.root().join(cache::shadow_rel(id))) {
                n += usize::from(atlas.put_sector(id, &b));
            }
        }
        if n > 0 {
            if let Err(e) = write_atomic(job.root(), cache::HORIZON_ATLAS_REL, &atlas.to_bytes(), job.ctx) {
                job.error(format!("horizon atlas: {e:#}"));
            }
        }
    }

    // Inland water planes (rivers, lakes) from the sectors' headers.
    if opts.only_sectors.is_none() && job.should_write(cache::WATER_PLANES_REL) {
        let planes: Vec<(u32, f32)> = terrain_ids
            .par_iter()
            .filter_map(|&id| {
                let sdat = job.vfs.read(&SectorCoord::from_id(id).sdat_path(&opts.world)).ok()?;
                // The sea's own sectors are the ocean's.
                ft::water_plane(&sdat).filter(|&l| l > WATER_PLANE_MIN).map(|l| (id, l))
            })
            .collect();
        let mut planes = planes;
        planes.sort_by_key(|p| p.0);
        let text: String = planes.iter().map(|(id, l)| format!("{id} {l}\n")).collect();
        if let Err(e) = write_atomic(job.root(), cache::WATER_PLANES_REL, text.as_bytes(), job.ctx) {
            job.error(format!("water planes: {e:#}"));
        }
    }
    if let Err(e) = convert_terrain_layers(&job) {
        job.error(format!("terrain layers: {e:#}"));
    }
    convert_sky(&job);
    convert_fx(&job);
    if opts.only_sectors.is_none() {
        if let Err(e) = convert_world_map(&job, &atlas_ids) {
            job.error(format!("world map: {e:#}"));
        }
    }

    // ---- sectors
    let sector_results: Vec<SectorResult> = job.run(Stage::Sectors, &sectors, |&id| match convert_sector(&job, id) {
        Ok(r) => r,
        Err(e) => {
            job.error(format!("sector {id}: {e:#}"));
            None
        }
    })?;
    let mut model_paths: BTreeSet<String> = BTreeSet::new();
    let mut material_paths: BTreeSet<String> = BTreeSet::new();
    let mut object_ids = Vec::new();
    let mut outpost_markers = Vec::new();
    let mut lights = Vec::new();
    let mut anchors = Vec::new();
    let mut traversal = fc3_formats::traversal::Traversal::default();
    let mut sound_points: Vec<(u32, [f32; 3])> = Vec::new();
    let mut effects: Vec<(&'static str, [f32; 3])> = Vec::new();
    let mut guard_spots: Vec<(&'static str, [f32; 3], [f32; 4])> = Vec::new();
    let mut villager_spots: Vec<(&'static str, [f32; 3], [f32; 4])> = Vec::new();
    let mut splines: Vec<(u32, fc3_formats::splines::SplinePrimitive)> = Vec::new();
    for r in sector_results {
        material_paths.extend(r.splines.iter().map(|s| s.material.clone()));
        splines.extend(r.splines.into_iter().map(|s| (r.id, s)));
        guard_spots.extend(r.guard_spots.iter().copied());
        villager_spots.extend(r.villager_spots.iter().copied());
        sound_points.extend(r.sound_points.iter().copied());
        effects.extend(r.effects.iter().copied());
        object_ids.push(r.id);
        model_paths.extend(r.models);
        material_paths.extend(r.override_materials);
        outpost_markers.extend(r.outpost_markers);
        lights.extend(r.lights);
        anchors.extend(r.anchors);
        traversal.ladders.extend(r.ladders);
        traversal.towers.extend(r.towers);
    }
    if opts.only_sectors.is_none() && job.should_write(cache::ENVIRONMENT_REL) {
        let env = job.vfs.read(fc3_formats::environment::MANAGERS_PATH).and_then(|b| fc3_formats::environment::Environment::parse(&b)).map(|mut e| {
            // The bloom's response, from the world's render settings.
            if let Ok(b) = job.vfs.read(fc3_formats::environment::WORLD_SETTINGS_PATH) {
                if let Some(c) = fc3_formats::environment::bloom_response(&b) {
                    e.bloom_response = c;
                }
                e.veg_sss = fc3_formats::environment::vegetation_sss(&b);
            }
            e
        });
        match env.and_then(|e| write_atomic(job.root(), cache::ENVIRONMENT_REL, e.to_text().as_bytes(), job.ctx)) {
            Ok(()) => {}
            Err(e) => job.error(format!("environment: {e:#}")),
        }
    }
    if opts.only_sectors.is_none() && job.should_write(cache::GRADE_REL) {
        match convert_grade(&job) {
            Ok(g) => {
                if let Err(e) = write_atomic(job.root(), cache::GRADE_REL, &g.to_bytes(), job.ctx) {
                    job.error(format!("grade: {e:#}"));
                }
            }
            Err(e) => job.error(format!("grade: {e:#}")),
        }
    }
    if opts.only_sectors.is_none() && job.should_write(cache::SPLINES_REL) {
        if let Err(e) = write_atomic(job.root(), cache::SPLINES_REL, &cache::splines_to_bytes(&splines), job.ctx) {
            job.error(format!("splines: {e:#}"));
        }
    }
    if opts.only_sectors.is_none() && job.should_write(cache::TRAVERSAL_REL) {
        traversal.cables = resolve_cables(&anchors);
        let keep = |p: &[f32; 3]| !fc3_formats::worldmap::in_test_area(p[0], -p[2]);
        traversal.cables.retain(|c| keep(&c.a) && keep(&c.b));
        traversal.ladders.retain(|l| keep(&l.base));
        traversal.towers.retain(keep);
        if let Err(e) = write_atomic(job.root(), cache::TRAVERSAL_REL, traversal.to_text().as_bytes(), job.ctx) {
            job.error(format!("traversal: {e:#}"));
        }
    }
    if opts.only_sectors.is_none() && job.should_write(cache::GUARD_SPOTS_REL) {
        guard_spots.retain(|(_, p, _)| !fc3_formats::worldmap::in_test_area(p[0], -p[2]));
        let text: String = guard_spots.iter().map(|(k, p, q)| format!("{k} {} {} {} {} {} {} {}\n", p[0], p[1], p[2], q[0], q[1], q[2], q[3])).collect();
        if let Err(e) = write_atomic(job.root(), cache::GUARD_SPOTS_REL, text.as_bytes(), job.ctx) {
            job.error(format!("guard spots: {e:#}"));
        }
    }
    if opts.only_sectors.is_none() && job.should_write(cache::VILLAGER_SPOTS_REL) {
        let text: String = villager_spots.iter().map(|(k, p, q)| format!("{k} {} {} {} {} {} {} {}\n", p[0], p[1], p[2], q[0], q[1], q[2], q[3])).collect();
        if let Err(e) = write_atomic(job.root(), cache::VILLAGER_SPOTS_REL, text.as_bytes(), job.ctx) {
            job.error(format!("villager spots: {e:#}"));
        }
    }
    if opts.only_sectors.is_none() && job.should_write(cache::WORLD_FX_REL) {
        effects.retain(|(_, p)| !fc3_formats::worldmap::in_test_area(p[0], -p[2]));
        let text: String = effects.iter().map(|(k, p)| format!("{k} {} {} {}\n", p[0], p[1], p[2])).collect();
        if let Err(e) = write_atomic(job.root(), cache::WORLD_FX_REL, text.as_bytes(), job.ctx) {
            job.error(format!("effects: {e:#}"));
        }
    }
    if opts.only_sectors.is_none() && job.should_write(cache::SOUND_POINTS_REL) {
        sound_points.retain(|(_, p)| !fc3_formats::worldmap::in_test_area(p[0], -p[2]));
        let text: String = sound_points.iter().map(|(e, p)| format!("{e:08x} {} {} {}\n", p[0], p[1], p[2])).collect();
        if let Err(e) = write_atomic(job.root(), cache::SOUND_POINTS_REL, text.as_bytes(), job.ctx) {
            job.error(format!("sound points: {e:#}"));
        }
    }
    if opts.only_sectors.is_none() && job.should_write(cache::LIGHTS_REL) {
        lights.retain(|l: &fc3_formats::lights::WorldLight| !fc3_formats::worldmap::in_test_area(l.position[0], -l.position[2]));
        if let Err(e) = write_atomic(job.root(), cache::LIGHTS_REL, fc3_formats::lights::to_text(&lights).as_bytes(), job.ctx) {
            job.error(format!("lights: {e:#}"));
        }
    }
    // The lights' flicker, as the engine's code has it.
    if opts.only_sectors.is_none() && job.should_write(cache::FLICKER_REL) {
        let dll = std::fs::read(fc3_formats::engine::dll_path(&opts.data_dir)).ok();
        match dll.as_deref().and_then(fc3_formats::engine::Pe::parse).and_then(|pe| fc3_formats::engine::flicker_wave(&pe)) {
            Some(wave) => {
                if let Err(e) = write_atomic(job.root(), cache::FLICKER_REL, wave.to_text().as_bytes(), job.ctx) {
                    job.error(format!("flicker: {e:#}"));
                }
            }
            None => job.error("flicker: the engine's flicker routine wasn't found in FC3.dll (lights will burn steady)".to_string()),
        }
    }
    if opts.only_sectors.is_none() && job.should_write(cache::OUTPOSTS_REL) {
        outpost_markers.retain(|(_, p)| !fc3_formats::worldmap::in_test_area(p[0], -p[2]));
        let outposts = fc3_formats::outposts::cluster(&outpost_markers);
        if let Err(e) = write_atomic(job.root(), cache::OUTPOSTS_REL, fc3_formats::outposts::to_text(&outposts).as_bytes(), job.ctx) {
            job.error(format!("outposts: {e:#}"));
        }
    }
    // ---- vegetation (baked per-sector plant instances)
    let veg_lib = fc3_formats::vegetation::ClusterLibrary::load(&vfs).ok();
    let veg_results: Vec<SectorVegetation> = match &veg_lib {
        Some(lib) => job.run(Stage::Sectors, &sectors, |&id| match convert_vegetation(&job, lib, id) {
            Ok(r) => r,
            Err(e) => {
                job.error(format!("vegetation {id}: {e:#}"));
                None
            }
        })?,
        None => {
            job.error("vegetation: cluster resource library not found".into());
            Vec::new()
        }
    };
    let mut veg_ids = Vec::new();
    let mut forest = cache::VistaForest::default();
    let mut cards: HashMap<String, u16> = HashMap::new();
    for (id, models, vista) in veg_results {
        veg_ids.push(id);
        model_paths.extend(models);
        for (card, pos, scale) in vista {
            let next = forest.cards.len() as u16;
            let card = *cards.entry(card.clone()).or_insert_with(|| {
                forest.cards.push(cache::model_rel(&card));
                model_paths.insert(card);
                next
            });
            forest.trees.push(cache::VistaTree { pos, scale, card });
        }
    }
    veg_ids.sort_unstable();
    // Every tree's billboard on the islands, for the forest far away.
    if let Some(far) = std::fs::read(job.root().join(cache::FAR_TERRAIN_REL)).ok().and_then(|b| cache::FarTerrain::from_bytes(&b)) {
        forest.trees.retain(|t| far.on_islands(SectorCoord::containing(t.pos[0], -t.pos[2]).id()));
    }
    if opts.only_sectors.is_none() && job.should_write(cache::VISTA_REL) && !forest.trees.is_empty() {
        if let Err(e) = write_atomic(job.root(), cache::VISTA_REL, &forest.to_bytes(), job.ctx) {
            job.error(format!("vista: {e:#}"));
        }
    }

    // Every named model too: vegetation, weapons and characters are spawned by gameplay code.
    if opts.only_sectors.is_none() {
        for name in vfs.names.values() {
            if name.ends_with(".xbg") {
                model_paths.insert(name.clone());
            }
        }
    }
    let model_paths: Vec<String> = model_paths.into_iter().collect();

    // ---- models
    let model_materials: Vec<Vec<String>> = job.run(Stage::Models, &model_paths, |p| match convert_model(&job, p) {
        Ok(m) => Some(m),
        Err(e) => {
            job.error(format!("model {p}: {e:#}"));
            None
        }
    })?;
    for m in model_materials {
        material_paths.extend(m);
    }
    if opts.only_sectors.is_none() {
        if let Err(e) = convert_surfaces(&job, &model_paths) {
            job.error(format!("surfaces: {e:#}"));
        }
        if let Err(e) = convert_collision(&job, &model_paths) {
            job.error(format!("collision: {e:#}"));
        }
    }
    // Skins the animal models only get from their archetypes.
    material_paths.extend(cache::ANIMAL_SKINS.iter().map(|s| cache::animal_skin_path(s)));
    // Enemy outfits and the material variants they swap in.
    match convert_kits(&job) {
        Ok(m) => material_paths.extend(m),
        Err(e) => job.error(format!("kits: {e:#}")),
    }
    if opts.only_sectors.is_none() {
        if let Err(e) = convert_patrols(&job) {
            job.error(format!("patrols: {e:#}"));
        }
        convert_nav(&job)?;
        if job.should_write(cache::INJURY_CLIPS_REL) {
            let mut lines: Vec<&String> = job.vfs.names.values().filter(|n| cache::is_injury_clip(n)).collect();
            lines.sort();
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            if let Err(e) = write_atomic(job.root(), cache::INJURY_CLIPS_REL, text.as_bytes(), job.ctx) {
                job.error(format!("injury clips: {e:#}"));
            }
        }
        if job.should_write(cache::FP_CLIPS_REL) {
            let mut lines: Vec<String> = job
                .vfs
                .names
                .values()
                .filter(|n| n.ends_with(".mab"))
                .filter_map(|n| cache::fp_clip_key(n).map(|(folder, action)| format!("{folder} {action} {n}")))
                .collect();
            lines.sort();
            if let Err(e) = write_atomic(job.root(), cache::FP_CLIPS_REL, (lines.join("\n") + "\n").as_bytes(), job.ctx) {
                job.error(format!("fp clips: {e:#}"));
            }
        }
    }
    // Weapon paint jobs, and the painted materials they swap in.
    if opts.only_sectors.is_none() {
        match convert_paints(&job) {
            Ok(paths) => material_paths.extend(paths),
            Err(e) => job.error(format!("paints: {e:#}")),
        }
    }

    let material_paths: Vec<String> = material_paths.into_iter().collect();

    // ---- Domino scripts: the missions' and side activities' Lua graphs
    if job.should_write(fc3_formats::scripts::DOMINO_PACK_REL) {
        if let Err(e) = convert_scripts(&job) {
            job.error(format!("scripts: {e:#}"));
        }
    }

    // ---- the story: the mission system's data and the entities scripts use
    if opts.only_sectors.is_none() {
        if let Err(e) = convert_story(&job) {
            job.error(format!("story: {e:#}"));
        }
    }

    // ---- the single-player videos, kept as they are (the game decodes
    // the Bink itself)
    if opts.only_sectors.is_none() {
        let mut videos: Vec<String> = job.vfs.names.values().filter(|n| cache::is_story_video(n)).cloned().collect();
        videos.sort();
        for v in videos {
            let rel = cache::video_rel(&v);
            if job.should_write(&rel) {
                if let Err(e) = job.vfs.read(&v).and_then(|b| write_atomic(job.root(), &rel, &b, job.ctx)) {
                    job.error(format!("video {v}: {e:#}"));
                }
            }
        }
    }

    // ---- what the shop sells, as scripts name it
    if opts.only_sectors.is_none() && job.should_write(fc3_formats::story::SHOP_ITEMS_REL) {
        let items = std::fs::read(job.root().join("story/entitylibrary.fcb")).map_err(anyhow::Error::from).and_then(|b| {
            let at = b.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
            let library = fc3_formats::fcb::parse(&b[at..])?;
            Ok(fc3_formats::story::shop_items(&library))
        });
        match items {
            Ok(items) => {
                let text: String = items.iter().map(|(m, a, i)| format!("{m}\t{a}\t{i}\n")).collect();
                if let Err(e) = write_atomic(job.root(), fc3_formats::story::SHOP_ITEMS_REL, text.as_bytes(), job.ctx) {
                    job.error(format!("shop items: {e:#}"));
                }
            }
            Err(e) => job.error(format!("shop items: {e:#}")),
        }
    }

    // ---- weapon stock: authored item/category ids and prices
    if opts.only_sectors.is_none() && job.should_write(fc3_formats::shop::WEAPONS_REL) {
        let stock = (|| -> Result<_> {
            let items = job.vfs.read(fc3_formats::shop::ITEMS_LIB)?;
            let at = items.windows(4).position(|w| w == b"nbCF").context("no shopping FCB header")?;
            let items = fc3_formats::fcb::parse(&items[at..])?;
            let library = std::fs::read(job.root().join("story/entitylibrary.fcb"))?;
            let at = library.windows(4).position(|w| w == b"nbCF").context("no entity library FCB header")?;
            let library = fc3_formats::fcb::parse(&library[at..])?;
            Ok(fc3_formats::shop::weapons(&items, &library))
        })();
        match stock {
            Ok(stock) => {
                let text = fc3_formats::shop::to_text(&stock);
                if let Err(e) = write_atomic(job.root(), fc3_formats::shop::WEAPONS_REL, text.as_bytes(), job.ctx) {
                    job.error(format!("weapon shop: {e:#}"));
                }
            }
            Err(e) => job.error(format!("weapon shop: {e:#}")),
        }
    }

    // ---- authored crafting/loot metadata (runtime rules migrate separately)
    if opts.only_sectors.is_none() {
        let current = std::fs::read_to_string(job.root().join(fc3_formats::progression::RECIPES_REL))
            .map_err(anyhow::Error::from).and_then(|text| fc3_formats::progression::Crafting::from_text(&text)).is_ok();
        if job.should_write(fc3_formats::progression::RECIPES_REL) || !current {
            if let Err(e) = convert_crafting(&job) {
                job.error(format!("crafting metadata: {e:#}"));
            }
        }
    }

    // ---- the clips the story's contextual actions play
    if opts.only_sectors.is_none() && (job.should_write(fc3_formats::moves::CA_MOVES_REL) || job.should_write(fc3_formats::moves::MOVES_REL)) {
        if let Err(e) = convert_ca_moves(&job) {
            job.error(format!("contextual action moves: {e:#}"));
        }
    }

    // ---- faces: the rigs, which character uses which, and every facial clip
    if opts.only_sectors.is_none() && (job.should_write(fc3_formats::facial::PAK_REL) || job.should_write(fc3_formats::facial::DEFINITIONS_REL)) {
        if let Err(e) = convert_facial(&job) {
            job.error(format!("facial: {e:#}"));
        }
    }

    // ---- cinematics: every sequence, with its camera paths
    if opts.only_sectors.is_none() {
        if let Err(e) = convert_cinema(&job) {
            job.error(format!("cinema: {e:#}"));
        }
    }

    // ---- shaders: the original D3D9 programs translated to WGSL (before
    // materials, which are paired with them)
    if job.should_write(cache::SHADER_INDEX_REL) {
        if let Err(e) = convert_shaders(&job) {
            job.error(format!("shaders: {e:#}"));
        }
    }
    let shader_set = std::fs::read_to_string(opts.cache_root.join(cache::SHADER_INDEX_REL)).ok().map(|text| {
        let mut set = fc3_formats::original::ShaderSet::new(text.lines().filter(|l| !l.starts_with('#')).filter_map(cache::ShaderEntry::from_line).collect());
        // Each shader family's parameter defaults and texture tiling rules.
        for name in job.vfs.names.values().filter(|n| n.starts_with(fc3_formats::original::DESCRIPTOR_DIR) && n.ends_with(".xml")) {
            if let Ok(bytes) = job.vfs.read(name) {
                set.add_descriptor(&String::from_utf8_lossy(&bytes));
            }
        }
        set
    });

    // ---- materials
    let texture_lists: Vec<Vec<String>> = job.run(Stage::Materials, &material_paths, |bin| {
        let info = job.material(bin);
        let rel = cache::material_rel(bin);
        let mut textures = info.textures.clone();
        if job.should_write(&rel) {
            let mut cached = info.cached.clone();
            if let Some((binding, added)) = shader_set.as_ref().and_then(|set| job.original_binding(set, &info)) {
                for (h, path) in added {
                    // With its colour map unbound the game draws it black; so
                    // does the standard material, further off.
                    if path == fc3_formats::original::UNBOUND_TEXTURE && fc3_formats::original::is_colour_map(h) {
                        cached.base_color[..3].fill(0.0);
                    }
                    cached.textures.push((h, cache::texture_rel(&path)));
                    if !textures.contains(&path) {
                        textures.push(path);
                    }
                }
                cached.original = Some(binding);
            }
            if let Err(e) = write_atomic(job.root(), &rel, &cached.to_bytes(), job.ctx) {
                job.error(format!("material {bin}: {e:#}"));
            }
        }
        Some(textures)
    })?;
    let mut textures: BTreeSet<String> = BTreeSet::new();
    for t in texture_lists {
        textures.extend(t);
    }
    let textures: Vec<String> = textures.into_iter().collect();

    // ---- textures
    job.run(Stage::Textures, &textures, |p| {
        let rel = cache::texture_rel(p);
        if job.should_write(&rel) {
            if let Err(e) = convert_texture_to(&job, p, &rel) {
                job.error(format!("texture {p}: {e:#}"));
            }
        }
        Some(())
    })?;

    // ---- animations and skeletons (raw, parsed by the game with fc3_formats::anim)
    let mut anim_files: Vec<(String, String)> = vfs
        .names
        .values()
        .filter_map(|n| {
            if n.ends_with(".mab") {
                Some((n.clone(), cache::anim_rel(n)))
            } else if n.ends_with(".skeleton") {
                Some((n.clone(), cache::skeleton_rel(n)))
            } else {
                None
            }
        })
        .collect();
    anim_files.sort();
    job.run(Stage::Animations, &anim_files, |(src, rel)| {
        if job.should_write(rel) {
            match vfs.read(src) {
                Ok(bytes) => {
                    if let Err(e) = write_atomic(job.root(), rel, &bytes, job.ctx) {
                        job.error(format!("anim {src}: {e:#}"));
                    }
                }
                Err(e) => job.error(format!("anim {src}: {e:#}")),
            }
        }
        Some(())
    })?;
    // Long clips the game keeps in segments: the clip's own name lists them
    // (the game joins them as it loads the clip).
    let mut segmented: std::collections::BTreeMap<String, Vec<(u32, String)>> = Default::default();
    for n in vfs.names.values() {
        if let Some((base, i)) = fc3_formats::anim::segment_of(n) {
            segmented.entry(base).or_default().push((i, cache::anim_rel(n)));
        }
    }
    for (base, mut segs) in segmented {
        let rel = cache::anim_rel(&base);
        if vfs.exists(&base) || !job.should_write(&rel) {
            continue;
        }
        segs.sort();
        let text: String = std::iter::once(fc3_formats::anim::SEGMENTS_MAGIC.to_string()).chain(segs.into_iter().map(|s| s.1)).collect::<Vec<_>>().join("\n");
        if let Err(e) = write_atomic(job.root(), &rel, text.as_bytes(), job.ctx) {
            job.error(format!("anim {base}: {e:#}"));
        }
    }

    // ---- audio: every playable sound plus the event manifest the game resolves at runtime
    let graph = fc3_formats::audio::SoundGraph::build(&vfs);
    let sounds = graph.sounds();
    job.run(Stage::Audio, &sounds, |snd| {
        let rel = format!("audio/{}.{}", snd.file_stem(), snd.file_ext());
        if job.should_write(&rel) {
            match fc3_formats::audio::decode_to_file_bytes(&vfs, snd) {
                Ok((bytes, _ext)) => {
                    if let Err(e) = write_atomic(job.root(), &rel, &bytes, job.ctx) {
                        job.error(format!("sound {rel}: {e:#}"));
                    }
                }
                Err(e) => job.error(format!("sound {rel}: {e:#}")),
            }
        }
        Some(())
    })?;
    write_atomic(&opts.cache_root, "audio/manifest.txt", graph.manifest().as_bytes(), ctx)?;

    report(Progress { stage: Stage::Finish, done: 0, total: 1, bytes_written: ctx.bytes.load(Ordering::Relaxed) });
    terrain_ids.sort_unstable();
    object_ids.sort_unstable();
    let manifest = Manifest {
        version: cache::CACHE_VERSION,
        sectors_with_terrain: terrain_ids,
        sectors_with_objects: object_ids,
        models: model_paths.len() as u32,
        textures: textures.len() as u32,
        sectors_with_vegetation: veg_ids,
    };
    let log: String = ctx.errors.lock().unwrap().iter().map(|e| format!("{e}\n")).collect();
    let _ = std::fs::write(opts.cache_root.join("convert_errors.log"), log);
    write_atomic(&opts.cache_root, Manifest::FILE, &manifest.to_bytes(), ctx)?;
    report(Progress { stage: Stage::Finish, done: 1, total: 1, bytes_written: ctx.bytes.load(Ordering::Relaxed) });
    Ok(manifest)
}

pub fn error_count(ctx: &Context_) -> usize {
    ctx.errors.lock().unwrap().len()
}

// ---------------------------------------------------------------- stages ---

/// The sectors of the two main islands and the islets just off them: land
/// joined across at most two sectors of sea to a sector on either island.
/// (The world also holds walled-off areas far out to sea, for scripted
/// sequences, that the game never shows from the islands.)
fn island_sectors(sectors: &HashMap<u32, cache::CachedTerrain>) -> Vec<u32> {
    let land = |c: SectorCoord| c.valid() && sectors.get(&c.id()).is_some_and(|t| t.max_height > fc3_formats::worldmap::SEA_LEVEL);
    // A sector inland on each island (south: Amanaki; north: Badtown).
    let seeds = [SectorCoord::containing(-1300.0, -1600.0), SectorCoord::containing(2560.0, 2400.0)];
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut stack: Vec<SectorCoord> = seeds.into_iter().filter(|&c| land(c)).collect();
    if stack.is_empty() {
        return sectors.keys().copied().collect();
    }
    while let Some(c) = stack.pop() {
        if !seen.insert(c.id()) {
            continue;
        }
        for dy in -3..=3 {
            for dx in -3..=3 {
                let n = SectorCoord::new(c.x + dx, c.y + dy);
                if land(n) && !seen.contains(&n.id()) {
                    stack.push(n);
                }
            }
        }
    }
    // The sea sectors round the land too (shores, shallows).
    let shore: Vec<u32> = seen
        .iter()
        .flat_map(|&id| {
            let c = SectorCoord::from_id(id);
            (-2..=2).flat_map(move |dy| (-2..=2).map(move |dx| SectorCoord::new(c.x + dx, c.y + dy)))
        })
        .filter(|c| c.valid() && sectors.contains_key(&c.id()))
        .map(|c| c.id())
        .collect();
    seen.extend(shore);
    seen.into_iter().collect()
}

fn convert_terrain(job: &Job, id: u32) -> Result<Option<u32>> {
    let c = SectorCoord::from_id(id);
    let path = c.sdat_path(&job.opts.world);
    if !job.vfs.exists(&path) {
        return Ok(None);
    }
    let rel = cache::terrain_rel(id);
    if job.should_write(&rel) {
        let s = ft::parse_sdat(&job.vfs.read(&path)?)?;
        let t = CachedTerrain {
            sector_id: id,
            min_height: s.min_height,
            max_height: s.max_height,
            heights: s.heights.iter().map(|h| (h * 64.0).round().clamp(0.0, 65535.0) as u16).collect(),
            layers: s.layers,
            holes: s.holes.clone(),
        };
        write_atomic(job.root(), &rel, &t.to_bytes(), job.ctx)?;
    }
    let srel = cache::shadow_rel(id);
    if job.should_write(&srel) {
        let _ = convert_texture_to(job, &c.shadow_path(&job.opts.world), &srel);
    }
    Ok(Some(id))
}

/// Converts the world's terrain layer table and its tiling ground textures.
/// Bakes the world map (`map/world.dds`): each 2x2-sector atlas becomes a
/// 64-pixel tile shaded from its baked ground colour and the sectors' heights.
fn convert_world_map(job: &Job, atlas_ids: &[u32]) -> Result<()> {
    use fc3_formats::worldmap::{self as wm, MAP_SIZE, METRES_PER_PIXEL};
    if !job.should_write(cache::WORLD_MAP_REL) {
        return Ok(());
    }
    let tile = (2.0 * ft::SECTOR_SIZE / METRES_PER_PIXEL) as usize;
    let root = job.root();
    // The developers' test areas aren't part of the islands; the map shows sea there.
    let test_area = |c: SectorCoord| {
        let [x, y] = c.origin();
        wm::in_test_area(x, y)
    };
    let tiles: Vec<(SectorCoord, Vec<[u8; 3]>)> = atlas_ids
        .par_iter()
        .filter(|&&id| !test_area(SectorCoord::from_id(id)))
        .map(|&id| {
            let a = SectorCoord::from_id(id);
            let dds = std::fs::read(root.join(cache::atlas_rel(id, "diffuse"))).ok();
            let size = dds.as_deref().and_then(fc3_formats::texture::dds_info).map(|i| i.width).unwrap_or(0);
            let sectors: Vec<Option<ft::TerrainSector>> = [(0, 0), (1, 0), (0, 1), (1, 1)]
                .iter()
                .map(|&(dx, dy)| {
                    let c = SectorCoord::new(a.x + dx, a.y + dy);
                    let bytes = std::fs::read(root.join(cache::terrain_rel(c.id()))).ok()?;
                    Some(CachedTerrain::from_bytes(&bytes).ok()?.to_sector())
                })
                .collect();
            let mut out = vec![wm::OPEN_SEA; tile * tile];
            for ty in 0..tile {
                for tx in 0..tile {
                    let x = (tx as f32 + 0.5) * METRES_PER_PIXEL;
                    let y = 2.0 * ft::SECTOR_SIZE - (ty as f32 + 0.5) * METRES_PER_PIXEL;
                    let (qx, qy) = ((x / ft::SECTOR_SIZE) as usize, (y / ft::SECTOR_SIZE) as usize);
                    let Some(s) = &sectors[qy.min(1) * 2 + qx.min(1)] else {
                        continue;
                    };
                    let (lx, ly) = (x - qx as f32 * ft::SECTOR_SIZE, y - qy as f32 * ft::SECTOR_SIZE);
                    let h = s.sample(lx, ly);
                    let (dx, dy) = (s.sample(lx + 1.0, ly) - s.sample(lx - 1.0, ly), s.sample(lx, ly + 1.0) - s.sample(lx, ly - 1.0));
                    let n = [-dx / 2.0, -dy / 2.0, 1.0];
                    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                    let coord = SectorCoord::new(a.x + qx as i32, a.y + qy as i32);
                    let ground = dds
                        .as_deref()
                        .filter(|_| size > 0)
                        .and_then(|d| {
                            let (row, col) = fc3_formats::vegetation::atlas_pixel(coord, lx, ly, size);
                            fc3_formats::texture::bc1_texel(d, col, row)
                        })
                        .unwrap_or([96, 110, 64]);
                    out[ty * tile + tx] = wm::shade(ground, h, n.map(|v| v / len));
                }
            }
            (a, out)
        })
        .collect();
    let size = MAP_SIZE as usize;
    let sea = wm::OPEN_SEA;
    let mut rgba: Vec<u8> = std::iter::repeat_n([sea[0], sea[1], sea[2], 255], size * size).flatten().collect();
    for (a, pixels) in tiles {
        let px0 = (a.x as usize / 2) * tile;
        // Atlas rows run south to north; the map is north up.
        let Some(py0) = size.checked_sub((a.y as usize / 2 + 1) * tile) else {
            continue;
        };
        for ty in 0..tile {
            for tx in 0..tile {
                let (px, py) = (px0 + tx, py0 + ty);
                if px < size && py < size {
                    let c = pixels[ty * tile + tx];
                    rgba[(py * size + px) * 4..(py * size + px) * 4 + 3].copy_from_slice(&c);
                }
            }
        }
    }
    write_atomic(root, cache::WORLD_MAP_REL, &wm::encode_dds(&rgba, MAP_SIZE, MAP_SIZE), job.ctx)
}

/// Noise texture the engine binds for terrain shaders.
const PERLIN_TEXTURE: &str = r"graphics\__fc3_graphics\_common\_textures\generic\mask\perlin.xbt";
const VISTA_NOISE_TEXTURE: &str = r"graphics\__fc3_graphics\_common\terrain\vistadetailmap\terrain_detail_01.xbt";
const WATER_RIPPLES: &str = r"graphics\__fc3_graphics\_common\_textures\terrain\water\oceanwater_01_n.xbt";
const WATER_SWELL: &str = r"graphics\__fc3_graphics\_common\_textures\generic\water\oceanmediumwaves_03_n.xbt";
const WATER_FOAM: &str = r"graphics\__fc3_graphics\singleplayer\_textures\biomes\water\waterfoam_01.xbt";

const SKY_CLOUDS: [&str; 5] = [
    r"graphics\__fc3_graphics\singleplayer\_textures\skybox\clouds\fc3_cloud_001_clear.xbt",
    r"graphics\__fc3_graphics\singleplayer\_textures\skybox\clouds\fc3_cloud_002_medium.xbt",
    r"graphics\__fc3_graphics\singleplayer\_textures\skybox\clouds\fc3_cloud_003_dense.xbt",
    r"graphics\__fc3_graphics\singleplayer\_textures\skybox\clouds\fc3_cloud_004_cumulonibus.xbt",
    r"graphics\__fc3_graphics\singleplayer\_textures\skybox\clouds\fc3_cloud_005_storm.xbt",
];

const SKY_MOON: &str = r"graphics\__fc3_graphics\singleplayer\_textures\skybox\night\fc3_moon.xbt";

/// Effect textures (see `cache::FX_TEXTURES`), each converted when missing.
fn convert_fx(job: &Job) {
    for stem in cache::FX_TEXTURES {
        let rel = cache::fx_rel(stem);
        if !job.should_write(&rel) {
            continue;
        }
        // Decal atlases live in their own folder.
        let dir = if stem.contains("decal") || stem.contains("blood") {
            "decal\\"
        } else if *stem == "lens_flare" {
            "lens_flare\\"
        } else {
            ""
        };
        let src = format!("graphics\\__fc3_graphics\\_common\\_textures\\fx\\{dir}{stem}.xbt");
        if let Err(e) = convert_texture_to(job, &src, &rel) {
            job.error(format!("fx {rel}: {e:#}"));
        }
    }
}

/// Vehicle patrol routes: each patrol marker with the path shape it names
/// (shapes live in both world and meta sectors).
/// The world's navigation meshes, one cache file per sector that has any.
fn convert_nav(job: &Job) -> Result<()> {
    let prefix = format!(r"worlds\{}\levels\", job.opts.world);
    let files: Vec<(u32, u64)> = job
        .vfs
        .names
        .iter()
        .filter(|(_, n)| n.starts_with(&prefix) && n.ends_with(".rnv") && n.contains(r"\nv\sector"))
        .filter_map(|(h, n)| {
            let id = n.rsplit("sector").next()?.strip_suffix(".rnv")?.parse::<u32>().ok()?;
            Some((id, *h))
        })
        .filter(|(id, _)| job.should_write(&cache::nav_rel(*id)))
        .collect();
    job.run(Stage::Sectors, &files, |&(id, hash)| {
        let bytes = job.vfs.read_hash(hash).ok()?;
        let tiles = fc3_formats::nav::parse_rnv(&bytes);
        if tiles.iter().all(|t| t.polys.is_empty()) {
            return Some(());
        }
        let origin = fc3_formats::terrain::SectorCoord::from_id(id).origin();
        let nav = fc3_formats::nav::build_sector(&tiles, origin, fc3_formats::terrain::SECTOR_SIZE);
        if let Err(e) = write_atomic(job.root(), &cache::nav_rel(id), &nav.to_bytes(), job.ctx) {
            job.error(format!("nav {id}: {e:#}"));
        }
        Some(())
    })?;
    Ok(())
}

fn convert_patrols(job: &Job) -> Result<()> {
    use fc3_formats::patrols;
    if !job.should_write(cache::PATROLS_REL) {
        return Ok(());
    }
    let prefix = format!("worlds\\{}\\", job.opts.world);
    let files: Vec<u64> = job.vfs.names.iter().filter(|(_, n)| n.starts_with(&prefix) && n.ends_with(".data.fcb")).map(|(h, _)| *h).collect();
    let mut shapes: HashMap<u64, Vec<[f32; 3]>> = HashMap::new();
    let mut markers: Vec<Vec<u64>> = Vec::new();
    for h in files {
        let Ok(bytes) = job.vfs.read_hash(h) else {
            continue;
        };
        let Ok(root) = fc3_formats::fcb::parse(&bytes) else {
            continue;
        };
        root.walk(&mut |o, _| {
            if !o.is("Entity") {
                return;
            }
            let Some(name) = o.str("hidName") else { return };
            if patrols::is_path_shape(&name) {
                if let (Some(id), Some(blob)) = (o.u64("disEntityId"), o.field_h(patrols::SHAPE_POINTS)) {
                    let points = patrols::shape_points(blob);
                    if points.len() >= 2 {
                        shapes.insert(id, points);
                    }
                }
            } else if patrols::is_patrol_marker(&name) {
                markers.push(patrols::u64_values(o));
            }
        });
    }
    let mut used = std::collections::HashSet::new();
    let routes: Vec<patrols::PatrolRoute> = markers
        .iter()
        .filter_map(|ids| ids.iter().find(|id| shapes.contains_key(id)))
        .filter(|id| used.insert(**id))
        .filter_map(|id| patrols::route(shapes[id].iter().map(|p| fw::pos_to_bevy(*p)).collect(), 15.0))
        .filter(|r| !fc3_formats::worldmap::in_test_area(r.points[0][0], -r.points[0][2]))
        .collect();
    write_atomic(job.root(), cache::PATROLS_REL, patrols::to_text(&routes).as_bytes(), job.ctx)?;
    Ok(())
}

/// The generic enemies' graphic kits, written as text; returns every material
/// they can swap in.
fn convert_kits(job: &Job) -> Result<Vec<String>> {
    let list = job.vfs.read(r"graphics\characters\graphickitlist.xml")?;
    let list = fc3_formats::kits::parse_kit_list(&String::from_utf8_lossy(&list));
    let mut kits = Vec::new();
    for want in cache::GENERIC_KITS {
        let Some((_, file)) = list.iter().find(|(n, _)| n.eq_ignore_ascii_case(want)) else {
            job.error(format!("kit {want}: not in the kit list"));
            continue;
        };
        let xml = job.vfs.read(file).with_context(|| format!("kit {want}"))?;
        kits.push(fc3_formats::kits::parse_kit(&String::from_utf8_lossy(&xml))?);
    }
    if job.should_write(cache::KITS_REL) {
        write_atomic(job.root(), cache::KITS_REL, fc3_formats::kits::to_text(&kits).as_bytes(), job.ctx)?;
    }
    // Every kit, for the story's characters.
    let mut all = Vec::new();
    for (name, file) in &list {
        match job.vfs.read(file).map_err(anyhow::Error::from).and_then(|xml| fc3_formats::kits::parse_kit(&String::from_utf8_lossy(&xml))) {
            Ok(mut k) => {
                k.id = fc3_formats::kits::id_from_file(file);
                all.push(k);
            }
            Err(e) => job.error(format!("kit {name}: {e:#}")),
        }
    }
    if job.should_write(cache::ALL_KITS_REL) {
        write_atomic(job.root(), cache::ALL_KITS_REL, fc3_formats::kits::to_text(&all).as_bytes(), job.ctx)?;
    }
    kits.extend(all);
    Ok(kits.iter().flat_map(|k| &k.slots).flat_map(|s| &s.parts).flat_map(|p| &p.swaps).flat_map(|s| s.choices.iter().map(|c| c.material.clone())).collect())
}

/// Sky textures, each converted when missing.
fn convert_sky(job: &Job) {
    for (src, rel) in SKY_CLOUDS.iter().chain([&SKY_MOON]).zip(cache::SKY_CLOUDS_REL.into_iter().chain([cache::SKY_MOON_REL])) {
        if job.should_write(rel) {
            if let Err(e) = convert_texture_to(job, src, rel) {
                job.error(format!("sky {rel}: {e:#}"));
            }
        }
    }
}

/// Footstep and impact surfaces (`world/surfaces.bin`): the sound switch
/// value of each logic material, the terrain layers' surface types and, for
/// models with a collision file, what their walkable area and walls are made of.
fn convert_surfaces(job: &Job, model_paths: &[String]) -> Result<()> {
    use fc3_formats::havok;
    // `Base/Bark_P` and `Base/Vege_P_Bush` in logicmaterials.lib.
    const LOGIC_BARK: u8 = 5;
    const LOGIC_BUSH: u8 = 20;
    if !job.should_write(cache::SURFACES_REL) {
        return Ok(());
    }
    let world = &job.opts.world;
    let switch_values = havok::logic_switch_values(&job.vfs.read(havok::LOGIC_MATERIALS)?)?;
    let game_xml = job.vfs.read(&format!(r"worlds\{world}\generated\{world}.game.xml"))?;
    let layers = ft::parse_layers(&game_xml)?.iter().map(|l| l.surface).collect();
    let to_bevy = |v: [f32; 3]| [v[0], v[2], -v[1]];
    let mut models: Vec<cache::ModelSurfaces> = model_paths
        .par_iter()
        .filter_map(|p| {
            let lower = p.to_ascii_lowercase();
            // RealTree trees and bushes have no collision file: bark, or bush.
            if lower.ends_with(".rtx") {
                let logic = if lower.contains("bush") { LOGIC_BUSH } else { LOGIC_BARK };
                return Some(cache::ModelSurfaces { model: fc3_formats::hash::path_hash(p), walk: logic, wall: logic, patches: Vec::new() });
            }
            let stem = lower.strip_suffix(".xbg")?.to_string();
            let tris = havok::surface_triangles(&job.vfs.read(&format!("{stem}.hkx")).ok()?).ok()?;
            // Characters walk on the coarse body; shots hit the detailed one.
            let (detail, coarse): (Vec<_>, Vec<_>) = tris.iter().partition(|t| t.detail);
            let walk_tris = if coarse.is_empty() { &detail } else { &coarse };
            let wall = havok::surface_areas(if detail.is_empty() { &coarse } else { &detail }, false).first()?.0;
            let walk = havok::surface_areas(walk_tris, true).first().map_or(wall, |w| w.0);
            let patches = walk_tris.iter().filter(|t| t.logic != walk && havok::walkable(t)).map(|t| (t.corners.map(to_bevy), t.logic)).collect();
            Some(cache::ModelSurfaces { model: fc3_formats::hash::path_hash(p), walk, wall, patches })
        })
        .collect();
    models.sort_by_key(|m| m.model);
    let table = cache::SurfaceTable { switch_values, layers, models };
    write_atomic(job.root(), cache::SURFACES_REL, &table.to_bytes(), job.ctx)
}

/// The game's own collision for every model that ships one (`<model>.hkx`,
/// read by `havok::collision_geometry`): `collision/<hash>.col` in Bevy
/// space per model, and the index of the models that have one. Runs whenever
/// the index is missing; files already there are kept. Models whose `.hkx`
/// has no usable body keep their render-derived collision.
fn convert_collision(job: &Job, model_paths: &[String]) -> Result<()> {
    if !job.should_write(cache::COLLISION_INDEX_REL) {
        return Ok(());
    }
    use fc3_formats::havok;
    // The logic material that leaves no impact, by name.
    let names = havok::logic_material_names(&job.vfs.read(havok::LOGIC_MATERIALS)?)?;
    let no_impact = names.iter().position(|n| n.to_ascii_lowercase().ends_with("noimpact")).map(|i| i as u32);
    let models: Vec<&String> = model_paths.iter().filter(|p| p.ends_with(".xbg")).collect();
    let mut have: Vec<String> = job.run(Stage::Models, &models, |p| {
        let hkx = job.vfs.read(&format!("{}.hkx", p.strip_suffix(".xbg")?)).ok()?;
        let rel = cache::collision_rel(p);
        if !job.should_write(&rel) {
            return Some(cache::hash_name(p));
        }
        let geometry = match havok::collision_geometry(&hkx) {
            Ok(g) if !g.triangles.is_empty() => g,
            Ok(_) => return None,
            Err(e) => {
                job.error(format!("collision {p}: {e:#}"));
                return None;
            }
        };
        let cached = cache::CachedCollision {
            positions: geometry.positions.iter().map(|&v| fmesh::to_bevy(v)).collect(),
            indices: geometry.triangles.iter().flatten().copied().collect(),
            no_impact: !geometry.palette.is_empty() && geometry.palette.iter().all(|&l| Some(l) == no_impact),
        };
        match write_atomic(job.root(), &rel, &cached.to_bytes(), job.ctx) {
            Ok(()) => Some(cache::hash_name(p)),
            Err(e) => {
                job.error(format!("collision {p}: {e:#}"));
                None
            }
        }
    })?;
    have.sort_unstable();
    have.dedup();
    let text: String = have.iter().map(|h| format!("{h}\n")).collect();
    write_atomic(job.root(), cache::COLLISION_INDEX_REL, text.as_bytes(), job.ctx)
}

/// Water planes at or below this height (sea level plus a little) are the
/// ocean's.
const WATER_PLANE_MIN: f32 = 100.5;

fn convert_terrain_layers(job: &Job) -> Result<()> {
    if !job.should_write(cache::TERRAIN_LAYERS_REL) {
        return Ok(());
    }
    let world = &job.opts.world;
    let game_xml = job.vfs.read(&format!(r"worlds\{world}\generated\{world}.game.xml"))?;
    let layers = ft::parse_layers(&game_xml)?;
    let textures: BTreeSet<&str> = layers.iter().flat_map(|l| [l.diffuse.as_str(), l.normal.as_str()]).filter(|t| !t.is_empty()).collect();
    let converted: HashSet<&str> = textures
        .par_iter()
        .filter(|&&t| {
            let rel = cache::texture_rel(t);
            !job.should_write(&rel) || convert_texture_to(job, t, &rel).is_ok()
        })
        .copied()
        .collect();
    let rel_if = |t: &str| {
        if converted.contains(t) {
            cache::texture_rel(t)
        } else {
            String::new()
        }
    };
    let table = cache::TerrainLayerTable {
        layers: layers
            .iter()
            .map(|l| cache::CachedTerrainLayer {
                diffuse: rel_if(&l.diffuse),
                normal: rel_if(&l.normal),
                tiling: l.tiling,
                axis: l.axis,
                name: l.name.replace(char::is_whitespace, "_"),
                normal_tiling: l.normal_tiling,
                noise_level: l.noise_level,
                glossiness: l.glossiness,
            })
            .collect(),
        noise_transition: ft::parse_noise_transition(&game_xml),
    };
    // Engine noise textures read by the original terrain shaders, and the water textures.
    for (src, rel) in [
        (PERLIN_TEXTURE, cache::TERRAIN_PERLIN_REL),
        (VISTA_NOISE_TEXTURE, cache::TERRAIN_VISTA_NOISE_REL),
        (WATER_RIPPLES, cache::WATER_RIPPLES_REL),
        (WATER_SWELL, cache::WATER_SWELL_REL),
        (WATER_FOAM, cache::WATER_FOAM_REL),
    ] {
        if let Err(e) = convert_texture_to(job, src, rel) {
            job.error(format!("terrain noise {rel}: {e:#}"));
        }
    }
    write_atomic(job.root(), cache::TERRAIN_LAYERS_REL, table.to_text().as_bytes(), job.ctx)
}

/// Translates every D3D9 shader object to WGSL, validates it the way wgpu
/// does, and writes the compressed modules plus an index.
/// Copies the generated files the mission system reads, and gathers the
/// entities the story needs from every sector and metasector (all layers):
/// those on mission layers, those with a script, trigger, zone, scene or
/// marker, and those scripts name.
fn convert_story(job: &Job) -> Result<()> {
    use fc3_formats::story;
    for (src, rel) in story::RAW_FILES {
        if job.should_write(rel) {
            match job.vfs.read(src) {
                Ok(b) => write_atomic(job.root(), rel, &b, job.ctx)?,
                Err(e) => job.error(format!("story: {src}: {e:#}")),
            }
        }
    }
    let current = std::fs::read(job.root().join(story::ENTITIES_REL)).is_ok_and(|b| story::is_current(&b));
    if current && !job.opts.force {
        return Ok(());
    }
    // Every packed script (the scripts stage runs first).
    let pack = std::fs::read(job.root().join(fc3_formats::scripts::DOMINO_PACK_REL)).context("the script pack")?;
    let pack = fc3_formats::scripts::ScriptPack::from_bytes(&pack)?;
    let named: HashSet<u64> = pack.paths().filter_map(|p| pack.get(p)).flat_map(|t| story::script_entity_ids(&t)).collect();
    let prefix = format!(r"worlds\{}\levels\", job.opts.world);
    let mut files: Vec<String> = job
        .vfs
        .names
        .values()
        .filter(|n| n.starts_with(&prefix) && n.ends_with(".data.fcb") && (n.contains(r"\worldsectors\") || n.contains(r"\metasectors\")))
        .cloned()
        .collect();
    files.sort();
    let lists = job.run(Stage::Story, &files, |path| {
        let bytes = job.vfs.read(path).ok()?;
        let root = fc3_formats::fcb::parse(&bytes).ok()?;
        let id = root.u32("Id").unwrap_or(0);
        let source = if path.contains(r"\metasectors\") { story::Source::Metasector(id) } else { story::Source::Sector(id) };
        let mut out = Vec::new();
        let entries = fw::layered_entities(&root);
        let prefab_children: HashSet<u64> = entries.iter().filter_map(|(_, e)| e.child("Children"))
            .flat_map(|children| children.children.iter().filter_map(|child| child.u64("ID"))).collect();
        for (layer, e) in entries {
            let id = e.u64("disEntityId").unwrap_or(0);
            let components = e.child("Components").cloned().unwrap_or_default();
            if layer == fw::DEFAULT_MISSION_LAYER && !named.contains(&id) && !prefab_children.contains(&id) && !story::has_story_component(&components) {
                continue;
            }
            out.push(story::StoryEntity {
                id,
                layer,
                source,
                name: e.str("hidName").unwrap_or_default(),
                class: e.str_h(fw::h::CLASS_NAME).unwrap_or_default(),
                pos: e.vec3("hidPos_precise").or_else(|| e.vec3("hidPos")).unwrap_or_default(),
                angles: e.vec3("hidAngles").unwrap_or_default(),
                components,
                fields: story::kept_fields(&e),
            });
        }
        Some(out)
    })?;
    let entities: Vec<story::StoryEntity> = lists.into_iter().flatten().collect();
    write_atomic(job.root(), story::ENTITIES_REL, &story::to_bytes(&entities), job.ctx)
}

/// Crafting ingredients, their authored loot sources and localized ids.
fn convert_crafting(job: &Job) -> Result<()> {
    use fc3_formats::{fcb, oasis, progression as p};
    let parse = |bytes: Vec<u8>| -> Result<std::sync::Arc<fcb::Object>> {
        let at = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
        fcb::parse(&bytes[at..])
    };
    let read = |path| parse(job.vfs.read(path)?);
    let templates = read(p::TEMPLATES)?;
    let recipes = read(p::RECIPE_LIB)?;
    let types = read(p::RECIPE_TYPE_LIB)?;
    let subtypes = read(p::RECIPE_SUBTYPE_LIB)?;
    let shop_items = read(p::SHOP_ITEMS_LIB)?;
    let loot_groups = read(p::LOOT_GROUPS)?;
    let library = parse(std::fs::read(job.root().join("story/entitylibrary.fcb"))?)?;
    let strings = oasis::Strings::from_bytes(&job.vfs.read(r"languages\english\oasisstrings_compressed.bin")?)?;
    let metadata = p::crafting(&p::CraftingFiles {
        templates: &templates, recipes: &recipes, types: &types, subtypes: &subtypes,
        shop_items: &shop_items, loot_groups: &loot_groups, library: &library,
    }, &|id| strings.get(id).unwrap_or_default().to_string());
    write_atomic(job.root(), p::RECIPES_REL, metadata.to_text().as_bytes(), job.ctx)
}

/// The moves the story's contextual actions name, with the clips found for
/// them by name (`fc3_formats::moves`).
fn convert_ca_moves(job: &Job) -> Result<()> {
    use fc3_formats::{moves, story};
    let entities = story::from_bytes(&std::fs::read(job.root().join(story::ENTITIES_REL)).context("the story entities")?)?;
    let mut names: BTreeSet<String> = BTreeSet::new();
    for e in &entities {
        let Some(ca) = e.components.children.iter().find(|c| c.is("CAIContextualActionComponent")) else { continue };
        if let Some(params) = ca.children.iter().find(|c| c.is("Parameters")) {
            names.extend(moves::move_names(params));
        }
    }
    let clips: Vec<&str> = job.vfs.names.values().filter(|n| n.ends_with(".mab")).map(String::as_str).collect();
    let db = moves::parse_move_db(&job.vfs.read(moves::MOVE_DB).context("the move database")?);
    write_atomic(job.root(), moves::MOVES_REL, moves::moves_to_text(&db).as_bytes(), job.ctx)?;
    let table = moves::ca_clips(names.iter().map(String::as_str), &db, &clips);
    write_atomic(job.root(), moves::CA_MOVES_REL, moves::to_text(&table).as_bytes(), job.ctx)
}

/// Faces (`fc3_formats::facial`): the definitions table (which rig a
/// definition key names), and one pack of every face rig, every named facial
/// clip and the lip-sync clip of every spoken line that has one.
fn convert_facial(job: &Job) -> Result<()> {
    use fc3_formats::{facial, hash::path_hash};
    let defs = facial::parse_definitions(&job.vfs.read(facial::DEFINITION_LIBRARY)?)?;
    let table: Vec<(u32, u64)> = defs.iter().filter(|d| !d.rig.is_empty()).map(|d| (d.key, path_hash(&d.rig))).collect();
    let mut paths: BTreeSet<String> = job.vfs.names.values().filter(|n| n.ends_with(".dpax") || n.ends_with(".dpdx")).cloned().collect();
    paths.extend(defs.iter().filter(|d| !d.rig.is_empty()).map(|d| d.rig.to_ascii_lowercase()));
    // Spoken lines' clips are named by the line's sound id.
    let speech = job.vfs.read(r"languages\english\speechlength.bin").map_err(anyhow::Error::from).and_then(|b| fc3_formats::oasis::speech_lengths(&b));
    match speech {
        Ok(lines) => paths.extend(lines.keys().map(|&id| facial::voice_clip_path(id))),
        Err(e) => job.error(format!("facial: speech lengths: {e:#}")),
    }
    let paths: Vec<String> = paths.into_iter().collect();
    let mut files: Vec<(u64, Vec<u8>)> = paths.par_iter().filter_map(|p| Some((path_hash(p), job.vfs.read(p).ok()?))).collect();
    files.sort_by_key(|f| f.0);
    files.dedup_by_key(|f| f.0);
    write_atomic(job.root(), facial::PAK_REL, &facial::write_pak(&mut files), job.ctx)?;
    write_atomic(job.root(), facial::DEFINITIONS_REL, facial::definitions_to_text(&table).as_bytes(), job.ctx)
}

/// Every in-engine cinematic (`.cseq`) the archives name or a scene entity
/// plays, as a timeline in the cache, its camera paths read in.
fn convert_cinema(job: &Job) -> Result<()> {
    use fc3_formats::cinema;
    let mut paths: std::collections::BTreeSet<String> = job.vfs.names.values().filter(|n| n.ends_with(".cseq")).cloned().collect();
    // Scene entities name what they play (some files aren't in the lists).
    if let Ok(entities) =
        std::fs::read(job.root().join(fc3_formats::story::ENTITIES_REL)).map_err(anyhow::Error::from).and_then(|b| fc3_formats::story::from_bytes(&b))
    {
        for e in &entities {
            e.components.walk(&mut |o, _| {
                if let Some(p) = o.str_h(0xB1C2_6EBB).filter(|p| p.to_ascii_lowercase().ends_with(".cseq")) {
                    paths.insert(p.to_ascii_lowercase());
                }
            });
        }
    }
    let paths: Vec<String> = paths.into_iter().collect();
    // Refresh older sequences that dropped vehicle movement and clip anchors.
    let refresh = !exists(job.root(), CINEMA_STAMP);
    job.run(Stage::Story, &paths, |path| {
        let rel = cinema::sequence_rel(path);
        if !refresh && !job.should_write(&rel) {
            return Some(());
        }
        let bytes = job.vfs.read(path).ok()?;
        let xml = String::from_utf8_lossy(&bytes);
        let mut seq = match cinema::parse(&xml) {
            Ok(s) => s,
            Err(e) => {
                job.error(format!("cinema {path}: {e:#}"));
                return Some(());
            }
        };
        // Camera clips get their paths, in clip order.
        let root = fc3_formats::xml::parse(&xml)?;
        let mut cams = cinema::camera_resources(&root).into_iter();
        for clip in seq.groups.iter_mut().flat_map(|g| g.clips.iter_mut()) {
            if let cinema::ClipKind::CameraAnimation { keys } = &mut clip.kind {
                let Some(res) = cams.next() else { break };
                match job.vfs.read(&res).map_err(anyhow::Error::from).and_then(|b| cinema::camera_keys(&b)) {
                    Ok(k) => *keys = k,
                    Err(e) => job.error(format!("cinema camera {res}: {e:#}")),
                }
            }
        }
        if let Err(e) = write_atomic(job.root(), &rel, seq.to_text().as_bytes(), job.ctx) {
            job.error(format!("cinema {path}: {e:#}"));
        }
        Some(())
    })?;
    write_atomic(job.root(), CINEMA_STAMP, b"vehicle movement and clip anchors\n", job.ctx)
}

/// Marks the current sequence conversion schema.
const CINEMA_STAMP: &str = "cinema/stamp-vehicle-anchors.txt";

/// Packs every Domino script the file lists name, and the ones they refer to
/// that the lists don't.
fn convert_scripts(job: &Job) -> Result<()> {
    use fc3_formats::scripts;
    let mut queue: Vec<String> = job.vfs.names.values().filter(|n| n.starts_with("domino") && n.ends_with(".lua")).cloned().collect();
    let mut seen: HashSet<String> = queue.iter().cloned().collect();
    let mut found = Vec::new();
    while let Some(path) = queue.pop() {
        let Ok(bytes) = job.vfs.read(&path) else {
            continue;
        };
        for r in scripts::references(&String::from_utf8_lossy(&bytes)) {
            let p = scripts::archive_path(&r);
            if job.vfs.exists(&p) && seen.insert(p.clone()) {
                queue.push(p);
            }
        }
        found.push((path, bytes));
    }
    found.sort();
    write_atomic(job.root(), scripts::DOMINO_PACK_REL, &scripts::pack(&found), job.ctx)
}

fn convert_shaders(job: &Job) -> Result<()> {
    use fc3_formats::d3d9::{self, Stage as ShaderStage, TextureType};
    use fc3_formats::d3d9_wgsl;

    let mut objects: Vec<(u64, &fc3_formats::archive::Archive, fc3_formats::archive::Entry)> = Vec::new();
    for a in &job.vfs.archives {
        if a.name == "shadersobj" || a.name == "patch" {
            objects.extend(a.fat.entries.iter().map(|e| (e.name_hash, a, *e)));
        }
    }
    let translated = job.run(Stage::Shaders, &objects, |(hash, archive, entry)| {
        let data = archive.read(entry).ok()?;
        if data.len() < 4 || u32::from_le_bytes(data[..4].try_into().unwrap()) != d3d9::CONTAINER_MAGIC {
            return None;
        }
        let result = (|| -> Result<(cache::ShaderEntry, Vec<u8>)> {
            let obj = d3d9::parse_object(&data)?;
            let opts = d3d9_wgsl::Options { entry_point: false, ..Default::default() };
            let t = d3d9_wgsl::translate(&obj.program, &opts)?;
            validate_wgsl(&t.wgsl)?;
            let io = |v: &[d3d9_wgsl::Io]| {
                v.iter()
                    .map(|i| cache::ShaderIo { usage: i.usage.code(), usage_index: i.usage_index, register: i.register, location: i.location, flags: i.flags })
                    .collect()
            };
            let entry = cache::ShaderEntry {
                hash: *hash,
                vertex: obj.program.stage == ShaderStage::Vertex,
                offset: 0,
                size: 0,
                inputs: io(&t.inputs),
                outputs: io(&t.outputs),
                samplers: t
                    .samplers
                    .iter()
                    .map(|(r, ty)| {
                        (
                            *r,
                            match ty {
                                TextureType::Cube => 2,
                                TextureType::Volume => 3,
                                _ => 1,
                            },
                        )
                    })
                    .collect(),
                bindings: obj
                    .bindings
                    .iter()
                    .filter(|b| b.rows > 0)
                    .map(|b| cache::ShaderBinding { name_hash: b.name_hash, register: b.first_register(), rows: b.rows, sampler: b.is_sampler() })
                    .collect(),
                color_outputs: t.color_outputs,
                nonuniform: t.nonuniform_samples,
            };
            Ok((entry, cache::compress(t.wgsl.as_bytes())))
        })();
        match result {
            Ok(r) => Some(r),
            Err(e) => {
                job.error(format!("shader {hash:016x}: {e:#}"));
                None
            }
        }
    })?;
    let mut translated = translated;
    translated.sort_by_key(|(e, _)| e.hash);
    let mut pack = Vec::new();
    let mut index = String::from("# hash stage offset size inputs outputs samplers bindings color_outputs nonuniform\n");
    for (mut entry, blob) in translated {
        entry.offset = pack.len() as u64;
        entry.size = blob.len() as u32;
        pack.extend_from_slice(&blob);
        index += &entry.to_line();
        index.push('\n');
    }
    write_atomic(job.root(), cache::SHADER_PACK_REL, &pack, job.ctx)?;
    write_atomic(job.root(), cache::SHADER_INDEX_REL, index.as_bytes(), job.ctx)
}

/// Parses and validates WGSL with the same checks wgpu applies.
pub fn validate_wgsl(src: &str) -> Result<()> {
    let module = naga::front::wgsl::parse_str(src).map_err(|e| anyhow::anyhow!("{}", e.emit_to_string(src)))?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map_err(|e| anyhow::anyhow!("{}", e.emit_to_string(src)))?;
    Ok(())
}

struct SectorResult {
    id: u32,
    models: Vec<String>,
    override_materials: Vec<String>,
    /// Alarm and rebel-post markers (for outposts), Bevy space.
    outpost_markers: Vec<(fc3_formats::outposts::MarkerKind, [f32; 3])>,
    lights: Vec<fc3_formats::lights::WorldLight>,
    /// Cable anchors: own id, linked id, attachment point (Bevy space), zip line.
    anchors: Vec<(u64, u64, [f32; 3], bool)>,
    ladders: Vec<fc3_formats::traversal::Ladder>,
    towers: Vec<[f32; 3]>,
    /// Sound points: event and position (Bevy space).
    sound_points: Vec<(u32, [f32; 3])>,
    /// Placed effects the game plays: kind and position.
    effects: Vec<(&'static str, [f32; 3])>,
    /// Guard spots: kind, position and orientation.
    guard_spots: Vec<(&'static str, [f32; 3], [f32; 4])>,
    villager_spots: Vec<(&'static str, [f32; 3], [f32; 4])>,
    /// Roads and rivers.
    splines: Vec<fc3_formats::splines::SplinePrimitive>,
}

/// Rotates a vector by a quaternion (x, y, z, w).
fn quat_rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    // t = 2 * cross(q.xyz, v); v' = v + w * t + cross(q.xyz, t)
    let t = [2.0 * (y * v[2] - z * v[1]), 2.0 * (z * v[0] - x * v[2]), 2.0 * (x * v[1] - y * v[0])];
    [v[0] + w * t[0] + (y * t[2] - z * t[1]), v[1] + w * t[1] + (z * t[0] - x * t[2]), v[2] + w * t[2] + (x * t[1] - y * t[0])]
}

/// Joins cable anchors to the anchors they link to; each pair once.
fn resolve_cables(anchors: &[(u64, u64, [f32; 3], bool)]) -> Vec<fc3_formats::traversal::Cable> {
    let by_id: HashMap<u64, &(u64, u64, [f32; 3], bool)> = anchors.iter().map(|a| (a.0, a)).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for a in anchors {
        let Some(b) = by_id.get(&a.1) else { continue };
        let key = (a.0.min(b.0), a.0.max(b.0));
        if a.0 == b.0 || !seen.insert(key) {
            continue;
        }
        out.push(fc3_formats::traversal::Cable { zipline: a.3 || b.3, a: a.2, b: b.2 });
    }
    out
}

fn convert_sector(job: &Job, id: u32) -> Result<Option<SectorResult>> {
    let c = SectorCoord::from_id(id);
    let path = c.entities_path(&job.opts.world);
    if !job.vfs.exists(&path) {
        return Ok(None);
    }
    let data = fw::parse_sector(&job.vfs.read(&path)?)?;
    let mut s = CachedSector { sector_id: id, ..Default::default() };
    let mut model_index: HashMap<String, u32> = HashMap::new();
    let mut set_index: HashMap<Vec<String>, u32> = HashMap::new();
    let mut models = Vec::new();
    let mut override_materials = Vec::new();
    let mut outpost_markers = Vec::new();
    let mut lights = Vec::new();
    let (mut anchors, mut ladders, mut towers) = (Vec::new(), Vec::new(), Vec::new());
    let mut sound_points = Vec::new();
    let mut effects = Vec::new();
    let mut guard_spots = Vec::new();
    let mut villager_spots = Vec::new();
    let mut splines = Vec::new();
    for e in &data.entities {
        let entity_index = s.entities.len() as u32;
        let rotation = fw::angles_to_bevy_quat(e.angles);
        let translation = fw::pos_to_bevy(e.pos);
        // Entities on mission layers come and go with missions: they're kept
        // (with their layer) for the game to show while their layer is on,
        // but stay out of the world's standing lists below.
        if e.mission_layer == fw::DEFAULT_MISSION_LAYER {
            if let Some(sp) = &e.spline {
                splines.push(sp.clone());
            }
            if let Some(event) = e.sound_event.filter(|_| cache::sound_point_kept(&e.name)) {
                sound_points.push((event, translation));
            }
            if let Some(kind) = e.effect.as_deref().and_then(cache::world_fx_kind) {
                effects.push((kind, translation));
            }
            if let Some(kind) = cache::guard_spot_kind(&e.name) {
                guard_spots.push((kind, translation, rotation));
            }
            if let Some(kind) = cache::villager_spot_kind(&e.name) {
                villager_spots.push((kind, translation, rotation));
            }
            if let Some(kind) = fc3_formats::outposts::marker_kind(&e.name) {
                outpost_markers.push((kind, translation));
            }
            if let Some(params) = e.light {
                use fc3_formats::lights::{LightKind, WorldLight};
                let spot = e.name.to_ascii_lowercase().contains("spot");
                // Spots shine along the entity's forward axis (-Z in Bevy space).
                let [x, y, z, w] = rotation;
                let direction = [-2.0 * (x * z + w * y), -2.0 * (y * z - w * x), -(1.0 - 2.0 * (x * x + y * y))];
                lights.push(WorldLight { kind: if spot { LightKind::Spot } else { LightKind::Omni }, position: translation, direction, params });
            }
            // The lights of the effects the entity plays (a camp fire's flames
            // light the logs), at the bone each plays at.
            let placed = e.effect.iter().map(|name| (name.as_str(), "", 1.0));
            for (name, bone, scale) in placed.chain(e.attached_effects.iter().map(|a| (a.name.as_str(), a.bone.as_str(), a.scale))) {
                use fc3_formats::lights::{LightKind, WorldLight};
                let at = match (bone.is_empty(), e.meshes.first()) {
                    (false, Some(mesh)) => job.node_positions(&mesh.to_ascii_lowercase()).get(&bone.to_ascii_lowercase()).copied().unwrap_or([0.0; 3]),
                    _ => [0.0; 3],
                };
                // Effects a prop plays on itself that the game draws (camp fires).
                if !bone.is_empty() {
                    if let Some(kind) = cache::world_fx_kind(name) {
                        let w = quat_rotate(rotation, at);
                        effects.push((kind, [translation[0] + w[0], translation[1] + w[1], translation[2] + w[2]]));
                    }
                }
                let Some(effect_lights) = job.effect_lights().get(name) else {
                    continue;
                };
                for l in effect_lights {
                    let o = fmesh::to_bevy(l.offset);
                    let local = [at[0] + o[0] * scale, at[1] + o[1] * scale, at[2] + o[2] * scale];
                    let w = quat_rotate(rotation, local);
                    let mut params = l.params;
                    params.radius *= scale;
                    params.inner_radius *= scale;
                    // Camp fires' lights also mark where the game draws their flames.
                    let kind = if name.contains("camp_fire") { LightKind::Fire } else { LightKind::Effect };
                    lights.push(WorldLight {
                        kind,
                        position: [translation[0] + w[0], translation[1] + w[1], translation[2] + w[2]],
                        direction: [0.0, -1.0, 0.0],
                        params,
                    });
                }
            }
            let rotate = |v: [f32; 3]| quat_rotate(rotation, v);
            if let Some(c) = e.cable {
                let o = rotate(fw::pos_to_bevy(c.offset));
                let at = [translation[0] + o[0], translation[1] + o[1], translation[2] + o[2]];
                anchors.push((e.id, c.link, at, fc3_formats::traversal::is_zipline_anchor(&e.name)));
            }
            if fc3_formats::traversal::is_tower_panel(&e.name) {
                towers.push(translation);
            }
            if fc3_formats::traversal::is_ladder(&e.name) {
                if let Some((lo, hi)) = e.meshes.first().and_then(|m| job.model_bounds(&m.to_ascii_lowercase())) {
                    let foot = rotate([0.0, lo[1], 0.0]);
                    let head = rotate([0.0, hi[1], 0.0]);
                    let fwd = rotate([0.0, 0.0, -1.0]);
                    let flat = (fwd[0] * fwd[0] + fwd[2] * fwd[2]).sqrt().max(1e-3);
                    ladders.push(fc3_formats::traversal::Ladder {
                        base: [translation[0] + foot[0], translation[1] + foot[1], translation[2] + foot[2]],
                        top: translation[1] + head[1],
                        facing: [fwd[0] / flat, 0.0, fwd[2] / flat],
                    });
                }
            }
        }
        s.entities.push(CachedEntity {
            id: e.id,
            class: e.class.clone(),
            name: e.name.clone(),
            archetype: e.archetype.clone().unwrap_or_default(),
            translation,
            rotation,
            mission_layer: e.mission_layer,
        });
        if fw::is_gameplay_mesh(e) {
            continue;
        }
        // Slot 0 is the intact mesh; later slots are destruction states.
        let Some(mesh) = e.meshes.first() else {
            continue;
        };
        let lower = mesh.to_ascii_lowercase();
        // A few entities reference meshes that aren't in the install; skip them.
        // So are the volumes and blockout pieces the engine never draws
        // (interior volumes, the caverns' volumes and sound rooms, fake
        // collision boxes, level-design blocks), all grey placeholders.
        if !job.vfs.exists(&lower) || NOT_DRAWN.iter().any(|k| lower.contains(k)) {
            continue;
        }
        let next = model_index.len() as u32;
        let model = *model_index.entry(lower.clone()).or_insert_with(|| {
            s.models.push(cache::model_rel(&lower));
            models.push(lower.clone());
            next
        });
        let own = job.model_materials(&lower);
        let raw = if e.material_replacements.is_empty() { Vec::new() } else { job.model_materials_raw(&lower) };
        let resolved = resolve_overrides(&lower, &own, &raw, &e.material_overrides, &e.material_replacements);
        let material_set = if resolved == *own {
            NO_MATERIAL_SET
        } else {
            override_materials.extend(resolved.iter().cloned());
            let rels: Vec<String> = resolved.iter().map(|m| cache::material_rel(m)).collect();
            let next = s.material_sets.len() as u32;
            *set_index.entry(rels.clone()).or_insert_with(|| {
                s.material_sets.push(rels);
                next
            })
        };
        s.placements.push(Placement { model, translation, rotation, scale: 1.0, entity: entity_index, material_set });
    }
    let rel = cache::sector_rel(id);
    if job.should_write(&rel) {
        write_atomic(job.root(), &rel, &s.to_bytes(), job.ctx)?;
    }
    Ok(Some(SectorResult {
        id,
        models,
        override_materials,
        outpost_markers,
        lights,
        anchors,
        ladders,
        towers,
        sound_points,
        effects,
        guard_spots,
        villager_spots,
        splines,
    }))
}

/// Bakes one sector's vegetation; returns the model paths it references.
/// A sector's vegetation: its id, the models it uses, and its trees'
/// far-distance billboards (card path, base in Bevy space, scale).
type SectorVegetation = (u32, Vec<String>, Vec<(String, [f32; 3], f32)>);

fn convert_vegetation(job: &Job, lib: &fc3_formats::vegetation::ClusterLibrary, id: u32) -> Result<Option<SectorVegetation>> {
    use fc3_formats::cache::{veg_kind, CachedVegetation, VegInstance};
    use fc3_formats::vegetation::PlantKind;
    let plants = fc3_formats::vegetation::sector_vegetation_with(job.vfs, lib, id)?;
    if plants.is_empty() {
        return Ok(None);
    }
    let mut v = CachedVegetation { sector_id: id, ..Default::default() };
    let mut index: HashMap<String, u16> = HashMap::new();
    // Terrain colour atlas, sampled per instance for ground-cover tint.
    let coord = SectorCoord::from_id(id);
    let atlas = job.vfs.read(&coord.atlas_path(&job.opts.world, "diffuse")).ok().and_then(|b| fc3_formats::texture::parse(&b).ok().map(|x| x.dds.to_vec()));
    let origin = coord.origin();
    let mut originals = Vec::new();
    let mut vista = Vec::new();
    for p in &plants {
        if p.kind == PlantKind::Tree {
            if let Some(card) = lib.get(p.resource).and_then(|r| r.vista.clone()).filter(|c| job.vfs.exists(&c.to_ascii_lowercase())) {
                vista.push((card.to_ascii_lowercase(), fw::pos_to_bevy(p.pos), p.scale));
            }
        }
        // Trees use their RealTree model, or the billboard when that is missing.
        let model = match p.kind {
            PlantKind::Tree if p.model.to_ascii_lowercase().ends_with(".rtx") && job.vfs.exists(&p.model) => p.model.clone(),
            PlantKind::Tree => match lib.get(p.resource).and_then(|r| r.vista.clone()) {
                Some(m) => m,
                None => continue,
            },
            _ => p.model.clone(),
        };
        let lower = model.to_ascii_lowercase();
        if !(lower.ends_with(".xbg") || lower.ends_with(".rtx")) || !job.vfs.exists(&lower) {
            continue;
        }
        let next = index.len() as u16;
        let mi = *index.entry(lower.clone()).or_insert_with(|| {
            v.models.push(cache::model_rel(&lower));
            originals.push(lower.clone());
            next
        });
        let rotation = match (p.kind, p.normal) {
            (PlantKind::Grass, Some(n)) => {
                // Tilt to the terrain normal, then yaw about it.
                let up = fw::pos_to_bevy(n);
                let len = (up[0] * up[0] + up[1] * up[1] + up[2] * up[2]).sqrt().max(1e-6);
                let up = [up[0] / len, up[1] / len, up[2] / len];
                let tilt = quat_from_to([0.0, 1.0, 0.0], up);
                quat_mul(tilt, fw::angles_to_bevy_quat([0.0, 0.0, p.yaw.to_degrees()]))
            }
            (PlantKind::Mesh, _) => fw::angles_to_bevy_quat(p.angles),
            _ => fw::angles_to_bevy_quat([0.0, 0.0, p.yaw.to_degrees()]),
        };
        v.instances.push(VegInstance {
            model: mi,
            kind: match p.kind {
                PlantKind::Tree => veg_kind::TREE,
                PlantKind::Grass => veg_kind::GRASS,
                PlantKind::Mesh => veg_kind::MESH,
            },
            translation: fw::pos_to_bevy(p.pos),
            rotation,
            scale: p.scale,
            tint: atlas
                .as_deref()
                .and_then(|dds| {
                    let size = fc3_formats::texture::dds_info(dds)?.width;
                    let (row, col) = fc3_formats::vegetation::atlas_pixel(coord, p.pos[0] - origin[0], p.pos[1] - origin[1], size);
                    fc3_formats::texture::bc1_texel(dds, col, row)
                })
                .unwrap_or([96, 110, 64]),
        });
    }
    if v.instances.is_empty() {
        return Ok(None);
    }
    let rel = cache::vegetation_rel(id);
    if job.should_write(&rel) {
        write_atomic(job.root(), &rel, &v.to_bytes(), job.ctx)?;
    }
    Ok(Some((id, originals, vista)))
}

fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [aw * bx + ax * bw + ay * bz - az * by, aw * by - ax * bz + ay * bw + az * bx, aw * bz + ax * by - ay * bx + az * bw, aw * bw - ax * bx - ay * by - az * bz]
}

/// Shortest-arc rotation taking unit vector `a` to unit vector `b`.
fn quat_from_to(a: [f32; 3], b: [f32; 3]) -> [f32; 4] {
    let d = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    if d > 0.99999 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let c = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let w = 1.0 + d;
    let len = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2] + w * w).sqrt();
    [c[0] / len, c[1] / len, c[2] / len, w / len]
}

/// Applies an entity's material overrides to a model's slots.
///
/// Override lists usually hold the mesh's own materials in a different order,
/// so they are treated as a set: slots whose material is in the set keep it,
/// and the remaining slots take the unmatched overrides in order. Entries
/// that name a replacement file swap the slot holding their material for it.
fn resolve_overrides(model: &str, own: &[String], raw: &[String], overrides: &[String], replacements: &[(String, String)]) -> Vec<String> {
    let stem = |p: &str| p.rsplit(['\\', '/']).next().unwrap_or(p).to_ascii_lowercase().trim_end_matches(".material.bin").to_string();
    // Matched on the slot's own material as the model has it (before a
    // placeholder's rock fallback).
    let replace = |i: usize, m: String| -> String {
        let from = raw.get(i).map(|r| stem(r)).unwrap_or_else(|| stem(&m));
        replacements.iter().find(|(f, _)| *f == from).map(|(_, to)| fw::material_override_path(to)).unwrap_or(m)
    };
    if overrides.is_empty() || own.is_empty() {
        return own.iter().enumerate().map(|(i, m)| replace(i, m.clone())).collect();
    }
    let ovr: Vec<String> = overrides.iter().filter(|n| !n.is_empty()).map(|n| substitute_placeholder(model, &fw::material_override_path(n))).collect();
    let set: HashSet<&String> = ovr.iter().collect();
    let own_set: HashSet<&String> = own.iter().collect();
    let mut spare = ovr.iter().filter(|m| !own_set.contains(m) && !m.contains("lodmosaic"));
    own.iter()
        .map(|m| if set.contains(m) { m.clone() } else { spare.next().cloned().unwrap_or_else(|| m.clone()) })
        .enumerate()
        .map(|(i, m)| replace(i, m))
        .collect()
}

fn substitute_placeholder(model: &str, bin: &str) -> String {
    let placeholder = bin == PLACEHOLDER_MATERIAL || bin.eq_ignore_ascii_case(CHECKER_PLACEHOLDER_MATERIAL);
    if placeholder && ["rock", "cliff", "cave", "boulder", "stone"].iter().any(|k| model.contains(k)) {
        ROCK_FALLBACK_MATERIAL.to_string()
    } else {
        bin.to_string()
    }
}

/// Converts one model; returns its per-slot material paths.
fn convert_model(job: &Job, path: &str) -> Result<Vec<String>> {
    if path.ends_with(".rtx") {
        return convert_realtree(job, path);
    }
    let mats = job.model_materials(path);
    let rel = cache::model_rel(path);
    if job.should_write(&rel) {
        let bytes = job.vfs.read(path)?;
        let mut model = fmesh::decode(&bytes, 8)?;
        // Skinning data and corrected per-primitive materials come from the anim decoder.
        let skin = fc3_formats::anim::skin_info_lods(&bytes, 8).ok();
        if let Some(skin) = &skin {
            for (li, (_, lod)) in model.lods.iter_mut().enumerate() {
                let Some(sl) = skin.lods.get(li) else {
                    continue;
                };
                for sm in &mut lod.submeshes {
                    if let Some(p) = sl.primitives.get(sm.primitive) {
                        sm.material = p.material as usize;
                    }
                }
                if sl.joints.len() == lod.positions.len() {
                    lod.joints = sl.joints.clone();
                    lod.weights = sl.weights.clone();
                }
                // Rigidly bound parts (weapon slides, magazines, prop pieces) are
                // stored relative to their node: place them in model space.
                for p in &sl.primitives {
                    let Some(m) = p.rigid_node.and_then(|n| skin.bind_world.get(n as usize)) else {
                        continue;
                    };
                    if is_identity(m) {
                        continue;
                    }
                    let range = p.vertex_start as usize..((p.vertex_start + p.vertex_count) as usize).min(lod.positions.len());
                    for v in range {
                        lod.positions[v] = transform_point(m, lod.positions[v]);
                        if let Some(n) = lod.normals.get_mut(v) {
                            *n = transform_dir(m, *n);
                        }
                        if let Some(t) = lod.tangents.get_mut(v) {
                            let d = transform_dir(m, [t[0], t[1], t[2]]);
                            *t = [d[0], d[1], d[2], t[3]];
                        }
                    }
                }
            }
        }
        let pivots = skin.as_ref().map(|s| moving_parts(&mut model, s)).unwrap_or_default();
        let flags: Vec<u32> = mats.iter().map(|m| job.material(m).cached.flags).collect();
        let mut cm = build_cached_model(&model, mats.iter().map(|m| cache::material_rel(m)).collect(), flags, &pivots);
        // Collision the game never draws (it is painted with a placeholder).
        if COLLISION_ONLY.iter().any(|k| path.to_ascii_lowercase().contains(k)) {
            for lod in &mut cm.lods {
                lod.parts.clear();
            }
        }
        cm.bones = cached_bones(&bytes, skin.as_ref());
        write_atomic(job.root(), &rel, &cm.to_bytes(), job.ctx)?;
    }
    Ok(mats.to_vec())
}

fn is_identity(m: &fc3_formats::anim::Mat4) -> bool {
    m.iter().enumerate().all(|(k, v)| (v - if k % 5 == 0 { 1.0 } else { 0.0 }).abs() < 1e-5)
}

/// Column-major affine transform of a point.
fn transform_point(m: &fc3_formats::anim::Mat4, p: [f32; 3]) -> [f32; 3] {
    let d = transform_dir(m, p);
    [d[0] + m[12], d[1] + m[13], d[2] + m[14]]
}

fn transform_dir(m: &fc3_formats::anim::Mat4, v: [f32; 3]) -> [f32; 3] {
    [m[0] * v[0] + m[4] * v[1] + m[8] * v[2], m[1] * v[0] + m[5] * v[1] + m[9] * v[2], m[2] * v[0] + m[6] * v[1] + m[10] * v[2]]
}

/// Detail levels built for RealTree models and the distance each starts at.
const TREE_LODS: [(usize, f32); 3] = [(0, 0.0), (1, 40.0), (2, 100.0)];

/// Builds a cached model from a RealTree file: branch tubes, leaf cards and
/// big leaves per detail level, plus the trunk as a collision mesh.
fn convert_realtree(job: &Job, path: &str) -> Result<Vec<String>> {
    use fc3_formats::realtree::{self, MeshOptions, PartKind};
    let tree = realtree::parse(&job.vfs.read(path)?)?;
    let mut materials: Vec<String> = Vec::new();
    for m in &tree.materials {
        let p = m.path.to_ascii_lowercase();
        if !materials.contains(&p) {
            materials.push(p);
        }
    }
    let rel = cache::model_rel(path);
    if !job.should_write(&rel) {
        return Ok(materials);
    }
    let to_bevy = |v: &[f32; 3]| [v[0], v[2], -v[1]];
    let mut out = CachedModel { materials: materials.iter().map(|m| cache::material_rel(m)).collect(), ..Default::default() };
    out.material_flags = materials.iter().map(|m| job.material(m).cached.flags).collect();
    for (lod, distance) in TREE_LODS {
        let parts = tree.build_parts(&MeshOptions { lod, ..Default::default() });
        let mut lod_parts = Vec::new();
        for part in parts {
            let Some(mat) = part.material.as_ref().map(|m| m.to_ascii_lowercase()) else {
                continue;
            };
            let Some(slot) = materials.iter().position(|m| *m == mat) else {
                continue;
            };
            if part.alpha_tested {
                out.material_flags[slot] |= mat_flags::ALPHA_TEST | mat_flags::VEGETATION;
            }
            if part.two_sided {
                out.material_flags[slot] |= mat_flags::DOUBLE_SIDED;
            }
            if lod == 2 && part.kind == PartKind::Branches {
                out.collision_positions = part.positions.iter().map(to_bevy).collect();
                out.collision_indices = part.indices.clone();
            }
            lod_parts.push(CachedPart {
                material: slot as u32,
                positions: part.positions.iter().map(to_bevy).collect(),
                normals: part.normals.iter().map(to_bevy).collect(),
                uvs: part.uvs.clone(),
                indices: part.indices.clone(),
                ..Default::default()
            });
        }
        out.lods.push(CachedLod { distance, parts: lod_parts });
    }
    let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in out.lods.first().into_iter().flat_map(|l| &l.parts).flat_map(|p| &p.positions) {
        for k in 0..3 {
            mn[k] = mn[k].min(p[k]);
            mx[k] = mx[k].max(p[k]);
        }
    }
    if mn[0] > mx[0] {
        bail!("tree has no geometry");
    }
    (out.aabb_min, out.aabb_max) = (mn, mx);
    write_atomic(job.root(), &rel, &out.to_bytes(), job.ctx)?;
    Ok(materials)
}

/// Material parameter hashes for the texture slots we use, in priority order.
mod slots {
    pub const DIFFUSE: [u32; 7] = [0xF95881AF, 0xEE34B08B, 0x6051D015, 0xA5B246C7, 0xC56E3875, 0xE6170719, 0x061C6B92];
    pub const NORMAL: [u32; 5] = [0x045829E2, 0x8C658791, 0xCFC0C64F, 0xECB9F923, 0xD58F890D];
    pub const SPECULAR: [u32; 1] = [0xDF3C1B89];
}

fn build_material(job: &Job, bin: &str) -> MaterialInfo {
    let mut out = CachedMaterial { base_color: [1.0, 1.0, 1.0, 1.0], roughness: 0.8, ..Default::default() };
    let mut textures = Vec::new();
    if bin == WATER_MASK_MATERIAL {
        out.flags |= mat_flags::NO_DRAW;
        return MaterialInfo { cached: out, textures, tex_paths: Vec::new() };
    }
    if bin == PLACEHOLDER_MATERIAL {
        out.flags |= mat_flags::PLACEHOLDER;
        out.base_color = [0.45, 0.43, 0.4, 1.0];
        return MaterialInfo { cached: out, textures, tex_paths: Vec::new() };
    }
    let Ok(m) = job.vfs.read(bin).and_then(|b| fmat::parse(&b)) else {
        out.base_color = [0.55, 0.55, 0.55, 1.0];
        return MaterialInfo { cached: out, textures, tex_paths: Vec::new() };
    };
    out.shader = m.shader.clone();
    // The debug folders' checker and "invisible" maps are placeholders; their
    // plain grey is a real colour a few props are painted with.
    let usable = |p: &str| {
        let p = p.to_ascii_lowercase();
        job.vfs.exists(&p) && (!p.contains(r"\debug\") || p.ends_with(r"\debug\grey.xbt"))
    };
    // Ordered candidates: known slots first, then anything whose name says what it is.
    let pick = |slot_order: &[u32], role: TexRole| -> Option<String> {
        let by_slot = slot_order.iter().filter_map(|h| m.textures.iter().find(|(k, _)| k == h).map(|(_, p)| p.clone()));
        let by_role = m.textures.iter().filter(|(_, p)| fmat::classify_texture(p) == role).map(|(_, p)| p.clone());
        by_slot.chain(by_role).find(|p| usable(p))
    };
    let diffuse = pick(&slots::DIFFUSE, TexRole::Diffuse)
        .or_else(|| m.textures.iter().map(|(_, p)| p.clone()).find(|p| fmat::classify_texture(p) == TexRole::Other && usable(p)));
    let normal = pick(&slots::NORMAL, TexRole::Normal);
    let specular = pick(&slots::SPECULAR, TexRole::Specular);
    let mut use_tex = |p: Option<String>| -> String {
        match p {
            Some(p) => {
                let p = p.to_ascii_lowercase();
                let rel = cache::texture_rel(&p);
                textures.push(p);
                rel
            }
            None => String::new(),
        }
    };
    out.diffuse = use_tex(diffuse);
    out.normal = use_tex(normal);
    out.specular = use_tex(specular);
    // Everything else the original shaders may sample.
    let mut tex_paths = Vec::new();
    for (hash, path) in &m.textures {
        if usable(path) {
            let p = path.to_ascii_lowercase();
            tex_paths.push((*hash, p.clone()));
            out.textures.push((*hash, cache::texture_rel(&p)));
            if !textures.contains(&p) {
                textures.push(p);
            }
        }
    }
    out.params = m.params.clone();
    // The material's own render switches.
    if m.flag(fmat::param::TWO_SIDED) {
        out.flags |= mat_flags::DOUBLE_SIDED;
    }
    if m.flag(fmat::param::ALPHA_TEST_ENABLED) {
        out.flags |= mat_flags::ALPHA_TEST;
    }
    let shader = m.shader.to_ascii_lowercase();
    if matches!(shader.as_str(), "grass" | "ground_vegetation" | "hair" | "leaf" | "bigleaf") {
        out.flags |= mat_flags::ALPHA_TEST | mat_flags::DOUBLE_SIDED;
    }
    if shader.contains("vegetation") || shader == "grass" {
        out.flags |= mat_flags::VEGETATION;
    }
    if shader == "unlit" || shader == "effectlit" {
        out.flags |= mat_flags::EMISSIVE;
    }
    if matches!(shader.as_str(), "clouddome" | "starsphere") {
        out.flags |= mat_flags::NO_DRAW;
    }
    // Painted only with the debug "invisible" map (fully transparent) and
    // alpha-tested or unlit: the game shows nothing there, where these had
    // stood in as grey surfaces on 35 models.
    let debug_map = |t: &str| t.to_ascii_lowercase().contains(r"\debug\");
    let invisible = |t: &str| t.to_ascii_lowercase().ends_with(r"\debug\invisible.xbt");
    if m.textures.iter().any(|(_, t)| invisible(t))
        && m.textures.iter().all(|(_, t)| debug_map(t))
        && (out.flags & mat_flags::ALPHA_TEST != 0 || shader == "unlit")
    {
        out.flags |= mat_flags::NO_DRAW;
    }
    if shader == "decal" || shader == "blended" {
        out.flags |= mat_flags::ALPHA_BLEND;
    }
    if shader == "water" {
        out.flags |= mat_flags::WATER;
    }
    // Only descriptive file names say what a material is for; most are
    // `<author>-m-<date>`, and one author's name starts with "grass".
    let stem = bin.rsplit(['\\', '/']).next().unwrap_or(bin);
    let descriptive = stem.split('-').nth(1) != Some("m");
    // Lit effects (waterfall sheets, foam) are see-through.
    if shader == "effectlit" {
        out.flags |= mat_flags::ALPHA_BLEND;
    }
    if descriptive && ["leaf", "leaves", "foliage", "palm", "fern", "vegetation", "frond", "grass", "bush"].iter().any(|k| stem.contains(k)) {
        out.flags |= mat_flags::ALPHA_TEST | mat_flags::DOUBLE_SIDED | mat_flags::VEGETATION;
    }
    out.roughness = match shader.as_str() {
        "metal" | "metal_advanced" => 0.45,
        "skin_advanced" | "skin_cinematic" | "eye" => 0.5,
        _ => 0.8,
    };
    MaterialInfo { cached: out, textures, tex_paths }
}

pub fn material_bin_path(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if let Some(stem) = lower.strip_suffix(".material.xml") {
        format!("{stem}.material.bin")
    } else if lower.ends_with(".material.bin") {
        lower
    } else {
        format!("{lower}.material.bin")
    }
}

/// Damage states past the first (`STATE02__...`) are left out, so a model shows
/// intact; parts under a wheel or the steering wheel are noted with that node
/// (per LOD, per submesh) so they get parts of their own.
fn moving_parts(model: &mut fmesh::Model, skin: &fc3_formats::anim::SkinInfo) -> Vec<Vec<Option<cache::PartPivot>>> {
    let names: Vec<String> = skin.nodes.iter().map(|n| n.name.to_ascii_uppercase()).collect();
    let ancestors = |node: Option<u16>| {
        let mut out = Vec::new();
        let mut n = node.map(usize::from);
        while let Some(i) = n.filter(|&i| i < names.len() && !out.contains(&i)) {
            out.push(i);
            let p = skin.nodes[i].parent;
            n = (p >= 0).then_some(p as usize);
        }
        out
    };
    let later_state = |name: &str| name.strip_prefix("STATE0").and_then(|r| r.chars().next()).is_some_and(|c| ('2'..='9').contains(&c));
    let mut out = Vec::new();
    for (li, (_, lod)) in model.lods.iter_mut().enumerate() {
        let prims = skin.lods.get(li).map(|l| l.primitives.as_slice()).unwrap_or(&[]);
        let mut keep = Vec::new();
        let mut pivots = Vec::new();
        for sm in std::mem::take(&mut lod.submeshes) {
            let chain = ancestors(prims.get(sm.primitive).and_then(|p| p.rigid_node));
            if chain.iter().any(|&i| later_state(&names[i])) {
                continue;
            }
            // Wheels and steering turn; a weapon's mounted or handheld pieces
            // show only when it's used that way.
            let pivot =
                chain.iter().copied().find(|&i| names[i].starts_with("WHEEL_") || names[i] == "STEERINGWHEEL" || names[i].starts_with("PIECES_")).map(|i| {
                    let m = &skin.bind_world[i];
                    cache::PartPivot { name: skin.nodes[i].name.clone(), at: [m[12], m[13], m[14]], rotation: fc3_formats::anim::mat_rotation(m) }
                });
            keep.push(sm);
            pivots.push(pivot);
        }
        lod.submeshes = keep;
        out.push(pivots);
    }
    out
}

fn build_cached_model(model: &fmesh::Model, materials: Vec<String>, material_flags: Vec<u32>, pivots: &[Vec<Option<cache::PartPivot>>]) -> CachedModel {
    let slot_flags = |m: usize| material_flags.get(m).copied().unwrap_or(0);
    let mut out = CachedModel { materials, ..Default::default() };
    for (li, (distance, lod)) in model.lods.iter().enumerate() {
        // One part per material, and per moving node.
        let pivot_of = |si: usize| pivots.get(li).and_then(|p| p.get(si)).cloned().flatten();
        let mut by_mat: Vec<(usize, Option<cache::PartPivot>, Vec<&fmesh::SubMesh>)> = Vec::new();
        for (si, s) in lod.submeshes.iter().enumerate() {
            let pivot = pivot_of(si);
            match by_mat.iter_mut().find(|(m, p, _)| *m == s.material && *p == pivot) {
                Some((_, _, v)) => v.push(s),
                None => by_mat.push((s.material, pivot, vec![s])),
            }
        }
        by_mat.sort_by_key(|(m, p, _)| (p.is_some(), *m));
        let parts = by_mat
            .into_iter()
            .filter(|(m, _, _)| slot_flags(*m) & mat_flags::NO_DRAW == 0)
            .map(|(m, pivot, subs)| CachedPart { pivot, ..compact_part(lod, &subs, m as u32) })
            .filter(|p| !p.indices.is_empty())
            .collect();
        out.lods.push(CachedLod { distance: *distance, parts });
    }
    if let Some((_, lod0)) = model.lods.first() {
        let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in &lod0.positions {
            for k in 0..3 {
                mn[k] = mn[k].min(p[k]);
                mx[k] = mx[k].max(p[k]);
            }
        }
        if lod0.positions.is_empty() {
            (mn, mx) = ([0.0; 3], [0.0; 3]);
        }
        out.aabb_min = mn;
        out.aabb_max = mx;
    }
    // Collision from the coarsest LOD, excluding foliage (walk-through) and non-solid materials.
    if let Some((_, coarse)) = model.lods.last() {
        let mut remap: HashMap<u32, u32> = HashMap::new();
        for s in &coarse.submeshes {
            if slot_flags(s.material) & (mat_flags::ALPHA_TEST | mat_flags::NO_DRAW | mat_flags::WATER | mat_flags::VEGETATION) != 0 {
                continue;
            }
            for &i in &coarse.indices[s.index_start..s.index_start + s.index_count] {
                let n = *remap.entry(i).or_insert_with(|| {
                    out.collision_positions.push(coarse.positions[i as usize]);
                    (out.collision_positions.len() - 1) as u32
                });
                out.collision_indices.push(n);
            }
        }
    }
    out.material_flags = material_flags;
    out
}

/// Bones from the corrected NODE parser, with inverse binds when the model is skinned.
fn cached_bones(xbg: &[u8], skin: Option<&fc3_formats::anim::SkinInfo>) -> Vec<cache::CachedBone> {
    let nodes = match skin {
        Some(s) => s.nodes.clone(),
        None => fc3_formats::anim::parse_nodes(xbg).unwrap_or_default(),
    };
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| cache::CachedBone {
            name: n.name.clone(),
            name_hash: n.name_hash,
            parent: n.parent,
            translation: n.translation,
            rotation: n.rotation,
            scale: n.scale,
            inverse_bind: skin.and_then(|s| s.inverse_bind.get(i).copied()).unwrap_or(cache::IDENTITY),
        })
        .collect()
}

fn compact_part(lod: &MeshData, subs: &[&fmesh::SubMesh], material: u32) -> CachedPart {
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut order: Vec<u32> = Vec::new();
    let mut indices = Vec::new();
    for s in subs {
        for &i in &lod.indices[s.index_start..s.index_start + s.index_count] {
            let n = *remap.entry(i).or_insert_with(|| {
                order.push(i);
                (order.len() - 1) as u32
            });
            indices.push(n);
        }
    }
    let n = lod.positions.len();
    let pick = |len: usize| len == n;
    CachedPart {
        material,
        positions: order.iter().map(|&i| lod.positions[i as usize]).collect(),
        normals: if pick(lod.normals.len()) { order.iter().map(|&i| lod.normals[i as usize]).collect() } else { vec![] },
        uvs: if pick(lod.uvs.len()) { order.iter().map(|&i| lod.uvs[i as usize]).collect() } else { vec![] },
        uvs1: if pick(lod.uvs1.len()) { order.iter().map(|&i| lod.uvs1[i as usize]).collect() } else { vec![] },
        tangents: if pick(lod.tangents.len()) { order.iter().map(|&i| lod.tangents[i as usize]).collect() } else { vec![] },
        colors: if pick(lod.colors.len()) { order.iter().map(|&i| lod.colors[i as usize]).collect() } else { vec![] },
        joints: if pick(lod.joints.len()) { order.iter().map(|&i| lod.joints[i as usize]).collect() } else { vec![] },
        weights: if pick(lod.weights.len()) { order.iter().map(|&i| lod.weights[i as usize]).collect() } else { vec![] },
        indices,
        pivot: None,
    }
}

/// Weapon paints: a camo pattern a weapon's material takes as its second
/// colour layer, blended by the weapon's own mask. The game keeps each painted
/// version as a material of its own (the weapon's maps plus the pattern), so a
/// material with the same colour map and no pattern can be swapped for any of
/// them. Patterns by texture, with plain names of ours; the one showing a
/// face is left out.
const PAINT_PATTERNS: &[(&str, &str)] = &[("camo_02_d", "Tribal"), ("camo_ulc_01", "Urban"), ("camo_ulc_02", "Tiger"), ("c_cm_camo_04_p", "Desert")];

/// Writes the paint table (`cache::PAINTS_REL`) and returns the painted
/// materials' paths for conversion.
fn convert_paints(job: &Job) -> Result<Vec<String>> {
    if !job.should_write(cache::PAINTS_REL) {
        let text = std::fs::read_to_string(job.root().join(cache::PAINTS_REL))?;
        return Ok(cache::paints_from_text(&text).into_iter().map(|p| p.variant_path).collect());
    }
    const DIFFUSE1: u32 = 0xF958_81AF;
    const DIFFUSE2: u32 = 0x6051_D015;
    let pattern = |tex: &str| {
        let stem = tex.rsplit(['\\', '/']).next().unwrap_or(tex).trim_end_matches(".xbt").to_ascii_lowercase();
        PAINT_PATTERNS.iter().find(|(s, _)| *s == stem).map(|(_, n)| *n)
    };
    // Every material by its colour map: plain ones, and painted ones by pattern.
    let mut plain: HashMap<String, Vec<String>> = HashMap::new();
    let mut painted: HashMap<String, Vec<(&str, String)>> = HashMap::new();
    let mut names: Vec<&String> = job.vfs.names.values().filter(|n| n.ends_with(".material.bin")).collect();
    names.sort();
    for name in names {
        let Ok(m) = job.vfs.read(name).and_then(|b| fc3_formats::material::parse(&b)) else {
            continue;
        };
        let Some((_, colour)) = m.textures.iter().find(|(h, _)| *h == DIFFUSE1) else {
            continue;
        };
        let colour = colour.to_ascii_lowercase();
        match m.textures.iter().find(|(h, _)| *h == DIFFUSE2).and_then(|(_, t)| pattern(t)) {
            Some(p) => painted.entry(colour).or_default().push((p, name.clone())),
            None if m.textures.iter().all(|(_, t)| !t.to_ascii_lowercase().contains("camo")) => plain.entry(colour).or_default().push(name.clone()),
            None => {}
        }
    }
    let mut paints = Vec::new();
    for (colour, variants) in &painted {
        for base in plain.get(colour).into_iter().flatten() {
            // One variant per pattern.
            let mut seen = HashSet::new();
            for (p, v) in variants {
                if seen.insert(*p) {
                    paints.push(cache::Paint {
                        base: cache::material_rel(base),
                        name: p.to_string(),
                        variant_path: v.clone(),
                        variant: cache::material_rel(v),
                    });
                }
            }
        }
    }
    paints.sort_by(|a, b| (&a.base, &a.name).cmp(&(&b.base, &b.name)));
    write_atomic(job.root(), cache::PAINTS_REL, cache::paints_to_text(&paints).as_bytes(), job.ctx)?;
    Ok(paints.into_iter().map(|p| p.variant_path).collect())
}

/// The game's tone curve, tabulated by running its final pass (see
/// `fc3_formats::grade`), and the world's colour cube.
fn convert_grade(job: &Job) -> Result<fc3_formats::grade::Grade> {
    use fc3_formats::d3d9;
    let cube_sampler = fc3_formats::hash::crc32(b"ColorRemap0VolumeSampler");
    let mut passes = Vec::new();
    for a in job.vfs.archives.iter().filter(|a| a.name == "shadersobj" || a.name == "patch") {
        for e in &a.fat.entries {
            let Ok(data) = a.read(e) else { continue };
            let Ok(obj) = d3d9::parse_object(&data) else {
                continue;
            };
            if obj.bindings.iter().any(|b| b.is_sampler() && b.name_hash == cube_sampler) {
                passes.push(obj);
            }
        }
    }
    let (x_max, curve) = fc3_formats::grade::curve_from_shaders(&passes)?;
    let managers = job.vfs.read(fc3_formats::environment::MANAGERS_PATH)?;
    let path = fc3_formats::environment::Environment::colour_cube_path(&managers).context("no colour cube named")?;
    let xbt = job.vfs.read(&path)?;
    let (size, cube) = fc3_formats::grade::cube_from_dds(fc3_formats::texture::parse(&xbt)?.dds).with_context(|| path.clone())?;
    Ok(fc3_formats::grade::Grade { x_max, curve, size, cube })
}

fn convert_texture_to(job: &Job, original: &str, rel: &str) -> Result<()> {
    let main = job.vfs.read(original)?;
    let xbt = fc3_formats::texture::parse(&main)?;
    let mip0 = xbt.mip0_path.as_deref().and_then(|p| job.vfs.read(p).ok());
    let dds = fc3_formats::texture::assemble_dds(&main, mip0.as_deref())?;
    write_atomic(job.root(), rel, &dds, job.ctx)
}

/// Whether a complete cache of the current version exists at `root`.
pub fn cache_ready(root: &Path) -> bool {
    Manifest::load(root).is_ok()
}
