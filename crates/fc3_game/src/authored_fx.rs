//! The game's own particle effects (`particle_effects::AUTHORED_EFFECTS`),
//! played as authored: an entity with [`AuthoredFx`] plays its effect where
//! it stands, each emitter as its definition has it.
//!
//! An emitter waits its delay, then emits for its duration (again and again
//! when it loops): a burst at its start (`NbInitPart`) and a rate
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
//! point light, flickering as authored.
//!
//! Not played (yet): the emitters' soft intersections, distortion, child
//! emitters, emission volumes and sounds; curves are joined linearly between
//! their knots.

use std::collections::HashMap;

use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use fc3_formats::particle_effects::{Effect, Emitter};
use fc3_formats::xml::Element;

use crate::particles::{empty_mesh, frame_uv, ParticleMaterial, Quads};
use crate::player::PlayerCamera;

pub struct AuthoredFxPlugin;

impl Plugin for AuthoredFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Library>().add_systems(Update, (fx_test, emit, simulate).chain()).add_systems(PostUpdate, draw.before(TransformSystems::Propagate));
    }
}

/// `FC3_AUTHORED_FX=<name>` (tests): that effect played on the ground four
/// metres before the player, once the player is in the world.
fn fx_test(players: Query<(&Transform, &crate::player::Player)>, mut done: Local<bool>, mut commands: Commands) {
    if *done {
        return;
    }
    let Ok(name) = std::env::var("FC3_AUTHORED_FX") else {
        *done = true;
        return;
    };
    let Ok((t, p)) = players.single() else { return };
    if p.spawning {
        return;
    }
    *done = true;
    let ahead = Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z * 4.0;
    let at = t.translation + ahead - Vec3::Y * crate::player::HALF_HEIGHT;
    let name: &'static str = Box::leak(name.into_boxed_str());
    commands.spawn((Transform::from_translation(at), AuthoredFx(name)));
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

/// Plays an authored effect (by its name) where the entity stands, turned
/// as it is; taken off (or despawned), it stops emitting and what it has
/// emitted lives out.
#[derive(Component, Clone)]
pub struct AuthoredFx(pub &'static str);

/// An effect's emitters' clocks.
#[derive(Component, Default)]
struct FxClock {
    age: f32,
    carry: Vec<f32>,
    burst: Vec<bool>,
    lights: Vec<Option<Entity>>,
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

/// What a batch draws: a texture, its grid and how it blends.
#[derive(Clone, PartialEq, Eq, Hash)]
struct BatchKey {
    texture: String,
    mode: u8,
}

/// An emitter made ready to play.
struct Compiled {
    batch: Option<BatchKey>,
    frames: [u32; 2],
    fps: Ranged,
    random_start: bool,
    play_once: bool,
    duration: f32,
    delay: f32,
    looping: bool,
    burst: u32,
    max: usize,
    offset: Vec3,
    /// The emitter's axis (Bevy space, the effect's own frame).
    axis: Quat,
    axis_spread: f32,
    plane_spread: f32,
    rate: Ranged,
    rate_ratio: Lut,
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
    lit: bool,
    near: Option<(f32, f32)>,
    far: Option<(f32, f32)>,
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
        let batch = e.texture_path().ok().filter(|_| !e.texture.is_empty()).map(|t| BatchKey { texture: fc3_formats::cache::texture_rel(&t), mode });
        let colours: Vec<(f32, Vec3)> = s
            .child("PartColorKF")
            .into_iter()
            .flat_map(|c| &c.children)
            .filter(|k| k.name == "Knot")
            .filter_map(|k| Some((k.f32("Position")?, dunia_colour(k.attr("Color")?)?)))
            .collect();
        let initial = s.child("InitColorKF").and_then(|c| c.child("Key")).and_then(|k| k.attr("Value")).and_then(dunia_colour).unwrap_or(Vec3::ONE);
        let colours = if colours.is_empty() { vec![(0.0, initial)] } else { colours.into_iter().map(|(p, c)| (p, c * initial)).collect() };
        let hdr = crate::vine_puff::curve(s, "EmitterHDRMulKF", 0.0, 1.0).max(0.0);
        let rate_ratio = s.child("EmitRateKF").and_then(|k| k.child("ValueKF")).map_or(Lut([1.0; 33]), |v| Lut::of(v, "RatioUI", 1.0));
        Compiled {
            batch,
            frames: e.frames,
            fps: Ranged { value: e.fps, variant: e.fps_variant },
            random_start: on("FrameRandomStart"),
            play_once: on("FramePlayOnce"),
            duration: e.duration.max(0.01),
            delay: e.delay,
            looping: e.looping,
            burst: e.initial_count,
            max: (f("StaticSize", 50.0) as usize).clamp(1, 400),
            offset: crate::data::to_bevy(e.offset),
            axis,
            axis_spread: f("EmitDirAxisSpread", 0.0).to_radians(),
            plane_spread: f("EmitDirPlaneSpread", 0.0).to_radians(),
            rate: Ranged::of(e, "EmitRateKF"),
            rate_ratio,
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
            lit: on("SHLighting") && mode != 0,
            near: (f("NearClippingRate", 0.0) > 0.0 && f("NearClippingAlpha", 0.0) > 0.0).then(|| (f("NearClippingStart", 0.0), f("NearClippingEnd", 0.0))),
            far: (f("FarClippingRate", 0.0) > 0.0).then(|| (f("FarClippingStart", 0.0), f("FarClippingEnd", 0.0))),
            light: on("LightActive").then(|| {
                let colour = dunia_colour(s.attr("LightColor").unwrap_or("1,1,1")).unwrap_or(Vec3::ONE);
                (colour, f("LightIntensity", 1.0), f("LightOuterRadius", 2.0), f("LightFlickeringFrequency", 0.0), f("LightFlickeringAmplitude", 0.0))
            }),
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
                        ids.push(self.emitters.len());
                        self.emitters.push(Compiled::of(e));
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
    pos: Vec3,
    dir: Vec3,
    speed: f32,
    fall: Vec3,
    gravity: Vec3,
    life: f32,
    age: f32,
    size: f32,
    rot: f32,
    spin: f32,
    colour: Option<Vec3>,
    frame: f32,
    fps: f32,
}

#[allow(clippy::type_complexity)]
fn emit(
    time: Res<Time>,
    mut library: ResMut<Library>,
    mut rng: ResMut<crate::weapons::Rng>,
    mut effects: Query<(Entity, &AuthoredFx, &GlobalTransform, Option<&mut FxClock>)>,
    mut lights: Query<&mut PointLight, With<FxLight>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    for (entity, fx, at, clock) in &mut effects {
        let Some(ids) = library.effect(fx.0).cloned() else { continue };
        let Some(mut clock) = clock else {
            commands.entity(entity).insert(FxClock { carry: vec![0.0; ids.len()], burst: vec![false; ids.len()], lights: vec![None; ids.len()], ..default() });
            continue;
        };
        clock.age += dt;
        let (_, turn, place) = at.to_scale_rotation_translation();
        for (k, &id) in ids.iter().enumerate() {
            let (spawn, light) = {
                let em = &library.emitters[id];
                let t = clock.age - em.delay;
                if t < 0.0 {
                    continue;
                }
                let running = em.looping || t <= em.duration;
                let phase = if em.looping { (t % em.duration) / em.duration } else { (t / em.duration).min(1.0) };
                let mut n = 0;
                if !clock.burst[k] {
                    clock.burst[k] = true;
                    n += em.burst;
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
            if let Some((colour, intensity, radius, freq, amp)) = light {
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
            if spawn == 0 || library.emitters[id].batch.is_none() {
                continue;
            }
            let alive = library.particles.iter().filter(|p| p.emitter == id && p.owner == entity).count();
            let room = library.emitters[id].max.saturating_sub(alive);
            for _ in 0..(spawn as usize).min(room) {
                let em = &library.emitters[id];
                // A heading within the spreads, about the emitter's axis
                // (Dunia +Y, Bevy -Z).
                let cone = Quat::from_rotation_y(rng.range(-1.0, 1.0) * em.axis_spread) * Quat::from_rotation_x(rng.range(-1.0, 1.0) * em.plane_spread);
                let dir = turn * em.axis * cone * Vec3::NEG_Z;
                let gravity = if em.local_gravity { turn * em.gravity_dir } else { em.gravity_dir } * em.gravity.roll(&mut rng).max(0.0);
                let frames = (em.frames[0] * em.frames[1]).max(1) as f32;
                let p = Particle {
                    emitter: id,
                    owner: entity,
                    pos: place + turn * em.offset,
                    dir,
                    speed: em.speed.roll(&mut rng).max(0.0),
                    fall: Vec3::ZERO,
                    gravity,
                    life: em.life.roll(&mut rng).max(0.02),
                    age: 0.0,
                    size: em.size.roll(&mut rng).max(0.0),
                    rot: em.angle.roll(&mut rng).to_radians(),
                    spin: em.spin.roll(&mut rng).to_radians(),
                    colour: em.random_colour.then(|| gradient(&em.colours, rng.f32())),
                    frame: if em.random_start { (rng.f32() * frames).floor() } else { 0.0 },
                    fps: em.fps.roll(&mut rng).max(0.0),
                };
                library.particles.push(p);
            }
        }
    }
}

/// An authored light's intensity (the engine's units) in ours.
const LIGHT_SCALE: f32 = 2.0e4;

fn simulate(time: Res<Time>, mut library: ResMut<Library>) {
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
        p.pos += (p.dir * p.speed * em.part_speed.at(t) + p.fall) * dt;
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

/// A batch's mesh: its particles as quads round the camera.
#[derive(Component)]
struct FxBatch(BatchKey);

#[allow(clippy::too_many_arguments)]
fn draw(
    mut library: ResMut<Library>,
    daylight: Res<crate::clouds::Daylight>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut batches: Query<(&FxBatch, &Mesh3d, &mut Transform, &mut Visibility)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ParticleMaterial>>,
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
        let material = materials.add(ParticleMaterial::plain(key.mode as f32, load_sheet(&server, &key.texture)));
        let e = commands
            .spawn((
                FxBatch(key.clone()),
                Mesh3d(meshes.add(empty_mesh())),
                MeshMaterial3d(material),
                Transform::default(),
                Visibility::Hidden,
                NotShadowCaster,
                NotShadowReceiver,
                bevy::camera::visibility::NoFrustumCulling,
                Name::new("authored fx"),
            ))
            .id();
        library.batches.insert(key, e);
    }
    let origin = cam.translation();
    let right = cam.right().as_vec3();
    let up = cam.up().as_vec3();
    let view = cam.forward().as_vec3();
    let ambient = 0.12 + 0.88 * daylight.0;
    for (batch, mesh, mut tr, mut vis) in &mut batches {
        tr.translation = origin;
        let mut live: Vec<&Particle> = library.particles.iter().filter(|p| library.emitters[p.emitter].batch.as_ref() == Some(&batch.0)).collect();
        if batch.0.mode != 0 {
            live.sort_by(|a, b| b.pos.distance_squared(origin).total_cmp(&a.pos.distance_squared(origin)));
        }
        let mut quads = Quads::default();
        for p in live {
            let em = &library.emitters[p.emitter];
            let t = (p.age / p.life).clamp(0.0, 1.0);
            let dist = p.pos.distance(origin);
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
            let colour = colour * light * if batch.0.mode == 0 { em.brightness } else { 1.0 };
            let half = (p.size * em.part_size.at(t) * 0.5).max(0.005);
            let (x, y) = if !em.uniform {
                // A streak along its motion as seen.
                let vel = p.dir * p.speed * em.part_speed.at(t) + p.fall;
                let along = (vel - view * vel.dot(view)).normalize_or(up);
                let side = view.cross(along).normalize_or(right);
                let width = half.max(0.012);
                (side * width, along * (width + vel.length() * 0.02 * em.part_length.at(t)))
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
            quads.push(p.pos - origin, x, y, frame_uv(frame, em.frames[0].max(1), em.frames[1].max(1)), -view, colour.extend(alpha));
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
}
