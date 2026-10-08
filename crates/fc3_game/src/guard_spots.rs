//! Guard spots: the world's places for idle guards to stand at and do
//! something (its `SimpleAnimation` points): lean on a wall, smoke, look
//! over a fence or round a corner, check their gear, sit on the ground, take
//! a leak, or just stand easy or watchful. An idle guard posted near a free
//! spot takes it, faces its way and plays its clip; anything that puts him on
//! guard sends him back to the usual.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;
use fc3_formats::cache;
use fc3_formats::hash::name_hash;

use crate::cache_assets::{cache_path, Fc3Clip};
use crate::characters::{AnimKind, CharacterRig, ClipSet, HeldWeapon, CLIP_ROOT};
use crate::combat::Health;
use crate::npc::{Faction, Npc, NpcState};
use crate::CacheInfo;

pub struct GuardSpotsPlugin;

impl Plugin for GuardSpotsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnExit(crate::menu::GameState::Loading), load_spots)
            .add_systems(Update, (take_spots, leave_spots, spot_test).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// A guard takes a free spot this near his post (m, across; and up or down).
const REACH: f32 = 18.0;
const REACH_UP: f32 = 3.0;

struct Spot {
    kind: &'static str,
    at: Vec3,
    yaw: f32,
}

#[derive(Resource, Default)]
struct GuardSpots {
    list: Vec<Spot>,
    /// Who holds each taken spot.
    taken: HashMap<usize, Entity>,
}

/// A guard posted at a spot: which, his clips from before, and whether the
/// spot's clip has his weapon put away.
#[derive(Component)]
pub struct AtSpot {
    index: usize,
    base: Arc<ClipSet>,
    unarmed: bool,
}

/// Looked for a spot once (taken or not).
#[derive(Component)]
struct SpotChecked;

/// The clips for a spot (under `animations\`), by what's done there; the
/// stances come in pirate and privateer builds.
fn clips(kind: &str, privateer: bool) -> &'static [&'static str] {
    match (kind, privateer) {
        ("relax", false) => &[
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\pirate\3rdge_fulbst_relaxguard_pirate_v00_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\pirate\3rdge_fulbst_relaxguard_pirate_v02_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\pirate\3rdge_fulbst_relaxguard_pirate_v03_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\pirate\3rdge_fulbst_relaxguard_pirate_v04_loop_ak74.mab",
        ],
        ("relax", true) => &[
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\privateer\3rdpriv_fulbst_relaxguard_privateer_v00_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\privateer\3rdpriv_fulbst_relaxguard_privateer_v02_loop_ak74.mab",
        ],
        ("cautious", false) => &[
            r"smart_terrain_points\fighters\behavior_specific\cautious\cautiousguard\pirate\3rdge_fulbst_cautiousguard_pirate_v00_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\cautious\cautiousguard\pirate\3rdge_fulbst_cautiousguard_pirate_v01_loop_ak74.mab",
        ],
        ("cautious", true) => &[
            r"smart_terrain_points\fighters\behavior_specific\cautious\cautiousguard\privateer\3rdpriv_fulbst_cautiousguard_privateer_v00_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\cautious\cautiousguard\privateer\3rdpriv_fulbst_cautiousguard_privateer_v01_loop_ak74.mab",
        ],
        ("smoke", _) => &[
            r"smart_terrain_points\fighters\faction_specific\generic\simple_animation\stand\smoke\3rdge_fulbst_idlerelax_smoke_loop_v00_+000fw_nowep.mab",
            r"smart_terrain_points\fighters\faction_specific\generic\simple_animation\stand\smoke\3rdge_fulbst_idlerelax_smoke_loop_v01_+000fw_nowep.mab",
        ],
        ("piss", _) => &[
            r"contextual_actions\patrol\pissing\3rdge_fulbst_pissingloop_v00_+000fw_ak74.mab",
            r"contextual_actions\patrol\pissing\3rdge_fulbst_pissingloop_v01_+000fw_ak74.mab",
        ],
        ("lean", _) => &[r"contextual_actions\patrol\leanonwall\ak74\3rdge_fulbst_idlerelaxleanonwall_v00_ak74.mab"],
        ("lookover", _) => {
            &[r"smart_terrain_points\fighters\behavior_specific\cautious\lookoverobject\3rdge_fulbst_cautious_lookoverobject_v00_+000fw_ak74.mab"]
        }
        ("corner_left", _) => {
            &[r"smart_terrain_points\fighters\behavior_specific\cautious\lookaroundcorner\3rdge_fulbst_lookaroundcorner_left_v00_+000fw_ak74.mab"]
        }
        ("corner_right", _) => {
            &[r"smart_terrain_points\fighters\behavior_specific\cautious\lookaroundcorner\3rdge_fulbst_lookaroundcorner_right_v00_+000fw_ak74.mab"]
        }
        ("gear", _) => &[r"smart_terrain_points\fighters\theme_specific\weapons_handling\verifygear\3rdge_fulbst_verifygear_merc_000fw_ak74.mab"],
        ("sit", _) => {
            &[r"smart_terrain_points\fighters\faction_specific\generic\simple_animation\ground\3rdge_fulbst_idlerelax_sitground_loop_+000fw_ak74.mab"]
        }
        ("flies", _) => &[r"smart_terrain_points\fighters\theme_specific\dead_bodies\swatflies\3rdge_fulbst_cautiousak_swatflies_v00_000fw_ak74.mab"],
        _ => &[],
    }
}

fn kind_of(s: &str) -> Option<&'static str> {
    ["relax", "cautious", "smoke", "piss", "lean", "lookover", "corner_left", "corner_right", "gear", "sit", "flies"].into_iter().find(|k| *k == s)
}

fn load_spots(mut commands: Commands, info: Res<CacheInfo>) {
    let text = std::fs::read_to_string(info.root.join(cache::GUARD_SPOTS_REL)).unwrap_or_default();
    let list: Vec<Spot> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let kind = kind_of(it.next()?)?;
            let mut v = [0.0f32; 7];
            for c in &mut v {
                *c = it.next()?.parse().ok()?;
            }
            let yaw = Quat::from_xyzw(v[3], v[4], v[5], v[6]).to_euler(EulerRot::YXZ).0;
            Some(Spot { kind, at: Vec3::new(v[0], v[1], v[2]), yaw })
        })
        .collect();
    info!("{} guard spots", list.len());
    commands.insert_resource(GuardSpots { list, taken: HashMap::new() });
}

/// Shows or hides what a rig holds in its right hand.
fn hold_weapon(rig: &CharacterRig, held: &mut Query<(&ChildOf, &mut Visibility), With<HeldWeapon>>, shown: bool) {
    if let Some((_, hand)) = rig.joint(name_hash("RightHand")) {
        for (_, mut v) in held.iter_mut().filter(|(c, _)| c.parent() == hand) {
            *v = if shown { Visibility::Inherited } else { Visibility::Hidden };
        }
    }
}

/// Idle guards (not patrolling, not heavies) take the nearest free spot by
/// their post.
#[allow(clippy::too_many_arguments)]
fn take_spots(
    time: Res<Time>,
    spots: Option<ResMut<GuardSpots>>,
    server: Res<AssetServer>,
    mut npcs: Query<(Entity, &mut Npc, &Health), (Without<AtSpot>, Without<SpotChecked>)>,
    all: Query<(), With<Npc>>,
    mut rigs: Query<&mut CharacterRig>,
    mut held: Query<(&ChildOf, &mut Visibility), With<HeldWeapon>>,
    mut tidy: Local<f32>,
    mut commands: Commands,
) {
    let Some(mut spots) = spots else { return };
    // Free the spots of guards who are gone.
    *tidy -= time.delta_secs();
    if *tidy <= 0.0 {
        *tidy = 2.0;
        spots.taken.retain(|_, e| all.get(*e).is_ok());
    }
    for (e, mut npc, h) in &mut npcs {
        let Some(rig_e) = npc.rig else {
            commands.entity(e).insert(SpotChecked);
            continue;
        };
        let Ok(mut rig) = rigs.get_mut(rig_e) else { continue };
        commands.entity(e).insert(SpotChecked);
        // Guards of the world's posts only (not reinforcements or test spawns).
        if h.dead || npc.state != NpcState::Idle || npc.role.armoured() || npc.patrols || npc.marker_id >= crate::npc::FREE_ID_BASE {
            continue;
        }
        let post = npc.post;
        let free = spots
            .list
            .iter()
            .enumerate()
            .filter(|(i, s)| !spots.taken.contains_key(i) && Vec2::new(s.at.x - post.x, s.at.z - post.z).length() < REACH && (s.at.y - post.y).abs() < REACH_UP)
            .min_by(|a, b| a.1.at.distance(post).total_cmp(&b.1.at.distance(post)))
            .map(|(i, s)| (i, s.kind, s.at, s.yaw));
        let Some((i, kind, at, yaw)) = free else { continue };
        let options = clips(kind, npc.faction == Faction::Privateer);
        let Some(path) = options.get(i % options.len().max(1)) else { continue };
        let clip: Handle<Fc3Clip> = server.load(cache_path(&cache::anim_rel(&format!("{CLIP_ROOT}{path}"))));
        let base = rig.clips().clone();
        let mut set = ClipSet { skeleton: base.skeleton.clone(), clips: base.clips.clone() };
        set.clips.insert(AnimKind::Idle, clip);
        rig.set_clips(Arc::new(set));
        let unarmed = kind == "smoke";
        if unarmed {
            hold_weapon(&rig, &mut held, false);
        }
        npc.post = at;
        npc.post_yaw = yaw;
        npc.at_spot = true;
        spots.taken.insert(i, e);
        commands.entity(e).insert(AtSpot { index: i, base, unarmed });
        // FC3_SPOT_DEBUG: who took which.
        if std::env::var("FC3_SPOT_DEBUG").is_ok() {
            info!("guard spot: {kind} at {at:.1?} facing {:.0}", yaw.to_degrees());
        }
    }
}

/// `FC3_SPOT_TEST[=kind]` (with `--noclip` near an outpost): from 25 s in,
/// stands the player a few metres in front of the nearest posted guard (of
/// that kind) and looks at him, logging what he's doing.
fn spot_test(
    time: Res<Time>,
    spots: Option<Res<GuardSpots>>,
    mut done: Local<bool>,
    guards: Query<(&Transform, &AtSpot, &Npc), Without<crate::player::Player>>,
    mut players: Query<(&mut Transform, &mut crate::player::Player)>,
) {
    let Ok(want) = std::env::var("FC3_SPOT_TEST") else { return };
    let (Some(spots), Ok((mut pt, mut p))) = (spots, players.single_mut()) else { return };
    if time.elapsed_secs() < 25.0 {
        return;
    }
    let near = guards
        .iter()
        .filter(|(_, at, _)| want.is_empty() || want == "1" || spots.list[at.index].kind == want)
        .min_by(|a, b| a.0.translation.distance(pt.translation).total_cmp(&b.0.translation.distance(pt.translation)));
    let Some((gt, at, npc)) = near else { return };
    let facing = Vec3::new(-npc.post_yaw.sin(), 0.0, -npc.post_yaw.cos());
    pt.translation = gt.translation + facing * 7.0 + Vec3::Y * 0.6;
    let to = gt.translation - pt.translation;
    p.yaw = (-to.x).atan2(-to.z);
    p.pitch = (to.y / Vec2::new(to.x, to.z).length().max(0.1)).atan();
    if !*done {
        *done = true;
        info!("spot-test: a guard at a {} spot", spots.list[at.index].kind);
    }
}

/// Put on guard (or killed), a guard leaves his spot: his own clips and
/// weapon back.
fn leave_spots(
    spots: Option<ResMut<GuardSpots>>,
    mut npcs: Query<(Entity, &mut Npc, &AtSpot, &Health)>,
    mut rigs: Query<&mut CharacterRig>,
    mut held: Query<(&ChildOf, &mut Visibility), With<HeldWeapon>>,
    mut commands: Commands,
) {
    let Some(mut spots) = spots else { return };
    for (e, mut npc, at, h) in &mut npcs {
        if !(h.dead || npc.state != NpcState::Idle || npc.awareness > 0.25) {
            continue;
        }
        if !h.dead {
            if let Some(mut rig) = npc.rig.and_then(|r| rigs.get_mut(r).ok()) {
                rig.set_clips(at.base.clone());
                if at.unarmed {
                    hold_weapon(&rig, &mut held, true);
                }
            }
        }
        npc.at_spot = false;
        spots.taken.remove(&at.index);
        commands.entity(e).remove::<AtSpot>();
    }
}
