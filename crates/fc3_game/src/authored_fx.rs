//! The game's own particle effects (`particle_effects::AUTHORED_EFFECTS`),
//! played as authored: an entity with [`AuthoredFx`] plays its effect where
//! it stands, each emitter as its definition has it.
//!
//! An emitter takes part in a play by its chance (`ActiveChance`), waits its
//! delay, then emits for its duration (again and again when it loops): a
//! burst at its start (`NbInitPart`, or between `NbInitPartMin` and `...Max`)
//! and a rate
//! (`EmitRateKF`, its value times its ratio curve over the emitter's time,
//! give or take its variant). Each particle is born with its life, speed,
//! size, gravity, angle and spin (each `Init...KF`'s value give or take its
//! variant), heading along the emitter's axis (`RotOffset`, from Dunia's +Y)
//! within its spreads; over its life its speed, size, gravity, spin and alpha
//! follow their `Part...KF` curves and its colour its gradient
//! (`PartColorKF`; one point of it picked at birth with `RandomColor`). Its
//! sheet is the emitter's texture and grid (`FrameCount` by `FrameCountY`),
//! shown at `FramePerSecond` (a random start with `FrameRandomStart`; still
//! without a rate); `BlendType` 1 adds light, `RedAsAlpha` takes brightness
//! as coverage, else the alpha blends. A particle sized unevenly
//! (`UniformSize` 0: sparks) is a streak along its motion on screen. The
//! emitter's HDR multiplier brightens what it adds (compressed: our
//! brightness isn't the engine's), lit sheets (`SHLighting`) darken with the
//! daylight, and the near and far fades (`NearClipping...`,
//! `FarClipping...`) apply. An emitter with a light (`LightActive`) burns a
//! point light, flickering as authored. A heat haze's particles
//! (`DistortionEnabled`) bend the view behind them through their distortion
//! map instead (`particles::HazeMaterial`). A mesh emitter's (`Geometry`:
//! bullet impacts' chips, explosions' debris) are its model, turned at
//! random as they're born, sized by their size.
//!
//! A child emitter (`PartChildEmitUnit`: the trails of sparks the
//! explosions' guide particles leave) emits from each of its parent's
//! particles as an emitter would, its clock the particle's age, heading
//! about the way the particle goes and taking on its speed.
//!
//! Not played (yet): plotted emission volumes and sounds; curves are joined
//! linearly between their knots.

use std::collections::HashMap;

use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use fc3_formats::particle_effects::{Effect, Emitter};
use fc3_formats::xml::Element;

use crate::particles::{empty_mesh, frame_uv, ParticleMaterial, Quads};
use crate::player::PlayerCamera;
pub use fc3_formats::particle_effects::blast;

pub struct AuthoredFxPlugin;

impl Plugin for AuthoredFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Library>()
            .init_resource::<BulletImpacts>()
            .add_systems(Update, (fx_test, emit, simulate).chain())
            .add_systems(PostUpdate, draw.before(TransformSystems::Propagate));
    }
}

/// `FC3_AUTHORED_FX=<name>[,<metres>]` (tests): that effect played as an
/// impact's on the ground that far (4 m) before the player, once he has
/// stood a while (the loading screen down); with
/// `FC3_AUTHORED_FX_SHOTS=<dir>`, the screen captured through it.
fn fx_test(
    time: Res<Time>,
    heights: Res<crate::terrain::Heightfield>,
    players: Query<(&Transform, &crate::player::Player)>,
    mut state: Local<(bool, f32, Option<f32>, usize)>,
    mut commands: Commands,
) {
    const WAIT: f32 = 15.0;
    const SHOTS: [f32; 6] = [0.05, 0.2, 0.5, 1.0, 2.0, 4.0];
    let (done, stood, fired, shot) = &mut *state;
    if *done {
        return;
    }
    let Ok(spec) = std::env::var("FC3_AUTHORED_FX") else {
        *done = true;
        return;
    };
    if let Some(at) = *fired {
        let dir = std::env::var("FC3_AUTHORED_FX_SHOTS").ok();
        match (dir, SHOTS.get(*shot)) {
            (Some(dir), Some(&when)) if time.elapsed_secs() - at >= when => {
                crate::camera::capture(&mut commands, format!("{dir}/fx_{when:.2}.png"));
                *shot += 1;
            }
            (Some(_), Some(_)) => {}
            _ => *done = true,
        }
        return;
    }
    let Ok((t, p)) = players.single() else { return };
    *stood = if p.spawning { 0.0 } else { *stood + time.delta_secs() };
    if *stood < WAIT {
        return;
    }
    let (name, far) = spec.split_once(',').map_or((spec.as_str(), 4.0), |(n, m)| (n, m.trim().parse().unwrap_or(4.0)));
    let ahead = Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z * far;
    let mut at = t.translation + ahead - Vec3::Y * crate::player::HALF_HEIGHT;
    at.y = heights.height_at(at).unwrap_or(at.y);
    let name: &'static str = Box::leak(name.to_string().into_boxed_str());
    commands.spawn((Transform::from_translation(at).with_rotation(IMPACT_FRAME), AuthoredFx(name)));
    *fired = Some(time.elapsed_secs());
    info!("authored fx test: {name} at {at:.1?}");
}

/// Whether an authored effect is in the cache (else what would play it
/// does its own way); looked for once.
pub fn available(name: &str) -> bool {
    static SEEN: std::sync::Mutex<Option<HashMap<String, bool>>> = std::sync::Mutex::new(None);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    *seen
        .get_or_insert_with(HashMap::new)
        .entry(name.to_string())
        .or_insert_with(|| fc3_formats::cache::default_cache_root().join(fc3_formats::particle_effects::authored_rel(name)).exists())
}

/// Plays an authored effect once where it's asked for, as an impact's
/// (the blasts'): its +Y (Dunia's) along the ground's normal, up. False
/// when it isn't cached, for the caller's own stand-in.
pub fn play(commands: &mut Commands, name: &'static str, at: Vec3) -> bool {
    play_along(commands, name, at, Vec3::Y)
}

/// Plays an authored effect once as an impact on a surface with normal
/// `normal` (its +Y along it). False when it isn't cached.
pub fn play_along(commands: &mut Commands, name: &'static str, at: Vec3, normal: Vec3) -> bool {
    if !available(name) {
        return false;
    }
    commands.spawn((Transform::from_translation(at).with_rotation(surface_turn(normal)), AuthoredFx(name), Once));
    true
}

/// An effect's turn on a surface with normal `normal`, as an impact's: its
/// +Y (Dunia's) along the normal.
pub fn surface_turn(normal: Vec3) -> Quat {
    if normal.y > 0.999 {
        IMPACT_FRAME
    } else {
        Quat::from_rotation_arc(Vec3::NEG_Z, normal.normalize_or(Vec3::Y))
    }
}

/// Plays a weapon's muzzle effect once on its muzzle (`muzzle`, the gun's
/// muzzle node: its -Z down the barrel), moving with it: the effect's frame
/// the muzzle bone's (Dunia's +Z down the barrel, its +Y to the right).
/// False when it isn't cached.
pub fn play_muzzle(commands: &mut Commands, name: &'static str, muzzle: Entity) -> bool {
    if !available(name) {
        return false;
    }
    commands.spawn((Transform::from_rotation(MUZZLE_FRAME), AuthoredFx(name), Once, ChildOf(muzzle)));
    true
}

/// Plays an effect on a gun's muzzle for as long as it's kept (a
/// flamethrower's nozzle): despawning what it returns stops it. None when
/// it isn't cached.
pub fn attach_muzzle(commands: &mut Commands, name: &'static str, muzzle: Entity) -> Option<Entity> {
    available(name).then(|| commands.spawn((Transform::from_rotation(MUZZLE_FRAME), AuthoredFx(name), ChildOf(muzzle))).id())
}

/// Plays a weapon's muzzle effect once where a shot leaves a gun that isn't
/// drawn with its own muzzle node (the characters'), heading `forward`, its
/// right level. False when it isn't cached.
pub fn play_muzzle_at(commands: &mut Commands, name: &'static str, at: Vec3, forward: Vec3) -> bool {
    if !available(name) {
        return false;
    }
    commands.spawn((Transform::from_translation(at).with_rotation(muzzle_frame(forward)), AuthoredFx(name), Once));
    true
}

/// A muzzle effect's frame for a shot heading `forward`, its right level:
/// Dunia's +Z down the barrel, its +Y to the right.
pub fn muzzle_frame(forward: Vec3) -> Quat {
    let forward = forward.normalize_or(Vec3::NEG_Z);
    let right = forward.cross(Vec3::Y).try_normalize().unwrap_or(Vec3::X);
    Quat::from_mat3(&Mat3::from_cols(right.cross(forward), forward, -right))
}

/// A muzzle effect's frame in its gun's: Bevy's local +Y (Dunia's +Z) down
/// the barrel (-Z), local -Z (Dunia's +Y) to the right (+X).
const MUZZLE_FRAME: Quat = Quat::from_xyzw(-0.5, -0.5, 0.5, 0.5);

/// An effect's name kept for the program's life (the weapons' are read at
/// load; each is kept once).
pub fn intern(name: &str) -> &'static str {
    static NAMES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = names.iter().find(|n| **n == name) {
        return n;
    }
    let n: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.push(n);
    n
}

/// Its emitters' lights left to others (the world's placed effects: the
/// converter puts their lights among the world's).
#[derive(Component)]
pub struct NoLights;

/// Played once: what loops in it runs its time once (a splash's ring).
#[derive(Component)]
struct Once;

/// The game's bullet impacts (`particle_effects::BULLET_IMPACTS_REL`): each
/// logic material's effect, and by material name.
#[derive(Resource)]
pub struct BulletImpacts {
    by_material: HashMap<u8, &'static str>,
    by_name: HashMap<String, &'static str>,
}

impl FromWorld for BulletImpacts {
    fn from_world(_: &mut World) -> Self {
        let path = fc3_formats::cache::default_cache_root().join(fc3_formats::particle_effects::BULLET_IMPACTS_REL);
        let impacts = std::fs::read_to_string(path).map(|t| fc3_formats::particle_effects::impacts_from_text(&t)).unwrap_or_default();
        let mut by_material = HashMap::new();
        let mut by_name = HashMap::new();
        for i in impacts {
            let effect: &'static str = Box::leak(i.effect.into_boxed_str());
            by_material.insert(i.material, effect);
            by_name.insert(i.name, effect);
        }
        BulletImpacts { by_material, by_name }
    }
}

impl BulletImpacts {
    /// A bullet's impact on what was hit.
    pub fn of(&self, material: crate::surfaces::Material) -> Option<&'static str> {
        use crate::surfaces::Material;
        match material {
            Material::Logic(id) => self.by_material.get(&id).copied().or_else(|| self.named("Base/Default")),
            Material::Flesh => self.named("Base/Flesh_P"),
            Material::Vehicle => self.named("Base/Metal_Vehicle"),
        }
    }

    /// A bullet's impact on a logic material, by name (`Base/Water_P_Deep`).
    pub fn named(&self, material: &str) -> Option<&'static str> {
        self.by_name.get(material).copied()
    }
}

/// An impact effect's frame: its +Y (Dunia's; Bevy's -Z) up.
const IMPACT_FRAME: Quat = Quat::from_xyzw(std::f32::consts::FRAC_1_SQRT_2, 0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2);

/// Plays an authored effect (by its name) where the entity stands, turned
/// as it is; taken off (or despawned), it stops emitting and what it has
/// emitted lives out. An effect none of whose emitters loop goes when the
/// last has had its time.
#[derive(Component, Clone)]
#[require(Transform, Visibility)]
pub struct AuthoredFx(pub &'static str);

/// An effect's emitters' clocks.
#[derive(Component, Default)]
struct FxClock {
    age: f32,
    carry: Vec<f32>,
    burst: Vec<bool>,
    /// Which emitters take part in this play (by their chance).
    active: Vec<bool>,
    lights: Vec<Option<Entity>>,
    /// Where the effect was last frame, and so how fast it goes (what its
    /// particles take on: `InitExtSpdRatioKF`).
    last: Option<Vec3>,
    velocity: Vec3,
}

/// A light an effect burns.
#[derive(Component)]
struct FxLight;

/// A curve sampled over [0, 1].
#[derive(Clone)]
struct Lut([f32; 33]);

impl Lut {
    fn of(e: &Element, name: &str, fallback: f32) -> Lut {
        let mut v = [fallback; 33];
        for (i, s) in v.iter_mut().enumerate() {
            *s = crate::vine_puff::curve(e, name, i as f32 / 32.0, fallback);
        }
        Lut(v)
    }

    fn at(&self, t: f32) -> f32 {
        let x = t.clamp(0.0, 1.0) * 32.0;
        let i = (x as usize).min(31);
        let f = x - i as f32;
        self.0[i] + (self.0[i + 1] - self.0[i]) * f
    }
}

/// A value given or taken (`ValueKF` and `VariantKF`).
#[derive(Clone, Copy, Default)]
struct Ranged {
    value: f32,
    variant: f32,
}

impl Ranged {
    fn of(e: &Emitter, name: &str) -> Ranged {
        let kf = e.source.child(name);
        let value = kf.and_then(|k| k.child("ValueKF")).and_then(|v| v.f32("Value")).unwrap_or(0.0);
        let variant = kf.and_then(|k| k.child("VariantKF")).and_then(|v| v.f32("Value")).unwrap_or(0.0);
        Ranged { value, variant }
    }

    fn roll(self, rng: &mut crate::weapons::Rng) -> f32 {
        self.value + self.variant * rng.range(-1.0, 1.0)
    }
}

/// What a batch draws: a texture, its grid, how it blends and how soft it
/// is (`Soft`'s `SoftRange`, in cm; 0 hard); or, for heat haze, how it
/// bends the view.
#[derive(Clone, PartialEq, Eq, Hash)]
struct BatchKey {
    texture: String,
    mode: u8,
    soft: u16,
    haze: Option<HazeKey>,
}

/// A heat haze's distortion map and how it bends the view through it: its
/// scroll a second, its tiling, its strength and blur (`Distortion...`, as
/// bits), and whether its shape is the sheet's red (`RedAsAlpha`).
#[derive(Clone, PartialEq, Eq, Hash)]
struct HazeKey {
    map: String,
    numbers: [u32; 6],
    red: bool,
}

impl HazeKey {
    fn of(e: &Emitter) -> Option<HazeKey> {
        let s = &e.source;
        let f = |n: &str, d: f32| s.f32(n).unwrap_or(d);
        let map = e.texture_of("DistortionTexture")?.ok()?;
        Some(HazeKey {
            map: fc3_formats::cache::texture_rel(&map),
            numbers: [
                f("DistortionSpeedU", 0.0),
                f("DistortionSpeedV", 0.0),
                f("DistortionTilingU", 1.0),
                f("DistortionTilingV", 1.0),
                f("DistortionStrength", 0.0),
                f("DistortionBlurStrength", 0.0),
            ]
            .map(f32::to_bits),
            red: s.f32("RedAsAlpha").is_some_and(|v| v != 0.0),
        })
    }

    /// (`FC3_HAZE_SCALE=<times>`, tests: bent that much more, to be seen.)
    fn material(&self, shape: Handle<Image>, map: Handle<Image>, soft: f32) -> crate::particles::HazeMaterial {
        let n = self.numbers.map(f32::from_bits);
        let scale = std::env::var("FC3_HAZE_SCALE").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.0);
        crate::particles::HazeMaterial::new(
            shape,
            map,
            Vec4::new(n[0], n[1], n[2], n[3]),
            Vec4::new(n[4] * scale, n[5], soft, if self.red { 1.0 } else { 0.0 }),
        )
    }
}

/// An emitter made ready to play.
struct Compiled {
    batch: Option<BatchKey>,
    /// A mesh emitter's model (cached), its particles shown as it.
    mesh: Option<String>,
    /// Its child emitter (an index among the emitters), emitting from each
    /// of its particles.
    child: Option<usize>,
    frames: [u32; 2],
    fps: Ranged,
    random_start: bool,
    play_once: bool,
    duration: f32,
    delay: f32,
    looping: bool,
    burst: u32,
    /// Its burst drawn between two counts instead (`NbInitPartMin`,
    /// `NbInitPartMax`, when the latter is set).
    burst_range: Option<[u32; 2]>,
    /// The share of the effect's plays it takes part in (`ActiveChance`).
    chance: f32,
    max: usize,
    offset: Vec3,
    /// The emitter's axis (Bevy space, the effect's own frame).
    axis: Quat,
    /// The heading's base angles and spreads (`EmitDir...`, radians).
    axis_angle: f32,
    axis_spread: f32,
    plane_angle: f32,
    plane_spread: f32,
    /// Where particles are born about the emitter (`EmitVol...`), its
    /// shape facing the camera (`ShapeTowardCamera`), and whether they head
    /// out from it (`DirFromVolumeSurface`).
    volume: Volume,
    toward_camera: bool,
    from_surface: bool,
    rate: Ranged,
    rate_ratio: Lut,
    /// How much of the effect's own speed a particle takes on at birth (its
    /// share, over the emitter's time) and keeps over its life.
    ext_speed: Ranged,
    ext_speed_ratio: Lut,
    part_ext_speed: Lut,
    life: Ranged,
    speed: Ranged,
    size: Ranged,
    gravity: Ranged,
    gravity_dir: Vec3,
    local_gravity: bool,
    angle: Ranged,
    spin: Ranged,
    part_speed: Lut,
    part_size: Lut,
    part_length: Lut,
    part_alpha: Lut,
    part_spin: Lut,
    part_gravity: Lut,
    alpha: f32,
    colours: Vec<(f32, Vec3)>,
    random_colour: bool,
    brightness: f32,
    uniform: bool,
    /// Where a sized-unevenly particle's quad hangs from its point (across,
    /// along: `PartSizeOffsetX`, `...Y`, in half its width and length).
    pivot: Vec2,
    /// Its particles move with the effect (`LocalEmitterSpace`: a muzzle's
    /// flames go with the gun).
    local: bool,
    /// Oriented in the effect's frame (`Orientation`): one that hardly moves
    /// (a flame, a standing wisp) stands along the frame's Z (Dunia's).
    oriented: bool,
    lit: bool,
    near: Option<(f32, f32)>,
    far: Option<(f32, f32)>,
    /// Shrunk nearing the camera (`CameraScalingDistance`,
    /// `CameraScaling`): from its size that far off to so much of it at the
    /// eye (a flamethrower's flames, fires' smoke, the sea's motes).
    camera_scale: Option<(f32, f32)>,
    light: Option<(Vec3, f32, f32, f32, f32)>,
}

fn dunia(v: &str) -> Vec3 {
    let p: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    if p.len() == 3 {
        crate::data::to_bevy([p[0], p[1], p[2]])
    } else {
        Vec3::ZERO
    }
}

fn gradient(colours: &[(f32, Vec3)], t: f32) -> Vec3 {
    let Some(first) = colours.first() else { return Vec3::ONE };
    if t <= first.0 {
        return first.1;
    }
    for pair in colours.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t <= b.0 && b.0 > a.0 {
            return a.1.lerp(b.1, (t - a.0) / (b.0 - a.0));
        }
    }
    colours.last().unwrap().1
}

impl Compiled {
    fn of(e: &Emitter) -> Compiled {
        let s = &e.source;
        let f = |n: &str, d: f32| s.f32(n).unwrap_or(d);
        let on = |n: &str| s.f32(n).is_some_and(|v| v != 0.0);
        // (Dunia's +Y turned by the offset: X first, -90 straight up.)
        let rx = s.attr("RotOffset").and_then(|r| r.split(',').next()).and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(0.0);
        let rz = s.attr("RotOffset").and_then(|r| r.split(',').nth(2)).and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(0.0);
        let axis = Quat::from_rotation_y(rz.to_radians()) * Quat::from_rotation_x((-rx).to_radians());
        let mode = if s.u32("BlendType") == Some(1) {
            0
        } else if on("RedAsAlpha") {
            2
        } else {
            1
        };
        // (Heat haze bends the view through its map; not drawn as sprites:
        // meshes, and haze without its map. A mesh's `.glm` is exported as
        // its `.xbg`.)
        let haze = if on("DistortionEnabled") { HazeKey::of(e) } else { None };
        let mesh = s
            .attr("Geometry")
            .filter(|g| !g.is_empty())
            .map(|g| fc3_formats::cache::model_rel(&format!("{}.xbg", g.rsplit_once('.').map_or(g, |(stem, _)| stem))));
        let drawn = mesh.is_none() && (haze.is_some() || !on("DistortionEnabled"));
        let soft = if on("Soft") { (s.f32("SoftRange").unwrap_or(0.0).max(0.0) * 100.0).round() as u16 } else { 0 };
        let batch = e.texture_path().ok().filter(|_| drawn && !e.texture.is_empty()).map(|t| BatchKey {
            texture: fc3_formats::cache::texture_rel(&t),
            mode,
            soft,
            haze,
        });
        // The gradient's points (`Knot`s), else its even samples (`Key`s).
        let gradient = s.child("PartColorKF");
        let mut colours: Vec<(f32, Vec3)> = gradient
            .into_iter()
            .flat_map(|c| &c.children)
            .filter(|k| k.name == "Knot")
            .filter_map(|k| Some((k.f32("Position")?, dunia_colour(k.attr("Color")?)?)))
            .collect();
        if colours.is_empty() {
            let keys: Vec<Vec3> =
                gradient.into_iter().flat_map(|c| &c.children).filter(|k| k.name == "Key").filter_map(|k| dunia_colour(k.attr("Value")?)).collect();
            let last = keys.len().saturating_sub(1).max(1) as f32;
            colours = keys.into_iter().enumerate().map(|(i, c)| (i as f32 / last, c)).collect();
        }
        let initial = s.child("InitColorKF").and_then(|c| c.child("Key")).and_then(|k| k.attr("Value")).and_then(dunia_colour).unwrap_or(Vec3::ONE);
        let colours = if colours.is_empty() { vec![(0.0, initial)] } else { colours.into_iter().map(|(p, c)| (p, c * initial)).collect() };
        let hdr = crate::vine_puff::curve(s, "EmitterHDRMulKF", 0.0, 1.0).max(0.0);
        let rate_ratio = s.child("EmitRateKF").and_then(|k| k.child("ValueKF")).map_or(Lut([1.0; 33]), |v| Lut::of(v, "RatioUI", 1.0));
        let ext_speed_ratio = s.child("InitExtSpdRatioKF").and_then(|k| k.child("ValueKF")).map_or(Lut([1.0; 33]), |v| Lut::of(v, "RatioUI", 1.0));
        Compiled {
            batch,
            mesh,
            child: None,
            frames: e.frames,
            fps: Ranged { value: e.fps, variant: e.fps_variant },
            random_start: on("FrameRandomStart"),
            play_once: on("FramePlayOnce"),
            duration: e.duration.max(0.01),
            delay: e.delay,
            looping: e.looping,
            burst: e.initial_count,
            burst_range: (f("NbInitPartMax", 0.0) >= 1.0).then(|| {
                let hi = f("NbInitPartMax", 0.0) as u32;
                [(f("NbInitPartMin", 0.0) as u32).min(hi), hi]
            }),
            chance: (f("ActiveChance", 100.0) / 100.0).clamp(0.0, 1.0),
            max: (f("StaticSize", 50.0) as usize).clamp(1, 400),
            offset: crate::data::to_bevy(e.offset),
            axis,
            axis_angle: f("EmitDirAxisAngle", 0.0).to_radians(),
            axis_spread: f("EmitDirAxisSpread", 0.0).to_radians(),
            plane_angle: f("EmitDirPlaneAngle", 0.0).to_radians(),
            plane_spread: f("EmitDirPlaneSpread", 0.0).to_radians(),
            volume: if f("EmitVolRadius", 0.0) > 0.0 {
                Volume::Sphere(f("EmitVolRadius", 0.0))
            } else {
                let half = Vec3::new(f("EmitVolHalfSizeX", 0.0), f("EmitVolHalfSizeY", 0.0), f("EmitVolHalfSizeZ", 0.0));
                if half.max_element() > 0.0 {
                    Volume::Box(half)
                } else {
                    Volume::Point
                }
            },
            toward_camera: s.child("EmitFlag").is_some_and(|e| e.bool("ShapeTowardCamera") == Some(true)),
            from_surface: s.child("EmitFlag").is_some_and(|e| e.bool("DirFromVolumeSurface") == Some(true)),
            rate: Ranged::of(e, "EmitRateKF"),
            rate_ratio,
            ext_speed: Ranged::of(e, "InitExtSpdRatioKF"),
            ext_speed_ratio,
            part_ext_speed: Lut::of(s, "PartExtSpdRatioKF", 1.0),
            life: Ranged::of(e, "InitLifeTimeKF"),
            speed: Ranged::of(e, "InitSpeedKF"),
            size: Ranged::of(e, "InitSizeKF"),
            gravity: Ranged::of(e, "InitGravityIntensityKF"),
            gravity_dir: dunia(s.attr("InitGravityDir").unwrap_or("0,0,-1")),
            local_gravity: on("LocalGravity"),
            angle: Ranged::of(e, "InitAnglesYKF"),
            spin: Ranged::of(e, "InitRotSpeedYKF"),
            part_speed: Lut::of(s, "PartSpeedKF", 1.0),
            part_size: Lut::of(s, "PartSizeKF", 1.0),
            part_length: Lut::of(s, "PartSizeZKF", 1.0),
            part_alpha: Lut::of(s, "PartAlphaKF", 1.0),
            part_spin: Lut::of(s, "PartRotSpeedKF", 1.0),
            part_gravity: Lut::of(s, "PartGravityKF", 1.0),
            alpha: crate::vine_puff::curve(s, "InitAlphaKF", 0.0, 1.0).clamp(0.0, 1.0)
                * crate::vine_puff::curve(s, "EmitterAlphaMulKF", 0.0, 1.0).clamp(0.0, 1.0),
            colours,
            random_colour: on("RandomColor"),
            // (Our light isn't the engine's: an HDR multiplier of 100 is
            // a few times as bright here, not a hundred.)
            brightness: 1.0 + hdr.max(1.0).log10() * 1.5,
            uniform: s.f32("UniformSize").is_none_or(|v| v != 0.0),
            pivot: Vec2::new(f("PartSizeOffsetX", 0.0), f("PartSizeOffsetY", 0.0)),
            local: s.child("Flags").is_some_and(|f| f.bool("LocalEmitterSpace") == Some(true)),
            oriented: s.child("Flags").is_some_and(|f| f.bool("Orientation") == Some(true)),
            lit: on("SHLighting") && mode != 0,
            // (Faded near the camera by its alpha (`NearClippingAlpha`: a
            // flamethrower's fireballs, impacts' smoke); far off by its alpha
            // or its rate, both of which we fade.)
            near: (f("NearClippingAlpha", 0.0) > 0.0).then(|| (f("NearClippingStart", 0.0), f("NearClippingEnd", 0.0))),
            far: (f("FarClippingRate", 0.0) > 0.0 || f("FarClippingAlpha", 0.0) > 0.0).then(|| (f("FarClippingStart", 0.0), f("FarClippingEnd", 0.0))),
            camera_scale: Some((f("CameraScalingDistance", 0.0), f("CameraScaling", 1.0))).filter(|&(d, s)| d > 0.0 && s != 1.0),
            light: on("LightActive").then(|| {
                let colour = dunia_colour(s.attr("LightColor").unwrap_or("1,1,1")).unwrap_or(Vec3::ONE);
                (colour, f("LightIntensity", 1.0), f("LightOuterRadius", 2.0), f("LightFlickeringFrequency", 0.0), f("LightFlickeringAmplitude", 0.0))
            }),
        }
    }
}

/// An emitter's volume (Dunia axes, metres).
#[derive(Clone, Copy)]
enum Volume {
    Point,
    Sphere(f32),
    Box(Vec3),
}

impl Volume {
    /// A point in it, Dunia axes, and on its surface for a sphere whose
    /// particles head out from it.
    fn sample(self, rng: &mut crate::weapons::Rng, surface: bool) -> Vec3 {
        match self {
            Volume::Point => Vec3::ZERO,
            Volume::Sphere(r) => {
                let dir = Vec3::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-1.0, 1.0)).normalize_or(Vec3::Z);
                dir * if surface { r } else { r * rng.f32().cbrt() }
            }
            Volume::Box(h) => Vec3::new(rng.range(-h.x, h.x), rng.range(-h.y, h.y), rng.range(-h.z, h.z)),
        }
    }
}

fn dunia_colour(v: &str) -> Option<Vec3> {
    let p: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    (p.len() == 3).then(|| Vec3::new(p[0], p[1], p[2]))
}

/// The effects read so far, their emitters, and the batches drawing them.
#[derive(Resource, Default)]
struct Library {
    effects: HashMap<&'static str, Option<Vec<usize>>>,
    emitters: Vec<Compiled>,
    batches: HashMap<BatchKey, Entity>,
    particles: Vec<Particle>,
    /// Mesh particles' models (by cache path), each with the entities that
    /// show them (as many as have lived at once; hidden when not needed).
    meshes: HashMap<String, (Handle<crate::cache_assets::Fc3Model>, Vec<Entity>)>,
}

impl Library {
    fn effect(&mut self, name: &'static str) -> Option<&Vec<usize>> {
        if !self.effects.contains_key(name) {
            let path = fc3_formats::cache::default_cache_root().join(fc3_formats::particle_effects::authored_rel(name));
            let read = std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|x| Effect::parse(&x));
            let ids = match read {
                Ok(effect) => {
                    let mut ids = Vec::new();
                    for e in effect.emitters.iter().filter(|e| e.active) {
                        let id = self.emitters.len();
                        ids.push(id);
                        self.emitters.push(Compiled::of(e));
                        // (Its child emits from its particles, not on the
                        // effect's clock.)
                        if let Some(child) = e.children().into_iter().find(|c| c.active) {
                            self.emitters[id].child = Some(self.emitters.len());
                            self.emitters.push(Compiled::of(&child));
                        }
                    }
                    debug!("authored effect {name}: {} emitters", ids.len());
                    Some(ids)
                }
                Err(e) => {
                    warn!("authored effect {name}: {e:#} (re-run the converter)");
                    None
                }
            };
            self.effects.insert(name, ids);
        }
        self.effects.get(name).and_then(|e| e.as_ref())
    }
}

struct Particle {
    emitter: usize,
    /// The effect that emitted it (each emitter's cap is its own).
    owner: Entity,
    /// For an emitter in its own space, where its effect is (its position,
    /// heading and fall then in the effect's frame), followed while the
    /// effect lasts.
    space: Option<bevy::math::Affine3A>,
    /// Its effect's Z (Dunia's; in the effect's frame for one in its own
    /// space), what an oriented one stands along.
    stand: Vec3,
    pos: Vec3,
    dir: Vec3,
    speed: f32,
    /// The effect's own motion it took on (`InitExtSpdRatioKF`).
    carried: Vec3,
    fall: Vec3,
    gravity: Vec3,
    life: f32,
    age: f32,
    size: f32,
    rot: f32,
    spin: f32,
    /// A mesh particle's turn as it was born (at random).
    turn: Quat,
    colour: Option<Vec3>,
    frame: f32,
    fps: f32,
    /// Its child emitter's count towards its next particle, and whether
    /// its burst has gone.
    child_carry: f32,
    child_burst: bool,
}

/// A particle born of an emitter, in a frame: turned `turn` at `place`
/// (with `space` the effect's whole transform, kept for an emitter in its
/// own space), taking on `carried`.
#[allow(clippy::too_many_arguments)]
fn birth(
    em: &Compiled,
    emitter: usize,
    owner: Entity,
    (turn, place, space): (Quat, Vec3, Option<bevy::math::Affine3A>),
    facing: Mat3,
    carried: Vec3,
    rng: &mut crate::weapons::Rng,
) -> Particle {
    // A heading within the spreads, about the emitter's axis (Dunia +Y,
    // Bevy -Z), or out from its volume.
    let cone = Quat::from_rotation_y(em.axis_angle + rng.range(-1.0, 1.0) * em.axis_spread)
        * Quat::from_rotation_x(em.plane_angle + rng.range(-1.0, 1.0) * em.plane_spread);
    let v = em.volume.sample(rng, em.from_surface);
    let born = if em.toward_camera { facing * v } else { turn * em.axis * Vec3::new(v.x, v.z, -v.y) };
    let dir = if em.from_surface && born.length_squared() > 1e-6 { born.normalize() } else { turn * em.axis * cone * Vec3::NEG_Z };
    let gravity = if em.local_gravity { turn * em.gravity_dir } else { em.gravity_dir } * em.gravity.roll(rng).max(0.0);
    let frames = (em.frames[0] * em.frames[1]).max(1) as f32;
    // (In its own space: kept in the effect's frame.)
    let back = turn.inverse();
    // (Dunia's Z is Bevy's local +Y.)
    let stand = em.axis * Vec3::Y;
    let (space, pos, dir, gravity, stand) = match space {
        Some(space) => (Some(space), em.offset + back * born, back * dir, back * gravity, stand),
        None => (None, place + turn * em.offset + born, dir, gravity, turn * stand),
    };
    Particle {
        emitter,
        owner,
        space,
        stand,
        pos,
        dir,
        speed: em.speed.roll(rng),
        carried,
        fall: Vec3::ZERO,
        gravity,
        life: em.life.roll(rng).max(0.02),
        age: 0.0,
        size: em.size.roll(rng).max(0.0),
        rot: em.angle.roll(rng).to_radians(),
        spin: em.spin.roll(rng).to_radians(),
        turn: if em.mesh.is_some() {
            let tau = std::f32::consts::TAU;
            Quat::from_euler(EulerRot::YXZ, rng.f32() * tau, rng.f32() * tau, rng.f32() * tau)
        } else {
            Quat::IDENTITY
        },
        colour: em.random_colour.then(|| gradient(&em.colours, rng.f32())),
        frame: if em.random_start { (rng.f32() * frames).floor() } else { 0.0 },
        fps: em.fps.roll(rng).max(0.0),
        child_carry: 0.0,
        child_burst: false,
    }
}

impl Particle {
    /// Where it is in the world (its effect's frame applied, in its own
    /// space).
    fn world_pos(&self) -> Vec3 {
        self.space.map_or(self.pos, |f| f.transform_point3(self.pos))
    }

    fn world_vector(&self, v: Vec3) -> Vec3 {
        self.space.map_or(v, |f| f.transform_vector3(v))
    }
}

#[allow(clippy::type_complexity)]
fn emit(
    time: Res<Time>,
    mut library: ResMut<Library>,
    mut rng: ResMut<crate::weapons::Rng>,
    mut effects: Query<(Entity, &AuthoredFx, &GlobalTransform, Option<&mut FxClock>, Has<Once>, Has<NoLights>)>,
    mut lights: Query<&mut PointLight, With<FxLight>>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    // (Shapes facing the camera: its right, its way back, its up.)
    let facing = cameras.single().map_or(Mat3::IDENTITY, |c| Mat3::from_cols(c.right().as_vec3(), c.back().as_vec3(), c.up().as_vec3()));
    for (entity, fx, at, clock, once, unlit) in &mut effects {
        let Some(ids) = library.effect(fx.0).cloned() else { continue };
        let Some(mut clock) = clock else {
            let active = ids.iter().map(|&id| rng.f32() < library.emitters[id].chance).collect();
            commands.entity(entity).insert(FxClock {
                carry: vec![0.0; ids.len()],
                burst: vec![false; ids.len()],
                active,
                lights: vec![None; ids.len()],
                last: Some(at.translation()),
                ..default()
            });
            continue;
        };
        clock.age += dt;
        let here = at.translation();
        clock.velocity = clock.last.map_or(Vec3::ZERO, |last| (here - last) / dt);
        clock.last = Some(here);
        // A one-off done: what it emitted lives out on its own.
        let looping = |em: &Compiled| em.looping && !once;
        if ids.iter().enumerate().all(|(k, &id)| {
            let em = &library.emitters[id];
            !clock.active[k] || (!looping(em) && clock.age > em.delay + em.duration)
        }) {
            commands.entity(entity).despawn();
            continue;
        }
        let (_, turn, place) = at.to_scale_rotation_translation();
        for p in library.particles.iter_mut().filter(|p| p.owner == entity && p.space.is_some()) {
            p.space = Some(at.affine());
        }
        for (k, &id) in ids.iter().enumerate() {
            if !clock.active[k] {
                continue;
            }
            let (spawn, light) = {
                let em = &library.emitters[id];
                let t = clock.age - em.delay;
                if t < 0.0 {
                    continue;
                }
                let running = looping(em) || t <= em.duration;
                let phase = if looping(em) { (t % em.duration) / em.duration } else { (t / em.duration).min(1.0) };
                let mut n = 0;
                if !clock.burst[k] {
                    clock.burst[k] = true;
                    n += em.burst_range.map_or(em.burst, |[lo, hi]| (lo + (rng.f32() * (hi - lo + 1) as f32) as u32).min(hi));
                }
                if running {
                    clock.carry[k] += (em.rate.roll(&mut rng) * em.rate_ratio.at(phase)).max(0.0) * dt;
                    while clock.carry[k] >= 1.0 {
                        clock.carry[k] -= 1.0;
                        n += 1;
                    }
                }
                (n, em.light.filter(|_| running))
            };
            // The emitter's light (the effect's child: it goes with it),
            // flickering as authored.
            if let Some((colour, intensity, radius, freq, amp)) = light.filter(|_| !unlit) {
                let flicker = 1.0 + amp * (clock.age * freq * std::f32::consts::TAU).sin() * rng.range(0.6, 1.0);
                match clock.lights[k].and_then(|l| lights.get_mut(l).ok()) {
                    Some(mut l) => l.intensity = intensity * LIGHT_SCALE * flicker.max(0.0),
                    None if clock.lights[k].is_none() => {
                        let l = commands
                            .spawn((
                                PointLight {
                                    color: Color::srgb(colour.x, colour.y, colour.z),
                                    intensity: intensity * LIGHT_SCALE,
                                    range: radius * 3.0,
                                    shadow_maps_enabled: false,
                                    ..default()
                                },
                                Transform::from_translation(library.emitters[id].offset),
                                FxLight,
                                ChildOf(entity),
                            ))
                            .id();
                        clock.lights[k] = Some(l);
                    }
                    None => {}
                }
            }
            let em = &library.emitters[id];
            if spawn == 0 || (em.batch.is_none() && em.mesh.is_none() && em.child.is_none()) {
                continue;
            }
            let alive = library.particles.iter().filter(|p| p.emitter == id && p.owner == entity).count();
            let room = library.emitters[id].max.saturating_sub(alive);
            for _ in 0..(spawn as usize).min(room) {
                let em = &library.emitters[id];
                // (Not in its own space: that follows the effect.)
                let carried = if em.local {
                    Vec3::ZERO
                } else {
                    let phase = ((clock.age - em.delay) / em.duration.max(1e-3)).clamp(0.0, 1.0);
                    clock.velocity * em.ext_speed.roll(&mut rng).max(0.0) * em.ext_speed_ratio.at(phase)
                };
                let p = birth(em, id, entity, (turn, place, em.local.then(|| at.affine())), facing, carried, &mut rng);
                library.particles.push(p);
            }
        }
    }
    // Child emitters: each of a parent's particles emits as its emitter
    // would (a burst, then its rate over its time), its clock the
    // particle's age, from where the particle is, heading about the way it
    // goes (the child's +Y, Dunia's) and taking on its speed.
    let mut born = Vec::new();
    {
        let Library { emitters, particles, .. } = &mut *library;
        for p in particles.iter_mut() {
            let Some(cid) = emitters[p.emitter].child else { continue };
            let c = &emitters[cid];
            let t = p.age - c.delay;
            if t < 0.0 {
                continue;
            }
            let mut n = 0;
            if !p.child_burst {
                p.child_burst = true;
                n += c.burst;
            }
            if c.looping || t <= c.duration {
                let phase = if c.looping { (t % c.duration) / c.duration } else { (t / c.duration).min(1.0) };
                p.child_carry += (c.rate.roll(&mut rng) * c.rate_ratio.at(phase)).max(0.0) * dt;
                while p.child_carry >= 1.0 {
                    p.child_carry -= 1.0;
                    n += 1;
                }
            }
            if n > 0 {
                let em = &emitters[p.emitter];
                let s = (p.age / p.life).clamp(0.0, 1.0);
                let velocity = p.world_vector(p.dir * p.speed * em.part_speed.at(s) + p.carried * em.part_ext_speed.at(s) + p.fall);
                born.push((cid, p.owner, p.world_pos(), velocity, n));
            }
        }
    }
    for (cid, owner, at, velocity, n) in born {
        // (As many as its pool holds for each of its parent's.)
        let alive = library.particles.iter().filter(|p| p.emitter == cid && p.owner == owner).count();
        let room = (library.emitters[cid].max * CHILD_POOLS).saturating_sub(alive);
        let turn = Quat::from_rotation_arc(Vec3::NEG_Z, velocity.normalize_or(Vec3::Y));
        for _ in 0..(n as usize).min(room) {
            let c = &library.emitters[cid];
            let carried = velocity * c.ext_speed.roll(&mut rng).max(0.0) * c.ext_speed_ratio.at(0.0);
            let p = birth(c, cid, owner, (turn, at, None), facing, carried, &mut rng);
            library.particles.push(p);
        }
    }
}

/// How many pools (`StaticSize`) of a child emitter's particles may live at
/// once in one effect: each parent particle has its own in the engine, and
/// an effect has a few parents at a time.
const CHILD_POOLS: usize = 8;

/// An authored light's intensity (the engine's units) in ours.
const LIGHT_SCALE: f32 = 2.0e4;

fn simulate(time: Res<Time>, mut library: ResMut<Library>, effects: Query<&AuthoredFx>, mut last: Local<f32>) {
    // `FC3_FX_DEBUG`: how many effects play and particles live, each 2 s.
    if std::env::var_os("FC3_FX_DEBUG").is_some() && time.elapsed_secs() - *last > 2.0 {
        *last = time.elapsed_secs();
        let mut by: HashMap<&str, usize> = HashMap::new();
        for p in &library.particles {
            *by.entry(library.effects.iter().find(|(_, ids)| ids.as_ref().is_some_and(|i| i.contains(&p.emitter))).map_or("?", |(n, _)| *n)).or_default() += 1;
        }
        let mut top: Vec<_> = by.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        top.truncate(6);
        info!("authored fx: {} playing, {} particles, most {:?}", effects.iter().count(), library.particles.len(), top);
    }
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let Library { emitters, particles, .. } = &mut *library;
    particles.retain_mut(|p| {
        p.age += dt;
        if p.age >= p.life {
            return false;
        }
        let em = &emitters[p.emitter];
        let t = p.age / p.life;
        p.fall += p.gravity * em.part_gravity.at(t) * dt;
        p.pos += (p.dir * p.speed * em.part_speed.at(t) + p.carried * em.part_ext_speed.at(t) + p.fall) * dt;
        p.rot += p.spin * em.part_spin.at(t) * dt;
        true
    });
}

fn load_sheet(server: &AssetServer, rel: &str) -> Handle<Image> {
    server
        .load_builder()
        .with_settings(|s: &mut ImageLoaderSettings| {
            s.is_srgb = true;
            s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::ClampToEdge,
                address_mode_v: ImageAddressMode::ClampToEdge,
                mag_filter: ImageFilterMode::Linear,
                min_filter: ImageFilterMode::Linear,
                mipmap_filter: ImageFilterMode::Linear,
                ..default()
            });
        })
        .load(crate::cache_assets::cache_path(rel))
}

/// A heat haze's distortion map: its offsets as they are (not colour), tiled.
fn load_map(server: &AssetServer, rel: &str) -> Handle<Image> {
    server
        .load_builder()
        .with_settings(|s: &mut ImageLoaderSettings| {
            s.is_srgb = false;
            s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::Repeat,
                address_mode_v: ImageAddressMode::Repeat,
                mag_filter: ImageFilterMode::Linear,
                min_filter: ImageFilterMode::Linear,
                mipmap_filter: ImageFilterMode::Linear,
                ..default()
            });
        })
        .load(crate::cache_assets::cache_path(rel))
}

/// How much of its size a particle so far from the camera keeps (its
/// emitter's camera scaling: all of it from that distance on).
fn camera_scale(em: &Compiled, dist: f32) -> f32 {
    em.camera_scale.map_or(1.0, |(d, s)| s + (1.0 - s) * (dist / d).clamp(0.0, 1.0))
}

/// A batch's mesh: its particles as quads round the camera.
#[derive(Component)]
struct FxBatch(BatchKey);

/// One of a mesh particle model's entities.
#[derive(Component)]
struct FxMesh;

#[allow(clippy::too_many_arguments)]
fn draw(
    mut library: ResMut<Library>,
    daylight: Res<crate::clouds::Daylight>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut batches: Query<(&FxBatch, &Mesh3d, &mut Transform, &mut Visibility), Without<FxMesh>>,
    (mut shown, models): (Query<(&mut Transform, &mut Visibility), (With<FxMesh>, Without<FxBatch>)>, Res<Assets<crate::cache_assets::Fc3Model>>),
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ParticleMaterial>>,
    mut hazes: ResMut<Assets<crate::particles::HazeMaterial>>,
    server: Res<AssetServer>,
    mut commands: Commands,
) {
    let Ok(cam) = cameras.single() else { return };
    // Batches for sheets not yet drawn.
    let wanted: Vec<BatchKey> = library.emitters.iter().filter_map(|e| e.batch.clone()).filter(|k| !library.batches.contains_key(k)).collect();
    for key in wanted {
        if library.batches.contains_key(&key) {
            continue;
        }
        let mut batch = commands.spawn((
            FxBatch(key.clone()),
            Mesh3d(meshes.add(empty_mesh())),
            Transform::default(),
            Visibility::Hidden,
            NotShadowCaster,
            NotShadowReceiver,
            bevy::camera::visibility::NoFrustumCulling,
            Name::new("authored fx"),
        ));
        let (shape, soft) = (load_sheet(&server, &key.texture), key.soft as f32 / 100.0);
        match &key.haze {
            Some(haze) => batch.insert(MeshMaterial3d(hazes.add(haze.material(shape, load_map(&server, &haze.map), soft)))),
            None => batch.insert(MeshMaterial3d(materials.add(ParticleMaterial::plain(key.mode as f32, shape).soft(soft)))),
        };
        let e = batch.id();
        library.batches.insert(key, e);
    }
    let origin = cam.translation();
    let right = cam.right().as_vec3();
    let up = cam.up().as_vec3();
    let view = cam.forward().as_vec3();
    let ambient = 0.12 + 0.88 * daylight.0;
    for (batch, mesh, mut tr, mut vis) in &mut batches {
        tr.translation = origin;
        let mut live: Vec<(&Particle, Vec3)> =
            library.particles.iter().filter(|p| library.emitters[p.emitter].batch.as_ref() == Some(&batch.0)).map(|p| (p, p.world_pos())).collect();
        if batch.0.mode != 0 {
            live.sort_by(|a, b| b.1.distance_squared(origin).total_cmp(&a.1.distance_squared(origin)));
        }
        let mut quads = Quads::default();
        for (p, pos) in live {
            let em = &library.emitters[p.emitter];
            let t = (p.age / p.life).clamp(0.0, 1.0);
            let dist = pos.distance(origin);
            let mut alpha = em.alpha * em.part_alpha.at(t).clamp(0.0, 1.0);
            if let Some((a, b)) = em.near {
                alpha *= crate::sky::smoothstep(a, b.max(a + 0.01), dist);
            }
            if let Some((a, b)) = em.far {
                alpha *= 1.0 - crate::sky::smoothstep(a, b.max(a + 0.01), dist);
            }
            if alpha <= 0.002 {
                continue;
            }
            let colour = p.colour.unwrap_or_else(|| gradient(&em.colours, t));
            let light = if em.lit { ambient } else { 1.0 };
            // (What glows: light added, and brightness as coverage.)
            let colour = colour * light * if batch.0.mode != 1 { em.brightness } else { 1.0 };
            // (A size is the particle's half-extent, its radius.)
            let near = camera_scale(em, dist);
            let half = (p.size * em.part_size.at(t) * near).max(0.005);
            let mut centre = pos - origin;
            let (x, y) = if !em.uniform {
                // Sized unevenly (sparks, streaks of smoke): its width
                // across and its length (`PartSizeZKF`) along its heading as
                // seen, hung from its point by its pivot.
                let vel = p.world_vector(p.dir * p.speed * em.part_speed.at(t) + p.fall);
                let heading = if em.oriented && vel.length_squared() < 0.25 {
                    p.world_vector(p.stand)
                } else if vel.length_squared() > 1e-4 {
                    vel
                } else {
                    p.world_vector(p.dir)
                };
                let along = (heading - view * heading.dot(view)).normalize_or(up);
                let side = view.cross(along).normalize_or(right);
                let (width, length) = (half.max(0.003), (p.size * em.part_length.at(t) * near).max(0.003));
                centre += side * (width * em.pivot.x) + along * (length * em.pivot.y);
                (side * width, along * length)
            } else {
                let (s, c) = p.rot.sin_cos();
                (right * (c * half) + up * (s * half), up * (c * half) - right * (s * half))
            };
            let n = (em.frames[0] * em.frames[1]).max(1);
            let frame = if n <= 1 {
                0
            } else if p.fps <= 0.0 {
                p.frame as u32 % n
            } else if em.play_once {
                ((p.frame + p.age * p.fps) as u32).min(n - 1)
            } else {
                (p.frame + p.age * p.fps) as u32 % n
            };
            quads.push(centre, x, y, frame_uv(frame, em.frames[0].max(1), em.frames[1].max(1)), -view, colour.extend(alpha));
        }
        let empty = quads.is_empty();
        let want = if empty { Visibility::Hidden } else { Visibility::Inherited };
        if *vis != want {
            *vis = want;
        }
        if !empty {
            if let Some(mut m) = meshes.get_mut(&mesh.0) {
                quads.write(&mut m);
            }
        }
    }
    // Mesh particles: their model where each is, turned as it was born and
    // sized by its size (a model a metre or so in radius), while it has a size
    // and isn't beyond its far fade.
    let mut places: HashMap<String, Vec<Transform>> = HashMap::new();
    for p in &library.particles {
        let em = &library.emitters[p.emitter];
        let Some(model) = &em.mesh else { continue };
        let pos = p.world_pos();
        let scale = p.size * em.part_size.at((p.age / p.life).clamp(0.0, 1.0)) * camera_scale(em, pos.distance(origin));
        if scale <= 0.002 || em.far.is_some_and(|(_, end)| pos.distance(origin) > end) {
            continue;
        }
        let up = p.world_vector(Vec3::Y).normalize_or(Vec3::Y);
        let rotation = Quat::from_axis_angle(up, p.rot) * p.turn;
        places.entry(model.clone()).or_default().push(Transform { translation: pos, rotation, scale: Vec3::splat(scale) });
    }
    for (model, (_, pool)) in &library.meshes {
        if !places.contains_key(model) {
            for e in pool {
                if let Ok((_, mut vis)) = shown.get_mut(*e) {
                    *vis = Visibility::Hidden;
                }
            }
        }
    }
    for (model, list) in places {
        let (handle, pool) = library.meshes.entry(model.clone()).or_insert_with(|| (server.load(crate::cache_assets::cache_path(&model)), Vec::new()));
        let Some(lod) = models.get(&*handle).and_then(|m| m.lods.first()) else { continue };
        while pool.len() < list.len() {
            let e = commands.spawn((FxMesh, Transform::default(), Visibility::Hidden, Name::new("authored fx mesh"))).id();
            for part in &lod.parts {
                commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), NotShadowCaster, ChildOf(e)));
            }
            pool.push(e);
        }
        for (i, e) in pool.iter().enumerate() {
            match (list.get(i), shown.get_mut(*e)) {
                (Some(place), Ok((mut t, mut vis))) => {
                    *t = *place;
                    *vis = Visibility::Inherited;
                }
                (Some(place), Err(_)) => {
                    commands.entity(*e).insert((*place, Visibility::Inherited));
                }
                (None, Ok((_, mut vis))) => *vis = Visibility::Hidden,
                (None, Err(_)) => {}
            }
        }
    }
}
