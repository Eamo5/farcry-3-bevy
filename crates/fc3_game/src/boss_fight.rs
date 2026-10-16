//! CIT_030's fight with the Ink Monster (`CCitraBossFight`): the giant
//! rising behind the temple, attacking the player on its top, and the mask
//! on its head to be shot until it is knocked out. Everything it does comes
//! from the GSF library's entry (`gsf::BossFightSettings`) and its moves'
//! own clips: each attack is a move played facing the player (`+000fw`,
//! `+045rt`, `-045lt`), and its clip's events say when a sweep's hand comes
//! down (`SweepHit`), a rock leaves the hand (`ThrowRock`) or the fire
//! breath starts and stops. When it is knocked out the fight tells the
//! script (`citrabossfight_EnableInvisibleBridge`) and ends, and B10E goes
//! on to B15E's finishing blow.

use std::collections::HashMap;

use avian3d::prelude::*;
use bevy::prelude::*;

use crate::cache_assets::{cache_path, Fc3Clip, Fc3Skeleton};
use crate::characters::{CharacterRig, ClipOverride};
use crate::combat::{DamageEvent, DamageKind, Health, Hit};
use crate::player::Player;
use crate::story_actors::StoryActor;

pub struct BossFightPlugin;

impl Plugin for BossFightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BossFight>()
            .add_systems(
                Update,
                (start_and_end, mask_hits, fight, rocks)
                    .chain()
                    .after(crate::story::StorySet)
                    .after(crate::cinema::CinemaSet)
                    .run_if(in_state(crate::menu::GameState::Playing)),
            )
            .add_systems(
                PreUpdate,
                autopilot.after(crate::controls::update_actions).before(crate::controls::apply_action_maps).run_if(in_state(crate::menu::GameState::Playing)),
            );
    }
}

/// The fight the story asks for.
pub struct Start {
    pub id: u64,
    pub params: HashMap<String, String>,
}

#[derive(Resource, Default)]
pub struct BossFight {
    pub pending: Vec<Start>,
    pub cancelled: Vec<u64>,
    active: Option<Fight>,
}

impl BossFight {
    /// The fight is on: the player fights it where he stands (the story's
    /// autopilot leaves him to it).
    pub fn holds_player(&self) -> bool {
        self.active.is_some()
    }

    /// A checkpoint reload: the fight ends (the block starts it again).
    pub fn reset(&mut self) {
        self.pending.clear();
        if let Some(f) = &self.active {
            self.cancelled.push(f.id);
        }
    }
}

/// What the giant is doing.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Arrival,
    Idle,
    Sweep { left: bool },
    Throw(u8),
    ThrowExit(u8),
    Breath,
    Knockout,
    KnockedOut,
}

impl Act {
    /// The settings' name for its move.
    fn move_key(self) -> String {
        match self {
            Act::Arrival => "arrival".into(),
            Act::Idle => "idle".into(),
            Act::Sweep { left: true } => "sweep_left".into(),
            Act::Sweep { left: false } => "sweep_right".into(),
            Act::Throw(n) => format!("throw{n}"),
            Act::ThrowExit(n) => format!("throw{n}_exit"),
            Act::Breath => "breath".into(),
            Act::Knockout => "knockout".into(),
            Act::KnockedOut => "knockout_idle".into(),
        }
    }

    /// Played over and over until something else starts.
    fn looping(self) -> bool {
        matches!(self, Act::Idle | Act::KnockedOut)
    }
}

struct Fight {
    id: u64,
    s: fc3_formats::gsf::BossFightSettings,
    /// The giant's story id.
    boss: u64,
    skeleton: Handle<Fc3Skeleton>,
    hp: f32,
    phase: usize,
    act: Act,
    /// The act's clip (its path) and the time into it.
    clip: Option<(String, Handle<Fc3Clip>)>,
    /// The clip whose moments the move keeps: its own, or its `+000fw`
    /// build's (the turned builds name none: a sweep's hit is the forward
    /// one's).
    moments: Option<Handle<Fc3Clip>>,
    t: f32,
    /// Events of the clip already raised.
    fired: usize,
    /// Seconds left of idling before the next attack.
    idle_for: f32,
    /// Attacks made (the next one in turn).
    attacks: usize,
    /// This sweep has hit.
    swept: bool,
    /// Breathing fire, and where the flame is aimed.
    breath: Option<Vec3>,
    /// The weak spot: a sphere riding on the head, shot at.
    mask: Option<Entity>,
    /// Told the player to shoot the mask.
    hinted: bool,
    rocks_thrown: u32,
    /// Hits on the mask since the last update.
    struck: Vec<f32>,
    /// Where its moves play from: where it stood as the fight began (where
    /// the block's scene put it, B15E's finishing blow's anchor). Their
    /// clips' root motion carries it from there.
    origin: Option<Transform>,
    /// The reference its moves carry (where they were authored about):
    /// those that carry it in their hips instead (the sweeps) are played
    /// with it taken off.
    reference: Option<Transform>,
    /// The arena's edges (`m_boundaryNearLeft`, `NearRight`, as the giant
    /// sees them): where the player is across it picks the moves' turn.
    near_edge: Option<(Vec3, Vec3)>,
    /// Debug screenshots taken.
    shots: u32,
}

/// The mask's sphere (its own health, never run down: hits come off the
/// giant's).
#[derive(Component)]
struct WeakSpot;

/// A rock the giant threw: flying, to be shot down.
#[derive(Component)]
struct Rock {
    velocity: Vec3,
    age: f32,
    damage: f32,
    radius: f32,
}

/// How much more a hit on the mask counts against the giant's health than
/// against a man's. The fight's own scale isn't in its data (the engine
/// keeps it); this puts the knockout some twenty bow hits away, as the
/// game's fight runs.
const WEAK_SPOT: f32 = 6.0;
/// The mask's reach about the head bone (m): the mask model is some 11 by
/// 9 m.
const MASK_RADIUS: f32 = 5.5;
/// Where the mask sits from the head bone, in the bone's frame (its model's
/// middle).
const MASK_OFFSET: Vec3 = Vec3::new(0.0, 4.0, 3.0);
/// A thrown rock's speed (m/s) and size.
const ROCK_SPEED: f32 = 32.0;
/// A move with no reference whose hips start this far from its root (m)
/// carries the reference in them.
const BAKED_REFERENCE: f32 = 30.0;
const ROCK_SIZE: f32 = 1.6;
const GRAVITY: f32 = 9.81;

fn start_and_end(
    mut fight: ResMut<BossFight>,
    mut hidden: ResMut<crate::story::HiddenEntities>,
    host: Option<NonSendMut<crate::story::StoryHost>>,
    info: Option<Res<crate::data::CacheInfo>>,
    server: Res<AssetServer>,
    mut commands: Commands,
    rocks: Query<Entity, With<Rock>>,
) {
    let cancelled = std::mem::take(&mut fight.cancelled);
    for start in std::mem::take(&mut fight.pending) {
        if cancelled.contains(&start.id) {
            continue;
        }
        let settings = (|| -> anyhow::Result<_> {
            use anyhow::Context;
            let root = &info.as_ref().context("cache unavailable")?.root;
            fc3_formats::gsf::BossFightSettings::parse(&std::fs::read(root.join(fc3_formats::gsf::CACHE_REL))?)
        })();
        let s = match settings {
            Ok(s) => s,
            Err(e) => {
                warn!("boss fight: not played: {e:#}");
                continue;
            }
        };
        let Some(boss) = start.params.get("m_citra/EntityID").and_then(|v| v.parse::<u64>().ok()) else {
            warn!("boss fight: no giant");
            continue;
        };
        // The giant's own skeleton (its pawn's), which its moves' clips drive.
        let skeleton = host
            .as_ref()
            .and_then(|h| h.story.data().entities.get(&boss).and_then(|e| skeleton_of(e)))
            .map(|p| server.load(cache_path(&fc3_formats::cache::skeleton_rel(&p))))
            .unwrap_or_default();
        let place = |key: &str| {
            let id = start.params.get(key)?.parse::<u64>().ok()?;
            host.as_ref()?.story.data().entities.get(&id).map(|e| crate::data::to_bevy(e.pos))
        };
        let near_edge = place("m_boundaryNearLeft").zip(place("m_boundaryNearRight"));
        info!("boss fight {}: the giant {boss}, {} health, phases at {:?}", start.id, s.hp, s.phases);
        // Kept out of sight since it was placed (CIT_030's main graph): the
        // fight brings it on, rising for its arrival.
        if hidden.0.contains(&boss) {
            hidden.0.remove(&boss);
        }
        fight.active = Some(Fight {
            id: start.id,
            hp: s.hp,
            s,
            boss,
            skeleton,
            phase: 0,
            act: Act::Arrival,
            clip: None,
            moments: None,
            t: 0.0,
            fired: 0,
            idle_for: 0.0,
            attacks: 0,
            swept: false,
            breath: None,
            mask: None,
            hinted: false,
            rocks_thrown: 0,
            struck: Vec::new(),
            origin: None,
            reference: None,
            near_edge,
            shots: 0,
        });
    }
    if fight.active.as_ref().is_some_and(|f| cancelled.contains(&f.id)) {
        let f = fight.active.take().unwrap();
        info!("boss fight {}: over", f.id);
        if let Some(m) = f.mask {
            commands.entity(m).despawn();
        }
        for r in &rocks {
            commands.entity(r).despawn();
        }
    }
}

/// The skeleton a pawn's graphic component names (`fileSkeleton`'s path).
fn skeleton_of(e: &fc3_story::data::Entry) -> Option<String> {
    let mut path = None;
    e.components.walk(&mut |o, _| {
        if path.is_none() {
            path = o.str_h(0xF9F2_D5F4).filter(|p| p.to_ascii_lowercase().ends_with(".skeleton"));
        }
    });
    path
}

/// The clip of a move facing the player: its `+045rt`, `-045lt` or `+000fw`
/// build, else its first.
fn clip_for(table: &crate::qte::MoveTable, name: &str, facing: &str) -> Option<String> {
    let states = table.0.get(name).or_else(|| table.0.get(&name.to_ascii_lowercase()))?;
    states
        .iter()
        .find(|(_, c)| c.to_ascii_lowercase().contains(facing))
        .or_else(|| states.iter().find(|(_, c)| c.contains("+000fw")))
        .or(states.first())
        .map(|(_, c)| c.clone())
}

#[allow(clippy::too_many_arguments)]
fn fight(
    mut fight: ResMut<BossFight>,
    mut host: Option<NonSendMut<crate::story::StoryHost>>,
    time: Res<Time>,
    server: Res<AssetServer>,
    table: Res<crate::qte::MoveTable>,
    clips: Res<Assets<Fc3Clip>>,
    mut actors: Query<(&StoryActor, &mut Transform, &GlobalTransform), (Without<Player>, Without<WeakSpot>)>,
    rigs: Query<&CharacterRig>,
    joints: Query<&GlobalTransform, Without<StoryActor>>,
    players: Query<(Entity, &Transform), With<Player>>,
    mut spots: Query<(&mut Transform, &mut Health), (With<WeakSpot>, Without<Player>)>,
    mut damage: MessageWriter<DamageEvent>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut shakes: MessageWriter<crate::shake::Shake>,
    (mut particles, mut rng, mut meshes, mut materials, cinema, placements): (
        ResMut<crate::particles::Particles>,
        ResMut<crate::weapons::Rng>,
        ResMut<Assets<Mesh>>,
        ResMut<Assets<StandardMaterial>>,
        Res<crate::cinema::Cinema>,
        Res<crate::story::EntityPlacements>,
    ),
    mut commands: Commands,
) {
    let Some(f) = fight.active.as_mut() else { return };
    // (Not before the block's scene has put the giant in place: B10E starts
    // the fight and that scene together.)
    if f.origin.is_none() && (cinema.owns_entity(f.boss) || !cinema.requests.is_empty()) {
        return;
    }
    let dt = time.delta_secs();
    let Ok((pe, pt)) = players.single() else { return };
    let Some((actor, mut boss_t, boss_at)) = actors.iter_mut().find(|(a, _, _)| a.id == f.boss) else { return };
    let boss_at = *boss_at;
    let origin = *f.origin.get_or_insert_with(|| {
        placements.pose(f.boss).unwrap_or(Transform {
            translation: boss_t.translation - boss_t.rotation * Vec3::Y * crate::player::HALF_HEIGHT,
            rotation: boss_t.rotation,
            scale: Vec3::ONE,
        })
    });
    let Some(rig_entity) = actor.rig else { return };
    let Ok(rig) = rigs.get(rig_entity) else { return };
    let bone = |name: &str| rig.joint(fc3_formats::hash::name_hash(name)).and_then(|(_, j)| joints.get(j).ok()).copied();
    let head = bone("Head");
    // The weak spot on the head.
    let mask = *f.mask.get_or_insert_with(|| {
        commands
            .spawn((Name::new("ink monster's mask"), WeakSpot, Transform::default(), Collider::sphere(MASK_RADIUS), RigidBody::Kinematic, Health::new(1.0e9)))
            .id()
    });
    if let (Some(h), Ok((mut st, mut sh))) = (head, spots.get_mut(mask)) {
        st.translation = h.transform_point(MASK_OFFSET);
        sh.current = sh.max;
        sh.dead = false;
    }
    // Hits on the mask, off the giant's health.
    let player_at = pt.translation;
    for amount in std::mem::take(&mut f.struck) {
        if f.act == Act::Knockout || f.act == Act::KnockedOut {
            continue;
        }
        let amount = amount * WEAK_SPOT;
        f.hp = (f.hp - amount).max(0.0);
        debug!("boss fight: the mask hit for {amount:.0} ({:.0} left)", f.hp);
        if amount >= f.s.hurt_threshold {
            sound(&mut sfx, &f.s, if amount > f.s.hurt_threshold * 4.0 { "BossGruntPainBig" } else { "BossGruntPainSmall" }, head.map(|h| h.translation()));
        }
    }
    let phase = f.s.phase_at(f.hp);
    if phase != f.phase {
        info!("boss fight: phase {} at {:.0} health", phase + 1, f.hp);
        f.phase = phase;
        let line = ["Fight", "YouAreSpecial", "YouAreStrong", "YouWillDefeatAnyone"][phase.min(3)];
        sound(&mut sfx, &f.s, line, None);
        if phase >= 3 && !matches!(f.act, Act::Knockout | Act::KnockedOut) {
            begin(f, Act::Knockout);
        }
    }
    // The move playing: its clip by where the player is.
    let across = across(f.near_edge, player_at);
    let facing = if across < 1.0 / 3.0 {
        "-045lt"
    } else if across > 2.0 / 3.0 {
        "+045rt"
    } else {
        "+000fw"
    };
    if f.clip.is_none() {
        let Some(name) = f.s.move_name(&f.act.move_key()).map(str::to_string) else {
            warn!("boss fight: no move for {:?}", f.act);
            next(f, across);
            return;
        };
        let Some(path) = clip_for(&table, &name, facing) else {
            warn!("boss fight: no clip for {name}");
            next(f, across);
            return;
        };
        debug!(
            "boss fight: {:?} plays {path} (the giant at {:.1?}, its head at {:.1?}, hands {:.1?} {:.1?}; the player at {:.1?})",
            f.act,
            boss_at.translation(),
            head.map(|h| h.translation()),
            bone("LeftHand").map(|h| h.translation()),
            bone("RightHand").map(|h| h.translation()),
            player_at
        );
        let handle = server.load(cache_path(&fc3_formats::cache::anim_rel(&path)));
        f.moments = clip_for(&table, &name, "+000fw").filter(|fw| *fw != path).map(|fw| server.load(cache_path(&fc3_formats::cache::anim_rel(&fw))));
        f.clip = Some((path, handle));
        f.t = 0.0;
        f.fired = 0;
    }
    let Some((_, handle)) = f.clip.clone() else { return };
    let Some(clip) = clips.get(&handle) else { return };
    let clip = clip.0.clone();
    // FC3_BOSS_SHOTS=<dir> (debug): a screenshot a second into each move.
    if let Some(dir) = std::env::var_os("FC3_BOSS_SHOTS").filter(|_| f.t < 1.0 && f.t + dt >= 1.0) {
        use bevy::render::view::window::screenshot::{save_to_disk, Screenshot};
        f.shots += 1;
        let shot = std::path::Path::new(&dir).join(format!("boss_{:02}_{:?}.png", f.shots, f.act).replace([' ', '{', '}', ':'], ""));
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(shot));
    }
    f.t += dt;
    // Its moments, as the clip passes them.
    let moments = match f.moments.as_ref().map(|h| clips.get(h)) {
        // (Waiting for the forward build to load.)
        Some(None) => return,
        Some(Some(fw)) if clip.events.is_empty() => fw.0.events.clone(),
        _ => clip.events.clone(),
    };
    while let Some((at, event)) = moments.get(f.fired).cloned() {
        if at > f.t {
            break;
        }
        f.fired += 1;
        match event.as_str() {
            "SweepHit" if !f.swept => {
                // A hand coming down near the player.
                let near = ["LeftHand", "RightHand"]
                    .into_iter()
                    .filter_map(|b| bone(b))
                    .map(|h| h.translation())
                    .min_by(|a, b| a.distance(player_at).total_cmp(&b.distance(player_at)));
                debug!(
                    "boss fight: SweepHit with hands {:.1?} {:.1?}, the player at {:.1}",
                    bone("LeftHand").map(|h| h.translation()),
                    bone("RightHand").map(|h| h.translation()),
                    player_at
                );
                if near.is_some_and(|h| h.distance(player_at) < f.s.sweep_radius) {
                    f.swept = true;
                    info!("boss fight: swept the player ({} harm)", f.s.sweep_damage);
                    harm(&mut damage, pe, f.s.sweep_damage, DamageKind::Melee, near);
                }
            }
            "ThrowRock" => {
                let hand = if matches!(f.act, Act::Throw(2)) { "RightHand" } else { "LeftHand" };
                if let Some(from) = bone(hand).map(|h| h.translation()) {
                    let flight = (from.distance(player_at) / ROCK_SPEED).clamp(1.0, 3.5);
                    let velocity = (player_at - from) / flight + Vec3::Y * 0.5 * GRAVITY * flight;
                    f.rocks_thrown += 1;
                    sound(&mut sfx, &f.s, "BossGruntRockThrow", head.map(|h| h.translation()));
                    commands.spawn((
                        Name::new("ink monster's rock"),
                        Rock { velocity, age: 0.0, damage: f.s.rock_damage, radius: f.s.rock_radius },
                        Transform::from_translation(from),
                        Visibility::Visible,
                        Collider::sphere(ROCK_SIZE),
                        RigidBody::Kinematic,
                        Health::new(f.s.rock_health.max(1.0)),
                        Mesh3d(meshes.add(Sphere::new(ROCK_SIZE))),
                        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.05, 0.05, 0.06), perceptual_roughness: 0.9, ..default() })),
                    ));
                }
            }
            "StartFirebreath" => {
                f.breath = Some(player_at);
                sound(&mut sfx, &f.s, "BossGruntAttack", head.map(|h| h.translation()));
            }
            "StopFirebreath" => f.breath = None,
            "DoCameraShakeAndPadRumble" => {
                shakes.write(crate::shake::Shake::new("explosionrumble", 0.6));
            }
            _ => debug!("boss fight: {event} at {at:.2} s"),
        }
    }
    // The fire breath: from the mouth at where the player was, following him
    // slowly; it burns within its reach of the flame.
    if let (Some(aim), Some(mouth)) = (f.breath.as_mut(), bone("Jaw").or(head).map(|j| j.translation())) {
        *aim = aim.lerp(player_at, (dt * 1.2).min(1.0));
        let dir = (*aim - mouth).normalize_or_zero();
        let reach = mouth.distance(*aim) + 4.0;
        crate::particles::fx::flame_stream(&mut particles, &mut rng, mouth, dir, Vec3::ZERO, reach);
        let along = (player_at - mouth).dot(dir).clamp(0.0, reach);
        if (mouth + dir * along).distance(player_at) < f.s.breath_radius {
            harm(&mut damage, pe, f.s.breath_damage * dt, DamageKind::Fire, Some(mouth));
        }
    }
    // Posed on its own skeleton, carried by the clip's root motion from where
    // the moves play.
    let time_in = if f.act.looping() { f.t % clip.duration.max(0.01) } else { f.t.min(clip.duration) };
    commands.entity(rig_entity).insert(ClipOverride { clip: handle.clone(), skeleton: f.skeleton.clone(), time: time_in });
    if f.reference.is_none() {
        f.reference = clip.root_motion.as_ref().filter(|rm| Vec3::from_array(rm.start_translation).length() > 0.01).map(|rm| Transform {
            translation: Vec3::from_array(rm.start_translation),
            rotation: Quat::from_array(rm.start_rotation),
            scale: Vec3::ONE,
        });
    }
    let world = move_frame(origin, f.reference, &clip, time_in);
    *boss_t =
        Transform { translation: world.translation + world.rotation * Vec3::Y * crate::player::HALF_HEIGHT, rotation: world.rotation, scale: boss_t.scale };
    // On when the move is over.
    let over = match f.act {
        Act::Idle => {
            f.idle_for -= dt;
            f.idle_for <= 0.0
        }
        Act::KnockedOut => false,
        _ => f.t >= clip.duration,
    };
    if over {
        if f.act == Act::Arrival && !f.hinted {
            f.hinted = true;
            if let Some(h) = host.as_mut() {
                h.story.message("citrabossfight_ShootTheMaskHint");
            }
        }
        if f.act == Act::Knockout {
            // Down: the way to it opens, and the fight is over.
            info!("boss fight: knocked out; the script carries on");
            if let Some(h) = host.as_mut() {
                h.story.message("citrabossfight_EnableInvisibleBridge");
                h.story.gameplay_ended(f.id);
            }
        }
        next(f, across);
    }
}

/// How far across the arena the player is, from the giant's left (0) to its
/// right (1), along its near edge (the middle without one).
fn across(edge: Option<(Vec3, Vec3)>, player: Vec3) -> f32 {
    let Some((left, right)) = edge else { return 0.5 };
    let along = (right - left).with_y(0.0);
    ((player - left).with_y(0.0).dot(along) / along.length_squared().max(1.0)).clamp(0.0, 1.0)
}

/// A move's pose about the giant's place: its root motion since the clip
/// began, and for one carrying the moves' reference in its hips, that
/// reference taken off.
fn move_frame(origin: Transform, reference: Option<Transform>, clip: &fc3_formats::anim::Clip, t: f32) -> Transform {
    let Some(rm) = &clip.root_motion else { return origin };
    let (tr, q) = rm.sample(t.clamp(0.0, clip.duration));
    let motion = Transform { translation: Vec3::from_array(tr), rotation: Quat::from_array(q), scale: Vec3::ONE };
    let referenced = Vec3::from_array(rm.start_translation).length() > 0.01;
    let hips = clip.tracks.iter().find(|t| t.bone == 0).and_then(|h| h.translations.first()).map(|h| Vec3::from_array(*h));
    match reference {
        Some(r) if !referenced && hips.is_some_and(|h| h.length() > BAKED_REFERENCE) => origin * Transform::from_matrix(r.to_matrix().inverse()) * motion,
        _ => origin * motion,
    }
}

/// Hits on the mask, kept for the fight's update.
fn mask_hits(mut fight: ResMut<BossFight>, mut hits: MessageReader<DamageEvent>) {
    let Some(f) = fight.active.as_mut() else {
        hits.clear();
        return;
    };
    let Some(mask) = f.mask else { return };
    f.struck.extend(hits.read().filter(|h| h.target == mask).map(|h| h.amount));
}

/// Starts a move afresh.
fn begin(f: &mut Fight, act: Act) {
    f.act = act;
    f.clip = None;
    f.t = 0.0;
    f.fired = 0;
    f.swept = false;
    f.breath = None;
    if act == Act::Idle {
        f.idle_for = f.s.attack_cooldown.max(0.5) + f.s.idle_between_attacks;
    }
}

/// What comes after the move that ended: from an attack, idling; from
/// idling, the next attack of those the phase has (sweeps, then rocks, then
/// fire from the second phase), turning on.
fn next(f: &mut Fight, across: f32) {
    let after = match f.act {
        Act::Knockout | Act::KnockedOut => Act::KnockedOut,
        Act::Throw(n) => Act::ThrowExit(n),
        Act::Idle => {
            let mut kinds = vec![0u8, 1];
            if f.phase >= 1 {
                kinds.push(2);
            }
            let kind = kinds[f.attacks % kinds.len()];
            f.attacks += 1;
            match kind {
                // (The hand on the player's side.)
                0 => Act::Sweep { left: across < 0.5 },
                1 => Act::Throw((f.rocks_thrown % 3 + 1) as u8),
                _ => Act::Breath,
            }
        }
        _ => Act::Idle,
    };
    begin(f, after);
}

fn harm(damage: &mut MessageWriter<DamageEvent>, player: Entity, amount: f32, kind: DamageKind, from: Option<Vec3>) {
    damage.write(DamageEvent { target: player, amount, kind, source: from, instigator: None, headshot: false, hit: Hit::Whole });
}

fn sound(sfx: &mut MessageWriter<crate::sound::PlaySfx>, s: &fc3_formats::gsf::BossFightSettings, name: &str, at: Option<Vec3>) {
    if let Some(&event) = s.sounds.get(name) {
        sfx.write(crate::sound::PlaySfx { event, at, volume: 1.0 });
    }
}

/// Thrown rocks: flying on, shot down, or landing (harming the player if
/// near).
fn rocks(
    time: Res<Time>,
    fight: Res<BossFight>,
    spatial: SpatialQuery,
    mut rocks: Query<(Entity, &mut Rock, &mut Transform, &Health), Without<Player>>,
    players: Query<(Entity, &Transform), With<Player>>,
    mut damage: MessageWriter<DamageEvent>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    (mut particles, mut rng): (ResMut<crate::particles::Particles>, ResMut<crate::weapons::Rng>),
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let Ok((pe, pt)) = players.single() else { return };
    for (e, mut rock, mut t, health) in &mut rocks {
        rock.age += dt;
        if health.dead || health.current < health.max {
            // Shot down.
            crate::particles::fx::explosion(&mut particles, &mut rng, t.translation, ROCK_SIZE);
            commands.entity(e).despawn();
            continue;
        }
        let step = rock.velocity * dt;
        rock.velocity.y -= GRAVITY * dt;
        let filter = SpatialQueryFilter::from_excluded_entities([e, pe]);
        let landed = Dir3::new(step)
            .ok()
            .and_then(|d| spatial.cast_ray(t.translation, d, step.length() + ROCK_SIZE, true, &filter))
            .map(|h| t.translation + step.normalize_or_zero() * h.distance);
        let near_player = t.translation.distance(pt.translation) < ROCK_SIZE + 0.8;
        if landed.is_some() || near_player || rock.age > 8.0 {
            let at = landed.unwrap_or(t.translation);
            if at.distance(pt.translation) < rock.radius {
                harm(&mut damage, pe, rock.damage, DamageKind::Explosion, Some(at));
            }
            crate::particles::fx::explosion(&mut particles, &mut rng, at, rock.radius);
            if let Some(s) = fight.active.as_ref().map(|f| &f.s) {
                sound(&mut sfx, s, "RockImpact", Some(at));
            }
            commands.entity(e).despawn();
            continue;
        }
        t.translation += step;
    }
}

/// The story's autopilot (tests) in the fight: the bow in hand, shooting
/// the mask through the sights.
fn autopilot(
    fight: Res<BossFight>,
    time: Res<Time>,
    spots: Query<&GlobalTransform, With<WeakSpot>>,
    mut players: Query<(&Transform, &mut Player, &crate::weapons::Inventory)>,
    mut actions: ResMut<crate::controls::ActionState>,
    mut next_shot: Local<f32>,
) {
    if std::env::var_os("FC3_STORY_AUTOPILOT").is_none() {
        return;
    }
    let Some(f) = fight.active.as_ref() else { return };
    if matches!(f.act, Act::Arrival | Act::Knockout | Act::KnockedOut) {
        return;
    }
    let Some(spot) = f.mask.and_then(|m| spots.get(m).ok()) else { return };
    let Ok((t, mut p, inv)) = players.single_mut() else { return };
    let eye = t.translation + Vec3::Y * (p.eye - crate::player::HALF_HEIGHT);
    let aim = spot.translation() - eye;
    p.yaw = (-aim.x).atan2(-aim.z);
    p.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.3));
    use crate::controls::Action;
    let class = inv.def().class;
    if class != crate::weapons::WeaponClass::Melee {
        actions.hold(Action::Aim);
    }
    let now = time.elapsed_secs();
    if (now * 0.5) as u32 != ((now - time.delta_secs()) * 0.5) as u32 {
        info!(
            "boss fight autopilot: the mask at {:.1} ({:.0} m, yaw {:.2} pitch {:.2}), {} in hand ({:?}), aim blend {:.2}, {} in the magazine, {:.0} health left",
            spot.translation(),
            aim.length(),
            p.yaw,
            p.pitch,
            inv.def().name,
            class,
            inv.aim_blend,
            inv.slot().mag,
            f.hp
        );
    }
    if now >= *next_shot && inv.aim_blend > 0.95 {
        *next_shot = now + 0.4;
        if inv.slot().mag == 0 {
            actions.press(Action::Reload);
        } else {
            actions.press(Action::Fire);
        }
    }
}
