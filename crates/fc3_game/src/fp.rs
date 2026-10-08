//! First-person arms: a generic pair of arms (the game's multiplayer rebel
//! arms, not the story's hero) posed by each weapon's own first-person clips:
//! breathing idle, aiming down the sights, a shot per round, reloads, draw,
//! walk and sprint. The rig hangs under the view model so the camera sits on
//! its `Camera` bone; the weapon rides the `RightHand` bone at the offset the
//! clip's weapon track gives. Weapons without clips keep the plain view model.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use fc3_formats::cache;
use fc3_formats::hash::name_hash;

use crate::cache_assets::{cache_path, skinned_cache_path, Fc3Clip, Fc3Model, ModelSettings};
use crate::characters::{spawn_rig_with, AnimKind, AnimRequest, AnimRestart, CharacterAssets, CharacterRig, ClipSet};
use crate::player::Player;
use crate::weapons::{Inventory, ViewModel};

pub struct FpPlugin;

impl Plugin for FpPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FpState>()
            .init_resource::<Cinematic>()
            .init_resource::<HandsBusy>()
            .add_message::<TakedownAnim>()
            .add_message::<FpAction>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_fp)
            .add_systems(
                Update,
                (spawn_arms, select_weapon, action_test, start_takedowns, start_actions, drive_arms, action_props, gesture_props).chain().before(crate::weapons::ViewModelSystems),
            )
            .add_systems(PostUpdate, (drive_takedown, wingsuit_membrane, align_arms).chain().after(crate::characters::animate_characters).before(TransformSystems::Propagate))
            .add_systems(Update, live_grenades);
    }
}

/// The generic first-person arms.
const ARMS: &str = r"graphics\__fc3_graphics\_common\characters\multiplayer\pvp\rebels\rebel_assault_fpsarms.xbg";

/// Clip folders (under `cache::FP_CLIP_ROOT`) by weapon model folder.
const FOLDERS: &[(&str, &str)] = &[
    ("mk23", "secondary\\kimber_1911"),
    ("mp5", "primary\\mp5"),
    ("ak74m", "primary\\ak74m"),
    ("870shotgun", "primary\\remington870"),
    ("700sniperrifle", "primary\\700sniperrifle"),
    ("pkm", "primary\\pkm"),
    ("rpg7", "special\\rpg-7"),
    ("kimber", "secondary\\kimber_1911"),
    ("6p9", "secondary\\makarov"),
    ("deserteagle", "secondary\\desert_eagle"),
    ("agram2000", "secondary\\agram"),
    ("kriss", "primary\\kriss"),
    ("vz61", "secondary\\vz61"),
    ("bison", "secondary\\pp19"),
    ("famas", "primary\\famas"),
    ("p416", "primary\\p416"),
    ("mk16", "primary\\mk16"),
    ("galilace52", "primary\\galil_ace"),
    ("sg551", "primary\\sg551"),
    ("1887", "primary\\model_1887"),
    ("spas12", "primary\\spas12"),
    ("svd", "primary\\draganov_svd"),
    ("m93", "primary\\m93"),
    ("u100", "primary\\u100"),
    ("lmg", "primary\\mkg"),
    ("bow", "special\\bow"),
    ("machete", "handtohand\\machete"),
    ("flamethrower", "special\\flamethrower"),
    ("gm94grenadelauncher", "special\\gm94"),
];

const ANIM_ROOT: &str = r"graphics\characters\_common\animations\";

/// What the arms do when they aren't holding a weapon up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Context {
    Swim,
    SwimMove,
    Ladder,
    Zip,
    /// At the wheel (first-person seat), by kind of vehicle.
    Drive,
    Boat,
    Glide,
    /// Flying the wingsuit, or under the parachute.
    Wingsuit,
    Chute,
    /// Falling a long way with nothing open: arms out.
    Skydive,
    /// Working on a damaged vehicle with the welding torch.
    Repair,
    /// Raising the camera to tag (then looking through it).
    Camera,
}

/// Falling faster than this (m/s) with nothing open puts the arms out.
const SKYDIVE_SPEED: f32 = 14.0;

impl Context {
    /// Its clip, under `ANIM_ROOT`.
    fn clip(self) -> &'static str {
        match self {
            Context::Swim => r"locomotion\stand\swim\1stge_uppb_swim_idle_nowep.mab",
            Context::SwimMove => r"locomotion\stand\swim\1stge_uppb_swim_+000fw_nowep.mab",
            Context::Ladder => r"locomotion\ladder\1stge_fulb_laddercycle_a_nowep.mab",
            Context::Zip => r"zipline\1stge_uppb_aimingcenter_+000fw_zip.mab",
            Context::Drive => r"vehicles\4x4\1stge_fulb_driving_nodir_nowep_4x4.mab",
            Context::Boat => r"vehicles\boat\zodiac\1stge_fulb_driving_zodiac.mab",
            Context::Glide => r"vehicles\other\glider\1stge_fulb_idle_+000fw_nowep_vo_glider.mab",
            Context::Wingsuit => r"vehicles\other\wingsuit\1stge_fulb_gliding_nodir_nowep_wingsuit.mab",
            Context::Chute => r"vehicles\other\wingsuit\1stge_fulb_chute_driving_nodir_nowep_wingsuit.mab",
            Context::Skydive => r"vehicles\other\wingsuit\1stge_fulb_skydivefalling_nodir_nowep_wingsuit.mab",
            Context::Repair => r"weapons\special\repairtool\1stge_uppb_canrepairshootcycle_+000fw_sprt.mab",
            Context::Camera => r"weapons\camera\1stge_uppb_iron_+000fw_eqcam.mab",
        }
    }

    /// A clip played once going into it (and its length) before the loop:
    /// the wingsuit or canopy coming out, the arms going out into a dive.
    fn intro(self) -> Option<(&'static str, f32)> {
        match self {
            Context::Wingsuit => Some((r"vehicles\other\wingsuit\1stge_fulb_deploywingsuit_nodir_nowep_wingsuit.mab", 1.167)),
            Context::Chute => Some((r"vehicles\other\wingsuit\1stge_fulb_deploychute_nodir_nowep_wingsuit.mab", 1.667)),
            Context::Skydive => Some((r"vehicles\other\wingsuit\1stge_fulb_jump2skydive_nodir_nowep_wingsuit.mab", 1.0)),
            Context::Repair => Some((r"weapons\special\repairtool\1stge_uppb_transitionaim2canrepair_+000fw_sprt.mab", 0.333)),
            Context::Camera => Some((r"weapons\camera\1stge_uppb_draw_+000fw_eqcam.mab", 0.567)),
            _ => None,
        }
    }
}

/// A takedown played out: the player's first-person half and the victim's half
/// of one of the game's paired clips.
#[derive(Message, Clone, Copy)]
pub struct TakedownAnim {
    pub victim: Entity,
    pub style: TakedownStyle,
    /// A second enemy to throw the victim's knife at (the chain takedown).
    pub chain: Option<Entity>,
    /// Grenade takedown: the group to kick him into, his grenade's pin pulled.
    pub grenade: Option<Vec3>,
    /// Drag the body away after the kill (the takedown drag).
    pub drag: bool,
    /// A second victim for a double takedown from above, and whether he
    /// stands to the player's left.
    pub second: Option<(Entity, bool)>,
    /// The pistol takedown: the enemies to shoot with the victim's pistol.
    pub pistol: [Option<Entity>; 3],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TakedownStyle {
    /// From behind (or the side).
    Back,
    /// Face to face (the frontal takedown skill).
    Front,
    /// Dropping onto him from above, from that side of him.
    Above { from: Quarter },
    /// Pulling him down off a ledge (or into the water) from below; `front`
    /// when he faces the player.
    Below { front: bool, water: bool },
}

/// Which side of an enemy the player comes at him from, by his facing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quarter {
    Front,
    Right,
    Back,
    Left,
}

impl Quarter {
    /// The side of an enemy facing `yaw` that `to_player` (horizontal) is on.
    pub fn of(yaw: f32, to_player: Vec3) -> Quarter {
        let facing = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
        let right = facing.cross(Vec3::Y);
        let (ahead, side) = (facing.dot(to_player), right.dot(to_player));
        if ahead.abs() >= side.abs() {
            if ahead >= 0.0 { Quarter::Front } else { Quarter::Back }
        } else if side >= 0.0 {
            Quarter::Right
        } else {
            Quarter::Left
        }
    }
}

/// Seconds left of a takedown: the player can't move or look around.
#[derive(Resource, Default)]
pub struct Cinematic(pub f32);

/// Seconds the hands are busy with a takedown, a heal or looting: no firing,
/// reloading, throwing or starting another action.
#[derive(Resource, Default)]
pub struct HandsBusy(pub f32);

impl HandsBusy {
    pub fn busy(&self) -> bool {
        self.0 > 0.0
    }
}

/// Something the arms do with their own clip and props. Heals bring health up
/// to `to` partway through.
#[derive(Message, Clone, Copy, Debug)]
pub enum FpAction {
    /// A medkit: the syringe.
    Syringe { to: f32 },
    /// Field surgery when out of medkits, picked by what last hurt.
    ManualHeal { kind: crate::combat::DamageKind, to: f32 },
    /// Searching a body, picking a plant, skinning an animal (full body: the
    /// view follows the clip and the player stays put).
    LootBody { crouch: bool },
    LootPlant { crouch: bool },
    Skin { large: bool },
    /// Pulling up onto a ledge (`high` above about 1.3 m) or vaulting over,
    /// fitted to `secs`.
    Climb { high: bool, secs: f32 },
    Vault { secs: f32 },
}

impl FpAction {
    /// The clips that can play it (under `ANIM_ROOT`, with their lengths) and
    /// whether they are full-body.
    fn clips(self) -> (&'static [(&'static str, f32)], bool) {
        use crate::combat::DamageKind as D;
        match self {
            FpAction::Syringe { .. } => (
                &[(r"healing\syringe_heals\1stge_uppb_syringe_heal_+000fw_nowep.mab", 2.0), (r"healing\syringe_heals\1stge_uppb_syringe_heal_2_+000fw_nowep.mab", 2.0)],
                false,
            ),
            FpAction::ManualHeal { kind, .. } => (
                match kind {
                    D::Bullet => &[
                        (r"healing\manual\pierce_heal\1stge_uppb_bullet_spit_+000fw_nowep.mab", 4.0),
                        (r"healing\manual\pierce_heal\1stge_uppb_stick_bullet_+000fw_nowep.mab", 4.33),
                        (r"healing\manual\pierce_heal\1stge_uppb_medpack_scalpel_+000fw_nowep.mab", 4.33),
                    ],
                    D::Explosion => &[
                        (r"healing\manual\explosion_heal\1stge_uppb_propshealing_glass_+000fw_nowep.mab", 4.17),
                        (r"healing\manual\explosion_heal\1stge_uppb_propshealing_nail_+000fw_nowep.mab", 4.23),
                    ],
                    D::Melee | D::Fall => &[
                        (r"healing\manual\crush_heal\1stge_uppb_crush_heal_thumb_+000fw_nowep.mab", 4.0),
                        (r"healing\manual\crush_heal\1stge_uppb_crush_heal_wrist_+000fw_nowep.mab", 4.0),
                        (r"healing\manual\crush_heal\1stge_uppb_crush_heal_elbow_+000fw_nowep.mab", 4.0),
                    ],
                    D::Fire => &[(r"healing\manual\burn_heal\1stge_uppb_burnshealinga_+000fw_nowep.mab", 1.67)],
                    D::Drown => &[(r"healing\manual\generic_heal\1stge_uppb_propshealing_bandage_+000fw_nowep.mab", 4.0)],
                },
                false,
            ),
            FpAction::LootBody { crouch } => {
                (if crouch { &[(r"loot\1stge_fulb_loot_human_crouch_+000fw.mab", 2.0)] } else { &[(r"loot\1stge_fulb_loot_human_stand_+000fw.mab", 2.0)] }, true)
            }
            FpAction::LootPlant { crouch } => {
                (if crouch { &[(r"loot\1stge_fulb_loot_bush_crouch_+000fw.mab", 1.33)] } else { &[(r"loot\1stge_fulb_loot_bush_stand_+000fw.mab", 1.33)] }, true)
            }
            FpAction::Skin { large } => {
                (if large { &[(r"loot\1stge_fulb_loot_animal_large+000fw.mab", 6.0)] } else { &[(r"loot\1stge_fulb_loot_animal_medium+000fw.mab", 4.0)] }, true)
            }
            FpAction::Climb { high: false, .. } => (&[(r"locomotion\climbing\1stge_uppb_climbingmed_+000fw_nowep.mab", 2.0)], false),
            FpAction::Climb { high: true, .. } => (&[(r"locomotion\climbing\1stge_uppb_climbinghigh_+000fw_nowep.mab", 4.33)], false),
            FpAction::Vault { .. } => (&[(r"locomotion\vaulting\1stge_uppb_vault_stand_+000fw_nowep.mab", 1.27)], false),
        }
    }

    /// How long it takes, if not the clip's own length.
    fn fitted(self) -> Option<f32> {
        match self {
            FpAction::Climb { secs, .. } | FpAction::Vault { secs } => Some(secs),
            _ => None,
        }
    }

    /// Health it restores, if a heal.
    fn heal_to(self) -> Option<f32> {
        match self {
            FpAction::Syringe { to } | FpAction::ManualHeal { to, .. } => Some(to),
            _ => None,
        }
    }
}

/// Props the action clips attach, by the clip's object name.
const PROPS: &[(&str, &str)] = &[
    ("syringe_01", r"graphics\__fc3_graphics\_common\props\equipment\syringe_01.xbg"),
    ("scalpel", r"graphics\__fc3_graphics\_common\props\equipment\scalpel.xbg"),
    ("heal_lonebullet", r"graphics\__fc3_graphics\singleplayer\props\gameplay\heal_lonebullet.xbg"),
    ("shrapnel_stickscalpel_01", r"graphics\__fc3_graphics\singleplayer\props\equipment\shrapnel_stickscalpel_01.xbg"),
    ("shrapnel_nail_01", r"graphics\__fc3_graphics\singleplayer\props\equipment\shrapnel_nail_01.xbg"),
    ("shrapnel_med_01", r"graphics\__fc3_graphics\singleplayer\props\equipment\shrapnel_med_01.xbg"),
    ("shrapnel_large_01", r"graphics\__fc3_graphics\singleplayer\props\equipment\shrapnel_large_01.xbg"),
    ("bandageroll_01", r"graphics\__fc3_graphics\singleplayer\props\equipment\bandageroll_01.xbg"),
    ("armcloth_01", r"graphics\__fc3_graphics\singleplayer\props\equipment\armcloth_01.xbg"),
    ("armcloth_02", r"graphics\__fc3_graphics\singleplayer\props\equipment\armcloth_02.xbg"),
    ("armcloth_03", r"graphics\__fc3_graphics\singleplayer\props\equipment\armcloth_03.xbg"),
    ("meatbag", r"graphics\__fc3_graphics\singleplayer\props\misc\meatbag.xbg"),
    ("redmeat_collectible_01", r"graphics\__fc3_graphics\singleplayer\props\misc\redmeat_collectible_01.xbg"),
    ("machete", r"graphics\__fc3_graphics\_common\weapons\machete\machete.xbg"),
    ("frag_grenade", r"graphics\__fc3_graphics\_common\weapons\grenade\frag_grenade.xbg"),
    ("molotov", r"graphics\__fc3_graphics\_common\weapons\molotov\molotov.xbg"),
    ("rock", r"graphics\__fc3_graphics\_common\weapons\rock\rock.xbg"),
    ("ied_c4_bomb", r"graphics\__fc3_graphics\_common\weapons\ied\ied_c4_bomb.xbg"),
    ("ied_c4_remote", r"graphics\__fc3_graphics\_common\weapons\ied\ied_c4_remote.xbg"),
    ("proxmine", r"graphics\__fc3_graphics\_common\weapons\proximitymine\proxmine.xbg"),
    ("desert_eagle", r"graphics\__fc3_graphics\_common\weapons\deserteagle\desert_eagle.xbg"),
    ("welding_torch", r"graphics\__fc3_graphics\_common\weapons\weldingtorch\welding_torch.xbg"),
    // The tagging camera (its clips call it the monocular).
    ("monocular", r"graphics\__fc3_graphics\_common\props\equipment\camera.xbg"),
];

/// The tagging camera's prop skeleton in its clips (a single bone).
const CAMERA_PROP: u32 = 0xA4E2_1CF2;

/// The welding torch's prop skeleton in the repair tool's clips.
const TORCH_PROP: u32 = 0x7DEA_9E33;

/// The wingsuit's membrane: its model and skeleton, and its prop skeleton in
/// the wingsuit clips (attached to `Spine2`).
const WINGSUIT_MODEL: &str = r"graphics\__fc3_graphics\_common\props\misc\wing_suit.xbg";
const WINGSUIT_SKELETON: &str = r"graphics\__fc3_graphics\_common\props\misc\wing_suit_ref.skeleton";
const WINGSUIT_PROP: u32 = 0x62B8_1C19;

/// When a thrown prop leaves the hand (gesture clip, prop, seconds into the
/// clip): the hand's furthest reach.
const RELEASES: &[(&str, &str, f32)] = &[
    ("throw", "frag_grenade", 0.12),
    ("throw_molotov", "molotov", 0.12),
    ("throw", "rock", 0.12),
    ("throw", "ied_c4_bomb", 0.28),
    ("place", "proxmine", 1.65),
];

/// Gesture clips play this much faster than authored.
pub const GESTURE_RATE: f32 = 1.3;

/// Seconds after a gesture starts that `prop` leaves the hand in it.
pub fn release_time(action: &str, prop: &str) -> Option<f32> {
    RELEASES.iter().find(|(a, p, _)| *a == action && *p == prop).map(|(_, _, t)| t / GESTURE_RATE)
}

/// Places the props a clip carries (by its attachments' object names) on the
/// rig's joints, each driven by the sub-clip with its bone count: (prop root,
/// sub-clip, object name).
fn place_props(clip: &fc3_formats::anim::Clip, rig: &CharacterRig, props: &HashMap<&'static str, Handle<Fc3Model>>, models: &Assets<Fc3Model>, commands: &mut Commands) -> Vec<(Entity, usize, String)> {
    let mut used = vec![false; clip.sub_clips.len()];
    let mut spawned = Vec::new();
    for at in &clip.attachments {
        let Some(model) = props.get(at.object.as_str()).and_then(|h| models.get(h)) else { continue };
        let bones = model.bones.len().max(1);
        let Some(sub) = (0..used.len()).find(|&i| !used[i] && clip.sub_clips[i].tracks.len() == bones) else { continue };
        used[sub] = true;
        let Some((_, joint)) = rig.joint(name_hash(&at.bone)) else { continue };
        spawned.push((spawn_prop(model, joint, commands), sub, at.object.clone()));
    }
    spawned
}

/// A prop model's meshes under a new root on `joint`.
fn spawn_prop(model: &Fc3Model, joint: Entity, commands: &mut Commands) -> Entity {
    let root = commands.spawn((Transform::default(), Visibility::Inherited, ChildOf(joint))).id();
    if let Some(lod) = model.lods.first() {
        for part in &lod.parts {
            commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), NotShadowCaster, crate::original_shaders::PropPart, ChildOf(root)));
        }
    }
    root
}

/// Starts an action on the arms: its clip, the weapon put away, its props
/// placed once the clip loads. In a swim, climb or vehicle (or without arms) a
/// heal just happens.
#[allow(clippy::too_many_arguments)]
fn start_actions(
    mut events: MessageReader<FpAction>,
    mut state: ResMut<FpState>,
    server: Res<AssetServer>,
    chars: Option<Res<CharacterAssets>>,
    mut rigs: Query<&mut CharacterRig>,
    mut visibility: Query<&mut Visibility>,
    mut health: Query<&mut crate::combat::Health, With<Player>>,
    mut rng: ResMut<crate::weapons::Rng>,
) {
    for ev in events.read() {
        let rig = state.rig.filter(|_| state.cinematic <= 0.0 && state.context.is_none());
        let (Some(rig), Some(chars)) = (rig, chars.as_ref()) else {
            if let (Some(to), Ok(mut h)) = (ev.heal_to(), health.single_mut()) {
                h.current = h.current.max(to);
            }
            continue;
        };
        let (options, full_body) = ev.clips();
        let (path, natural) = options[(rng.f32() * options.len() as f32) as usize % options.len()];
        let secs = ev.fitted().unwrap_or(natural);
        state.action_rate = natural / secs.max(0.1);
        let clip: Handle<Fc3Clip> = server.load(cache_path(&cache::anim_rel(&format!("{ANIM_ROOT}{path}"))));
        let Ok(mut r) = rigs.get_mut(rig) else { continue };
        r.set_clips(Arc::new(ClipSet { skeleton: chars.human.skeleton.clone(), clips: HashMap::from([(AnimKind::Shot, clip.clone())]) }));
        debug!("fp action {ev:?}: {path}");
        state.throwing = 0.0;
        state.cinematic = secs;
        state.takedown = Some(Takedown {
            clip,
            victim_clip: None,
            victim: None,
            anchor: None,
            chain: None,
            grenade: None,
            drag: false,
            second: None,
            below: false,
            dropped: false,
            water: false,
            pistol: Vec::new(),
            fired: 0,
            stages: Vec::new(),
            stage: 0,
            stage_time: 0.0,
            thrown: false,
            locked: full_body,
            heal: ev.heal_to().map(|to| (secs * 0.55, to)),
            props: None,
        });
        // The arms show (whatever the weapon), the weapon doesn't.
        if let Ok(mut v) = visibility.get_mut(rig) {
            *v = Visibility::Inherited;
        }
        if let Some(mut v) = state.mount.and_then(|m| visibility.get_mut(m).ok()) {
            *v = Visibility::Hidden;
        }
    }
}

/// `FC3_ACTION_TEST=syringe|manual[:bullet|explosion|melee|fire|drown]|body|plant|skin[:large]`
/// (tests): plays that action once the arms are up, with `--screenshot` and
/// `FC3_TD_SHOT=0.5,1.2` capturing `<screenshot>_tdN.png` that long after it starts.
#[allow(clippy::too_many_arguments)]
fn action_test(
    args: Res<crate::Args>,
    time: Res<Time>,
    state: Res<FpState>,
    mut clock: Local<(f32, Option<f32>, usize)>,
    players: Query<&Player>,
    mut health: Query<&mut crate::combat::Health, With<Player>>,
    mut actions: MessageWriter<FpAction>,
    mut commands: Commands,
) {
    let Ok(test) = std::env::var("FC3_ACTION_TEST") else { return };
    let (wait, started, shots) = &mut *clock;
    if players.single().map(|p| p.spawning).unwrap_or(true) || !state.active {
        return;
    }
    let dt = time.delta_secs();
    match started {
        None => {
            *wait += dt;
            if *wait < 2.0 {
                return;
            }
            use crate::combat::DamageKind as D;
            let (name, arg) = test.split_once(':').unwrap_or((test.as_str(), ""));
            let action = match name {
                "manual" => FpAction::ManualHeal {
                    kind: match arg {
                        "explosion" => D::Explosion,
                        "melee" => D::Melee,
                        "fire" => D::Fire,
                        "drown" => D::Drown,
                        _ => D::Bullet,
                    },
                    to: 50.0,
                },
                "body" => FpAction::LootBody { crouch: false },
                "plant" => FpAction::LootPlant { crouch: false },
                "skin" => FpAction::Skin { large: arg == "large" },
                _ => FpAction::Syringe { to: 100.0 },
            };
            if let (Some(_), Ok(mut h)) = (action.heal_to(), health.single_mut()) {
                h.current = 20.0;
            }
            info!("action-test: {action:?}");
            actions.write(action);
            *started = Some(0.0);
        }
        Some(t) => {
            *t += dt;
            let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
            if let (Some(&when), Some(path)) = (at.get(*shots), args.screenshot.as_ref()) {
                if *t >= when {
                    let stem = path.strip_suffix(".png").unwrap_or(path);
                    crate::camera::capture(&mut commands, format!("{stem}_td{shots}.png"));
                    *shots += 1;
                }
            }
            if let Ok(h) = health.single() {
                if *shots == at.len() && *t > 0.0 {
                    info!("action-test: health {:.0} after {:.1}s", h.current, *t);
                    *shots += 1;
                }
            }
        }
    }
}

/// Places an action's props on the arms once its clip has loaded (each on the
/// joint the clip names, posed by the next prop track with as many bones as
/// the prop model), heals partway through, and clears up afterwards.
#[allow(clippy::too_many_arguments)]
fn action_props(
    time: Res<Time>,
    mut state: ResMut<FpState>,
    clips: Res<Assets<Fc3Clip>>,
    models: Res<Assets<Fc3Model>>,
    rigs: Query<&CharacterRig>,
    mut health: Query<&mut crate::combat::Health, With<Player>>,
    mut commands: Commands,
) {
    if state.takedown.is_none() {
        for (e, _) in std::mem::take(&mut state.action_props) {
            commands.entity(e).despawn();
        }
        if let Some(e) = state.takedown_pistol.take() {
            commands.entity(e).despawn();
        }
        return;
    }
    let props = state.props.clone();
    let rig = state.rig.and_then(|r| rigs.get(r).ok());
    // The pistol takedown: the victim's pistol in hand from the draw on.
    let pistol = state.takedown.as_ref().is_some_and(|td| !td.pistol.is_empty() && td.stage >= 1);
    if pistol && state.takedown_pistol.is_none() {
        let hand = rig.and_then(|r| r.joint(name_hash("RightHand"))).map(|(_, j)| j);
        if let (Some(hand), Some(model)) = (hand, props.get("desert_eagle").and_then(|h| models.get(h))) {
            state.takedown_pistol = Some(spawn_prop(model, hand, &mut commands));
        }
    }
    let Some(td) = state.takedown.as_mut() else { return };
    if let Some((at, to)) = td.heal.as_mut() {
        *at -= time.delta_secs();
        if *at <= 0.0 {
            if let Ok(mut h) = health.single_mut() {
                if !h.dead {
                    h.current = h.current.max(*to);
                }
            }
            td.heal = None;
        }
    }
    if td.props.is_some() || td.victim.is_some() {
        return;
    }
    let (Some(clip), Some(rig)) = (clips.get(&td.clip), rig) else { return };
    let spawned: Vec<(Entity, usize)> = place_props(&clip.0, rig, &props, &models, &mut commands).into_iter().map(|(e, sub, _)| (e, sub)).collect();
    td.props = Some(spawned.len());
    state.action_props = spawned;
}

/// The grenade, rock, charge or mine in hand through a throw, gone from the
/// hand once it lets go.
fn gesture_props(time: Res<Time>, mut state: ResMut<FpState>, clips: Res<Assets<Fc3Clip>>, models: Res<Assets<Fc3Model>>, rigs: Query<&CharacterRig>, mut commands: Commands) {
    if state.throwing <= 0.0 {
        for (e, _, _) in std::mem::take(&mut state.gesture_props) {
            commands.entity(e).despawn();
        }
        state.gesture_clip = None;
        return;
    }
    state.gesture_time += time.delta_secs();
    if let Some(handle) = state.gesture_clip.clone() {
        let rig = state.rig.and_then(|r| rigs.get(r).ok());
        if let (Some(clip), Some(rig)) = (clips.get(&handle), rig) {
            state.gesture_clip = None;
            // As long as the clip runs.
            state.throwing = (clip.0.duration / GESTURE_RATE - state.gesture_time).max(state.throwing);
            let props = state.props.clone();
            state.gesture_props = place_props(&clip.0, rig, &props, &models, &mut commands);
        }
    }
    let (action, t) = (state.gesture_action, state.gesture_time);
    state.gesture_props.retain(|(e, _, object)| {
        let gone = release_time(action, object).is_some_and(|r| t >= r);
        if gone {
            commands.entity(*e).despawn();
        }
        !gone
    });
}

/// The paired clips (player, victim) under `ANIM_ROOT`.
fn takedown_clips(style: TakedownStyle, heavy: bool, variant: u32) -> (String, String) {
    let pair = |dir: &str, a: String, b: String| (format!(r"melee\{dir}\{a}"), format!(r"melee\{dir}\{b}"));
    match style {
        TakedownStyle::Below { front, water } => {
            let side = if front { "fw" } else { "bk" };
            let w = if water { "water" } else { "" };
            pair("takedown", format!("1stge_fulb_{w}ledgetakedown_{side}_var1_+000fw_nowep.mab"), format!("3rdge_fulb_{w}ledgetakedown_{side}_var1_+000fw_target_1.mab"))
        }
        // From his front or back (heavies: those two only), or either side.
        TakedownStyle::Above { from } => {
            let (kind, dir) = match (from, heavy) {
                (Quarter::Front, false) => ("a", "+000fw"),
                (Quarter::Front, true) => ("a_heavy", "+000fw"),
                (_, true) => ("a_heavy", "-180bk"),
                (Quarter::Back, false) => ("a", "-180bk"),
                (Quarter::Right, false) => ("b", "+090rt"),
                (Quarter::Left, false) => ("b", "-090lt"),
            };
            pair("stealthtakedown_above", format!("1stge_fulb_stealthtakedown_above_{kind}_{dir}_hhmac.mab"), format!("3rdge_fulb_stealthtakedown_above_{kind}_{dir}_target.mab"))
        }
        _ => {
            let side = if style == TakedownStyle::Front { "front" } else { "back" };
            // Heavies have one variant of their own.
            let (who, v) = if heavy { ("pirateheavy", 1) } else { ("pirateassault", variant % 3 + 1) };
            let name = format!("takedown{side}_kill_var{v}_{who}_+000fw_nowep.mab");
            pair("takedown", format!("1stge_fulb_{name}"), format!("3rdge_fulb_{name}"))
        }
    }
}

/// The chain takedown's follow-ups (player, victim): pulling the victim's knife,
/// then throwing it. They branch from the kill clip's hold.
fn chain_clips(style: TakedownStyle, variant: u32) -> [(String, String); 2] {
    let side = if style == TakedownStyle::Front { "front" } else { "back" };
    let v = match variant % 3 + 1 {
        1 => String::new(),
        n => format!("_var{n}"),
    };
    let pair = |name: String| (format!(r"melee\takedown\1stge_fulb_{name}"), format!(r"melee\takedown\3rdge_fulb_{name}"));
    [
        pair(format!("takedown{side}_kill2knife{v}_pirateassault_+000fw_nowep.mab")),
        pair(format!("takedown{side}_knifethrow_pirateassault_+000fw_nowep.mab")),
    ]
}

/// The grenade takedown's follow-ups (player, victim): grabbing the victim's
/// grenade, then the kick that sends him (pin pulled) about 6 m ahead.
fn grenade_clips(style: TakedownStyle, variant: u32) -> [(String, String); 2] {
    let side = if style == TakedownStyle::Front { "front" } else { "back" };
    let v = match variant % 3 + 1 {
        1 => String::new(),
        n => format!("_var{n}"),
    };
    let pair = |name: String| (format!(r"melee\takedown\1stge_fulb_{name}"), format!(r"melee\takedown\3rdge_fulb_{name}"));
    [
        pair(format!("takedown{side}_kill2grenade{v}_pirateassault_+000fw_nowep.mab")),
        pair(format!("takedown{side}_grenade_pirateassault_+000fw_nowep.mab")),
    ]
}

/// The takedown drag's follow-ups (player, victim): getting a grip, the hold
/// while dragging (looped), and letting go.
fn drag_clips(style: TakedownStyle, variant: u32) -> [(String, String); 3] {
    let side = if style == TakedownStyle::Front { "front" } else { "back" };
    let v = match variant % 3 + 1 {
        1 => String::new(),
        n => format!("_var{n}"),
    };
    let pair = |name: String| (format!(r"melee\takedown\1stge_fulb_{name}"), format!(r"melee\takedown\3rdge_fulb_{name}"));
    [
        pair(format!("takedown{side}_kill2drag{v}_pirateassault_+000fw_nowep.mab")),
        pair(format!("takedown{side}_drag_pirateassault_+000fw_nowep.mab")),
        pair(format!("takedown{side}_dragdrop_pirateassault_+000fw_nowep.mab")),
    ]
}

/// The pistol takedown's follow-ups (player, victim): drawing the victim's
/// pistol (his body held up in front), a shot for each enemy (he hangs in the
/// aiming pose; the scene turns to each), and letting him drop. The player's
/// shot and drop are the same from either side.
fn pistol_clips(style: TakedownStyle, variant: u32, shots: usize) -> Vec<(String, String)> {
    let side = if style == TakedownStyle::Front { "front" } else { "back" };
    let v = match variant % 3 + 1 {
        1 => String::new(),
        n => format!("_var{n}"),
    };
    let clip = |who: &str, side: &str, what: &str| format!(r"melee\takedown\{who}_fulb_takedown{side}_{what}_pirateassault_+000fw_nowep.mab");
    let mut stages = vec![(clip("1stge", side, &format!("kill2pistol{v}")), clip("3rdge", side, &format!("kill2pistol{v}")))];
    stages.extend((0..shots).map(|_| (clip("1stge", "front", "shoot"), clip("3rdge", side, "aiming"))));
    stages.push((clip("1stge", "front", "droppistol"), clip("3rdge", side, "droppistol")));
    stages
}

/// Pistol takedown timing: the draw, each shot (and when in it the round
/// goes), and the drop.
const PISTOL_DRAW: f32 = 1.8;
const PISTOL_SHOT: f32 = 0.6;
const PISTOL_FIRE: f32 = 0.05;
const PISTOL_DROP: f32 = 1.3;
/// The pistol's prop skeleton in the takedown clips.
const PISTOL_PROP: u32 = 0x876B_E34E;

/// The whole hold of a pistol takedown with this many shots.
pub fn pistol_time(shots: usize) -> f32 {
    CHAIN_BRANCH + PISTOL_DRAW + shots as f32 * PISTOL_SHOT + PISTOL_DROP
}

/// Takedown drag timing: getting a grip, the longest drag, and letting go.
const DRAG_GRIP: f32 = 1.33;
const DRAG_MAX: f32 = 15.0;
const DRAG_DROP: f32 = 1.5;

/// Grenade takedown timing: the grab's length, then (into the kick) when the
/// grenade on him goes off, and the whole hold.
const GRENADE_GRAB: f32 = 1.0;
const GRENADE_FUSE: f32 = 1.7;
pub const GRENADE_TIME: f32 = CHAIN_BRANCH + GRENADE_GRAB + 1.25;

/// A takedown victim with a live grenade on him (the grenade takedown).
#[derive(Component)]
pub struct LiveGrenade {
    rig: Option<Entity>,
    fallback: Vec3,
    fuse: f32,
    owner: Entity,
}

/// The grenade on a kicked victim goes off where his body has landed.
fn live_grenades(
    time: Res<Time>,
    mut grenades: Query<(Entity, &mut LiveGrenade)>,
    rigs: Query<&CharacterRig>,
    globals: Query<&GlobalTransform>,
    heights: Res<crate::terrain::Heightfield>,
    mut explosions: MessageWriter<crate::weapons::Explosion>,
    mut commands: Commands,
) {
    for (e, mut g) in &mut grenades {
        g.fuse -= time.delta_secs();
        if g.fuse > 0.0 {
            continue;
        }
        let hips = g.rig.and_then(|r| rigs.get(r).ok()).and_then(|r| r.joint(name_hash("Hips"))).and_then(|(_, j)| globals.get(j).ok()).map(|t| t.translation());
        // The clip lands him on its own flat ground: on a slope, up to the
        // surface where he lies.
        let mut at = hips.unwrap_or(g.fallback);
        if let Some(ground) = heights.height_at(at) {
            at.y = at.y.max(ground + 0.3);
        }
        explosions.write(crate::weapons::Explosion { at, radius: 8.0, damage: 200.0, instigator: Some(g.owner) });
        info!("grenade takedown: it went off at {at:.1?}");
        commands.entity(e).despawn();
    }
}

/// Chain takedown timing: where the kill clip branches into the knife pull,
/// the pull's length, when the knife leaves the hand, and the whole hold.
const CHAIN_BRANCH: f32 = 1.2;
const KNIFE_PULL: f32 = 0.5;
const KNIFE_RELEASE: f32 = 0.6;
pub const CHAIN_TIME: f32 = CHAIN_BRANCH + KNIFE_PULL + 1.75;

/// Starts a takedown: the victim is placed in front of the player and plays
/// his half (he dies into it); the arms play the player's half.
#[allow(clippy::too_many_arguments)]
fn start_takedowns(
    mut events: MessageReader<TakedownAnim>,
    mut state: ResMut<FpState>,
    mut cinematic: ResMut<Cinematic>,
    server: Res<AssetServer>,
    chars: Option<Res<CharacterAssets>>,
    mut rigs: Query<&mut CharacterRig>,
    npcs: Query<&crate::npc::Npc>,
    mut held: Query<(&ChildOf, &mut Visibility), With<crate::characters::HeldWeapon>>,
    mut variant: Local<u32>,
) {
    let Some(chars) = chars else { return };
    for ev in events.read() {
        let Ok(npc) = npcs.get(ev.victim) else { continue };
        *variant += 1;
        let double = ev.second.filter(|_| matches!(ev.style, TakedownStyle::Above { .. }) && !npc.role.armoured());
        let (mine, theirs) = match double {
            // Both from the front, the second to one side.
            Some((_, left)) => {
                let side = if left { "lt" } else { "rt" };
                let name = |who: &str| format!(r"melee\stealthtakedown_above\{who}_fulb_doubletakedownfront_{side}_var1_+000fw_");
                (format!("{}hhmac.mab", name("1stge")), format!("{}target_1.mab", name("3rdge")))
            }
            None => takedown_clips(ev.style, npc.role.armoured(), *variant),
        };
        let ground = matches!(ev.style, TakedownStyle::Back | TakedownStyle::Front);
        let chain = ev.chain.filter(|_| ground && !npc.role.armoured());
        let grenade = ev.grenade.filter(|_| ground && !npc.role.armoured());
        let drag = ev.drag && ground && !npc.role.armoured();
        let pistol: Vec<Entity> = ev.pistol.iter().flatten().copied().filter(|_| ground && !drag && !npc.role.armoured()).collect();
        debug!("takedown {:?}: {mine}", ev.style);
        if std::env::var("FC3_TD_DEBUG").is_ok() {
            info!("takedown {:?}: {mine} / {theirs}", ev.style);
        }
        let load = |rel: &str| -> Handle<Fc3Clip> { server.load(cache_path(&cache::anim_rel(&format!("{ANIM_ROOT}{rel}")))) };
        // He dies into his half (the death that follows plays it), unarmed as in the clip.
        let victim_clip = load(&theirs);
        if let Some(mut r) = npc.rig.and_then(|r| rigs.get_mut(r).ok()) {
            let clip = victim_clip.clone();
            r.set_clips(Arc::new(ClipSet { skeleton: chars.human.skeleton.clone(), clips: HashMap::from([(AnimKind::Death, clip.clone()), (AnimKind::Idle, clip)]) }));
            if let Some((_, hand)) = r.joint(name_hash("RightHand")) {
                for (_, mut v) in held.iter_mut().filter(|(c, _)| c.parent() == hand) {
                    *v = Visibility::Hidden;
                }
            }
        }
        // The second victim's half, the same way.
        let second = double.and_then(|(e2, left)| {
            let side = if left { "lt" } else { "rt" };
            let clip = load(&format!(r"melee\stealthtakedown_above\3rdge_fulb_doubletakedownfront_{side}_var1_+000fw_target_2.mab"));
            let n2 = npcs.get(e2).ok()?;
            if let Some(mut r) = n2.rig.and_then(|r| rigs.get_mut(r).ok()) {
                r.set_clips(Arc::new(ClipSet { skeleton: chars.human.skeleton.clone(), clips: HashMap::from([(AnimKind::Death, clip.clone()), (AnimKind::Idle, clip.clone())]) }));
                if let Some((_, hand)) = r.joint(name_hash("RightHand")) {
                    for (_, mut v) in held.iter_mut().filter(|(c, _)| c.parent() == hand) {
                        *v = Visibility::Hidden;
                    }
                }
            }
            Some((e2, clip))
        });
        let Some(mut r) = state.rig.and_then(|r| rigs.get_mut(r).ok()) else { continue };
        let clip = load(&mine);
        r.set_clips(Arc::new(ClipSet { skeleton: chars.human.skeleton.clone(), clips: HashMap::from([(AnimKind::Shot, clip.clone())]) }));
        let stages = match (chain, grenade) {
            _ if drag => drag_clips(ev.style, *variant).iter().map(|(a, b)| (load(a), load(b))).collect(),
            _ if !pistol.is_empty() => pistol_clips(ev.style, *variant, pistol.len()).iter().map(|(a, b)| (load(a), load(b))).collect(),
            (_, Some(_)) => grenade_clips(ev.style, *variant).iter().map(|(a, b)| (load(a), load(b))).collect(),
            (Some(_), _) => chain_clips(ev.style, *variant).iter().map(|(a, b)| (load(a), load(b))).collect(),
            _ => Vec::new(),
        };
        let time = if second.is_some() {
            DOUBLE_TIME
        } else if drag {
            // Until the grip; the drag then holds it open.
            CHAIN_BRANCH + DRAG_GRIP + 0.5
        } else if !pistol.is_empty() {
            pistol_time(pistol.len())
        } else if grenade.is_some() {
            GRENADE_TIME
        } else if chain.is_some() {
            CHAIN_TIME
        } else {
            TAKEDOWN_TIME
        };
        state.throwing = 0.0;
        state.cinematic = time;
        state.takedown = Some(Takedown {
            clip,
            victim_clip: Some(victim_clip),
            victim: Some(ev.victim),
            anchor: None,
            locked: true,
            heal: None,
            props: None,
            chain,
            grenade,
            drag,
            second,
            below: matches!(ev.style, TakedownStyle::Below { .. }),
            dropped: false,
            water: matches!(ev.style, TakedownStyle::Below { water: true, .. }),
            pistol,
            fired: 0,
            stages,
            stage: 0,
            stage_time: 0.0,
            thrown: false,
        });
        cinematic.0 = time;
    }
}

/// A takedown under way.
struct Takedown {
    clip: Handle<Fc3Clip>,
    victim_clip: Option<Handle<Fc3Clip>>,
    victim: Option<Entity>,
    anchor: Option<Anchor>,
    /// The view follows the clip's camera and the player can't move (takedowns
    /// and full-body actions).
    locked: bool,
    /// A heal still to come (seconds, health to bring it up to).
    heal: Option<(f32, f32)>,
    /// How many props were placed, once they have been.
    props: Option<usize>,
    /// Chain takedown: the knife's target and the follow-up clips (player,
    /// victim), the one playing (0 = the kill) and seconds into it.
    chain: Option<Entity>,
    /// Grenade takedown: the group he's kicked into.
    grenade: Option<Vec3>,
    /// Takedown drag (stage 2 is the drag itself, with the player in control).
    drag: bool,
    /// A double takedown's second victim and his half.
    second: Option<(Entity, Handle<Fc3Clip>)>,
    /// From below: the victim starts up on the ledge, where his half's first
    /// root key puts him relative to the player's.
    below: bool,
    /// From below: his half is over and he's falling off the ledge.
    dropped: bool,
    /// From below, out of the water: his half takes him in itself.
    water: bool,
    /// The pistol takedown's targets, and how many have been shot.
    pistol: Vec<Entity>,
    fired: usize,
    stages: Vec<(Handle<Fc3Clip>, Handle<Fc3Clip>)>,
    stage: usize,
    stage_time: f32,
    thrown: bool,
}

/// Where the paired clips' shared origin was put (feet level) and their
/// facing, the view at the start, and seconds since.
#[derive(Clone, Copy)]
struct Anchor {
    origin: Vec3,
    yaw: f32,
    eye: Vec3,
    view: Quat,
    age: f32,
    /// The victim's turn from his kill clip (kept through the follow-ups).
    turn: Option<f32>,
}

/// The `Camera` bone in rig space, from the joints' local poses.
fn camera_bone(rig: &CharacterRig, local: impl Fn(Entity) -> Option<Transform>) -> Option<Mat4> {
    bone_matrix(rig, "Camera", local)
}

/// A joint's pose in the rig's space, from the joints' local transforms.
fn bone_matrix(rig: &CharacterRig, name: &str, local: impl Fn(Entity) -> Option<Transform>) -> Option<Mat4> {
    let (mut i, _) = rig.joint(name_hash(name))?;
    let mut m = Mat4::IDENTITY;
    loop {
        let Some(t) = rig.joint_entity(i).and_then(&local) else { break };
        m = t.to_matrix() * m;
        let parent = rig.parent(i);
        if parent < 0 || parent as usize == i {
            break;
        }
        i = parent as usize;
    }
    Some(m)
}

/// A takedown plays out where it started: the clips' shared origin is fixed in
/// the world, the victim stands on it and the camera rides the player clip's
/// camera bone (eased in from the view at the start).
#[allow(clippy::type_complexity)]
fn drive_takedown(
    time: Res<Time>,
    mut state: ResMut<FpState>,
    clips: Res<Assets<Fc3Clip>>,
    mut rigs: Query<&mut CharacterRig>,
    joints: Query<&Transform, (Without<Player>, Without<crate::player::PlayerCamera>, Without<crate::npc::Npc>)>,
    mut players: Query<(Entity, &mut Transform, &mut Player), Without<crate::player::PlayerCamera>>,
    mut cams: Query<&mut Transform, (With<crate::player::PlayerCamera>, Without<Player>)>,
    mut npcs: Query<(&mut Transform, &mut crate::npc::Npc), (Without<Player>, Without<crate::player::PlayerCamera>)>,
    (mut damage, mut sfx, mut particles, mut rng, mut noise): (
        MessageWriter<crate::combat::DamageEvent>,
        MessageWriter<crate::sound::PlaySfx>,
        ResMut<crate::particles::Particles>,
        ResMut<crate::weapons::Rng>,
        MessageWriter<crate::npc::Noise>,
    ),
    mut commands: Commands,
    (actions, heights): (Res<crate::controls::ActionState>, Res<crate::terrain::Heightfield>),
) {
    use crate::player::HALF_HEIGHT;
    let mut state_cinematic: Option<f32> = None;
    if state.cinematic <= 0.0 {
        state.takedown = None;
        return;
    }
    let Some(rig_e) = state.rig else { return };
    let Some(td) = state.takedown.as_mut().filter(|td| td.locked || td.drag) else { return };
    if clips.get(&td.clip).is_none() {
        return;
    }
    let Ok((pe, mut pt, mut p)) = players.single_mut() else { return };
    // Chain takedown: from the kill's hold into the knife pull, then the throw.
    let dt = time.delta_secs();
    if td.anchor.is_some() {
        td.stage_time += dt;
    }
    // Letting go of a dragged body: either key, or when it's been long enough.
    let drop = td.drag && td.stage == 2 && (actions.just_pressed(crate::controls::Action::Interact) || actions.just_pressed(crate::controls::Action::Melee) || td.stage_time > DRAG_MAX);
    let next = match td.stage {
        0 => CHAIN_BRANCH,
        1 if td.drag => DRAG_GRIP,
        2 if td.drag => {
            if drop {
                0.0
            } else {
                f32::INFINITY
            }
        }
        1 if td.grenade.is_some() => GRENADE_GRAB,
        1 if !td.pistol.is_empty() => PISTOL_DRAW,
        s if !td.pistol.is_empty() && (2..=td.pistol.len() + 1).contains(&s) => PISTOL_SHOT,
        1 => KNIFE_PULL,
        _ => f32::INFINITY,
    };
    if td.stage_time >= next && td.stage < td.stages.len() {
        let (mine, theirs) = td.stages[td.stage].clone();
        let skeleton = rigs.get(rig_e).ok().map(|r| r.clips().skeleton.clone());
        if let (Some(skeleton), Ok(mut r)) = (skeleton, rigs.get_mut(rig_e)) {
            r.set_clips(Arc::new(ClipSet { skeleton: skeleton.clone(), clips: HashMap::from([(AnimKind::Shot, mine.clone())]) }));
            let victim_rig = td.victim.and_then(|v| npcs.get(v).ok()).and_then(|(_, n)| n.rig);
            if let Some(mut vr) = victim_rig.and_then(|v| rigs.get_mut(v).ok()) {
                vr.set_clips(Arc::new(ClipSet { skeleton, clips: HashMap::from([(AnimKind::Death, theirs.clone()), (AnimKind::Idle, theirs)]) }));
            }
        }
        td.clip = mine;
        td.stage += 1;
        td.stage_time = 0.0;
        if td.drag {
            match td.stage {
                // The drag: the player walks (and looks) with the body.
                2 => td.locked = false,
                // Letting go: the clip takes the view again.
                3 => {
                    td.locked = true;
                    state_cinematic = Some(DRAG_DROP);
                }
                _ => {}
            }
        }
    }
    p.dragging = td.drag && td.stage == 2;
    if p.dragging {
        // Held open while dragging.
        state_cinematic = Some(1.0);
    }
    let target = td.chain.and_then(|c| npcs.get(c).ok()).map(|(t, _)| t.translation + Vec3::Y * 0.4).or(td.grenade);
    // The pistol takedown's man to shoot (from the draw, the first).
    let aim = match td.stage {
        1 => td.pistol.first(),
        s if s >= 2 => td.pistol.get(s - 2),
        _ => None,
    }
    .and_then(|e| npcs.get(*e).ok())
    .map(|(t, _)| t.translation + Vec3::Y * 0.6);
    if let (Some(at), true) = (aim, td.stage >= 2 && td.fired < td.stage - 1 && td.stage_time >= PISTOL_FIRE) {
        let shot = td.pistol[td.stage - 2];
        td.fired += 1;
        let eye = pt.translation + Vec3::Y * (p.eye - HALF_HEIGHT);
        let dir = (at - eye).normalize_or_zero();
        let muzzle = eye + dir * 0.6 - Vec3::Y * 0.12;
        damage.write(crate::combat::DamageEvent { target: shot, amount: 10_000.0, kind: crate::combat::DamageKind::Bullet, source: Some(eye), instigator: Some(pe), headshot: true });
        crate::particles::fx::muzzle_flash(&mut particles, &mut rng, muzzle, dir, 1.0);
        crate::particles::fx::blood(&mut particles, &mut rng, at, dir);
        sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::PISTOL, at: Some(muzzle), volume: 1.0 });
        crate::npc::player_noise(&mut noise, muzzle, 120.0);
    }
    // The kick: the grenade on him is live from here.
    if td.grenade.is_some() && td.stage == 2 && !td.thrown {
        td.thrown = true;
        let victim = td.victim.and_then(|v| npcs.get(v).ok());
        let fallback = victim.as_ref().map(|(t, _)| t.translation).unwrap_or(pt.translation) + Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z * 6.0;
        let rig = victim.and_then(|(_, n)| n.rig);
        commands.spawn(LiveGrenade { rig, fallback, fuse: GRENADE_FUSE, owner: pe });
        sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: Some(pt.translation), volume: 0.6 });
    }
    if td.chain.is_some() && td.stage == 2 && !td.thrown && td.stage_time >= KNIFE_RELEASE {
        td.thrown = true;
        if let (Some(c), Some(at)) = (td.chain, target) {
            damage.write(crate::combat::DamageEvent { target: c, amount: 10_000.0, kind: crate::combat::DamageKind::Melee, source: Some(pt.translation), instigator: Some(pe), headshot: false });
            let dir = (at - pt.translation).normalize_or_zero();
            crate::particles::fx::blood(&mut particles, &mut rng, at, dir);
            sfx.write(crate::sound::PlaySfx { event: crate::sound::switched(crate::sound::ev::MELEE_IMPACT, crate::surfaces::value::FLESH, 0), at: Some(at), volume: 0.8 });
        }
    }
    let Ok(rig) = rigs.get(rig_e) else { return };
    let Some(m) = camera_bone(rig, |e| joints.get(e).ok().copied()) else { return };
    let (_, bone_rot, bone_pos) = m.to_scale_rotation_translation();
    let eye_offset = Vec3::Y * (p.eye - HALF_HEIGHT);
    // Dragging: the scene follows the player (who moves and looks freely),
    // the body held in front at the clips' shared origin.
    if p.dragging {
        let (bone_yaw, _, _) = bone_rot.to_euler(EulerRot::YXZ);
        let yaw = p.yaw - bone_yaw;
        let eye = pt.translation + eye_offset;
        let mut origin = eye - Quat::from_rotation_y(yaw) * bone_pos;
        origin.y = pt.translation.y - HALF_HEIGHT;
        if let Some(a) = td.anchor.as_mut() {
            a.origin = origin;
            a.yaw = yaw;
            a.eye = eye;
            a.view = Quat::from_euler(EulerRot::YXZ, p.yaw, p.pitch, 0.0);
            a.age = 0.0;
        }
        let turn = td.anchor.and_then(|a| a.turn).unwrap_or(0.0);
        if let Some(Ok((mut nt, mut npc))) = td.victim.map(|v| npcs.get_mut(v)) {
            npc.yaw = yaw + turn;
            nt.translation = origin + Vec3::Y * HALF_HEIGHT;
            nt.rotation = Quat::from_rotation_y(npc.yaw);
        }
        if let Some(c) = state_cinematic {
            state.cinematic = c;
        }
        return;
    }
    let a = td.anchor.get_or_insert_with(|| {
        // Line the clip's camera up with the view (yaw only); the origin on the ground.
        let (bone_yaw, _, _) = bone_rot.to_euler(EulerRot::YXZ);
        let yaw = p.yaw - bone_yaw;
        let eye = pt.translation + eye_offset;
        let mut origin = eye - Quat::from_rotation_y(yaw) * bone_pos;
        origin.y = pt.translation.y - HALF_HEIGHT;
        Anchor { origin, yaw, eye, view: Quat::from_euler(EulerRot::YXZ, p.yaw, p.pitch, 0.0), age: 0.0, turn: None }
    });
    a.age += dt;
    // Pulling the knife, the scene swings round (about its origin) to face the next enemy.
    let swing = target.filter(|_| (1..=2).contains(&td.stage) && !td.thrown).or(aim);
    if let Some(at) = swing {
        let eye = a.origin + Quat::from_rotation_y(a.yaw) * bone_pos;
        let want = (-(at.x - eye.x)).atan2(-(at.z - eye.z));
        let have = (Quat::from_rotation_y(a.yaw) * bone_rot).to_euler(EulerRot::YXZ).0;
        let delta = (want - have + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        a.yaw += delta * (dt * 8.0).min(1.0);
    }
    let frame = Quat::from_rotation_y(a.yaw);
    // From below the player's root moves too (up to grab him, back down):
    // its motion since the first key, in the scene's frame.
    let (root_t, root_r) = if td.below {
        let (kind, time) = rig.playing();
        clips
            .get(&td.clip)
            .and_then(|c| c.0.root_motion.as_ref().map(|r| (r, crate::characters::clip_time(kind, time, c.0.duration))))
            .map(|(r, t)| {
                let (dt_, dr) = r.sample(t);
                let r0 = Quat::from_array(r.start_rotation);
                (r0 * Vec3::from_array(dt_), r0 * Quat::from_array(dr) * r0.inverse())
            })
            .unwrap_or((Vec3::ZERO, Quat::IDENTITY))
    } else {
        (Vec3::ZERO, Quat::IDENTITY)
    };
    let ease = (a.age / 0.3).min(1.0);
    let eye = a.eye.lerp(a.origin + frame * (root_t + root_r * bone_pos), ease);
    let (yaw, pitch, roll) = a.view.slerp(frame * root_r * bone_rot, ease).to_euler(EulerRot::YXZ);
    p.yaw = yaw;
    p.pitch = pitch;
    p.velocity = Vec3::ZERO;
    pt.rotation = Quat::from_rotation_y(yaw);
    pt.translation = eye - eye_offset;
    if let Ok(mut ct) = cams.single_mut() {
        ct.rotation = Quat::from_euler(EulerRot::YXZ, 0.0, pitch, roll);
    }
    // The victim's clip turns him by its reference rotation (round to face us
    // for the frontal kills).
    if a.turn.is_none() {
        a.turn = td.victim_clip.as_ref().and_then(|c| clips.get(c)).map(|c| Quat::from_array(c.0.reference_rotation).to_euler(EulerRot::YXZ).0);
        if let (Some(turn), Some(Ok((_, npc)))) = (a.turn, td.victim.map(|v| npcs.get(v))) {
            let by = (a.yaw + turn - npc.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            // FC3_TD_DEBUG: how far he was swung round to fit his half.
            if std::env::var("FC3_TD_DEBUG").is_ok() {
                info!("takedown: the victim turned {:.0} degrees into his half", by.to_degrees());
            }
        }
    }
    let turn = a.turn.unwrap_or(0.0);
    // From below he starts up on the ledge: the halves' first root keys give
    // where (the other takedowns share one origin).
    let start = |h: &Handle<Fc3Clip>| clips.get(h).and_then(|c| c.0.root_motion.as_ref()).map(|r| Vec3::from_array(r.start_translation));
    let lift = match (td.below, td.victim_clip.as_ref().and_then(start), start(&td.clip)) {
        (true, Some(v), Some(p)) => Quat::from_rotation_y(a.yaw) * (v - p),
        _ => Vec3::ZERO,
    };
    // And his root moves with his half: to the edge, or (from the water) off
    // the dock and in, turning as he goes.
    let victim_motion = |rigs: &Query<&mut CharacterRig>, rig: Option<Entity>| -> (Vec3, f32) {
        if !td.below {
            return (Vec3::ZERO, 0.0);
        }
        let Some((kind, time)) = rig.and_then(|r| rigs.get(r).ok()).map(|r| r.playing()) else { return (Vec3::ZERO, 0.0) };
        td.victim_clip
            .as_ref()
            .and_then(|h| clips.get(h))
            .and_then(|c| c.0.root_motion.as_ref().map(|r| (r, crate::characters::clip_time(kind, time, c.0.duration))))
            .map(|(r, t)| {
                let (moved, turned) = r.sample(t);
                (Quat::from_array(r.start_rotation) * Vec3::from_array(moved), Quat::from_array(turned).to_euler(EulerRot::YXZ).0)
            })
            .unwrap_or((Vec3::ZERO, 0.0))
    };
    // His half pulls him to the edge; then he goes over it (the original
    // hands him to its ragdoll here).
    let over = td.below
        && td.victim.and_then(|v| npcs.get(v).ok()).and_then(|(_, n)| n.rig).and_then(|r| rigs.get(r).ok()).is_some_and(|r| {
            let (kind, time) = r.playing();
            td.victim_clip.as_ref().and_then(|h| clips.get(h)).is_some_and(|c| crate::characters::clip_time(kind, time, c.0.duration) >= c.0.duration - 0.05)
        });
    if over && !td.dropped && !td.water {
        td.dropped = true;
        let out = start(&td.clip).zip(td.victim_clip.as_ref().and_then(start)).map(|(p, v)| frame * (p - v)).unwrap_or(Vec3::ZERO);
        // Past the player's shoulder, to the side his half pulls him.
        let pulled = td.victim_clip.as_ref().and_then(|h| clips.get(h)).and_then(|c| c.0.root_motion.as_ref()).map(|r| r.sample(f32::MAX).0[0]).unwrap_or(1.0);
        let side = frame * Vec3::X * if pulled < 0.0 { -1.0 } else { 1.0 };
        let out = Vec3::new(out.x, 0.0, out.z).normalize_or_zero() * 0.55 + side * 0.85;
        if let Some(v) = td.victim {
            commands.entity(v).insert(crate::npc::LedgeFall::toward(out));
        }
    }
    if let Some(Ok((mut nt, mut npc))) = td.victim.filter(|_| !td.dropped).map(|v| npcs.get_mut(v)) {
        let (moved, turned) = victim_motion(&rigs, npc.rig);
        npc.yaw = a.yaw + turn + turned;
        nt.translation = a.origin + lift + frame * moved + Vec3::Y * HALF_HEIGHT;
        nt.rotation = Quat::from_rotation_y(npc.yaw);
    }
    // A double takedown's second victim stands where his half starts, beside
    // the first (their halves' first root keys give the offset).
    if let Some((e2, clip2)) = td.second.clone() {
        let starts = |h: &Handle<Fc3Clip>| clips.get(h).and_then(|c| c.0.root_motion.as_ref().map(|r| (Vec3::from_array(r.start_translation), Quat::from_array(c.0.reference_rotation))));
        let first = td.victim_clip.as_ref().and_then(starts);
        if let (Some((s1, _)), Some((s2, r2))) = (first, starts(&clip2)) {
            if let Ok((mut nt, mut npc)) = npcs.get_mut(e2) {
                let offset = Quat::from_rotation_y(a.yaw) * (s2 - s1);
                npc.yaw = a.yaw + r2.to_euler(EulerRot::YXZ).0;
                let mut at = a.origin + Vec3::new(offset.x, 0.0, offset.z);
                // On his own ground (the scene was laid out on the flat).
                at.y = heights.height_at(at).unwrap_or(at.y);
                nt.translation = at + Vec3::Y * HALF_HEIGHT;
                nt.rotation = Quat::from_rotation_y(npc.yaw);
            }
        }
    }
    // Letting go of a dragged body: its clip's length from here.
    if let Some(c) = state_cinematic {
        state.cinematic = c;
    }
}

/// The machete's prop skeleton (its track in clips that carry several props).
const MACHETE_PROP: u32 = 0x744C_BC42;

/// How long a takedown holds the player (the paired clips run about 2.1-2.3 s).
const TAKEDOWN_TIME: f32 = 2.1;
/// A double takedown's hold (its clip runs 3.8 s).
pub const DOUBLE_TIME: f32 = 3.4;

/// Actions and the clip names that provide them, best first.
const ACTIONS: &[(AnimKind, &[&str])] = &[
    (AnimKind::Idle, &["btheaimslow", "idleaim"]),
    (AnimKind::IronIdle, &["btheironslow", "idleiron"]),
    // Machete: right and left swings take turns.
    (AnimKind::Shot, &["shootcycle", "machete_right_a"]),
    (AnimKind::IronShot, &["shootcycleiron", "ironshoot", "machete_left_a", "shootcycle"]),
    (AnimKind::Reload, &["reloadregular", "reloadaim", "reloadbegin", "reloadbeginregular"]),
    (AnimKind::ReloadEmpty, &["reloadforced", "reloadregular", "reloadaim", "reloadbeginforced"]),
    (AnimKind::Draw, &["draw"]),
    (AnimKind::Holster, &["holster"]),
    (AnimKind::Sprint, &["sprintcycle", "sprint"]),
    (AnimKind::Walk, &["wlk", "wlkstd", "stdwlk"]),
    (AnimKind::Slide, &["slidestart", "slidepose"]),
];

#[derive(Resource, Default)]
pub struct FpState {
    /// Clip paths by folder and clip name.
    index: HashMap<String, HashMap<String, String>>,
    arms: Option<Handle<Fc3Model>>,
    sets: HashMap<String, Arc<ClipSet>>,
    /// The arms rig (under the view model) and the weapon mount on its hand.
    rig: Option<Entity>,
    pub mount: Option<Entity>,
    /// The current weapon is held by the arms (else the plain view model).
    pub active: bool,
    weapon: Option<usize>,
    /// Seconds into the draw after a weapon change.
    drawing: f32,
    last_since_shot: f32,
    shots: u32,
    /// Length of the reload under way (to fit the clip to it).
    reload_total: f32,
    last_reloading: f32,
    /// A one-off throw to play (`grenade` or `rock`, the clip folder, or
    /// `folder|clip`), set by the throwing systems; the weapon is put away
    /// for it.
    pub gesture: Option<&'static str>,
    /// Seconds of the throw left, and the weapon's clips to go back to.
    throwing: f32,
    /// Seconds left of a context's intro clip.
    intro: f32,
    weapon_set: Option<Arc<ClipSet>>,
    context: Option<Context>,
    /// Seconds left of a takedown or action on the arms.
    cinematic: f32,
    takedown: Option<Takedown>,
    /// Prop models for actions, by the clips' object names, and the props
    /// placed for the current one (entity, prop track).
    props: HashMap<&'static str, Handle<Fc3Model>>,
    action_props: Vec<(Entity, usize)>,
    /// Playback rate of the current action's clip.
    action_rate: f32,
    /// On a mounted gun: where the gun sits relative to the view.
    mounted: Option<Vec3>,
    /// The throw's clip (until its props are placed), its name, how long
    /// it has played, and the props in hand (entity, prop track, object).
    gesture_clip: Option<Handle<Fc3Clip>>,
    gesture_action: &'static str,
    gesture_time: f32,
    gesture_props: Vec<(Entity, usize, String)>,
    /// The victim's pistol in hand (the pistol takedown).
    takedown_pistol: Option<Entity>,
    /// The wingsuit's membrane between the arms and body while flying it (its
    /// rig), its model and skeleton, and its tracks from the deploy and the
    /// glide.
    membrane: Option<Entity>,
    membrane_assets: Option<(Handle<Fc3Model>, Handle<crate::cache_assets::Fc3Skeleton>)>,
    membrane_clips: Option<(Handle<Fc3Clip>, Handle<Fc3Clip>)>,
    /// The welding torch in hand while repairing.
    torch: Option<Entity>,
    /// The tagging camera in hand.
    camera: Option<Entity>,
}

impl FpState {
    /// The first-person arms rig, once spawned.
    pub fn rig(&self) -> Option<Entity> {
        self.rig
    }

    /// The arms are busy swimming, climbing, riding or driving.
    pub fn in_context(&self) -> bool {
        self.context.is_some()
    }
}

fn load_fp(mut state: ResMut<FpState>, server: Res<AssetServer>, info: Res<crate::data::CacheInfo>) {
    let text = std::fs::read_to_string(info.root.join(cache::FP_CLIPS_REL)).unwrap_or_default();
    for line in text.lines() {
        let mut it = line.splitn(3, ' ');
        if let (Some(folder), Some(action), Some(path)) = (it.next(), it.next(), it.next()) {
            // The molotov's throw shares the grenade folder.
            let action = if path.ends_with("_molotov.mab") { format!("{action}_molotov") } else { action.to_string() };
            state.index.entry(folder.to_string()).or_default().insert(action, path.to_string());
        }
    }
    if state.index.is_empty() {
        warn!("no first-person clips in the cache ({}); re-run the converter", cache::FP_CLIPS_REL);
        return;
    }
    state.arms = Some(server.load_builder().with_settings(|s: &mut ModelSettings| s.skinned = true).load(skinned_cache_path(&cache::model_rel(ARMS))));
    state.props = PROPS.iter().map(|(name, path)| (*name, server.load(cache_path(&cache::model_rel(path))))).collect();
}

/// How far a mounted gun and the arms on it sit out from where the clips put
/// them (view space), to bring the hands on the grips into view.
pub const MOUNT_VIEW: Vec3 = Vec3::new(0.0, 0.05, -0.2);

/// Mounted guns' clip folders, by model folder.
const MOUNTED_FOLDERS: &[(&str, &str)] = &[("dshk", "mounted\\dshk"), ("lmg", "mounted\\mkg")];

/// The clip folder for a weapon model path (`ak74m\ak74m.xbg` -> `primary\ak74m`).
fn folder_for(model: &str) -> Option<&'static str> {
    let dir = model.split('\\').next()?.to_ascii_lowercase();
    FOLDERS.iter().find(|(d, _)| *d == dir).map(|(_, f)| *f)
}

/// The clip set for a weapon folder, loading it on first use; none unless it
/// has at least an idle and a shot.
fn clip_set(state: &mut FpState, folder: &str, skeleton: &Handle<crate::cache_assets::Fc3Skeleton>, server: &AssetServer) -> Option<Arc<ClipSet>> {
    if let Some(s) = state.sets.get(folder) {
        return Some(s.clone());
    }
    let clips = state.index.get(folder)?;
    let mut map = HashMap::new();
    for (kind, names) in ACTIONS {
        if let Some(path) = names.iter().find_map(|n| clips.get(*n)) {
            map.insert(*kind, server.load::<Fc3Clip>(cache_path(&cache::anim_rel(path))));
        }
    }
    if !map.contains_key(&AnimKind::Idle) || !map.contains_key(&AnimKind::Shot) {
        return None;
    }
    let set = Arc::new(ClipSet { skeleton: skeleton.clone(), clips: map });
    state.sets.insert(folder.to_string(), set.clone());
    Some(set)
}

/// Builds the arms rig under the view model once the model has loaded.
fn spawn_arms(
    mut state: ResMut<FpState>,
    mut commands: Commands,
    models: Res<Assets<Fc3Model>>,
    server: Res<AssetServer>,
    chars: Option<Res<CharacterAssets>>,
    view: Query<Entity, With<ViewModel>>,
    rigs: Query<(&CharacterRig, &Children)>,
    parts: Query<(), With<crate::original_shaders::SkinnedPart>>,
) {
    // Finish setting up a rig spawned last frame: the weapon mount and shadows.
    if let (Some(rig), None) = (state.rig, state.mount) {
        if let Ok((r, children)) = rigs.get(rig) {
            if let Some((_, hand)) = r.joint(name_hash("RightHand")) {
                state.mount = Some(commands.spawn((Transform::default(), Visibility::Inherited, ChildOf(hand))).id());
            }
            for c in children.iter().filter(|c| parts.contains(*c)) {
                commands.entity(c).insert(NotShadowCaster);
            }
        }
        return;
    }
    if state.rig.is_some() {
        return;
    }
    let (Some(arms), Some(chars), Ok(vm)) = (state.arms.clone(), chars, view.single()) else { return };
    if !crate::cache_assets::usable(&server, arms.id()) {
        return;
    }
    let Some(model) = models.get(&arms) else { return };
    let Some(folder) = FOLDERS.iter().map(|(_, f)| *f).find(|f| state.index.contains_key(*f)) else { return };
    let Some(set) = clip_set(&mut state, folder, &chars.human.skeleton, &server) else { return };
    let Some(rig) = spawn_rig_with(&mut commands, vm, &[model], None, set, Vec3::ZERO, None) else { return };
    commands.entity(rig).insert((AnimRestart::default(), Visibility::Hidden));
    state.rig = Some(rig);
}

/// Picks the clips for the weapon in hand and tells the view model which way
/// it is held.
#[allow(clippy::too_many_arguments)]
fn select_weapon(
    mut state: ResMut<FpState>,
    server: Res<AssetServer>,
    chars: Option<Res<CharacterAssets>>,
    players: Query<&Inventory>,
    mut rigs: Query<(&mut CharacterRig, &mut Visibility)>,
    mut view: Query<&mut ViewModel>,
    mut mounts: Query<&mut Visibility, Without<CharacterRig>>,
) {
    let (Ok(inv), Some(chars), Some(rig), Some(_)) = (players.single(), chars, state.rig, state.mount) else { return };
    let def = inv.slot().def;
    // A takedown or action finishes first.
    if state.weapon == Some(def) || state.cinematic > 0.0 {
        return;
    }
    state.weapon = Some(def);
    let d = &inv.defs[def];
    // A mounted gun sits before the view with the hands on its grips.
    let mounted = d.class == crate::weapons::WeaponClass::Mounted;
    state.mounted = mounted.then_some(d.hold);
    let dir = d.model.split('\\').next().unwrap_or_default().to_ascii_lowercase();
    let folder = if mounted { MOUNTED_FOLDERS.iter().find(|(m, _)| *m == dir).map(|(_, f)| *f) } else { folder_for(d.model) };
    let set = folder.and_then(|f| clip_set(&mut state, f, &chars.human.skeleton, &server));
    let Ok((mut r, mut vis)) = rigs.get_mut(rig) else { return };
    state.active = set.is_some();
    *vis = if state.active || state.context.is_some() { Visibility::Inherited } else { Visibility::Hidden };
    state.weapon_set = set.clone();
    // A throw cut short by the switch: the weapon shows again.
    state.throwing = 0.0;
    if let Some(mut m) = state.mount.and_then(|m| mounts.get_mut(m).ok()) {
        *m = if state.context.is_some() { Visibility::Hidden } else { Visibility::Inherited };
    }
    // Busy arms keep their clip; the weapon's comes back afterwards.
    if state.context.is_some() {
        if let Ok(mut v) = view.single_mut() {
            v.reshow();
        }
        return;
    }
    if let Some(set) = set {
        r.set_clips(set);
        state.drawing = 0.0;
    }
    // The weapon model moves between the hand and the plain view model.
    if let Ok(mut v) = view.single_mut() {
        v.reshow();
    }
}

/// What the arms are doing: draw, reload, a shot, aiming, sprinting, walking
/// or breathing.
fn drive_arms(
    time: Res<Time>,
    clips: Res<Assets<Fc3Clip>>,
    mut state: ResMut<FpState>,
    players: Query<(&Inventory, &Player)>,
    mut rigs: Query<(&mut CharacterRig, &mut AnimRequest, &mut AnimRestart)>,
    server: Res<AssetServer>,
    chars: Option<Res<CharacterAssets>>,
    mut mounts: Query<&mut Visibility>,
    (holding, driving, vehicles, plain): (Res<crate::traversal::Mount>, Res<crate::vehicles::Driving>, Query<&crate::vehicles::Vehicle>, Query<Entity, With<crate::weapons::ViewModelPart>>),
    (mut cinematic, mut busy, repairing, bino): (ResMut<Cinematic>, ResMut<HandsBusy>, Res<crate::vehicles::Repairing>, Res<crate::stealth::Binoculars>),
    mut commands: Commands,
) {
    let Some(rig) = state.rig else { return };
    let Ok((inv, p)) = players.single() else { return };
    let Ok((mut r, mut req, mut restart)) = rigs.get_mut(rig) else { return };
    // Swimming, climbing, on a zip line or at the wheel: the arms do that
    // instead, with the weapon put away.
    let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    let context = if p.driving && driving.first_person {
        driving.vehicle.and_then(|v| vehicles.get(v).ok()).map(|v| match v.kind() {
            crate::vehicles::Kind::Car => Context::Drive,
            crate::vehicles::Kind::Boat => Context::Boat,
            crate::vehicles::Kind::Glider => Context::Glide,
        })
    } else {
        match *holding {
            _ if p.air == crate::player::AirMode::Wingsuit => Some(Context::Wingsuit),
            _ if p.air == crate::player::AirMode::Chute => Some(Context::Chute),
            // Into a long fall (and out of it only on landing).
            crate::traversal::Mount::None if !p.grounded && !p.swimming && !p.noclip && !p.driving && (p.velocity.y < -SKYDIVE_SPEED || (state.context == Some(Context::Skydive) && p.velocity.y < 0.0)) => Some(Context::Skydive),
            _ if repairing.vehicle.is_some() => Some(Context::Repair),
            crate::traversal::Mount::None if bino.held > 0.0 => Some(Context::Camera),
            crate::traversal::Mount::Ladder { .. } => Some(Context::Ladder),
            crate::traversal::Mount::Zip { .. } => Some(Context::Zip),
            _ if p.swimming && !p.driving => Some(if speed > 0.6 { Context::SwimMove } else { Context::Swim }),
            _ => None,
        }
    };
    if context != state.context {
        debug!("fp context {:?} -> {context:?} (active {})", state.context, state.active);
        state.context = context;
        let shown = |v: bool| if v { Visibility::Inherited } else { Visibility::Hidden };
        match (context, chars.as_ref()) {
            (Some(c), Some(chars)) => {
                let load = |rel: &str| -> Handle<Fc3Clip> { server.load(cache_path(&cache::anim_rel(&format!("{ANIM_ROOT}{rel}")))) };
                let mut clips = HashMap::from([(AnimKind::Idle, load(c.clip()))]);
                state.intro = 0.0;
                if let Some((intro, secs)) = c.intro() {
                    clips.insert(AnimKind::Shot, load(intro));
                    state.intro = secs;
                }
                r.set_clips(Arc::new(ClipSet { skeleton: chars.human.skeleton.clone(), clips }));
                state.throwing = 0.0;
            }
            _ => {
                if let Some(set) = state.weapon_set.clone() {
                    r.set_clips(set);
                }
                state.drawing = 0.0;
            }
        }
        // The arms show for a context or a weapon they hold; the weapon only outside contexts.
        let arms = context.is_some() || state.active;
        if let Ok(mut v) = mounts.get_mut(rig) {
            *v = shown(arms);
        }
        for e in state.mount.into_iter().chain(plain.iter()) {
            if let Ok(mut v) = mounts.get_mut(e) {
                *v = shown(context.is_none());
            }
        }
    }
    // A takedown or action: the arms play it through, then pick the weapon back up.
    if state.cinematic > 0.0 {
        state.cinematic -= time.delta_secs();
        let left = state.cinematic.max(0.0);
        busy.0 = left;
        cinematic.0 = if state.takedown.as_ref().is_some_and(|t| t.locked) { left } else { 0.0 };
        let rate = if state.takedown.as_ref().is_some_and(|t| t.victim.is_none()) { state.action_rate.max(0.1) } else { 1.0 };
        *req = AnimRequest { kind: AnimKind::Shot, rate };
        if state.cinematic <= 0.0 {
            state.action_rate = 1.0;
            if let Some(set) = state.weapon_set.clone() {
                r.set_clips(set);
            }
            state.drawing = 0.0;
            if let Some(mut v) = state.mount.and_then(|m| mounts.get_mut(m).ok()) {
                *v = Visibility::Inherited;
            }
            if let Ok(mut v) = mounts.get_mut(rig) {
                *v = if state.active || state.context.is_some() { Visibility::Inherited } else { Visibility::Hidden };
            }
            state.takedown = None;
        }
        return;
    }
    busy.0 = 0.0;
    if let Some(c) = context {
        // Its intro first.
        if state.intro > 0.0 {
            state.intro -= time.delta_secs();
            *req = AnimRequest { kind: AnimKind::Shot, rate: 1.0 };
            return;
        }
        let rate = match c {
            Context::SwimMove => (speed / 2.5).clamp(0.6, 1.5),
            Context::Ladder => (p.velocity.y.abs() / 2.0).clamp(0.0, 1.5),
            _ => 1.0,
        };
        *req = AnimRequest { kind: AnimKind::Idle, rate };
        return;
    }
    if !state.active {
        return;
    }
    // FC3_FP_GESTURE=grenade|rock (tests): throw over and over.
    if let Ok(g) = std::env::var("FC3_FP_GESTURE") {
        if state.throwing <= 0.0 && state.gesture.is_none() && state.drawing > 0.6 {
            state.gesture = Some(if g == "rock" { "rock" } else { "grenade" });
            info!("fp: throw ({g})");
        }
    }
    // Throws: the throw clip from that folder, the weapon out of sight, then a draw.
    if let (Some(gesture), Some(chars)) = (state.gesture.take(), chars.as_ref()) {
        // `folder` (its throw) or `folder|action`.
        let (folder, action) = gesture.split_once('|').unwrap_or((gesture, "throw"));
        if let Some(path) = state.index.get(folder).and_then(|c| c.get(action)).cloned() {
            let clip: Handle<Fc3Clip> = server.load(cache_path(&cache::anim_rel(&path)));
            let set = Arc::new(ClipSet { skeleton: chars.human.skeleton.clone(), clips: HashMap::from([(AnimKind::Shot, clip.clone()), (AnimKind::Idle, clip.clone())]) });
            r.set_clips(set);
            state.throwing = 0.75;
            // Props in hand: last throw's gone, this one's placed once it loads.
            for (e, _, _) in std::mem::take(&mut state.gesture_props) {
                commands.entity(e).despawn();
            }
            state.gesture_clip = Some(clip);
            state.gesture_action = action;
            state.gesture_time = 0.0;
            if let Some(m) = state.mount.and_then(|m| mounts.get_mut(m).ok()) {
                *m.into_inner() = Visibility::Hidden;
            }
        }
    }
    if state.throwing > 0.0 {
        // FC3_FP_GESTURE=hold (tests): stay in the throw.
        if std::env::var("FC3_FP_GESTURE").as_deref() != Ok("hold") {
            state.throwing -= time.delta_secs();
        }
        *req = AnimRequest { kind: AnimKind::Shot, rate: GESTURE_RATE };
        if state.throwing <= 0.0 {
            if let Some(set) = state.weapon_set.clone() {
                r.set_clips(set);
            }
            state.drawing = 0.0;
            if let Some(m) = state.mount.and_then(|m| mounts.get_mut(m).ok()) {
                *m.into_inner() = Visibility::Inherited;
            }
        }
        return;
    }
    let d = inv.def();
    let has = |k: AnimKind| r.clips().clips.contains_key(&k);
    state.drawing += time.delta_secs();
    // A new round fired: the shot clip starts over.
    let fired = inv.since_shot < state.last_since_shot;
    state.last_since_shot = inv.since_shot;
    if fired {
        state.shots += 1;
        restart.0 = state.shots;
    }
    // A reload just started: note how long it will take.
    if inv.reloading > state.last_reloading + 0.05 {
        state.reload_total = inv.reloading;
    }
    state.last_reloading = inv.reloading;
    let duration = |k: AnimKind| r.clips().clips.get(&k).and_then(|h| clips.get(h)).map(|c| c.0.duration).unwrap_or(1.0);
    let aiming = inv.aim_blend > 0.5;
    let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    // FC3_FP_POSE (tests): hold one action (idle|iron|shot|reload|draw|sprint|walk).
    if let Ok(pose) = std::env::var("FC3_FP_POSE") {
        let kind = match pose.as_str() {
            "iron" => AnimKind::IronIdle,
            "shot" => AnimKind::Shot,
            "reload" => AnimKind::Reload,
            "draw" => AnimKind::Draw,
            "sprint" => AnimKind::Sprint,
            "walk" => AnimKind::Walk,
            _ => AnimKind::Idle,
        };
        // FC3_FP_TIME: frozen at that time into the clip.
        match std::env::var("FC3_FP_TIME").ok().and_then(|t| t.parse::<f32>().ok()) {
            Some(t) if r.playing().0 == kind => {
                r.seek(t);
                *req = AnimRequest { kind, rate: 0.0 };
            }
            _ => *req = AnimRequest { kind, rate: 1.0 },
        }
        return;
    }
    *req = if state.drawing < 0.6 && has(AnimKind::Draw) {
        AnimRequest { kind: AnimKind::Draw, rate: 1.0 }
    } else if inv.reloading > 0.0 {
        let kind = if inv.slot().mag == 0 && has(AnimKind::ReloadEmpty) { AnimKind::ReloadEmpty } else { AnimKind::Reload };
        // The clip plays over exactly the reload's length.
        AnimRequest { kind, rate: (duration(kind) / state.reload_total.max(0.3)).clamp(0.5, 2.5) }
    } else if inv.since_shot < (d.interval * 1.5).max(0.25) {
        let melee = d.class == crate::weapons::WeaponClass::Melee;
        let iron = if melee { state.shots % 2 == 0 } else { aiming };
        AnimRequest { kind: if iron && has(AnimKind::IronShot) { AnimKind::IronShot } else { AnimKind::Shot }, rate: 1.0 }
    } else if aiming && has(AnimKind::IronIdle) {
        AnimRequest { kind: AnimKind::IronIdle, rate: 1.0 }
    } else if p.sliding.is_some() && has(AnimKind::Slide) {
        AnimRequest { kind: AnimKind::Slide, rate: 1.0 }
    } else if p.sprinting && speed > 3.0 && has(AnimKind::Sprint) {
        AnimRequest { kind: AnimKind::Sprint, rate: (speed / 6.5).clamp(0.7, 1.4) }
    } else if speed > 0.8 && p.grounded && has(AnimKind::Walk) {
        AnimRequest { kind: AnimKind::Walk, rate: (speed / 3.0).clamp(0.6, 1.5) }
    } else {
        AnimRequest { kind: AnimKind::Idle, rate: 1.0 }
    };
}

/// The wingsuit's membrane on the arms while flying it: its own little rig on
/// `Spine2`, playing the membrane's tracks from the arms' clips (unfolding
/// with the deploy, then the glide); gone again after.
#[allow(clippy::too_many_arguments)]
fn wingsuit_membrane(
    mut state: ResMut<FpState>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    mut clips: ResMut<Assets<Fc3Clip>>,
    rigs: Query<&CharacterRig>,
    mut anims: Query<&mut AnimRequest>,
    mut commands: Commands,
) {
    // The torch in hand while repairing.
    match (state.context == Some(Context::Repair), state.torch) {
        (true, None) => {
            let hand = state.rig.and_then(|r| rigs.get(r).ok()).and_then(|r| r.joint(name_hash("RightHand"))).map(|(_, j)| j);
            if let (Some(hand), Some(model)) = (hand, state.props.get("welding_torch").and_then(|h| models.get(h))) {
                state.torch = Some(spawn_prop(model, hand, &mut commands));
            }
        }
        (false, Some(e)) => {
            commands.entity(e).despawn();
            state.torch = None;
        }
        _ => {}
    }
    // The camera in hand while tagging.
    match (state.context == Some(Context::Camera), state.camera) {
        (true, None) => {
            let hand = state.rig.and_then(|r| rigs.get(r).ok()).and_then(|r| r.joint(name_hash("LeftHand"))).map(|(_, j)| j);
            if let (Some(hand), Some(model)) = (hand, state.props.get("monocular").and_then(|h| models.get(h))) {
                state.camera = Some(spawn_prop(model, hand, &mut commands));
            }
        }
        (false, Some(e)) => {
            commands.entity(e).despawn();
            state.camera = None;
        }
        _ => {}
    }
    if state.context != Some(Context::Wingsuit) {
        if let Some(e) = state.membrane.take() {
            commands.entity(e).despawn();
        }
        return;
    }
    let (model, skeleton) = state
        .membrane_assets
        .get_or_insert_with(|| {
            let model = server.load_builder().with_settings(|m: &mut crate::cache_assets::ModelSettings| m.skinned = true).load(crate::cache_assets::skinned_cache_path(&cache::model_rel(WINGSUIT_MODEL)));
            (model, server.load(cache_path(&cache::skeleton_rel(WINGSUIT_SKELETON))))
        })
        .clone();
    // The membrane's tracks out of the deploy and glide clips, as clips of
    // their own.
    if state.membrane_clips.is_none() {
        let tracks = |rel: &str| -> Option<fc3_formats::anim::Clip> {
            let h: Handle<Fc3Clip> = server.load(cache_path(&cache::anim_rel(&format!("{ANIM_ROOT}{rel}"))));
            clips.get(&h).and_then(|c| c.0.sub_clips.iter().find(|s| s.skeleton_id == WINGSUIT_PROP).cloned())
        };
        let deploy = Context::Wingsuit.intro().and_then(|(rel, _)| tracks(rel));
        let (Some(deploy), Some(glide)) = (deploy, tracks(Context::Wingsuit.clip())) else { return };
        state.membrane_clips = Some((clips.add(Fc3Clip(Arc::new(deploy))), clips.add(Fc3Clip(Arc::new(glide)))));
    }
    if state.membrane.is_none() {
        let spine = state.rig.and_then(|r| rigs.get(r).ok()).and_then(|r| r.joint(name_hash("Spine2"))).map(|(_, j)| j);
        let (Some(spine), Some(m), Some((deploy, glide))) = (spine, models.get(&model), state.membrane_clips.clone()) else { return };
        let set = Arc::new(ClipSet { skeleton, clips: HashMap::from([(AnimKind::Shot, deploy), (AnimKind::Idle, glide)]) });
        state.membrane = crate::characters::spawn_rig_with(&mut commands, spine, &[m], None, set, Vec3::ZERO, None);
        if let Some(e) = state.membrane {
            commands.entity(e).insert(NotShadowCaster);
        }
    }
    // In step with the arms: the deploy while it plays, then the glide.
    if let Some(mut req) = state.membrane.and_then(|e| anims.get_mut(e).ok()) {
        *req = AnimRequest { kind: if state.intro > 0.0 { AnimKind::Shot } else { AnimKind::Idle }, rate: 1.0 };
    }
}

/// Puts the camera on the rig's `Camera` bone (the rig moves, not the view)
/// and the weapon at the clip's hand offset.
fn align_arms(
    state: Res<FpState>,
    clips: Res<Assets<Fc3Clip>>,
    rigs: Query<&CharacterRig>,
    mut transforms: Query<&mut Transform>,
) {
    let (Some(rig_e), Some(mount)) = (state.rig, state.mount) else { return };
    let Ok(rig) = rigs.get(rig_e) else { return };
    let Some(m) = camera_bone(rig, |e| transforms.get(e).ok().copied()) else { return };
    if let Ok(mut root) = transforms.get_mut(rig_e) {
        // On a mounted gun the arms and gun sit further out, so the hands on
        // its grips come into view.
        let shift = if state.mounted.is_some() && state.takedown.is_none() { MOUNT_VIEW } else { Vec3::ZERO };
        *root = Transform::from_matrix(Mat4::from_translation(shift) * m.inverse());
    }
    // The weapon's root relative to the hand, from the clip's weapon track.
    let (kind, time) = rig.playing();
    let Some(clip) = rig.clips().clips.get(&kind).or_else(|| rig.clips().clips.get(&AnimKind::Idle)).and_then(|h| clips.get(h)) else { return };
    // Takedown clips carry every prop they might use; the machete is ours.
    let machete = state.takedown.as_ref().and_then(|_| clip.0.sub_clips.iter().find(|c| c.skeleton_id == MACHETE_PROP));
    let t = crate::characters::clip_time(kind, time, clip.0.duration);
    // A prop's root relative to its joint, from its track.
    let pose = |prop: &fc3_formats::anim::Clip| -> Transform {
        let track = prop.tracks.iter().min_by_key(|t| t.bone);
        let rotation = track.and_then(|tr| tr.sample_rotation(t)).map(Quat::from_array).unwrap_or(Quat::from_array(prop.reference_rotation));
        let translation = track.and_then(|tr| tr.sample_translation(t)).map(Vec3::from_array).unwrap_or(Vec3::from_array(prop.reference_translation));
        Transform { translation, rotation, ..default() }
    };
    for (e, sub) in state.action_props.iter().copied().chain(state.gesture_props.iter().map(|(e, s, _)| (*e, *s))) {
        if let (Some(prop), Ok(mut pt)) = (clip.0.sub_clips.get(sub), transforms.get_mut(e)) {
            *pt = pose(prop);
        }
    }
    // The tagging camera, by its track (the held pose keeps where the raise
    // left it).
    if let (Some(e), Some(prop)) = (state.camera, clip.0.sub_clips.iter().find(|c| c.skeleton_id == CAMERA_PROP && c.tracks.len() == 1)) {
        if let Ok(mut pt) = transforms.get_mut(e) {
            *pt = pose(prop);
        }
    }
    // The welding torch, by its track (the hand's; the other is the left
    // hand's grip on it).
    if let (Some(e), Some(prop)) = (state.torch, clip.0.sub_clips.iter().find(|c| c.skeleton_id == TORCH_PROP && c.tracks.len() > 1)) {
        if let Ok(mut pt) = transforms.get_mut(e) {
            *pt = pose(prop);
        }
    }
    // The pistol takedown's pistol, by its own track.
    let pistol = state.takedown.as_ref().and_then(|_| clip.0.sub_clips.iter().find(|c| c.skeleton_id == PISTOL_PROP));
    if let (Some(e), Some(prop)) = (state.takedown_pistol, pistol) {
        if let Ok(mut pt) = transforms.get_mut(e) {
            *pt = pose(prop);
        }
    }
    // Its shots and drop carry no machete: put ours out of sight.
    if machete.is_none() && pistol.is_some() {
        if let Ok(mut mt) = transforms.get_mut(mount) {
            mt.scale = Vec3::ZERO;
        }
        return;
    }
    // A mounted gun stays put before the view while the hands work it.
    if let Some(hold) = state.mounted.filter(|_| state.takedown.is_none()) {
        if let Some(hand) = bone_matrix(rig, "RightHand", |e| transforms.get(e).ok().copied()) {
            if let Ok(mut mt) = transforms.get_mut(mount) {
                *mt = Transform::from_matrix(hand.inverse() * m * Mat4::from_translation(hold));
            }
        }
        return;
    }
    let Some(prop) = machete.or_else(|| clip.0.main_prop_clip()) else { return };
    let p = pose(prop);
    if let Ok(mut mt) = transforms.get_mut(mount) {
        mt.translation = p.translation;
        mt.rotation = p.rotation;
        mt.scale = Vec3::ONE;
    }
}
