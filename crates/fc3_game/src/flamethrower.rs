//! The flamethrower's own effects (its `FireStrategyProperties`, through the
//! weapons' table): its nozzle's, on the gun's muzzle (alight and idle while
//! held, shooting while the trigger is down, stopping as it's let go), and
//! those it sends down the stream while it fires, each launched every so
//! often (more often as the aim swings) at its speed for its distance or
//! until the stream strikes something, where it stops. Our flame stream
//! stands in when none of them is cached. Its flame mesh glows round the
//! stream: a tube round a rope of knots sent from the nozzle, which keeps
//! straight ahead of the gun near it and swings out behind as the aim
//! turns.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use avian3d::prelude::*;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::Indices;
use bevy::prelude::*;
use fc3_formats::weapons::FlameRope;

use crate::particles::ParticleMaterial;
use crate::weapons::{Inventory, Muzzle};

pub struct FlamethrowerPlugin;

impl Plugin for FlamethrowerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (player_flame, fly, ropes).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// A flamethrower's flame: its nozzle's effects (first person, then third)
/// and those it sends down the stream.
#[derive(Clone, Debug, Default)]
pub struct Flame {
    pub idle: Option<&'static str>,
    pub shoot: Option<&'static str>,
    pub stop: Option<&'static str>,
    pub shoot_3p: Option<&'static str>,
    pub stop_3p: Option<&'static str>,
    pub stream: Vec<Stream>,
    /// Its flame mesh, if it draws one.
    pub rope: Option<Arc<FlameRope>>,
}

/// An effect sent down the stream: one every so often (s; by how fast the
/// aim turns), going so fast (m/s) so far (m).
#[derive(Clone, Debug)]
pub struct Stream {
    pub effect: Option<&'static str>,
    pub effect_3p: Option<&'static str>,
    pub timing: fc3_formats::weapons::StreamEffect,
}

impl Flame {
    pub fn from_stats(f: &fc3_formats::weapons::Flame) -> Flame {
        let name = |n: &str| (!n.is_empty()).then(|| crate::authored_fx::intern(n));
        Flame {
            idle: name(&f.tip_idle),
            shoot: name(&f.tip_shoot),
            stop: name(&f.tip_stop),
            shoot_3p: name(&f.tip_shoot_3p),
            stop_3p: name(&f.tip_stop_3p),
            stream: f
                .stream
                .iter()
                .filter(|s| s.every > 0.0 && s.speed > 0.0)
                .map(|s| Stream { effect: name(&s.effect), effect_3p: name(&s.effect_3p), timing: s.clone() })
                .collect(),
            rope: f.rope.clone().map(Arc::new),
        }
    }

    /// Whether any of its stream's effects is cached (else our stream
    /// stands in).
    pub fn streams(&self, third_person: bool) -> bool {
        self.stream.iter().filter_map(|s| if third_person { s.effect_3p } else { s.effect }).any(crate::authored_fx::available)
    }
}

/// The most stream effects in flight at once.
const MAX_IN_FLIGHT: usize = 320;

/// A stream effect on its way, and the seconds it has left to go (where it
/// stops: what it emitted lives out).
#[derive(Component)]
struct Launched {
    velocity: Vec3,
    left: f32,
}

/// The player's flame: the nozzle's effect playing, whether he was firing,
/// each stream effect's clock, and where he aimed last frame.
#[derive(Default)]
struct PlayerFlame {
    tip: Option<(Entity, &'static str)>,
    firing: bool,
    clocks: Vec<f32>,
    aim: Option<Vec3>,
}

#[allow(clippy::too_many_arguments)]
fn player_flame(
    time: Res<Time>,
    players: Query<(Entity, &Inventory, &crate::player::Player)>,
    muzzles: Query<(Entity, &GlobalTransform), With<Muzzle>>,
    cameras: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    in_flight: Query<(), With<Launched>>,
    spatial: SpatialQuery,
    mut state: Local<PlayerFlame>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let held = players.single().ok().filter(|(_, inv, _)| inv.switching <= 0.0 && inv.slots.get(inv.current).is_some()).and_then(|(e, inv, p)| {
        let d = inv.def();
        // (Firing while its shots keep coming.)
        d.flame.clone().map(|f| (e, f, inv.since_shot < d.interval * 2.5, p.velocity))
    });
    let muzzle = muzzles.single().ok();
    let (want, firing) = match (&held, muzzle) {
        (Some((_, f, firing, _)), Some(_)) => (if *firing { f.shoot } else { f.idle }, *firing),
        _ => (None, false),
    };
    // The nozzle: alight, shooting, or nothing in hand.
    if state.tip.map(|t| t.1) != want {
        if let Some((e, _)) = state.tip.take() {
            commands.entity(e).try_despawn();
        }
        if let (Some(name), Some((m, _))) = (want, muzzle) {
            state.tip = crate::authored_fx::attach_muzzle(&mut commands, name, m).map(|e| (e, name));
        }
    }
    if state.firing && !firing {
        if let (Some((_, f, _, _)), Some((m, _))) = (&held, muzzle) {
            if let Some(stop) = f.stop {
                crate::authored_fx::play_muzzle(&mut commands, stop, m);
            }
        }
    }
    state.firing = firing;
    // The stream, while it fires: each effect launched along the aim from
    // the nozzle, as far as it reaches.
    let (Some((player, f, true, velocity)), Some((_, at)), Ok(cam)) = (&held, muzzle, cameras.single()) else {
        state.clocks.clear();
        state.aim = None;
        return;
    };
    let from = at.translation();
    let forward = cam.forward();
    // (How fast the aim swings, deg/s.)
    let turn = state.aim.map_or(0.0, |a| a.angle_between(forward.as_vec3()).to_degrees() / dt.max(1e-3));
    state.aim = Some(forward.as_vec3());
    let filter = SpatialQueryFilter::from_excluded_entities([*player]);
    let reach = spatial.cast_ray(from, forward, 60.0, true, &filter).map(|h| h.distance).unwrap_or(f32::MAX);
    if let Some(rope) = &f.rope {
        feed_rope(&mut commands, *player, rope.clone(), from, forward.as_vec3(), *velocity, reach, 0.05);
    }
    let frame = crate::authored_fx::muzzle_frame(forward.as_vec3());
    let mut flying = in_flight.iter().count();
    state.clocks.resize(f.stream.len(), 0.0);
    for (s, clock) in f.stream.iter().zip(state.clocks.iter_mut()) {
        *clock += dt;
        let Some(name) = s.effect.filter(|n| crate::authored_fx::available(n)) else { continue };
        let t = &s.timing;
        let every = (t.every * t.turning_ratio(turn)).max(1e-3);
        let left = t.distance.min(reach) / t.speed;
        while *clock >= every {
            *clock -= every;
            if flying >= MAX_IN_FLIGHT {
                continue;
            }
            // (Each from where it would be had it left on its tick.)
            let start = from + forward * (t.speed * *clock).min(t.distance.min(reach));
            commands.spawn((
                Transform::from_translation(start).with_rotation(frame),
                crate::authored_fx::AuthoredFx(name),
                Launched { velocity: forward * t.speed + *velocity, left: (left - *clock).max(0.0) },
            ));
            flying += 1;
        }
    }
}

/// A pulse of a flamer's flame (the characters' `LPO-50`), seen from
/// outside: each third-person stream effect launched as often as it goes
/// in `span` seconds, staggered along the stream as if each had left on
/// its tick. False when none is cached (our stream stands in).
pub fn launch_3p(commands: &mut Commands, flame: &Flame, from: Vec3, dir: Vec3, velocity: Vec3, reach: f32, span: f32) -> bool {
    let frame = crate::authored_fx::muzzle_frame(dir);
    let mut any = false;
    for s in &flame.stream {
        let Some(name) = s.effect_3p.filter(|n| crate::authored_fx::available(n)) else { continue };
        let t = &s.timing;
        let every = (t.every * t.turning_ratio(0.0)).max(1e-3);
        let end = t.distance.min(reach);
        for i in 0..((span / every).round() as usize).clamp(1, 8) {
            let gone = (t.speed * i as f32 * every).min(end);
            commands.spawn((
                Transform::from_translation(from + dir * gone).with_rotation(frame),
                crate::authored_fx::AuthoredFx(name),
                Launched { velocity: dir * t.speed + velocity, left: (end - gone) / t.speed },
            ));
        }
        any = true;
    }
    any
}

/// Feeds a flame's mesh for `span` seconds: from its owner's nozzle, along
/// its aim, as far as it reaches (a new rope if the owner has none going).
#[allow(clippy::too_many_arguments)]
pub fn feed_rope(commands: &mut Commands, owner: Entity, def: Arc<FlameRope>, from: Vec3, dir: Vec3, velocity: Vec3, reach: f32, span: f32) {
    commands.spawn(RopeFeed { owner, def, from, dir, velocity, reach, span });
}

/// A feed for a flame's mesh, taken up by `ropes`.
#[derive(Component)]
struct RopeFeed {
    owner: Entity,
    def: Arc<FlameRope>,
    from: Vec3,
    dir: Vec3,
    velocity: Vec3,
    reach: f32,
    span: f32,
}

/// A flame's mesh: its owner's nozzle and aim (the last it was fed), the
/// seconds it goes on sending knots, how long it has sent them (its
/// strength), the time since the last, and its knots, newest first.
#[derive(Component)]
struct Rope {
    owner: Entity,
    def: Arc<FlameRope>,
    nozzle: Vec3,
    aim: Vec3,
    velocity: Vec3,
    reach: f32,
    feeding: f32,
    fired: f32,
    clock: f32,
    age: f32,
    knots: VecDeque<Knot>,
}

/// A knot of the rope: where it is, how fast it goes, how far it has gone
/// and will, and the flame's strength as it left.
struct Knot {
    at: Vec3,
    velocity: Vec3,
    gone: f32,
    end: f32,
    strength: f32,
}

impl Rope {
    fn feed(&mut self, f: &RopeFeed) {
        (self.nozzle, self.aim, self.velocity, self.reach) = (f.from, f.dir, f.velocity, f.reach);
        self.feeding = self.feeding.max(f.span);
    }

    /// Its knots go on, and while it's fed a new one leaves every segment
    /// (each from where it would be had it left on its tick).
    fn step(&mut self, dt: f32) {
        let d = &self.def;
        self.age += dt;
        for k in &mut self.knots {
            k.at += k.velocity * dt;
            k.gone += d.speed * dt;
        }
        self.knots.retain(|k| k.gone < k.end);
        if self.feeding <= 0.0 {
            self.fired = (self.fired - dt * 2.0).max(0.0);
            return;
        }
        self.feeding -= dt;
        self.fired += dt;
        self.clock += dt;
        let every = d.segment / d.speed;
        let strength = (self.fired / d.grow_time.max(1e-3)).min(1.0);
        while self.clock >= every {
            self.clock -= every;
            let gone = d.speed * self.clock;
            let end = d.length.min(self.reach);
            if gone < end && self.knots.len() < 256 {
                let at = self.nozzle + self.aim * gone + self.velocity * self.clock;
                self.knots.push_front(Knot { at, velocity: self.aim * d.speed + self.velocity, gone, end, strength });
            }
        }
    }

    /// The rope's points (where, how far along, the strength there): the
    /// nozzle while it's fed, then its knots, held straight ahead of the gun
    /// near it; from where it's drawn.
    fn points(&self) -> Vec<(Vec3, f32, f32)> {
        let d = &self.def;
        let mut points = Vec::with_capacity(self.knots.len() + 1);
        if self.feeding > 0.0 {
            points.push((self.nozzle, 0.0, (self.fired / d.grow_time.max(1e-3)).min(1.0)));
        }
        for k in &self.knots {
            let straight = self.nozzle + self.aim * k.gone;
            let free = smoothstep(d.straight[0], d.straight[1], k.gone);
            points.push((straight.lerp(k.at, free), k.gone, k.strength));
        }
        let Some(first) = points.iter().position(|p| p.1 >= d.cutoff) else { return Vec::new() };
        if first > 0 {
            let (a, b) = (points[first - 1], points[first]);
            let t = ((d.cutoff - a.1) / (b.1 - a.1).max(1e-4)).clamp(0.0, 1.0);
            points[first - 1] = (a.0.lerp(b.0, t), d.cutoff, a.2 + (b.2 - a.2) * t);
            points.drain(..first - 1);
        }
        points
    }
}

fn smoothstep(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from).max(1e-4)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How brightly the flame mesh's texture glows (`FC3_ROPE_GLOW`, else 3).
fn rope_glow() -> f32 {
    static GLOW: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *GLOW.get_or_init(|| std::env::var("FC3_ROPE_GLOW").ok().and_then(|v| v.parse().ok()).unwrap_or(3.0))
}

/// The flames' meshes: fed ones send knots, all of them go on, and each is
/// drawn as a tube round its rope (as wide as its flame says there, its
/// texture's frames in turn, tiled along it, fading in past the nozzle and
/// out at its tip), until its last knot has gone.
#[allow(clippy::too_many_arguments)]
fn ropes(
    time: Res<Time>,
    feeds: Query<(Entity, &RopeFeed)>,
    mut ropes: Query<(Entity, &mut Rope, &Mesh3d, &mut Visibility)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ParticleMaterial>>,
    server: Res<AssetServer>,
    mut looks: Local<HashMap<String, Option<Handle<ParticleMaterial>>>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (e, f) in &feeds {
        commands.entity(e).despawn();
        if let Some((_, mut rope, _, _)) = ropes.iter_mut().find(|(_, r, _, _)| r.owner == f.owner) {
            rope.feed(f);
            continue;
        }
        // (Its texture, if it's cached.)
        let look = looks.entry(f.def.texture.clone()).or_insert_with(|| {
            let rel = fc3_formats::cache::texture_rel(&f.def.texture);
            fc3_formats::cache::default_cache_root().join(&rel).exists().then(|| {
                let texture = server
                    .load_builder()
                    .with_settings(|s: &mut bevy::image::ImageLoaderSettings| {
                        s.is_srgb = true;
                        s.sampler = crate::cache_assets::repeat_sampler();
                    })
                    .load(crate::cache_assets::cache_path(&rel));
                materials.add(ParticleMaterial::plain(0.0, texture).soft(0.25))
            })
        });
        let Some(material) = look.clone() else { continue };
        let mut rope = Rope {
            owner: f.owner,
            def: f.def.clone(),
            nozzle: f.from,
            aim: f.dir,
            velocity: f.velocity,
            reach: f.reach,
            feeding: 0.0,
            fired: 0.0,
            clock: 0.0,
            age: 0.0,
            knots: VecDeque::new(),
        };
        rope.feed(f);
        commands.spawn((
            rope,
            Mesh3d(meshes.add(crate::particles::empty_mesh())),
            MeshMaterial3d(material),
            Transform::default(),
            Visibility::Hidden,
            NotShadowCaster,
            NotShadowReceiver,
            bevy::camera::visibility::NoFrustumCulling,
            Name::new("flame mesh"),
        ));
    }
    let glow = rope_glow();
    for (e, mut rope, mesh, mut vis) in &mut ropes {
        rope.step(dt);
        if rope.feeding <= 0.0 && rope.knots.is_empty() {
            meshes.remove(&mesh.0);
            commands.entity(e).despawn();
            continue;
        }
        let points = rope.points();
        let want = if points.len() < 2 { Visibility::Hidden } else { Visibility::Inherited };
        if *vis != want {
            *vis = want;
        }
        if points.len() < 2 {
            continue;
        }
        let d = &rope.def;
        let n = d.ring.max(3) as usize;
        let frames = d.frames.max(1);
        let frame = if d.frame_time > 0.0 { (rope.age / d.frame_time) as u32 % frames } else { 0 };
        let (cutoff, tip) = (d.cutoff, points[points.len() - 1].1);
        let mut positions = Vec::with_capacity(points.len() * (n + 1));
        let mut normals = Vec::with_capacity(positions.capacity());
        let mut uvs = Vec::with_capacity(positions.capacity());
        let mut colours = Vec::with_capacity(positions.capacity());
        for (i, &(at, along, strength)) in points.iter().enumerate() {
            let tangent = (points[(i + 1).min(points.len() - 1)].0 - points[i.saturating_sub(1)].0).normalize_or(rope.aim);
            let up = if tangent.y.abs() < 0.95 { Vec3::Y } else { Vec3::X };
            let side = up.cross(tangent).normalize();
            let over = tangent.cross(side);
            let width = d.width(along) * strength.max(0.05);
            let fade = smoothstep(cutoff, cutoff + 1.0, along) * smoothstep(0.0, 1.5, tip - along);
            for k in 0..=n {
                let round = k as f32 / n as f32;
                let angle = d.ring_angle.to_radians() + round * std::f32::consts::TAU;
                let out = side * angle.cos() + over * angle.sin();
                positions.push((at + out * width).to_array());
                normals.push(out.to_array());
                uvs.push([(frame as f32 + round) / frames as f32, along / d.tiling.max(0.01)]);
                colours.push([glow, glow, glow, fade]);
            }
        }
        let mut indices: Vec<u32> = Vec::with_capacity((points.len() - 1) * n * 6);
        for i in 0..points.len() - 1 {
            for k in 0..n {
                let (a, b) = ((i * (n + 1) + k) as u32, ((i + 1) * (n + 1) + k) as u32);
                indices.extend([a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        if let Some(mut m) = meshes.get_mut(&mesh.0) {
            m.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
            m.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
            m.insert_attribute(Mesh::ATTRIBUTE_COLOR, colours);
            m.insert_indices(Indices::U32(indices));
        }
    }
}

/// A flame breathed for `dt` seconds (the Ink Monster's): each of its stream
/// effects launched on its clock (as often as with the aim at rest) from
/// `from` along `dir`, as far as it reaches; first person's effects, or
/// third's. False when none is cached.
#[allow(clippy::too_many_arguments)]
pub fn stream(commands: &mut Commands, flame: &Flame, clocks: &mut Vec<f32>, dt: f32, from: Vec3, dir: Vec3, reach: f32, third_person: bool) -> bool {
    let frame = crate::authored_fx::muzzle_frame(dir);
    clocks.resize(flame.stream.len(), 0.0);
    let mut any = false;
    for (s, clock) in flame.stream.iter().zip(clocks.iter_mut()) {
        let Some(name) = (if third_person { s.effect_3p } else { s.effect }).filter(|n| crate::authored_fx::available(n)) else { continue };
        any = true;
        *clock += dt;
        let t = &s.timing;
        let every = (t.every * t.turning_ratio(0.0)).max(1e-3);
        let end = t.distance.min(reach);
        while *clock >= every {
            *clock -= every;
            // (Each from where it would be had it left on its tick.)
            let gone = (t.speed * *clock).min(end);
            commands.spawn((
                Transform::from_translation(from + dir * gone).with_rotation(frame),
                crate::authored_fx::AuthoredFx(name),
                Launched { velocity: dir * t.speed, left: ((end - gone) / t.speed).max(0.0) },
            ));
        }
    }
    any
}

/// Stream effects go on their way until they've gone their distance or
/// struck something, and stop there.
fn fly(time: Res<Time>, mut launched: Query<(Entity, &mut Transform, &mut Launched)>, mut commands: Commands) {
    let dt = time.delta_secs();
    for (e, mut t, mut l) in &mut launched {
        t.translation += l.velocity * l.left.min(dt);
        l.left -= dt;
        if l.left <= 0.0 {
            // (It may have played out already.)
            commands.entity(e).try_despawn();
        }
    }
}
