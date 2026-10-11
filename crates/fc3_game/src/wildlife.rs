//! Wildlife: animals roaming the island, built from the game's animal models
//! and clips. Each behaves by its personality in the game's data
//! (`fc3_formats::animals`): it notices someone by sight (its sensory
//! system), smell, a moving vehicle or sounds, watches while aware, and once
//! that turns to action, prey (deer, goats, tapirs, macaques, chickens) keep
//! off or bolt, boar and buffalo charge whoever comes too near or hurts them,
//! and predators (wild dogs, leopards, tigers, bears, komodo dragons,
//! cassowaries, crocodiles) hunt and strike in bursts; bull sharks hunt
//! swimmers. Dead animals can be skinned for crafting.

use std::sync::Arc;

use avian3d::prelude::*;
use bevy::prelude::*;
use fc3_formats::cache;

use fc3_formats::animals::Personality;

use crate::cache_assets::{cache_path, skinned_cache_path, Fc3Model, ModelSettings};
use crate::characters::{spawn_rig_with, AnimKind, AnimRequest, ClipSet};
use crate::combat::{DamageEvent, DamageKind, Died, Health};
use crate::player::Player;
use crate::progression::Animal;
use crate::terrain::Heightfield;
use crate::weapons::Rng;

pub struct WildlifePlugin;

impl Plugin for WildlifePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AnimalTable>().add_systems(OnExit(crate::menu::GameState::Loading), (load_animals, load_animal_table)).add_systems(
            Update,
            (
                animal_test,
                spawn_animals,
                release_caged,
                cage_test,
                animal_ai,
                animal_deaths,
                skin_animals.before(crate::hud::HudSet).after(crate::turrets::man_turrets),
                despawn_animals,
            )
                .chain()
                .after(crate::story_actors::ActorSet)
                .run_if(in_state(crate::menu::GameState::Playing)),
        );
    }
}

/// Animals alive near the player at once.
const MAX_ANIMALS: usize = 12;

/// How an animal reacts to the player.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Behaviour {
    /// Runs away.
    Prey,
    /// Charges when crowded or hurt, otherwise keeps away.
    Defensive,
    /// Hunts the player on sight.
    Predator,
}

/// Where an animal lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Habitat {
    Land,
    /// On land and in shallow water.
    Shore,
    /// Swims in deep water.
    Sea,
}

/// Swimming depth of sea animals below the surface.
const SWIM_DEPTH: f32 = 1.6;
const SPAWN_MIN: f32 = 90.0;
const SPAWN_MAX: f32 = 200.0;
const DESPAWN: f32 = 300.0;

struct Species {
    animal: Animal,
    model: &'static str,
    skeleton: &'static str,
    clips: &'static str,
    idle: &'static str,
    walk: &'static str,
    run: &'static str,
    die: &'static str,
    eat: Option<&'static str>,
    attack: Option<&'static str>,
    health: f32,
    walk_speed: f32,
    run_speed: f32,
    /// Capsule radius and half-height of the body.
    size: (f32, f32),
    behaviour: Behaviour,
    habitat: Habitat,
    /// Damage per bite or charge.
    damage: f32,
    /// Relative spawn weight.
    weight: f32,
    /// Skin material stem (see `cache::ANIMAL_SKINS`) for models that only
    /// carry placeholders.
    skin: Option<&'static str>,
}

const MODELS: &str = r"graphics\__fc3_graphics\_common\characters\animals\";
const CLIPS: &str = r"graphics\characters\_common\animations\animals\";

const SPECIES: &[Species] = &[
    Species {
        animal: Animal::Deer,
        model: r"deer\deer.xbg",
        skeleton: r"deer\pelvis_ref.skeleton",
        clips: r"deer\",
        idle: "gazel_fulb_idle_+000fw_nowep_i1.mab",
        walk: "gazel_fulb_walkcycle_+000fw_nowep_i1.mab",
        run: "small_gazel_fulb_jogcycle_+000fw_nowep_i1.mab",
        die: "small_gazel_fulb_die_+000fw_nowep.mab",
        eat: Some("small_gazel_fulb_eatgrass_stp.mab"),
        attack: None,
        health: 60.0,
        walk_speed: 1.4,
        run_speed: 9.0,
        size: (0.35, 0.5),
        behaviour: Behaviour::Prey,
        habitat: Habitat::Land,
        damage: 0.0,
        weight: 3.0,
        skin: Some("yanzhou-m-2106201150629658"),
    },
    Species {
        animal: Animal::Goat,
        model: r"goat\goat.xbg",
        skeleton: r"goat\hips_ref.skeleton",
        clips: r"goat\",
        idle: "goat_fulb_idle_+000fw_nowep_i1.mab",
        walk: "goat_fulb_walkcycle_+000fw_nowep.mab",
        run: "goat_fulb_runcycle_+000fw_nowep_i1.mab",
        die: "goat_fulb_die_+000fw_nowep.mab",
        eat: None,
        attack: None,
        health: 50.0,
        walk_speed: 1.2,
        run_speed: 7.0,
        size: (0.3, 0.4),
        behaviour: Behaviour::Prey,
        habitat: Habitat::Land,
        damage: 0.0,
        weight: 3.0,
        skin: Some("yanzhou-m-20120208181926"),
    },
    Species {
        animal: Animal::Boar,
        model: r"boar\boar.xbg",
        skeleton: r"boar\hips_ref.skeleton",
        clips: r"boar\",
        idle: "boar_fulb_idlecycle_+000fw_nowep.mab",
        walk: "boar_fulb_walkcycle_+000fw_nowep.mab",
        run: "boar_fulb_runcycle_+000fw_nowep.mab",
        die: "boar_fulb_die_+000fw_nowep.mab",
        eat: Some("boar_fulb_eat_+000fw_nowep.mab"),
        attack: Some("boar_fulb_closeattackbutt_+000fw_nowep.mab"),
        health: 110.0,
        walk_speed: 1.1,
        run_speed: 7.5,
        size: (0.35, 0.45),
        behaviour: Behaviour::Defensive,
        habitat: Habitat::Land,
        damage: 18.0,
        weight: 2.5,
        skin: None,
    },
    Species {
        animal: Animal::Tapir,
        model: r"asian_tapir\atapir.xbg",
        skeleton: r"asian_tapir\pelvis_ref.skeleton",
        clips: r"asian_tapir\",
        idle: "atapir_fulb_idlecycle_+000fw_nowep.mab",
        walk: "atapir_fulb_walkcycle_+000fw_nowep.mab",
        run: "atapir_fulb_runcycle_+000fw_nowep.mab",
        die: "atapir_fulb_die_+000fw_nowep.mab",
        eat: Some("atapir_fulb_eat_+000fw_nowep.mab"),
        attack: None,
        health: 150.0,
        walk_speed: 1.0,
        run_speed: 6.0,
        size: (0.45, 0.55),
        behaviour: Behaviour::Prey,
        habitat: Habitat::Land,
        damage: 0.0,
        weight: 1.2,
        skin: Some("yanzhou-m-20120201170940"),
    },
    Species {
        animal: Animal::Dog,
        model: r"rabid_dog\rdog_01.xbg",
        skeleton: r"rabid_dog\pelvis_ref.skeleton",
        clips: r"rabid_dog\",
        idle: "rdog_fulb_idlecycle_+000fw_nowep.mab",
        walk: "rdog_fulb_walkcycle_+000fw_nowep.mab",
        run: "rdog_fulb_runcycle_+000fw_nowep.mab",
        die: "rdog_fulb_die_+000fw_nowep.mab",
        eat: Some("rdog_fulb_eat_+000fw_nowep.mab"),
        attack: Some("rdog_fulb_closeattack_success_+000fw_nowep.mab"),
        health: 70.0,
        walk_speed: 1.5,
        run_speed: 8.0,
        size: (0.3, 0.4),
        behaviour: Behaviour::Predator,
        habitat: Habitat::Land,
        damage: 14.0,
        weight: 1.0,
        skin: Some("yanzhou-m-0909201161474399"),
    },
    Species {
        animal: Animal::Leopard,
        model: r"leopard\leopard_01.xbg",
        skeleton: r"leopard\pelvis_ref.skeleton",
        clips: r"leopard\",
        idle: "leopard_fulb_idlecycle_+000fw_nowep.mab",
        walk: "leopard_fulb_walkcycle_+000fw_nowep.mab",
        run: "leopard_fulb_runcycle_+000fw_nowep.mab",
        die: "leopard_fulb_die_+000fw_nowep.mab",
        eat: Some("leopard_fulb_eat_stp_+000fw_nowep.mab"),
        attack: Some("leopard_fulb_closeattack_success_+000fw_nowep.mab"),
        health: 140.0,
        walk_speed: 1.6,
        run_speed: 10.0,
        size: (0.35, 0.5),
        behaviour: Behaviour::Predator,
        habitat: Habitat::Land,
        damage: 25.0,
        weight: 0.5,
        skin: None,
    },
    Species {
        animal: Animal::Bear,
        model: r"asian_black_bear\ab_bear.xbg",
        skeleton: r"asian_black_bear\hips_ref.skeleton",
        clips: r"asian_black_bear\",
        idle: "bear_fulb_idlecycle_+000fw_nowep.mab",
        walk: "bear_fulb_walkcycle_+000fw_nowep.mab",
        run: "bear_fulb_runcycle_+000fw_nowep.mab",
        die: "bear_fulb_die_+000fw_nowep.mab",
        eat: Some("bear_fulb_eat_+000fw_nowep.mab"),
        attack: Some("bear_fulb_closeattack_success_+000fw_nowep.mab"),
        health: 320.0,
        walk_speed: 1.3,
        run_speed: 8.5,
        size: (0.55, 0.7),
        behaviour: Behaviour::Predator,
        habitat: Habitat::Land,
        damage: 45.0,
        weight: 0.5,
        skin: Some("yanzhou-m-0711201150689792"),
    },
    Species {
        animal: Animal::Tiger,
        model: r"sumatran_tiger\sumatran_tiger.xbg",
        skeleton: r"sumatran_tiger\hips_ref.skeleton",
        clips: r"sumatran_tiger\",
        idle: "tiger_fulb_idle_01_+000fw_nowep.mab",
        walk: "tiger_fulb_walkcycle_+000fw_nowep.mab",
        run: "tiger_fulb_runcycle_+000fw_nowep.mab",
        die: "tiger_fulb_die_+000fw_nowep.mab",
        eat: Some("tiger_fulb_eat02_standcycle_+000fw_nowep.mab"),
        attack: Some("tiger_fulb_closeattack_success_+000fw_nowep.mab"),
        health: 240.0,
        walk_speed: 1.6,
        run_speed: 11.0,
        size: (0.45, 0.7),
        behaviour: Behaviour::Predator,
        habitat: Habitat::Land,
        damage: 40.0,
        weight: 0.35,
        skin: Some("yanzhou-m-0811201155154915"),
    },
    Species {
        animal: Animal::Komodo,
        model: r"komododragon\komododragon01.xbg",
        skeleton: r"komododragon\hips_ref.skeleton",
        clips: r"komododragon\",
        idle: "komododragon_fulb_idlecycle_+000fw_nowep.mab",
        walk: "komododragon_fulb_walkcycle_+000fw_nowep.mab",
        run: "komododragon_fulb_walkcycle_+000fw_nowep.mab",
        die: "komododragon_fulb_die_+000fw_nowep.mab",
        eat: Some("komododragon_fulb_eat_+000fw_nowep.mab"),
        attack: Some("komododragon_fulb_closeattack_+000fw_nowep.mab"),
        health: 180.0,
        walk_speed: 0.8,
        run_speed: 3.5,
        size: (0.3, 0.8),
        behaviour: Behaviour::Predator,
        habitat: Habitat::Land,
        damage: 22.0,
        weight: 0.5,
        skin: Some("vfortin-m-2010120863940176"),
    },
    Species {
        animal: Animal::Cassowary,
        model: r"cassowary\cassowary.xbg",
        skeleton: r"cassowary\pelvis_ref.skeleton",
        clips: r"cassowary\",
        idle: "cassowary_fulb_idlecycle_+000fw_nowep.mab",
        walk: "cassowary_fulb_walkcycle_+000fw_nowep.mab",
        run: "cassowary_fulb_runcycle_+000fw_nowep.mab",
        die: "cassowary_fulb_die_+000fw_nowep.mab",
        eat: Some("cassowary_fulb_eat_+180bk_nowep.mab"),
        attack: Some("cassowary_fulb_attack_success_+000fw_nowep.mab"),
        health: 90.0,
        walk_speed: 1.3,
        run_speed: 9.0,
        size: (0.3, 0.5),
        behaviour: Behaviour::Defensive,
        habitat: Habitat::Land,
        damage: 22.0,
        weight: 1.0,
        skin: Some("yanzhou-m-0407201152986069"),
    },
    Species {
        animal: Animal::Buffalo,
        model: r"water_buffalo\water_buffalo.xbg",
        skeleton: r"water_buffalo\hips_ref.skeleton",
        clips: r"water_buffalo\",
        idle: "buffalo_fulb_idlecycle_+000fw_nowep.mab",
        walk: "buffalo_fulb_walkcycle_+000fw_nowep.mab",
        run: "buffalo_fulb_runcycle_+000fw_nowep.mab",
        die: "buffalo_fulb_die_+000fw_nowep.mab",
        eat: Some("buffalo_fulb_eat_+000fw_nowep.mab"),
        attack: Some("buffalo_fulb_closeattack_+000fw_nowep.mab"),
        health: 280.0,
        walk_speed: 1.0,
        run_speed: 7.0,
        size: (0.6, 0.8),
        behaviour: Behaviour::Defensive,
        habitat: Habitat::Land,
        damage: 30.0,
        weight: 1.2,
        skin: None,
    },
    Species {
        animal: Animal::Crocodile,
        model: r"crocodile\crocodile.xbg",
        skeleton: r"crocodile\hips_ref.skeleton",
        clips: r"crocodile\",
        idle: "croc_fulb_awarecycle_+000fw_nowep.mab",
        walk: "croc_fulb_walkcycle_+000fw_nowep.mab",
        run: "croc_fulb_jogcycle_+000fw_nowep.mab",
        die: "croc_fulb_die_+000fw_nowep.mab",
        eat: None,
        attack: Some("croc_fulb_attackturn_+090rt_nowep.mab"),
        health: 220.0,
        walk_speed: 0.8,
        run_speed: 5.5,
        size: (0.4, 1.0),
        behaviour: Behaviour::Predator,
        habitat: Habitat::Shore,
        damage: 45.0,
        weight: 0.7,
        skin: Some("yanzhou-m-2107201165520254"),
    },
    Species {
        animal: Animal::Shark,
        model: r"shark\bull_shark_01.xbg",
        skeleton: r"shark\hips_ref.skeleton",
        clips: r"shark\",
        idle: "bull_shark_fulb_swimcycle_+000fw_nowep.mab",
        walk: "bull_shark_fulb_swimcycle_+000fw_nowep.mab",
        run: "bull_shark_fulb_swimfastcycle_+000fw_nowep.mab",
        die: "bull_shark_fulb_die_success_+000fw_nowep.mab",
        eat: None,
        attack: Some("bull_shark_fulb_closetattack_+000fw_nowep_new.mab"),
        health: 200.0,
        walk_speed: 2.0,
        run_speed: 8.0,
        size: (0.4, 0.9),
        behaviour: Behaviour::Predator,
        habitat: Habitat::Sea,
        damage: 50.0,
        weight: 2.0,
        skin: None,
    },
    Species {
        animal: Animal::Macaque,
        model: r"macaque\macaque.xbg",
        skeleton: r"macaque\hips_ref.skeleton",
        clips: r"macaque\",
        idle: "monkey_fulb_idle01_+000fw_nowep.mab",
        walk: "monkey_fulb_walkcycle_+000fw_nowep.mab",
        run: "monkey_fulb_runcycle_+000fw_nowep.mab",
        die: "monkey_fulb_die_+000fw_nowep.mab",
        eat: None,
        attack: None,
        health: 30.0,
        walk_speed: 1.0,
        run_speed: 6.0,
        size: (0.2, 0.25),
        behaviour: Behaviour::Prey,
        habitat: Habitat::Land,
        damage: 0.0,
        weight: 0.8,
        skin: None,
    },
    Species {
        animal: Animal::Chicken,
        model: r"chicken\chicken_a.xbg",
        skeleton: r"chicken\spine_ref.skeleton",
        clips: r"chicken\",
        idle: "chick_fulb_lookaround_+000fw_nowep_i1.mab",
        walk: "chick_fulb_walkcycle_+000fw_nowep_i1.mab",
        run: "chick_fulb_runcycle_+000fw_nowep_i1.mab",
        die: "chick_fulb_die_+000fw_nowep_i1.mab",
        eat: Some("chick_fulb_eat_+000fw_nowep_i1.mab"),
        attack: None,
        health: 10.0,
        walk_speed: 0.6,
        run_speed: 4.0,
        size: (0.12, 0.12),
        behaviour: Behaviour::Prey,
        habitat: Habitat::Land,
        damage: 0.0,
        weight: 0.5,
        skin: Some("yanzhou-m-1908201167678161"),
    },
];

#[derive(Resource)]
pub(crate) struct AnimalAssets {
    models: Vec<Handle<Fc3Model>>,
    sets: Vec<Arc<ClipSet>>,
    skins: Vec<Option<Handle<StandardMaterial>>>,
}

/// Supported normal story animals. Use the authored family, not the shared
/// model (a pig can use a boar's skeleton); don't flatten rare loot variants.
pub(crate) fn story_species(name: &str) -> Option<usize> {
    let name = name.to_ascii_lowercase();
    if name.contains("rare") { return None }
    let family = name.rsplit('/').next()?.split('.').next()?;
    let family = family.rsplit_once('_').filter(|(_, suffix)| !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_digit()))
        .map_or(family, |(family, _)| family);
    let animal = match family {
        "boar" => Animal::Boar, "deerfc3" => Animal::Deer, "goat" => Animal::Goat,
        "dogs_rapid" | "attackdog_fc3" => Animal::Dog, "asiantapir" => Animal::Tapir,
        "leopard" => Animal::Leopard, "asianblackbear" | "bear_young" | "bear_huge" => Animal::Bear,
        "tiger" => Animal::Tiger, "komododragon" => Animal::Komodo, "cassowary" => Animal::Cassowary,
        "buffalo" => Animal::Buffalo, "crocodile" => Animal::Crocodile, "bullshark" => Animal::Shark,
        _ => return None,
    };
    SPECIES.iter().position(|s| s.animal == animal)
}

impl AnimalAssets {
    pub(crate) fn ready_for_story(&self, species: usize, server: &AssetServer) -> bool {
        self.sets[species].ready(server)
    }

    /// Keep the authored graphic kit and story id, but give this root actual
    /// animal behavior, clips, health and the wildlife death/skinning path.
    pub(crate) fn bind_story(&self, commands: &mut Commands, root: Entity, species: usize, rig: Option<Entity>, at: Vec3, yaw: f32, rng: &mut Rng) {
        let s = &SPECIES[species];
        commands.entity(root).insert((WildAnimal {
            species, mood: Mood::Graze, call_in: rng.range(CALL_EVERY.0, CALL_EVERY.1),
            health_seen: f32::MAX, timer: rng.range(2.0, 8.0), goal: at, yaw,
            velocity: Vec3::ZERO, rig, attack_cooldown: 0.0, frozen: None,
            body_lift: crate::player::HALF_HEIGHT, sense: Sensing::default(),
        }, Health::new(s.health), Collider::capsule(s.size.0, s.size.1 * 2.0)));
        if let Some(rig) = rig {
            let clips = self.sets[species].clone();
            commands.entity(rig).entry::<crate::characters::CharacterRig>().and_modify(move |mut r| r.set_clips(clips.clone()));
        }
    }
}

fn load_animals(mut commands: Commands, server: Res<AssetServer>) {
    let mut models = Vec::new();
    let mut sets = Vec::new();
    let mut skins = Vec::new();
    for s in SPECIES {
        skins.push(s.skin.map(|stem| server.load(cache_path(&cache::material_rel(&cache::animal_skin_path(stem))))));
        models.push(
            server
                .load_builder()
                .with_settings(|m: &mut ModelSettings| m.skinned = true)
                .load(skinned_cache_path(&cache::model_rel(&format!("{MODELS}{}", s.model)))),
        );
        let clip = |name: &str| server.load(cache_path(&cache::anim_rel(&format!("{CLIPS}{}{name}", s.clips))));
        let mut clips = std::collections::HashMap::new();
        clips.insert(AnimKind::Idle, clip(s.idle));
        clips.insert(AnimKind::Walk, clip(s.walk));
        clips.insert(AnimKind::Run, clip(s.run));
        clips.insert(AnimKind::Death, clip(s.die));
        if let Some(e) = s.eat {
            clips.insert(AnimKind::Eat, clip(e));
        }
        if let Some(a) = s.attack {
            clips.insert(AnimKind::Attack, clip(a));
        }
        sets.push(Arc::new(ClipSet { skeleton: server.load(cache_path(&cache::skeleton_rel(&format!("{MODELS}{}", s.skeleton)))), clips }));
    }
    commands.insert_resource(AnimalAssets { models, sets, skins });
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mood {
    Graze,
    Wander,
    /// Aware of someone (or a sound): stops and watches.
    Watch,
    /// Keeping off from someone.
    Avoid,
    Flee,
    Hunt,
    Attack,
}

/// What an animal makes of the one it notices (the game's animal awareness).
#[derive(Clone, Copy, Default)]
struct Sensing {
    /// Seconds in view toward being seen.
    noticing: f32,
    /// Seconds aware with them sensed (toward acting), and since they were
    /// last sensed (toward relaxing, after `relax_after`, or giving up).
    aware: f32,
    lost: f32,
    relax_after: f32,
    /// A defensive animal's charge: seconds left.
    aggro: f32,
    /// Strikes made in this burst, and how many it makes before it stops to
    /// look (0 until it picks).
    strikes: u32,
    burst: u32,
}

/// The animals' personalities by species: the game's (`fc3_formats::animals`)
/// once read from the cache, else rough ones by behaviour.
#[derive(Resource)]
pub(crate) struct AnimalTable(Vec<Personality>);

impl Default for AnimalTable {
    fn default() -> Self {
        AnimalTable(SPECIES.iter().map(fallback).collect())
    }
}

fn fallback(s: &Species) -> Personality {
    let (smell, immediate) = match s.behaviour {
        Behaviour::Predator => (35.0, 5.0),
        Behaviour::Defensive => (20.0, 9.0),
        Behaviour::Prey => (22.0, 12.0),
    };
    Personality {
        temper: if s.behaviour == Behaviour::Predator { fc3_formats::animals::TEMPER_AGGRESSIVE } else { 2 },
        defensive: s.behaviour == Behaviour::Defensive,
        smell,
        stealth_smell: smell * 0.5,
        immediate,
        vehicle: 50.0,
        aware_to_active: 1.0,
        aware_cooldown: 5.0,
        max_height: 20.0,
        interesting_sound: 25.0,
        threatening_sound: 60.0,
        explosion_sound: 60.0,
        wander_chance: 0.5,
        wander: (5.0, 15.0),
        avoid: (25.0, 30.0),
        avoid_cooldown: 5.0,
        flee: (30.0, 40.0),
        defensive_stop: 12.0,
        attack_delay: (0.4, 0.4),
        attacks_before_pause: (3, 3),
        attack_reach: 1.8,
        attack_height: 1.5,
        ..Default::default()
    }
}

fn load_animal_table(info: Option<Res<crate::data::CacheInfo>>, mut table: ResMut<AnimalTable>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::animals::ANIMALS_REL))
        .map_err(anyhow::Error::from)
        .and_then(|t| fc3_formats::animals::from_text(&t))
    {
        Ok(all) => {
            let mut found = 0;
            for (i, s) in SPECIES.iter().enumerate() {
                if let Some(p) = all.iter().find(|p| p.name == s.animal.archetype()) {
                    table.0[i] = p.clone();
                    found += 1;
                }
            }
            info!("animals: {} personalities, {found} of {} species", all.len(), SPECIES.len());
        }
        Err(e) => warn!("animals: no personality table ({e:#}); re-run the converter"),
    }
}

/// How a personality reacts once it acts on someone.
fn behaviour(p: &Personality) -> Behaviour {
    if p.defensive {
        Behaviour::Defensive
    } else if p.predator() {
        Behaviour::Predator
    } else {
        Behaviour::Prey
    }
}

/// The swing of an attack (seconds), before the personality's wait between
/// attacks.
const ATTACK_SWING: f32 = 0.8;
/// Animals see ahead and to the sides (the cosine of half their view).
const SIGHT_COS: f32 = 0.0;

/// A species' calls: sound events from its archetype's sound component in
/// the entity library (`sndIdleSound`, `sndHurtSound`, `sndDeathSound`, and
/// `sndPanicSound` or `sndFleeSound` for the alarm; `fc3tool animal-sounds`
/// lists them). 0 is none.
#[derive(Clone, Copy, Default)]
struct Calls {
    idle: u32,
    hurt: u32,
    death: u32,
    alarm: u32,
}

fn calls(animal: Animal) -> Calls {
    let c = |idle, hurt, death, alarm| Calls { idle, hurt, death, alarm };
    match animal {
        Animal::Boar => c(0x000A_77C3, 0x000A_4772, 0x000A_46DA, 0x000A_4771),
        Animal::Deer => c(0, 0x000A_7B4A, 0x000A_7B4A, 0),
        Animal::Goat => c(0x000A_47D7, 0x000A_47D6, 0x000A_47D6, 0x000A_47D5),
        Animal::Dog => c(0, 0x0016_98DD, 0x0016_98DB, 0x0016_98DA),
        Animal::Tapir => c(0x001A_A5E6, 0x001A_A5E5, 0x001A_A5E2, 0x001A_A5E4),
        Animal::Leopard => c(0x001A_4011, 0x001A_4012, 0x001A_400F, 0x001A_4008),
        Animal::Bear => c(0x0015_8854, 0x001A_412A, 0x001A_411B, 0x001A_4123),
        Animal::Tiger => c(0x001A_A57E, 0x001A_A57F, 0x001A_A57C, 0x001A_A57E),
        Animal::Komodo => c(0, 0x001A_E587, 0x001A_E586, 0x0007_55CF),
        Animal::Cassowary => c(0x0011_3C79, 0x001A_A4AF, 0x0019_5C08, 0x0019_5C10),
        Animal::Buffalo => c(0x0019_5A51, 0x0019_5890, 0x0019_588F, 0x0019_58F9),
        Animal::Crocodile => c(0x001A_3F43, 0x001A_3F44, 0x001A_3F47, 0),
        Animal::Macaque => c(0x001A_A483, 0x001A_4197, 0x001A_4197, 0x001A_4196),
        Animal::Chicken => c(0, 0, 0x001B_085F, 0),
        Animal::Shark => Calls::default(),
    }
}

/// The health bars the player starts with (gained ones make animals' share
/// of the player's health smaller).
const BASE_HEALTH_BARS: f32 = 4.0;

/// Idle calls come every so often (seconds) from animals this near (m).
const CALL_EVERY: (f32, f32) = (10.0, 25.0);
const CALL_RANGE: f32 = 70.0;

#[derive(Component)]
pub struct WildAnimal {
    species: usize,
    mood: Mood,
    /// Seconds to the next idle call; health when last heard from.
    call_in: f32,
    health_seen: f32,
    timer: f32,
    goal: Vec3,
    yaw: f32,
    velocity: Vec3,
    rig: Option<Entity>,
    attack_cooldown: f32,
    /// Posed for a test: no AI.
    frozen: Option<AnimKind>,
    /// Story roots share the story host's capsule offset; ordinary wildlife
    /// uses the species' own offset. Keep both feet and graphic kits grounded.
    body_lift: f32,
    sense: Sensing,
}

impl WildAnimal {
    pub(crate) fn animal(&self) -> Animal {
        SPECIES[self.species].animal
    }

    /// Predators: soldiers shoot these on sight when they come close.
    pub fn dangerous(&self) -> bool {
        SPECIES[self.species].behaviour == Behaviour::Predator
    }
}

/// An animal cage's door (in outposts): shot or blown open, it lets out a
/// predator that goes for whoever is nearest, the garrison first.
#[derive(Component)]
pub struct CageDoor {
    pub id: u64,
}

pub const CAGE_DOOR_HEALTH: f32 = 40.0;

/// A dead animal that can be skinned.
#[derive(Component)]
pub(crate) struct Carcass {
    species: usize,
    age: f32,
    /// Already skinned (the body stays until it rots).
    skinned: bool,
}

impl Carcass {
    pub(crate) fn can_skin(&self) -> bool { !self.skinned }
}

fn random_ground(rng: &mut Rng, around: Vec3, heights: &Heightfield, outposts: &crate::outposts::Outposts, habitat: Habitat) -> Option<Vec3> {
    let sea = crate::sky::SEA_LEVEL;
    for _ in 0..8 {
        let a = rng.f32() * std::f32::consts::TAU;
        let d = rng.range(SPAWN_MIN, SPAWN_MAX);
        let p = around + Vec3::new(a.cos() * d, 0.0, a.sin() * d);
        let Some(h) = heights.height_at(p) else { continue };
        let edge = crate::water::plane_level(p);
        match habitat {
            // Deep water, swimming below the surface.
            Habitat::Sea if h < sea - 6.0 => return Some(Vec3::new(p.x, sea - SWIM_DEPTH, p.z)),
            Habitat::Sea => continue,
            // The water's edge (the sea's, a river's or a lake's).
            Habitat::Shore if !(edge - 1.0..edge + 2.5).contains(&h) => continue,
            // On land, away from outposts.
            Habitat::Land if h < crate::water::surface(p, Some(h)) + 0.5 || outposts.at(p).is_some() => continue,
            _ => {}
        }
        let slope = (heights.height_at(p + Vec3::X * 2.0).unwrap_or(h) - h).abs() + (heights.height_at(p + Vec3::Z * 2.0).unwrap_or(h) - h).abs();
        if slope > 2.0 {
            continue;
        }
        return Some(Vec3::new(p.x, h, p.z));
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn spawn_animals(
    args: Res<crate::Args>,
    time: Res<Time>,
    mut timer: Local<f32>,
    assets: Option<Res<AnimalAssets>>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    heights: Res<Heightfield>,
    outposts: Res<crate::outposts::Outposts>,
    players: Query<&Transform, With<Player>>,
    animals: Query<(), With<WildAnimal>>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
) {
    *timer -= time.delta_secs();
    if *timer > 0.0 || args.animal_test.is_some() {
        return;
    }
    *timer = 3.0;
    let Some(assets) = assets else { return };
    let Ok(pt) = players.single() else { return };
    if animals.iter().count() >= MAX_ANIMALS {
        return;
    }
    // Pick a species by weight, then place a small group.
    let total: f32 = SPECIES.iter().map(|s| s.weight).sum();
    let mut roll = rng.f32() * total;
    let mut species = 0;
    for (i, s) in SPECIES.iter().enumerate() {
        if roll < s.weight {
            species = i;
            break;
        }
        roll -= s.weight;
    }
    let s = &SPECIES[species];
    // Sharks only where the player is near or in the sea.
    if s.habitat == Habitat::Sea && heights.height_at(pt.translation).is_some_and(|h| h > crate::sky::SEA_LEVEL + 3.0) {
        return;
    }
    let Some(model) = models.get(&assets.models[species]) else { return };
    if !crate::cache_assets::usable(&server, assets.models[species].id()) || !assets.sets[species].ready(&server) {
        return;
    }
    let Some(centre) = random_ground(&mut rng, pt.translation, &heights, &outposts, s.habitat) else { return };
    let group = match s.behaviour {
        Behaviour::Predator => 1 + (rng.f32() * 1.6) as usize,
        _ => 1 + (rng.f32() * 3.0) as usize,
    };
    for k in 0..group {
        let offset = Vec3::new(rng.range(-6.0, 6.0), 0.0, rng.range(-6.0, 6.0)) * (k as f32).min(1.0);
        let at = centre + offset;
        let ground = if s.habitat == Habitat::Sea { centre.y } else { heights.height_at(at).unwrap_or(centre.y) };
        let feet = Vec3::new(at.x, ground, at.z);
        let yaw = rng.f32() * std::f32::consts::TAU;
        spawn_animal(&mut commands, &assets, model, species, feet, yaw, Mood::Graze, &mut rng);
    }
}

fn spawn_animal(commands: &mut Commands, assets: &AnimalAssets, model: &Fc3Model, species: usize, feet: Vec3, yaw: f32, mood: Mood, rng: &mut Rng) -> Entity {
    let s = &SPECIES[species];
    let (radius, half) = s.size;
    let root = commands
        .spawn((
            WildAnimal {
                species,
                mood,
                call_in: rng.range(CALL_EVERY.0, CALL_EVERY.1),
                health_seen: f32::MAX,
                timer: rng.range(2.0, 8.0),
                goal: feet,
                yaw,
                velocity: Vec3::ZERO,
                rig: None,
                attack_cooldown: 0.0,
                frozen: None,
                body_lift: radius + 0.25,
                sense: Sensing::default(),
            },
            Health::new(s.health),
            RigidBody::Kinematic,
            // Lying along the body: a capsule turned to point forward.
            Collider::capsule(radius, half * 2.0),
            Transform::from_translation(feet + Vec3::Y * (radius + 0.25)).with_rotation(Quat::from_rotation_y(yaw)),
            Visibility::Visible,
            Name::new(s.animal.name()),
        ))
        .id();
    if let Some(rig) =
        spawn_rig_with(commands, root, &[model], None, assets.sets[species].clone(), Vec3::new(0.0, -(radius + 0.25), 0.0), assets.skins[species].clone())
    {
        commands.entity(root).entry::<WildAnimal>().and_modify(move |mut a| a.rig = Some(rig));
    }
    root
}

/// `FC3_CAGE_TEST`: shoots the nearest cage door open 20 s in, then logs the
/// animals near it every few seconds.
fn cage_test(
    time: Res<Time>,
    mut next: Local<f32>,
    doors: Query<(Entity, &GlobalTransform), With<CageDoor>>,
    players: Query<(Entity, &Transform), With<Player>>,
    animals: Query<(&Transform, &WildAnimal, &Health)>,
    people: Query<(&Transform, &Health), With<crate::npc::Npc>>,
    mut damage: MessageWriter<DamageEvent>,
) {
    if std::env::var("FC3_CAGE_TEST").is_err() || time.elapsed_secs() < 20.0 || time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 3.0;
    let Ok((pe, pt)) = players.single() else { return };
    if let Some((door, gt)) = doors.iter().min_by(|a, b| a.1.translation().distance(pt.translation).total_cmp(&b.1.translation().distance(pt.translation))) {
        info!("cage-test: shooting the door at {:.0?}", gt.translation());
        damage.write(DamageEvent {
            target: door,
            amount: 100.0,
            kind: DamageKind::Bullet,
            source: Some(pt.translation),
            instigator: Some(pe),
            headshot: false,
            hit: crate::combat::Hit::Whole,
        });
        return;
    }
    for (t, a, h) in &animals {
        if t.translation.distance(pt.translation) < 80.0 && SPECIES[a.species].behaviour == Behaviour::Predator {
            let near = people.iter().filter(|(p, ph)| !ph.dead && p.translation.distance(t.translation) < 30.0).count();
            info!("cage-test: {} {:?} at {:.0?} health {:.0}, {near} people within 30 m", SPECIES[a.species].animal.name(), a.mood, t.translation, h.current);
        }
    }
}

/// A broken cage door: gone for good, and out comes a big cat (a bear on the
/// southern island), already on the hunt.
#[allow(clippy::too_many_arguments)]
fn release_caged(
    mut died: MessageReader<crate::combat::Died>,
    doors: Query<(&CageDoor, &GlobalTransform)>,
    mut world: ResMut<crate::pickups::WorldState>,
    assets: Option<Res<AnimalAssets>>,
    models: Res<Assets<Fc3Model>>,
    heights: Res<Heightfield>,
    mut rng: ResMut<Rng>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut commands: Commands,
) {
    for d in died.read() {
        let Ok((door, gt)) = doors.get(d.entity) else { continue };
        world.removed.insert(door.id);
        commands.entity(d.entity).despawn();
        let Some(assets) = assets.as_ref() else { continue };
        let at = gt.translation();
        let animal = if at.z > 600.0 { Animal::Bear } else { Animal::Tiger };
        let Some(species) = SPECIES.iter().position(|s| s.animal == animal) else { continue };
        let Some(model) = models.get(&assets.models[species]) else { continue };
        // Out through the doorway, from just inside it.
        let out = gt.rotation() * Vec3::Z;
        let inside = at - Vec3::new(out.x, 0.0, out.z).normalize_or(Vec3::X) * 1.2;
        let feet = Vec3::new(inside.x, heights.height_at(inside).unwrap_or(at.y), inside.z);
        let e = spawn_animal(&mut commands, assets, model, species, feet, yaw_to(out), Mood::Hunt, &mut rng);
        commands.entity(e).entry::<WildAnimal>().and_modify(|mut a| a.timer = 20.0);
        let voice = calls(animal);
        if voice.alarm != 0 {
            sfx.write(crate::sound::PlaySfx { event: voice.alarm, at: Some(feet), volume: 1.0 });
        }
        info!("cage opened at {at:.0?}: {}", animal.name());
    }
}

fn yaw_to(v: Vec3) -> f32 {
    (-v.x).atan2(-v.z)
}

/// Moves off from `from`, some way within `range` (metres).
fn move_off(a: &mut WildAnimal, rng: &mut Rng, pos: Vec3, from: Vec3, range: (f32, f32), mood: Mood) {
    a.mood = mood;
    let away = Vec3::new(pos.x - from.x, 0.0, pos.z - from.z).normalize_or(Vec3::X);
    a.goal = pos + away * rng.range(range.0, range.1.max(range.0));
    // (Given up if it cannot get there.)
    a.timer = 15.0;
}

/// Aware of something at `at`: stops and watches it, relaxing once nothing
/// has been sensed for `relax_after` seconds.
fn watch(a: &mut WildAnimal, at: Vec3, relax_after: f32) {
    a.mood = Mood::Watch;
    a.goal = at;
    a.timer = 0.0;
    a.sense.aware = 0.0;
    a.sense.lost = 0.0;
    a.sense.relax_after = relax_after;
    a.sense.burst = 0;
}

#[allow(clippy::too_many_arguments)]
fn animal_ai(
    time: Res<Time>,
    spatial: SpatialQuery,
    cinema: Option<Res<crate::cinema::Cinema>>,
    heights: Res<Heightfield>,
    mut rng: ResMut<Rng>,
    mut noises: MessageReader<crate::npc::Noise>,
    players: Query<(Entity, &Transform, &Player, &Health, Option<&crate::weapons::Inventory>), Without<WildAnimal>>,
    people: Query<(Entity, &Transform, &Health), (With<crate::npc::Npc>, Without<WildAnimal>, Without<Player>)>,
    mut animals: Query<
        (Entity, &mut Transform, &mut WildAnimal, &Health, Option<&crate::story_actors::StoryActor>, Option<&crate::combat::Counters>),
        Without<Carcass>,
    >,
    mut anims: Query<&mut AnimRequest>,
    mut damage: MessageWriter<DamageEvent>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    (boosts, table, sense_table): (Option<Res<crate::syringes::Boosts>>, Res<AnimalTable>, Res<crate::npc::SenseTable>),
    (driving, vehicles): (Res<crate::vehicles::Driving>, Query<(&crate::vehicles::Vehicle, &LinearVelocity)>),
) {
    let dt = time.delta_secs();
    let Ok((pe, pt, player, ph, inventory)) = players.single() else { return };
    let debug = std::env::var_os("FC3_ANIMAL_DEBUG").is_some();
    // (The animal repellent syringe: they leave the player be.)
    let repellent = boosts.as_ref().is_some_and(|b| b.active(crate::syringes::Syringe::AnimalRepellent));
    let mut call = |event: u32, at: Vec3, volume: f32| {
        if event != 0 {
            sfx.write(crate::sound::PlaySfx { event, at: Some(at), volume });
        }
    };
    let noises: Vec<crate::npc::Noise> = noises.read().copied().collect();
    // Pirates, privateers and rebels nearby are prey (or a threat) too.
    let npcs: Vec<(Entity, Vec3)> =
        people.iter().filter(|(_, t, h)| !h.dead && t.translation.distance(pt.translation) < 160.0).map(|(e, t, _)| (e, t.translation)).collect();
    // The player's posture for their senses, and the vehicle he drives (its
    // speed against its top speed).
    let player_posture = crate::npc::posture(player, inventory);
    let ride = driving.vehicle.and_then(|v| vehicles.get(v).ok()).map(|(v, lv)| (lv.0.length() / v.top_speed().max(1.0)).min(1.0));
    for (e, mut t, mut a, h, story, counters) in &mut animals {
        if h.dead || story.is_some_and(|s| s.script_driven()) {
            continue;
        }
        if let Some(kind) = a.frozen {
            if let Some(mut req) = a.rig.and_then(|r| anims.get_mut(r).ok()) {
                *req = AnimRequest { kind, rate: 1.0 };
            }
            continue;
        }
        let s = &SPECIES[a.species];
        let p = &table.0[a.species];
        let behaviour = behaviour(p);
        let sight = p.senses.as_deref().and_then(|n| sense_table.named(n));
        let pos = t.translation;
        let voice = calls(s.animal);
        let mood_before = a.mood;
        // A cry when hurt (the death call is the death's; health set from
        // the game's counters is no hurt).
        let hurt_now = h.current < a.health_seen && h.current < h.max && a.health_seen != f32::MAX;
        if hurt_now {
            call(voice.hurt, pos, 1.0);
        }
        a.health_seen = h.current;
        // Now and then, a call from one near enough to hear.
        a.call_in -= dt;
        if a.call_in <= 0.0 {
            a.call_in = rng.range(CALL_EVERY.0, CALL_EVERY.1);
            if pos.distance(pt.translation) < CALL_RANGE && matches!(a.mood, Mood::Graze | Mood::Wander | Mood::Hunt) {
                call(voice.idle, pos, 0.8);
            }
        }
        // The nearest one it could notice: the player, or a soldier (sea
        // animals only go for swimmers). Horizontal steering does not imply
        // horizontal attack range: a surface animal cannot bite a player in
        // the cave beneath it.
        let player_dist = pt.translation.distance(pos);
        let player_available = !ph.dead && !player.noclip && !player.spawning && !repellent && cinema.as_ref().is_none_or(|c| c.view.is_none());
        let mut target = player_available.then_some((pe, pt.translation, player_dist));
        if s.habitat != Habitat::Sea {
            for (ne, np) in &npcs {
                let d = np.distance(pos);
                if target.is_none_or(|t| d < t.2) {
                    target = Some((*ne, *np, d));
                }
            }
        }
        let (target_e, target_pos, dist) = target.unwrap_or((pe, pos, f32::MAX));
        let is_player = target_e == pe;
        let to_target = target_pos - pos;
        a.timer -= dt;
        a.attack_cooldown -= dt;
        // Sharks only go for someone in the water.
        let reachable = target.is_some() && (s.habitat != Habitat::Sea || player.swimming);
        let ph_dead = is_player && ph.dead;
        // What it senses of them: nothing too far above or below; by smell
        // (nearer for a stealthy player, and ignored while idle by some), at
        // once when very near, a moving vehicle (farther the faster), and by
        // sight in its sensory system's time for their posture and its state.
        let posture = if is_player { player_posture } else { 1 };
        let level = match a.mood {
            Mood::Graze | Mood::Wander => 0,
            Mood::Watch | Mood::Avoid => 1,
            Mood::Flee | Mood::Hunt | Mood::Attack => 2,
        };
        let near = reachable && !ph_dead && (p.max_height <= 0.0 || to_target.y.abs() <= p.max_height);
        let immediate = near && dist < p.immediate;
        let smelled = near && dist < p.smell_for(posture == 0) && !(level == 0 && p.ignore_while_idle);
        let driven = if is_player && player.driving { ride } else { None };
        let vehicle = near && driven.is_some_and(|speed| p.vehicle < 0.0 || dist < p.vehicle * speed);
        let mut seen = false;
        match sight.and_then(|v| v.get(posture, level).time(dist).map(|need| (need, v.sixth_sense.0))) {
            Some((need, sixth)) if near => {
                let facing = Vec3::new(-a.yaw.sin(), 0.0, -a.yaw.cos());
                let ahead = facing.dot(Vec3::new(to_target.x, 0.0, to_target.z).normalize_or_zero()) > SIGHT_COS || dist < sixth;
                let eye = pos + Vec3::Y * 0.3;
                let filter = SpatialQueryFilter::from_excluded_entities([e, target_e]);
                let clear = ahead && Dir3::new(target_pos - eye).is_ok_and(|d| spatial.cast_ray(eye, d, dist, true, &filter).is_none());
                if clear {
                    a.sense.noticing += dt;
                    seen = a.sense.noticing >= need;
                } else {
                    a.sense.noticing = (a.sense.noticing - dt).max(0.0);
                }
            }
            _ => a.sense.noticing = (a.sense.noticing - dt).max(0.0),
        }
        let sensed = seen || smelled || immediate || vehicle;
        // Sounds within its hearing for their kind.
        let (mut interest, mut threat, mut blast) = (None, None, None);
        for n in &noises {
            let cap = match n.kind {
                crate::npc::NoiseKind::Interesting => p.interesting_sound,
                crate::npc::NoiseKind::Threatening => p.threatening_sound,
                crate::npc::NoiseKind::Explosion => p.explosion_sound,
            };
            let reach = if cap < 0.0 { n.radius } else { n.radius.min(cap) };
            if n.at.distance(pos) < reach {
                *match n.kind {
                    crate::npc::NoiseKind::Interesting => &mut interest,
                    crate::npc::NoiseKind::Threatening => &mut threat,
                    crate::npc::NoiseKind::Explosion => &mut blast,
                } = Some(n.at);
            }
        }
        // Acting on them: predators hunt; defensive animals charge when
        // pressed (too near, or hurt) and otherwise keep off; the rest keep
        // off or, pressed, run.
        let act = |a: &mut WildAnimal, rng: &mut Rng, pressed: bool| match behaviour {
            Behaviour::Predator => {
                a.mood = Mood::Hunt;
                a.sense.lost = 0.0;
            }
            Behaviour::Defensive if pressed => {
                a.mood = Mood::Hunt;
                a.sense.lost = 0.0;
                a.sense.aggro = p.defensive_stop;
            }
            Behaviour::Defensive | Behaviour::Prey if !pressed => move_off(a, rng, pos, target_pos, p.avoid, Mood::Avoid),
            _ => move_off(a, rng, pos, target_pos, p.flee, Mood::Flee),
        };
        // Explosions scare them all off, gunfire all but the predators.
        let scare = blast.or(threat.filter(|_| behaviour != Behaviour::Predator));
        match a.mood {
            Mood::Graze | Mood::Wander => {
                if hurt_now || immediate {
                    act(&mut a, &mut rng, true);
                } else if let Some(at) = scare {
                    move_off(&mut a, &mut rng, pos, at, p.flee, Mood::Flee);
                } else if sensed {
                    watch(&mut a, target_pos, p.aware_cooldown);
                } else if let Some(at) = interest.or(threat) {
                    watch(&mut a, at, p.aware_cooldown);
                }
            }
            Mood::Watch => {
                // (A pause after a burst of attacks holds off the next.)
                if hurt_now || (immediate && a.timer <= 0.0) {
                    act(&mut a, &mut rng, true);
                } else if let Some(at) = scare {
                    move_off(&mut a, &mut rng, pos, at, p.flee, Mood::Flee);
                } else if sensed {
                    a.goal = target_pos;
                    a.sense.lost = 0.0;
                    a.sense.aware += dt;
                    let need = if driven.is_some() && p.aware_to_active_vehicle > 0.0 { p.aware_to_active_vehicle } else { p.aware_to_active };
                    if a.sense.aware >= need && a.timer <= 0.0 {
                        act(&mut a, &mut rng, false);
                    }
                } else {
                    a.sense.lost += dt;
                    if a.sense.lost >= a.sense.relax_after {
                        a.mood = Mood::Graze;
                        a.timer = rng.range(2.0, 6.0);
                    }
                }
            }
            Mood::Avoid | Mood::Flee => {
                let bolting = a.mood == Mood::Flee && behaviour == Behaviour::Prey;
                if hurt_now || (immediate && !bolting) {
                    act(&mut a, &mut rng, true);
                } else if a.timer <= 0.0 {
                    watch(&mut a, target_pos, p.avoid_cooldown);
                }
            }
            Mood::Hunt => {
                a.goal = target_pos;
                if sensed {
                    a.sense.lost = 0.0;
                } else {
                    a.sense.lost += dt;
                }
                if behaviour == Behaviour::Defensive {
                    if hurt_now || threat.is_some() {
                        a.sense.aggro = p.defensive_stop;
                    }
                    a.sense.aggro -= dt;
                }
                if ph_dead || !reachable || a.sense.lost >= p.aware_cooldown.max(1.0) {
                    watch(&mut a, target_pos, p.aware_cooldown);
                } else if behaviour == Behaviour::Defensive && a.sense.aggro <= 0.0 {
                    move_off(&mut a, &mut rng, pos, target_pos, p.avoid, Mood::Avoid);
                } else if dist < p.attack_reach.max(1.2) {
                    a.mood = Mood::Attack;
                }
            }
            Mood::Attack => {
                a.goal = target_pos;
                let reach = p.attack_reach.max(1.2);
                if a.sense.burst == 0 {
                    let (lo, hi) = (p.attacks_before_pause.0.max(1), p.attacks_before_pause.1);
                    a.sense.burst = lo + (rng.f32() * (hi.saturating_sub(lo) + 1) as f32) as u32;
                    a.sense.strikes = 0;
                }
                if a.sense.strikes >= a.sense.burst {
                    // A burst done: once the swing ends it stops to look.
                    if a.attack_cooldown <= 0.0 {
                        watch(&mut a, target_pos, p.aware_cooldown);
                        a.timer = p.aware_to_active;
                    }
                } else if a.attack_cooldown <= 0.0
                    && dist < reach + 0.4
                    && to_target.y.abs() < p.attack_height.max(1.0) + 0.5
                    && !ph_dead
                    && reachable
                    && spatial
                        .cast_ray(pos, Dir3::new(to_target).unwrap_or(Dir3::X), dist, true, &SpatialQueryFilter::from_excluded_entities([e, target_e]))
                        .is_none()
                {
                    a.attack_cooldown = ATTACK_SWING + rng.range(p.attack_delay.0, p.attack_delay.1.max(p.attack_delay.0));
                    a.sense.strikes += 1;
                    if a.sense.strikes >= a.sense.burst {
                        a.attack_cooldown = ATTACK_SWING;
                    }
                    // The game's attack: a share of the player's health (less
                    // for each health bar gained), else within its range.
                    let amount = match counters.and_then(|c| c.v.attack) {
                        Some(attack) => attack
                            .to_player(ph.max, ph.max / crate::combat::HEALTH_SEGMENT - BASE_HEALTH_BARS)
                            .filter(|_| is_player)
                            .unwrap_or_else(|| rng.range(attack.min, attack.max.max(attack.min))),
                        None => s.damage,
                    };
                    damage.write(DamageEvent {
                        target: target_e,
                        amount,
                        kind: DamageKind::Melee,
                        source: Some(pos),
                        instigator: Some(e),
                        headshot: false,
                        hit: crate::combat::Hit::Whole,
                    });
                }
                if a.mood == Mood::Attack {
                    if !reachable || ph_dead {
                        watch(&mut a, target_pos, p.aware_cooldown);
                    } else if dist > reach + 1.0 && a.sense.strikes < a.sense.burst {
                        a.mood = Mood::Hunt;
                    }
                }
            }
        }
        // Bolting, or going for someone: the alarm call.
        if a.mood != mood_before && matches!(a.mood, Mood::Flee | Mood::Hunt) && !matches!(mood_before, Mood::Flee | Mood::Hunt | Mood::Attack) {
            call(voice.alarm, pos, 1.0);
        }
        // FC3_ANIMAL_DEBUG: each change of mood and what it sensed.
        if debug && a.mood != mood_before {
            info!(
                "animal: {} {mood_before:?} -> {:?} at {dist:.1} m (seen {seen}, smelled {smelled}, immediate {immediate}, vehicle {vehicle}, hurt {hurt_now}, sounds {}/{}/{})",
                s.animal.name(),
                a.mood,
                interest.is_some(),
                threat.is_some(),
                blast.is_some()
            );
        }
        let (mut speed, mut anim) = (0.0, AnimKind::Idle);
        match a.mood {
            Mood::Graze | Mood::Wander if a.timer <= 0.0 => {
                // Now and then a wander, else more grazing.
                if a.mood == Mood::Graze && rng.f32() < p.wander_chance {
                    a.mood = Mood::Wander;
                    let angle = rng.f32() * std::f32::consts::TAU;
                    a.goal = pos + Vec3::new(angle.cos(), 0.0, angle.sin()) * rng.range(p.wander.0, p.wander.1.max(p.wander.0));
                    a.timer = rng.range(5.0, 10.0);
                } else {
                    a.mood = Mood::Graze;
                    a.timer = rng.range(4.0, 12.0);
                }
            }
            Mood::Graze => anim = AnimKind::Eat,
            Mood::Wander => {
                speed = s.walk_speed;
                anim = AnimKind::Walk;
            }
            Mood::Watch => {
                let look = Vec3::new(a.goal.x - pos.x, 0.0, a.goal.z - pos.z);
                if look.length() > 0.5 {
                    a.yaw = turn_toward(a.yaw, yaw_to(look), dt * 3.0);
                }
            }
            Mood::Avoid | Mood::Flee => {
                speed = s.run_speed;
                anim = AnimKind::Run;
            }
            Mood::Hunt => {
                speed = s.run_speed * 0.85;
                anim = AnimKind::Run;
            }
            Mood::Attack => {
                anim = AnimKind::Attack;
                a.yaw = yaw_to(Vec3::new(to_target.x, 0.0, to_target.z));
            }
        }
        // Steer toward the goal over the ground.
        let mut dir = Vec3::new(a.goal.x - pos.x, 0.0, a.goal.z - pos.z);
        if speed > 0.0 && dir.length() > 0.8 {
            dir = dir.normalize();
            a.yaw = turn_toward(a.yaw, yaw_to(dir), dt * 3.0);
            let fwd = Vec3::new(-a.yaw.sin(), 0.0, -a.yaw.cos());
            a.velocity = a.velocity.lerp(fwd * speed, (dt * 4.0).min(1.0));
        } else {
            a.velocity = a.velocity.lerp(Vec3::ZERO, (dt * 6.0).min(1.0));
            match a.mood {
                Mood::Wander if speed > 0.0 => {
                    a.mood = Mood::Graze;
                    a.timer = rng.range(4.0, 10.0);
                }
                // Far enough off: it stops and looks back.
                Mood::Avoid | Mood::Flee if speed > 0.0 => watch(&mut a, target_pos, p.avoid_cooldown),
                _ => {}
            }
        }
        let mut next = pos + a.velocity * dt;
        let sea = crate::sky::SEA_LEVEL;
        // Rivers and lakes as well as the sea for those on land and shore.
        let water = heights.height_at(next).map_or(sea, |g| crate::water::surface(next, Some(g)));
        let lift = a.body_lift;
        match (s.habitat, heights.height_at(next)) {
            // Sharks keep to deep water, below the surface (or up to a swimmer).
            (Habitat::Sea, Some(g)) if g > sea - 3.0 => {
                next = pos;
                a.velocity = Vec3::ZERO;
                a.goal = pos - (a.goal - pos);
            }
            (Habitat::Sea, _) => next.y = if a.mood == Mood::Attack { pt.translation.y.min(sea - 0.5) } else { sea - SWIM_DEPTH },
            // Crocodiles wade into the shallows; nothing else enters the sea.
            (Habitat::Shore, Some(g)) if g < water - 3.0 => {
                next = pos;
                a.velocity = Vec3::ZERO;
                a.goal = pos - (a.goal - pos);
            }
            (Habitat::Shore, Some(g)) => next.y = g.max(water - 0.7) + lift,
            (Habitat::Land, Some(g)) if g < water => {
                next = pos;
                a.velocity = Vec3::ZERO;
                a.goal = pos - (a.goal - pos);
            }
            (_, Some(g)) => next.y = g + lift,
            (_, None) => {}
        }
        t.translation = next;
        t.rotation = Quat::from_rotation_y(a.yaw);
        if let Some(mut req) = a.rig.and_then(|r| anims.get_mut(r).ok()) {
            let moving = Vec2::new(a.velocity.x, a.velocity.z).length();
            let rate = match anim {
                AnimKind::Run => (moving / s.run_speed).clamp(0.6, 1.3),
                AnimKind::Walk => (moving / s.walk_speed).clamp(0.6, 1.4),
                _ => 1.0,
            };
            *req = AnimRequest { kind: anim, rate };
        }
    }
}

fn turn_toward(from: f32, to: f32, max: f32) -> f32 {
    let d = (to - from + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    from + d.clamp(-max, max)
}

fn animal_deaths(
    mut died: MessageReader<Died>,
    players: Query<Entity, With<Player>>,
    animals: Query<&WildAnimal>,
    mut anims: Query<&mut AnimRequest>,
    mut xp: MessageWriter<crate::progression::XpGain>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    transforms: Query<&Transform>,
    mut commands: Commands,
    crafting: Option<Res<crate::crafting::CraftCatalog>>,
) {
    let player = players.single().ok();
    for d in died.read() {
        let Ok(a) = animals.get(d.entity) else { continue };
        let death = calls(SPECIES[a.species].animal).death;
        if death != 0 {
            sfx.write(crate::sound::PlaySfx { event: death, at: transforms.get(d.entity).ok().map(|t| t.translation), volume: 1.0 });
        }
        if let Some(mut req) = a.rig.and_then(|r| anims.get_mut(r).ok()) {
            *req = AnimRequest { kind: AnimKind::Death, rate: 1.0 };
        }
        commands.entity(d.entity).insert(Carcass { species: a.species, age: 0.0, skinned: false }).remove::<(Collider, RigidBody)>();
        // (Each animal's own XP.)
        let amount = crafting.as_ref().map_or(0, |c| c.animal_xp(a.animal()));
        if d.killer.is_some() && d.killer == player && amount > 0 {
            xp.write(crate::progression::XpGain { amount, reason: "Hunt" });
        }
    }
}

/// Skinning a carcass yields one skin of its species.
#[allow(clippy::too_many_arguments)]
fn skin_animals(
    mut actions: ResMut<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    players: Query<(&Transform, &Health), With<Player>>,
    mut carcasses: Query<(Entity, &GlobalTransform, &mut Carcass)>,
    mut progress: ResMut<crate::progression::Progress>,
    mut hud: ResMut<crate::hud::HudState>,
    mut looted: MessageWriter<crate::progression::ItemLooted>,
    (busy, mut gestures, scripted): (Res<crate::fp::HandsBusy>, MessageWriter<crate::fp::FpAction>, Res<crate::story::ScriptedPlayer>),
    crafting: Option<Res<crate::crafting::CraftCatalog>>,
    perks: Option<Res<crate::progression::Perks>>,
) {
    let Ok((pt, h)) = players.single() else { return };
    if h.dead || hud.prompt.is_some() || busy.busy() || !scripted.animal_looting {
        return;
    }
    let Some((_, _, mut c)) = carcasses
        .iter_mut()
        .filter(|(_, gt, c)| !c.skinned && gt.translation().distance(pt.translation) < 2.8)
        .min_by(|a, b| a.1.translation().distance(pt.translation).total_cmp(&b.1.translation().distance(pt.translation)))
    else {
        return;
    };
    let animal = SPECIES[c.species].animal;
    if crate::crafting::loot_count(&progress) >= crafting.as_ref().map_or(16, |c| c.loot_capacity(&progress)) {
        hud.prompt = Some("Rucksack full - use or sell ingredients".into());
        if actions.just_pressed(crate::controls::Action::Interact) { actions.consume(crate::controls::Action::Interact); }
        return;
    }
    hud.prompt = Some(format!("{}  skin {}", actions.label(crate::controls::Action::Interact, &settings), animal.name().to_lowercase()));
    if actions.just_pressed(crate::controls::Action::Interact) {
        // (Zoology 101: two skins from each.)
        let n = if perks.as_ref().is_some_and(|p| p.double_skins) { 2 } else { 1 };
        *progress.skins.entry(animal).or_insert(0) += n;
        looted.write(crate::progression::ItemLooted(crate::progression::LootKind::Skin(animal)));
        actions.consume(crate::controls::Action::Interact);
        hud.notify(format!("+{n} {} skin", animal.name()));
        // The skinning plays out with the machete; the carcass stays.
        c.skinned = true;
        gestures.write(crate::fp::FpAction::Skin { large: SPECIES[c.species].size.0 >= 0.5 });
    }
}

fn despawn_animals(
    time: Res<Time>,
    players: Query<&Transform, With<Player>>,
    mut animals: Query<(Entity, &Transform, Option<&mut Carcass>), (With<WildAnimal>, Without<crate::story_actors::StoryActor>)>,
    mut commands: Commands,
) {
    let Ok(pt) = players.single() else { return };
    for (e, t, carcass) in &mut animals {
        let far = t.translation.distance(pt.translation) > DESPAWN;
        let rotten = carcass.is_some_and(|mut c| {
            c.age += time.delta_secs();
            c.age > 180.0
        });
        if far || rotten {
            commands.entity(e).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::{Action, ActionState};
    use crate::progression::{ItemLooted, LootKind, Progress};

    fn player() -> Player {
        Player {
            velocity: Vec3::ZERO, yaw: 0.0, pitch: 0.0, grounded: true,
            crouching: false, swimming: false, noclip: false, spawning: false,
            keep_height: false, last_landing_speed: 0.0, sprinting: false,
            spawned_at: 0.0, driving: false, eye: crate::player::EYE_HEIGHT,
            air: crate::player::AirMode::None, mantle: None, mounted: None,
            sliding: None, dragging: false,
        }
    }

    #[test]
    fn story_species_recognizes_placed_normal_families_without_flattening_pigs_or_rares() {
        for (name, animal) in [
            ("FC3_Quadrupeds/Boar.Boar_normal.Boar_Jas_080_2", Animal::Boar),
            ("FC3_Quadrupeds/Boar_0", Animal::Boar),
            ("FC3_Quadrupeds/DeerFC3_18", Animal::Deer),
            ("FC3_Quadrupeds/AsianBlackBear.Bear_Jas_020_0", Animal::Bear),
            ("FC3_Quadrupeds/Dogs_Rapid.Black_0", Animal::Dog),
            ("FC3_Aquatic/BullShark_0", Animal::Shark),
        ] {
            assert_eq!(SPECIES[story_species(name).unwrap()].animal, animal, "{name}");
        }
        for name in ["FC3_Domestic/Pig_0", "FC3_Quadrupeds/Pig_Wild_0", "FC3_Quadrupeds/Boar.Boar_boss_rare_1",
            "FC3_Quadrupeds/Leopard.Panther_rare_0", "Merc/UniqueMerc_FC3.Grant_0", "BoarDecoration_0"] {
            assert!(story_species(name).is_none(), "{name}");
        }
    }

    #[test]
    fn bound_story_boars_keep_their_id_and_offset_and_die_into_skinnable_carcasses() {
        use crate::story_actors::StoryActor;
        fn bind(assets: Res<AnimalAssets>, roots: Query<(Entity, &Transform), Added<StoryActor>>, mut commands: Commands, mut rng: ResMut<Rng>) {
            for (root, at) in &roots {
                assets.bind_story(&mut commands, root, story_species("FC3_Quadrupeds/Boar_0").unwrap(), None, at.translation, 0.0, &mut rng);
            }
        }
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default(), bevy::mesh::MeshPlugin,
            bevy::transform::TransformPlugin, PhysicsPlugins::default()))
            .init_resource::<ActionState>().init_resource::<crate::settings::Settings>()
            .init_resource::<Progress>().init_resource::<crate::hud::HudState>()
            .init_resource::<crate::fp::HandsBusy>().init_resource::<crate::story::ScriptedPlayer>()
            .init_resource::<Heightfield>()
            .init_resource::<AnimalTable>().init_resource::<crate::npc::SenseTable>().init_resource::<crate::vehicles::Driving>()
            .insert_resource(Rng::seeded(1)).insert_resource(AnimalAssets {
                models: Vec::new(), skins: Vec::new(),
                sets: SPECIES.iter().map(|_| Arc::new(ClipSet { skeleton: Handle::default(), clips: std::collections::HashMap::new() })).collect(),
            })
            .add_message::<ItemLooted>().add_message::<crate::fp::FpAction>().add_message::<Died>()
            .add_message::<crate::progression::XpGain>().add_message::<crate::sound::PlaySfx>()
            .add_message::<crate::npc::Noise>().add_message::<DamageEvent>()
            .add_systems(Update, (bind, animal_ai, animal_deaths, skin_animals, despawn_animals).chain());
        let player = app.world_mut().spawn((player(), Transform::from_xyz(0.0, crate::player::HALF_HEIGHT, 0.0), Health::new(100.0))).id();
        let position = Transform::from_xyz(1.0, crate::player::HALF_HEIGHT, 0.0);
        let mut actor = StoryActor::new(42, None, false);
        actor.in_scene = true;
        let root = app.world_mut().spawn((actor, position, GlobalTransform::from(position), Health::new(150.0), RigidBody::Kinematic)).id();
        app.update();
        let animal = app.world().get::<WildAnimal>(root).unwrap();
        assert_eq!(SPECIES[animal.species].animal, Animal::Boar);
        assert_eq!(animal.body_lift, crate::player::HALF_HEIGHT);
        assert_eq!(app.world().get::<Health>(root).unwrap().max, SPECIES[animal.species].health);
        assert_eq!(app.world().get::<StoryActor>(root).unwrap().id, 42);
        assert_eq!(*app.world().get::<Transform>(root).unwrap(), position);
        assert!(matches!(animal.mood, Mood::Graze)); // The scene retains control.
        assert!(app.world().get::<Collider>(root).is_some());
        app.world_mut().get_mut::<StoryActor>(root).unwrap().in_scene = false;
        app.update();
        assert!(matches!(app.world().get::<WildAnimal>(root).unwrap().mood, Mood::Hunt | Mood::Attack));
        app.init_resource::<crate::cinema::Cinema>();
        for unavailable in 0..4 {
            app.world_mut().get_mut::<Player>(player).unwrap().noclip = unavailable == 0;
            app.world_mut().get_mut::<Player>(player).unwrap().spawning = unavailable == 1;
            app.world_mut().get_mut::<Health>(player).unwrap().dead = unavailable == 2;
            app.world_mut().resource_mut::<crate::cinema::Cinema>().view =
                (unavailable == 3).then_some((Transform::IDENTITY, 60.0));
            let mut animal = app.world_mut().get_mut::<WildAnimal>(root).unwrap();
            animal.mood = Mood::Attack;
            animal.attack_cooldown = 0.0;
            app.world_mut().resource_mut::<Messages<DamageEvent>>().clear();
            app.update();
            assert!(app.world().resource::<Messages<DamageEvent>>().is_empty(), "unavailable player case {unavailable}");
            // (It stops and watches, relaxing after its aware cooldown.)
            assert!(matches!(app.world().get::<WildAnimal>(root).unwrap().mood, Mood::Watch));
        }
        // Leaving the cinematic restores ordinary combat, not invulnerability.
        app.world_mut().resource_mut::<crate::cinema::Cinema>().view = None;
        let original_player = *app.world().get::<Transform>(player).unwrap();
        for separation in [-56.0, -3.0, 3.0, 56.0] {
            let animal_pos = app.world().get::<Transform>(root).unwrap().translation;
            app.world_mut().get_mut::<Transform>(player).unwrap().translation = animal_pos + Vec3::Y * separation;
            let mut animal = app.world_mut().get_mut::<WildAnimal>(root).unwrap();
            animal.mood = Mood::Attack;
            animal.attack_cooldown = 0.0;
            app.world_mut().resource_mut::<Messages<DamageEvent>>().clear();
            app.update();
            assert!(app.world().resource::<Messages<DamageEvent>>().is_empty(), "vertical melee at {separation} metres");
            assert!(!matches!(app.world().get::<WildAnimal>(root).unwrap().mood, Mood::Attack));
        }
        *app.world_mut().get_mut::<Transform>(player).unwrap() = original_player;
        *app.world_mut().get_mut::<Transform>(root).unwrap() = position;
        let wall = app.world_mut().spawn((RigidBody::Static, Collider::cuboid(0.1, 10.0, 6.0),
            Transform::from_xyz(0.5, 1.0, 0.0))).id();
        // Let physics register the barrier before testing the attack itself.
        app.update();
        app.update();
        let mut animal = app.world_mut().get_mut::<WildAnimal>(root).unwrap();
        animal.mood = Mood::Attack;
        animal.attack_cooldown = 0.0;
        app.world_mut().resource_mut::<Messages<DamageEvent>>().clear();
        app.update();
        assert!(app.world().resource::<Messages<DamageEvent>>().is_empty(), "melee crossed a solid wall");
        app.world_mut().despawn(wall);
        app.update();
        app.update();
        app.world_mut().resource_mut::<Messages<DamageEvent>>().clear();
        let mut animal = app.world_mut().get_mut::<WildAnimal>(root).unwrap();
        animal.mood = Mood::Attack;
        animal.attack_cooldown = 0.0;
        app.update();
        assert!(!app.world().resource::<Messages<DamageEvent>>().is_empty());
        app.world_mut().get_mut::<Health>(root).unwrap().dead = true;
        app.world_mut().write_message(Died { entity: root, killer: Some(player), kind: crate::combat::DamageKind::Bullet });
        app.update();
        assert!(app.world().get::<Carcass>(root).is_some());
        assert!(app.world().get::<Collider>(root).is_none());
        assert!(app.world().get::<RigidBody>(root).is_none());
        // Wildlife's generic rot/despawn path must not discard story-owned roots.
        app.world_mut().get_mut::<Carcass>(root).unwrap().age = 181.0;
        app.world_mut().resource_mut::<crate::hud::HudState>().prompt = None;
        app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
        app.update();
        assert_eq!(app.world().get::<StoryActor>(root).unwrap().id, 42);
        assert!(app.world().get::<Carcass>(root).unwrap().skinned);
        assert_eq!(app.world().resource::<Progress>().skins[&Animal::Boar], 1);
        let mut cursor = bevy::ecs::message::MessageCursor::<ItemLooted>::default();
        assert_eq!(cursor.read(app.world().resource::<Messages<ItemLooted>>()).map(|e| e.0).collect::<Vec<_>>(), vec![LootKind::Skin(Animal::Boar)]);
    }

    #[test]
    fn skinning_requires_a_live_player_an_enabled_category_and_free_hands_and_emits_once() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ActionState>().init_resource::<crate::settings::Settings>()
            .init_resource::<Progress>().init_resource::<crate::hud::HudState>()
            .init_resource::<crate::fp::HandsBusy>().init_resource::<crate::story::ScriptedPlayer>()
            .add_message::<ItemLooted>().add_message::<crate::fp::FpAction>()
            .add_systems(Update, skin_animals);
        let player = app.world_mut().spawn((player(), Transform::default(), Health::new(100.0))).id();
        let species = SPECIES.iter().position(|s| s.animal == Animal::Boar).unwrap();
        let carcass = app.world_mut().spawn((GlobalTransform::from_xyz(1.0, 0.0, 0.0), Carcass { species, age: 0.0, skinned: false })).id();
        let mut cursor = bevy::ecs::message::MessageCursor::<ItemLooted>::default();
        app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
        app.world_mut().resource_mut::<crate::story::ScriptedPlayer>().animal_looting = false;
        app.update();
        assert!(app.world().resource::<Progress>().skins.is_empty());
        app.world_mut().resource_mut::<crate::story::ScriptedPlayer>().animal_looting = true;
        app.world_mut().get_mut::<Health>(player).unwrap().dead = true;
        app.update();
        assert!(app.world().resource::<Progress>().skins.is_empty());
        app.world_mut().get_mut::<Health>(player).unwrap().dead = false;
        app.world_mut().resource_mut::<crate::fp::HandsBusy>().0 = 1.0;
        app.update();
        assert!(app.world().resource::<Progress>().skins.is_empty());
        app.world_mut().resource_mut::<crate::fp::HandsBusy>().0 = 0.0;
        app.world_mut().get_mut::<Transform>(player).unwrap().translation = Vec3::splat(20.0);
        app.update();
        assert!(app.world().resource::<Progress>().skins.is_empty());
        app.world_mut().get_mut::<Transform>(player).unwrap().translation = Vec3::ZERO;
        app.world_mut().resource_mut::<ActionState>().clear();
        app.update();
        assert!(app.world().resource::<Progress>().skins.is_empty());
        assert!(!app.world().get::<Carcass>(carcass).unwrap().skinned);
        app.world_mut().resource_mut::<crate::hud::HudState>().prompt = None;
        app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
        app.update();
        assert_eq!(app.world().resource::<Progress>().skins[&Animal::Boar], 1);
        assert!(app.world().get::<Carcass>(carcass).unwrap().skinned);
        assert!(!app.world().resource::<ActionState>().just_pressed(Action::Interact));
        assert_eq!(cursor.read(app.world().resource::<Messages<ItemLooted>>()).map(|e| e.0).collect::<Vec<_>>(), vec![LootKind::Skin(Animal::Boar)]);
        app.world_mut().resource_mut::<crate::hud::HudState>().prompt = None;
        app.update();
        assert_eq!(app.world().resource::<Progress>().skins[&Animal::Boar], 1);
        assert_eq!(cursor.read(app.world().resource::<Messages<ItemLooted>>()).count(), 0);
    }
}

/// `--animal-test <clip>`: every species in a row 8 m ahead, posed.
#[allow(clippy::too_many_arguments)]
fn animal_test(
    args: Res<crate::Args>,
    time: Res<Time>,
    (mut done, mut shot_at): (Local<bool>, Local<Option<f32>>),
    assets: Option<Res<AnimalAssets>>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    heights: Res<Heightfield>,
    players: Query<(&Transform, &Player)>,
    mut noise: MessageWriter<crate::npc::Noise>,
    mut commands: Commands,
) {
    let Some(clip) = args.animal_test.as_deref() else { return };
    let Some(assets) = assets else { return };
    let Ok((pt, p)) = players.single() else { return };
    // "herd": a shot by the player 15 s after they appear.
    if shot_at.is_some_and(|t| time.elapsed_secs() > t) {
        *shot_at = None;
        info!("animal-test: a shot");
        noise.write(crate::npc::Noise { at: pt.translation, radius: 120.0, kind: crate::npc::NoiseKind::Threatening });
    }
    if *done || p.spawning {
        return;
    }
    if !assets.models.iter().all(|m| crate::cache_assets::usable(&server, m.id())) || !assets.sets.iter().all(|s| s.ready(&server)) {
        return;
    }
    *done = true;
    let kind = match clip {
        "walk" => AnimKind::Walk,
        "run" => AnimKind::Run,
        "eat" => AnimKind::Eat,
        "die" => AnimKind::Death,
        "attack" => AnimKind::Attack,
        _ => AnimKind::Idle,
    };
    let fwd = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let right = Vec3::new(-fwd.z, 0.0, fwd.x);
    // "hunt": one live tiger out in front (within its smell), left to its
    // own devices; "herd": the prey and defensive animals in a row 18 m out,
    // live.
    let hunt = clip == "hunt";
    let herd = clip == "herd";
    if herd {
        *shot_at = Some(time.elapsed_secs() + 15.0);
    }
    let mut placed = 0.0;
    for (i, s) in SPECIES.iter().enumerate() {
        if hunt && s.animal != Animal::Tiger {
            continue;
        }
        if herd && (s.behaviour == Behaviour::Predator || s.habitat != Habitat::Land) {
            continue;
        }
        let Some(model) = models.get(&assets.models[i]) else { continue };
        // A grid of five across, rows further out.
        let (col, row) = ((i % 5) as f32, (i / 5) as f32);
        let at = if herd {
            placed += 1.0;
            pt.translation + fwd * 18.0 + right * ((placed - 4.0) * 4.0)
        } else if hunt {
            pt.translation + fwd * 22.0
        } else {
            pt.translation + fwd * (9.0 + row * 5.0) + right * ((col - 2.0) * (3.2 + row * 1.2))
        };
        let ground = heights.height_at(at).unwrap_or(at.y);
        let (radius, half) = s.size;
        // Side-on to the camera.
        let yaw = (-right.x).atan2(-right.z);
        let root = commands
            .spawn((
                WildAnimal {
                    species: i,
                    mood: Mood::Graze,
                    call_in: 3.0,
                    health_seen: f32::MAX,
                    timer: if hunt || herd { 1.0 } else { 99.0 },
                    goal: at,
                    yaw,
                    velocity: Vec3::ZERO,
                    rig: None,
                    attack_cooldown: 0.0,
                    frozen: (!hunt && !herd).then_some(kind),
                    body_lift: radius + 0.25,
                    sense: Sensing::default(),
                },
                Health::new(s.health),
                Collider::capsule(radius, half * 2.0),
                RigidBody::Kinematic,
                Transform::from_xyz(at.x, ground + radius + 0.25, at.z).with_rotation(Quat::from_rotation_y(yaw)),
                Visibility::Visible,
            ))
            .id();
        if let Some(rig) =
            spawn_rig_with(&mut commands, root, &[model], None, assets.sets[i].clone(), Vec3::new(0.0, -(radius + 0.25), 0.0), assets.skins[i].clone())
        {
            commands.entity(root).entry::<WildAnimal>().and_modify(move |mut a| a.rig = Some(rig));
        }
    }
    info!("animal-test: spawned {} species", SPECIES.len());
}
