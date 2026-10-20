//! People and animals set alight. Fire's harm (flames underfoot, a molotov,
//! the flamethrower) catches on whoever takes it: they burn for a while,
//! wearing the game's own human torch (the mercs' `CBurnComponent`'s
//! `psFireSpineParticles`), hurt by it each second, with its start and stop
//! sounds; water puts them out with a hiss of steam
//! (`psFireWaterSmokeParticles`). A body burns on until the fire dies.
//!
//! A person alight panics: drops what they were doing and runs this way and
//! that on the game's burning move (`pawn_generic_burn`), screaming (their
//! voice's `reflex_flames`, in `barks`), and falls to a death by fire.

use std::sync::Arc;

use bevy::prelude::*;

use crate::cache_assets::Fc3Clip;
use crate::characters::{AnimKind, CharacterRig, ClipSet};
use crate::combat::{DamageEvent, DamageKind, Health, Hit};

pub struct BurningPlugin;

impl Plugin for BurningPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerAlight>().add_systems(Startup, load_clips).add_systems(
            Update,
            (burn_test, ground_fire_test, catch_fire, player_catch_fire, burn, player_burn).chain().run_if(in_state(crate::menu::GameState::Playing)),
        );
    }
}

/// The human torch and the steam of one put out in water.
pub const HUMAN_TORCH: &str = "fc3_gameplay_fire.fc3_gameplay_pawnfire.fire_humantorch";
pub const WATER_SMOKE: &str = "fc3_gameplay_fire.fc3_gameplay_pawnfire.water_smoke";
/// The burning's start and stop sounds (`sndStartBurningSoundID`,
/// `sndStopBurningSoundID`).
const START_SOUND: u32 = 0x001B_0477;
const STOP_SOUND: u32 = 0x001B_0479;

/// How long one burns (s) and what the fire does to them each second.
const BURN_SECS: f32 = 6.0;
const BURN_DAMAGE: f32 = 30.0;
/// How often (s) the fire on the player hurts him.
const PLAYER_TICK: f32 = 0.25;
/// The bone the torch burns on (the chest), and where it is on a person
/// without their skeleton in (m up from their capsule's centre). An
/// animal's burns at its body's middle.
const SPINE: &str = "Spine2";
const CHEST: f32 = 0.4;

/// The game's burning move (`pawn_generic_burn`): the run it has armed, and
/// its loop on the spot unarmed. Then the deaths by fire.
const RUN_CLIP: &str = r"graphics\characters\_common\animations\eventreactions\reactonfire\3rdge_fulbst_reactonfire_runloop_v00_+000fw_mltv.mab";
const SPOT_CLIP: &str = r"graphics\characters\_common\animations\eventreactions\reactonfire\3rdge_fulbst_reactonfire_idleloop_v00_+000fw_mltv.mab";
const DEATH_CLIPS: [&str; 3] = [
    r"graphics\characters\_common\animations\eventreactions\reactonfire\3rdge_fulbst_idle_diebyfire_v00_+000fw_noweapon.mab",
    r"graphics\characters\_common\animations\eventreactions\reactonfire\3rdge_fulbst_idle_diebyfire_v01_+000fw_noweapon.mab",
    r"graphics\characters\_common\animations\eventreactions\reactonfire\3rdge_fulbst_idle_diebyfire_v02_+000fw_noweapon.mab",
];

/// A burning person's runs: how far (m) before turning, and for how long
/// at most (s).
const PANIC_RUN: (f32, f32) = (5.0, 10.0);
const PANIC_LEG: f32 = 2.5;

#[derive(Resource)]
struct BurnClips {
    run: Handle<Fc3Clip>,
    spot: Handle<Fc3Clip>,
    deaths: Vec<Handle<Fc3Clip>>,
}

fn load_clips(mut commands: Commands, server: Res<AssetServer>) {
    let load = |p: &str| server.load(crate::cache_assets::cache_path(&fc3_formats::cache::anim_rel(p)));
    commands.insert_resource(BurnClips { run: load(RUN_CLIP), spot: load(SPOT_CLIP), deaths: DEATH_CLIPS.iter().map(|p| load(p)).collect() });
}

impl BurnClips {
    /// A person's clips while alight: the burning run for any going, its
    /// loop for standing, and one of the deaths by fire.
    fn alight(&self, set: &ClipSet, death: usize) -> ClipSet {
        let mut set = set.clone();
        for kind in [AnimKind::Walk, AnimKind::Run, AnimKind::Sprint, AnimKind::Reload] {
            set.clips.insert(kind, self.run.clone());
        }
        for kind in [AnimKind::Idle, AnimKind::Aim, AnimKind::Fire, AnimKind::Cover] {
            set.clips.insert(kind, self.spot.clone());
        }
        if let Some(d) = self.deaths.get(death % self.deaths.len().max(1)) {
            set.clips.insert(AnimKind::Death, d.clone());
        }
        set
    }

    /// Back to their own clips once put out (keeping whatever else changed
    /// on the way: a flinch).
    fn put_out(&self, set: &ClipSet, before: &ClipSet) -> Option<ClipSet> {
        let mut set = set.clone();
        let burning: Vec<AnimKind> =
            set.clips.iter().filter(|(_, c)| **c == self.run || **c == self.spot || self.deaths.contains(c)).map(|(k, _)| *k).collect();
        for kind in &burning {
            match before.clips.get(kind) {
                Some(c) => set.clips.insert(*kind, c.clone()),
                None => set.clips.remove(kind),
            };
        }
        (!burning.is_empty()).then_some(set)
    }
}

/// Alight: seconds left, the torch on them, who set it, the wait to the
/// next hurt, and a person's own clips (to go back to).
#[derive(Component)]
pub struct Aflame {
    left: f32,
    torch: Option<Entity>,
    instigator: Option<Entity>,
    tick: f32,
    clips: Option<Arc<ClipSet>>,
}

/// Fire's harm sets those it touches alight (people and animals; the player
/// takes it as his own harm). A character catches fire once fire has done it
/// its counters' share within their time (`Burn`: a merc 25 in 8 s, a
/// villager 50); an animal at once.
#[allow(clippy::too_many_arguments)]
fn catch_fire(
    time: Res<Time>,
    table: Res<crate::combat::DamageTable>,
    mut taken: Local<std::collections::HashMap<Entity, Vec<(f32, f32)>>>,
    mut hits: MessageReader<DamageEvent>,
    targets: Query<
        (&Health, &GlobalTransform, Has<Aflame>, Option<&crate::npc::Npc>, Option<&crate::combat::Counters>),
        Or<(With<crate::npc::Npc>, With<crate::wildlife::WildAnimal>)>,
    >,
    burn_clips: Option<Res<BurnClips>>,
    mut rigs: Query<&mut CharacterRig>,
    mut rng: ResMut<crate::weapons::Rng>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut commands: Commands,
) {
    let now = time.elapsed_secs();
    taken.retain(|_, l| l.last().is_some_and(|(t, _)| now - t < 30.0));
    // (`FC3_BURN_TEST`: any touch of fire sets alight.)
    let test = std::env::var_os("FC3_BURN_TEST").is_some();
    let mut lit = Vec::new();
    for hit in hits.read().filter(|h| h.kind == DamageKind::Fire) {
        let Ok((health, at, burning, npc, counters)) = targets.get(hit.target) else { continue };
        if burning || health.dead || lit.contains(&hit.target) {
            continue;
        }
        // Fire's harm to it (as its counters take it) adds up over their time.
        let catch = npc.and(counters.and_then(|c| c.v.burn).or_else(|| table.0.as_ref().and_then(|d| d.others_burn))).filter(|_| !test);
        if let Some(c) = catch {
            let share = counters.map_or(1.0, |c| c.v.body * c.v.harm(fc3_formats::counters::Harm::Burn));
            let list = taken.entry(hit.target).or_default();
            list.retain(|(t, _)| now - t < c.window);
            list.push((now, hit.amount * share));
            if list.iter().map(|(_, a)| a).sum::<f32>() < c.threshold {
                continue;
            }
            taken.remove(&hit.target);
        }
        lit.push(hit.target);
        debug!("{} catches fire", hit.target);
        let torch = crate::authored_fx::available(HUMAN_TORCH)
            .then(|| commands.spawn((Transform::from_xyz(0.0, CHEST, 0.0), crate::authored_fx::AuthoredFx(HUMAN_TORCH), ChildOf(hit.target))).id());
        // A person takes up the burning move.
        let clips = match (npc.and_then(|n| n.rig).and_then(|r| rigs.get_mut(r).ok()), burn_clips.as_ref()) {
            (Some(mut rig), Some(burn)) => {
                let before = rig.clips().clone();
                rig.set_clips_blended(Arc::new(burn.alight(&before, rng.next_u32() as usize)));
                Some(before)
            }
            _ => None,
        };
        commands.entity(hit.target).insert(Aflame { left: BURN_SECS, torch, instigator: hit.instigator, tick: 1.0, clips });
        sfx.write(crate::sound::PlaySfx { event: START_SOUND, at: Some(at.translation()), volume: 1.0 });
    }
}

/// The fire burns on: it hurts each second, and goes out when it's had its
/// time or in water. The torch keeps to the chest, upright, as the body
/// moves and falls; a person runs about while it burns.
#[allow(clippy::too_many_arguments)]
fn burn(
    time: Res<Time>,
    heights: Res<crate::terrain::Heightfield>,
    burn_clips: Option<Res<BurnClips>>,
    mut rng: ResMut<crate::weapons::Rng>,
    mut burning: Query<(Entity, &mut Aflame, &GlobalTransform, &Health, Option<&mut crate::npc::Npc>)>,
    mut rigs: Query<&mut CharacterRig>,
    joints: Query<&GlobalTransform, Without<Aflame>>,
    mut torches: Query<&mut Transform, Without<Aflame>>,
    mut damage: MessageWriter<DamageEvent>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let spine = fc3_formats::hash::name_hash(SPINE);
    for (e, mut fire, at, health, mut npc) in &mut burning {
        let p = at.translation();
        let rig = npc.as_ref().and_then(|n| n.rig);
        let chest = match npc {
            Some(_) => rig.and_then(|r| rigs.get(r).ok()).and_then(|r| r.joint(spine)).and_then(|(_, j)| joints.get(j).ok()).map(|j| j.translation()),
            None => Some(p),
        }
        .unwrap_or(p + Vec3::Y * CHEST);
        if let Some(mut t) = fire.torch.and_then(|torch| torches.get_mut(torch).ok()) {
            t.translation = at.affine().inverse().transform_point3(chest);
        }
        let drowned = crate::water::surface_at(p, &heights) > chest.y;
        fire.left -= dt;
        fire.tick -= dt;
        // (Not as it goes out: the hurt would light them again.)
        if fire.tick <= 0.0 && fire.left > 0.0 && !health.dead && !drowned {
            fire.tick += 1.0;
            damage.write(DamageEvent {
                target: e,
                amount: BURN_DAMAGE,
                kind: DamageKind::Fire,
                source: Some(p),
                instigator: fire.instigator,
                headshot: false,
                hit: Hit::Whole,
            });
        }
        if fire.left > 0.0 && !drowned {
            // Running blind, this way and that, onto dry ground.
            if let Some(npc) = npc.as_mut().filter(|n| n.dodge.is_none() && !health.dead) {
                let dry = |s: &Vec3| heights.height_at(*s).is_some_and(|g| g > crate::water::surface_at(*s, &heights) + 0.3);
                let spot = (0..4)
                    .map(|_| {
                        let a = rng.range(0.0, std::f32::consts::TAU);
                        p + Vec3::new(a.cos(), 0.0, a.sin()) * rng.range(PANIC_RUN.0, PANIC_RUN.1)
                    })
                    .find(dry);
                if let Some(spot) = spot {
                    npc.dodge = Some((spot, PANIC_LEG));
                }
            }
            continue;
        }
        if let Some(torch) = fire.torch {
            commands.entity(torch).despawn();
        }
        if drowned {
            crate::authored_fx::play(&mut commands, WATER_SMOKE, chest);
        }
        // Put out alive: their own moves again. (The dead keep their fall.)
        if let (Some(before), Some(burn), false) = (fire.clips.take(), burn_clips.as_ref(), health.dead) {
            if let Some(mut r) = rig.and_then(|r| rigs.get_mut(r).ok()) {
                if let Some(set) = burn.put_out(r.clips(), &before) {
                    r.set_clips_blended(Arc::new(set));
                }
            }
        }
        sfx.write(crate::sound::PlaySfx { event: STOP_SOUND, at: Some(p), volume: 1.0 });
        debug!("{e}'s fire goes out{}", if drowned { " in water" } else { "" });
        commands.entity(e).remove::<Aflame>();
    }
}

/// The player alight (`StatsAndCounters.Health`'s burn): when fire last
/// touched him and who set it, the wait to the next hurt, and the fire done
/// him lately.
#[derive(Resource, Default)]
pub struct PlayerAlight {
    pub on: bool,
    last: f32,
    tick: f32,
    instigator: Option<Entity>,
    taken: Vec<(f32, f32)>,
}

/// The player catches fire once fire has done him the counters' share
/// within their time (45 in 8 s), unless the fireproof syringe keeps it off.
fn player_catch_fire(
    time: Res<Time>,
    table: Res<crate::combat::DamageTable>,
    (settings, boosts): (Res<crate::settings::Settings>, Res<crate::syringes::Boosts>),
    mut hits: MessageReader<DamageEvent>,
    players: Query<(Entity, &GlobalTransform), With<crate::player::Player>>,
    mut alight: ResMut<PlayerAlight>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let (Some(burn), Ok((pe, at))) = (table.0.as_ref().map(|d| (d.player.burn, d.player.burn_taken)), players.single()) else {
        hits.clear();
        return;
    };
    let (Some(b), taken) = burn else {
        hits.clear();
        return;
    };
    let now = time.elapsed_secs();
    let share = taken[settings.difficulty.index()];
    // (Its own hurt, from no place, isn't fire touching him.)
    for hit in hits.read().filter(|h| h.kind == DamageKind::Fire && h.target == pe && h.source.is_some() && h.amount > 0.0) {
        if boosts.active(crate::syringes::Syringe::Fireproof) {
            continue;
        }
        alight.last = now;
        alight.instigator = hit.instigator.or(alight.instigator);
        if alight.on {
            continue;
        }
        alight.taken.retain(|(t, _)| now - t < b.window);
        alight.taken.push((now, hit.amount * share));
        if alight.taken.iter().map(|(_, a)| a).sum::<f32>() >= b.threshold {
            debug!("the player catches fire");
            alight.on = true;
            alight.tick = PLAYER_TICK;
            alight.taken.clear();
            sfx.write(crate::sound::PlaySfx { event: START_SOUND, at: Some(at.translation()), volume: 1.0 });
        }
    }
}

/// The fire on the player hurts him its rate a second until he has been out
/// of the flames for his burn's expiry (2 s), or water or death puts it out.
fn player_burn(
    time: Res<Time>,
    table: Res<crate::combat::DamageTable>,
    heights: Res<crate::terrain::Heightfield>,
    players: Query<(Entity, &crate::player::Player, &Health, &GlobalTransform)>,
    mut alight: ResMut<PlayerAlight>,
    mut damage: MessageWriter<DamageEvent>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    if !alight.on {
        return;
    }
    let (Some(b), Ok((pe, p, h, at))) = (table.0.as_ref().and_then(|d| d.player.burn), players.single()) else { return };
    let now = time.elapsed_secs();
    let pos = at.translation();
    let drowned = p.swimming || crate::water::surface_at(pos, &heights) > pos.y + CHEST;
    if h.dead || drowned || now - alight.last > b.expiry.max(PLAYER_TICK) {
        debug!("the player's fire goes out{}", if drowned { " in water" } else { "" });
        alight.on = false;
        alight.instigator = None;
        sfx.write(crate::sound::PlaySfx { event: STOP_SOUND, at: Some(pos), volume: 1.0 });
        return;
    }
    alight.tick -= time.delta_secs();
    if alight.tick <= 0.0 {
        alight.tick += PLAYER_TICK;
        damage.write(DamageEvent {
            target: pe,
            amount: b.rate * PLAYER_TICK,
            kind: DamageKind::Fire,
            source: None,
            instigator: alight.instigator,
            headshot: false,
            hit: Hit::Whole,
        });
    }
}

/// Test: `FC3_GROUND_FIRE_TEST=<seconds>` sets the ground alight under the
/// player and every character within 40 m that long after the start (real
/// time), three times 2 s apart, and logs the player's health each second
/// for 20 s.
fn ground_fire_test(
    time: Res<Time<Real>>,
    mut state: Local<(u32, f32)>,
    players: Query<(&GlobalTransform, &Health), With<crate::player::Player>>,
    npcs: Query<&GlobalTransform, With<crate::npc::Npc>>,
    alight: Res<PlayerAlight>,
    mut ignite: MessageWriter<crate::fire::Ignite>,
) {
    let Some(at) = std::env::var("FC3_GROUND_FIRE_TEST").ok().and_then(|s| s.parse::<f32>().ok()) else { return };
    let Ok((player, h)) = players.single() else { return };
    let now = time.elapsed_secs();
    let (lit, logged) = &mut *state;
    if *lit < 3 && now >= at + *lit as f32 * 2.0 {
        *lit += 1;
        let me = player.translation();
        for p in std::iter::once(me).chain(npcs.iter().map(|t| t.translation()).filter(|p| p.distance(me) < 40.0)) {
            ignite.write(crate::fire::Ignite { at: p, radius: 3.0, instigator: None, damage: None });
        }
    }
    if now >= at && now < at + 20.0 && now - *logged >= 1.0 {
        *logged = now;
        info!("ground-fire test: {:.0} s, player health {:.1} of {:.0}, alight {}", now - at, h.current, h.max, alight.on);
    }
}

/// Test: `FC3_BURN_TEST=<seconds>` sets everyone (and every animal) within
/// 40 m of the player alight that long after the start (real time).
fn burn_test(
    time: Res<Time<Real>>,
    mut done: Local<bool>,
    players: Query<&GlobalTransform, With<crate::player::Player>>,
    targets: Query<(Entity, &GlobalTransform), Or<(With<crate::npc::Npc>, With<crate::wildlife::WildAnimal>)>>,
    mut damage: MessageWriter<DamageEvent>,
) {
    if *done {
        return;
    }
    let Some(at) = std::env::var("FC3_BURN_TEST").ok().and_then(|s| s.parse::<f32>().ok()) else {
        *done = true;
        return;
    };
    let Ok(player) = players.single() else { return };
    if time.elapsed_secs() < at {
        return;
    }
    *done = true;
    for (e, t) in &targets {
        if t.translation().distance(player.translation()) < 40.0 {
            damage.write(DamageEvent {
                target: e,
                amount: 1.0,
                kind: DamageKind::Fire,
                source: Some(t.translation()),
                instigator: None,
                headshot: false,
                hit: Hit::Whole,
            });
        }
    }
}
