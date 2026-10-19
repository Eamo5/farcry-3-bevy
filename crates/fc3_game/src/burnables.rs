//! Burnable objects: the props the fire manager burns, each as its object
//! profile says (`fire_settings`' object profiles, by model: in the world
//! each carries the ignitor its model's profile puts on it). The settings
//! list crates, baskets, straw roofs, tyres and more, but few of those
//! models are in this game's data: the villages' wooden house platforms
//! and frames are what burn. Fire reaching one (a
//! burning patch of ground under it, or a molotov, the flamethrower or an
//! explosion setting fire about it: `fire::Ignite`) sets it alight. The fire
//! manager's object fires burn over its top and its smoke rises (black from
//! tyres and fuel) for its profile's time, while it chars to its burn
//! colour. Once its fire has grown it sets the ground under it alight. It's
//! left charred, and doesn't burn again.

use std::collections::HashMap;

use bevy::prelude::*;
use fc3_formats::fire_settings::{self as fs, ObjectProfile};

use crate::cache_assets::Fc3Model;
use crate::fire::{Fires, Ignite};
use crate::objects::WorldObject;
use crate::weapons::Rng;

pub struct BurnablesPlugin;

impl Plugin for BurnablesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Table>()
            .add_systems(Startup, load)
            .add_systems(Update, (tag, ignite, burn, linger).chain().run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(PreUpdate, crate::original_shaders::finish_swaps::<StandardMaterial>);
    }
}

/// The object profiles, which models burn as which, and the fire manager's
/// object fires and smokes (with how many of each may play at once).
#[derive(Resource, Default)]
struct Table {
    profiles: Vec<ObjectProfile>,
    by_model: HashMap<String, usize>,
    fires: Vec<(&'static str, usize)>,
    smokes: Vec<(&'static str, usize)>,
    black: Vec<(&'static str, usize)>,
}

/// A world object that burns, as its profile (an index into the table).
#[derive(Component)]
struct Burnable(usize);

/// One that has burnt.
#[derive(Component)]
struct Burnt;

/// One burning: how long it has, its fires, its smoke and its parts'
/// charring materials (with their colours before).
#[derive(Component)]
struct Burning {
    profile: usize,
    age: f32,
    life: f32,
    flames: Vec<(Entity, &'static str)>,
    smoke: Option<Entity>,
    chars: Vec<(Handle<StandardMaterial>, LinearRgba)>,
    started: bool,
    spread: bool,
}

/// A burnt object's smoke, rising on a while (seconds left).
#[derive(Component)]
struct Linger(f32);

/// Most fires over one object, and the least room (m) between them.
const MAX_FLAMES: usize = 8;
const FLAME_SPACING: f32 = 1.5;

fn load(mut table: ResMut<Table>) {
    let root = fc3_formats::cache::default_cache_root();
    let Ok(text) = std::fs::read_to_string(root.join(fs::FIRE_OBJECTS_REL)) else { return };
    let (profiles, burnables) = fs::objects_from_text(&text);
    let index: HashMap<u64, usize> = profiles.iter().enumerate().map(|(i, p)| (p.key, i)).collect();
    table.by_model = burnables.iter().filter_map(|b| Some((fc3_formats::cache::model_rel(&b.model), *index.get(&b.profile)?))).collect();
    table.profiles = profiles;
    let effects = std::fs::read_to_string(root.join(fs::FIRE_SETTINGS_REL)).map(|t| fs::from_text(&t)).unwrap_or_default();
    let mut ground_smokes = Vec::new();
    for f in effects.into_iter().filter(|f| crate::authored_fx::available(&f.effect)) {
        let entry = (crate::authored_fx::intern(&f.effect), f.pool as usize);
        // (A tree trunk's fire isn't an object's; the black smokes are the
        // black-smoking objects'.)
        match f.kind.as_str() {
            fs::OBJECT if !f.effect.contains("cyltree") => table.fires.push(entry),
            fs::OBJECT_SMOKE if f.effect.contains("black") => table.black.push(entry),
            fs::OBJECT_SMOKE => table.smokes.push(entry),
            fs::REGION_SMOKE => ground_smokes.push(entry),
            _ => {}
        }
    }
    // (The objects' own smokes aren't in the library: the burning ground's
    // rise over them.)
    if table.smokes.is_empty() {
        table.smokes = ground_smokes;
    }
    info!(
        "burnables: {} profiles, {} models, {} fires, {} smokes ({} black)",
        table.profiles.len(),
        table.by_model.len(),
        table.fires.len(),
        table.smokes.len(),
        table.black.len()
    );
}

/// World objects that burn (each, in the world, carrying the ignitor its
/// model's profile puts on it), marked as their models come in.
fn tag(table: Res<Table>, server: Res<AssetServer>, added: Query<(Entity, &WorldObject), Added<WorldObject>>, mut commands: Commands) {
    if table.by_model.is_empty() {
        return;
    }
    for (e, object) in &added {
        let Some(path) = server.get_path(&object.model) else { continue };
        let rel = path.path().to_string_lossy().replace('\\', "/");
        if let Some(&profile) = table.by_model.get(&rel) {
            commands.entity(e).insert(Burnable(profile));
        }
    }
}

/// How far (m) an object reaches out from where it stands, across.
fn reach(model: Option<&Fc3Model>, t: &GlobalTransform) -> f32 {
    model.map_or(0.5, |m| Vec2::new(m.aabb_max.x - m.aabb_min.x, m.aabb_max.z - m.aabb_min.z).length() * 0.5 * t.scale().max_element()).max(0.5)
}

/// Fire set about an object, or burning under it, sets it alight.
#[allow(clippy::too_many_arguments)]
fn ignite(
    time: Res<Time>,
    table: Res<Table>,
    fires: Res<Fires>,
    models: Res<Assets<Fc3Model>>,
    mut events: MessageReader<Ignite>,
    mut next: Local<f32>,
    mut rng: ResMut<Rng>,
    objects: Query<(Entity, &GlobalTransform, &WorldObject, &Burnable), (Without<Burning>, Without<Burnt>)>,
    mut commands: Commands,
) {
    let events: Vec<Ignite> = events.read().copied().collect();
    *next -= time.delta_secs();
    let ground = *next <= 0.0;
    if ground {
        *next = 0.5;
    }
    if events.is_empty() && !ground {
        return;
    }
    for (e, t, object, b) in &objects {
        let at = t.translation();
        let r = reach(models.get(&object.model), t);
        if events.iter().any(|ev| ev.at.distance(at) < ev.radius + r) || (ground && fires.is_burning_at(at)) {
            let p = &table.profiles[b.0];
            let life = ((p.starting + p.alive + p.extinction) * rng.range(0.85, 1.15)).max(1.0);
            debug!("burnables: {e} catches fire ({}, {life:.1} s)", p.name);
            commands.entity(e).insert(Burning {
                profile: b.0,
                age: 0.0,
                life,
                flames: Vec::new(),
                smoke: None,
                chars: Vec::new(),
                started: false,
                spread: false,
            });
        }
    }
}

/// One of a list with room left in its pool, by a lot.
fn pick(list: &[(&'static str, usize)], using: &HashMap<&'static str, usize>, lot: usize) -> Option<&'static str> {
    (0..list.len()).map(|i| list[(lot + i) % list.len()]).find(|(n, cap)| using.get(n).copied().unwrap_or(0) < *cap).map(|(n, _)| n)
}

/// The fires over a burning object's top and its smoke, its charring, and
/// the ground under it set alight once its fire has grown.
#[allow(clippy::too_many_arguments)]
fn burn(
    time: Res<Time>,
    table: Res<Table>,
    models: Res<Assets<Fc3Model>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut rng: ResMut<Rng>,
    mut burning: Query<(Entity, &GlobalTransform, &WorldObject, &mut Burning, Option<&Children>)>,
    parts: Query<(Option<&MeshMaterial3d<StandardMaterial>>, Option<&crate::original_shaders::OriginalSwap>)>,
    mut ignite: MessageWriter<Ignite>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let mut using: HashMap<&'static str, usize> = HashMap::new();
    for (.., b, _) in &burning {
        for (_, n) in &b.flames {
            *using.entry(n).or_default() += 1;
        }
    }
    for (e, t, object, mut b, children) in &mut burning {
        let profile = &table.profiles[b.profile];
        let at = t.translation();
        let model = models.get(&object.model);
        if !b.started {
            b.started = true;
            // Its fires over its top (its box as it stands), as many as
            // fit, and its smoke above.
            let corners = model.map_or([Vec3::ZERO; 2], |m| [m.aabb_min, m.aabb_max]);
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for i in 0..8 {
                let c = Vec3::new(corners[i & 1].x, corners[(i >> 1) & 1].y, corners[(i >> 2) & 1].z);
                let w = t.transform_point(c);
                (lo, hi) = (lo.min(w), hi.max(w));
            }
            let size = (hi - lo).max(Vec3::splat(0.1));
            let spacing = (size.x * size.z / MAX_FLAMES as f32).sqrt().max(FLAME_SPACING);
            let (nx, nz) = (((size.x / spacing).ceil() as usize).max(1), ((size.z / spacing).ceil() as usize).max(1));
            let y = hi.y - size.y * 0.25;
            for i in 0..nx {
                for j in 0..nz {
                    if b.flames.len() >= MAX_FLAMES {
                        break;
                    }
                    let Some(name) = pick(&table.fires, &using, rng.next_u32() as usize) else { break };
                    let p = Vec3::new(lo.x + (i as f32 + 0.5) * size.x / nx as f32, y, lo.z + (j as f32 + 0.5) * size.z / nz as f32);
                    let f = commands.spawn((Transform::from_translation(p), crate::authored_fx::AuthoredFx(name), crate::authored_fx::NoLights)).id();
                    *using.entry(name).or_default() += 1;
                    b.flames.push((f, name));
                }
            }
            let smokes = if profile.black && !table.black.is_empty() { &table.black } else { &table.smokes };
            if let Some(name) = pick(smokes, &using, rng.next_u32() as usize) {
                b.smoke = Some(commands.spawn((Transform::from_translation(Vec3::new(at.x, hi.y, at.z)), crate::authored_fx::AuthoredFx(name))).id());
            }
            // Its parts' own materials, to char (off the original shaders).
            for &part in children.into_iter().flatten() {
                let Ok((own, swap)) = parts.get(part) else { continue };
                let Some(standard) = swap.map(|s| s.standard.clone()).or_else(|| own.map(|m| m.0.clone())) else { continue };
                let Some(material) = materials.get(&standard).cloned() else { continue };
                let before = material.base_color.to_linear();
                let charred = materials.add(material);
                crate::original_shaders::swap_next_frame(&mut commands, part, charred.clone());
                b.chars.push((charred, before));
            }
            debug!("burnables: {e} at {:.1?} burns with {} fires, charring {} parts", crate::data::to_dunia(at), b.flames.len(), b.chars.len());
        }
        b.age += dt;
        // Charring as it burns, to its burn colour by the end.
        let k = crate::sky::smoothstep(0.0, 1.0, b.age / b.life);
        let char = Vec3::from_array(profile.char);
        for (h, before) in &b.chars {
            if let Some(mut m) = materials.get_mut(h) {
                let tint = Vec3::ONE.lerp(char, k);
                m.base_color = Color::LinearRgba(LinearRgba::new(before.red * tint.x, before.green * tint.y, before.blue * tint.z, before.alpha));
            }
        }
        // The ground under it, once its fire has grown.
        if !b.spread && b.age >= profile.starting {
            b.spread = true;
            ignite.write(Ignite { at, radius: reach(model, t), instigator: None });
        }
        // Its fires out halfway through dying down; then smoke a while.
        if b.age >= b.life - profile.extinction * 0.5 {
            for (f, _) in b.flames.drain(..) {
                commands.entity(f).try_despawn();
            }
        }
        if b.age >= b.life {
            if let Some(s) = b.smoke {
                let (lo, hi) = profile.smoke;
                commands.entity(s).insert(Linger(if hi > 0.0 { rng.range(lo, hi.max(lo)) } else { 10.0 }));
            }
            commands.entity(e).remove::<Burning>().insert(Burnt);
        }
    }
}

fn linger(time: Res<Time>, mut smokes: Query<(Entity, &mut Linger)>, mut commands: Commands) {
    for (e, mut l) in &mut smokes {
        l.0 -= time.delta_secs();
        if l.0 <= 0.0 {
            commands.entity(e).try_despawn();
        }
    }
}
