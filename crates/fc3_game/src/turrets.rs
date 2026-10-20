//! Mounted machine guns: the DShK and M249 turrets the world places on tripods
//! and bunker mounts. The player mans one with the interact key: the view sits
//! behind the gun and turns within its arcs, and the gun fires without
//! reloading but overheats. Enemies get on a free one when a fight comes near.

use std::collections::HashSet;
use std::f32::consts::PI;

use bevy::prelude::*;
use fc3_formats::cache;

use crate::cache_assets::{cache_path, Fc3Model, Fc3Sector};
use crate::combat::Health;
use crate::controls::{Action, ActionState};
use crate::hud::HudState;
use crate::objects::SectorObjects;
use crate::player::{GunArcs, Player, HALF_HEIGHT};
use crate::weapons::{Inventory, WeaponSlot};

pub struct TurretsPlugin;

impl Plugin for TurretsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GunHeat>()
            .add_systems(Startup, load_mounts)
            .add_systems(PreUpdate, (turret_test, gunner_test, armed_test).after(crate::controls::update_actions))
            .add_systems(
                Update,
                (spawn_turrets, arm_vehicles, follow_vehicles, man_turrets, recruit_gunners, crew_gunners, drive_gunners, heat, pose_guns)
                    .chain()
                    .after(crate::patrols::carry_crew)
                    .run_if(in_state(crate::menu::GameState::Playing)),
            );
    }
}

/// A kind of placed turret: its entity name prefix, mount model and gun (a
/// weapon by name), and how far it turns either side of its facing.
struct Kind {
    prefix: &'static str,
    mount: usize,
    gun: &'static str,
    arc_deg: f32,
}

const MOUNTS: [&str; 2] =
    [r"graphics\__fc3_graphics\_common\weapons\turret\tripod_01.xbg", r"graphics\__fc3_graphics\_common\weapons\turret\tripodmount_01.xbg"];

const KINDS: &[Kind] = &[
    Kind { prefix: "fc3/turret_ground360_dshk", mount: 0, gun: "DShK", arc_deg: 180.0 },
    Kind { prefix: "fc3/turret_ground180_dshk", mount: 1, gun: "DShK", arc_deg: 45.0 },
    Kind { prefix: "fc3/turret_ground360_m249", mount: 0, gun: "M249", arc_deg: 180.0 },
    // On an armed technical (not placed on its own), and on the barge's tripod.
    Kind { prefix: "", mount: 0, gun: "DShK", arc_deg: 180.0 },
    Kind { prefix: "", mount: 0, gun: "M249", arc_deg: 180.0 },
];
const VEHICLE_KIND: usize = 3;

impl Kind {
    /// The gun in the game's weapons table (its muzzle's effects).
    fn weapon(&self) -> &'static str {
        match self.gun {
            "M249" => "M249_Mounted",
            _ => "DSHK_Mounted",
        }
    }
}

/// The kind of gun a vehicle carries, by its weapon.
fn vehicle_kind(gun: &str) -> usize {
    (VEHICLE_KIND..KINDS.len()).find(|k| KINDS[*k].gun == gun).unwrap_or(VEHICLE_KIND)
}

/// How far down and up every mount lets the gun point (degrees).
const PITCH: (f32, f32) = (-40.0, 23.0);
/// How close to the gunner's spot the player must be to get on.
const REACH: f32 = 1.4;
/// Turrets appear within this distance and go beyond the second.
const SPAWN: f32 = 160.0;
const DESPAWN: f32 = 200.0;
/// Heat per round, cooling per second once the trigger is let go, and where
/// an overheated gun can fire again.
const HEAT_PER_SHOT: f32 = 0.022;
const COOLING: f32 = 0.32;
const RECOVERED: f32 = 0.35;

fn kind_for(name: &str) -> Option<usize> {
    let n = name.to_ascii_lowercase();
    KINDS.iter().position(|k| !k.prefix.is_empty() && n.starts_with(k.prefix))
}

#[derive(Resource)]
struct MountModels(Vec<Handle<Fc3Model>>);

fn load_mounts(mut commands: Commands, server: Res<AssetServer>) {
    commands.insert_resource(MountModels(MOUNTS.iter().map(|m| server.load(cache_path(&cache::model_rel(m)))).collect()));
    commands.insert_resource(GunnerClip(server.load(cache_path(&cache::anim_rel(GUNNER_CLIP)))));
}

/// A gunner behind a DShK: its frames run from the gun pointing all the way
/// down (hands high on the grips) to all the way up.
const GUNNER_CLIP: &str = r"graphics\characters\_common\animations\weapons\mounted\dshk\3rdge_fulbst_aimvert+0fw_+000fw_modshk.mab";

#[derive(Resource)]
struct GunnerClip(Handle<crate::cache_assets::Fc3Clip>);

/// A mounted gun, on the entity that shows it (at the pivot).
#[derive(Component)]
pub struct Turret {
    kind: usize,
    spawn_id: u64,
    /// The mount's facing (yaw), the gun's pivot and the gunner's spot.
    base_yaw: f32,
    pub pivot: Vec3,
    /// Where the gunner stands, in the gun's turning frame (it swings round
    /// with the gun).
    back: Vec3,
    /// Where the gun points (world yaw and pitch).
    pub yaw: f32,
    pub pitch: f32,
    pub heat: f32,
    pub overheated: bool,
    /// Who is on it.
    pub user: Option<Entity>,
    /// On a vehicle: it, its physics body, and the pivot and facing relative
    /// to it.
    vehicle: Option<(Entity, Entity, Vec3, f32)>,
}

impl Turret {
    /// The look limits for whoever mans it.
    pub fn arcs(&self, turret: Entity) -> GunArcs {
        GunArcs { turret, base_yaw: self.base_yaw, arc: KINDS[self.kind].arc_deg.to_radians(), pitch: (PITCH.0.to_radians(), PITCH.1.to_radians()) }
    }

    /// The gun's weapon (by name).
    pub fn gun(&self) -> &'static str {
        KINDS[self.kind].gun
    }

    /// The vehicle it rides on, if any.
    pub fn on_vehicle(&self) -> Option<Entity> {
        self.vehicle.map(|v| v.0)
    }

    /// The muzzle's direction.
    pub fn forward(&self) -> Vec3 {
        Quat::from_euler(EulerRot::YXZ, self.yaw, self.pitch, 0.0) * Vec3::NEG_Z
    }

    /// Where the gunner stands (feet), behind the gun as it points now.
    pub fn gunner(&self) -> Vec3 {
        self.pivot + Quat::from_rotation_y(self.yaw) * self.back
    }

    /// The muzzle.
    fn muzzle(&self) -> Vec3 {
        self.pivot + Vec3::Y * 0.08 + self.forward() * 1.2
    }
}

/// The manned gun's heat, for the HUD.
#[derive(Resource, Default)]
pub struct GunHeat {
    pub heat: f32,
    pub overheated: bool,
}

/// A node's model-space position, by name prefix (bunker mounts number theirs).
fn node(model: &Fc3Model, prefix: &str) -> Option<Vec3> {
    let name = model.bones.iter().find(|b| b.name.starts_with(prefix))?.name.clone();
    crate::weapons::node_pose(model, &name).map(|t| t.translation)
}

#[allow(clippy::too_many_arguments)]
fn spawn_turrets(
    mut spawned: Local<HashSet<u64>>,
    mounts: Option<Res<MountModels>>,
    models: Res<Assets<Fc3Model>>,
    sectors: Res<Assets<Fc3Sector>>,
    loaded: Query<&SectorObjects>,
    players: Query<(&Transform, &Inventory), With<Player>>,
    existing: Query<(Entity, &Turret)>,
    mut commands: Commands,
) {
    let Some(mounts) = mounts else { return };
    let Ok((pt, inv)) = players.single() else { return };
    for (e, t) in &existing {
        if t.vehicle.is_none() && t.user.is_none() && t.pivot.distance(pt.translation) > DESPAWN {
            spawned.remove(&t.spawn_id);
            commands.entity(e).despawn();
        }
    }
    for so in &loaded {
        let Some(sector) = sectors.get(&so.sector) else { continue };
        for e in &sector.entities {
            let Some(k) = kind_for(&e.name) else { continue };
            let at = Vec3::from_array(e.translation);
            if spawned.contains(&e.id) || at.distance(pt.translation) > SPAWN {
                continue;
            }
            let kind = &KINDS[k];
            let Some(mount) = models.get(&mounts.0[kind.mount]) else { continue };
            let Some(def) = inv.defs.iter().position(|d| d.name == kind.gun) else { continue };
            let Some(gun) = models.get(&inv.models[def]) else { continue };
            let rotation = Quat::from_array(e.rotation);
            let (Some(pivot), Some(gunner)) = (node(mount, "GunMount"), node(mount, "GunnerPh")) else { continue };
            let (pivot, gunner) = (at + rotation * pivot, at + rotation * gunner);
            let fwd = rotation * Vec3::NEG_Z;
            let base_yaw = (-fwd.x).atan2(-fwd.z);
            spawned.insert(e.id);
            let back = Quat::from_rotation_y(-base_yaw) * (gunner - pivot);
            let turret =
                Turret { kind: k, spawn_id: e.id, base_yaw, pivot, back, yaw: base_yaw, pitch: 0.0, heat: 0.0, overheated: false, user: None, vehicle: None };
            let root = commands.spawn((turret, Transform::from_translation(pivot).with_rotation(Quat::from_rotation_y(base_yaw)), Visibility::Visible)).id();
            // The gun on its mount, its grip over the pivot.
            let seat = Transform::from_translation(gun_offset(kind.gun));
            let holder = commands.spawn((seat, Visibility::Inherited, ChildOf(root))).id();
            for part in gun.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]).iter().filter(|p| p.shown(true)) {
                commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), ChildOf(holder)));
            }
            info!("turret {} at {:.0?} facing {:.0} deg", e.name, pivot, base_yaw.to_degrees());
        }
    }
}

/// Where a gun's model origin sits relative to the mount's pivot.
fn gun_offset(gun: &str) -> Vec3 {
    match gun {
        // The M249's origin is its pistol grip; its receiver sits on the mount.
        "M249" => Vec3::new(0.0, 0.1, 0.18),
        _ => Vec3::ZERO,
    }
}

/// The player gets on a free gun next to them (and off again) with the
/// interact key; while on, the view rides behind the gun. Runs ahead of the
/// other interact prompts: standing at a gun's grips is the most specific.
#[allow(clippy::too_many_arguments)]
pub(crate) fn man_turrets(
    time: Res<Time>,
    mut actions: ResMut<ActionState>,
    settings: Res<crate::settings::Settings>,
    mut hud: ResMut<HudState>,
    mut shown_hint: Local<f32>,
    mut players: Query<(Entity, &mut Player, &mut Transform, &mut Inventory, &Health), Without<Turret>>,
    mut turrets: Query<(Entity, &mut Turret)>,
    (host, vehicles): (Option<NonSend<crate::story::StoryHost>>, Query<&crate::vehicles::Vehicle>),
) {
    let Ok((pe, mut p, mut pt, mut inv, h)) = players.single_mut() else { return };
    let label = actions.label(Action::Interact, &settings);
    if let Some(arcs) = p.mounted {
        let on = turrets.get_mut(arcs.turret).ok();
        // (Held on a vehicle's gun a script won't let the player out of:
        // HUR_040's barge.)
        let held = on
            .as_ref()
            .and_then(|(_, t)| t.on_vehicle())
            .and_then(|ve| vehicles.get(ve).ok())
            .is_some_and(|v| host.as_ref().is_some_and(|h| h.story.vehicle_rules(v.story_id()).exit_disabled));
        let leave = h.dead || on.is_none() || p.driving || (actions.just_pressed(Action::Interact) && !held);
        if leave {
            actions.consume(Action::Interact);
            p.mounted = None;
            // Back to the weapon held before.
            if let Some(slot) = inv.mounted_from.take() {
                let last = inv.slots.len() - 1;
                if inv.defs[inv.slots[last].def].class == crate::weapons::WeaponClass::Mounted {
                    inv.slots.pop();
                }
                inv.current = slot.min(inv.slots.len() - 1);
                inv.switching = 0.35;
            }
            if let Some((_, mut t)) = on {
                t.user = None;
                // Down behind the gun.
                pt.translation = t.gunner() + Vec3::Y * (HALF_HEIGHT + 0.05);
            }
            return;
        }
        let Some((_, mut t)) = on else { return };
        t.yaw = p.yaw;
        t.pitch = p.pitch;
        // The view where it sees the gun on its mount: the gun sits at its
        // hold (plus the arms' shift) before the eye.
        let hold = inv.def().hold + crate::fp::MOUNT_VIEW;
        let eye = t.pivot + Quat::from_euler(EulerRot::YXZ, t.yaw, t.pitch, 0.0) * (gun_offset(t.gun()) - hold);
        pt.translation = eye - Vec3::Y * (p.eye - HALF_HEIGHT);
        p.velocity = Vec3::ZERO;
        // A belt that never runs out.
        let cur = inv.current;
        inv.slots[cur].mag = 999;
        *shown_hint += time.delta_secs();
        if *shown_hint < 4.0 && hud.prompt.is_none() {
            hud.prompt = Some(format!("{label}  leave the gun"));
        }
        return;
    }
    if h.dead || p.driving || p.swimming || p.noclip || hud.prompt.is_some() {
        if (std::env::var("FC3_TURRET_TEST").is_ok() || std::env::var("FC3_STORY_AUTOPILOT").is_ok()) && actions.just_pressed(Action::Interact) {
            debug!("turret: not now (dead {} driving {} swimming {} noclip {} prompt {:?})", h.dead, p.driving, p.swimming, p.noclip, hud.prompt);
        }
        return;
    }
    let near = turrets
        .iter()
        .filter(|(_, t)| t.user.is_none())
        .map(|(e, t)| (e, t.gunner()))
        .map(|(e, g)| (e, Vec2::new(g.x - pt.translation.x, g.z - pt.translation.z).length(), g.y))
        .filter(|(_, d, y)| *d < REACH && (pt.translation.y - HALF_HEIGHT - y).abs() < 1.5)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((e, _, _)) = near else { return };
    hud.prompt = Some(format!("{label}  use the machine gun"));
    if !actions.used() {
        return;
    }
    actions.consume(Action::Interact);
    let Ok((_, mut t)) = turrets.get_mut(e) else { return };
    let Some(def) = inv.defs.iter().position(|d| d.name == t.gun()) else { return };
    t.user = Some(pe);
    p.mounted = Some(t.arcs(e));
    p.yaw = t.yaw;
    p.pitch = 0.0;
    p.velocity = Vec3::ZERO;
    inv.mounted_from = Some(inv.current);
    inv.slots.push(WeaponSlot { def, mag: 999, reserve: 0 });
    inv.current = inv.slots.len() - 1;
    inv.reloading = 0.0;
    inv.switching = 0.35;
    *shown_hint = 0.0;
}

/// Firing heats the manned gun; too hot and it stops until it cools.
fn heat(
    time: Res<Time>,
    mut last_shots: Local<u32>,
    mut gauge: ResMut<GunHeat>,
    mut players: Query<(Entity, &mut Inventory), With<Player>>,
    mut turrets: Query<&mut Turret>,
    perks: Res<crate::progression::Perks>,
) {
    let Ok((pe, mut inv)) = players.single_mut() else { return };
    let dt = time.delta_secs();
    let fired = inv.shots_fired.wrapping_sub(*last_shots);
    *last_shots = inv.shots_fired;
    for mut t in &mut turrets {
        let ours = t.user == Some(pe);
        if ours {
            // (`SP_MountedGunHandling`: slower.)
            t.heat += fired as f32 * HEAT_PER_SHOT * perks.gun_heat;
        }
        if !ours || inv.since_shot > 0.3 {
            t.heat = (t.heat - COOLING * dt).max(0.0);
        }
        if t.heat >= 1.0 {
            t.heat = 1.0;
            t.overheated = true;
        } else if t.overheated && t.heat <= RECOVERED {
            t.overheated = false;
        }
        if ours {
            if t.overheated {
                inv.cooldown = inv.cooldown.max(0.1);
            }
            gauge.heat = t.heat;
            gauge.overheated = t.overheated;
        }
    }
}

/// Points each gun where it's aimed; the player's own shows as the view model.
fn pose_guns(players: Query<Entity, With<Player>>, mut turrets: Query<(&Turret, &mut Transform, &mut Visibility)>) {
    let player = players.single().ok();
    for (t, mut tf, mut vis) in &mut turrets {
        tf.translation = t.pivot;
        tf.rotation = Quat::from_euler(EulerRot::YXZ, t.yaw, t.pitch, 0.0);
        let want = if t.user.is_some() && t.user == player { Visibility::Hidden } else { Visibility::Visible };
        if *vis != want {
            *vis = want;
        }
    }
}

/// Wraps an angle to -PI..PI.
pub fn wrap(a: f32) -> f32 {
    (a + PI).rem_euclid(2.0 * PI) - PI
}

/// `FC3_TURRET_TEST`: steps up to the nearest gun, gets on, swings it round
/// and fires until it overheats, logging the heat (`FC3_TD_SHOT` screenshots
/// that long after getting on).
#[allow(clippy::too_many_arguments)]
fn turret_test(
    time: Res<Time>,
    args: Res<crate::Args>,
    mut state: Local<(Option<f32>, usize, f32)>,
    mut actions: ResMut<ActionState>,
    mut players: Query<(&mut Player, &mut Transform), Without<Turret>>,
    turrets: Query<&Turret>,
    gauge: Res<GunHeat>,
    mut captured: ResMut<crate::player::InputCaptured>,
    mut commands: Commands,
) {
    if std::env::var("FC3_TURRET_TEST").is_err() {
        return;
    }
    // Automated runs don't capture input, which firing needs.
    captured.0 = true;
    let Ok((p, mut pt)) = players.single_mut() else { return };
    if p.spawning {
        return;
    }
    let (on_at, shots, last_log) = &mut *state;
    let now = time.elapsed_secs();
    let Some(t) = turrets.iter().filter(|t| t.vehicle.is_none()).min_by(|a, b| a.pivot.distance(pt.translation).total_cmp(&b.pivot.distance(pt.translation)))
    else {
        return;
    };
    let Some(start) = *on_at else {
        if p.mounted.is_none() {
            // Behind the gun, then on.
            pt.translation = t.gunner() + Vec3::Y * (HALF_HEIGHT + 0.05);
            actions.press(Action::Interact);
        } else {
            *on_at = Some(now);
            info!("turret-test: on the {} at {:.0?}", t.gun(), t.pivot);
        }
        return;
    };
    let secs = now - start;
    let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    if let (Some(&when), Some(path)) = (at.get(*shots), args.screenshot.as_ref()) {
        if secs >= when {
            let stem = path.strip_suffix(".png").unwrap_or(path);
            crate::camera::capture(&mut commands, format!("{stem}_td{shots}.png"));
            *shots += 1;
        }
    }
    // A slow sweep, then a long burst.
    if (1.0..3.0).contains(&secs) {
        actions.look.x = 0.6 * time.delta_secs();
    }
    if secs > 3.0 && secs < 12.0 {
        actions.press(Action::Fire);
    }
    if now - *last_log >= 1.0 {
        *last_log = now;
        info!("turret-test t={secs:.0} yaw {:.0} pitch {:.0} heat {:.2} overheated {}", t.yaw.to_degrees(), t.pitch.to_degrees(), gauge.heat, gauge.overheated);
    }
    if secs > 13.0 && p.mounted.is_some() {
        actions.press(Action::Interact);
        info!("turret-test: off");
    }
}

/// An enemy on (or on the way to) a mounted gun; the usual fighting is off
/// while it is.
#[derive(Component)]
pub struct Gunner {
    turret: Entity,
    on: bool,
    /// Seconds left of the burst (or, below zero, of the pause before the next).
    burst: f32,
    cooldown: f32,
    /// How long the target has been somewhere the gun can't point.
    out_of_arc: f32,
    /// A crewman on a vehicle's gun: rides along and keeps the gun.
    riding: bool,
    /// Seconds the player has been in its sights (aim settles in).
    engaged: f32,
    /// The clips it had before taking the gun's pose.
    clips: Option<std::sync::Arc<crate::characters::ClipSet>>,
}

/// How near a free gun an enemy in a fight must be to go for it, and how
/// close the player can come before a gunner gets off to fight.
const RECRUIT: f32 = 14.0;
const TOO_CLOSE: f32 = 3.0;
/// How fast a gunner swings the gun (radians a second), and its bursts.
const GUNNER_TURN: f32 = 1.6;
const BURST: (f32, f32) = (1.3, 1.4);
const GUNNER_DAMAGE: f32 = 12.0;

/// Enemies in a fight take a free gun nearby that can reach the player.
fn recruit_gunners(
    time: Res<Time>,
    mut next: Local<f32>,
    players: Query<&Transform, With<Player>>,
    mut turrets: Query<(Entity, &mut Turret)>,
    npcs: Query<(Entity, &Transform, &crate::npc::Npc, &Health), (Without<Gunner>, Without<crate::npc::Riding>)>,
    mut commands: Commands,
) {
    if time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 0.5;
    let Ok(pt) = players.single() else { return };
    let mut taken: Vec<Entity> = Vec::new();
    for (te, mut t) in &mut turrets {
        if t.user.is_some() || t.vehicle.is_some() || t.pivot.distance(pt.translation) > 150.0 {
            continue;
        }
        let arcs = t.arcs(te);
        let to = pt.translation - t.pivot;
        let yaw = (-to.x).atan2(-to.z);
        if (arcs.clamp(yaw, 0.0).0 - yaw).abs() > 0.05 && arcs.arc < PI - 1e-3 {
            continue;
        }
        let spot = t.gunner();
        let pick = npcs
            .iter()
            .filter(|(e, nt, n, h)| {
                !h.dead
                    && !taken.contains(e)
                    && n.hostile()
                    && matches!(n.state, crate::npc::NpcState::Combat | crate::npc::NpcState::Search)
                    && nt.translation.distance(spot) < RECRUIT
            })
            .min_by(|a, b| a.1.translation.distance(spot).total_cmp(&b.1.translation.distance(spot)));
        if let Some((e, _, _, _)) = pick {
            taken.push(e);
            t.user = Some(e);
            commands.entity(e).insert(Gunner { turret: te, on: false, burst: 0.0, cooldown: 0.0, out_of_arc: 0.0, riding: false, engaged: 0.0, clips: None });
        }
    }
}

/// Gunners run to the gun, then swing it onto the player and fire bursts;
/// they get off when the player is too close or out of its reach for long,
/// or the fight is over.
#[allow(clippy::too_many_arguments)]
fn drive_gunners(
    time: Res<Time>,
    spatial: avian3d::prelude::SpatialQuery,
    fx: Option<Res<crate::weapons::EffectAssets>>,
    mut rng: ResMut<crate::weapons::Rng>,
    players: Query<(Entity, &Transform), With<Player>>,
    mut turrets: Query<&mut Turret>,
    mut gunners: Query<(Entity, &mut Gunner, &mut Transform, &mut crate::npc::Npc, &Health), Without<Player>>,
    mut anims: Query<&mut crate::characters::AnimRequest>,
    (children, mut held): (Query<&Children>, Query<&mut Visibility, With<crate::characters::HeldWeapon>>),
    (mut rigs, gunner_clip, clips): (Query<&mut crate::characters::CharacterRig>, Option<Res<GunnerClip>>, Res<Assets<crate::cache_assets::Fc3Clip>>),
    (mut damage, mut noises, mut loops): (MessageWriter<crate::combat::DamageEvent>, MessageWriter<crate::npc::Noise>, MessageWriter<crate::sound::LoopSfx>),
    mut shot_fx: crate::particles::ShotFx,
    mut commands: Commands,
) {
    use avian3d::prelude::SpatialQueryFilter;
    let Some(fx) = fx else { return };
    let Ok((pe, pt)) = players.single() else { return };
    let dt = time.delta_secs();
    for (e, mut g, mut nt, mut npc, h) in &mut gunners {
        let mut show_weapon = |shown: bool| {
            for c in npc.rig.into_iter().flat_map(|r| children.iter_descendants(r)) {
                if let Ok(mut v) = held.get_mut(c) {
                    *v = if shown { Visibility::Inherited } else { Visibility::Hidden };
                }
            }
        };
        // Back to its own clips when it gets off.
        let mut restore = |g: &mut Gunner| {
            if let (Some(set), Some(mut r)) = (g.clips.take(), npc.rig.and_then(|r| rigs.get_mut(r).ok())) {
                r.set_clips(set);
            }
        };
        let Ok(mut t) = turrets.get_mut(g.turret) else {
            commands.entity(e).remove::<Gunner>();
            show_weapon(true);
            restore(&mut g);
            continue;
        };
        let target = pt.translation + Vec3::Y * 0.3;
        let to = target - t.pivot;
        let want_yaw = (-to.x).atan2(-to.z);
        let want_pitch = (to.y / Vec2::new(to.x, to.z).length().max(0.1)).atan();
        let arcs = t.arcs(g.turret);
        let (yaw, pitch) = arcs.clamp(want_yaw, want_pitch);
        let reachable = wrap(yaw - want_yaw).abs() < 0.05 && (pitch - want_pitch).abs() < 0.05;
        g.out_of_arc = if reachable { 0.0 } else { g.out_of_arc + dt };
        let close = pt.translation.distance(nt.translation) < TOO_CLOSE;
        let calm = matches!(npc.state, crate::npc::NpcState::Idle);
        // A crewman stays on his vehicle's gun until he's killed.
        let give_up = !g.riding && (close || calm || g.out_of_arc > 4.0);
        if h.dead || give_up {
            t.user = None;
            commands.entity(e).remove::<Gunner>();
            if !h.dead {
                show_weapon(true);
                restore(&mut g);
            }
            continue;
        }
        let rig = npc.rig;
        let rig_anim = |kind, rate, anims: &mut Query<&mut crate::characters::AnimRequest>| {
            if let Some(mut a) = rig.and_then(|r| anims.get_mut(r).ok()) {
                *a = crate::characters::AnimRequest { kind, rate };
            }
        };
        if !g.on {
            // Run to the gun (an NPC's origin is half its height up).
            let spot = t.gunner() + Vec3::Y * HALF_HEIGHT;
            let flat = Vec3::new(spot.x - nt.translation.x, 0.0, spot.z - nt.translation.z);
            if flat.length() < 0.35 {
                g.on = true;
                show_weapon(false);
            } else {
                let step = flat.normalize() * (4.5 * dt).min(flat.length());
                nt.translation += step;
                nt.translation.y += (spot.y - nt.translation.y) * (dt * 4.0).min(1.0) * if flat.length() < 3.0 { 1.0 } else { 0.0 };
                npc.yaw = (-flat.x).atan2(-flat.z);
                nt.rotation = Quat::from_rotation_y(npc.yaw);
                rig_anim(crate::characters::AnimKind::Run, 1.0, &mut anims);
                continue;
            }
        }
        // On the gun: swing it round onto the player.
        let turn = GUNNER_TURN * dt;
        t.yaw += wrap(yaw - t.yaw).clamp(-turn, turn);
        t.pitch += (pitch - t.pitch).clamp(-turn, turn);
        nt.translation = t.gunner() + Vec3::Y * HALF_HEIGHT;
        npc.yaw = t.yaw;
        npc.velocity = Vec3::ZERO;
        nt.rotation = Quat::from_rotation_y(t.yaw);
        // Hands on the grips: the gunner clip held at the frame for the
        // gun's pitch, in place of its aim.
        let posed = match (gunner_clip.as_ref(), rig.and_then(|r| rigs.get_mut(r).ok())) {
            (Some(gc), Some(mut r)) => {
                if g.clips.is_none() {
                    g.clips = Some(r.clips().clone());
                    let mut set = crate::characters::ClipSet::clone(r.clips());
                    set.clips.insert(crate::characters::AnimKind::Aim, gc.0.clone());
                    r.set_clips(std::sync::Arc::new(set));
                }
                let duration = clips.get(&gc.0).map(|c| c.0.duration).unwrap_or(3.333);
                let f = ((t.pitch - PITCH.0.to_radians()) / (PITCH.1 - PITCH.0).to_radians()).clamp(0.0, 1.0);
                if r.playing().0 == crate::characters::AnimKind::Aim {
                    r.seek(f * duration);
                }
                true
            }
            _ => false,
        };
        rig_anim(crate::characters::AnimKind::Aim, if posed { 0.0 } else { 1.0 }, &mut anims);
        // Bursts while it bears on what it can see (from the gun: riders
        // don't look around otherwise).
        let on_target = wrap(want_yaw - t.yaw).abs() < 0.06 && (want_pitch - t.pitch).abs() < 0.06;
        let eye = t.pivot + Vec3::Y * 0.3;
        let sight = target - eye;
        let mut skip = vec![e];
        skip.extend(t.vehicle.map(|(v, b, _, _)| [v, b]).into_iter().flatten());
        let sees = !calm
            && sight.length() < 150.0
            && Dir3::new(sight).ok().is_some_and(|d| {
                spatial
                    .cast_ray(eye, d, sight.length(), true, &SpatialQueryFilter::from_excluded_entities(skip.clone()))
                    .is_none_or(|h| h.entity == pe || h.distance > sight.length() - 0.6)
            });
        g.burst -= dt;
        if g.burst < -BURST.1 {
            g.burst = BURST.0;
        }
        g.cooldown -= dt;
        g.engaged = if sees { g.engaged + dt } else { 0.0 };
        if !(sees && on_target && g.burst > 0.0 && g.cooldown <= 0.0) {
            continue;
        }
        g.cooldown = 0.1;
        let muzzle = t.muzzle();
        let dist = muzzle.distance(target);
        // Loose at first, and a heavy gun's rounds scatter: about a third hit
        // at 30 m once it's settled.
        let settle = (g.engaged / 2.5).clamp(0.25, 1.0);
        let hit_chance = (npc.accuracy * 0.6 * settle * (1.0 - dist / 160.0)).clamp(0.03, 0.35);
        let aim = if rng.f32() < hit_chance {
            target + Vec3::Y * rng.range(-0.3, 0.4)
        } else {
            target + Vec3::new(rng.range(-1.5, 1.5), rng.range(-0.8, 1.2), rng.range(-1.5, 1.5)) * (1.0 + dist / 50.0)
        };
        let dir = (aim - muzzle).normalize_or(t.forward());
        let Ok(d3) = Dir3::new(dir) else { continue };
        let filter = SpatialQueryFilter::from_excluded_entities(skip);
        let hit = spatial.cast_ray(muzzle, d3, 400.0, true, &filter);
        let end = hit.map(|h| muzzle + dir * h.distance).unwrap_or(muzzle + dir * 400.0);
        match hit {
            Some(h) if h.entity == pe => {
                damage.write(crate::combat::DamageEvent {
                    target: pe,
                    amount: GUNNER_DAMAGE,
                    kind: crate::combat::DamageKind::Bullet,
                    source: Some(muzzle),
                    instigator: Some(e),
                    headshot: false,
                    hit: crate::combat::Hit::Whole,
                });
            }
            _ if end.distance(pt.translation) < 50.0 => shot_fx.bullet_end(&mut rng, muzzle, dir, 400.0, hit.map(|h| (h.entity, h.distance, h.normal))),
            _ => {}
        }
        let len = muzzle.distance(end);
        if len > 1.0 && rng.f32() < 0.4 {
            commands.spawn((
                Mesh3d(fx.tracer_mesh.clone()),
                MeshMaterial3d(fx.tracer_mat.clone()),
                Transform::from_translation((muzzle + end) * 0.5).looking_at(end, Vec3::Y).with_scale(Vec3::new(1.0, 1.0, len)),
                crate::weapons::Effect { age: 0.0, life: 0.05, grow: 0.0, light: false },
                bevy::light::NotShadowCaster,
            ));
        }
        crate::weapons::flash_3p(&mut shot_fx, &mut rng, KINDS[t.kind].weapon(), muzzle, dir, 1.5);
        noises.write(crate::npc::Noise { at: muzzle, radius: 120.0, kind: crate::npc::NoiseKind::Threatening, aside: false });
        loops.write(crate::sound::LoopSfx { key: g.turret.to_bits(), event: crate::sound::ev::LMG_LOOP, at: Some(muzzle), volume: 1.0, ..default() });
    }
}

/// `FC3_GUNNER_TEST` (try with `FC3_GOD`): stands the player 35 m in front of
/// the nearest gun and sends a pirate after them from beside it, logging
/// whether it gets on the gun and what it does with it.
#[allow(clippy::too_many_arguments)]
fn gunner_test(
    time: Res<Time>,
    mut state: Local<(u32, f32)>,
    heights: Res<crate::terrain::Heightfield>,
    mut players: Query<(&mut Player, &mut Transform, &Health), Without<Turret>>,
    turrets: Query<&Turret>,
    gunners: Query<&Gunner>,
    mut spawns: MessageWriter<crate::npc::SpawnEnemy>,
) {
    if std::env::var("FC3_GUNNER_TEST").is_err() {
        return;
    }
    let Ok((mut p, mut pt, h)) = players.single_mut() else { return };
    if p.spawning {
        return;
    }
    let (step, last) = &mut *state;
    let now = time.elapsed_secs();
    let Some(t) = turrets.iter().filter(|t| t.vehicle.is_none()).min_by(|a, b| a.pivot.distance(pt.translation).total_cmp(&b.pivot.distance(pt.translation)))
    else {
        return;
    };
    if *step == 0 {
        *step = 1;
        let ahead = Vec3::new(-t.base_yaw.sin(), 0.0, -t.base_yaw.cos());
        // The highest ground in front (turrets often watch the water).
        let spots = (-4..=4).flat_map(|k| [25.0, 35.0].map(|d| Quat::from_rotation_y(k as f32 * 0.25) * ahead * d)).map(|o| t.pivot + o);
        let Some(mut at) = spots.filter_map(|s| heights.height_at(s).map(|h| Vec3::new(s.x, h, s.z))).max_by(|a, b| a.y.total_cmp(&b.y)) else { return };
        at.y += HALF_HEIGHT;
        // "stay": where `--pos` put the player.
        if std::env::var("FC3_GUNNER_TEST").as_deref() == Ok("stay") {
            at = pt.translation;
        }
        pt.translation = at;
        let back = t.pivot - at;
        p.yaw = (-back.x).atan2(-back.z);
        let side = Vec3::new(ahead.z, 0.0, -ahead.x) * 5.0;
        let mut from = t.gunner() + side;
        from.y = heights.height_at(from).unwrap_or(from.y);
        spawns.write(crate::npc::SpawnEnemy { at: from, hunting: Some(at), garrison: None, faction: crate::npc::Faction::Pirate, role: None, ride: None });
        info!("gunner-test: player at {:.0?}, pirate from {:.0?}", at, from);
        *last = now;
        return;
    }
    // Keep watching the gun.
    if std::env::var("FC3_GUNNER_TEST").as_deref() == Ok("stay") {
        let to = t.pivot - pt.translation;
        p.yaw = (-to.x).atan2(-to.z);
        p.pitch = (to.y / Vec2::new(to.x, to.z).length().max(0.1)).atan();
    }
    if now - *last >= 1.0 {
        *last = now;
        let g = gunners.iter().next();
        info!(
            "gunner-test: gunner {} on {} turret yaw {:.0} (base {:.0}) pitch {:.0} player health {:.0}",
            g.is_some(),
            g.is_some_and(|g| g.on),
            t.yaw.to_degrees(),
            t.base_yaw.to_degrees(),
            t.pitch.to_degrees(),
            h.current
        );
    }
}

/// Has its gun.
#[derive(Component)]
struct Gunned;

/// Armed vehicles get their gun.
fn arm_vehicles(
    vehicles: Query<(Entity, &crate::vehicles::ArmedVehicle, &crate::vehicles::Vehicle, &Transform), Without<Gunned>>,
    players: Query<&Inventory, With<Player>>,
    models: Res<Assets<Fc3Model>>,
    mut commands: Commands,
) {
    let Ok(inv) = players.single() else { return };
    for (ve, armed, v, vt) in &vehicles {
        let kind = vehicle_kind(armed.gun);
        let Some(def) = inv.defs.iter().position(|d| d.name == KINDS[kind].gun) else { continue };
        let Some(gun) = models.get(&inv.models[def]) else { continue };
        commands.entity(ve).insert(Gunned);
        let pivot = vt.transform_point(armed.pivot);
        let fwd = vt.rotation * Vec3::NEG_Z;
        let base_yaw = (-fwd.x).atan2(-fwd.z);
        let back = armed.gunner - armed.pivot;
        let turret = Turret {
            kind,
            spawn_id: 0,
            base_yaw,
            pivot,
            back,
            yaw: base_yaw,
            pitch: 0.0,
            heat: 0.0,
            overheated: false,
            user: None,
            vehicle: Some((ve, v.body(), armed.pivot, 0.0)),
        };
        let root = commands.spawn((turret, Transform::from_translation(pivot), Visibility::Visible)).id();
        let holder = commands.spawn((Transform::from_translation(gun_offset(KINDS[kind].gun)), Visibility::Inherited, ChildOf(root))).id();
        for part in gun.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]).iter().filter(|p| p.shown(true)) {
            commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), ChildOf(holder)));
        }
    }
}

/// Vehicle guns ride along: the pivot follows the vehicle and the gun (and
/// whoever's on it) turns with it. They go when it does.
fn follow_vehicles(
    vehicles: Query<&Transform, (With<crate::vehicles::Vehicle>, Without<Turret>)>,
    mut turrets: Query<(Entity, &mut Turret)>,
    mut players: Query<(Entity, &mut Player)>,
    mut commands: Commands,
) {
    let player = players.single_mut().ok();
    let (pe, mut p) = match player {
        Some((e, p)) => (Some(e), Some(p)),
        None => (None, None),
    };
    for (te, mut t) in &mut turrets {
        let Some((ve, _, local, local_yaw)) = t.vehicle else { continue };
        let Ok(vt) = vehicles.get(ve) else {
            commands.entity(te).despawn();
            continue;
        };
        t.pivot = vt.transform_point(local);
        let fwd = vt.rotation * Vec3::NEG_Z;
        let base = (-fwd.x).atan2(-fwd.z) + local_yaw;
        let turn = wrap(base - t.base_yaw);
        t.base_yaw = base;
        t.yaw += turn;
        // A player on it turns with the vehicle.
        if t.user.is_some() && t.user == pe {
            if let Some(p) = p.as_mut() {
                p.yaw += turn;
                if let Some(arcs) = p.mounted.as_mut() {
                    arcs.base_yaw = base;
                }
            }
        }
    }
}

/// The crewman standing in the back of an armed vehicle mans its gun.
fn crew_gunners(
    mut turrets: Query<(Entity, &mut Turret)>,
    mut crew: Query<(Entity, &mut crate::npc::Riding, &Health), Without<Gunner>>,
    mut commands: Commands,
) {
    for (te, mut t) in &mut turrets {
        let Some((ve, _, local, _)) = t.vehicle else { continue };
        if t.user.is_some() {
            continue;
        }
        let Some((e, mut ride, _)) = crew.iter_mut().find(|(_, r, h)| r.vehicle == ve && r.visible && !h.dead) else { continue };
        // To the gun.
        ride.seat = local + t.back;
        t.user = Some(e);
        debug!("a crewman takes the gun on {ve:?}");
        commands.entity(e).insert(Gunner { turret: te, on: true, burst: 0.0, cooldown: 0.0, out_of_arc: 0.0, riding: true, engaged: 0.0, clips: None });
    }
}

/// `FC3_ARMED_TEST`: parks an armed technical 10 m ahead (`FC3_TD_SHOT`
/// screenshots that long in), then at 4 s gets on its gun.
#[allow(clippy::too_many_arguments)]
fn armed_test(
    time: Res<Time>,
    args: Res<crate::Args>,
    mut state: Local<(Option<f32>, usize, u32)>,
    assets: Option<Res<crate::vehicles::VehicleAssets>>,
    models: Res<Assets<Fc3Model>>,
    heights: Res<crate::terrain::Heightfield>,
    mut actions: ResMut<ActionState>,
    mut players: Query<(&mut Player, &mut Transform), Without<Turret>>,
    turrets: Query<&Turret>,
    mut commands: Commands,
) {
    if std::env::var("FC3_ARMED_TEST").is_err() {
        return;
    }
    let (Some(assets), Ok((p, mut pt))) = (assets, players.single_mut()) else { return };
    if p.spawning {
        return;
    }
    let (start, shots, step) = &mut *state;
    let kind = std::env::var("FC3_ARMED_KIND").unwrap_or_else(|_| "Technical".into());
    let Some(def) = crate::vehicles::def_named(&kind) else { return };
    let Some(model) = models.get(&assets.models[def]) else { return };
    if models.get(&assets.turret_base).is_none() || models.get(&assets.boat_ring).is_none() {
        return;
    }
    let now = time.elapsed_secs();
    let t0 = *start.get_or_insert(now);
    let secs = now - t0;
    if *step == 0 {
        *step = 1;
        let fwd = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
        let mut at = pt.translation + fwd * 10.0;
        at.y = heights.height_at(at).unwrap_or(at.y) + 1.0;
        // Side on to the player.
        let rotation = Quat::from_rotation_y(p.yaw + std::f32::consts::FRAC_PI_2);
        let v = crate::vehicles::build_vehicle(&mut commands, def, model, at, rotation, 0x5200_0000_0000_0000);
        crate::vehicles::arm_vehicle(&mut commands, v, model, &assets, &models);
        info!("armed-test: {kind} at {at:.0?}");
        return;
    }
    let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    if let (Some(&when), Some(path)) = (at.get(*shots), args.screenshot.as_ref()) {
        if secs >= when {
            let stem = path.strip_suffix(".png").unwrap_or(path);
            crate::camera::capture(&mut commands, format!("{stem}_td{shots}.png"));
            *shots += 1;
        }
    }
    if *step == 1 && secs > 4.0 {
        let near = turrets.iter().filter(|t| t.vehicle.is_some()).min_by(|a, b| a.pivot.distance(pt.translation).total_cmp(&b.pivot.distance(pt.translation)));
        if let Some(t) = near {
            if p.mounted.is_none() {
                pt.translation = t.gunner() + Vec3::Y * (HALF_HEIGHT + 0.05);
                actions.press(Action::Interact);
            } else {
                *step = 2;
                info!("armed-test: on its gun");
            }
        }
    }
}
