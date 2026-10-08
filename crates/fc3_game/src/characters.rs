//! Skinned characters: a shared joint rig per character, body parts skinned
//! to it, and clip playback with crossfades.
//!
//! Rigs are built from the cached bone list of the first body part; other
//! parts map their bones into it by name hash (all parts of a character share
//! the same skeleton). Posing uses `fc3_formats::anim::pose_locals`, which also
//! drives the skeleton's procedural helper bones.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::NotShadowCaster;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::prelude::*;
use fc3_formats::anim;
use fc3_formats::cache;
use fc3_formats::hash::name_hash;
use fc3_formats::kits;

use crate::cache_assets::{cache_path, skinned_cache_path, Fc3Clip, Fc3Model, Fc3Skeleton, ModelSettings};
use crate::npc::{Faction, Role};
use crate::weapons::Rng;

pub struct CharactersPlugin;

impl Plugin for CharactersPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ArmsFoley>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_character_assets)
            .add_systems(PostUpdate, animate_characters.before(TransformSystems::Propagate))
            .add_systems(Update, clip_sounds);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AnimKind {
    Idle,
    Walk,
    Run,
    Aim,
    Fire,
    Reload,
    Death,
    /// Animals: grazing / feeding.
    Eat,
    /// Animals: a strike.
    Attack,
    /// First person: idle aiming down the sights.
    IronIdle,
    /// First person: one shot from the hip or down the sights (restarted per shot).
    Shot,
    IronShot,
    /// First person: reloading an empty weapon.
    ReloadEmpty,
    Draw,
    Holster,
    Sprint,
    /// NPCs: crouched behind cover.
    Cover,
    /// NPCs: flinching from a hit.
    Hit,
    /// First person: sliding after a sprint (the clip's last frame holds).
    Slide,
}

impl AnimKind {
    fn looping(self) -> bool {
        !matches!(
            self,
            AnimKind::Death
                | AnimKind::Reload
                | AnimKind::Attack
                | AnimKind::Shot
                | AnimKind::IronShot
                | AnimKind::ReloadEmpty
                | AnimKind::Draw
                | AnimKind::Holster
                | AnimKind::Hit
                | AnimKind::Slide
        )
    }
}

/// Bumped to replay the current clip from the start without a crossfade
/// (a first-person shot per round fired).
#[derive(Component, Default, Clone, Copy)]
pub struct AnimRestart(pub u32);

/// A skeleton and the clips it plays (humans share one; each animal species has its own).
#[derive(Clone)]
pub struct ClipSet {
    pub skeleton: Handle<Fc3Skeleton>,
    pub clips: HashMap<AnimKind, Handle<Fc3Clip>>,
}

impl ClipSet {
    pub fn ready(&self, server: &AssetServer) -> bool {
        let loaded = |id: bevy::asset::UntypedAssetId| crate::cache_assets::usable(&server, id);
        loaded(self.skeleton.id().untyped()) && self.clips.values().all(|h| loaded(h.id().untyped()))
    }
}

pub const CLIP_ROOT: &str = r"graphics\characters\_common\animations\";
const CLIPS: [(AnimKind, &str); 8] = [
    (AnimKind::Cover, r"cover_system\idle\3rdge_fulbcr_coverhilt_idlefrantic_nodir_prak74.mab"),
    (AnimKind::Idle, r"locomotion\stand\idle\alert\3rdge_fulbst_idlealert_noaim_v01_+000fw_ak74.mab"),
    (AnimKind::Walk, r"locomotion\stand\walk\alert\3rdge_fulbst_walkfastalertcycle_noaim_+000fw_prak47.mab"),
    (AnimKind::Run, r"locomotion\stand\run\alert\3rdge_fulbst_runalert_noaim_v00_+000fw_ak74.mab"),
    (AnimKind::Aim, r"locomotion\stand\idle\alert\3rdle_fulbst_idlealert_aimnormal_v00_+000fw_ak74.mab"),
    (AnimKind::Fire, r"locomotion\stand\idle\envfire\3rdge_fulbst_idlealert_aimenvfire_v00_+000fw_ak74.mab"),
    (AnimKind::Reload, r"weapons\npc\lotuseaters\ak47\reloads\3rdge_fulbst_idlealert_normal_reload_v00_+000fw_ak74.mab"),
    (AnimKind::Death, r"injury\death\stand\head\3rdge_fulbst_diehead00_fromfw_+000fw_ak74.mab"),
];
/// Skeleton whose bone indices the human clips use.
const MASTER_SKELETON: &str = r"graphics\actors_\_skeleton_master\hips_ref.skeleton";
macro_rules! gun {
    ($dir:literal, $f:literal) => {
        concat!(r"graphics\__fc3_graphics\_common\weapons\", $dir, r"\", $f, "_tp.xbg")
    };
}

/// How an enemy of a faction and role is dressed (the game's graphic kits,
/// see `cache::GENERIC_KITS`) and armed.
pub struct LookDef {
    pub faction: Faction,
    pub role: Role,
    pub kits: &'static [&'static str],
    pub weapon: &'static str,
}

const fn look(faction: Faction, role: Role, kits: &'static [&'static str], weapon: &'static str) -> LookDef {
    LookDef { faction, role, kits, weapon }
}

pub const LOOKS: &[LookDef] = &[
    look(Faction::Pirate, Role::Assault, &["Pirate_Assault"], gun!("ak74m", "ak74m")),
    look(Faction::Pirate, Role::Shotgunner, &["Pirate_Shotgun", "Pirate_Beheader"], gun!("870shotgun", "870shotgun")),
    look(Faction::Pirate, Role::Sniper, &["Pirate_sniper"], gun!("svd", "svd")),
    look(Faction::Pirate, Role::Rocket, &["Pirate_Rocket"], gun!("rpg7", "rpg7")),
    look(Faction::Pirate, Role::Heavy, &["Pirate_HeavyGunner"], gun!("pkm", "pkm")),
    look(Faction::Pirate, Role::Flamer, &["Pirate_HeavyGunner"], gun!("flamethrower", "flamethrower")),
    look(Faction::Pirate, Role::Molotov, &["Pirate_Molotov"], gun!("6p9", "6p9")),
    look(Faction::Privateer, Role::Assault, &["Privateer_Assault", "Privateer_Assault", "Privateer_Recruit"], gun!("p416", "p416")),
    look(Faction::Privateer, Role::Shotgunner, &["Privateer_Charger"], gun!("spas12", "spas12")),
    look(Faction::Privateer, Role::Sniper, &["Privateer_Sniper"], gun!("700sniperrifle", "700sniperrifle")),
    look(Faction::Privateer, Role::Rocket, &["Privateer_Rocket"], gun!("rpg7", "rpg7")),
    look(Faction::Privateer, Role::Heavy, &["Privateer_HeavyGunner", "Privateer_Defender"], gun!("lmg", "minimi")),
    look(Faction::Privateer, Role::Flamer, &["Privateer_HeavyFlammer"], gun!("flamethrower", "flamethrower")),
    look(Faction::Rebel, Role::Assault, &["Rebel_Assaulter"], gun!("ak74m", "ak74m")),
    look(Faction::Rebel, Role::Sniper, &["Rebel_Sniper"], gun!("svd", "svd")),
];

/// A kit part with its models and swap materials loading.
pub struct KitPart {
    pub model: Handle<Fc3Model>,
    pub weight: f32,
    /// Per swap: the default material, and a handle per choice.
    pub swaps: Vec<(Handle<StandardMaterial>, Vec<Handle<StandardMaterial>>)>,
    pub def: kits::KitPart,
}

/// A loaded graphic kit: one weighted list of parts per slot.
pub struct LoadedKit {
    pub name: String,
    pub slots: Vec<Vec<KitPart>>,
}

/// A loaded [`LookDef`].
pub struct Look {
    pub faction: Faction,
    pub role: Role,
    /// Indices into `CharacterAssets::kits`.
    pub kits: Vec<usize>,
    pub weapon: Handle<Fc3Model>,
}

#[derive(Resource)]
pub struct CharacterAssets {
    pub human: Arc<ClipSet>,
    pub kits: Vec<LoadedKit>,
    pub looks: Vec<Look>,
    /// Villagers: the men's and women's kits (indices into `kits`) and their
    /// unarmed clips.
    pub villager_kits: [Option<usize>; 2],
    pub villager_clips: [Arc<ClipSet>; 2],
}

/// The villagers' unarmed clips (men, women): idle, walk, run, cowering, and
/// a death.
const VILLAGER_CLIPS: [[(AnimKind, &str); 5]; 2] = [
    [
        (AnimKind::Idle, r"locomotion\stand\idle\civilians\male\3rdcivm_fulbst_idlerelax_v00_+000fw_nowep.mab"),
        (AnimKind::Walk, r"locomotion\stand\walk\civilians\male\3rdcivm_fulbst_walkrelax_v00_+000fw_nowep.mab"),
        (AnimKind::Run, r"locomotion\stand\sprint\civilians\male\3rdcivm_fulbst_sprint_+000fw_noweapon.mab"),
        (AnimKind::Cover, r"eventreactions\panic\male\3rdout_fulbst_panic_cower_+000fw_noweap.mab"),
        (AnimKind::Death, r"injury\death\stand\head\3rdge_fulbst_diehead00_fromfw_+000fw_ak74.mab"),
    ],
    [
        (AnimKind::Idle, r"locomotion\stand\idle\civilians\female\3rdcivf_fulbst_idlerelax_v00_+000fw_noweapon.mab"),
        (AnimKind::Walk, r"locomotion\stand\walk\civilians\female\3rdcivf_fulbst_walkrelax_v00_+000fw_noweapon.mab"),
        (AnimKind::Run, r"locomotion\stand\sprint\civilians\female\3rdcivf_fulbst_sprint_+000fw_noweapon.mab"),
        (AnimKind::Cover, r"eventreactions\panic\female\3rdoutf_fulbst_panic_cower_+000fw_noweap.mab"),
        (AnimKind::Death, r"injury\death\stand\head\3rdge_fulbst_diehead00_fromfw_+000fw_ak74.mab"),
    ],
];

impl CharacterAssets {
    pub fn ready(&self, server: &AssetServer) -> bool {
        // Missing pieces are skipped, so settled (loaded or failed) is enough.
        let settled = |id: bevy::asset::UntypedAssetId| crate::cache_assets::settled(server, id);
        self.human.ready(server)
            && self.looks.iter().all(|l| settled(l.weapon.id().untyped()))
            && self
                .kits
                .iter()
                .flat_map(|k| k.slots.iter().flatten())
                .all(|p| settled(p.model.id().untyped()) && p.swaps.iter().flat_map(|(_, c)| c).all(|m| settled(m.id().untyped())))
    }

    /// Indices of the looks for a faction and role (the faction's assault
    /// look if it has none for that role).
    pub fn looks_for(&self, faction: Faction, role: Role) -> Vec<usize> {
        let pick = |r: Role| -> Vec<usize> {
            self.looks.iter().enumerate().filter(|(_, l)| l.faction == faction && l.role == r && !l.kits.is_empty()).map(|(i, _)| i).collect()
        };
        let v = pick(role);
        if v.is_empty() {
            pick(Role::Assault)
        } else {
            v
        }
    }
}

/// Starts loading a kit's part models and swap materials.
pub fn load_kit(server: &AssetServer, k: kits::Kit) -> LoadedKit {
    let skinned = |path: &str| -> Handle<Fc3Model> {
        server.load_builder().with_settings(|s: &mut ModelSettings| s.skinned = true).load(skinned_cache_path(&cache::model_rel(path)))
    };
    let material = |path: &str| -> Handle<StandardMaterial> { server.load(cache_path(&cache::material_rel(path))) };
    LoadedKit {
        name: k.name,
        slots: k
            .slots
            .into_iter()
            .map(|s| {
                s.parts
                    .into_iter()
                    .map(|p| KitPart {
                        model: skinned(&p.model),
                        weight: p.weight,
                        swaps: p.swaps.iter().map(|sw| (material(&sw.default), sw.choices.iter().map(|c| material(&c.material)).collect())).collect(),
                        def: p,
                    })
                    .collect()
            })
            .collect(),
    }
}

impl LoadedKit {
    /// Every part and swap loaded or failed.
    pub fn settled(&self, server: &AssetServer) -> bool {
        let settled = |id: bevy::asset::UntypedAssetId| crate::cache_assets::settled(server, id);
        self.slots.iter().flatten().all(|p| settled(p.model.id().untyped()) && p.swaps.iter().flat_map(|(_, c)| c).all(|m| settled(m.id().untyped())))
    }
}

/// Spawns a character dressed from one kit under `parent` (feet at
/// `offset`), on the human clips; `weapon` goes in its right hand.
pub fn spawn_kit_character(
    commands: &mut Commands,
    parent: Entity,
    assets: &CharacterAssets,
    kit: &LoadedKit,
    models: &Assets<Fc3Model>,
    weapon: Option<&Fc3Model>,
    rng: &mut Rng,
    offset: Vec3,
) -> Option<Entity> {
    let parts = dress(kit, models, rng);
    spawn_dressed_rig(commands, parent, &parts, weapon, assets.human.clone(), offset, None)
}

fn load_character_assets(mut commands: Commands, server: Res<AssetServer>, info: Res<crate::data::CacheInfo>) {
    let text = std::fs::read_to_string(info.root.join(cache::KITS_REL)).unwrap_or_default();
    let kits: Vec<LoadedKit> = kits::from_text(&text).into_iter().map(|k| load_kit(&server, k)).collect();
    if kits.is_empty() {
        warn!("no enemy outfits in the cache ({}); re-run the converter", cache::KITS_REL);
    }
    let looks = LOOKS
        .iter()
        .map(|l| Look {
            faction: l.faction,
            role: l.role,
            kits: l.kits.iter().filter_map(|n| kits.iter().position(|k| k.name.eq_ignore_ascii_case(n))).collect(),
            weapon: server.load(cache_path(&cache::model_rel(l.weapon))),
        })
        .collect();
    let clips = CLIPS.iter().map(|(k, p)| (*k, server.load(cache_path(&cache::anim_rel(&format!("{CLIP_ROOT}{p}")))))).collect();
    let skeleton: Handle<Fc3Skeleton> = server.load(cache_path(&cache::skeleton_rel(MASTER_SKELETON)));
    let villager_clips = VILLAGER_CLIPS.map(|set| {
        let clips = set.iter().map(|(k, p)| (*k, server.load(cache_path(&cache::anim_rel(&format!("{CLIP_ROOT}{p}")))))).collect();
        Arc::new(ClipSet { skeleton: skeleton.clone(), clips })
    });
    let villager_kits = ["Villager_Men", "Local_Women"].map(|n| kits.iter().position(|k| k.name.eq_ignore_ascii_case(n)));
    commands.insert_resource(CharacterAssets { human: Arc::new(ClipSet { skeleton, clips }), kits, looks, villager_kits, villager_clips });
}

/// Weighted pick of an index (uniform when every weight is zero).
fn weighted(weights: impl Iterator<Item = f32> + Clone, rng: &mut Rng) -> Option<usize> {
    let total: f32 = weights.clone().map(|w| w.max(0.0)).sum();
    let n = weights.clone().count();
    if n == 0 {
        return None;
    }
    if total <= 0.0 {
        return Some((rng.f32() * n as f32) as usize % n);
    }
    let mut x = rng.f32() * total;
    for (i, w) in weights.enumerate() {
        if x < w.max(0.0) {
            return Some(i);
        }
        x -= w.max(0.0);
    }
    Some(n - 1)
}

/// Animation playback state for one rig.
#[derive(Component)]
pub struct CharacterRig {
    set: Arc<ClipSet>,
    nodes: Arc<Vec<anim::Node>>,
    joints: Vec<Entity>,
    current: AnimKind,
    time: f32,
    previous: Option<(AnimKind, f32)>,
    blend: f32,
    /// Seconds between pose updates (distant characters update less often).
    accum: f32,
    /// The last `AnimRestart` seen.
    restarts: u32,
}

impl CharacterRig {
    /// A line for logs: node and joint counts, the clip and its time.
    pub fn summary(&self) -> String {
        format!("{} nodes, {} joints, {:?} at {:.2}s", self.nodes.len(), self.joints.len(), self.current, self.time)
    }

    /// The clip playing and the time into it (wrapped as it plays).
    pub fn playing(&self) -> (AnimKind, f32) {
        (self.current, self.time)
    }

    pub fn clips(&self) -> &Arc<ClipSet> {
        &self.set
    }

    /// Jumps the current clip to a time (tests).
    pub fn seek(&mut self, t: f32) {
        self.time = t;
        self.previous = None;
    }

    /// Plays from another set of clips on the same skeleton (first person:
    /// a different weapon).
    pub fn set_clips(&mut self, set: Arc<ClipSet>) {
        self.set = set;
        self.previous = None;
        self.time = 0.0;
    }

    /// A joint by bone name hash, with its index.
    pub fn joint(&self, name_hash: u32) -> Option<(usize, Entity)> {
        let i = self.nodes.iter().position(|n| n.name_hash == name_hash)?;
        Some((i, *self.joints.get(i)?))
    }

    /// Parent index of a joint (-1 at the root).
    pub fn parent(&self, i: usize) -> i32 {
        self.nodes.get(i).map(|n| n.parent).unwrap_or(-1)
    }

    pub fn joint_entity(&self, i: usize) -> Option<Entity> {
        self.joints.get(i).copied()
    }
}

/// Time into a clip as played (looping clips wrap, others hold the end).
pub fn clip_time(kind: AnimKind, t: f32, duration: f32) -> f32 {
    wrap(kind, t, duration)
}

/// Whether the first-person arms' reload clip brings its own sounds (so the
/// generic reload sound stays quiet).
#[derive(Resource, Default)]
pub struct ArmsFoley {
    pub reload: bool,
}

/// Characters farther than this (m) don't sound their clips.
const CLIP_SOUND_RANGE: f32 = 25.0;

/// Plays the sounds clips carry as each rig's playhead passes them: the
/// first-person arms' reloads, draws and takedowns close up, others where
/// they stand.
fn clip_sounds(
    rigs: Query<(Entity, &CharacterRig, &GlobalTransform)>,
    clips: Res<Assets<Fc3Clip>>,
    fp: Option<Res<crate::fp::FpState>>,
    listener: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    mut last: Local<HashMap<Entity, (AssetId<Fc3Clip>, f32)>>,
    mut foley: ResMut<ArmsFoley>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let ear = listener.single().map(|l| l.translation()).unwrap_or_default();
    let arms = fp.and_then(|f| f.rig());
    last.retain(|e, _| rigs.contains(*e));
    for (e, rig, gt) in &rigs {
        let first_person = arms == Some(e);
        if first_person {
            foley.reload = rig.clips().clips.get(&AnimKind::Reload).and_then(|h| clips.get(h)).is_some_and(|c| !c.0.sounds.is_empty());
        }
        let (kind, time) = rig.playing();
        let Some(handle) = rig.clips().clips.get(&kind) else { continue };
        let Some(clip) = clips.get(handle) else { continue };
        let t = clip_time(kind, time, clip.0.duration);
        let prev = last.insert(e, (handle.id(), t));
        if std::env::var("FC3_CLIP_SOUND_DEBUG").is_ok() && prev.is_none_or(|p| p.0 != handle.id()) {
            info!("clip-sound: rig {e:?} first_person {first_person} {kind:?} {} sounds", clip.0.sounds.len());
        }
        if clip.0.sounds.is_empty() || (!first_person && gt.translation().distance(ear) > CLIP_SOUND_RANGE) {
            continue;
        }
        // From where it was last frame; a new or restarted clip from its start
        // (a rig just come into earshot from where it is).
        let from = match prev {
            Some((id, p)) if id == handle.id() && p <= t => p,
            Some(_) => -1.0,
            None => t,
        };
        for &(at, event) in &clip.0.sounds {
            if at > from && at <= t {
                sfx.write(crate::sound::PlaySfx { event, at: (!first_person).then(|| gt.translation()), volume: if first_person { 0.8 } else { 0.9 } });
            }
        }
    }
}

/// What gameplay wants the character to play; read every frame.
#[derive(Component, Clone, Copy)]
pub struct AnimRequest {
    pub kind: AnimKind,
    /// Playback rate multiplier (locomotion matches movement speed).
    pub rate: f32,
}

/// Spawns a skinned character under `parent`, with its feet at `offset`
/// (parent-local), dressed from one of the look's kits: a weighted part per
/// slot, and weighted material swaps whose tags keep skin tones consistent.
/// Returns the rig entity, or `None` if assets aren't loaded.
/// Spawns a villager (`female` picks the women's kit) under `parent`,
/// unarmed, on the villagers' clips.
pub fn spawn_villager(
    commands: &mut Commands,
    parent: Entity,
    assets: &CharacterAssets,
    models: &Assets<Fc3Model>,
    female: bool,
    rng: &mut Rng,
    offset: Vec3,
) -> Option<Entity> {
    let i = usize::from(female);
    let kit = &assets.kits[assets.villager_kits[i]?];
    let parts = dress(kit, models, rng);
    spawn_dressed_rig(commands, parent, &parts, None, assets.villager_clips[i].clone(), offset, None)
}

/// Dresses a kit: a weighted part per slot, with matching material swaps,
/// sorted so the part with the fullest bone list comes first.
fn dress<'a>(kit: &LoadedKit, models: &'a Assets<Fc3Model>, rng: &mut Rng) -> Vec<Dressed<'a>> {
    let mut tags = kits::Tags::default();
    let mut parts: Vec<Dressed> = Vec::new();
    for slot in &kit.slots {
        let Some(i) = weighted(slot.iter().map(|p| p.weight), rng) else { continue };
        let part = &slot[i];
        let Some(model) = models.get(&part.model) else { continue };
        let mut swaps = Vec::new();
        for ((default, choices), def) in part.swaps.iter().zip(&part.def.swaps) {
            let allowed = tags.allowed(def);
            let Some(j) = weighted(allowed.iter().map(|&j| def.choices[j].weight), rng).map(|k| allowed[k]) else { continue };
            tags.take(&def.choices[j]);
            swaps.push((default.id(), choices[j].clone()));
        }
        parts.push(Dressed { model, swaps });
    }
    parts.sort_by_key(|p| std::cmp::Reverse(p.model.bones.len()));
    parts
}

pub fn spawn_character(
    commands: &mut Commands,
    parent: Entity,
    assets: &CharacterAssets,
    models: &Assets<Fc3Model>,
    look: usize,
    rng: &mut Rng,
    offset: Vec3,
) -> Option<Entity> {
    let look = assets.looks.get(look)?;
    let kit = &assets.kits[*look.kits.get((rng.f32() * look.kits.len() as f32) as usize % look.kits.len().max(1))?];
    let mut tags = kits::Tags::default();
    let mut parts: Vec<Dressed> = Vec::new();
    for slot in &kit.slots {
        let Some(i) = weighted(slot.iter().map(|p| p.weight), rng) else { continue };
        let part = &slot[i];
        // Parts that failed to load are left off.
        let Some(model) = models.get(&part.model) else { continue };
        let mut swaps = Vec::new();
        for ((default, choices), def) in part.swaps.iter().zip(&part.def.swaps) {
            let allowed = tags.allowed(def);
            let Some(j) = weighted(allowed.iter().map(|&j| def.choices[j].weight), rng).map(|k| allowed[k]) else { continue };
            tags.take(&def.choices[j]);
            swaps.push((default.id(), choices[j].clone()));
        }
        parts.push(Dressed { model, swaps });
    }
    if std::env::var("FC3_KIT_DEBUG").is_ok() {
        for p in &parts {
            let used: Vec<AssetId<StandardMaterial>> = p.model.lods.first().map(|l| l.parts.iter().map(|q| q.material.id()).collect()).unwrap_or_default();
            let hits = p.swaps.iter().filter(|(from, _)| used.contains(from)).count();
            let to: Vec<String> = p.swaps.iter().map(|(_, h)| h.path().map(|p| p.to_string()).unwrap_or_default()).collect();
            info!("kit {}: part with {} bones, {} swaps, {hits} match its materials: {to:?}", kit.name, p.model.bones.len(), p.swaps.len());
        }
    }
    // The part with the fullest bone list builds the rig.
    parts.sort_by_key(|p| std::cmp::Reverse(p.model.bones.len()));
    let weapon = models.get(&look.weapon);
    spawn_dressed_rig(commands, parent, &parts, weapon, assets.human.clone(), offset, None)
}

/// A part of the weapon a character holds (on its `RightHand` joint).
#[derive(Component)]
pub struct HeldWeapon;

/// Spawns a skinned rig from body parts sharing one skeleton, playing clips
/// from `set`; `weapon` (if any) goes in the right hand, and `skin` (if any)
/// dresses every part (animals whose models carry placeholder materials).
pub fn spawn_rig_with(
    commands: &mut Commands,
    parent: Entity,
    parts: &[&Fc3Model],
    weapon: Option<&Fc3Model>,
    set: Arc<ClipSet>,
    offset: Vec3,
    skin: Option<Handle<StandardMaterial>>,
) -> Option<Entity> {
    let dressed: Vec<Dressed> = parts.iter().map(|m| Dressed { model: m, swaps: Vec::new() }).collect();
    spawn_dressed_rig(commands, parent, &dressed, weapon, set, offset, skin)
}

/// A body part and the materials swapped in for this character.
pub struct Dressed<'a> {
    pub model: &'a Fc3Model,
    /// (default material, replacement)
    pub swaps: Vec<(AssetId<StandardMaterial>, Handle<StandardMaterial>)>,
}

fn spawn_dressed_rig(
    commands: &mut Commands,
    parent: Entity,
    parts: &[Dressed],
    weapon: Option<&Fc3Model>,
    set: Arc<ClipSet>,
    offset: Vec3,
    skin: Option<Handle<StandardMaterial>>,
) -> Option<Entity> {
    let base = &parts.first()?.model.bones;
    if base.is_empty() {
        return None;
    }
    // Converted character models already face -Z, Bevy's forward.
    let rig_root = commands.spawn((Transform::from_translation(offset), Visibility::Inherited, ChildOf(parent))).id();
    // One joint entity per bone, bind-local transforms, parented like the skeleton.
    let joints: Vec<Entity> = base
        .iter()
        .map(|b| {
            commands
                .spawn((
                    Transform { translation: Vec3::from_array(b.translation), rotation: Quat::from_array(b.rotation), scale: Vec3::from_array(b.scale) },
                    Visibility::Inherited,
                    ChildOf(rig_root),
                ))
                .id()
        })
        .collect();
    for (i, b) in base.iter().enumerate() {
        if b.parent >= 0 && (b.parent as usize) < joints.len() && b.parent as usize != i {
            commands.entity(joints[i]).insert(ChildOf(joints[b.parent as usize]));
        }
    }
    let by_hash: HashMap<u32, usize> = base.iter().enumerate().map(|(i, b)| (b.name_hash, i)).collect();
    for (pi, dressed) in parts.iter().enumerate() {
        let part = dressed.model;
        let Some(ibp) = part.inverse_bindposes.clone() else { continue };
        // Map this part's bones into the shared rig; its own root node maps to the rig's root bone.
        let part_joints: Vec<Entity> = part.bones.iter().map(|b| by_hash.get(&b.name_hash).map(|&i| joints[i]).unwrap_or(joints[0])).collect();
        let Some(lod) = part.lods.first() else { continue };
        for p in &lod.parts {
            commands.spawn((
                Mesh3d(p.mesh.clone()),
                MeshMaterial3d(skin.clone().unwrap_or_else(|| {
                    dressed.swaps.iter().find(|(from, _)| *from == p.material.id()).map(|(_, to)| to.clone()).unwrap_or_else(|| p.material.clone())
                })),
                SkinnedMesh { inverse_bindposes: ibp.clone(), joints: part_joints.clone() },
                crate::original_shaders::SkinnedPart,
                // The bind-pose bounds don't follow animation; characters are few, so skip culling.
                NoFrustumCulling,
                Transform::IDENTITY,
                ChildOf(rig_root),
            ));
        }
        let _ = pi;
    }
    // Third-person weapon in the right hand.
    if let (Some(&hand), Some(w)) = (by_hash.get(&name_hash("RightHand")), weapon) {
        if let Some(lod) = w.lods.first() {
            for p in lod.parts.iter().filter(|p| p.shown(false)) {
                commands.spawn((
                    Mesh3d(p.mesh.clone()),
                    MeshMaterial3d(p.material.clone()),
                    NotShadowCaster,
                    crate::original_shaders::PropPart,
                    HeldWeapon,
                    ChildOf(joints[hand]),
                ));
            }
        }
    }
    let nodes: Vec<anim::Node> = base
        .iter()
        .enumerate()
        .map(|(i, b)| anim::Node {
            name: b.name.clone(),
            name_hash: b.name_hash,
            parent: b.parent,
            first_child: -1,
            next_sibling: -1,
            translation: b.translation,
            rotation: b.rotation,
            scale: b.scale,
            bind_index: i as i32,
        })
        .collect();
    commands.entity(rig_root).insert((
        CharacterRig { set, nodes: Arc::new(nodes), joints, current: AnimKind::Idle, time: 0.0, previous: None, blend: 1.0, accum: 0.0, restarts: 0 },
        AnimRequest { kind: AnimKind::Idle, rate: 1.0 },
    ));
    Some(rig_root)
}

const BLEND_TIME: f32 = 0.25;

fn sample(rig: &CharacterRig, skel: &anim::Skeleton, clip: &anim::Clip, t: f32) -> Vec<anim::Mat4> {
    anim::pose_locals(&rig.nodes, skel, clip, t)
}

fn wrap(kind: AnimKind, t: f32, duration: f32) -> f32 {
    if duration <= 0.0 {
        0.0
    } else if kind.looping() {
        t.rem_euclid(duration)
    } else {
        t.min(duration - 1e-3)
    }
}

/// A rig played by a scene (`cinema`): this clip at this time, in place of
/// what its `AnimRequest` asks.
#[derive(Component, Clone)]
pub struct ClipOverride {
    pub clip: Handle<Fc3Clip>,
    /// The skeleton the clip targets.
    pub skeleton: Handle<Fc3Skeleton>,
    pub time: f32,
}

#[allow(clippy::type_complexity)]
pub fn animate_characters(
    time: Res<Time>,
    skeletons: Res<Assets<Fc3Skeleton>>,
    clips: Res<Assets<Fc3Clip>>,
    cameras: Query<&GlobalTransform, With<Camera3d>>,
    mut rigs: Query<(&mut CharacterRig, &AnimRequest, &GlobalTransform, Option<&AnimRestart>, Option<&ClipOverride>)>,
    mut joints: Query<&mut Transform>,
) {
    let cam = cameras.iter().next().map(|c| c.translation()).unwrap_or(Vec3::ZERO);
    let dt = time.delta_secs();
    for (mut rig, req, gt, restart, scene) in &mut rigs {
        // A scene's clip, exactly where the scene is.
        if let Some(o) = scene {
            let (Some(skel), Some(clip)) = (skeletons.get(&o.skeleton), clips.get(&o.clip)) else { continue };
            let pose = sample(&rig, &skel.0, &clip.0, o.time.clamp(0.0, clip.0.duration));
            for (i, m) in pose.iter().enumerate() {
                let Some(&j) = rig.joints.get(i) else { break };
                if let Ok(mut t) = joints.get_mut(j) {
                    let (s, r, tr) = Mat4::from_cols_array(m).to_scale_rotation_translation();
                    *t = Transform { translation: tr, rotation: r, scale: s };
                }
            }
            continue;
        }
        // Replays: back to the start at once (each first-person shot).
        if let Some(r) = restart.filter(|r| r.0 != rig.restarts) {
            rig.restarts = r.0;
            if req.kind == rig.current {
                rig.time = 0.0;
            }
        }
        // State changes start a crossfade from the current pose.
        if req.kind != rig.current {
            let prev = (rig.current, rig.time);
            rig.previous = Some(prev);
            rig.current = req.kind;
            rig.time = 0.0;
            rig.blend = 0.0;
        }
        rig.time += dt * req.rate;
        if let Some((_, pt)) = &mut rig.previous {
            *pt += dt;
        }
        rig.blend = (rig.blend + dt / BLEND_TIME).min(1.0);
        if rig.blend >= 1.0 {
            rig.previous = None;
        }
        // Distant characters pose at a lower rate.
        let dist = gt.translation().distance(cam);
        let interval = if dist < 40.0 {
            0.0
        } else if dist < 100.0 {
            1.0 / 20.0
        } else {
            1.0 / 8.0
        };
        rig.accum += dt;
        if rig.accum < interval {
            continue;
        }
        rig.accum = 0.0;
        let set = rig.set.clone();
        let Some(skel) = skeletons.get(&set.skeleton) else { continue };
        // Missing clips (e.g. an animal without an attack) fall back to idle.
        let Some(clip) = set.clips.get(&rig.current).or_else(|| set.clips.get(&AnimKind::Idle)).and_then(|h| clips.get(h)) else { continue };
        let t = wrap(rig.current, rig.time, clip.0.duration);
        let mut pose = sample(&rig, &skel.0, &clip.0, t);
        if let Some((pk, pt)) = rig.previous {
            if let Some(pclip) = set.clips.get(&pk).and_then(|h| clips.get(h)) {
                let prev = sample(&rig, &skel.0, &pclip.0, wrap(pk, pt, pclip.0.duration));
                let w = rig.blend;
                for (cur, old) in pose.iter_mut().zip(prev.iter()) {
                    let (s1, r1, t1) = Mat4::from_cols_array(old).to_scale_rotation_translation();
                    let (s2, r2, t2) = Mat4::from_cols_array(cur).to_scale_rotation_translation();
                    *cur = Mat4::from_scale_rotation_translation(s1.lerp(s2, w), r1.slerp(r2, w), t1.lerp(t2, w)).to_cols_array();
                }
            }
        }
        for (i, m) in pose.iter().enumerate() {
            let Some(&j) = rig.joints.get(i) else { break };
            if let Ok(mut tf) = joints.get_mut(j) {
                let (s, r, t) = Mat4::from_cols_array(m).to_scale_rotation_translation();
                tf.translation = t;
                tf.rotation = r;
                tf.scale = s;
            }
        }
    }
}
