//! Enemy NPCs: spawning on the world's guard-post markers, perception,
//! a patrol/alert/combat/search state machine, shooting, and death.
//!
//! Visuals are a placeholder figure built from primitives until skinned
//! character rendering is available; all behaviour is independent of it.

use std::collections::HashSet;

use avian3d::prelude::*;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;

use crate::cache_assets::{Fc3Clip, Fc3Model, Fc3Sector};
use crate::characters::{spawn_character, AnimKind, AnimRequest, CharacterAssets, CharacterRig, ClipSet};
use crate::combat::{DamageEvent, DamageKind, Died, Health};
use crate::objects::SectorObjects;
use crate::player::{Player, HALF_HEIGHT};
use crate::terrain::Heightfield;
use crate::weapons::{EffectAssets, Rng};

pub struct NpcPlugin;

impl Plugin for NpcPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NpcDirector>()
            .add_message::<Noise>()
            .add_message::<SpawnEnemy>()
            .init_resource::<InjuryClips>()
            .init_resource::<SenseTable>()
            .init_resource::<AimTable>()
            .add_systems(Startup, (setup_npc_assets, load_melee_clips))
            .add_systems(OnExit(crate::menu::GameState::Loading), (load_injury_clips, load_senses, load_aim))
            .add_systems(
                Update,
                (
                    spawn_from_sectors,
                    spawn_requested,
                    despawn_far,
                    (perceive, think, act, npc_melee).run_if(in_state(crate::menu::GameState::Playing)),
                    hit_reactions,
                    react_to_death,
                    sink_corpses,
                    ledge_falls,
                )
                    .chain(),
            )
            .add_systems(Update, (npc_test_spawn, npc_test_pose, npc_test_log, npc_test_stealth, kill_test.before(crate::pickups::loot_bodies), melee_test).after(sink_corpses))
            .add_systems(PreUpdate, outpost_fire_test.after(crate::controls::update_actions).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(PostUpdate, track_heads.after(TransformSystems::Propagate));
    }
}

/// Enemies spawn within this distance of the player and despawn beyond `DESPAWN_RANGE`.
const SPAWN_RANGE: f32 = 220.0;
const DESPAWN_RANGE: f32 = 320.0;
/// Upper bound on live enemies, to keep fights readable and frames smooth.
const MAX_ALIVE: usize = 28;
/// Outpost guards may go this far past it.
const GARRISON_EXTRA: usize = 12;
/// How long a killed post stays empty before it can be re-garrisoned.
const RESPAWN_COOLDOWN: f32 = 600.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Faction {
    Pirate,
    Privateer,
    /// Friendly: holds liberated outposts and fights the others.
    Rebel,
}

impl Faction {
    pub fn hostile_to_player(self) -> bool {
        self != Faction::Rebel
    }

    pub fn hostile_to(self, other: Faction) -> bool {
        (self == Faction::Rebel) != (other == Faction::Rebel)
    }
}

/// What an enemy fights with, which sets its kit, toughness and tactics.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Role {
    Assault,
    /// Rushes in with a shotgun.
    Shotgunner,
    /// Hangs back with a scoped rifle: slow, accurate, hard-hitting.
    Sniper,
    /// Armoured, with a light machine gun; only taken down with the skill.
    Heavy,
    /// Fires rockets.
    Rocket,
    /// Lobs firebombs, with a pistol to back them up.
    Molotov,
    /// Armoured, with a flamethrower; closes in to burn.
    Flamer,
}

/// Combat numbers for a role.
pub struct RoleStats {
    pub health: f32,
    /// Base chance a shot finds its mark, and damage per bullet or pellet.
    pub accuracy: f32,
    pub damage: f32,
    /// Distance band the enemy tries to fight from.
    pub near: f32,
    pub far: f32,
    /// Longest range it opens fire at.
    pub reach: f32,
    pub mag: u32,
    /// Shots per burst, the gap between them, and the pause between bursts.
    pub burst: (u32, u32),
    pub interval: f32,
    pub pause: (f32, f32),
    pub pellets: u32,
    pub reload: f32,
    /// Combat movement speed multiplier.
    pub pace: f32,
    /// How much it sidesteps while fighting.
    pub strafe: f32,
    /// Multiplier on body-shot bullet damage taken.
    pub armour: f32,
    /// Sight range multiplier.
    pub sight: f32,
}

impl Role {
    /// The game's shooting preferences and NPC weapon for a role whose fire
    /// lands by the damage lap (`fc3_formats::aim`): the riflemen, shotgunners,
    /// snipers and machine gunners (rockets, firebombs and flames keep theirs).
    fn aim(self, faction: Faction) -> Option<(&'static str, &'static str)> {
        Some(match (self, faction) {
            (Role::Assault, Faction::Privateer) => ("assaultrifle", "P416_NPC"),
            (Role::Assault, _) => ("assaultrifle", "AK47_NPC"),
            (Role::Shotgunner, _) => ("shotgun", "M133_NPC"),
            (Role::Sniper, Faction::Privateer) => ("rifle", "R700_NPC"),
            (Role::Sniper, _) => ("rifle", "SVD_NPC"),
            (Role::Heavy, _) => ("machinegun", "PKM_NPC"),
            _ => return None,
        })
    }

    /// The game's enemy archetype for this role in a faction (its counters:
    /// health, armour and what harms it), `fc3_formats::counters`.
    pub fn archetype(self, faction: Faction) -> &'static str {
        match (faction, self) {
            (Faction::Pirate, Role::Assault) => "Pirate_Assaulter",
            (Faction::Pirate, Role::Shotgunner) => "Pirate_Defender",
            (Faction::Pirate, Role::Sniper) => "Pirate_Sniper",
            (Faction::Pirate, Role::Heavy) => "Pirate_HeavyGunner",
            (Faction::Pirate, Role::Rocket) => "Pirate_Rocket",
            (Faction::Pirate | Faction::Privateer, Role::Molotov) => "Pirate_Molotov",
            (Faction::Pirate | Faction::Privateer, Role::Flamer) => "Privateer_HeavyFlammer",
            (Faction::Privateer, Role::Assault) => "Privateer_Assaulter",
            (Faction::Privateer, Role::Shotgunner) => "Privateer_Defender",
            (Faction::Privateer, Role::Sniper) => "Privateer_Sniper",
            (Faction::Privateer, Role::Heavy) => "Privateer_HeavyGunner",
            (Faction::Privateer, Role::Rocket) => "Privateer_Rocket",
            (Faction::Rebel, Role::Shotgunner) => "Rebel_Defender",
            (Faction::Rebel, Role::Sniper) => "Rebel_Sniper",
            (Faction::Rebel, Role::Rocket) => "Rebel_Rocket",
            (Faction::Rebel, _) => "Rebel_Assaulter",
        }
    }

    pub fn stats(self, faction: Faction) -> RoleStats {
        let mut s = match self {
            Role::Assault => RoleStats {
                health: 100.0,
                accuracy: 0.38,
                damage: 7.0,
                near: 10.0,
                far: 35.0,
                reach: 140.0,
                mag: 30,
                burst: (3, 6),
                interval: 0.11,
                pause: (0.6, 1.4),
                pellets: 1,
                reload: 2.5,
                pace: 1.0,
                strafe: 0.8,
                armour: 1.0,
                sight: 1.0,
            },
            Role::Shotgunner => RoleStats {
                health: 120.0,
                accuracy: 0.55,
                damage: 6.0,
                near: 0.0,
                far: 5.0,
                reach: 28.0,
                mag: 6,
                burst: (1, 1),
                interval: 0.9,
                pause: (0.7, 1.1),
                pellets: 8,
                reload: 3.0,
                pace: 1.45,
                strafe: 0.5,
                armour: 1.0,
                sight: 1.0,
            },
            Role::Sniper => RoleStats {
                health: 80.0,
                accuracy: 0.6,
                damage: 30.0,
                near: 25.0,
                far: 150.0,
                reach: 260.0,
                mag: 5,
                burst: (1, 1),
                interval: 1.0,
                pause: (2.4, 3.6),
                pellets: 1,
                reload: 3.0,
                pace: 0.8,
                strafe: 0.0,
                armour: 1.0,
                sight: 1.9,
            },
            Role::Heavy => RoleStats {
                health: 230.0,
                accuracy: 0.3,
                damage: 7.0,
                near: 8.0,
                far: 28.0,
                reach: 120.0,
                mag: 100,
                burst: (8, 16),
                interval: 0.09,
                pause: (0.8, 1.6),
                pellets: 1,
                reload: 4.5,
                pace: 0.7,
                strafe: 0.3,
                armour: 0.45,
                sight: 1.0,
            },
            Role::Rocket => RoleStats {
                health: 100.0,
                accuracy: 0.45,
                damage: 65.0,
                near: 18.0,
                far: 60.0,
                reach: 130.0,
                mag: 1,
                burst: (1, 1),
                interval: 1.0,
                pause: (2.0, 3.0),
                pellets: 1,
                reload: 6.0,
                pace: 0.9,
                strafe: 0.6,
                armour: 1.0,
                sight: 1.1,
            },
            Role::Molotov => RoleStats {
                health: 90.0,
                accuracy: 0.4,
                damage: 6.0,
                near: 10.0,
                far: 26.0,
                reach: 60.0,
                mag: 8,
                burst: (1, 3),
                interval: 0.35,
                pause: (0.8, 1.6),
                pellets: 1,
                reload: 2.0,
                pace: 1.1,
                strafe: 0.9,
                armour: 1.0,
                sight: 1.0,
            },
            Role::Flamer => RoleStats {
                health: 210.0,
                accuracy: 1.0,
                // Per flame pulse (about 12 a second).
                damage: 1.6,
                near: 0.0,
                far: 7.0,
                reach: 13.0,
                mag: 200,
                burst: (12, 20),
                interval: 0.08,
                pause: (0.5, 0.9),
                pellets: 1,
                reload: 3.0,
                pace: 0.95,
                strafe: 0.2,
                armour: 0.5,
                sight: 1.0,
            },
        };
        // Privateers are better trained and equipped.
        if faction == Faction::Privateer {
            s.health *= 1.3;
            s.accuracy = (s.accuracy + 0.08).min(1.0);
            s.damage *= 1.25;
        }
        s
    }

    /// Takedowns on this role need the heavy takedown skill.
    pub fn armoured(self) -> bool {
        matches!(self, Role::Heavy | Role::Flamer)
    }
}

/// The role a guard post's marker asks for, if it names one.
fn post_role(name: &str, faction: Faction) -> Option<Role> {
    let n = name.to_ascii_lowercase();
    if n.contains("molotov") {
        Some(if faction == Faction::Pirate { Role::Molotov } else { Role::Assault })
    } else if n.contains("heavyflamer") {
        Some(Role::Flamer)
    } else if n.contains("heavygunner") {
        Some(Role::Heavy)
    } else if n.contains("beheader") {
        Some(Role::Shotgunner)
    } else if n.contains("sniper") {
        Some(Role::Sniper)
    } else {
        None
    }
}

fn random_role(faction: Faction, rng: &mut Rng) -> Role {
    let table: &[(Role, f32)] = match faction {
        Faction::Rebel => &[(Role::Assault, 80.0), (Role::Sniper, 20.0)],
        Faction::Pirate => &[
            (Role::Assault, 52.0),
            (Role::Shotgunner, 13.0),
            (Role::Sniper, 8.0),
            (Role::Heavy, 8.0),
            (Role::Rocket, 7.0),
            (Role::Molotov, 9.0),
            (Role::Flamer, 3.0),
        ],
        Faction::Privateer => {
            &[(Role::Assault, 46.0), (Role::Shotgunner, 12.0), (Role::Sniper, 11.0), (Role::Heavy, 12.0), (Role::Rocket, 9.0), (Role::Flamer, 10.0)]
        }
    };
    let total: f32 = table.iter().map(|(_, w)| w).sum();
    let mut x = rng.f32() * total;
    for (r, w) in table {
        if x < *w {
            return *r;
        }
        x -= w;
    }
    Role::Assault
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NpcState {
    /// At post: idle, looking around.
    Idle,
    /// Something was noticed; walk toward it.
    Suspicious,
    /// Engaging a visible target.
    Combat,
    /// Lost sight; moving to the last known position.
    Search,
}

#[derive(Component)]
pub struct Npc {
    /// Its damage lap on the player (`AimTable`).
    pub lap: LapState,
    /// A rifle-butt strike under way (seconds in), and the wait before the next.
    pub melee: Option<f32>,
    pub melee_cooldown: f32,
    pub faction: Faction,
    pub role: Role,
    pub state: NpcState,
    pub post: Vec3,
    pub post_yaw: f32,
    /// Detection meter 0..1; full means the player is spotted.
    pub awareness: f32,
    pub last_known: Option<Vec3>,
    pub sees_player: bool,
    pub yaw: f32,
    pub velocity: Vec3,
    pub state_time: f32,
    pub fire_cooldown: f32,
    pub burst_left: u32,
    pub mag: u32,
    pub reload: f32,
    pub strafe_dir: f32,
    pub marker_id: u64,
    pub accuracy: f32,
    pub damage: f32,
    /// Skinned character rig, when one was spawned (else the primitive figure is used).
    pub rig: Option<Entity>,
    /// Seconds since this NPC last fired.
    pub since_fired: f32,
    step_timer: f32,
    /// Seconds spent in combat this engagement (accuracy ramps up with it).
    pub engaged: f32,
    /// Running to this alarm box to raise the alarm.
    pub alarm_target: Option<Vec3>,
    /// Seconds until the next thrown grenade or firebomb.
    pub throw_cooldown: f32,
    /// Seconds spent trying to move without getting anywhere.
    stuck: f32,
    /// The way to the current goal over the navigation mesh, the goal it was
    /// planned for, and seconds until it is planned again.
    path: Vec<Vec3>,
    path_goal: Option<Vec3>,
    path_timer: f32,
    /// A hostile NPC being fought (rebels against the others, and back).
    pub foe: Option<Entity>,
    /// Whether `foe` is in sight.
    pub foe_seen: bool,
    /// Seconds until the next look around for foes.
    scan: f32,
    /// Walks between spots around the post while idle.
    pub patrols: bool,
    /// Posted at a guard spot (see `guard_spots`): faces its way, no looking
    /// around.
    pub at_spot: bool,
    wander: Option<Vec3>,
    wander_wait: f32,
    /// Taking cover in a fight, and seconds until the next look for some.
    cover: Option<Cover>,
    cover_search: f32,
    /// Seconds left of a flinch from a hit.
    flinch: f32,
}

/// How an enemy was moving, where it was hit and from which side, for its
/// hit and death clips.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Motion {
    Stand,
    Walk,
    Run,
    Crouch,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Region {
    Head,
    Torso,
    Legs,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Front,
    Back,
    Left,
    Right,
}

/// The game's death and hit clips, sorted by motion, region and side (from
/// the converter's index, `cache::INJURY_CLIPS_REL`).
#[derive(Resource, Default)]
pub struct InjuryClips {
    deaths: Vec<(Motion, Region, Side, Handle<Fc3Clip>)>,
    hits: Vec<(Motion, Region, Side, Handle<Fc3Clip>)>,
}

/// Reads a clip's motion, region and side from its path (`death\run\torso\
/// ..._dietorso_frombk_...`), and whether it is a death.
fn injury_kind(path: &str) -> Option<(bool, Motion, Region, Side)> {
    let p = path.to_ascii_lowercase();
    let file = p.rsplit('\\').next()?;
    let death = p.contains("\\death\\");
    let motion = if p.contains("\\run\\") {
        Motion::Run
    } else if p.contains("\\walk\\") {
        Motion::Walk
    } else if p.contains("\\crouch\\") || p.contains("\\coverhi\\") {
        Motion::Crouch
    } else {
        Motion::Stand
    };
    let region = if file.contains("head") {
        Region::Head
    } else if file.contains("legs") {
        Region::Legs
    } else {
        Region::Torso
    };
    let side = if file.contains("frombk") || file.contains("from180") {
        Side::Back
    } else if file.contains("fromfw") {
        Side::Front
    } else if file.contains("fromlt") {
        Side::Left
    } else if file.contains("fromrt") {
        Side::Right
    } else {
        return None;
    };
    Some((death, motion, region, side))
}

fn load_injury_clips(mut clips: ResMut<InjuryClips>, server: Res<AssetServer>, info: Res<crate::data::CacheInfo>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::INJURY_CLIPS_REL)).unwrap_or_default();
    for line in text.lines() {
        let Some((death, motion, region, side)) = injury_kind(line) else { continue };
        let handle: Handle<Fc3Clip> = server.load(crate::cache_assets::cache_path(&fc3_formats::cache::anim_rel(line)));
        if death {
            clips.deaths.push((motion, region, side, handle));
        } else {
            clips.hits.push((motion, region, side, handle));
        }
    }
    info!("{} death and {} hit clips", clips.deaths.len(), clips.hits.len());
}

impl InjuryClips {
    /// The best match, relaxing the side, then the region, then the motion.
    fn pick(list: &[(Motion, Region, Side, Handle<Fc3Clip>)], m: Motion, r: Region, s: Side, rng: &mut Rng) -> Option<Handle<Fc3Clip>> {
        let tries: [&dyn Fn(&(Motion, Region, Side, Handle<Fc3Clip>)) -> bool; 4] =
            [&|c| c.0 == m && c.1 == r && c.2 == s, &|c| c.0 == m && c.1 == r, &|c| c.0 == m && c.2 == s, &|c| c.0 == Motion::Stand && c.2 == s];
        for t in tries {
            let found: Vec<&Handle<Fc3Clip>> = list.iter().filter(|c| t(c)).map(|c| &c.3).collect();
            if !found.is_empty() {
                return Some(found[rng.next_u32() as usize % found.len()].clone());
            }
        }
        None
    }
}

/// Which side of an enemy a shot came from.
fn side_of(npc: &Npc, pos: Vec3, from: Vec3) -> Side {
    let facing = Vec3::new(-npc.yaw.sin(), 0.0, -npc.yaw.cos());
    let to = Vec3::new(from.x - pos.x, 0.0, from.z - pos.z).normalize_or(facing);
    let ahead = facing.dot(to);
    if ahead > 0.7 {
        Side::Front
    } else if ahead < -0.7 {
        Side::Back
    } else if facing.cross(Vec3::Y).dot(to) > 0.0 {
        Side::Right
    } else {
        Side::Left
    }
}

fn motion_of(npc: &Npc) -> Motion {
    let speed = Vec2::new(npc.velocity.x, npc.velocity.z).length();
    if npc.hiding() {
        Motion::Crouch
    } else if speed > 2.6 {
        Motion::Run
    } else if speed > 0.35 {
        Motion::Walk
    } else {
        Motion::Stand
    }
}

/// Swaps one clip of a rig's set (its own copy).
fn swap_clip(rig: &mut CharacterRig, kind: AnimKind, clip: Handle<Fc3Clip>) {
    let mut set = ClipSet::clone(rig.clips());
    set.clips.insert(kind, clip);
    rig.set_clips(std::sync::Arc::new(set));
}

/// Enemies flinch from hits that don't kill them.
fn hit_reactions(
    mut damage: MessageReader<DamageEvent>,
    melee_hits: Option<Res<MeleeHitClips>>,
    clips: Res<InjuryClips>,
    mut rng: ResMut<Rng>,
    mut npcs: Query<(&Transform, &mut Npc, &Health)>,
    mut rigs: Query<&mut CharacterRig>,
) {
    for ev in damage.read() {
        if !matches!(ev.kind, DamageKind::Bullet | DamageKind::Melee) {
            continue;
        }
        let Ok((t, mut npc, h)) = npcs.get_mut(ev.target) else { continue };
        if h.dead || npc.flinch > 0.0 {
            continue;
        }
        let from = ev.source.unwrap_or(t.translation);
        // A blow in close quarters: the game's struck-by-melee reactions.
        if ev.kind == DamageKind::Melee {
            let side = side_of(&npc, t.translation, from);
            let i = match side {
                Side::Front => 0,
                Side::Left => 1,
                Side::Right => 2,
                Side::Back => 3,
            };
            if let (Some(clip), Some(mut r)) = (melee_hits.as_ref().and_then(|m| m.0.get(i).cloned()), npc.rig.and_then(|r| rigs.get_mut(r).ok())) {
                swap_clip(&mut r, AnimKind::Hit, clip);
                npc.flinch = 0.9;
                npc.melee = None;
            }
            continue;
        }
        let region = if ev.headshot { Region::Head } else { Region::Torso };
        let motion = if npc.hiding() { Motion::Crouch } else { Motion::Stand };
        let Some(clip) = InjuryClips::pick(&clips.hits, motion, region, side_of(&npc, t.translation, from), &mut rng) else { continue };
        if let Some(mut r) = npc.rig.and_then(|r| rigs.get_mut(r).ok()) {
            swap_clip(&mut r, AnimKind::Hit, clip);
            npc.flinch = 0.45;
        }
    }
}

/// A spot behind low cover: run there, then keep down and pop up to fire.
#[derive(Clone, Copy, Debug)]
struct Cover {
    spot: Vec3,
    settled: bool,
    /// Up and firing (else down behind it), and seconds left of that.
    up: bool,
    timer: f32,
    /// Seconds before moving on.
    stay: f32,
}

impl Npc {
    /// Down behind cover (out of sight below the top of it).
    pub fn hiding(&self) -> bool {
        self.cover.is_some_and(|c| c.settled && !c.up)
    }
}

/// Where an enemy's head is (from its rig), for headshots.
#[derive(Component, Default)]
pub struct HeadPos(pub Vec3);

fn track_heads(
    mut npcs: Query<(&Npc, &GlobalTransform, &mut HeadPos)>,
    joints: Query<&GlobalTransform, Without<Npc>>,
    rigs: Query<&crate::characters::CharacterRig>,
) {
    let head = fc3_formats::hash::name_hash("Head");
    for (npc, gt, mut h) in &mut npcs {
        let joint = npc.rig.and_then(|r| rigs.get(r).ok()).and_then(|r| r.joint(head)).and_then(|(_, j)| joints.get(j).ok());
        h.0 = match joint {
            // The joint sits at the neck end of the skull.
            Some(j) => j.translation() + Vec3::Y * 0.08,
            None => gt.translation() + Vec3::Y * 0.67,
        };
    }
}

/// A spot behind low cover between `pos` and `threat` (capsule centres): a
/// wall of the navigation mesh facing the threat, with something solid in
/// front at chest height and a clear view over it.
fn find_cover(nav: &crate::nav::NavMesh, spatial: &SpatialQuery, me: Entity, player: Entity, pos: Vec3, threat: Vec3) -> Option<Vec3> {
    let feet = pos - Vec3::Y * HALF_HEIGHT;
    let eyes = threat + Vec3::Y * 0.6;
    let mut spots = nav.walls_near(feet, 14.0);
    spots.sort_by(|a, b| a.0.distance_squared(feet).total_cmp(&b.0.distance_squared(feet)));
    let filter = SpatialQueryFilter::from_excluded_entities([me, player]);
    let mut tries = 0;
    for (spot, away) in spots {
        let to = Vec3::new(eyes.x - spot.x, 0.0, eyes.z - spot.z);
        let d = to.length();
        // The wall has to be between the spot and the threat.
        if !(8.0..50.0).contains(&d) || away.dot(to / d) > -0.3 {
            continue;
        }
        tries += 1;
        if tries > 8 {
            break;
        }
        let (low, high) = (spot + Vec3::Y * 0.9, spot + Vec3::Y * 1.6);
        let (Ok(dl), Ok(dh)) = (Dir3::new(eyes - low), Dir3::new(eyes - high)) else { continue };
        let covered = spatial.cast_ray(low, dl, 1.6, true, &filter).is_some();
        let view = spatial.cast_ray(high, dh, (eyes.distance(high) - 1.0).max(0.1), true, &filter).is_none();
        if covered && view {
            return Some(spot);
        }
    }
    None
}

/// Spawns an enemy outside the garrison system (reinforcements, patrol crews).
#[derive(Message, Clone, Copy)]
pub struct SpawnEnemy {
    pub at: Vec3,
    /// Arrives hunting this position.
    pub hunting: Option<Vec3>,
    pub garrison: Option<usize>,
    pub faction: Faction,
    /// A fighting role, or a random one suited to reinforcements.
    pub role: Option<Role>,
    /// Starts aboard a vehicle.
    pub ride: Option<Riding>,
}

/// An enemy carried by a vehicle (patrol crews): placed at a seat each frame
/// and left out of the on-foot AI until it gets out.
#[derive(Component, Clone, Copy)]
pub struct Riding {
    pub vehicle: Entity,
    /// Feet position in the vehicle's model space.
    pub seat: Vec3,
    /// Shown (a gunner standing in the back) or hidden (the driver).
    pub visible: bool,
}

/// Body parts of the placeholder figure, animated procedurally.
#[derive(Component)]
struct NpcLimb {
    kind: Limb,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Limb {
    LeftLeg,
    RightLeg,
    Torso,
    Gun,
}

/// A dead NPC: tips over and sinks out of sight before despawning.
#[derive(Component)]
struct Corpse {
    age: f32,
    fall_axis: Vec3,
    /// Skinned corpses play a death clip instead of toppling.
    skinned: bool,
    /// A guard has already found this body.
    found: bool,
}

/// A sound the AI can hear (gunshots, explosions).
#[derive(Message, Clone, Copy)]
pub struct Noise {
    pub at: Vec3,
    pub radius: f32,
    pub kind: NoiseKind,
}

/// What a sound means to those who hear it (animals hear each kind only so
/// far, and run from the threatening ones).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoiseKind {
    /// A thrown rock, a voice: worth a look.
    Interesting,
    /// Gunfire and flames.
    Threatening,
    Explosion,
}

#[derive(Resource, Default)]
pub struct NpcDirector {
    /// Markers already garrisoned in loaded sectors.
    occupied: HashSet<u64>,
    /// Markers whose occupant was killed, with the time of death.
    cleared: Vec<(u64, f32)>,
    pub kills: u32,
    /// Ids for enemies spawned without a marker.
    next_free_id: u64,
}

impl NpcDirector {
    /// Whether this post's guard was killed recently.
    pub fn was_killed(&self, marker: u64) -> bool {
        self.cleared.iter().any(|(id, _)| *id == marker)
    }
}

/// Seconds a body lies (and can be found) before it sinks away.
const CORPSE_LIFE: f32 = 75.0;
/// Guards notice bodies within this distance.
const BODY_SIGHT: f32 = 20.0;

/// Marker ids at or above this belong to enemies spawned without a post.
pub(crate) const FREE_ID_BASE: u64 = 0x4000_0000_0000_0000;

#[derive(Resource)]
struct NpcAssets {
    body: Handle<Mesh>,
    head: Handle<Mesh>,
    leg: Handle<Mesh>,
    gun: Handle<Mesh>,
    pirate: Handle<StandardMaterial>,
    privateer: Handle<StandardMaterial>,
    skin: Handle<StandardMaterial>,
    dark: Handle<StandardMaterial>,
}

fn setup_npc_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let mat = |c: Color| StandardMaterial { base_color: c, perceptual_roughness: 0.85, ..default() };
    commands.insert_resource(NpcAssets {
        body: meshes.add(Capsule3d::new(0.22, 0.5)),
        head: meshes.add(Sphere::new(0.13).mesh().uv(16, 10)),
        leg: meshes.add(Capsule3d::new(0.09, 0.62)),
        gun: meshes.add(Cuboid::new(0.06, 0.08, 0.75)),
        pirate: materials.add(mat(Color::srgb(0.62, 0.12, 0.1))),
        privateer: materials.add(mat(Color::srgb(0.28, 0.3, 0.22))),
        skin: materials.add(mat(Color::srgb(0.55, 0.4, 0.3))),
        dark: materials.add(mat(Color::srgb(0.08, 0.08, 0.08))),
    });
}

/// Classifies a marker entity name into a garrison post, if it is one.
fn post_faction(name: &str) -> Option<Faction> {
    let n = name.to_ascii_lowercase();
    let is_post = n.starts_with("cautious/") || n.starts_with("relax/") || n.starts_with("relax_and_cautious/") || n.starts_with("campencounter/");
    // Boat patrols put to sea instead (see `patrols`).
    if !is_post || n.contains("villager") || n.contains("civil") || n.contains("boatpatrol") || n.contains("patrolboat") {
        return None;
    }
    Some(if n.contains("rebel") {
        Faction::Rebel
    } else if n.contains("privateer") {
        Faction::Privateer
    } else {
        Faction::Pirate
    })
}

/// Rebels holding a liberated outpost use its posts under ids of their own.
const REBEL_POST: u64 = 1 << 61;

#[allow(clippy::too_many_arguments)]
fn spawn_from_sectors(
    time: Res<Time>,
    mut director: ResMut<NpcDirector>,
    assets: Option<Res<NpcAssets>>,
    sectors: Res<Assets<Fc3Sector>>,
    loaded: Query<&SectorObjects>,
    players: Query<&Transform, With<Player>>,
    npcs: Query<(), With<Npc>>,
    heights: Res<Heightfield>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
    chars: Option<Res<CharacterAssets>>,
    models: Res<Assets<Fc3Model>>,
    server: Res<AssetServer>,
    mut outposts: ResMut<crate::outposts::Outposts>,
    area: Res<crate::story::MissionArea>,
) {
    // FC3_PEACEFUL (tests): no enemies.
    if std::env::var("FC3_PEACEFUL").is_ok() {
        return;
    }
    let Some(assets) = assets else { return };
    let Ok(player) = players.single() else { return };
    // Wait for character assets (briefly) so enemies appear fully animated.
    let chars_ready = chars.as_ref().is_some_and(|c| c.ready(&server));
    if !chars_ready && time.elapsed_secs() < 30.0 {
        return;
    }
    let chars = chars.filter(|_| chars_ready);
    let now = time.elapsed_secs();
    director.cleared.retain(|(_, t)| now - *t < RESPAWN_COOLDOWN);
    let mut alive = npcs.iter().count();
    if alive >= MAX_ALIVE + GARRISON_EXTRA {
        return;
    }
    let cleared: HashSet<u64> = director.cleared.iter().map(|(id, _)| *id).collect();
    let mut candidates = Vec::new();
    for so in &loaded {
        let Some(sector) = sectors.get(&so.sector) else { continue };
        for e in &sector.entities {
            let Some(mut faction) = post_faction(&e.name) else { continue };
            let pos = Vec3::from_array(e.translation);
            // A mission being played here has its own people.
            if area.contains(pos) {
                continue;
            }
            let mut id = e.id;
            // Outposts: enemy ones are fully manned; liberated ones by rebels.
            let mut outpost = outposts.at(pos);
            if let Some(i) = outpost {
                let o = &mut outposts.list[i];
                if o.liberated {
                    faction = Faction::Rebel;
                    id ^= REBEL_POST;
                    outpost = None;
                } else if !o.posts.iter().any(|(id, _)| *id == e.id) {
                    o.posts.push((e.id, pos));
                }
            }
            if director.occupied.contains(&id) || cleared.contains(&id) {
                continue;
            }
            let d = pos.distance(player.translation);
            // Don't pop enemies in right in front of the player.
            if d > SPAWN_RANGE || d < 35.0 {
                continue;
            }
            candidates.push((d, id, faction, pos, Quat::from_array(e.rotation), outpost, post_role(&e.name, faction)));
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, id, faction, pos, rot, outpost, role) in candidates {
        // Outposts are manned in full, past the cap that patrols and
        // roadside posts keep to.
        if alive >= MAX_ALIVE + if outpost.is_some() { GARRISON_EXTRA } else { 0 } {
            continue;
        }
        director.occupied.insert(id);
        // Only some roadside posts are manned at any given time; outposts are full.
        if outpost.is_none() && rng.f32() > 0.55 {
            continue;
        }
        let ground = heights.height_at(pos).unwrap_or(pos.y);
        let feet = Vec3::new(pos.x, pos.y.max(ground), pos.z);
        let (yaw, _, _) = rot.to_euler(EulerRot::YXZ);
        let role = role.unwrap_or_else(|| random_role(faction, &mut rng));
        let e = spawn_npc(&mut commands, &assets, chars.as_deref(), &models, faction, role, feet, yaw, id, &mut rng, None);
        if let Some(i) = outpost {
            commands.entity(e).insert(crate::outposts::Garrison(i));
        }
        alive += 1;
    }
}

/// Reinforcements and other scripted spawns.
#[allow(clippy::too_many_arguments)]
fn spawn_requested(
    mut requests: MessageReader<SpawnEnemy>,
    mut director: ResMut<NpcDirector>,
    assets: Option<Res<NpcAssets>>,
    chars: Option<Res<CharacterAssets>>,
    models: Res<Assets<Fc3Model>>,
    server: Res<AssetServer>,
    heights: Res<Heightfield>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
) {
    let Some(assets) = assets else { return };
    let chars = chars.filter(|c| c.ready(&server));
    for r in requests.read() {
        let ground = heights.height_at(r.at).unwrap_or(r.at.y);
        let feet = Vec3::new(r.at.x, ground, r.at.z);
        let yaw = r.hunting.map(|h| yaw_to(h - feet)).unwrap_or(0.0);
        director.next_free_id += 1;
        let id = FREE_ID_BASE + director.next_free_id;
        // Reinforcements come to fight: no snipers or rocket teams.
        let role = r.role.unwrap_or_else(|| match rng.f32() {
            x if x < 0.6 => Role::Assault,
            x if x < 0.8 => Role::Shotgunner,
            x if x < 0.9 => Role::Heavy,
            _ if r.faction == Faction::Pirate => Role::Molotov,
            _ => Role::Assault,
        });
        let e = spawn_npc(&mut commands, &assets, chars.as_deref(), &models, r.faction, role, feet, yaw, id, &mut rng, None);
        if let Some(ride) = r.ride {
            commands.entity(e).insert((ride, ColliderDisabled, if ride.visible { Visibility::Inherited } else { Visibility::Hidden }));
        }
        if let Some(g) = r.garrison {
            commands.entity(e).insert(crate::outposts::Garrison(g));
        }
        if let Some(h) = r.hunting {
            commands.entity(e).entry::<Npc>().and_modify(move |mut n| {
                n.state = NpcState::Search;
                n.awareness = 0.8;
                n.last_known = Some(h);
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_npc(
    commands: &mut Commands,
    a: &NpcAssets,
    chars: Option<&CharacterAssets>,
    models: &Assets<Fc3Model>,
    faction: Faction,
    role: Role,
    feet: Vec3,
    yaw: f32,
    marker_id: u64,
    rng: &mut Rng,
    look: Option<usize>,
) -> Entity {
    let stats = role.stats(faction);
    let cloth = match faction {
        Faction::Pirate => a.pirate.clone(),
        Faction::Privateer | Faction::Rebel => a.privateer.clone(),
    };
    let root = commands
        .spawn((
            new_npc(faction, role, feet, yaw, marker_id, rng),
            HeadPos::default(),
            Health::new(stats.health),
            crate::combat::Armour(stats.armour),
            RigidBody::Kinematic,
            Collider::capsule(0.32, 1.1),
            Transform::from_translation(feet + Vec3::Y * HALF_HEIGHT).with_rotation(Quat::from_rotation_y(yaw)),
            Visibility::Visible,
            Name::new("enemy"),
        ))
        .id();
    if let Some(chars) = chars {
        let looks = chars.looks_for(faction, role);
        let look = look.unwrap_or_else(|| looks.get(rng.next_u32() as usize % looks.len().max(1)).copied().unwrap_or(0));
        if let Some(rig) = spawn_character(commands, root, chars, models, look, rng, Vec3::new(0.0, -HALF_HEIGHT, 0.0)) {
            commands.entity(root).entry::<Npc>().and_modify(move |mut n| n.rig = Some(rig));
            return root;
        }
    }
    // Fallback figure (original, simple primitives), relative to the capsule centre.
    let parts: [(Handle<Mesh>, Handle<StandardMaterial>, Vec3, Limb); 5] = [
        (a.body.clone(), cloth.clone(), Vec3::new(0.0, 0.15, 0.0), Limb::Torso),
        (a.leg.clone(), a.dark.clone(), Vec3::new(-0.1, -0.5, 0.0), Limb::LeftLeg),
        (a.leg.clone(), a.dark.clone(), Vec3::new(0.1, -0.5, 0.0), Limb::RightLeg),
        (a.head.clone(), a.skin.clone(), Vec3::new(0.0, 0.72, 0.0), Limb::Torso),
        (a.gun.clone(), a.dark.clone(), Vec3::new(0.18, 0.25, -0.35), Limb::Gun),
    ];
    for (mesh, mat, at, kind) in parts {
        commands.spawn((Mesh3d(mesh), MeshMaterial3d(mat), Transform::from_translation(at), NpcLimb { kind }, ChildOf(root)));
    }
    root
}

/// Marks an enemy the story is driving (walking to a contextual action,
/// held by a script): it still sees and hears, but doesn't decide or move by
/// itself until something it notices takes it out of the script's hands.
#[derive(Component)]
pub struct Scripted;

/// The AI of a story character fighting as this faction and role (its kit
/// and rig are the story's), posted where it stands.
pub fn story_npc(faction: Faction, role: Role, feet: Vec3, yaw: f32, id: u64, rig: Option<Entity>, rng: &mut Rng) -> impl Bundle {
    let stats = role.stats(faction);
    let mut npc = new_npc(faction, role, feet, yaw, id, rng);
    npc.rig = rig;
    npc.patrols = false;
    (npc, HeadPos::default(), crate::combat::Armour(stats.armour))
}

/// The fighting role an AI's shooting preferences (`tmplPrimaryWeapon`)
/// describe, or none for the unarmed.
pub fn role_from_weapon(prefs: &str) -> Option<Role> {
    let p = prefs.to_ascii_lowercase();
    Some(if p.contains("unused") || p.is_empty() {
        return None;
    } else if p.contains("shotgun") {
        Role::Shotgunner
    } else if p.contains("sniper") {
        Role::Sniper
    } else if p.contains("rpg") || p.contains("rocket") {
        Role::Rocket
    } else if p.contains("flame") {
        Role::Flamer
    } else if p.contains("molotov") {
        Role::Molotov
    } else if p.contains("machinegun") || p.contains("lmg") || p.contains("heavy") {
        Role::Heavy
    } else {
        Role::Assault
    })
}

pub(crate) fn new_npc(faction: Faction, role: Role, feet: Vec3, yaw: f32, marker_id: u64, rng: &mut Rng) -> Npc {
    let stats = role.stats(faction);
    Npc {
        lap: LapState::default(),
        faction,
        role,
        state: NpcState::Idle,
        post: feet,
        post_yaw: yaw,
        awareness: 0.0,
        last_known: None,
        sees_player: false,
        melee: None,
        melee_cooldown: 0.0,
        yaw,
        velocity: Vec3::ZERO,
        state_time: rng.range(0.0, 5.0),
        fire_cooldown: 0.0,
        burst_left: 0,
        mag: stats.mag,
        reload: 0.0,
        strafe_dir: if rng.f32() < 0.5 { -1.0 } else { 1.0 },
        marker_id,
        accuracy: stats.accuracy,
        damage: stats.damage,
        rig: None,
        since_fired: 99.0,
        step_timer: 0.0,
        engaged: 0.0,
        alarm_target: None,
        throw_cooldown: rng.range(4.0, 12.0),
        stuck: 0.0,
        path: Vec::new(),
        path_goal: None,
        path_timer: 0.0,
        foe: None,
        foe_seen: false,
        scan: rng.range(0.0, 0.5),
        patrols: rng.f32() < 0.35 && role != Role::Sniper,
        at_spot: false,
        wander: None,
        wander_wait: rng.range(0.0, 6.0),
        cover: None,
        cover_search: rng.range(0.0, 2.0),
        flinch: 0.0,
    }
}

fn despawn_far(
    mut commands: Commands,
    mut director: ResMut<NpcDirector>,
    players: Query<&Transform, With<Player>>,
    npcs: Query<(Entity, &Transform, &Npc), (Without<Corpse>, Without<Riding>, Without<crate::story_actors::StoryActor>)>,
) {
    let Ok(p) = players.single() else { return };
    for (e, t, npc) in &npcs {
        if t.translation.distance(p.translation) > DESPAWN_RANGE && npc.state != NpcState::Combat {
            director.occupied.remove(&npc.marker_id);
            commands.entity(e).despawn();
        }
    }
}

/// Builds awareness from sight; gunshots and explosions alert nearby enemies.
/// How far (m) guards hear the player's steps at a ground speed.
fn footstep_range(speed: f32, crouching: bool, sprinting: bool) -> f32 {
    if speed < 0.5 {
        0.0
    } else if crouching {
        if speed > 2.5 { 3.5 } else { 1.5 }
    } else if sprinting {
        13.0
    } else {
        6.0
    }
}

/// How often the AI's fire lands on the player (`fc3_formats::aim`), once
/// read from the cache.
#[derive(Resource, Default)]
pub struct AimTable(Option<fc3_formats::aim::Aim>);

fn load_aim(info: Option<Res<crate::data::CacheInfo>>, mut table: ResMut<AimTable>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::aim::AIM_REL)).map_err(anyhow::Error::from).and_then(|t| fc3_formats::aim::from_text(&t)) {
        Ok(a) => {
            info!("aim: {} kinds of shooting preferences, {} NPC weapons", a.prefs.len(), a.weapons.len());
            table.0 = Some(a);
        }
        Err(e) => warn!("aim: no shooting table ({e:#}); re-run the converter"),
    }
}

/// An enemy's damage lap on the player: while it shoots at him without
/// losing him (`chain` seconds), its progress fills at its focus's lap; a
/// shot when it is full lands, the rest miss. The least time before the
/// first hit runs down first.
#[derive(Default, Clone, Copy, Debug)]
pub struct LapState {
    progress: f32,
    buffer: f32,
    chain: f32,
    unseen: f32,
    shooting: bool,
    /// Where he was when last seen.
    lost_at: Vec3,
    /// How exposed he showed at the last shot (its multiplier; 0 unknown).
    exposure: f32,
    focus: f32,
}

/// What a shot by the damage lap needs: the kind's preferences, the
/// weapon's most damage, the difficulty and how many are shooting at him.
struct Lap<'a> {
    prefs: &'a fc3_formats::aim::Prefs,
    max_damage: f32,
    level: usize,
    agents: u32,
    /// The player's damage group: progress falls back after this long (s)
    /// at this rate (percent a second).
    decay: (f32, f32),
}

/// How the AI notices the player (`fc3_formats::senses`), once read from the
/// cache.
#[derive(Resource, Default)]
pub struct SenseTable(Vec<fc3_formats::senses::Senses>);

impl SenseTable {
    /// A role's senses: a sniper's through his scope, the rest the default.
    fn for_role(&self, role: Role) -> Option<&fc3_formats::senses::Senses> {
        let name = if role == Role::Sniper { "aisensorysystem:default.default.scoped" } else { "aisensorysystem:default.default" };
        self.named(name)
    }

    /// A sensory system by its template's name.
    pub fn named(&self, name: &str) -> Option<&fc3_formats::senses::Senses> {
        self.0.iter().find(|s| s.name == name)
    }
}

fn load_senses(info: Option<Res<crate::data::CacheInfo>>, mut table: ResMut<SenseTable>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::senses::SENSES_REL)).map_err(anyhow::Error::from).and_then(|t| fc3_formats::senses::from_text(&t))
    {
        Ok(s) => {
            info!("senses: {} sensory systems", s.len());
            table.0 = s;
        }
        Err(e) => warn!("senses: no sensory table ({e:#}); re-run the converter"),
    }
}

/// The player's posture for the AI's senses: stealthy (crouched), in action
/// (sprinting, driving, or just fired) or normal.
pub(crate) fn posture(player: &Player, inventory: Option<&crate::weapons::Inventory>) -> usize {
    let fired = inventory.is_some_and(|i| !i.slots.is_empty() && i.since_shot < 2.0 && !i.def().silenced);
    if player.sprinting || player.driving || fired {
        2
    } else if player.crouching {
        0
    } else {
        1
    }
}

#[allow(clippy::too_many_arguments)]
fn perceive(
    time: Res<Time>,
    (perks, settings, sense_table): (Res<crate::progression::Perks>, Res<crate::settings::Settings>, Res<SenseTable>),
    spatial: SpatialQuery,
    mut bodies: Query<(Entity, &Transform, &mut Corpse)>,
    (mut shouts, mut sensing): (Local<Vec<Noise>>, Local<std::collections::HashMap<Entity, f32>>),
    mut noises: MessageReader<Noise>,
    mut damage: MessageReader<DamageEvent>,
    players: Query<(Entity, &Transform, &Player, &Health, Option<&crate::weapons::Inventory>)>,
    mut npcs: Query<(Entity, &Transform, &mut Npc, &Health), (Without<Corpse>, Without<Frozen>, Without<Riding>)>,
    beasts: Query<(Entity, &Transform, &Health, &crate::wildlife::WildAnimal), Without<Npc>>,
    (cover, cinema): (Res<crate::vegetation::Concealment>, Res<crate::cinema::Cinema>),
) {
    let dt = time.delta_secs();
    let Ok((pe, pt, player, ph, inventory)) = players.single() else { return };
    let posture = posture(player, inventory);
    // The story's autopilot (tests) steps into triggers in plain view: unseen.
    // (Nor while a cutscene has the player.)
    let unseen = player.noclip || cinema.cutscene() || std::env::var_os("FC3_STORY_AUTOPILOT").is_some();
    // Last frame's shouts over found bodies are heard now.
    let mut noises: Vec<Noise> = noises.read().copied().collect();
    noises.append(&mut shouts);
    let hurt: Vec<(Entity, Option<Vec3>, Option<Entity>)> = damage.read().map(|d| (d.target, d.source, d.instigator)).collect();
    let chest = pt.translation + Vec3::Y * 0.4;
    // Everyone alive, for picking fights between factions.
    let roster: Vec<(Entity, Vec3, Faction)> = npcs.iter().filter(|(_, _, _, h)| !h.dead).map(|(e, t, n, _)| (e, t.translation, n.faction)).collect();
    // Live animals: predators are shot when they come close, any animal once it bites.
    let animals: Vec<(Entity, Vec3, bool)> = beasts.iter().filter(|(_, _, h, _)| !h.dead).map(|(e, t, _, a)| (e, t.translation, a.dangerous())).collect();
    for (e, t, mut npc, h) in &mut npcs {
        if h.dead {
            continue;
        }
        let friendly = !npc.faction.hostile_to_player();
        let eye = t.translation + Vec3::Y * 0.75;
        let to = chest - eye;
        let dist = to.length();
        let facing = Vec3::new(-npc.yaw.sin(), 0.0, -npc.yaw.cos());
        let cos = facing.dot(Vec3::new(to.x, 0.0, to.z).normalize_or_zero());
        let alert = matches!(npc.state, NpcState::Combat | NpcState::Search);
        let (range, fov_cos) = if alert { (110.0, -0.2) } else { (70.0, 0.35) };
        let range = range * npc.role.stats(npc.faction).sight;
        // The game's senses: how far he can notice the player at this posture
        // in his state, and the sixth sense close by.
        let senses = sense_table.for_role(npc.role);
        let level = match npc.state {
            NpcState::Idle => 0,
            NpcState::Suspicious | NpcState::Search => 1,
            NpcState::Combat => 2,
        };
        let sense = senses.map(|s| *s.get(posture, level));
        let (reach, sixth) = match senses {
            Some(s) => (sense.map_or(range, |t| t.max_dist), s.sixth_sense),
            None => (range, (4.0, 0.0)),
        };
        let mut visible = false;
        if !friendly && !ph.dead && !unseen && dist < reach && (cos > fov_cos || dist < sixth.0) {
            if let Ok(dir) = Dir3::new(to) {
                let filter = SpatialQueryFilter::from_excluded_entities([e]);
                visible = match spatial.cast_ray(eye, dir, dist, true, &filter) {
                    Some(hit) => hit.entity == pe || hit.distance > dist - 0.6,
                    None => true,
                };
            }
        }
        // Grace period right after the player (re)spawns.
        if time.elapsed_secs() - player.spawned_at < 5.0 {
            visible = false;
        }
        npc.sees_player = visible;
        // Foes: the nearest hostile NPC in view, checked a few times a second.
        npc.scan -= dt;
        if npc.foe.is_some_and(|f| !roster.iter().any(|(r, _, _)| *r == f) && !animals.iter().any(|(a, _, _)| *a == f)) {
            npc.foe = None;
            npc.foe_seen = false;
        }
        if npc.scan <= 0.0 {
            npc.scan = 0.4;
            let mut seen = None;
            let fought = npc.foe;
            let mut candidates: Vec<(f32, Entity, Vec3)> = roster
                .iter()
                .filter(|(o, _, f)| *o != e && npc.faction.hostile_to(*f))
                .map(|(o, p, _)| (p.distance(t.translation), *o, *p))
                .chain(
                    animals
                        .iter()
                        .map(|(a, p, danger)| (p.distance(t.translation), *a, *p, *danger))
                        .filter(|(d, a, _, danger)| (*danger && *d < 15.0) || (fought == Some(*a) && *d < 40.0))
                        .map(|(d, a, p, _)| (d, a, p)),
                )
                .filter(|(d, _, p)| *d < range && (facing.dot(Vec3::new(p.x - eye.x, 0.0, p.z - eye.z).normalize_or_zero()) > fov_cos || *d < 6.0))
                .collect();
            candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (d, o, p) in candidates.into_iter().take(3) {
                let to_foe = p + Vec3::Y * 0.4 - eye;
                let Ok(dir) = Dir3::new(to_foe) else { continue };
                let filter = SpatialQueryFilter::from_excluded_entities([e]);
                if spatial.cast_ray(eye, dir, d, true, &filter).is_none_or(|hit| hit.entity == o || hit.distance > d - 0.6) {
                    seen = Some((o, p));
                    break;
                }
            }
            npc.foe_seen = seen.is_some();
            if let Some((o, p)) = seen {
                npc.foe = Some(o);
                npc.last_known = Some(p);
                // Soldiers react to an armed foe faster than to a sneaking player.
                npc.awareness = (npc.awareness + if friendly { 1.0 } else { 0.5 }).min(1.0);
            }
        }
        // A body in view: search the area and call nearby guards over.
        if npc.state != NpcState::Combat {
            for (be, bt, mut corpse) in &mut bodies {
                if corpse.found || be == e {
                    continue;
                }
                let to_body = bt.translation + Vec3::Y * 0.3 - eye;
                let d = to_body.length();
                if d > BODY_SIGHT || facing.dot(Vec3::new(to_body.x, 0.0, to_body.z).normalize_or_zero()) < 0.2 {
                    continue;
                }
                let Ok(dir) = Dir3::new(to_body) else { continue };
                let filter = SpatialQueryFilter::from_excluded_entities([e, be]);
                if spatial.cast_ray(eye, dir, d, true, &filter).is_some_and(|h| h.distance < d - 0.8) {
                    continue;
                }
                corpse.found = true;
                npc.state = NpcState::Search;
                npc.state_time = 0.0;
                npc.awareness = npc.awareness.max(0.8);
                npc.last_known = Some(bt.translation);
                shouts.push(Noise { at: bt.translation, radius: 30.0, kind: NoiseKind::Interesting });
                break;
            }
        }
        if visible {
            // Closer, moving and upright targets are noticed faster; grass and
            // bushes round the player slow it down (most for a still,
            // crouched player, and only up close do they give him away).
            let hidden = cover.level * if player.crouching { 1.0 } else { 0.6 } * if player.velocity.length() > 4.0 { 0.5 } else { 1.0 };
            let concealed = 1.0 - 0.85 * hidden * (dist / 8.0).min(1.0);
            let rate = match sense.and_then(|t| t.time(dist)) {
                // The game's time to notice him at this distance (the sixth
                // sense's at most, close by).
                Some(t) => {
                    let t = if dist < sixth.0 && sixth.1 > 0.0 { t.min(sixth.1) } else { t };
                    let crouched = if posture == 0 { perks.crouch_detection } else { 1.0 };
                    concealed * crouched / t.max(0.05) * settings.difficulty.spotting()
                }
                None => {
                    let stance = if player.crouching { 0.45 * perks.crouch_detection } else { 1.0 } * concealed;
                    let motion = 0.6 + (player.velocity.length() / 6.0).min(1.0) * 0.6;
                    // Roughly 2 s to spot an upright player at 40 m, about 1 s point-blank.
                    ((1.0 - dist / range).max(0.05) * 0.9 + 0.1) * stance * motion * if alert { 3.0 } else { 1.0 } * settings.difficulty.spotting()
                }
            };
            let before = npc.awareness;
            npc.awareness = (npc.awareness + rate * dt).min(1.0);
            if npc.awareness > 0.35 {
                npc.last_known = Some(pt.translation);
            }
            // FC3_SENSE_DEBUG: how long each took to notice the player fully.
            if std::env::var_os("FC3_SENSE_DEBUG").is_some() {
                let started = *sensing.entry(e).or_insert(time.elapsed_secs());
                if before < 1.0 && npc.awareness >= 1.0 {
                    info!("sense: noticed at {dist:.0} m after {:.2} s (posture {posture}, level {level})", time.elapsed_secs() - started);
                }
            }
        } else {
            sensing.remove(&e);
            npc.awareness = (npc.awareness - dt * if alert { 0.05 } else { 0.12 }).max(0.0);
        }
        // Steps heard out of sight: a sprint carries farthest, a crouch
        // hardly at all (`SP_SilentRun` quietens them).
        if !friendly && !visible && !ph.dead && !unseen && !player.driving && player.grounded && time.elapsed_secs() - player.spawned_at >= 5.0 {
            let speed = Vec2::new(player.velocity.x, player.velocity.z).length();
            let heard = footstep_range(speed, player.crouching, player.sprinting) * perks.footstep_noise;
            if dist < heard {
                npc.awareness = (npc.awareness + (1.0 - dist / heard) * 0.9 * dt).min(1.0);
                if npc.awareness > 0.35 {
                    npc.last_known = Some(pt.translation);
                }
            }
        }
        for n in noises.iter().filter(|_| !friendly) {
            if t.translation.distance(n.at) < n.radius {
                npc.awareness = npc.awareness.max(0.6);
                npc.last_known = Some(n.at);
            }
        }
        for (target, src, by) in &hurt {
            if *target == e && by.is_some_and(|b| animals.iter().any(|(a, _, _)| *a == b)) {
                // Bitten: turn on the animal.
                npc.foe = *by;
                npc.foe_seen = true;
                npc.awareness = 1.0;
                npc.scan = 0.4;
                continue;
            }
            if *target == e {
                npc.awareness = 1.0;
                npc.last_known = Some(src.unwrap_or(if friendly { t.translation } else { pt.translation }));
            }
        }
    }
}

fn think(
    time: Res<Time>,
    mut npcs: Query<(&mut Npc, &Health, &Transform), (Without<Corpse>, Without<Frozen>, Without<Riding>, Without<crate::turrets::Gunner>, Without<Scripted>)>,
) {
    let dt = time.delta_secs();
    for (mut npc, h, t) in &mut npcs {
        if h.dead {
            continue;
        }
        npc.state_time += dt;
        if npc.state == NpcState::Combat {
            npc.engaged += dt;
        } else {
            npc.engaged = 0.0;
        }
        let next = match npc.state {
            // An armed foe in plain sight: fight at once.
            NpcState::Idle | NpcState::Suspicious | NpcState::Search if npc.foe_seen && npc.awareness > 0.9 => Some(NpcState::Combat),
            NpcState::Idle if npc.awareness >= 1.0 => Some(NpcState::Combat),
            NpcState::Idle if npc.awareness > 0.35 && npc.last_known.is_some() => Some(NpcState::Suspicious),
            NpcState::Suspicious if npc.awareness >= 1.0 => Some(NpcState::Combat),
            // Investigate: walk to the noise, look around there, then go back.
            NpcState::Suspicious
                if npc.awareness <= 0.05
                    && npc.state_time > 6.0
                    && (!npc.last_known.is_some_and(|lk| flat_distance(lk, t.translation) > 2.5) || npc.state_time > 25.0) =>
            {
                Some(NpcState::Idle)
            }
            NpcState::Combat if !npc.sees_player && !npc.foe_seen && npc.state_time > 1.5 => Some(NpcState::Search),
            NpcState::Search if (npc.sees_player || npc.foe_seen) && npc.awareness > 0.5 => Some(NpcState::Combat),
            NpcState::Search if npc.state_time > 25.0 => Some(NpcState::Idle),
            _ => None,
        };
        if npc.state == NpcState::Combat && (npc.sees_player || npc.foe_seen) {
            npc.state_time = 0.0;
        }
        if let Some(s) = next {
            npc.state = s;
            npc.state_time = 0.0;
            if s == NpcState::Idle {
                npc.last_known = None;
                npc.awareness = 0.0;
                npc.foe = None;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn act(
    time: Res<Time>,
    mut rng: ResMut<Rng>,
    spatial: SpatialQuery,
    heights: Res<Heightfield>,
    fx: Option<Res<EffectAssets>>,
    mut commands: Commands,
    mut noises: MessageWriter<Noise>,
    mut damage: MessageWriter<DamageEvent>,
    (mut loops, mut sfx, aim_table, settings): (
        MessageWriter<crate::sound::LoopSfx>,
        MessageWriter<crate::sound::PlaySfx>,
        Res<AimTable>,
        Res<crate::settings::Settings>,
    ),
    (mut alarms, mut throw_clock, nav, probe, collider_names): (
        MessageWriter<crate::outposts::AlarmRaised>,
        Local<f32>,
        Res<crate::nav::NavMesh>,
        crate::surfaces::SurfaceProbe,
        Query<&Name>,
    ),
    players: Query<(Entity, &Transform, &Player), Without<Npc>>,
    mut npcs: Query<
        (Entity, &mut Transform, &mut Npc, &Health, &Children, Option<&crate::outposts::Garrison>),
        (Without<Corpse>, Without<Frozen>, Without<Riding>, Without<crate::turrets::Gunner>, Without<Scripted>),
    >,
    mut limbs: Query<(&NpcLimb, &mut Transform), (Without<Npc>, Without<Player>)>,
    mut anims: Query<&mut AnimRequest>,
    mut shot_fx: crate::particles::ShotFx,
    beasts: Query<(Entity, &Transform), (With<crate::wildlife::WildAnimal>, Without<Npc>, Without<NpcLimb>)>,
) {
    let Some(fx) = fx else { return };
    // Those shooting at the player now (the focus counts them).
    let shooting_at_player =
        npcs.iter().filter(|(_, _, n, h, _, _)| !h.dead && n.state == NpcState::Combat && n.sees_player && n.since_fired < 1.0).count() as u32;
    let dt = time.delta_secs();
    let trace_nav = std::env::var_os("FC3_NAV_TRACE").is_some();
    let log_nav = trace_nav && (time.elapsed_secs() * 2.0) as u32 != ((time.elapsed_secs() - dt) * 2.0) as u32;
    // Path searches allowed this frame (the rest wait their turn).
    let mut plan_budget = 3;
    let Ok((pe, pt, player)) = players.single() else { return };
    *throw_clock -= dt;
    let mut others: std::collections::HashMap<Entity, (Vec3, Vec3)> = npcs.iter().map(|(e, t, n, _, _, _)| (e, (t.translation, n.velocity))).collect();
    others.extend(beasts.iter().map(|(e, t)| (e, (t.translation, Vec3::ZERO))));
    for (e, mut t, mut npc, h, children, garrison) in &mut npcs {
        if h.dead {
            continue;
        }
        let pos = t.translation;
        // Its fire on the player by the game's damage lap, where the role has one.
        let lap = aim_table.0.as_ref().and_then(|a| {
            let (kind, weapon) = npc.role.aim(npc.faction)?;
            Some(Lap {
                prefs: a.prefs(kind)?,
                max_damage: a.weapon(weapon)?,
                level: settings.difficulty.index(),
                agents: shooting_at_player,
                decay: (a.decay_start, a.decay_rate),
            })
        });
        // Desired movement and facing for this frame.
        let (mut goal, mut speed, mut face) = (None, 0.0, None);
        // A guard running for the alarm ignores everything else.
        let running_to = npc.alarm_target.filter(|_| npc.state != NpcState::Idle);
        if let Some(target) = running_to {
            if Vec2::new(target.x - pos.x, target.z - pos.z).length() < 1.8 {
                npc.alarm_target = None;
                if let Some(g) = garrison {
                    alarms.write(crate::outposts::AlarmRaised { outpost: g.0 });
                }
            } else {
                goal = Some(target);
                speed = 4.2;
            }
        }
        match npc.state {
            _ if running_to.is_some() => {}
            NpcState::Idle if npc.patrols => {
                // Walk between spots around the post, pausing at each.
                match npc.wander {
                    _ if npc.wander_wait > 0.0 => {
                        npc.wander_wait -= dt;
                        face = Some(npc.yaw + (npc.state_time * 0.5).sin() * dt * 0.6);
                    }
                    None => {
                        let a = rng.range(0.0, std::f32::consts::TAU);
                        let spot = npc.post + Vec3::new(a.cos(), 0.0, a.sin()) * rng.range(4.0, 14.0);
                        let dry = heights.height_at(spot).is_some_and(|g| g > crate::water::surface(spot, Some(g)) + 0.3);
                        npc.wander = if dry { Some(spot) } else { Some(npc.post) };
                    }
                    Some(w) if Vec2::new(w.x - pos.x, w.z - pos.z).length() < 1.2 => {
                        npc.wander = None;
                        npc.wander_wait = rng.range(4.0, 10.0);
                    }
                    Some(w) => {
                        goal = Some(w);
                        speed = 1.3;
                        // Give up on spots that can't be reached.
                        if rng.f32() < dt * 0.05 {
                            npc.wander = None;
                        }
                    }
                }
            }
            NpcState::Idle => {
                // Slowly scan around the post (at a guard spot, face its way).
                face = Some(if npc.at_spot { npc.post_yaw } else { npc.post_yaw + (npc.state_time * 0.35).sin() * 0.9 });
                if flat_distance(pos, npc.post) > 1.5 {
                    goal = Some(npc.post);
                    speed = 1.4;
                }
            }
            NpcState::Suspicious => {
                if let Some(lk) = npc.last_known {
                    if Vec2::new(lk.x - pos.x, lk.z - pos.z).length() > 2.0 {
                        goal = Some(lk);
                        speed = 1.6;
                    } else {
                        // At the spot: look around.
                        face = Some(npc.yaw + dt * 0.8);
                    }
                }
            }
            NpcState::Combat => {
                let stats = npc.role.stats(npc.faction);
                // The player when in sight, else a foe, else where either was last seen.
                let foe = npc.foe.and_then(|f| others.get(&f).map(|(p, v)| (f, *p, *v)));
                let tgt = match foe {
                    _ if npc.sees_player => {
                        Target { entity: pe, pos: pt.translation, vel: player.velocity, crouching: player.crouching, seen: true, is_player: true }
                    }
                    Some((f, p, v)) if npc.foe_seen || !npc.faction.hostile_to_player() => {
                        Target { entity: f, pos: p, vel: v, crouching: false, seen: npc.foe_seen, is_player: false }
                    }
                    _ if npc.faction.hostile_to_player() => {
                        Target { entity: pe, pos: npc.last_known.unwrap_or(pt.translation), vel: Vec3::ZERO, crouching: false, seen: false, is_player: true }
                    }
                    _ => Target { entity: e, pos: npc.last_known.unwrap_or(pos), vel: Vec3::ZERO, crouching: false, seen: false, is_player: false },
                };
                let to = tgt.pos - pos;
                face = Some(yaw_to(to));
                let dist = to.length();
                // Riflemen, snipers and rocket and firebomb throwers take cover
                // when they can; shotgunners, heavies and flamers keep coming.
                let takes_cover = !matches!(npc.role, Role::Shotgunner | Role::Heavy | Role::Flamer) && tgt.seen;
                npc.cover_search -= dt;
                if takes_cover && npc.cover.is_none() && npc.cover_search <= 0.0 && dist > 9.0 && nav.ready() {
                    npc.cover_search = rng.range(2.5, 4.5);
                    npc.cover = find_cover(&nav, &spatial, e, pe, pos, tgt.pos).map(|spot| Cover {
                        spot,
                        settled: false,
                        up: false,
                        timer: 0.0,
                        stay: rng.range(12.0, 25.0),
                    });
                }
                if let Some(mut c) = npc.cover {
                    c.stay -= dt;
                    // Rushed, or there too long: move on.
                    let leave = dist < 7.0 || c.stay <= 0.0 || (!c.settled && flat_distance(pos, c.spot) > 18.0);
                    if leave {
                        npc.cover = None;
                        npc.cover_search = rng.range(3.0, 6.0);
                    } else if !c.settled {
                        goal = Some(c.spot + Vec3::Y * HALF_HEIGHT);
                        speed = 4.2;
                        if flat_distance(pos, c.spot) < 0.7 {
                            c.settled = true;
                            c.up = false;
                            c.timer = rng.range(0.8, 1.6);
                        }
                        npc.cover = Some(c);
                    } else {
                        c.timer -= dt;
                        if c.timer <= 0.0 {
                            c.up = !c.up;
                            c.timer = if c.up { rng.range(1.6, 3.0) } else { rng.range(1.2, 2.6) };
                        }
                        // Reload while down.
                        if !c.up && npc.mag < npc.role.stats(npc.faction).mag / 2 && npc.reload <= 0.0 {
                            npc.reload = 2.2;
                        }
                        npc.cover = Some(c);
                        if c.up || npc.reload > 0.0 {
                            if tgt.is_player && c.up {
                                throw(&mut npc, e, pos, tgt.pos, dt, &mut rng, &fx, &mut commands, &mut throw_clock);
                            }
                            shoot(
                                &mut npc,
                                e,
                                pos,
                                &tgt,
                                pt.translation,
                                dt,
                                &mut rng,
                                &spatial,
                                &fx,
                                &mut shot_fx,
                                &mut commands,
                                &mut damage,
                                &mut noises,
                                &mut loops,
                                &mut sfx,
                                lap.as_ref(),
                            );
                        }
                    }
                }
                if npc.cover.is_some() {
                    // (Handled above.)
                } else {
                    // Keep the role's fighting distance, strafing to be harder to hit.
                    let fwd = Vec3::new(to.x, 0.0, to.z).normalize_or_zero();
                    let side = Vec3::new(-fwd.z, 0.0, fwd.x) * npc.strafe_dir;
                    let push = if dist > stats.far {
                        fwd
                    } else if dist < stats.near {
                        -fwd
                    } else {
                        Vec3::ZERO
                    };
                    // Snipers stand still to aim unless the player gets close.
                    let strafe = if npc.role == Role::Sniper && push == Vec3::ZERO { 0.0 } else { stats.strafe };
                    if push != Vec3::ZERO || strafe > 0.0 {
                        goal = Some(pos + (push + side * strafe) * 3.0);
                    }
                    // Shotgunners sprint in.
                    speed = if push == fwd && npc.role == Role::Shotgunner { 4.4 } else { 2.6 * stats.pace };
                    if rng.f32() < dt * 0.4 {
                        npc.strafe_dir = -npc.strafe_dir;
                    }
                    if tgt.is_player {
                        throw(&mut npc, e, pos, tgt.pos, dt, &mut rng, &fx, &mut commands, &mut throw_clock);
                    }
                    shoot(
                        &mut npc,
                        e,
                        pos,
                        &tgt,
                        pt.translation,
                        dt,
                        &mut rng,
                        &spatial,
                        &fx,
                        &mut shot_fx,
                        &mut commands,
                        &mut damage,
                        &mut noises,
                        &mut loops,
                        &mut sfx,
                        lap.as_ref(),
                    );
                }
            }
            NpcState::Search => {
                if let Some(lk) = npc.last_known {
                    // Flush the player out of cover with a grenade.
                    if npc.state_time < 8.0 && npc.faction.hostile_to_player() {
                        throw(&mut npc, e, pos, lk, dt, &mut rng, &fx, &mut commands, &mut throw_clock);
                    }
                    goal = Some(lk);
                    speed = 2.8;
                    if flat_distance(pos, lk) < 2.0 {
                        npc.last_known = Some(lk + Vec3::new(rng.range(-12.0, 12.0), 0.0, rng.range(-12.0, 12.0)));
                    }
                }
            }
        }
        // The way round buildings and fences, over the navigation mesh.
        let feet = pos - Vec3::Y * HALF_HEIGHT;
        let goal = goal.map(|g| {
            // Only short combat strafes bypass the mesh. A nearby noise or
            // guard post can be on the other side of a fence or a doorway.
            if !nav.ready() || (npc.state == NpcState::Combat && flat_distance(pos, g) < 4.0) {
                npc.path.clear();
                return g;
            }
            npc.path_timer -= dt;
            let stale = npc.path_goal.is_none_or(|pg| flat_distance(pg, g) > 2.0) || npc.path_timer <= 0.0;
            if stale && plan_budget > 0 {
                plan_budget -= 1;
                npc.path = nav.path(feet, g).unwrap_or_default();
                debug!("npc {e:?}: path of {} waypoints to {g:.1?}", npc.path.len());
                npc.path_goal = Some(g);
                npc.path_timer = 1.5 + rng.f32();
            }
            while npc.path.len() > 1 && flat_distance(npc.path[0], pos) < 0.8 {
                npc.path.remove(0);
            }
            npc.path.first().map(|w| Vec3::new(w.x, g.y, w.z)).unwrap_or(g)
        });
        // Steering with simple obstacle avoidance.
        let mut vel = Vec3::ZERO;
        if let Some(g) = goal {
            let mut dir = Vec3::new(g.x - pos.x, 0.0, g.z - pos.z);
            if dir.length() > 0.4 {
                dir = dir.normalize();
                let filter = SpatialQueryFilter::from_excluded_entities([e, pe]);
                if let Ok(d) = Dir3::new(dir) {
                    if let Some(hit) = spatial.cast_ray(pos, d, 1.2, true, &filter) {
                        if log_nav {
                            debug!("npc {}: blocked at {pos:.3?}, toward {g:.3?}, by {:?} ({:?}) at {:.3} m, normal {:.3?}",
                                npc.marker_id, hit.entity, collider_names.get(hit.entity).ok().map(|n| n.as_str()), hit.distance, hit.normal);
                        }
                        // Blocked: try sidestepping.
                        let side = Vec3::new(-dir.z, 0.0, dir.x) * npc.strafe_dir;
                        dir = (dir * 0.2 + side).normalize();
                    }
                }
                vel = dir * speed;
                if face.is_none() {
                    face = Some(yaw_to(dir));
                }
            }
        }
        npc.velocity = npc.velocity.lerp(vel, (dt * 6.0).min(1.0));
        // Pinned against something: give up on this goal.
        let moving = Vec2::new(npc.velocity.x, npc.velocity.z).length();
        if goal.is_some() && speed > 0.0 && moving < speed * 0.3 {
            npc.stuck += dt;
        } else {
            npc.stuck = 0.0;
        }
        if npc.stuck > 2.0 {
            npc.stuck = 0.0;
            npc.strafe_dir = -npc.strafe_dir;
            match npc.state {
                NpcState::Search => npc.last_known = npc.last_known.map(|lk| lk + Vec3::new(rng.range(-10.0, 10.0), 0.0, rng.range(-10.0, 10.0))),
                NpcState::Suspicious => npc.last_known = Some(pos),
                NpcState::Idle => {
                    npc.wander = None;
                    npc.wander_wait = 2.0;
                }
                NpcState::Combat => {}
            }
        }
        // Footsteps for nearby moving enemies (a cue to their position).
        let step_speed = Vec2::new(npc.velocity.x, npc.velocity.z).length();
        if step_speed > 0.5 && pos.distance(pt.translation) < 30.0 {
            npc.step_timer -= dt * step_speed;
            if npc.step_timer <= 0.0 {
                npc.step_timer = 1.4;
                let feet = pos - Vec3::Y * HALF_HEIGHT;
                let surface = if feet.y < probe.water(feet) - 0.05 {
                    crate::surfaces::value::SHALLOW_WATER
                } else {
                    let filter = SpatialQueryFilter::from_excluded_entities([e]);
                    match spatial.cast_ray(feet + Vec3::Y * 0.3, Dir3::NEG_Y, 1.0, true, &filter) {
                        Some(hit) => probe.at(hit.entity, feet + Vec3::Y * (0.3 - hit.distance), hit.normal),
                        None => probe.terrain(feet),
                    }
                };
                let (event, gait) = if step_speed > 3.5 { (crate::sound::ev::NPC_RUN, 3) } else { (crate::sound::ev::NPC_STEP, 1) };
                sfx.write(crate::sound::PlaySfx { event: crate::sound::switched(event, surface, gait), at: Some(feet), volume: 0.5 });
            }
        }
        let mut next = pos + npc.velocity * dt;
        // Stay on the ground and out of the sea, or on a floor the navigation
        // mesh has above the ground (platforms, upper storeys) or well below it
        // (caves).
        if let Some(ground) = heights.height_at(next) {
            let floor = nav.height_at(next - Vec3::Y * HALF_HEIGHT).filter(|h| (*h > ground + 0.5 || *h < ground - 2.0) && (*h - feet.y).abs() < 1.0);
            if let Some(f) = floor {
                next.y = f + HALF_HEIGHT;
            } else if ground < crate::water::surface(next, Some(ground)) - 0.8 {
                next = pos;
                npc.velocity = Vec3::ZERO;
            } else {
                next.y = ground + HALF_HEIGHT;
            }
        }
        t.translation = next;
        if log_nav && goal.is_some() {
            debug!("npc {}: {:?} at {next:.3?}, target {:?}, {} waypoints, next {goal:.3?}, velocity {:.3?}",
                npc.marker_id, npc.state, npc.last_known, npc.path.len(), npc.velocity);
        }
        if let Some(target_yaw) = face {
            npc.yaw = turn_toward(npc.yaw, target_yaw, dt * 4.0);
        }
        t.rotation = Quat::from_rotation_y(npc.yaw);
        npc.since_fired += dt;
        if let Some(mut req) = npc.rig.and_then(|r| anims.get_mut(r).ok()) {
            let speed = Vec2::new(npc.velocity.x, npc.velocity.z).length();
            npc.flinch = (npc.flinch - dt).max(0.0);
            *req = if npc.flinch > 0.0 {
                AnimRequest { kind: AnimKind::Hit, rate: 1.3 }
            } else if npc.reload > 0.0 {
                AnimRequest { kind: AnimKind::Reload, rate: 1.0 }
            } else if speed > 2.6 {
                AnimRequest { kind: AnimKind::Run, rate: (speed / 4.9).clamp(0.6, 1.4) }
            } else if speed > 0.35 {
                AnimRequest { kind: AnimKind::Walk, rate: (speed / 1.5).clamp(0.6, 1.6) }
            } else if npc.hiding() && npc.state == NpcState::Combat {
                AnimRequest { kind: AnimKind::Cover, rate: 1.0 }
            } else if npc.state == NpcState::Combat && npc.since_fired < 0.5 {
                AnimRequest { kind: AnimKind::Fire, rate: 1.0 }
            } else if npc.state == NpcState::Combat {
                AnimRequest { kind: AnimKind::Aim, rate: 1.0 }
            } else {
                AnimRequest { kind: AnimKind::Idle, rate: 1.0 }
            };
        }
        animate_limbs(children, &mut limbs, npc.velocity.length(), time.elapsed_secs(), npc.state == NpcState::Combat);
    }
}

/// Who an enemy (or rebel) is shooting at this frame.
struct Target {
    entity: Entity,
    /// Capsule centre.
    pos: Vec3,
    vel: Vec3,
    crouching: bool,
    /// In sight right now.
    seen: bool,
    is_player: bool,
}

/// Out of sight this long (s), an enemy's damage lap starts over.
const LAP_LOST_AFTER: f32 = 2.0;
/// A missing shot passes at least this far from the target's middle (m;
/// beyond the body), plus the miss distance.
const LAP_MISS_CLEARANCE: f32 = 0.45;

/// Launch velocity for a lob from `from` landing at `to`, trying flatter arcs first.
fn lob(from: Vec3, to: Vec3, gravity: f32) -> Vec3 {
    let delta = to - from;
    let flat = Vec2::new(delta.x, delta.z);
    let d = flat.length().max(0.5);
    let along = flat / d;
    for angle in [0.7f32, 0.95, 1.15] {
        let (sin, cos) = angle.sin_cos();
        let denom = 2.0 * cos * cos * (d * sin / cos - delta.y);
        if denom > 0.01 {
            let v = (gravity * d * d / denom).sqrt();
            return Vec3::new(along.x * cos * v, sin * v, along.y * cos * v);
        }
    }
    Vec3::new(along.x * 6.0, 9.0, along.y * 6.0)
}

/// Thrown weapons: firebomb throwers lob at the player whenever they can;
/// assault troops use grenades to flush out a player who has been hiding for
/// a few seconds. `clock` spaces throws across the whole squad.
#[allow(clippy::too_many_arguments)]
fn throw(npc: &mut Npc, me: Entity, pos: Vec3, target: Vec3, dt: f32, rng: &mut Rng, fx: &EffectAssets, commands: &mut Commands, clock: &mut f32) {
    npc.throw_cooldown -= dt;
    let molotov = npc.role == Role::Molotov;
    if npc.throw_cooldown > 0.0 || *clock > 0.0 || npc.reload > 0.0 || !(molotov || npc.role == Role::Assault) {
        return;
    }
    if !molotov && (npc.sees_player || npc.state != NpcState::Search || npc.state_time < 2.5) {
        return;
    }
    let flat = Vec2::new(target.x - pos.x, target.z - pos.z).length();
    if !(7.0..=28.0).contains(&flat) {
        return;
    }
    npc.throw_cooldown = if molotov { rng.range(7.0, 11.0) } else { rng.range(20.0, 32.0) };
    *clock = if molotov { 3.0 } else { 8.0 };
    let hand = pos + Vec3::Y * 0.7;
    let scatter = Vec3::new(rng.range(-1.0, 1.0), 0.0, rng.range(-1.0, 1.0)) * (flat / 10.0);
    let aim = target - Vec3::Y * (HALF_HEIGHT - 0.1) + scatter;
    commands.spawn((
        crate::weapons::Projectile {
            incendiary: molotov,
            arrow: false,
            velocity: lob(hand, aim, 9.81),
            gravity: 9.81,
            fuse: if molotov { None } else { Some(2.8) },
            explode_on_impact: false,
            radius: if molotov { 4.0 } else { 7.0 },
            damage: if molotov { 0.0 } else { 80.0 },
            owner: me,
            age: 0.0,
            travelled: 0.0,
        },
        Transform::from_translation(hand),
        Visibility::Visible,
        crate::weapons::ProjectileModel(if molotov { fx.molotov.clone() } else { fx.grenade.clone() }),
    ));
}

#[allow(clippy::too_many_arguments)]
/// Bullets passing within this distance (m) of the listener whizz by.
const PASS_BY_RANGE: f32 = 2.2;

fn shoot(
    npc: &mut Npc,
    me: Entity,
    pos: Vec3,
    tgt: &Target,
    viewer: Vec3,
    dt: f32,
    rng: &mut Rng,
    spatial: &SpatialQuery,
    fx: &EffectAssets,
    shot_fx: &mut crate::particles::ShotFx,
    commands: &mut Commands,
    damage: &mut MessageWriter<DamageEvent>,
    noises: &mut MessageWriter<Noise>,
    loops: &mut MessageWriter<crate::sound::LoopSfx>,
    sfx: &mut MessageWriter<crate::sound::PlaySfx>,
    lap: Option<&Lap>,
) {
    use crate::sound::ev;
    let stats = npc.role.stats(npc.faction);
    let forward = Vec3::new(-npc.yaw.sin(), 0.0, -npc.yaw.cos());
    let muzzle = pos + Vec3::Y * 0.35 + forward * 0.7;
    let dist_to_player = tgt.pos.distance(muzzle);
    // The damage lap on the player: while he stays in sight and in range, the
    // focus (distance, time on him, his movement and exposure, how many are
    // shooting) fills the progress at its lap, after the least time first.
    let lap = lap.filter(|_| tgt.is_player);
    if let Some(l) = lap {
        let st = &mut npc.lap;
        if tgt.seen && dist_to_player <= l.prefs.firing_range {
            st.unseen = 0.0;
            st.lost_at = tgt.pos;
            st.chain += dt;
            let exposure = if st.exposure > 0.0 { st.exposure } else { l.prefs.exposure[0] };
            let movement = l.prefs.movement(tgt.vel.to_array(), (pos - tgt.pos).to_array(), dist_to_player);
            st.focus = l.prefs.focus(dist_to_player, st.chain, movement, exposure, l.agents);
            if !st.shooting {
                st.shooting = true;
                st.progress = 0.0;
                st.buffer = l.prefs.buffer[l.level].at(st.focus);
            }
            if st.buffer > 0.0 {
                st.buffer -= dt;
            } else {
                st.progress = (st.progress + dt / l.prefs.lap[l.level].at(st.focus).max(0.05)).min(1.0);
            }
        } else {
            // Out of sight: the progress falls back after a moment, and the
            // chaining starts over once he has moved off (or after a while).
            st.unseen += dt;
            if st.unseen > l.decay.0 {
                st.progress = (st.progress - l.decay.1 / 100.0 * dt).max(0.0);
            }
            let moved = tgt.pos.distance(st.lost_at) > l.prefs.chain_reset_move;
            if moved || (l.prefs.chain_reset_time >= 0.0 && st.unseen > l.prefs.chain_reset_time) {
                st.chain = 0.0;
            }
            if st.unseen > LAP_LOST_AFTER && st.progress <= 0.0 {
                st.shooting = false;
            }
        }
    }
    // A sniper's scope glints while it is trained on the player.
    if npc.role == Role::Sniper && tgt.seen && tgt.is_player && npc.reload <= 0.0 && dist_to_player > 20.0 {
        let mut g =
            crate::particles::Particle::new(crate::particles::Sheet::Glow, pos + Vec3::Y * 0.62 + forward * 0.25, 1.0, (dist_to_player * 0.018).max(0.35));
        g.colour = Vec4::new(1.0, 0.97, 0.9, 1.0 + 0.2 * (npc.state_time * 9.0).sin());
        shot_fx.particles.sprite(g);
    }
    npc.fire_cooldown -= dt;
    if npc.reload > 0.0 {
        npc.reload -= dt;
        if npc.reload <= 0.0 {
            npc.mag = stats.mag;
        }
        return;
    }
    if !tgt.seen || npc.fire_cooldown > 0.0 || dist_to_player > stats.reach {
        return;
    }
    if npc.burst_left == 0 {
        let (lo, hi) = stats.burst;
        npc.burst_left = lo + (rng.f32() * (hi - lo + 1) as f32) as u32;
        npc.fire_cooldown = rng.range(stats.pause.0, stats.pause.1);
        return;
    }
    npc.burst_left -= 1;
    npc.fire_cooldown = stats.interval;
    npc.since_fired = 0.0;
    npc.mag = npc.mag.saturating_sub(1);
    if npc.mag == 0 {
        npc.reload = stats.reload;
        npc.burst_left = 0;
    }
    // Miss chance grows with distance and target movement.
    let moving = (tgt.vel.length() / 7.0).min(1.0);
    // The opening shots after spotting the player are deliberately loose.
    let warmup = (npc.engaged / 2.5).clamp(0.25, 1.0);
    let crouch = if tgt.crouching { 0.8 } else { 1.0 };
    let hit_chance = (npc.accuracy * warmup * (1.0 - dist_to_player / stats.reach) * (1.0 - 0.45 * moving) * crouch).clamp(0.03, 0.8);
    match npc.role {
        Role::Rocket => {
            // Rockets go for the body, or the ground at the player's feet.
            let target = if rng.f32() < hit_chance {
                tgt.pos + tgt.vel * (dist_to_player / 45.0) * 0.6
            } else {
                tgt.pos - Vec3::Y * HALF_HEIGHT + Vec3::new(rng.range(-3.0, 3.0), 0.0, rng.range(-3.0, 3.0)) * (1.0 + dist_to_player / 50.0)
            };
            let dir = (target - muzzle).normalize_or(forward);
            commands.spawn((
                crate::weapons::Projectile {
                    incendiary: false,
                    arrow: false,
                    velocity: dir * 45.0,
                    gravity: 1.5,
                    fuse: None,
                    explode_on_impact: true,
                    radius: 5.0,
                    damage: stats.damage,
                    owner: me,
                    age: 0.0,
                    travelled: 0.0,
                },
                Transform::from_translation(muzzle + Vec3::Y * 0.25).looking_to(dir, Vec3::Y),
                Visibility::Visible,
                crate::weapons::ProjectileModel(fx.rocket.clone()),
            ));
            crate::particles::fx::muzzle_flash(&mut shot_fx.particles, rng, muzzle, dir, 1.8);
            for _ in 0..4 {
                crate::particles::fx::barrel_smoke(&mut shot_fx.particles, rng, muzzle);
            }
            sfx.write(crate::sound::PlaySfx { event: ev::RPG, at: Some(muzzle), volume: 1.0 });
            noises.write(Noise { at: muzzle, radius: 120.0, kind: NoiseKind::Threatening });
            return;
        }
        Role::Flamer => {
            // A stream of flame: burns the player if it reaches, lights the ground where it lands.
            let target = tgt.pos + Vec3::Y * rng.range(-0.6, 0.2);
            let dir = (target - muzzle).normalize_or(forward);
            let Ok(d3) = Dir3::new(dir) else { return };
            let filter = SpatialQueryFilter::from_excluded_entities([me]);
            let hit = spatial.cast_ray(muzzle, d3, stats.reach, true, &filter);
            crate::particles::fx::flame_stream(
                &mut shot_fx.particles,
                rng,
                muzzle,
                dir,
                npc.velocity,
                hit.map(|h| if h.entity == tgt.entity { (h.distance - 1.2).max(0.5) } else { h.distance }).unwrap_or(stats.reach),
            );
            if let Some(h) = hit {
                if h.entity == tgt.entity {
                    damage.write(DamageEvent {
                        target: tgt.entity,
                        amount: stats.damage,
                        kind: DamageKind::Fire,
                        source: Some(muzzle),
                        instigator: Some(me),
                        headshot: false,
                        hit: crate::combat::Hit::Whole,
                    });
                }
                if rng.f32() < 0.12 {
                    shot_fx.ignite.write(crate::fire::Ignite { at: muzzle + dir * h.distance, radius: 1.5, instigator: Some(me) });
                }
            }
            noises.write(Noise { at: muzzle, radius: 40.0, kind: NoiseKind::Threatening });
            return;
        }
        _ => {}
    }
    let filter = SpatialQueryFilter::from_excluded_entities([me]);
    // By the damage lap: this shot lands (for the weapon's most damage times
    // the focus's power) once the progress is full; until then shots miss,
    // nearer as the next hit nears. How much of him shows (head, chest,
    // knees) is his exposure for the next.
    let lap_shot: Option<Option<f32>> = lap.map(|l| {
        let shown = [0.75f32, 0.2, -0.5]
            .iter()
            .filter(|h| {
                let to = tgt.pos + Vec3::Y * **h - muzzle;
                Dir3::new(to).is_ok_and(|d| {
                    spatial.cast_ray(muzzle, d, to.length(), true, &filter).is_none_or(|hit| hit.entity == tgt.entity || hit.distance > to.length() - 0.4)
                })
            })
            .count();
        let st = &mut npc.lap;
        st.exposure = match shown {
            3 => l.prefs.exposure[0],
            2 => l.prefs.exposure[1],
            _ => l.prefs.exposure[2],
        };
        if st.progress >= 1.0 {
            st.progress = 0.0;
            Some(l.max_damage * l.prefs.power[l.level].at(st.focus))
        } else {
            None
        }
    });
    let miss_by = lap.map(|l| {
        let st = npc.lap;
        let next = (1.0 - st.progress) * l.prefs.lap[l.level].at(st.focus) + st.buffer.max(0.0);
        l.prefs.miss.at(next)
    });
    let mut target_damage = 0.0;
    let mut stray: Vec<(Entity, f32, Vec3)> = Vec::new();
    let mut last_dir = forward;
    let mut whizzed = false;
    for i in 0..stats.pellets.max(1) {
        let target = tgt.pos + Vec3::Y * rng.range(-0.4, 0.6);
        // Pellets spread: each rolls its own hit.
        let aim = match (lap_shot, miss_by) {
            (Some(Some(_)), _) => tgt.pos + Vec3::Y * rng.range(-0.2, 0.5),
            (Some(None), Some(miss)) => {
                // Past him, by the miss distance beyond his body.
                let along = (target - muzzle).normalize_or(forward);
                let side = along.cross(Vec3::Y).normalize_or(Vec3::X);
                let up = side.cross(along);
                let a = rng.f32() * std::f32::consts::TAU;
                target + (side * a.cos() + up * a.sin()) * (LAP_MISS_CLEARANCE + miss)
            }
            _ if rng.f32() < hit_chance => target,
            _ => target + Vec3::new(rng.range(-1.5, 1.5), rng.range(-0.6, 1.4), rng.range(-1.5, 1.5)) * (1.0 + dist_to_player / 40.0),
        };
        let dir = (aim - muzzle).normalize_or_zero();
        let Ok(d3) = Dir3::new(dir) else { continue };
        last_dir = dir;
        let range = stats.reach + 30.0;
        let hit = spatial.cast_ray(muzzle, d3, range, true, &filter);
        let end = hit.map(|h| muzzle + dir * h.distance).unwrap_or(muzzle + dir * range);
        // A bullet that flies past the listener (rather than into the player) whizzes.
        let into_player = tgt.is_player && hit.is_some_and(|h| h.entity == tgt.entity);
        if !whizzed && !into_player {
            let seg = end - muzzle;
            let t = ((viewer - muzzle).dot(seg) / seg.length_squared().max(1e-6)).clamp(0.0, 1.0);
            let near = muzzle + seg * t;
            if t > 0.05 && t < 0.98 && near.distance(viewer) < PASS_BY_RANGE {
                whizzed = true;
                let event = if viewer.y < crate::water::plane_level(viewer) { ev::PASS_BY_UNDERWATER } else { ev::PASS_BY };
                sfx.write(crate::sound::PlaySfx { event, at: Some(near), volume: 0.8 });
            }
        }
        match hit {
            Some(h) if h.entity == tgt.entity => target_damage += npc.damage,
            // Something else in the way (a vehicle, the player's included).
            Some(h) if end.distance(viewer) < 400.0 => {
                stray.push((h.entity, npc.damage, muzzle));
                if end.distance(viewer) < 40.0 && (i == 0 || rng.f32() < 0.4) {
                    shot_fx.bullet_end(rng, muzzle, dir, range, Some((h.entity, h.distance, h.normal)));
                }
            }
            // Misses kick up dust and splashes (only worth drawing near the player).
            _ if end.distance(viewer) < 40.0 && (i == 0 || rng.f32() < 0.4) => {
                shot_fx.bullet_end(rng, muzzle, dir, range, hit.map(|h| (h.entity, h.distance, h.normal)));
            }
            _ => {}
        }
        // Tracer from the shared effect assets.
        let len = muzzle.distance(end);
        if len > 1.0 && (i == 0 || rng.f32() < 0.3) {
            commands.spawn((
                Mesh3d(fx.tracer_mesh.clone()),
                MeshMaterial3d(fx.tracer_mat.clone()),
                Transform::from_translation((muzzle + end) * 0.5).looking_at(end, Vec3::Y).with_scale(Vec3::new(1.0, 1.0, len)),
                crate::weapons::Effect { age: 0.0, life: 0.05, grow: 0.0, light: false },
                NotShadowCaster,
            ));
        }
    }
    // By the damage lap, a landing shot does the lap's damage once (a miss
    // none); one stopped by cover tries again with the next.
    match lap_shot {
        Some(Some(d)) if target_damage > 0.0 => target_damage = d,
        Some(Some(_)) => npc.lap.progress = 1.0,
        Some(None) => target_damage = 0.0,
        None => {}
    }
    if target_damage > 0.0 {
        damage.write(DamageEvent {
            target: tgt.entity,
            amount: target_damage,
            kind: DamageKind::Bullet,
            source: Some(muzzle),
            instigator: Some(me),
            headshot: false,
            hit: crate::combat::Hit::Whole,
        });
    }
    for (entity, amount, from) in stray {
        shot_fx.stray.write(crate::vehicles::StrayHit { entity, amount, from, instigator: me });
    }
    let flash = match npc.role {
        Role::Shotgunner | Role::Sniper => 1.4,
        Role::Molotov => 0.8,
        _ => 1.1,
    };
    crate::particles::fx::muzzle_flash(&mut shot_fx.particles, rng, muzzle, last_dir, flash);
    noises.write(Noise { at: muzzle, radius: if npc.role == Role::Sniper { 150.0 } else { 90.0 }, kind: NoiseKind::Threatening });
    // The third-person sounds of each role's weapon (M133, SVD, 6P9, PKM, AK).
    match npc.role {
        Role::Shotgunner => {
            sfx.write(crate::sound::PlaySfx { event: ev::SHOTGUN_3P, at: Some(muzzle), volume: 1.0 });
        }
        Role::Sniper => {
            sfx.write(crate::sound::PlaySfx { event: ev::SNIPER_3P, at: Some(muzzle), volume: 1.0 });
        }
        Role::Molotov => {
            sfx.write(crate::sound::PlaySfx { event: ev::PISTOL_3P, at: Some(muzzle), volume: 0.9 });
        }
        Role::Heavy => {
            loops.write(crate::sound::LoopSfx { key: me.to_bits(), event: ev::LMG_3P, at: Some(muzzle), volume: 1.0, ..default() });
        }
        _ => {
            loops.write(crate::sound::LoopSfx { key: me.to_bits(), event: ev::RIFLE_3P, at: Some(muzzle), volume: 1.0, ..default() });
        }
    }
}

fn animate_limbs(children: &Children, limbs: &mut Query<(&NpcLimb, &mut Transform), (Without<Npc>, Without<Player>)>, speed: f32, t: f32, aiming: bool) {
    let swing = (t * (2.0 + speed * 1.8)).sin() * (speed / 3.0).min(1.0) * 0.6;
    for c in children.iter() {
        if let Ok((limb, mut tf)) = limbs.get_mut(c) {
            tf.rotation = match limb.kind {
                Limb::LeftLeg => Quat::from_rotation_x(swing),
                Limb::RightLeg => Quat::from_rotation_x(-swing),
                Limb::Torso => Quat::from_rotation_x(if aiming { -0.08 } else { 0.0 }),
                Limb::Gun => Quat::from_rotation_x(if aiming { 0.0 } else { -0.5 }),
            };
        }
    }
}

/// Distance on the ground plane.
fn flat_distance(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

fn yaw_to(v: Vec3) -> f32 {
    (-v.x).atan2(-v.z)
}

fn turn_toward(from: f32, to: f32, max: f32) -> f32 {
    let mut d = (to - from) % std::f32::consts::TAU;
    if d > std::f32::consts::PI {
        d -= std::f32::consts::TAU;
    } else if d < -std::f32::consts::PI {
        d += std::f32::consts::TAU;
    }
    from + d.clamp(-max, max)
}

/// Gunfire from the player alerts enemies; deaths become corpses.
fn react_to_death(
    time: Res<Time>,
    mut died: MessageReader<Died>,
    mut director: ResMut<NpcDirector>,
    mut commands: Commands,
    npcs: Query<(&Npc, &Transform)>,
    players: Query<Entity, With<Player>>,
    mut rng: ResMut<Rng>,
    mut anims: Query<&mut AnimRequest>,
    mut explosions: MessageWriter<crate::weapons::Explosion>,
    (mut hits, injury, mut rigs): (MessageReader<DamageEvent>, Res<InjuryClips>, Query<&mut CharacterRig>),
) {
    let player = players.single().ok();
    // The blow that killed each (its origin and whether it was to the head).
    let blows: std::collections::HashMap<Entity, (Option<Vec3>, bool, DamageKind)> = hits.read().map(|h| (h.target, (h.source, h.headshot, h.kind))).collect();
    for d in died.read() {
        let Ok((npc, t)) = npcs.get(d.entity) else { continue };
        // A death that fits: running, walking, standing or down in cover; shot
        // in the head, body or legs; from the front, back or a side. (Takedowns
        // and burning keep their own.)
        if let (Some(&(from, headshot, kind)), Some(mut r)) = (blows.get(&d.entity), npc.rig.and_then(|r| rigs.get_mut(r).ok())) {
            if matches!(kind, DamageKind::Bullet | DamageKind::Explosion) {
                let region = if headshot {
                    Region::Head
                } else if kind == DamageKind::Bullet && rng.f32() < 0.25 {
                    Region::Legs
                } else {
                    Region::Torso
                };
                let side = side_of(npc, t.translation, from.unwrap_or(t.translation));
                if let Some(clip) = InjuryClips::pick(&injury.deaths, motion_of(npc), region, side, &mut rng) {
                    swap_clip(&mut r, AnimKind::Death, clip);
                }
            }
        }
        director.cleared.push((npc.marker_id, time.elapsed_secs()));
        if d.killer == player {
            director.kills += 1;
        }
        let side = Vec3::new(rng.range(-1.0, 1.0), 0.0, rng.range(-1.0, 1.0)).normalize_or(Vec3::X);
        let skinned = npc.rig.is_some();
        if let Some(mut req) = npc.rig.and_then(|r| anims.get_mut(r).ok()) {
            *req = AnimRequest { kind: AnimKind::Death, rate: 1.0 };
        }
        commands.entity(d.entity).remove::<(Collider, RigidBody)>().insert(Corpse { age: 0.0, fall_axis: side, skinned, found: false });
        if npc.faction.hostile_to_player() {
            commands.entity(d.entity).insert(crate::pickups::LootableBody { role: npc.role, rig: npc.rig });
        }
        // A flamer's fuel tank goes up when he falls.
        if npc.role == Role::Flamer {
            explosions.write(crate::weapons::Explosion { at: t.translation, radius: 4.5, damage: 60.0, instigator: d.killer });
        }
    }
}

fn sink_corpses(time: Res<Time>, mut commands: Commands, mut q: Query<(Entity, &mut Corpse, &mut Transform)>) {
    let dt = time.delta_secs();
    for (e, mut c, mut t) in &mut q {
        c.age += dt;
        if c.age < 0.6 && !c.skinned {
            // Topple over.
            t.rotate_axis(Dir3::new(c.fall_axis).unwrap_or(Dir3::X), dt * 2.6);
            t.translation.y -= dt * 1.2;
        } else if c.age > CORPSE_LIFE {
            t.translation.y -= dt * 0.3;
            if c.age > CORPSE_LIFE + 7.0 {
                commands.entity(e).despawn();
            }
        }
    }
}

/// A body pulled off a ledge (the takedown from below): it tips over the
/// edge and drops to whatever is beneath, landing on its front.
#[derive(Component)]
pub struct LedgeFall {
    velocity: Vec3,
    /// Horizontal axis it tips about, and how far it has tipped.
    axis: Vec3,
    tipped: f32,
}

impl LedgeFall {
    /// Over the edge towards `out` (horizontal).
    pub fn toward(out: Vec3) -> Self {
        let out = Vec3::new(out.x, 0.0, out.z).normalize_or(Vec3::NEG_Z);
        Self { velocity: out * 1.6 + Vec3::Y * 0.6, axis: Vec3::Y.cross(out), tipped: 0.0 }
    }
}

/// How far a pulled body tips (its clip already leans it out) and how fast.
const FALL_TIP: f32 = 1.3;
const FALL_TIP_RATE: f32 = 2.3;

fn ledge_falls(
    time: Res<Time>,
    spatial: SpatialQuery,
    players: Query<Entity, With<Player>>,
    mut q: Query<(Entity, &mut LedgeFall, &mut Transform)>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut commands: Commands,
    probe: crate::surfaces::SurfaceProbe,
) {
    let dt = time.delta_secs();
    let filter = SpatialQueryFilter::from_excluded_entities(players.iter());
    for (e, mut f, mut t) in &mut q {
        let step = (FALL_TIP_RATE * dt).min(FALL_TIP - f.tipped);
        if step > 0.0 {
            t.rotation = Quat::from_axis_angle(f.axis, step) * t.rotation;
            f.tipped += step;
        }
        f.velocity.y -= 9.81 * dt;
        let next = t.translation + f.velocity * dt;
        // The body's centre comes to rest just off the ground as it lies flat.
        let rest = HALF_HEIGHT + (0.2 - HALF_HEIGHT) * (f.tipped / FALL_TIP);
        let below = spatial.cast_ray(t.translation, Dir3::NEG_Y, 40.0, true, &filter);
        let ground = below.map_or(next.y - 1.0, |hit| t.translation.y - hit.distance);
        let water = crate::water::surface(t.translation, Some(ground));
        let floor = ground.max(water - 0.4);
        if next.y <= floor + rest {
            t.translation = Vec3::new(next.x, floor + rest, next.z);
            if f.velocity.y < -2.0 {
                // The body hits the ground (or the water) with a thump.
                let surface = match below {
                    _ if ground < water - 0.4 => crate::surfaces::value::SHALLOW_WATER,
                    Some(hit) => probe.at(hit.entity, Vec3::new(t.translation.x, ground, t.translation.z), hit.normal),
                    None => probe.terrain(t.translation),
                };
                let event = crate::sound::switched(crate::sound::ev::LANDING, surface, 0);
                sfx.write(crate::sound::PlaySfx { event, at: Some(t.translation), volume: 0.8 });
            }
            f.velocity = Vec3::ZERO;
            if f.tipped >= FALL_TIP {
                commands.entity(e).remove::<LedgeFall>();
            }
        } else {
            t.translation = next;
        }
    }
}

/// Marks an NPC that takes no AI decisions (debug poses).
#[derive(Component)]
pub struct Frozen;

fn npc_test_spawn(
    args: Res<crate::Args>,
    mut done: Local<bool>,
    assets: Option<Res<NpcAssets>>,
    chars: Option<Res<CharacterAssets>>,
    models: Res<Assets<Fc3Model>>,
    server: Res<AssetServer>,
    players: Query<(&Player, &Transform)>,
    heights: Res<Heightfield>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
) {
    if *done || args.npc_test.is_none() {
        return;
    }
    let (Some(assets), Some(chars)) = (assets, chars) else { return };
    if !chars.ready(&server) {
        return;
    }
    let Ok((p, t)) = players.single() else { return };
    if p.spawning && !p.noclip {
        return;
    }
    let forward = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let right = Vec3::new(-forward.z, 0.0, forward.x);
    // Face the player.
    let yaw = (forward.x).atan2(forward.z);
    // "fight:role[:privateer]": three alerted enemies of a role, 15-30 m out.
    if let Some(spec) = args.npc_test.as_deref().and_then(|s| s.strip_prefix("fight:")) {
        let (name, faction) = match spec.split_once(':') {
            Some((r, "privateer")) => (r, Faction::Privateer),
            Some((r, "rebel")) => (r, Faction::Rebel),
            Some((r, _)) => (r, Faction::Pirate),
            None => (spec, Faction::Pirate),
        };
        let role = match name {
            "shotgun" | "shotgunner" => Role::Shotgunner,
            "sniper" => Role::Sniper,
            "heavy" => Role::Heavy,
            "rocket" | "rpg" => Role::Rocket,
            "molotov" => Role::Molotov,
            "flamer" => Role::Flamer,
            _ => Role::Assault,
        };
        let far = if role == Role::Sniper { 60.0 } else { 18.0 };
        for i in 0..3 {
            let mut at = t.translation + forward * (far + i as f32 * 6.0) + right * (i as f32 - 1.0) * 7.0;
            at.y = heights.height_at(at).unwrap_or(at.y - HALF_HEIGHT);
            let e = spawn_npc(&mut commands, &assets, Some(&chars), &models, faction, role, at, yaw, u64::MAX - 1 - i, &mut rng, None);
            let target = t.translation;
            commands.entity(e).entry::<Npc>().and_modify(move |mut n| {
                n.state = NpcState::Combat;
                n.awareness = 1.0;
                n.last_known = Some(target);
            });
        }
        *done = true;
        info!("npc-test: three {faction:?} {role:?} enemies attacking");
        return;
    }
    // "skirmish": three rebels and three pirates facing off across the view.
    if args.npc_test.as_deref() == Some("skirmish") {
        for (side, faction) in [(-1.0f32, Faction::Rebel), (1.0, Faction::Pirate)] {
            for i in 0..3u64 {
                let mut at = t.translation + forward * (10.0 + i as f32 * 4.0) + right * side * 15.0;
                at.y = heights.height_at(at).unwrap_or(at.y - HALF_HEIGHT);
                let facing = yaw_to(-right * side);
                let id = u64::MAX - 1 - i - if faction == Faction::Pirate { 3 } else { 0 };
                spawn_npc(&mut commands, &assets, Some(&chars), &models, faction, Role::Assault, at, facing, id, &mut rng, None);
            }
        }
        *done = true;
        info!("npc-test: skirmish");
        return;
    }
    // "distract" / "body": idle guards for the stealth checks (see `npc_test_stealth`).
    if matches!(args.npc_test.as_deref(), Some("distract" | "body" | "guards")) {
        for i in 0..2u64 {
            let mut at = t.translation + forward * 14.0 + right * (i as f32 * 4.0 - 2.0);
            at.y = heights.height_at(at).unwrap_or(at.y - HALF_HEIGHT);
            // They face each other.
            let facing = yaw_to(right * (1.0 - 2.0 * i as f32));
            let e = spawn_npc(&mut commands, &assets, Some(&chars), &models, Faction::Pirate, Role::Assault, at, facing, u64::MAX - 1 - i, &mut rng, None);
            commands.entity(e).entry::<Npc>().and_modify(|mut n| n.patrols = false);
        }
        *done = true;
        info!("npc-test: two idle guards");
        return;
    }
    // "crowd": a guard with three more a few metres beyond him, all facing
    // away (the grenade takedown's test).
    if args.npc_test.as_deref() == Some("crowd") {
        let facing = yaw_to(forward);
        for (i, (ahead, side)) in [(10.0, 0.0), (16.5, -1.5), (17.0, 1.5), (18.0, 0.0)].into_iter().enumerate() {
            let mut at = t.translation + forward * ahead + right * side;
            at.y = heights.height_at(at).unwrap_or(at.y - HALF_HEIGHT);
            let e =
                spawn_npc(&mut commands, &assets, Some(&chars), &models, Faction::Pirate, Role::Assault, at, facing, u64::MAX - 1 - i as u64, &mut rng, None);
            commands.entity(e).entry::<Npc>().and_modify(|mut n| n.patrols = false);
        }
        *done = true;
        info!("npc-test: a guard and three beyond him");
        return;
    }
    // "lineup": every enemy look side by side; otherwise one assault pirate.
    // Lineup: pirates in front, privateers behind.
    let lineup: Vec<usize> = if args.npc_test.as_deref() == Some("lineup") { (0..chars.looks.len()).collect() } else { vec![0] };
    let n = lineup.len() as f32;
    for look in lineup {
        let l = &chars.looks[look];
        let row: Vec<usize> = (0..chars.looks.len()).filter(|&j| chars.looks[j].faction == l.faction).collect();
        let col = row.iter().position(|&j| j == look).unwrap_or(0) as f32;
        let (dist, width) = match l.faction {
            // FC3_NPC_DIST brings the single test enemy closer (face close-ups).
            _ if n == 1.0 => (std::env::var("FC3_NPC_DIST").ok().and_then(|d| d.parse().ok()).unwrap_or(5.0), 0.0),
            Faction::Pirate => (6.0, 0.95),
            Faction::Privateer => (8.5, 1.15),
            Faction::Rebel => (4.0, 1.4),
        };
        let mut at = t.translation + forward * dist + right * (col - (row.len() as f32 - 1.0) * 0.5) * width;
        at.y = heights.height_at(at).unwrap_or(at.y - HALF_HEIGHT);
        spawn_npc(&mut commands, &assets, Some(&chars), &models, l.faction, l.role, at, yaw, u64::MAX, &mut rng, Some(look));
    }
    *done = true;
    info!("npc-test: spawned {n} posed enemies");
}

/// `FC3_KILL_TEST=head|body|hit` (tests, with `--npc-test idle`): two seconds
/// after the enemy appears, shoots it from the player's position (`hit`:
/// a wound, not a kill), taking `FC3_TD_SHOT` screenshots after.
/// Opt-in combat smoke: aim and press ordinary weapon controls. No damage,
/// ammunition, kills or mission events are injected.
fn outpost_fire_test(
    host: Option<NonSend<crate::story::StoryHost>>,
    cinema: Res<crate::cinema::Cinema>,
    time: Res<Time>,
    spatial: SpatialQuery,
    nav: Res<crate::nav::NavMesh>,
    mut players: Query<(Entity, &Transform, &mut Player, &Health, &crate::weapons::Inventory), Without<Npc>>,
    npcs: Query<(Entity, &Transform, &Npc, &Health), Without<Player>>,
    mut actions: ResMut<crate::controls::ActionState>,
    mut pulse: Local<f32>,
    mut route: Local<(f32, Vec<Vec3>)>,
    (busy, scripted, fp_cinema): (Res<crate::fp::HandsBusy>, Res<crate::story::ScriptedPlayer>, Res<crate::fp::Cinematic>),
    mut heal_pulse: Local<f32>,
    bodies: Query<(Entity, &GlobalTransform), With<crate::pickups::LootableBody>>,
    heads: Query<&HeadPos>,
    stats: Query<&crate::combat::PlayerStats, With<Player>>,
    animals: Query<(Entity, &Transform, &crate::wildlife::WildAnimal, &Health, Option<&crate::story_actors::StoryActor>), Without<Player>>,
) {
    use crate::controls::Action;
    if std::env::var("FC3_OUTPOST_FIRE_TEST").is_err() { return; }
    let Some(host) = host else { return };
    if !host.story.blocks().iter().any(|m| m.0 == "JAS_100_B10" && m.1 == "Running") || cinema.view.is_some() { return; }
    let Ok((pe, at, mut player, health, inv)) = players.single_mut() else { return };
    if health.dead || player.spawning || player.driving { return; }
    *heal_pulse = (*heal_pulse - time.delta_secs()).max(0.0);
    let can_heal = stats.single().is_ok_and(|s| s.medkits > 0) || health.current < crate::combat::HEALTH_SEGMENT;
    if can_heal && health.current < health.max * 0.6 && !busy.busy() && *heal_pulse == 0.0 {
        actions.press(Action::Medkit);
        *heal_pulse = 1.0;
        info!("outpost fire test: ordinary heal input at {:.1} health", health.current);
        return;
    }
    let eye = at.translation + Vec3::Y * (player.eye - crate::player::HALF_HEIGHT);
    let filter = SpatialQueryFilter::from_excluded_entities([pe]);
    let pirates = host.story.group_members("#C50341A9");
    let building = host.story.watched_triggers().into_iter().find(|t| t.0 == 2074714802313236257);
    let cleared = building.is_some() || (!pirates.is_empty() && pirates.iter().all(|id| host.story.is_dead(*id))
        && host.story.is_dead(2077647746472455879));
    let needs_ammo = !cleared && inv.slot().mag == 0 && inv.slot().reserve == 0;
    let candidates: Vec<_> = npcs.iter().filter(|(_, _, n, h)| n.faction.hostile_to_player() && !h.dead)
        .map(|(e, t, _, _)| (e, t.translation, heads.get(e).map_or(t.translation + Vec3::Y * 0.6, |head| head.0)))
        .chain(animals.iter().filter(|(_, t, animal, h, story)| !h.dead && animal.dangerous()
            && (story.is_some() || t.translation.distance(at.translation) < 15.0))
            .map(|(e, t, _, _, _)| (e, t.translation, t.translation)))
        .collect();
    let target = candidates.iter().filter(|_| !needs_ammo && !cleared)
        .filter_map(|&(e, _, aim_at)| {
            let delta = aim_at - eye;
            let distance = delta.length();
            let dir = Dir3::new(delta).ok()?;
            (distance < 30.0 && spatial.cast_ray(eye, dir, distance + 0.2, true, &filter).is_some_and(|h| h.entity == e))
                .then_some((e, dir.as_vec3(), distance))
        }).min_by(|a, b| a.2.total_cmp(&b.2));
    let Some((target, direction, distance)) = target else {
        let body = needs_ammo.then(|| bodies.iter().min_by(|a, b|
            a.1.translation().distance_squared(at.translation).total_cmp(&b.1.translation().distance_squared(at.translation))))
            .flatten();
        if let Some((entity, body_at)) = body {
            let feet = at.translation - Vec3::Y * crate::player::HALF_HEIGHT;
            let delta = body_at.translation() - feet;
            if delta.with_y(0.0).length() + delta.y.abs() * 0.5 < 2.4 {
                *pulse -= time.delta_secs();
                if *pulse <= 0.0 && !busy.busy() {
                    actions.press(Action::Interact);
                    *pulse = 0.5;
                    info!("outpost fire test: ordinary search input at {entity:?}");
                }
                return;
            }
        }
        route.0 -= time.delta_secs();
        if route.0 <= 0.0 {
            route.0 = 1.0;
            let nearest = candidates.iter()
                .min_by(|a, b| a.1.distance_squared(at.translation).total_cmp(&b.1.distance_squared(at.translation)));
            let destination = if cleared {
                // Walk into the authored main-building trigger after combat.
                building.as_ref().map(|t| Vec3::new(t.2[0], t.2[2], -t.2[1]))
            } else if needs_ammo { body.map(|(_, t)| t.translation()) }
                else { nearest.map(|(_, at, _)| *at - Vec3::Y * crate::player::HALF_HEIGHT) };
            route.1 = destination.and_then(|destination| nav.path(
                at.translation - Vec3::Y * crate::player::HALF_HEIGHT,
                destination)).unwrap_or_default();
            if time.elapsed_secs() as u32 % 5 == 0 {
                info!("outpost fire test: approach from {:.1?}, nearest {:?}, {} route nodes", at.translation,
                    nearest.map(|(e, at, _)| (e, at)), route.1.len());
                info!("outpost fire test: destination {destination:?}, next {:?}, movement {}, cinematic {}, noclip {}", route.1.first(), scripted.movement, fp_cinema.0, player.noclip);
            }
        }
        while route.1.first().is_some_and(|p| (*p - at.translation).with_y(0.0).length() < 1.0) {
            route.1.remove(0);
        }
        if let Some(next) = route.1.first() {
            let direction = (*next - at.translation).with_y(0.0).normalize_or_zero();
            player.yaw = (-direction.x).atan2(-direction.z);
            player.pitch = 0.0;
            actions.movement = Vec2::Y;
        }
        return;
    };
    let yaw = (-direction.x).atan2(-direction.z);
    let pitch = direction.y.asin();
    let aligned = (Quat::from_euler(EulerRot::YXZ, player.yaw, player.pitch, 0.0) * Vec3::NEG_Z).dot(direction) > 0.9999;
    player.yaw = yaw;
    player.pitch = pitch;
    actions.press(Action::Aim);
    *pulse -= time.delta_secs();
    if !aligned || inv.aim_blend < 0.95 || *pulse > 0.0 || inv.reloading > 0.0 || inv.switching > 0.0 || inv.cooldown > 0.0 { return; }
    *pulse = 0.3;
    if inv.slot().mag == 0 {
        if inv.slot().reserve > 0 { actions.press(Action::Reload); }
    } else {
        actions.press(Action::Fire);
        info!("outpost fire test: ordinary fire at {target:?}, {distance:.1} m, ammo {}/{}", inv.slot().mag, inv.slot().reserve);
    }
}

fn kill_test(
    args: Res<crate::Args>,
    time: Res<Time>,
    mut clock: Local<(Option<f32>, usize)>,
    npcs: Query<(Entity, &Npc)>,
    players: Query<(Entity, &Transform), With<Player>>,
    mut damage: MessageWriter<DamageEvent>,
    mut commands: Commands,
    (mut actions, progress, mut searched): (ResMut<crate::controls::ActionState>, Res<crate::progression::Progress>, Local<u8>),
) {
    let Ok(mode) = std::env::var("FC3_KILL_TEST") else { return };
    let Some((e, _)) = npcs.iter().find(|(_, n)| n.marker_id == u64::MAX) else { return };
    let Ok((pe, pt)) = players.single() else { return };
    let (start, shots) = &mut *clock;
    let t = time.elapsed_secs() - *start.get_or_insert(time.elapsed_secs());
    if t >= 2.0 && *shots == 0 {
        let amount = if mode == "hit" { 10.0 } else { 10_000.0 };
        damage.write(DamageEvent {
            target: e,
            amount,
            kind: DamageKind::Bullet,
            source: Some(pt.translation),
            instigator: Some(pe),
            headshot: mode == "head",
            hit: crate::combat::Hit::Whole,
        });
        info!("kill-test: shot ({mode})");
        *shots = 1;
    }
    // "loot": search the body once it is down, and log what was found.
    if mode == "loot" && (4.0..4.3).contains(&t) {
        actions.press(crate::controls::Action::Interact);
        *searched = 1;
    } else if mode == "loot" && t >= 4.5 && *searched == 1 {
        info!("kill-test: looted: money {} loot {:?}", progress.money, progress.loot);
        *searched = 2;
    }
    let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    if let (Some(&when), Some(path)) = (at.get(*shots - (*shots > 0) as usize), args.screenshot.as_ref()) {
        if *shots > 0 && t >= 2.0 + when {
            let stem = path.strip_suffix(".png").unwrap_or(path);
            crate::camera::capture(&mut commands, format!("{stem}_td{}.png", *shots - 1));
            *shots += 1;
        }
    }
}

/// Fight tests: logs the attackers every two seconds.
fn npc_test_log(
    args: Res<crate::Args>,
    time: Res<Time>,
    mut next: Local<f32>,
    npcs: Query<(&Npc, &Transform, &Health)>,
    players: Query<(&Transform, &Health), With<Player>>,
) {
    if !args.npc_test.as_deref().is_some_and(|s| s.starts_with("fight:") || s == "skirmish") || time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 2.0;
    let Ok((pt, ph)) = players.single() else { return };
    let list: Vec<String> = npcs
        .iter()
        .filter(|(n, _, _)| n.marker_id >= u64::MAX - 6 && n.marker_id < u64::MAX)
        .map(|(n, t, h)| {
            let cover = match n.cover {
                Some(c) if !c.settled => " to cover",
                Some(c) if c.up => " cover:up",
                Some(_) => " cover:down",
                None => "",
            };
            format!("{:?} {:?}{cover} {:.0}m {:.0}hp foe {}", n.faction, n.state, t.translation.distance(pt.translation), h.current, n.foe_seen)
        })
        .collect();
    info!("fight: player {:.0}hp | {}", ph.current, list.join(" | "));
}

/// Stealth checks: a noise beside the guards ("distract") or one guard killed
/// in view of the other ("body"), then the guards' states every second.
fn npc_test_stealth(
    args: Res<crate::Args>,
    time: Res<Time>,
    mut clock: Local<(f32, f32, bool)>,
    npcs: Query<(Entity, &Npc, &Transform, &Health)>,
    mut noise: MessageWriter<Noise>,
    mut damage: MessageWriter<DamageEvent>,
) {
    let Some(mode) = args.npc_test.as_deref().filter(|m| matches!(*m, "distract" | "body" | "guards")) else { return };
    let mut guards: Vec<(Entity, &Npc, &Transform, &Health)> =
        npcs.iter().filter(|(_, n, _, _)| n.marker_id >= u64::MAX - 2 && n.marker_id < u64::MAX).collect();
    if guards.len() < 2 {
        return;
    }
    guards.sort_by_key(|(_, n, _, _)| n.marker_id);
    let (start, next_log, fired) = &mut *clock;
    if *start == 0.0 {
        *start = time.elapsed_secs();
    }
    let t = time.elapsed_secs() - *start;
    if t > 8.0 && !*fired && mode != "guards" {
        *fired = true;
        let (a, b) = (guards[0].2.translation, guards[1].2.translation);
        if mode == "distract" {
            let side = (b - a).normalize_or_zero().cross(Vec3::Y) * 8.0;
            let at = a + side;
            info!("stealth-test: noise at {at:.1?}");
            noise.write(Noise { at, radius: 16.0, kind: NoiseKind::Interesting });
        } else {
            info!("stealth-test: killing one guard");
            damage.write(DamageEvent {
                target: guards[1].0,
                amount: 10_000.0,
                kind: DamageKind::Melee,
                source: None,
                instigator: None,
                headshot: false,
                hit: crate::combat::Hit::Whole,
            });
        }
    }
    if t > *next_log {
        *next_log = t + 1.0;
        let list: Vec<String> =
            guards.iter().map(|(_, n, tf, h)| format!("{:?}{} at {:.1?}", n.state, if h.dead { " (dead)" } else { "" }, tf.translation)).collect();
        info!("stealth-test t={t:.0}: {}", list.join(" | "));
    }
}

fn npc_test_pose(
    args: Res<crate::Args>,
    mut commands: Commands,
    mut npcs: Query<(Entity, &mut Npc), Without<Frozen>>,
    mut anims: Query<&mut AnimRequest>,
    frozen: Query<(&Npc, &Health), With<Frozen>>,
) {
    let Some(name) = args.npc_test.as_deref() else { return };
    let kind = match name {
        "walk" => AnimKind::Walk,
        "run" => AnimKind::Run,
        "aim" => AnimKind::Aim,
        "fire" => AnimKind::Fire,
        "reload" => AnimKind::Reload,
        "death" => AnimKind::Death,
        _ => AnimKind::Idle,
    };
    for (e, mut npc) in &mut npcs {
        if npc.marker_id == u64::MAX {
            npc.state = NpcState::Idle;
            commands.entity(e).insert(Frozen);
        }
    }
    // (The dead play their death.)
    for (npc, _) in frozen.iter().filter(|(_, h)| !h.dead) {
        if let Some(mut req) = npc.rig.and_then(|r| anims.get_mut(r).ok()) {
            *req = AnimRequest { kind, rate: 1.0 };
        }
    }
}

/// Lets player gunfire (and his explosions) be heard by the AI.
pub fn player_noise(writer: &mut MessageWriter<Noise>, at: Vec3, radius: f32, kind: NoiseKind) {
    writer.write(Noise { at, radius, kind });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injury_clip_names() {
        let k = |p: &str| injury_kind(p).map(|(d, m, r, s)| (d, m, r, s));
        assert_eq!(k(r"a\injury\death\run\torso\3rdge_fulbst_run_dietorso_frombk_v01_+000fw_ak74.mab"), Some((true, Motion::Run, Region::Torso, Side::Back)));
        assert_eq!(k(r"a\injury\death\stand\head\3rdge_fulbst_diehead00_fromlt_+000fw_ak74.mab"), Some((true, Motion::Stand, Region::Head, Side::Left)));
        assert_eq!(
            k(r"a\injury\hits\crouch\torso\3rdge_fulbcr_idlealert_hittorso_from180rt_v00_ak74.mab"),
            Some((false, Motion::Crouch, Region::Torso, Side::Back))
        );
        assert_eq!(
            k(r"a\injury\death\coverhi\3rdge_fulbcr_coverhileftdie_generic_fromfwv01_+000fw_ak74.mab"),
            Some((true, Motion::Crouch, Region::Torso, Side::Front))
        );
        assert_eq!(k(r"a\injury\death\stand\legs\3rdge_fulbst_dielegs_v01_+000fw_ak74.mab"), None);
    }
}

/// The game's close-quarters rifle strikes for enemies.
const MELEE_CLIPS: [&str; 3] = [
    r"graphics\characters\_common\animations\weapons\npc\lotuseaters\ak47\melee\3rdge_fulbst_idlealert_aimnormal_meleehit_v00_+000fw_ak74.mab",
    r"graphics\characters\_common\animations\weapons\npc\lotuseaters\ak47\melee\3rdge_fulbst_idlealert_aimnormal_meleehit_v01_+000fw_ak74.mab",
    r"graphics\characters\_common\animations\weapons\npc\lotuseaters\ak47\melee\3rdge_fulbst_idlealert_aimnormal_meleehit_v02_+000fw_ak74.mab",
];
/// Reach to start a strike, reach when it lands, when it lands, how long it
/// holds the enemy, its damage and the wait before another.
const MELEE_START: f32 = 1.6;
const MELEE_REACH: f32 = 2.1;
const MELEE_CONTACT: f32 = 0.48;
const MELEE_TIME: f32 = 1.1;
const MELEE_DAMAGE: f32 = 25.0;
const MELEE_COOLDOWN: f32 = 2.5;

#[derive(Resource)]
struct MeleeClips(Vec<Handle<Fc3Clip>>);

/// Struck in close quarters (the player's machete), by side: front, left,
/// right, back.
const MELEE_HIT_CLIPS: [&str; 4] = [
    r"graphics\characters\_common\animations\melee\melee_enemy_hitreacts\rifle\3rdge_fulbst_cqchitft_+000fw_prak7_.mab",
    r"graphics\characters\_common\animations\melee\melee_enemy_hitreacts\rifle\3rdge_fulbst_cqchitlt_+000fw_prak7_.mab",
    r"graphics\characters\_common\animations\melee\melee_enemy_hitreacts\rifle\3rdge_fulbst_cqchitrt_+000fw_prak7_.mab",
    r"graphics\characters\_common\animations\melee\melee_enemy_hitreacts\rifle\3rdge_fulbst_cqchitrt_-180bk_prak7_.mab",
];

#[derive(Resource)]
struct MeleeHitClips(Vec<Handle<Fc3Clip>>);

fn load_melee_clips(mut commands: Commands, server: Res<AssetServer>) {
    let load = |p: &&str| server.load(crate::cache_assets::cache_path(&fc3_formats::cache::anim_rel(p)));
    commands.insert_resource(MeleeClips(MELEE_CLIPS.iter().map(load).collect()));
    commands.insert_resource(MeleeHitClips(MELEE_HIT_CLIPS.iter().map(load).collect()));
}

/// An enemy in a fight with the player at arm's length swings the rifle butt
/// (instead of shooting), knocking the view.
#[allow(clippy::too_many_arguments)]
fn npc_melee(
    time: Res<Time>,
    clips: Option<Res<MeleeClips>>,
    mut rng: ResMut<Rng>,
    mut players: Query<(Entity, &Transform, &mut Player, &Health), Without<Npc>>,
    mut npcs: Query<(Entity, &Transform, &mut Npc, &Health), (Without<Corpse>, Without<Riding>, Without<crate::turrets::Gunner>)>,
    mut rigs: Query<&mut crate::characters::CharacterRig>,
    mut anims: Query<&mut AnimRequest>,
    mut damage: MessageWriter<DamageEvent>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    cinema: Res<crate::cinema::Cinema>,
) {
    let Some(clips) = clips else { return };
    let Ok((pe, pt, mut player, ph)) = players.single_mut() else { return };
    if cinema.cutscene() {
        return;
    }
    let dt = time.delta_secs();
    for (e, t, mut npc, h) in &mut npcs {
        npc.melee_cooldown -= dt;
        if h.dead {
            npc.melee = None;
            continue;
        }
        let to = pt.translation - t.translation;
        let flat = Vec2::new(to.x, to.z);
        let facing = Vec2::new(-npc.yaw.sin(), -npc.yaw.cos());
        let ahead = flat.normalize_or_zero().dot(facing);
        match npc.melee {
            None => {
                let ready = npc.state == NpcState::Combat && npc.melee_cooldown <= 0.0 && !ph.dead && !player.noclip && !player.driving && to.y.abs() < 1.2;
                if !ready || flat.length() > MELEE_START || ahead < 0.6 || npc.role == Role::Flamer {
                    continue;
                }
                npc.melee = Some(0.0);
                npc.melee_cooldown = MELEE_COOLDOWN;
                let pick = clips.0[(rng.f32() * clips.0.len() as f32) as usize % clips.0.len()].clone();
                if let Some(mut r) = npc.rig.and_then(|r| rigs.get_mut(r).ok()) {
                    swap_clip(&mut r, AnimKind::Attack, pick);
                }
            }
            Some(s) => {
                let s = s + dt;
                // The butt lands: if he's still in front and in reach.
                if s >= MELEE_CONTACT && s - dt < MELEE_CONTACT && flat.length() < MELEE_REACH && ahead > 0.3 && to.y.abs() < 1.2 {
                    damage.write(DamageEvent {
                        target: pe,
                        amount: MELEE_DAMAGE,
                        kind: DamageKind::Melee,
                        source: Some(t.translation),
                        instigator: Some(e),
                        headshot: false,
                        hit: crate::combat::Hit::Whole,
                    });
                    sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::BUTT_HIT, at: Some(pt.translation), volume: 0.9 });
                    // Knocked back a step, the view jolted.
                    let push = flat.normalize_or_zero() * 2.5;
                    player.velocity += Vec3::new(push.x, 0.0, push.y);
                    player.pitch = (player.pitch + 0.12).min(1.5);
                    player.yaw += rng.range(-0.08, 0.08);
                }
                npc.melee = (s < MELEE_TIME).then_some(s);
            }
        }
        if npc.melee.is_some() {
            // No shooting mid-swing, and the strike plays out.
            npc.fire_cooldown = npc.fire_cooldown.max(0.3);
            if let Some(mut a) = npc.rig.and_then(|r| anims.get_mut(r).ok()) {
                *a = AnimRequest { kind: AnimKind::Attack, rate: 1.0 };
            }
        }
    }
}

/// `FC3_MELEE_TEST` (with `--npc-test fight:<role>` and `FC3_GOD`): keeps the
/// player at arm's length in front of the nearest enemy in a fight.
fn melee_test(mut players: Query<(&mut Transform, &mut Player), Without<Npc>>, npcs: Query<(&Transform, &Npc, &Health), Without<Player>>) {
    if std::env::var("FC3_MELEE_TEST").is_err() {
        return;
    }
    let Ok((mut pt, mut p)) = players.single_mut() else { return };
    let near = npcs
        .iter()
        .filter(|(_, n, h)| !h.dead && n.state == NpcState::Combat)
        .min_by(|a, b| a.0.translation.distance(pt.translation).total_cmp(&b.0.translation.distance(pt.translation)));
    let Some((nt, n, _)) = near else { return };
    let facing = Vec3::new(-n.yaw.sin(), 0.0, -n.yaw.cos());
    pt.translation = nt.translation + facing * 1.3;
    p.velocity = Vec3::ZERO;
    p.yaw = (facing.x).atan2(facing.z);
}
