//! The sharpshooter challenges (shooting galleries, `CShootingGallery...`).
//! A challenge keeper's script starts the gallery's gameplay when the
//! player talks to them, and hears how it went from the message it
//! broadcasts (`shooting_gallery_mini_game_finished`, `_failed`, `_quit`,
//! `_cancelled`), then stops it.
//!
//! The gameplay plays as the engine played it: the difficulty chosen and the
//! wager paid (practice is free), the challenge's base and difficulty layers
//! brought in (their bird boxes and the shooter's spot register themselves,
//! `SpawnPointRegistration`), a countdown, then the gallery's waves: birds
//! released from the boxes fly off within a frustum in front of the player,
//! turning as their difficulty has them, while the player, held at the spot
//! with the gallery's weapon and its view limits, shoots them. The kill
//! target reached, the wager comes back multiplied; the waves over without
//! it, it's lost. What the HUD shows (`acti_shooting.feu`, `original_ui`)
//! is kept in `Gallery::screen`.

use std::collections::HashMap;
use std::sync::Arc;

use avian3d::prelude::*;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use fc3_formats::cache;
use fc3_formats::gsf::{GalleryAnimal, ShootingGalleryResources, ShootingGallerySettings};

use crate::cache_assets::{cache_path, skinned_cache_path, Fc3Model, ModelSettings};
use crate::characters::{spawn_rig_with, AnimKind, AnimRequest, ClipSet};
use crate::combat::Health;
use crate::controls::{Action, ActionState};
use crate::player::{Player, ViewArc, ViewLimits, HALF_HEIGHT};
use crate::weapons::{Inventory, Rng, WeaponSlot};

pub struct ShootingGalleryPlugin;

impl Plugin for ShootingGalleryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Gallery>()
            .add_systems(PreUpdate, hold_controls.after(crate::controls::apply_action_maps))
            .add_systems(Update, (run, fly_birds).chain().after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(Update, hold_place.after(crate::player::player_move));
    }
}

/// A gallery the story starts.
pub struct Start {
    pub id: u64,
    pub name: String,
    pub params: HashMap<String, String>,
}

/// The gallery the story asks for, the spots its prefabs registered, and
/// what its HUD shows.
#[derive(Resource, Default)]
pub struct Gallery {
    pub pending: Vec<Start>,
    pub cancelled: Vec<u64>,
    /// `SpawnPointRegistration`: the spot's kind (`BirdBox`...) and its
    /// entity.
    pub spawn_points: Vec<(String, u64)>,
    /// What the HUD shows, none when no gallery runs.
    pub screen: Option<Screen>,
    /// The difficulty box's row the mouse is over (`OnDifficultyRollOver`),
    /// and whether it's over the cancel button (`OnCancelRollOver`).
    pub hovered: Option<usize>,
    pub cancel_hovered: bool,
    run: Option<Run>,
}

impl Gallery {
    /// A checkpoint reload: what runs ends.
    pub fn reset(&mut self) {
        self.pending.clear();
        if let Some(r) = &self.run {
            self.cancelled.push(r.id);
        }
    }

    /// Whether the gallery has Escape (backing out of it) rather than the
    /// pause menu.
    pub fn claims_escape(&self) -> bool {
        self.run.as_ref().is_some_and(|r| matches!(r.phase, Phase::Choosing | Phase::Result))
    }

    /// A gallery runs: the player is at it (the story's autopilot leaves
    /// him to it).
    pub fn holds_player(&self) -> bool {
        self.run.is_some()
    }
}

/// The HUD's state (`acti_shooting.feu`'s boxes).
#[derive(Clone, Debug, PartialEq)]
pub enum Screen {
    /// The difficulty box: the row chosen (0 practice, 1 easy, 2 medium,
    /// 3 hard), each row's wager, the kill target, whether the row chosen
    /// costs more than the player has, and its subtitle (a string id).
    Choosing {
        row: usize,
        wagers: [u32; 4],
        target: u32,
        short: bool,
        subtitle: u32,
    },
    Ready,
    /// Seconds left to count.
    Countdown(u32),
    /// Birds killed, the target, the wave's seconds left, and the seconds
    /// before the next wave when one is coming.
    Playing {
        kills: u32,
        target: u32,
        left: f32,
        next_wave: Option<f32>,
    },
    /// How it went, the birds killed, the target and the money won.
    Result {
        success: bool,
        kills: u32,
        target: u32,
        won: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Choosing,
    Ready,
    Countdown,
    Wave,
    Result,
}

struct Run {
    id: u64,
    name: String,
    settings: ShootingGallerySettings,
    animal: GalleryAnimal,
    countdown_sound: u32,
    ready_sound: u32,
    /// The weapon's archetype.
    weapon: String,
    /// The messages for done, failed, quit and cancelled.
    messages: [String; 4],
    /// The challenge's block, its base layer and its difficulties' layers
    /// (names).
    block: String,
    base_layer: String,
    difficulty_layers: [String; 3],
    /// The layers brought in (ids), taken out at the end.
    layers: Vec<u64>,
    phase: Phase,
    /// Seconds in this phase.
    t: f32,
    row: usize,
    wager: u32,
    kills: u32,
    wave: usize,
    /// The wave's birds yet to come, and when the next does (s into it).
    to_come: u32,
    next_bird: f32,
    /// Where the player stands and faces (its yaw), once placed, and how
    /// many spots had registered then (placed again as more do).
    spot: Option<(Vec3, f32)>,
    placed_with: usize,
    lent: Option<usize>,
    /// The weapon looked for (once).
    armed: bool,
    /// The phase last captured and how often (`FC3_GALLERY_SHOTS`).
    shot: Option<(Phase, u32)>,
    /// The birds' model and clips (loading).
    model: Handle<Fc3Model>,
    set: Arc<ClipSet>,
    success: bool,
    won: u32,
    /// The result's message sent (the script stops the gameplay on it).
    told: bool,
}

impl Run {
    /// Nothing (only to move a run out and back in, `std::mem::replace`).
    fn placeholder() -> Self {
        Run {
            id: 0,
            name: String::new(),
            settings: ShootingGallerySettings::default(),
            animal: GalleryAnimal::default(),
            countdown_sound: 0,
            ready_sound: 0,
            weapon: String::new(),
            messages: Default::default(),
            block: String::new(),
            base_layer: String::new(),
            difficulty_layers: Default::default(),
            layers: Vec::new(),
            phase: Phase::Choosing,
            t: 0.0,
            row: 0,
            wager: 0,
            kills: 0,
            wave: 0,
            to_come: 0,
            next_bird: 0.0,
            spot: None,
            placed_with: 0,
            lent: None,
            armed: false,
            shot: None,
            model: Handle::default(),
            set: Arc::new(ClipSet { skeleton: Handle::default(), clips: HashMap::new() }),
            success: false,
            won: 0,
            told: false,
        }
    }
}

/// A bird of the gallery's: its way, the turns it has left and when the
/// next comes, and its fall once shot.
#[derive(Component)]
pub struct GalleryBird {
    run: u64,
    velocity: Vec3,
    turns: u32,
    next_turn: f32,
    turning: Option<(Vec3, Vec3, f32)>,
    rig: Option<Entity>,
    counted: bool,
    falling: f32,
    age: f32,
}

/// Seconds the "get ready" box shows, the countdown counts, and the result
/// shows before a test leaves it.
const READY: f32 = 2.0;
const COUNTDOWN: u32 = 3;
const RESULT_WAIT: f32 = 6.0;
/// The difficulty rows: practice, then the gallery's own.
const ROWS: usize = 4;
/// How far past the frustum's distance a bird flies before it has got away.
const GOT_AWAY: f32 = 1.5;

/// The gallery's weapon's slot, ammunition kept up.
fn lend(inv: &mut Inventory, def: usize) {
    let d = &inv.defs[def];
    let slot = WeaponSlot { def, mag: d.magazine, reserve: d.max_reserve };
    inv.mounted_from = Some(inv.current);
    inv.slots.push(slot);
    inv.current = inv.slots.len() - 1;
    inv.reloading = 0.0;
    inv.switching = 0.35;
}

fn give_back(inv: &mut Inventory, lent: Option<usize>) {
    if let (Some(def), Some(from)) = (lent, inv.mounted_from) {
        if inv.slots.last().is_some_and(|s| s.def == def) {
            inv.slots.pop();
            inv.mounted_from = None;
            inv.current = from.min(inv.slots.len().saturating_sub(1));
            inv.reloading = 0.0;
            inv.switching = 0.35;
        }
    }
}

/// An archetype's model and skeleton (its graphic's `.xbg`, its simple
/// animation's skeleton).
fn archetype_parts(data: &fc3_story::data::StoryData, archetype: &str) -> Option<(String, String)> {
    let entry = data.archetype_named(archetype)?;
    let model = crate::story_props::archetype_model(entry)?;
    let skeleton = entry.component("CSimpleAnimationComponent")?.str_h(0xF9F2_D5F4).filter(|s| !s.is_empty())?;
    Some((model, skeleton))
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut gallery: ResMut<Gallery>,
    mut host: Option<NonSendMut<crate::story::StoryHost>>,
    info: Option<Res<crate::CacheInfo>>,
    (server, models, mut music, mut limits, mut progress): (
        Res<AssetServer>,
        Res<Assets<Fc3Model>>,
        ResMut<crate::music::Music>,
        ResMut<ViewLimits>,
        ResMut<crate::progression::Progress>,
    ),
    mut players: Query<(&mut Transform, &mut Player, &crate::combat::Health, &mut Inventory)>,
    birds: Query<(Entity, &GalleryBird, &Transform, &Health), Without<Player>>,
    (keys, mouse, time, mut rng, mut sfx, mut commands): (
        Res<ButtonInput<KeyCode>>,
        Res<ButtonInput<MouseButton>>,
        Res<Time>,
        ResMut<Rng>,
        MessageWriter<crate::sound::PlaySfx>,
        Commands,
    ),
) {
    let Ok((mut t, mut p, health, mut inv)) = players.single_mut() else { return };
    let g = &mut *gallery;
    let mut cancelled = std::mem::take(&mut g.cancelled);
    if health.dead {
        cancelled.extend(g.run.as_ref().map(|r| r.id));
    }
    for start in std::mem::take(&mut g.pending) {
        if cancelled.contains(&start.id) {
            continue;
        }
        let Some(host) = host.as_ref() else { continue };
        let read = || -> anyhow::Result<(ShootingGallerySettings, ShootingGalleryResources)> {
            use anyhow::Context;
            let bytes = std::fs::read(info.as_ref().context("cache unavailable")?.root.join(fc3_formats::gsf::CACHE_REL))?;
            Ok((ShootingGallerySettings::parse(&bytes, &start.name)?, ShootingGalleryResources::parse(&bytes)?))
        };
        let (settings, resources) = match read() {
            Ok(s) => s,
            Err(e) => {
                warn!("shooting gallery: {} not played: {e:#}", start.name);
                continue;
            }
        };
        let param = |k: &str| start.params.get(k).cloned().unwrap_or_default();
        let animal = resources.animals.get(settings.animal).or(resources.animals.first()).cloned().unwrap_or_default();
        let data = host.story.data();
        let (model, skeleton) = archetype_parts(&data, &animal.archetype).unwrap_or_default();
        let clip = |c: &str| server.load(cache_path(&cache::anim_rel(c)));
        let set = Arc::new(ClipSet {
            skeleton: server.load(cache_path(&cache::skeleton_rel(&skeleton))),
            clips: HashMap::from([(AnimKind::Idle, clip(&animal.fly)), (AnimKind::Walk, clip(&animal.fly)), (AnimKind::Death, clip(&animal.death))]),
        });
        let model = server.load_builder().with_settings(|m: &mut ModelSettings| m.skinned = true).load(skinned_cache_path(&cache::model_rel(&model)));
        let weapon = resources.weapons.get(settings.weapon).or(resources.weapons.first()).map(|w| w.0.clone()).unwrap_or_default();
        info!("shooting gallery ({}): {} with {weapon}, birds {}", start.id, start.name, animal.archetype);
        g.run = Some(Run {
            id: start.id,
            name: start.name.clone(),
            animal,
            countdown_sound: resources.countdown_sound,
            ready_sound: resources.ready_sound,
            weapon,
            messages: ["DominoFinishMessage", "DominoFailMessage", "DominoQuitMessage", "DominoCancelMessage"]
                .map(|k| param(&format!("hidHiddenSettings/{k}"))),
            block: param("ChallengeMissionBlock"),
            base_layer: param("ChallengeMissionLayerBase"),
            difficulty_layers: [0, 1, 2].map(|i| param(&format!("DifficultySettings/index:{i}/DifficultyMissionLayer"))),
            layers: Vec::new(),
            settings,
            phase: Phase::Choosing,
            t: 0.0,
            row: 0,
            wager: 0,
            kills: 0,
            wave: 0,
            to_come: 0,
            next_bird: 0.0,
            spot: None,
            placed_with: 0,
            lent: None,
            armed: false,
            shot: None,
            model,
            set,
            success: false,
            won: 0,
            told: false,
        });
        g.hovered = None;
        g.cancel_hovered = false;
    }
    if g.run.as_ref().is_some_and(|r| cancelled.contains(&r.id)) {
        let r = g.run.take().unwrap();
        info!("shooting gallery ({}): {} over", r.id, r.name);
        give_back(&mut inv, r.lent);
        limits.0 = None;
        if !r.settings.music.is_empty() {
            music.request(&r.settings.music, false);
        }
        if let Some(host) = host.as_mut() {
            for &l in &r.layers {
                host.story.set_layer_state(l, fc3_story::LayerState::Disabled);
            }
        }
        for (e, b, ..) in &birds {
            if b.run == r.id {
                commands.entity(e).despawn();
            }
        }
        g.screen = None;
    }
    let Some(r) = g.run.as_mut() else {
        g.screen = None;
        return;
    };
    let dt = time.delta_secs();
    r.t += dt;
    let difficulty = r.row.saturating_sub(1).min(r.settings.difficulties.len().saturating_sub(1));
    let target = r.settings.difficulties.get(difficulty).map_or(10, |d| d.kill_target);
    // FC3_GALLERY_TEST=<row> (tests): that row chosen, and the birds shot.
    let test = std::env::var("FC3_GALLERY_TEST").ok().and_then(|s| s.parse::<usize>().ok());
    let tell = |host: &mut Option<NonSendMut<crate::story::StoryHost>>, message: &str| {
        if let Some(host) = host.as_mut().filter(|_| !message.is_empty()) {
            info!("shooting gallery: {message}");
            host.story.message(message);
        }
    };
    match r.phase {
        Phase::Choosing => {
            let wagers = [0, 1, 2].map(|i| r.settings.difficulties.get(i).map_or(0, |d| d.wager));
            let wagers = [0, wagers[0], wagers[1], wagers[2]];
            if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
                r.row = (r.row + ROWS - 1) % ROWS;
            }
            if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
                r.row = (r.row + 1) % ROWS;
            }
            if let Some(row) = g.hovered {
                r.row = row.min(ROWS - 1);
            }
            let short = progress.money < wagers[r.row];
            let accept = keys.just_pressed(KeyCode::Enter)
                || keys.just_pressed(KeyCode::KeyE)
                || (mouse.just_pressed(MouseButton::Left) && g.hovered.is_some())
                || test.is_some_and(|row| {
                    r.row = row.min(ROWS - 1);
                    r.t > 1.0
                });
            let cancel = keys.just_pressed(KeyCode::Escape) || (mouse.just_pressed(MouseButton::Left) && g.cancel_hovered);
            g.screen = Some(Screen::Choosing { row: r.row, wagers, target, short, subtitle: r.settings.subtitle });
            if cancel {
                // (Backed out: the keeper hears it, and stops the gameplay.)
                if !r.told {
                    r.told = true;
                    tell(&mut host, &r.messages[3]);
                }
            } else if accept && !short {
                r.wager = wagers[r.row];
                if r.wager > 0 {
                    progress.spend(r.wager);
                }
                // The challenge's layers in: its spot and its birds' boxes.
                if let Some(host) = host.as_mut() {
                    let data = host.story.data();
                    let block = data.block_named(&r.block).map(|b| b.id);
                    for name in [&r.base_layer, &r.difficulty_layers[difficulty]] {
                        if let Some(l) = data.layer_named(block, name).filter(|_| !name.is_empty()) {
                            host.story.set_layer_state(l.id, fc3_story::LayerState::Active);
                            r.layers.push(l.id);
                        }
                    }
                }
                info!("shooting gallery ({}): row {} (wager {}), layers {:?}", r.id, r.row, r.wager, r.layers);
                if !r.settings.music.is_empty() {
                    music.request(&r.settings.music, true);
                }
                if r.ready_sound != 0 {
                    sfx.write(crate::sound::PlaySfx { event: r.ready_sound, at: None, volume: 1.0 });
                }
                r.phase = Phase::Ready;
                r.t = 0.0;
            }
        }
        Phase::Ready | Phase::Countdown => {
            // Placed at the shooter's spot (else where they stand), facing
            // the birds' boxes: again as more of them register (they do as
            // their layers come in).
            if r.spot.is_none() || g.spawn_points.len() != r.placed_with {
                r.placed_with = g.spawn_points.len();
                let data = host.as_ref().map(|h| h.story.data());
                let loaded = |id: u64| host.as_ref().is_some_and(|h| h.story.is_loaded(id));
                let at = |id: u64| data.as_ref().and_then(|d| d.position(id)).map(crate::data::to_bevy);
                let boxes: Vec<Vec3> =
                    g.spawn_points.iter().filter(|(k, id)| k.eq_ignore_ascii_case("BirdBox") && loaded(*id)).filter_map(|(_, id)| at(*id)).collect();
                let shooter = g.spawn_points.iter().filter(|(k, id)| !k.eq_ignore_ascii_case("BirdBox") && loaded(*id)).filter_map(|(_, id)| at(*id)).last();
                let feet = shooter.or(r.spot.map(|s| s.0 - Vec3::Y * HALF_HEIGHT)).unwrap_or(t.translation - Vec3::Y * HALF_HEIGHT);
                let towards =
                    if boxes.is_empty() { feet + Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z } else { boxes.iter().sum::<Vec3>() / boxes.len() as f32 };
                let flat = (towards - feet).with_y(0.0);
                let yaw = (-flat.x).atan2(-flat.z);
                t.translation = feet + Vec3::Y * HALF_HEIGHT;
                p.yaw = yaw;
                p.pitch = 0.0;
                p.velocity = Vec3::ZERO;
                r.spot = Some((t.translation, yaw));
                limits.0 =
                    Some(ViewArc { yaw, pitch: 0.0, yaw_range: r.settings.yaw.map(f32::to_radians), pitch_range: r.settings.pitch.map(f32::to_radians) });
                info!(
                    "shooting gallery ({}): at {:.1?} facing {:.0} deg ({} boxes, shooter's spot {})",
                    r.id,
                    crate::data::to_dunia(feet),
                    yaw.to_degrees(),
                    boxes.len(),
                    shooter.is_some()
                );
            }
            // The gallery's weapon lent (once).
            if !r.armed {
                r.armed = true;
                let def = host.as_ref().and_then(|h| h.weapon_def(&inv, &r.weapon));
                r.lent = def.filter(|_| inv.mounted_from.is_none()).inspect(|&d| lend(&mut inv, d));
                info!("shooting gallery ({}): {} lent", r.id, r.lent.map_or("no weapon", |d| inv.defs[d].name));
            }
            if r.phase == Phase::Ready {
                g.screen = Some(Screen::Ready);
                if r.t >= READY {
                    r.phase = Phase::Countdown;
                    r.t = 0.0;
                    tell(&mut host, "ShootingMinigame_StartCountdown");
                    if r.countdown_sound != 0 {
                        sfx.write(crate::sound::PlaySfx { event: r.countdown_sound, at: None, volume: 1.0 });
                    }
                }
            } else {
                let before = (r.t - dt).max(0.0) as u32;
                if (r.t as u32) != before && (r.t as u32) < COUNTDOWN && r.countdown_sound != 0 {
                    sfx.write(crate::sound::PlaySfx { event: r.countdown_sound, at: None, volume: 1.0 });
                }
                g.screen = Some(Screen::Countdown(COUNTDOWN.saturating_sub(r.t as u32).max(1)));
                if r.t >= COUNTDOWN as f32 {
                    r.phase = Phase::Wave;
                    r.wave = 0;
                    r.t = 0.0;
                    r.to_come = r.settings.waves.first().map_or(0, |w| w.birds);
                    r.next_bird = r.settings.wave_times(0, difficulty).map_or(0.0, |w| w.0);
                }
            }
        }
        Phase::Wave => {
            // The weapon's magazine kept up (`gMaxTimeBetweenAutomaticWeaponRefills`).
            if let Some(def) = r.lent {
                let (mag, reserve) = (inv.defs[def].magazine, inv.defs[def].max_reserve);
                if let Some(slot) = inv.slots.last_mut().filter(|s| s.def == def) {
                    slot.reserve = reserve.max(mag);
                }
            }
            let (delay, duration) = r.settings.wave_times(r.wave, difficulty).unwrap_or((0.0, 10.0));
            // Its birds out of their boxes, one by one (or all at once as a
            // flock).
            if r.to_come > 0 && r.t >= r.next_bird && crate::cache_assets::usable(&server, r.model.id()) && r.set.ready(&server) {
                if let Some(model) = models.get(&r.model) {
                    let (eye, yaw) = r.spot.map_or((t.translation, p.yaw), |(at, yaw)| (at + Vec3::Y * (p.eye - HALF_HEIGHT), yaw));
                    let data = host.as_ref().map(|h| h.story.data());
                    let boxes: Vec<Vec3> = g
                        .spawn_points
                        .iter()
                        .filter(|(k, id)| k.eq_ignore_ascii_case("BirdBox") && host.as_ref().is_some_and(|h| h.story.is_loaded(*id)))
                        .filter_map(|(_, id)| data.as_ref().and_then(|d| d.position(*id)).map(crate::data::to_bevy))
                        .collect();
                    let n = if r.settings.flock { r.to_come } else { 1 };
                    for _ in 0..n {
                        let from = if boxes.is_empty() {
                            eye + Quat::from_rotation_y(yaw + rng.range(-0.6, 0.6)) * Vec3::new(0.0, -1.0, -18.0)
                        } else {
                            boxes[(rng.f32() * boxes.len() as f32) as usize % boxes.len()] + Vec3::Y * 0.5
                        };
                        // Off to a point within the frustum, at its distance.
                        let [left, right, up, down] = r.settings.frustum.map(f32::to_radians);
                        let heading =
                            Quat::from_euler(EulerRot::YXZ, yaw + rng.range(-right, left), rng.range(0.15f32.min(up), up.max(0.15)) * 0.6 - down * 0.1, 0.0);
                        let goal = eye + heading * Vec3::NEG_Z * r.settings.frustum_distance.max(10.0);
                        let d = &r.settings.difficulties[difficulty.min(r.settings.difficulties.len() - 1)];
                        let velocity = (goal - from).normalize_or(Vec3::Y) * d.speed.max(1.0);
                        let root = commands
                            .spawn((
                                Transform::from_translation(from).looking_to(velocity, Vec3::Y),
                                Visibility::Visible,
                                Health::new(1.0),
                                RigidBody::Kinematic,
                                Collider::sphere(0.45),
                                Name::new("gallery bird"),
                            ))
                            .id();
                        let rig = spawn_rig_with(&mut commands, root, &[model], None, r.set.clone(), Vec3::ZERO, None);
                        if let Some(rig) = rig {
                            commands.entity(rig).insert(NotShadowCaster);
                        }
                        commands.entity(root).insert(GalleryBird {
                            run: r.id,
                            velocity,
                            turns: d.turns,
                            next_turn: d.turn_from / d.speed.max(1.0) + d.turn_every,
                            turning: None,
                            rig,
                            counted: false,
                            falling: 0.0,
                            age: 0.0,
                        });
                        if r.animal.release_sound != 0 {
                            sfx.write(crate::sound::PlaySfx { event: r.animal.release_sound, at: Some(from), volume: 1.0 });
                        }
                        debug!(
                            "shooting gallery ({}): a bird from {:.1?} ({:.0} m off) to {:.1?}",
                            r.id,
                            crate::data::to_dunia(from),
                            from.distance(eye),
                            crate::data::to_dunia(goal)
                        );
                    }
                    r.to_come -= n;
                    r.next_bird = r.t + rng.range(r.settings.spawn_delay[0], r.settings.spawn_delay[1].max(r.settings.spawn_delay[0]));
                }
            }
            // The kills (the counting is `fly_birds`'s).
            r.kills = birds.iter().filter(|(_, b, _, h)| b.run == r.id && h.dead).count() as u32;
            let left = (delay + duration - r.t).max(0.0);
            let last = r.wave + 1 >= r.settings.waves.len();
            g.screen = Some(Screen::Playing { kills: r.kills, target, left, next_wave: None });
            if r.kills >= target || (left <= 0.0 && last) {
                r.success = r.kills >= target;
                r.won = if r.success { (r.wager as f32 * r.settings.winnings).round() as u32 } else { 0 };
                if r.won > 0 {
                    progress.add_money(r.won);
                }
                info!("shooting gallery ({}): {} with {}/{} birds, {} won", r.id, if r.success { "done" } else { "failed" }, r.kills, target, r.won);
                r.phase = Phase::Result;
                r.t = 0.0;
            } else if left <= 0.0 {
                // The next wave; what's left of this one gets away.
                for (e, b, _, h) in &birds {
                    if b.run == r.id && !h.dead {
                        commands.entity(e).despawn();
                    }
                }
                r.wave += 1;
                r.t = 0.0;
                r.to_come = r.settings.waves[r.wave].birds;
                r.next_bird = r.settings.wave_times(r.wave, difficulty).map_or(0.0, |w| w.0);
            }
            // FC3_GALLERY_TEST: on the nearest live bird and firing.
            if test.is_some() {
                let eye = t.translation + Vec3::Y * (p.eye - HALF_HEIGHT);
                if let Some((_, _, bt, _)) = birds
                    .iter()
                    .filter(|(_, b, bt, h)| b.run == r.id && !h.dead && b.age > 0.4 && bt.translation.distance(eye) > 3.0)
                    .min_by(|a, b| a.2.translation.distance(eye).total_cmp(&b.2.translation.distance(eye)))
                {
                    let aim = bt.translation - eye;
                    p.yaw = (-aim.x).atan2(-aim.z);
                    p.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.1));
                }
            }
        }
        Phase::Result => {
            g.screen = Some(Screen::Result { success: r.success, kills: r.kills, target, won: r.won });
            // Restart (the difficulty box again), or leave: the keeper hears
            // how it went. (A test leaves after a while.)
            let restart = keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::KeyE);
            let leave = keys.just_pressed(KeyCode::Escape) || (test.is_some() && r.t > RESULT_WAIT);
            if restart && !r.told {
                if let Some(host) = host.as_mut() {
                    for l in r.layers.drain(..) {
                        host.story.set_layer_state(l, fc3_story::LayerState::Disabled);
                    }
                }
                for (e, b, ..) in &birds {
                    if b.run == r.id {
                        commands.entity(e).despawn();
                    }
                }
                *r = Run {
                    phase: Phase::Choosing,
                    t: 0.0,
                    kills: 0,
                    wave: 0,
                    to_come: 0,
                    success: false,
                    won: 0,
                    wager: 0,
                    ..std::mem::replace(r, Run::placeholder())
                };
                info!("shooting gallery ({}): restarted", r.id);
            } else if leave && !r.told {
                r.told = true;
                let message = r.messages[if r.success { 0 } else { 1 }].clone();
                tell(&mut host, &message);
            }
        }
    }
    // FC3_GALLERY_SHOTS=<dir> (tests): the screen captured once into each
    // phase, the shooting every few seconds of its first wave.
    if let Ok(dir) = std::env::var("FC3_GALLERY_SHOTS") {
        let taken = r.shot.filter(|s| s.0 == r.phase).map_or(0, |s| s.1);
        let (due, most) = if r.phase == Phase::Wave { (2.0 + 3.0 * taken as f32, if r.wave == 0 { 4 } else { 0 }) } else { (0.8, 1) };
        if r.t > due && taken < most {
            r.shot = Some((r.phase, taken + 1));
            crate::camera::capture(&mut commands, format!("{dir}/{}_{taken}.png", format!("{:?}", r.phase).to_ascii_lowercase()));
        }
    }
}

/// The birds' flight: straight on, turning now and then by their
/// difficulty's angles; shot, they fall; flown past the frustum's distance,
/// they're gone.
#[allow(clippy::too_many_arguments)]
fn fly_birds(
    time: Res<Time>,
    gallery: Res<Gallery>,
    heights: Res<crate::terrain::Heightfield>,
    mut rng: ResMut<Rng>,
    players: Query<&Transform, (With<Player>, Without<GalleryBird>)>,
    mut birds: Query<(Entity, &mut GalleryBird, &mut Transform, &Health)>,
    mut anims: Query<&mut AnimRequest>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let Some(r) = gallery.run.as_ref() else { return };
    let difficulty = r.row.saturating_sub(1).min(r.settings.difficulties.len().saturating_sub(1));
    let Some(d) = r.settings.difficulties.get(difficulty) else { return };
    let eye = players.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    for (e, mut b, mut t, h) in &mut birds {
        b.age += dt;
        if h.dead {
            if !b.counted {
                b.counted = true;
                if r.animal.death_sound != 0 {
                    sfx.write(crate::sound::PlaySfx { event: r.animal.death_sound, at: Some(t.translation), volume: 1.0 });
                }
            }
            b.falling += 9.81 * dt;
            let floor = heights.height_at(t.translation).unwrap_or(t.translation.y - 50.0) + 0.1;
            if t.translation.y > floor {
                t.translation.y = (t.translation.y - b.falling * dt).max(floor);
                t.rotate_local_z(dt * 3.0);
            } else if b.falling > 0.0 && r.animal.impact_sound != 0 {
                sfx.write(crate::sound::PlaySfx { event: r.animal.impact_sound, at: Some(t.translation), volume: 1.0 });
                b.falling = 0.0;
            }
            if let Some(mut a) = b.rig.and_then(|rig| anims.get_mut(rig).ok()) {
                *a = AnimRequest { kind: AnimKind::Death, rate: 1.0 };
            }
            continue;
        }
        // A turn now and then, eased over its time.
        b.next_turn -= dt;
        if b.next_turn <= 0.0 && b.turns > 0 && b.turning.is_none() {
            b.turns -= 1;
            b.next_turn = d.turn_every;
            let side = if rng.f32() < 0.5 { -1.0 } else { 1.0 };
            let yaw = side * rng.range(d.horizontal_turn[0].abs(), d.horizontal_turn[1].abs()).to_radians();
            let pitch = rng.range(d.vertical_turn[0], d.vertical_turn[1]).to_radians() * if rng.f32() < 0.7 { 1.0 } else { -0.5 };
            let to = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0) * b.velocity;
            b.turning = Some((b.velocity, to, 0.0));
        }
        if let Some((from, to, s)) = b.turning {
            let s = (s + dt / d.turn_time.max(0.1)).min(1.0);
            b.velocity = from.lerp(to, s).normalize_or(from) * from.length();
            b.turning = (s < 1.0).then_some((from, to, s));
        }
        // Not into the ground.
        let ground = heights.height_at(t.translation).unwrap_or(f32::MIN);
        if t.translation.y < ground + 2.0 && b.velocity.y < 0.0 {
            b.velocity.y = b.velocity.y.abs();
        }
        let v = b.velocity;
        t.translation += v * dt;
        t.look_to(v, Vec3::Y);
        if let Some(mut a) = b.rig.and_then(|rig| anims.get_mut(rig).ok()) {
            *a = AnimRequest { kind: AnimKind::Walk, rate: 1.0 };
        }
        if t.translation.distance(eye) > r.settings.frustum_distance.max(10.0) * GOT_AWAY + 20.0 || b.age > 40.0 {
            commands.entity(e).despawn();
        }
    }
}

/// At the gallery: no walking, jumping or other weapons; while choosing, no
/// shooting either.
fn hold_controls(gallery: Res<Gallery>, mut actions: ResMut<ActionState>) {
    let Some(r) = &gallery.run else { return };
    actions.movement = Vec2::ZERO;
    actions.cycle = 0;
    for x in
        [Action::Jump, Action::NextWeapon, Action::PrevWeapon, Action::Grenade, Action::Molotov, Action::Melee, Action::ThrowRock, Action::C4, Action::Mine]
    {
        actions.suppress(x);
    }
    match r.phase {
        Phase::Wave => {
            // (A test through the sights, as a player would.)
            if std::env::var_os("FC3_GALLERY_TEST").is_some() {
                actions.hold(Action::Aim);
                actions.press(Action::Fire);
            }
        }
        _ => {
            actions.suppress(Action::Fire);
            actions.suppress(Action::Aim);
        }
    }
}

/// The player kept at the shooter's spot.
fn hold_place(gallery: Res<Gallery>, mut players: Query<(&mut Transform, &mut Player)>) {
    let Some((at, _)) = gallery.run.as_ref().and_then(|r| r.spot) else { return };
    let Ok((mut t, mut p)) = players.single_mut() else { return };
    t.translation.x = at.x;
    t.translation.z = at.z;
    p.velocity = Vec3::ZERO;
}
