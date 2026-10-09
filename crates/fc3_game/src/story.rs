//! The game's story missions and side activities in the world.
//!
//! The story itself (the game's own Domino graphs and the mission system
//! around them) is `fc3_story`; this module hosts it: it builds what the
//! scripts may see of the world each frame (the player, who is where) and
//! applies what they ask for: mission layers in the streamed world,
//! objectives, voiced lines with subtitles, mission titles and results,
//! teleports. Cinematics play in `cinema`.
//!
//! A new game plays the story unless the settings say free roam
//! (`FC3_STORY=1` / `0` overrides); a continued game plays it when the save
//! has the story's progress next to it. FC3_DOMINO_TRACE logs the
//! natives scripts reach that aren't provided, and FC3_STORY_TRACE=<path
//! part> logs the calls into matching graphs.

use std::collections::HashMap;

use bevy::prelude::*;
use fc3_story::{Arg, Command, LayerState, Story, View};

use crate::data::{to_bevy, to_dunia, CacheInfo};
use crate::menu::GameState;
use crate::mission_layers::MissionLayers;
use crate::player::Player;

pub struct StoryPlugin;

impl Plugin for StoryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StoryUi>()
            .init_resource::<ScriptedPlayer>()
            .init_resource::<MissionArea>()
            .init_resource::<ObjectiveMarkers>()
            .init_resource::<HiddenEntities>()
            .init_resource::<EntityPlacements>()
            .init_resource::<StoryPurchases>()
            .init_resource::<StoryLoot>()
            .init_resource::<StoryCrafts>()
            .init_resource::<StoryMenuEvents>()
            .add_systems(Update, hide_entities.after(StorySet))
            .add_systems(Update, apply_entity_poses.after(StorySet).in_set(EntityPosesSet))
            .add_systems(Update, apply_entity_physics.after(StorySet).in_set(EntityPosesSet))
            .add_systems(Update, draw_markers.after(StorySet))
            .add_systems(PreUpdate, story_boxes.after(crate::controls::update_actions))
            .add_systems(PreUpdate, story_interactions.after(story_boxes))
            .add_systems(PreUpdate, autopilot_loot.after(story_interactions).run_if(in_state(GameState::Playing)))
            .add_systems(PreUpdate, collect_shop_purchases)
            .add_systems(PreUpdate, collect_loot)
            .add_systems(PreUpdate, collect_crafts)
            .add_systems(PreUpdate, collect_menu_events)
            .add_systems(Startup, setup_story_ui)
            .add_systems(Update, fade_screen)
            .add_systems(Update, start_story.after(crate::save::continue_game).run_if(in_state(GameState::Playing)))
            .add_systems(Update, run_story.in_set(StorySet).run_if(|state: Res<State<GameState>>| {
                matches!(state.get(), GameState::Playing | GameState::Paused | GameState::Map | GameState::Journal | GameState::Shop)
            }))
            .add_systems(Update, show_story_ui);
    }
}

/// The story's frame; systems acting on what it asked run after it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorySet;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityPosesSet;

/// Scripted placements survive streaming, so a character teleported before
/// its models load spawns at the destination rather than its editor position.
#[derive(Resource, Default)]
pub struct EntityPlacements {
    poses: HashMap<u64, (u64, Transform)>,
    revision: u64,
    physics: HashMap<u64, (u64, bool)>,
}

impl EntityPlacements {
    pub fn pose(&self, id: u64) -> Option<Transform> {
        self.poses.get(&id).map(|(_, t)| *t)
    }

    fn set(&mut self, id: u64, pose: fc3_story::Pose) {
        self.revision += 1;
        let transform = Transform {
            translation: to_bevy(pose.position),
            rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(pose.angles)),
            scale: Vec3::ONE,
        };
        self.poses.insert(id, (self.revision, transform));
    }

    fn reset(&mut self, data: &fc3_story::data::StoryData) {
        let ids: Vec<u64> = self.poses.keys().copied().collect();
        for id in ids {
            if let Some(e) = data.entities.get(&id) {
                self.set(id, fc3_story::Pose { position: e.pos, angles: e.angles });
            }
        }
        let ids: Vec<u64> = self.physics.keys().copied().collect();
        for id in ids {
            self.set_physics(id, true);
        }
    }

    fn set_physics(&mut self, id: u64, enabled: bool) {
        self.revision += 1;
        self.physics.insert(id, (self.revision, enabled));
    }
}

#[derive(Component)]
struct AppliedPose(u64);

#[derive(Component)]
struct AppliedPhysics(u64);

#[allow(clippy::type_complexity)]
fn apply_entity_physics(
    placements: Res<EntityPlacements>,
    roots: Query<(Entity, Option<&crate::objects::PlacedId>, Option<&crate::story_actors::StoryActor>, Option<&AppliedPhysics>),
        Or<(With<crate::objects::PlacedId>, With<crate::story_actors::StoryActor>)>>,
    mut commands: Commands,
) {
    if placements.physics.is_empty() {
        return;
    }
    for (root, prop, actor, applied) in &roots {
        let Some(id) = prop.map(|p| p.0).or_else(|| actor.map(|a| a.id)) else { continue };
        let Some(&(revision, enabled)) = placements.physics.get(&id) else { continue };
        if applied.is_some_and(|a| a.0 == revision) {
            continue;
        }
        if enabled {
            commands.entity(root).remove::<avian3d::prelude::ColliderDisabled>();
        } else {
            commands.entity(root).insert(avian3d::prelude::ColliderDisabled);
        }
        commands.entity(root).insert(AppliedPhysics(revision));
    }
}

/// Applies each teleport once to every visual root of the entity, including
/// roots that stream in later. Props retain their model's scale.
#[allow(clippy::type_complexity)]
fn apply_entity_poses(
    poses: Res<EntityPlacements>,
    nav: Res<crate::nav::NavMesh>,
    mut actors: Query<(Entity, &mut crate::story_actors::StoryActor, &mut Transform, Option<&AppliedPose>)>,
    mut props: Query<(Entity, Ref<crate::objects::PlacedId>, &mut Transform, Option<&AppliedPose>, Option<&ChildOf>), Without<crate::story_actors::StoryActor>>,
    parents: Query<&GlobalTransform>,
    mut commands: Commands,
) {
    if poses.poses.is_empty() {
        return;
    }
    for (entity, mut actor, mut at, applied) in &mut actors {
        let Some(&(revision, mut pose)) = poses.poses.get(&actor.id) else { continue };
        if !actor.is_added() && applied.is_some_and(|a| a.0 == revision) {
            continue;
        }
        pose.translation += Vec3::Y * crate::player::HALF_HEIGHT;
        pose.scale = at.scale;
        *at = pose;
        actor.relocate(pose.translation, &nav);
        commands.entity(entity).insert(AppliedPose(revision));
    }
    for (entity, id, mut at, applied, parent) in &mut props {
        let Some(&(revision, mut pose)) = poses.poses.get(&id.0) else { continue };
        if !id.is_added() && applied.is_some_and(|a| a.0 == revision) {
            continue;
        }
        if let Some(parent) = parent.and_then(|p| parents.get(p.parent()).ok()) {
            pose = Transform::from_matrix(parent.to_matrix().inverse() * pose.to_matrix());
        }
        pose.scale = at.scale;
        *at = pose;
        commands.entity(entity).insert(AppliedPose(revision));
    }
}

/// Purchases made while mission simulation is paused (the shop).
#[derive(Resource, Default)]
struct StoryPurchases(Vec<crate::progression::ItemBought>);

/// Bevy messages expire after two frames, even while virtual time is paused.
/// Keep shop events until the mission can consume them with play resumed.
fn collect_shop_purchases(
    host: Option<NonSend<StoryHost>>,
    mut bought: MessageReader<crate::progression::ItemBought>,
    mut pending: ResMut<StoryPurchases>,
) {
    if host.is_some() {
        pending.0.extend(bought.read().cloned());
    } else {
        bought.clear();
        pending.0.clear();
    }
}

/// Gathering may open a tutorial that pauses mission simulation; keep its
/// event until the story can read the updated inventory on a playing frame.
#[derive(Resource, Default)]
struct StoryLoot(Vec<crate::progression::ItemLooted>);

fn collect_loot(
    host: Option<NonSend<StoryHost>>,
    mut looted: MessageReader<crate::progression::ItemLooted>,
    mut pending: ResMut<StoryLoot>,
) {
    if host.is_some() {
        pending.0.extend(looted.read().copied());
    } else {
        looted.clear();
        pending.0.clear();
    }
}

/// The running story (its Lua state lives on the main thread).
#[derive(Resource, Default)]
struct StoryCrafts(Vec<crate::crafting::ItemCrafted>);

fn collect_crafts(host: Option<NonSend<StoryHost>>, mut crafted: MessageReader<crate::crafting::ItemCrafted>, mut pending: ResMut<StoryCrafts>) {
    if host.is_some() { pending.0.extend(crafted.read().cloned()); }
    else { crafted.clear(); pending.0.clear(); }
}

/// Menu callbacks outlive message expiry and are delivered after the live
/// inventory snapshot and any committed crafting callbacks, even while paused.
#[derive(Resource, Default)]
struct StoryMenuEvents(Vec<&'static str>);

fn collect_menu_events(
    host: Option<NonSend<StoryHost>>, state: Res<State<GameState>>, menu: Res<crate::progression::PauseMenu>,
    mut previous: Local<Option<(GameState, crate::progression::JournalPage)>>, mut events: ResMut<StoryMenuEvents>,
) {
    use crate::progression::JournalPage;
    let current = (*state.get(), menu.page);
    let old = previous.replace(current);
    if host.is_none() { events.0.clear(); return; }
    let Some((old_state, old_page)) = old else { return; };
    if current == (old_state, old_page) { return; }
    if old_state != current.0 {
        match current.0 {
            GameState::Map => events.0.push("CDominoWorldMapOpenedEvent"),
            GameState::Paused | GameState::Journal if !matches!(old_state, GameState::Paused | GameState::Journal) => {
                events.0.push("CDominoOpenPauseMenuEvent");
            }
            GameState::Playing if matches!(old_state, GameState::Paused | GameState::Journal) && menu.can_exit() => {
                events.0.push("CDominoExitPauseMenuEvent");
            }
            _ => {}
        }
    }
    if current.0 == GameState::Journal && (old_state != GameState::Journal || old_page != current.1) {
        events.0.push(match current.1 {
            JournalPage::Crafting => "CDominoCraftingPageOpenedEvent",
            JournalPage::Skills => "CDominoSkillPageOpenedEvent",
        });
    }
}

/// The running story (its Lua state lives on the main thread).
pub struct StoryHost {
    pub story: Story,
    /// Host work due later: (time, what).
    due: Vec<(f32, Due)>,
    /// The last block that completed, and whether it was a cinematic one
    /// (its end spawn is where the cinematic left the player).
    last_completed: Option<String>,
    /// Tests (`FC3_STORY_AUTOPILOT`): seconds since anything happened, and
    /// the triggers already walked into.
    quiet: f32,
    used: std::collections::HashSet<(u64, String)>,
    /// The last checkpoint's progress, and when a reload to it is due.
    checkpoint: Option<fc3_story::save::StorySave>,
    reload_at: Option<f32>,
    was_dead: bool,
    /// Put the player at the story's respawn point (a test load).
    to_respawn: bool,
    /// Checkpoints kept for tests.
    kept: u32,
    /// A smoke checkpoint's matching live inventory/world snapshot, captured
    /// after host commands have settled. Never written to a real save slot.
    pub(crate) checkpoint_game_paths: Vec<std::path::PathBuf>,
    /// The shop's weapons as scripts know them: model path -> (archetype,
    /// shop name id).
    shop_items: HashMap<String, (String, u32)>,
    /// The character last sent on an action (tests: the one to follow).
    last_ordered: Option<u64>,
    /// The hidden entities were set from the story's removed ones.
    hidden_synced: bool,
}

impl StoryHost {
    fn forward_menu_events(&mut self, events: &mut StoryMenuEvents) {
        for event in events.0.drain(..) { self.story.generic_event(event, vec![]); }
    }

    fn forward_crafts(&mut self, crafts: &mut StoryCrafts) {
        for crafted in crafts.0.drain(..) {
            self.story.entity_event(fc3_story::PLAYER, "DominoEvent_CraftRecipe",
                vec![Arg::Num(crafted.kind as f64), Arg::Str(crafted.label)]);
        }
    }
    /// Called after updating the story's view, so loot callbacks querying
    /// quantities see the items just collected, not the preceding frame.
    fn forward_loot(&mut self, loot: &mut StoryLoot, catalog: &crate::progression::LootCatalog) {
        for looted in loot.0.drain(..) {
            if let Some(item) = catalog.item(looted.0) {
                info!("story loot: {} ({})", item.name, item.name_id);
                self.story.entity_event(fc3_story::PLAYER, "CPawnLootItemEvent",
                    vec![Arg::Str(item.key.clone()), Arg::Str("Name".into()), Arg::Str(item.name_id.to_string())]);
            }
        }
    }

    fn forward_purchases(&mut self, purchases: &mut StoryPurchases) {
        for b in purchases.0.drain(..) {
            if let Some((archetype, id)) = b.model.as_ref().and_then(|m| self.shop_items.get(m)) {
                self.story.entity_event(fc3_story::PLAYER, "CPawnBuyItemEvent",
                    vec![Arg::Str(archetype.clone()), Arg::Str("Name".into()), Arg::Str(id.to_string())]);
            }
        }
    }

    pub(crate) fn new(story: Story, checkpoint: Option<fc3_story::save::StorySave>) -> StoryHost {
        StoryHost {
            story,
            due: Vec::new(),
            last_completed: None,
            quiet: 0.0,
            used: Default::default(),
            checkpoint,
            reload_at: None,
            was_dead: false,
            to_respawn: false,
            kept: 0,
            checkpoint_game_paths: Vec::new(),
            shop_items: HashMap::new(),
            last_ordered: None,
            hidden_synced: false,
        }
    }
}

enum Due {
    Sound(u64),
}

/// What the story shows on screen.
#[derive(Resource, Default)]
pub struct StoryUi {
    pub objective: Option<String>,
    /// A subtitle and how long it stays.
    pub subtitle: Option<(String, f32)>,
    /// A large centre banner (mission title, complete, failed).
    pub banner: Option<(String, f32)>,
    /// Hint texts by id (tutorial boxes).
    pub hints: HashMap<u64, String>,
    /// Authored checklist rows, updated and hidden individually by missions.
    pub checklist: std::collections::BTreeMap<u32, String>,
    /// Message and tutorial boxes waiting, the first one shown.
    pub boxes: std::collections::VecDeque<StoryBox>,
    /// A phone call: who, incoming, and how long it has gone on.
    pub phone: Option<(String, bool, f32)>,
    pub timers: Vec<fc3_story::Timer>,
}

/// A box the game stops for until the player closes it.
pub struct StoryBox {
    pub id: u64,
    pub title: String,
    pub body: String,
    /// Accept or cancel (else just closed).
    pub choice: bool,
    /// A localized footer supplied by a queued tutorial.
    pub prompt: Option<String>,
}

/// What missions set on the player: signals ignored (jumping, sprinting
/// through a scene), a movement scale, no damage, no HUD, and the screen
/// fading to black.
#[derive(Resource)]
pub struct ScriptedPlayer {
    pub ignored: std::collections::HashSet<String>,
    pub movement: f32,
    pub god: bool,
    pub hud_hidden: bool,
    /// Black over the screen: (now, target, seconds to get there).
    pub fade: (f32, f32, f32),
    /// Seconds the screen has been held black (a scene lifts it; this is
    /// the fallback).
    pub black_for: f32,
    /// The inventory a mission put aside (`BackupInventory`).
    backup: Option<InventoryBackup>,
    /// Non-weapon inventory entries missions give and take (rock, camera).
    pub gadgets: std::collections::HashSet<String>,
    pub animal_looting: bool,
    pub plant_looting: bool,
}

struct InventoryBackup {
    slots: Vec<crate::weapons::WeaponSlot>,
    current: usize,
    explosives: [u32; 4],
    gadgets: std::collections::HashSet<String>,
}

impl Default for ScriptedPlayer {
    fn default() -> Self {
        ScriptedPlayer {
            ignored: Default::default(), movement: 1.0, god: false, hud_hidden: false,
            fade: (0.0, 0.0, 0.0), black_for: 0.0, backup: None,
            gadgets: ["gadgets:FC3/Rock".into(), "gadgets:Equipped/Tablet".into()].into_iter().collect(),
            animal_looting: true, plant_looting: true,
        }
    }
}

/// The world sectors of the mission zones being played: the world's own
/// enemies and outposts keep out of them while a mission runs there.
#[derive(Resource, Default)]
pub struct MissionArea {
    pub sectors: std::collections::HashSet<u32>,
}

impl MissionArea {
    /// Whether a point (Bevy space) is in a mission zone being played.
    pub fn contains(&self, at: Vec3) -> bool {
        if self.sectors.is_empty() {
            return false;
        }
        let d = to_dunia(at);
        self.sectors.contains(&fc3_formats::terrain::SectorCoord::containing(d[0], d[1]).id())
    }
}

/// Story entities scripts have hidden or removed: their placed objects and
/// characters aren't shown.
#[derive(Resource, Default)]
pub struct HiddenEntities(pub std::collections::HashSet<u64>);

/// Shows and hides placed objects and characters as the scripts ask (when
/// that changes, and as they come into the world).
#[allow(clippy::type_complexity)]
fn hide_entities(
    hidden: Res<HiddenEntities>,
    mut props: Query<(Ref<crate::objects::PlacedId>, &mut Visibility), Without<crate::story_actors::StoryActor>>,
    mut actors: Query<(Ref<crate::story_actors::StoryActor>, &mut Visibility)>,
) {
    let all = hidden.is_changed();
    for (id, mut vis) in &mut props {
        if all || id.is_added() {
            let want = if hidden.0.contains(&id.0) { Visibility::Hidden } else { Visibility::Inherited };
            vis.set_if_neq(want);
        }
    }
    for (a, mut vis) in &mut actors {
        if all || a.is_added() {
            let want = if hidden.0.contains(&a.id) { Visibility::Hidden } else { Visibility::Visible };
            vis.set_if_neq(want);
        }
    }
}

/// The mission's objective markers (Bevy space), shown on screen.
#[derive(Resource, Default)]
pub struct ObjectiveMarkers(pub Vec<Vec3>);

#[derive(Component)]
struct MarkerIcon;

/// Objective markers: a diamond and the distance, held at the screen's edge
/// when off screen; none while a scene holds the view or the HUD is off.
fn draw_markers(
    markers: Res<ObjectiveMarkers>,
    scripted: Res<ScriptedPlayer>,
    cinema: Res<crate::cinema::Cinema>,
    cameras: Query<(&Camera, &GlobalTransform), With<crate::player::PlayerCamera>>,
    icons: Query<Entity, With<MarkerIcon>>,
    mut commands: Commands,
) {
    for e in &icons {
        commands.entity(e).despawn();
    }
    if markers.0.is_empty() || scripted.hud_hidden || cinema.view.is_some() {
        if !markers.0.is_empty() {
            trace!("markers hidden: hud {} scene view {}", scripted.hud_hidden, cinema.view.is_some());
        }
        return;
    }
    let Ok((cam, cam_t)) = cameras.single() else { return };
    let Some(size) = cam.logical_viewport_size() else { return };
    // Off screen markers sit on a frame inside the compass and the HUD.
    let (lo, hi) = (Vec2::new(40.0, 90.0), size - Vec2::new(40.0, 150.0));
    let mid = (lo + hi) * 0.5;
    let view = cam_t.affine().inverse();
    for &at in &markers.0 {
        let to = at - cam_t.translation();
        let local = view.transform_point3(at);
        let seen = cam.world_to_viewport(cam_t, at).ok().filter(|p| local.z < 0.0 && p.cmpge(lo).all() && p.cmple(hi).all());
        let px = seen.unwrap_or_else(|| {
            // Toward it from the middle, out to the frame (behind: its side).
            let mut d = Vec2::new(local.x, -local.y);
            if local.z > 0.0 {
                d.y = d.y.max(0.0);
            }
            if d.length_squared() < 1e-6 {
                d = Vec2::Y;
            }
            let half = (hi - lo) * 0.5;
            let k = (half.x / d.x.abs().max(1e-6)).min(half.y / d.y.abs().max(1e-6));
            mid + d * k
        });
        let metres = to.length();
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(px.x - 8.0),
                top: Val::Px(px.y - 8.0),
                width: Val::Px(16.0),
                height: Val::Px(16.0),
                ..default()
            },
            BackgroundColor(Color::srgba(1.0, 0.82, 0.25, 0.9)),
            UiTransform::from_rotation(Rot2::degrees(45.0)),
            MarkerIcon,
            crate::hud::HudElement,
        ));
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(px.x - 30.0),
                top: Val::Px(px.y + 12.0),
                width: Val::Px(60.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
            children![(
                Text::new(format!("{metres:.0}m")),
                TextFont { font_size: bevy::text::FontSize::Px(14.0), ..default() },
                TextColor(Color::srgba(1.0, 0.9, 0.6, 0.9))
            )],
            MarkerIcon,
            crate::hud::HudElement,
        ));
    }
}

/// A weapon by a script's archetype name (`weapons:FC3/machete`): the
/// game's weapon whose name or model folder it is.
fn weapon_def(inv: &crate::weapons::Inventory, archetype: &str) -> Option<usize> {
    let short = archetype.rsplit(['/', ':']).next().unwrap_or(archetype).to_ascii_lowercase();
    inv.defs.iter().position(|d| d.name.to_ascii_lowercase() == short || d.model.split('\\').next().is_some_and(|folder| folder.eq_ignore_ascii_case(&short)))
}

impl ScriptedPlayer {
    pub fn has_gadget(&self, archetype: &str) -> bool {
        self.gadgets.iter().any(|g| g.eq_ignore_ascii_case(archetype))
    }

    pub fn has_camera(&self) -> bool {
        self.gadgets.iter().any(|g| camera_gadget(g))
    }

    fn backup_inventory(&mut self, inv: &crate::weapons::Inventory) {
        self.backup = Some(InventoryBackup {
            slots: inv.slots.clone(), current: inv.current,
            explosives: [inv.grenades, inv.molotovs, inv.c4, inv.mines],
            gadgets: self.gadgets.clone(),
        });
    }

    fn restore_inventory(&mut self, inv: &mut crate::weapons::Inventory) {
        let Some(backup) = self.backup.take() else { return };
        inv.slots = backup.slots;
        inv.current = backup.current;
        (inv.grenades, inv.molotovs, inv.c4, inv.mines) = (
            backup.explosives[0], backup.explosives[1], backup.explosives[2], backup.explosives[3],
        );
        self.gadgets = backup.gadgets;
        crate::weapons::settle_unarmed(inv);
    }
}

fn camera_gadget(archetype: &str) -> bool {
    ["gadgets:Equipped/Tablet", "gadgets:Equipped/Binoculars"].iter().any(|g| archetype.eq_ignore_ascii_case(g))
}

/// Inventory entries as the original scripts name them. Empty hands are not
/// a weapon, and the currently held entry uses the same name as the list.
fn story_weapons(
    defs: &[crate::weapons::WeaponDef],
    slots: &[crate::weapons::WeaponSlot],
    current: usize,
    shop: &HashMap<String, (String, u32)>,
) -> (Vec<fc3_story::Item>, Option<String>) {
    use crate::weapons::{WeaponClass as W, UNARMED};
    let mut items = Vec::new();
    let mut held = None;
    for (i, slot) in slots.iter().enumerate() {
        let Some(def) = defs.get(slot.def).filter(|d| d.name != UNARMED) else { continue };
        let name = shop.get(&def.model_path()).map(|(a, _)| a.clone()).unwrap_or_else(|| {
            format!("weapons:FC3/{}", def.model.split('\\').next().unwrap_or(def.name))
        });
        let category = match def.class {
            W::Melee => "melee", W::Pistol => "handgun", W::Smg => "smg",
            W::Rifle => "assault", W::Shotgun => "shotgun", W::Sniper => "sniper",
            W::Lmg | W::Mounted => "lmg", W::Launcher => "rpg", W::Bow => "bow",
            W::Flamethrower => "flamethrower", W::GrenadeLauncher => "grenadelauncher",
        };
        if i == current {
            held = Some(name.clone());
        }
        items.push(fc3_story::Item { name, class: category.into(), category: category.into(), count: 1 });
    }
    (items, held)
}

/// The story's texts carry the game's UI markup (`<br>`, `<font>`, images).
pub fn plain(text: &str) -> String {
    let text = text.replace("<br>", "\n").replace("<BR>", "\n");
    let mut out = String::new();
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Replaces action placeholders with the current device's bound controls;
/// unfamiliar bracketed text (including subtitles) is kept verbatim.
fn prompt_text(text: &str, actions: &crate::controls::ActionState, settings: &crate::settings::Settings) -> String {
    let mut rest = text;
    let mut out = String::new();
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let Some(close) = rest.find(']') else { break };
        if let Some(action) = crate::controls::Action::from_prompt(&rest[1..close]) {
            out.push_str(&format!("[{}]", actions.label(action, settings)));
        } else if rest[1..close].eq_ignore_ascii_case("MapFastTravel") {
            out.push_str(if actions.gamepad_active { "[A]" } else { "[LMB]" });
        } else {
            out.push_str(&rest[..=close]);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

fn timer_text(timer: &fc3_story::Timer) -> String {
    if !timer.append_time && !timer.label.is_empty() {
        return plain(&timer.label);
    }
    let value = timer.seconds.max(0.0);
    let seconds = if timer.increasing { value.floor() } else { value.ceil() } as u64;
    let time = if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    };
    let label = plain(&timer.label);
    if label.is_empty() { time } else { format!("{label}  {time}") }
}

/// Opens the story from the cache, from a save when given one.
fn open_story(root: &std::path::Path, save: Option<&fc3_story::save::StorySave>) -> Option<Story> {
    let mut story = match Story::open(root) {
        Ok(s) => s,
        Err(e) => {
            warn!("the story could not start: {e:#} (convert the game again to include it)");
            return None;
        }
    };
    if let Ok(p) = std::env::var("FC3_STORY_TRACE") {
        let _ = story.domino().trace_calls(&p);
    }
    match save {
        Some(s) => story.boot_saved(s),
        None => story.boot(),
    }
    Some(story)
}

/// Starts the story with the game (once, when asked for): from its save
/// when the session continues one.
fn start_story(world: &mut World, mut tried: Local<bool>) {
    if *tried {
        return;
    }
    *tried = true;
    let wanted = match std::env::var("FC3_STORY").as_deref() {
        Ok("0") | Ok("false") => false,
        Ok(_) => true,
        // (Tests at a place, or of something else, go without.)
        Err(_) => {
            world.get_resource::<crate::settings::Settings>().is_none_or(|s| s.story)
                && world.get_resource::<crate::Args>().is_none_or(|a| !a.placed && !a.scripted_test())
        }
    };
    if !wanted {
        return;
    }
    let Some(root) = world.get_resource::<CacheInfo>().map(|c| c.root.clone()) else { return };
    let continued = world.get_resource::<crate::save::SaveContinued>().is_some_and(|c| c.0);
    // FC3_STORY_LOAD=<file> (tests): from a kept checkpoint (see
    // FC3_STORY_CHECKPOINTS), at its respawn point.
    let test_load = std::env::var("FC3_STORY_LOAD").ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| fc3_story::save::StorySave::from_text(&t));
    let to_respawn = test_load.is_some();
    let saved = test_load.or_else(|| {
        world
            .get_resource::<crate::save::SaveSlot>()
            .and_then(|s| s.story_path())
            .filter(|_| continued)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| fc3_story::save::StorySave::from_text(&t))
    });
    // A save from free roam goes on without the story.
    if continued && saved.is_none() && !to_respawn {
        info!("story: the save has no story progress; free roam");
        return;
    }
    let started = std::time::Instant::now();
    let Some(story) = open_story(&root, saved.as_ref()) else { return };
    if let Some(mut menu) = world.get_resource_mut::<crate::progression::PauseMenu>() {
        let previous = std::mem::replace(&mut *menu, crate::progression::PauseMenu::from_story(&story));
        if continued {
            menu.tutorial = previous.tutorial;
            menu.tutorial_completed = previous.tutorial_completed;
        }
    }
    if saved.is_none() && !continued {
        // The campaign begins empty-handed; the free-roam starter medical
        // supplies are not a story reward. Continued inventories are untouched.
        for mut stats in world.query_filtered::<&mut crate::combat::PlayerStats, With<Player>>().iter_mut(world) {
            stats.medkits = 0;
        }
    }
    info!("story started in {:.1}s{}", started.elapsed().as_secs_f32(), if saved.is_some() { " from its save" } else { "" });
    let mut host = StoryHost::new(story, saved);
    host.to_respawn = to_respawn;
    host.shop_items = std::fs::read_to_string(root.join(fc3_formats::story::SHOP_ITEMS_REL))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            Some((it.next()?.to_string(), (it.next()?.to_string(), it.next()?.parse().ok()?)))
        })
        .collect();
    // The old model-name lookup includes menu and signature prototypes that
    // share a model. Prefer the shopping database's actual base item identity.
    if let Some(catalog) = world.get_resource::<crate::progression::ShopCatalog>() {
        for weapon in catalog.0.iter().filter(|w| w.is_base()) {
            host.shop_items.insert(weapon.model.clone(), (weapon.archetype.clone(), weapon.name_id));
        }
    }
    world.insert_non_send(host);
}

#[allow(clippy::too_many_arguments)]
fn run_story(
    host: Option<NonSendMut<StoryHost>>,
    time: Res<Time>,
    mut ui: ResMut<StoryUi>,
    mut layers: ResMut<MissionLayers>,
    mut hud: ResMut<crate::hud::HudState>,
    (mut players, mut stats, perks, mount, state): (
        Query<(&mut Transform, &mut Player, Option<&mut crate::combat::Health>)>,
        Query<&mut crate::combat::PlayerStats, With<Player>>,
        Res<crate::progression::Perks>,
        Res<crate::traversal::Mount>,
        Res<State<GameState>>,
    ),
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    actors: Query<(&crate::story_actors::StoryActor, &Transform, Option<&crate::npc::Npc>), Without<Player>>,
    mut orders: ResMut<crate::story_actors::ActorOrders>,
    mut scripted: ResMut<ScriptedPlayer>,
    (mut inventory, mut clock, catalog, mut shop_override, mut pause_menu, mut next, mut menu_events): (
        Query<&mut crate::weapons::Inventory, With<Player>>, ResMut<crate::sky::TimeOfDay>,
        Res<crate::progression::ShopCatalog>, ResMut<crate::progression::ShopOverride>,
        ResMut<crate::progression::PauseMenu>, ResMut<NextState<GameState>>, ResMut<StoryMenuEvents>,
    ),
    (mut area, mut cinema, mut placements): (ResMut<MissionArea>, ResMut<crate::cinema::Cinema>, ResMut<EntityPlacements>),
    (actions, mut markers, heights, mut voices, driving, vehicle_views, mut seat_requests): (
        Res<crate::controls::ActionState>,
        ResMut<ObjectiveMarkers>,
        Res<crate::terrain::Heightfield>,
        ResMut<crate::faces::Voices>,
        Res<crate::vehicles::Driving>,
        Query<(Entity, &crate::vehicles::Vehicle, &Transform, Option<&crate::combat::Health>), Without<Player>>,
        ResMut<crate::vehicles::StorySeatRequests>,
    ),
    (info, slot, fov, mut bought, mut towers, ways, mut looted, loot_catalog, mut crafted, mut crafting): (
        Res<CacheInfo>,
        Res<crate::save::SaveSlot>,
        Res<crate::settings::Settings>,
        ResMut<StoryPurchases>,
        MessageReader<crate::traversal::TowerTaken>,
        Res<crate::traversal::Ways>,
        ResMut<StoryLoot>,
        Res<crate::progression::LootCatalog>,
        ResMut<StoryCrafts>,
        ResMut<crate::crafting::CraftCatalog>,
    ),
    (story_actors, mut commands_out, mut hidden, tagged): (
        Query<Entity, With<crate::story_actors::StoryActor>>, Commands, ResMut<HiddenEntities>,
        Query<&crate::story_actors::StoryActor, With<crate::stealth::Tagged>>,
    ),
    (mut xp_gain, mut progress, mut music, mut noise, mut videos, mut qtes): (
        MessageWriter<crate::progression::XpGain>,
        ResMut<crate::progression::Progress>,
        ResMut<crate::music::Music>,
        MessageWriter<crate::npc::Noise>,
        ResMut<crate::video::Videos>,
        ResMut<crate::qte::Qtes>,
    ),
) {
    // (Not a run condition: those may run off the main thread.)
    let Some(mut host) = host else { return };
    let now = time.elapsed_secs();
    let gameplay = *state.get() == GameState::Playing;
    let dt = if gameplay { time.delta_secs().min(0.1) } else { 0.0 };
    let host = &mut *host;
    // Host work that came due.
    host.due.sort_by(|a, b| a.0.total_cmp(&b.0));
    while host.due.first().is_some_and(|d| d.0 <= now) {
        match host.due.remove(0).1 {
            Due::Sound(id) => host.story.sound_finished(id, 0.0),
        }
    }
    let Ok((mut t, mut p, mut health)) = players.single_mut() else { return };
    let mut heal = false;
    // Dying in a mission takes the player back to its last checkpoint.
    if std::mem::take(&mut host.to_respawn) {
        let data = host.story.data();
        if let Some(sp) = host.story.respawn_point().and_then(|r| data.spawn_points.get(&r)) {
            teleport(&mut t, &mut p, sp.pos, sp.angles[2]);
        }
    }
    let dead = health.as_ref().is_some_and(|h| h.dead);
    if dead && !host.was_dead && host.story.missions().iter().any(|m| m.1 == "Running") {
        host.reload_at = Some(now + 6.0);
    }
    host.was_dead = dead;
    if host.reload_at.is_some_and(|at| now >= at) && !dead {
        host.reload_at = None;
        let checkpoint = host.checkpoint.clone();
        if let Some(story) = open_story(&info.root, checkpoint.as_ref()) {
            info!("story: back to the last checkpoint");
            host.story = story;
            placements.reset(&host.story.data());
            host.hidden_synced = false;
            host.due.clear();
            layers.present.clear();
            orders.0.clear();
            cinema.reset();
            *scripted = ScriptedPlayer::default();
            shop_override.0 = None;
            *pause_menu = crate::progression::PauseMenu::from_story(&host.story);
            bought.0.clear();
            looted.0.clear();
            crafted.0.clear();
            menu_events.0.clear();
            ui.objective = None;
            ui.hints.clear();
            ui.boxes.clear();
            ui.phone = None;
            ui.timers.clear();
            music.reset();
            videos.reset();
            qtes.reset();
            ui.subtitle = None;
            for e in &story_actors {
                commands_out.entity(e).despawn();
            }
            let data = host.story.data();
            if let Some(sp) = host.story.respawn_point().and_then(|r| data.spawn_points.get(&r)) {
                teleport(&mut t, &mut p, sp.pos, sp.angles[2]);
            }
        }
    }
    if !host.hidden_synced {
        host.hidden_synced = true;
        hidden.0 = host.story.removed();
    }
    // Where the view looks (Bevy forward at this yaw and pitch, in Dunia).
    let forward = Quat::from_euler(EulerRot::YXZ, p.yaw, p.pitch, 0.0) * Vec3::NEG_Z;
    let mut view = View {
        player: to_dunia(t.translation),
        player_yaw: -p.yaw.to_degrees(),
        look: [forward.x, -forward.z, forward.y],
        fov: fov.fov,
        time_of_day: clock.hours,
        difficulty: match fov.difficulty {
            crate::settings::Difficulty::Casual => 0,
            crate::settings::Difficulty::Adventurer => 1,
            crate::settings::Difficulty::Warrior => 2,
        },
        player_in_vehicle: p.driving,
        tagged: tagged.iter().map(|a| a.id).collect(),
        ..View::default()
    };
    if let Some(h) = health.as_ref() {
        view.player_dead = h.dead;
        view.player_state.health = h.current;
        view.player_state.max_health = h.max;
    }
    view.player_state.money = progress.money;
    view.player_state.loot = loot_catalog.quantities(&progress);
    view.player_state.medpacks = stats.single().map_or(0, |s| s.medkits);
    view.player_state.weapon_slots = perks.weapon_slots as u32;
    view.player_state.skills = progress.skills.iter().map(|s| s.to_ascii_lowercase()).collect();
    view.player_state.on_zipline = matches!(*mount, crate::traversal::Mount::Zip { .. });
    view.player_state.look_angles = [p.pitch.to_degrees(), 0.0, -p.yaw.to_degrees()];
    for (entity, vehicle, at, health) in &vehicle_views {
        view.spawned.insert(vehicle.story_id(), fc3_story::Live { pos: to_dunia(at.translation), alive: health.is_none_or(|h| !h.dead),
            health: health.map_or(1.0, |h| h.current), ai_state: 0 });
        view.pawn_vehicles.extend(vehicle.npc_occupants());
        if p.driving && driving.vehicle == Some(entity) {
            if let Some(occupant) = vehicle.occupant(driving.seat) {
                view.player_state.vehicle = Some(occupant.vehicle);
                view.player_state.seat = occupant.kind;
                view.player_state.seat_index = occupant.index;
                view.player_state.vehicle_type = occupant.vehicle_type;
            }
        }
    }
    if let Ok(inv) = inventory.single() {
        (view.player_state.weapons, view.player_state.in_hands) = story_weapons(&inv.defs, &inv.slots, inv.current, &host.shop_items);
        for (name, count) in [("Grenade", inv.grenades), ("Molotov", inv.molotovs), ("C4", inv.c4), ("Mine", inv.mines)] {
            if count > 0 {
                view.player_state.gadgets.push(fc3_story::Item { name: format!("gadgets:FC3/{name}"), count, ..Default::default() });
            }
        }
    }
    view.player_state.gadgets.extend(scripted.gadgets.iter().map(|name| {
        let camera = camera_gadget(name);
        fc3_story::Item {
            name: name.clone(), count: 1,
            category: if camera { "binoculars" } else { "" }.into(),
            class: if camera { "binoculars" } else { "" }.into(),
        }
    }));
    // FC3_STORY_LOOK=<entity id>: face that character once it is in (tests).
    if let Some(id) = std::env::var("FC3_STORY_LOOK").ok().and_then(|v| v.parse::<u64>().ok()) {
        if let Some((_, at, _)) = actors.iter().find(|(a, _, _)| a.id == id) {
            let dir = at.translation - t.translation;
            if dir.length() > 0.5 {
                p.yaw = (-dir.x).atan2(-dir.z);
                p.pitch = -0.1;
            }
        }
    }
    for (a, at, npc) in &actors {
        // The AI's state as scripts' state listeners know it: relaxed,
        // investigating, alerted, in combat, dead.
        let ai_state = match npc.map(|n| n.state) {
            _ if a.dead => 4,
            Some(crate::npc::NpcState::Suspicious) => 1,
            Some(crate::npc::NpcState::Search) => 2,
            Some(crate::npc::NpcState::Combat) => 3,
            _ => 0,
        };
        view.spawned
            .insert(a.id, fc3_story::Live { pos: to_dunia(at.translation - Vec3::Y * crate::player::HALF_HEIGHT), alive: !a.dead, health: 1.0, ai_state });
    }
    view.player_state.stance = if p.swimming {
        2
    } else if p.crouching {
        1
    } else {
        0
    };
    // The player's inputs, as the signals scripts listen for.
    if gameplay {
        use crate::controls::Action;
        const SIGNALS: [(Action, &str); 11] = [
            (Action::Jump, "jump"),
            (Action::Sprint, "sprint"),
            (Action::Crouch, "crouch"),
            (Action::ThrowRock, "throw_rock"),
            (Action::Grenade, "throw_grenade"),
            (Action::Fire, "updateshooting"),
            (Action::Map, "show_map"),
            (Action::Medkit, "heal"),
            (Action::NextWeapon, "select_next_weapon"),
            (Action::PrevWeapon, "select_previous_weapon"),
            (Action::Interact, "use"),
        ];
        for (action, signal) in SIGNALS {
            if actions.just_pressed(action) && !scripted.ignored.contains(signal) {
                host.story.input_signal(signal);
            }
        }
        if actions.movement.length() > 0.1 && !scripted.ignored.contains("move") {
            host.story.input_signal("move");
        }
    }
    // A radio tower taken: its panel's start and finish (on the player and
    // on the panel).
    for tower in towers.read() {
        tower_taken(&mut host.story, tower.at);
    }
    // Purchases, as the shop's buy event on the player (the game's item,
    // its name's section and string id).
    let mut commands = host.story.update(dt, view);
    host.forward_purchases(&mut bought);
    host.forward_loot(&mut looted, &loot_catalog);
    host.forward_crafts(&mut crafted);
    host.forward_menu_events(&mut menu_events);
    crafting.completed_missions = host.story.missions().into_iter().filter(|m| m.1 == "Completed")
        .map(|m| m.0.to_ascii_lowercase()).collect();
    pause_menu.update_unlocks(&crafting.completed_missions);
    commands.extend(host.story.take_commands());
    ui.timers = host.story.timers();
    let data = host.story.data();
    let now_markers: Vec<Vec3> = host.story.objective_markers().into_iter().map(|(_, p)| to_bevy(p)).collect();
    if now_markers.len() != markers.0.len() {
        debug!("story: {} objective markers", now_markers.len());
    }
    markers.0 = now_markers;
    let in_mission = host.story.missions().iter().any(|m| m.1 == "Running");
    if music.in_mission != in_mission {
        music.in_mission = in_mission;
    }
    let sectors = host.story.active_zone_sectors();
    if sectors != area.sectors {
        area.sectors = sectors;
    }
    // Tests: when nothing has happened for a while, step into a trigger a
    // script waits on.
    host.quiet += dt;
    if commands.iter().any(|c| !matches!(c, Command::Layer { .. } | Command::Print(_) | Command::Dialog { .. })) {
        host.quiet = 0.0;
    }
    if gameplay && std::env::var("FC3_STORY_AUTOPILOT").is_ok() && host.quiet > 20.0 && host.quiet - dt <= 20.0 {
        info!("story autopilot: stalled; triggers {:?}, signals {:?}", host.story.watched_triggers(), host.story.watched_signals());
        for w in host.story.waiting_on() {
            info!("story autopilot: waiting on {w}");
        }
        for w in host.story.watched_triggers() {
            info!("story autopilot: trigger {} {}: {}", w.0, w.1, host.story.trigger_debug(w.0));
        }
    }
    if gameplay && !p.driving && cinema.view.is_none() && std::env::var("FC3_STORY_AUTOPILOT").is_ok()
        && host.quiet > 6.0 && host.story.watched_loot_filters().is_empty() {
        let watched = host.story.watched_triggers();
        // Stuck: triggers stepped into before they were armed go again.
        if host.quiet > 25.0 {
            host.used.retain(|u| !watched.iter().any(|w| w.0 == u.0 && w.1 == u.1));
        }
        if let Some((id, event, point, _)) = watched.iter().find(|w| w.3 && !host.used.contains(&(w.0, w.1.clone()))) {
            info!("story autopilot: {event} {}", data.name_of(*id).unwrap_or("?"));
            host.used.insert((*id, event.clone()));
            // A region: somewhere its heights reach the ground.
            let ground = |x: f32, y: f32| heights.height_at(to_bevy([x, y, 0.0]));
            let point = host.story.trigger_ground_point(*id, ground).unwrap_or(*point);
            teleport(&mut t, &mut p, point, 0.0);
            // High up (a tower's top, a roof): held there, not set down on
            // whatever is below.
            let at = to_bevy(point);
            p.noclip = event != "EnterFOV" && heights.height_at(at).is_some_and(|g| at.y > g + 6.0);
            if p.noclip {
                t.translation = at + Vec3::Y * crate::player::HALF_HEIGHT;
                p.spawning = false;
            }
            if event == "EnterFOV" {
                // Stand back from the spot and look at it.
                let at = to_bevy(point);
                t.translation = at + Vec3::new(0.0, 0.0, 2.0);
                p.yaw = 0.0;
                p.pitch = ((at.y - t.translation.y - 0.6) / 2.0).atan();
            }
            if event == "Use" {
                host.story.entity_event(*id, "Use", vec![Arg::id(fc3_story::PLAYER)]);
            }
            host.quiet = 0.0;
        } else if let Some((id, at)) = actors
            .iter()
            .filter(|(a, _, npc)| npc.is_some() && !a.dead && !host.used.contains(&(a.id, "distract".into())))
            .find(|(a, _, _)| host.story.current_action(a.id).is_some_and(|ca| host.story.awaited_interrupts().contains(&ca)))
            .map(|(a, t, _)| (a.id, t.translation))
        {
            // A script waits for a guard to be drawn off: a rock lands by him.
            info!("story autopilot: distract {}", data.name_of(id).unwrap_or("?"));
            host.used.insert((id, "distract".into()));
            noise.write(crate::npc::Noise { at: at + Vec3::new(3.0, 0.0, 0.0), radius: 6.0 });
            host.quiet = 0.0;
        } else if !host.used.contains(&(0, "buy".into())) && host.story.waiting_on().iter().any(|w| w.contains("CPawnBuyItemEvent")) {
            if shop_override.0.is_some() {
                // The tutorial's smoke purchase uses the UI's real transaction,
                // rather than emitting a purchase for an unowned weapon.
                if let Some(item) = inventory.single_mut().ok().and_then(|mut inv|
                    crate::progression::purchase_tutorial_weapon(&mut inv, &mut progress, &perks, &catalog, &shop_override)) {
                    if let Some((archetype, id)) = item.model.as_ref().and_then(|m| host.shop_items.get(m)) {
                        info!("story autopilot: bought {archetype}");
                        host.story.entity_event(fc3_story::PLAYER, "CPawnBuyItemEvent",
                            vec![Arg::Str(archetype.clone()), Arg::Str("Name".into()), Arg::Str(id.to_string())]);
                        host.used.insert((0, "buy".into()));
                        host.quiet = 0.0;
                    }
                }
            }
        } else if let Some((id, name)) = host.story.gameplays().into_iter().find(|g| g.1 != "QTEGSF") {
            // A minigame or QTE the game doesn't play yet: done.
            info!("story autopilot: end gameplay {name}");
            host.story.gameplay_ended(id);
            host.quiet = 0.0;
        } else if let Some(at) = host
            .last_ordered
            .filter(|_| host.quiet > 12.0)
            .and_then(|id| actors.iter().find(|(a, _, npc)| a.id == id && npc.is_none() && !a.dead))
            .map(|(_, at, _)| at.translation)
            .filter(|at| at.distance(t.translation) > 6.0)
        {
            // Following someone: catch up with the friend last sent off.
            info!("story autopilot: follow");
            let to = to_dunia(at + Vec3::new(1.5, 0.0, 1.5) - Vec3::Y * crate::player::HALF_HEIGHT);
            teleport(&mut t, &mut p, to, 0.0);
            host.quiet = 0.0;
        } else if let Some(at) = ways
            .data
            .towers
            .iter()
            .enumerate()
            .filter(|(i, _)| !progress.towers.contains(&(*i as u32)))
            .map(|(_, at)| Vec3::from(*at))
            .filter(|at| host.quiet > 12.0 && at.distance(t.translation) < 80.0)
            // (Once a mission listens for the tower, not just the towers'
            // own manager.)
            .filter(|_| host.story.waiting_on().iter().any(|w| w.contains("CRadioTowerFinishedEvent") && !w.contains("RadioTowersManager")))
            .min_by(|a, b| a.distance(t.translation).total_cmp(&b.distance(t.translation)))
        {
            // Up a radio tower: its panel taken (the climb is the player's).
            info!("story autopilot: tower panel");
            if let Some(i) = ways.data.towers.iter().position(|w| Vec3::from(*w) == at) {
                progress.towers.insert(i as u32);
            }
            tower_taken(&mut host.story, at);
            host.quiet = 0.0;
        } else if let Some(signal) = host.story.watched_signals().into_iter().find(|s| !host.used.contains(&(0, s.clone()))) {
            // No trigger left: give an input a script waits for.
            info!("story autopilot: input {signal}");
            host.used.insert((0, signal.clone()));
            // A rock "lands" where a trigger waits for someone lured there.
            if signal == "throw_rock" {
                if let Some(w) = host.story.watched_triggers().first() {
                    noise.write(crate::npc::Noise { at: to_bevy(w.2), radius: 12.0 });
                }
            }
            host.story.input_signal(&signal);
            host.quiet = 0.0;
        }
    }
    for c in commands {
        match c {
            Command::Layer { layer, state } => {
                if state.present() {
                    layers.present.insert(layer);
                } else {
                    layers.present.remove(&layer);
                }
                let _ = LayerState::Active;
            }
            Command::Objective { text, .. } => {
                info!("story: objective {:?}", text.as_deref().map(plain));
                if let Some(t) = &text {
                    hud.notify(format!("New objective: {}", plain(t)));
                }
                ui.objective = text.map(|t| plain(&t));
            }
            Command::MissionTitle { title, .. } => {
                if let Some(t) = title {
                    ui.banner = Some((plain(&t), 5.0));
                }
            }
            Command::MissionComplete { title, xp, money, .. } => {
                let name = title.map(|t| plain(&t)).unwrap_or_default();
                ui.banner = Some((format!("{name}\nMission complete"), 6.0));
                if xp > 0 {
                    xp_gain.write(crate::progression::XpGain { amount: xp, reason: "Mission complete" });
                }
                if money > 0 {
                    let got = progress.add_money(money);
                    hud.notify(format!("+${got}"));
                }
            }
            Command::MissionFailed { text, reload } => {
                info!("story: mission failed ({:?}), reload {reload}", text.as_deref().map(plain));
                ui.banner = Some((format!("Mission failed\n{}", text.map(|t| plain(&t)).unwrap_or_default()), 5.0));
                if reload && host.reload_at.is_none() {
                    host.reload_at = Some(now + 4.0);
                }
            }
            Command::Checkpoint => {
                let save = host.story.save();
                if let Some(path) = slot.story_path() {
                    let tmp = path.with_extension("tmp");
                    if let Err(e) = std::fs::write(&tmp, save.to_text()).and_then(|_| std::fs::rename(&tmp, &path)) {
                        warn!("could not save the story to {}: {e}", path.display());
                    }
                }
                // FC3_STORY_CHECKPOINTS=<dir> (tests): every checkpoint kept,
                // named by the order and the missions running.
                if let Ok(dir) = std::env::var("FC3_STORY_CHECKPOINTS") {
                    let running: Vec<String> = host.story.blocks().into_iter().filter(|b| b.1 == "Running").map(|b| b.0).collect();
                    host.kept += 1;
                    let path = std::path::Path::new(&dir).join(format!("{:03}_{}.txt", host.kept, running.join("+")));
                    let _ = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, save.to_text()));
                    host.checkpoint_game_paths.push(path.with_extension("game.txt"));
                }
                host.checkpoint = Some(save);
            }
            Command::MissionState { name, state, block } => {
                info!("story: {name} {state}");
                if block.is_some() && state == "Completed" {
                    host.last_completed = Some(name);
                }
            }
            // Removed by a script: gone from the world (the story
            // remembers it in its saves).
            Command::Remove(id) => {
                hidden.0.insert(id);
            }
            Command::Teleport { entity, pose } => {
                if entity == fc3_story::PLAYER {
                    teleport(&mut t, &mut p, pose.position, pose.angles[2]);
                } else {
                    placements.set(entity, pose);
                }
            }
            Command::RespawnPoint(spawn) => {
                // A cinematic block hands over where its scene left the
                // player: there (cinematics aren't played yet).
                if host.last_completed.as_deref().is_some_and(|n| n.ends_with('C')) {
                    if let Some(sp) = data.spawn_points.get(&spawn) {
                        teleport(&mut t, &mut p, sp.pos, sp.angles[2]);
                    }
                }
            }
            Command::Dialog { speaker, sound, subtitle, seconds, .. } => {
                let at = if speaker == fc3_story::PLAYER {
                    None
                } else {
                    actors.iter().find(|(a, _, _)| a.id == speaker).map(|(_, t, _)| t.translation).or_else(|| data.position(speaker).map(to_bevy))
                };
                sfx.write(crate::sound::PlaySfx { event: sound, at, volume: 1.0 });
                voices.0.push((speaker, sound));
                if let Some(s) = subtitle {
                    ui.subtitle = Some((plain(&s), seconds.max(1.0)));
                }
            }
            Command::StopDialog { .. } => ui.subtitle = None,
            Command::Action { pawn, action } => {
                if action.is_some() {
                    host.last_ordered = Some(pawn);
                }
                orders.0.insert(pawn, action);
            }
            Command::Print(text) => debug!("story: {text}"),
            Command::Message(m) if m == "PhoneCallEnd" => ui.phone = None,
            Command::Native { name, args } => {
                let a = |i: usize| args.get(i).cloned().unwrap_or_default();
                match name.as_str() {
                    "OverridePauseMenu" | "OverridePauseMenuTutorial" => {
                        pause_menu.override_page(a(1) == "true", a(2) == "true", name == "OverridePauseMenuTutorial");
                        if a(0) == "true" { next.set(GameState::Journal); }
                    }
                    "LockPauseMenuOption" => {
                        if a(1) == "true" { pause_menu.crafting_locked = a(0) == "true"; }
                        if a(2) == "true" { pause_menu.skills_locked = a(0) == "true"; }
                    }
                    // No cinematic player yet: a scene starts and ends at once.
                    // Scenes play in `cinema`.
                    "Sequence" => {
                        if a(0).starts_with("play") {
                            scripted.fade = (scripted.fade.0, 0.0, 0.6);
                        }
                        cinema.requests.push(crate::cinema::Request {
                            mode: a(0),
                            scene: a(1).parse().unwrap_or(0),
                            file: a(2),
                            sync: Some(a(4)).filter(|s| !s.is_empty()),
                            meta: a(3).parse().ok(),
                        });
                    }
                    "CSequenceManager:StopCinematicMetaSequence" => cinema.requests.push(crate::cinema::Request {
                        mode: "stop_meta".into(), scene: 0, file: String::new(), sync: None, meta: a(0).parse().ok(),
                    }),
                    // Videos play in `video` (which tells the story when
                    // they start and end).
                    "PlayBinkVideo" | "PlayFullscreenVideo" => videos.requests.push(crate::video::VideoRequest {
                        path: a(0),
                        // (A Bink "screen" that is the player, or with no
                        // material, is the whole screen.)
                        fullscreen: name == "PlayFullscreenVideo" || a(1) == fc3_story::PLAYER.to_string() || a(2).eq_ignore_ascii_case("not_used"),
                        looped: a(4) == "true",
                        volume: a(3).parse().unwrap_or(1.0),
                    }),
                    // Quick-time events play in `qte`; other specific
                    // gameplays aren't played yet.
                    "CreateSpecificGameplay" if a(1) == "QTEGSF" => qtes.pending.push(crate::qte::QteStart {
                        id: a(0).parse().unwrap_or(0),
                        params: args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                    }),
                    "DestroySpecificGameplay" => qtes.cancelled.push(a(0).parse().unwrap_or(0)),
                    "StopBinkVideo" => videos.stops.push(false),
                    "StopFullscreenVideo" => videos.stops.push(true),
                    "PlaySound" => {
                        let at = a(1).parse().ok().filter(|&e| e != fc3_story::PLAYER).and_then(|e| data.position(e)).map(to_bevy);
                        if let Some(id) = fc3_story::values::Text(a(2)).sound() {
                            sfx.write(crate::sound::PlaySfx { event: id, at, volume: 1.0 });
                        }
                        host.due.push((now + 2.0, Due::Sound(a(0).parse().unwrap_or(0))));
                    }
                    "HudText" => {
                        let text = [a(2), a(3)].iter().filter(|s| !s.is_empty()).map(|s| plain(s)).collect::<Vec<_>>().join("\n");
                        ui.hints.insert(a(0).parse().unwrap_or(0), text);
                    }
                    "MusicState" => music.request(&a(0), a(1) == "play"),
                    "CAPI_Player.StartPhoneCall" => ui.phone = Some((plain(&a(2)), a(3) == "true", 0.0)),
                    "CFCXMusicService:SetMissionMTParameters" => {
                        let on = a(2) == "true" || a(2) == "1";
                        music.set_level(&a(0), if on { a(1).parse::<f32>().unwrap_or(0.0) as u8 } else { 0 });
                    }
                    "HudTextHidden" => {
                        ui.hints.remove(&a(0).parse().unwrap_or(0));
                    }
                    "ChecklistObjective" => {
                        let id = a(1).parse().unwrap_or(0);
                        if a(0) == "true" {
                            ui.checklist.insert(id, plain(&a(2)));
                        } else {
                            ui.checklist.remove(&id);
                        }
                    }
                    "ShowDialogBox" | "ShowTutorialBox" => {
                        let box_ = StoryBox {
                            id: a(0).parse().unwrap_or(0), title: plain(&a(1)), body: plain(&a(2)), choice: a(3) == "true",
                            prompt: Some(plain(&a(4))).filter(|p| !p.is_empty()),
                        };
                        // A high-priority tutorial goes immediately after the
                        // current box, keeping that box's answer target intact.
                        if a(5) == "true" && !ui.boxes.is_empty() {
                            ui.boxes.insert(1, box_);
                        } else {
                            ui.boxes.push_back(box_);
                        }
                    }
                    // What a mission does to the player.
                    "CAPI_Player.SetIgnorePlayerSignal" => {
                        if a(2) == "true" || a(2) == "1" {
                            scripted.ignored.insert(a(1));
                        } else {
                            scripted.ignored.remove(&a(1));
                        }
                    }
                    "CAPI_Player.SetMovementSensitivity" => scripted.movement = a(1).parse().unwrap_or(1.0f32).clamp(0.0, 2.0),
                    "EnableGodMode" => scripted.god = a(0) != "0" && a(0) != "false",
                    "SetHudVisible" => scripted.hud_hidden = a(0) == "0" || a(0) == "false",
                    "OverrideShopMenu" => shop_override.set(a(0) == "true" || a(0) == "1", a(1).parse().unwrap_or(0), a(2).parse().unwrap_or(0)),
                    "CAPI_Player.StartPostFX" | "CAPI_Player.StopPostFX" => {
                        let on = name.ends_with("StartPostFX");
                        match a(1).to_ascii_lowercase().as_str() {
                            "blackscreennowfx" => scripted.fade = if on { (1.0, 1.0, 0.0) } else { (0.0, 0.0, 0.0) },
                            "blackscreenfx" | "fadetoblackfx" => scripted.fade = (scripted.fade.0, if on { 1.0 } else { 0.0 }, 0.6),
                            _ => {}
                        }
                    }
                    "CAPI_Entity.SetVisibility" => {
                        if let Ok(id) = a(0).parse::<u64>() {
                            if a(1) == "false" || a(1) == "0" {
                                hidden.0.insert(id);
                            } else {
                                hidden.0.remove(&id);
                            }
                        }
                    }
                    "SetTimeOfDay" => {
                        let (h, m, sec) = (a(0).parse().unwrap_or(12.0f32), a(1).parse().unwrap_or(0.0f32), a(2).parse().unwrap_or(0.0f32));
                        clock.hours = h + m / 60.0 + sec / 3600.0;
                    }
                    "SetTimeScale" => clock.speed = crate::sky::CLOCK_SPEED * a(0).parse().unwrap_or(1.0f32).max(0.0),
                    "CAPI_Pawn.HealPawn" if a(0) == fc3_story::PLAYER.to_string() => {
                        heal = true;
                    }
                    "CAPI_Pawn.RemoveAllInventory" | "CAPI_Pawn.RemoveAllWeapon" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let Ok(mut inv) = inventory.single_mut() {
                            inv.slots.clear();
                            inv.current = 0;
                            if name.ends_with("RemoveAllInventory") {
                                (inv.grenades, inv.molotovs, inv.c4, inv.mines) = (0, 0, 0, 0);
                                scripted.gadgets.clear();
                            }
                            crate::weapons::settle_unarmed(&mut inv);
                        }
                    }
                    "CAPI_Pawn.SpawnWeapon" if a(0) == fc3_story::PLAYER.to_string() => {
                        if a(1).starts_with("gadgets:") {
                            scripted.gadgets.insert(a(1));
                        }
                        if let Ok(mut inv) = inventory.single_mut() {
                            if let Some(def) = weapon_def(&inv, &a(1)) {
                                let wname = inv.defs[def].name;
                                crate::weapons::grant_weapon(&mut inv, wname, 4);
                            }
                        }
                    }
                    "CAPI_Pawn.RemoveWeapon" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let Ok(mut inv) = inventory.single_mut() {
                            if let Some(def) = weapon_def(&inv, &host.story.item_name(&a(1))) {
                                inv.slots.retain(|w| w.def != def);
                                inv.current = inv.current.min(inv.slots.len().saturating_sub(1));
                                crate::weapons::settle_unarmed(&mut inv);
                            }
                        }
                    }
                    "CAPI_Entity.SetPhysicsEnabled" => {
                        if let Ok(id) = a(0).parse::<u64>() {
                            placements.set_physics(id, a(1) != "false" && a(1) != "0");
                        }
                    }
                    "CAPI_Player.AddGadget" if a(0) == fc3_story::PLAYER.to_string() => {
                        scripted.gadgets.insert(host.story.item_name(&a(1)));
                    }
                    "CAPI_Player.EnableAnimalLooting" if a(0) == fc3_story::PLAYER.to_string() => {
                        scripted.animal_looting = a(1) != "false" && a(1) != "0";
                    }
                    "CAPI_Player.EnablePlantLooting" if a(0) == fc3_story::PLAYER.to_string() => {
                        scripted.plant_looting = a(1) != "false" && a(1) != "0";
                    }
                    "CAPI_Vehicle.VehicleUseTakeSeat" | "CAPI_Vehicle.VehicleUseExit" => seat_requests.native(&name, &args),
                    "CAPI_Player.ForceSetPlayerInVehicle" => seat_requests.force_player(&args),
                    "CAPI_Player.GiveMedpacksToPlayer" | "CAPI_Player.RemoveMedpacksToPlayer" => {
                        if let Ok(mut stats) = stats.single_mut() {
                            apply_medpack_command(&mut stats.medkits, perks.medkit_cap, &name, &args);
                        }
                    }
                    "CAPI_Player.RemoveGadget" if a(0) == fc3_story::PLAYER.to_string() => {
                        let name = host.story.item_name(&a(1));
                        scripted.gadgets.retain(|g| !g.eq_ignore_ascii_case(&name));
                    }
                    "CAPI_Player.RemoveAllGadget" if a(0) == fc3_story::PLAYER.to_string() => {
                        scripted.gadgets.clear();
                        if let Ok(mut inv) = inventory.single_mut() {
                            (inv.grenades, inv.molotovs, inv.c4, inv.mines) = (0, 0, 0, 0);
                        }
                    }
                    "CAPI_Pawn.BackupInventory" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let Ok(inv) = inventory.single() {
                            scripted.backup_inventory(inv);
                        }
                    }
                    "CAPI_Pawn.RestoreBackupInventory" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let Ok(mut inv) = inventory.single_mut() {
                            scripted.restore_inventory(&mut inv);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if heal {
        if let Some(h) = health.as_mut() {
            h.current = h.max;
        }
    }
    for line in host.story.take_log() {
        if !line.starts_with("console:") {
            debug!("story: {line}");
        }
    }
    let unknown = host.story.take_unknown_calls();
    if std::env::var("FC3_DOMINO_TRACE").is_ok() {
        for call in unknown {
            info!("script native not provided: {call}");
        }
    }
}

#[derive(Component)]
struct BoxPanel(u64);

const USE_HOLD_SECONDS: f32 = 0.5;

#[derive(Default)]
pub(crate) struct StoryInteraction {
    target: Option<u64>,
    held: f32,
    latched: bool,
}

/// Script inventory adjustments aren't crafting or looting: they change the
/// carried count without generating a recipe callback or consuming leaves.
fn apply_medpack_command(count: &mut u32, capacity: u32, name: &str, args: &[String]) -> bool {
    if args.first().and_then(|a| a.parse::<u64>().ok()) != Some(fc3_story::PLAYER) { return false; }
    let Some(amount) = args.get(1).and_then(|a| a.parse::<u32>().ok()) else { return false; };
    match name {
        "CAPI_Player.GiveMedpacksToPlayer" => *count = count.saturating_add(amount).min(capacity),
        "CAPI_Player.RemoveMedpacksToPlayer" => *count = count.saturating_sub(amount),
        _ => return false,
    }
    true
}

impl StoryInteraction {
    fn advance(&mut self, target: Option<u64>, pressed: bool, dt: f32) -> bool {
        if !pressed {
            *self = Self::default();
            return false;
        }
        if self.latched {
            return false;
        }
        if self.target != target {
            self.target = target;
            self.held = 0.0;
        }
        if target.is_none() {
            self.held = 0.0;
            return false;
        }
        self.held += dt.max(0.0);
        if self.held >= USE_HOLD_SECONDS {
            self.latched = true;
            return true;
        }
        false
    }
}

/// Mission use triggers are driven by the player's real interact control,
/// before ordinary pickups, reloads and vehicles can claim the same press.
#[allow(clippy::too_many_arguments)]
pub(crate) fn story_interactions(
    host: Option<NonSendMut<StoryHost>>,
    state: Res<State<GameState>>,
    time: Res<Time>,
    virtual_time: Res<Time<Virtual>>,
    ui: Res<StoryUi>,
    cinema: Res<crate::cinema::Cinema>,
    qtes: Res<crate::qte::Qtes>,
    cinematic: Res<crate::fp::Cinematic>,
    players: Query<(&Transform, &Player, &crate::combat::Health)>,
    mut actions: ResMut<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    mut hud: ResMut<crate::hud::HudState>,
    mut hold: Local<StoryInteraction>,
) {
    let Some(mut host) = host else { return };
    if *state.get() != GameState::Playing || virtual_time.is_paused() || !ui.boxes.is_empty()
        || cinema.view.is_some() || qtes.active() || cinematic.0 > 0.0 {
        hold.held = 0.0;
        return;
    }
    let Ok((at, player, health)) = players.single() else { return };
    if health.dead || player.driving {
        *hold = StoryInteraction::default();
        return;
    }
    let pressed = actions.pressed(crate::controls::Action::Interact);
    let look = Quat::from_euler(EulerRot::YXZ, player.yaw, player.pitch, 0.0) * Vec3::NEG_Z;
    let eye = at.translation + Vec3::Y * (crate::player::EYE_HEIGHT - crate::player::HALF_HEIGHT);
    let target = host.story.use_target(to_dunia(at.translation), to_dunia(eye), to_dunia(look));
    let used = hold.advance(target.as_ref().map(|t| t.entity), pressed, time.delta_secs());
    if target.is_some() || hold.latched {
        if pressed {
            actions.suppress(crate::controls::Action::Interact);
            if actions.gamepad_active {
                actions.suppress(crate::controls::Action::Reload);
            }
        }
    }
    let Some(target) = target else { return };
    let key = actions.label(crate::controls::Action::Interact, &settings);
    let progress = (hold.held / USE_HOLD_SECONDS).clamp(0.0, 1.0);
    hud.prompt = Some(if pressed && !hold.latched {
        format!("[{key}]  {}  {:.0}%", plain(&target.text), progress * 100.0)
    } else {
        format!("Hold [{key}]  {}", plain(&target.text))
    });
    if used {
        host.story.entity_event(target.entity, "Use", vec![Arg::id(fc3_story::PLAYER)]);
    }
}

/// The first waiting box: the game stops (time and input) until the player
/// accepts, or cancels one that asks.
#[allow(clippy::too_many_arguments)]
fn story_boxes(
    host: Option<NonSendMut<StoryHost>>,
    mut ui: ResMut<StoryUi>,
    mut actions: ResMut<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    state: Res<State<GameState>>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    gamepads: Query<&Gamepad>,
    mut time: ResMut<Time<Virtual>>,
    mut paused: Local<bool>,
    panels: Query<(Entity, &BoxPanel)>,
    mut commands: Commands,
) {
    let shown = ui.boxes.front().map(|b| b.id);
    for (e, panel) in &panels {
        if Some(panel.0) != shown {
            commands.entity(e).despawn();
        }
    }
    let Some(b) = ui.boxes.front() else {
        if std::mem::take(&mut *paused) && matches!(state.get(), GameState::Playing | GameState::MainMenu) {
            time.unpause();
        }
        return;
    };
    if !*paused || !time.is_paused() {
        time.pause();
        *paused = true;
    }
    let pad = actions.gamepad_active;
    let pad_pressed = |button| gamepads.iter().any(|g| g.just_pressed(button));
    // (The autopilot accepts whatever is asked.)
    // Raw edges preserve release-to-press semantics while the modal clears
    // ActionState every frame, and work even when a menu releases the cursor.
    let interact = match settings.binding(crate::controls::Action::Interact) {
        Some(crate::controls::Binding::Key(key)) => keys.just_pressed(key),
        Some(crate::controls::Binding::Mouse(button)) => mouse.just_pressed(button),
        None => false,
    };
    let accept = interact
        || keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter, KeyCode::Space])
        || pad_pressed(GamepadButton::South)
        || std::env::var("FC3_STORY_AUTOPILOT").is_ok();
    let cancel = b.choice && (keys.any_just_pressed([KeyCode::Backspace, KeyCode::KeyQ]) || pad_pressed(GamepadButton::East));
    actions.clear();
    if !panels.iter().any(|(_, p)| p.0 == b.id) {
        let fallback = match (b.choice, pad) {
            (true, false) => "[Enter]  Accept        [Backspace]  Cancel",
            (true, true) => "A  Accept        B  Cancel",
            (false, false) => "[Enter]  Continue",
            (false, true) => "A  Continue",
        };
        let prompt = b.prompt.as_ref().map(|p| prompt_text(p, &actions, &settings)).unwrap_or_else(|| fallback.to_string());
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)),
            GlobalZIndex(12),
            BoxPanel(b.id),
            children![(
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(16.0),
                    padding: UiRect::all(Val::Px(28.0)),
                    max_width: Val::Px(680.0),
                    border: UiRect::top(Val::Px(3.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.04, 0.05, 0.05, 0.92)),
                BorderColor::all(Color::srgb(1.0, 0.75, 0.2)),
                children![
                    (
                        Text::new(b.title.to_uppercase()),
                        TextFont { font_size: bevy::text::FontSize::Px(28.0), ..default() },
                        TextColor(Color::srgb(1.0, 0.82, 0.35))
                    ),
                    (Text::new(prompt_text(&b.body, &actions, &settings)), TextFont { font_size: bevy::text::FontSize::Px(19.0), ..default() }, TextColor(Color::srgb(0.93, 0.93, 0.9))),
                    (Text::new(prompt), TextFont { font_size: bevy::text::FontSize::Px(16.0), ..default() }, TextColor(Color::srgba(0.85, 0.85, 0.8, 0.85))),
                ],
            )],
        ));
    }
    if !(accept || cancel) {
        return;
    }
    let id = b.id;
    ui.boxes.pop_front();
    if let Some(mut host) = host {
        host.story.box_answered(id, accept);
    }
}

/// Puts the player at a place (Dunia space) facing a heading (degrees).
/// A radio tower taken at its panel (Bevy space): the panel's start and
/// finish, on the player and on the panel.
fn tower_taken(story: &mut Story, at: Vec3) {
    let at = to_dunia(at);
    let panel = story
        .data()
        .entities
        .values()
        .filter(|e| e.has("CRadioTowerPanelComponent"))
        .map(|e| (e.id, (0..3).map(|i| (e.pos[i] - at[i]).powi(2)).sum::<f32>()))
        .filter(|(_, d)| *d < 40.0 * 40.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id);
    if let Some(panel) = panel {
        for event in ["CRadioTowerStartedEvent", "CRadioTowerFinishedEvent"] {
            for on in [fc3_story::PLAYER, panel] {
                story.entity_event(on, event, vec![Arg::id(panel)]);
            }
        }
    }
}

pub(crate) fn teleport(t: &mut Transform, p: &mut Player, pos: [f32; 3], yaw_deg: f32) {
    t.translation = to_bevy(pos) + Vec3::Y * 1.0;
    p.yaw = -yaw_deg.to_radians();
    p.pitch = 0.0;
    p.velocity = Vec3::ZERO;
    p.spawning = true;
    p.keep_height = true;
    p.noclip = false;
}

/// The full-screen black a mission fades in and out.
#[derive(Component)]
struct FadeOverlay;

fn fade_screen(time: Res<Time>, mut scripted: ResMut<ScriptedPlayer>, mut overlay: Query<&mut BackgroundColor, With<FadeOverlay>>) {
    // A black hold covers a scene being set up; a scene playing (or, failing
    // that, a few seconds) lifts it.
    if scripted.fade.1 > 0.0 {
        scripted.black_for += time.delta_secs();
        if scripted.black_for > 5.0 {
            scripted.fade = (scripted.fade.0, 0.0, 0.6);
        }
    } else {
        scripted.black_for = 0.0;
    }
    let (now, target, secs) = scripted.fade;
    if now != target {
        let step = if secs <= 0.0 { 1.0 } else { time.delta_secs() / secs };
        let next = if target > now { (now + step).min(target) } else { (now - step).max(target) };
        scripted.fade.0 = next;
    }
    for mut bg in &mut overlay {
        bg.0 = Color::srgba(0.0, 0.0, 0.0, scripted.fade.0);
    }
}

#[derive(Component)]
struct ObjectiveText;
#[derive(Component)]
struct SubtitleText;
#[derive(Component)]
struct BannerText;
#[derive(Component)]
struct HintText;
#[derive(Component)]
struct PhoneText;
#[derive(Component)]
struct PhonePanel;
#[derive(Component)]
struct TimerText;

fn story_text(size: f32) -> (Text, TextFont, TextColor) {
    (Text::new(""), TextFont { font_size: bevy::text::FontSize::Px(size), ..default() }, TextColor(Color::srgba(0.95, 0.95, 0.92, 0.95)))
}

fn setup_story_ui(mut commands: Commands) {
    commands.spawn((
        Node { position_type: PositionType::Absolute, top: Val::Px(72.0), width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() },
        GlobalZIndex(2),
        crate::hud::HudElement,
        children![(story_text(26.0), TextLayout::justify(Justify::Center), TimerText)],
    ));
    commands.spawn((
        Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
        GlobalZIndex(10),
        FadeOverlay,
    ));
    // Objective: top left.
    commands.spawn((
        Node { position_type: PositionType::Absolute, top: Val::Px(24.0), left: Val::Px(24.0), max_width: Val::Percent(30.0), ..default() },
        GlobalZIndex(1),
        crate::hud::HudElement,
        children![(story_text(18.0), ObjectiveText)],
    ));
    // Subtitles: bottom centre.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Percent(12.0),
            left: Val::Percent(7.0),
            width: Val::Percent(86.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        GlobalZIndex(2),
        children![(story_text(22.0), Node { width: Val::Percent(100.0), ..default() },
            TextLayout::justify(Justify::Center), TextShadow { offset: Vec2::splat(1.5), color: Color::BLACK }, SubtitleText)],
    ));
    // Banner: centre.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Percent(36.0),
            width: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        GlobalZIndex(2),
        children![(story_text(34.0), TextLayout::justify(Justify::Center), BannerText)],
    ));
    // A phone call: bottom right, over the HUD's weapon line.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(110.0),
            right: Val::Px(36.0),
            padding: UiRect::axes(Val::Px(18.0), Val::Px(12.0)),
            border: UiRect::left(Val::Px(3.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.03, 0.04, 0.04, 0.82)),
        BorderColor::all(Color::srgb(0.35, 0.8, 0.45)),
        Visibility::Hidden,
        GlobalZIndex(2),
        PhonePanel,
        crate::hud::HudElement,
        children![(story_text(18.0), PhoneText)],
    ));
    // Hints: left, under the objective.
    commands.spawn((
        Node { position_type: PositionType::Absolute, top: Val::Px(90.0), left: Val::Px(24.0), max_width: Val::Percent(30.0), ..default() },
        GlobalZIndex(1),
        crate::hud::HudElement,
        children![(story_text(16.0), HintText)],
    ));
}

#[allow(clippy::type_complexity)]
fn show_story_ui(
    time: Res<Time>,
    settings: Res<crate::settings::Settings>,
    actions: Res<crate::controls::ActionState>,
    mut ui: ResMut<StoryUi>,
    mut texts: ParamSet<(
        Query<&mut Text, With<ObjectiveText>>,
        Query<&mut Text, With<SubtitleText>>,
        Query<&mut Text, With<BannerText>>,
        Query<&mut Text, With<HintText>>,
        Query<&mut Text, With<PhoneText>>,
        Query<&mut Text, With<TimerText>>,
    )>,
    mut phone_panel: Query<&mut Visibility, With<PhonePanel>>,
) {
    let dt = time.delta_secs();
    let ui = &mut *ui;
    for slot in [&mut ui.subtitle, &mut ui.banner] {
        if let Some((_, left)) = slot {
            *left -= dt;
            if *left <= 0.0 {
                *slot = None;
            }
        }
    }
    let objective = ui.objective.as_ref().map(|s| prompt_text(s, &actions, &settings)).unwrap_or_default();
    let subtitle = if settings.subtitles { ui.subtitle.as_ref().map(|s| s.0.clone()).unwrap_or_default() } else { String::new() };
    let banner = ui.banner.as_ref().map(|s| s.0.clone()).unwrap_or_default();
    let mut hint_ids: Vec<_> = ui.hints.keys().copied().collect();
    hint_ids.sort_unstable();
    let hints = hint_ids.iter().map(|id| &ui.hints[id]).chain(ui.checklist.values())
        .map(|text| prompt_text(text, &actions, &settings)).collect::<Vec<_>>().join("\n\n");
    set_text(&mut texts.p0(), &objective);
    set_text(&mut texts.p1(), &subtitle);
    set_text(&mut texts.p2(), &banner);
    set_text(&mut texts.p3(), &hints);
    let timers = ui.timers.iter().map(timer_text).collect::<Vec<_>>().join("\n");
    set_text(&mut texts.p5(), &timers);
    // The call: ringing first, then its running time (it ends with the
    // scripts' `PhoneCallEnd`, or after two minutes at most).
    if let Some((_, _, age)) = &mut ui.phone {
        *age += dt;
        if *age > 120.0 {
            ui.phone = None;
        }
    }
    let phone = ui.phone.as_ref().map(|(who, incoming, age)| {
        let state = if *age < 4.0 {
            if *incoming {
                "INCOMING CALL".to_string()
            } else {
                "CALLING...".to_string()
            }
        } else {
            format!("ON CALL  {:01}:{:02}", (*age as u32) / 60, (*age as u32) % 60)
        };
        format!("{state}\n{who}")
    });
    set_text(&mut texts.p4(), phone.as_deref().unwrap_or(""));
    for mut v in &mut phone_panel {
        let want = if phone.is_some() { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
    }
}

fn set_text<F: bevy::ecs::query::QueryFilter>(q: &mut Query<&mut Text, F>, value: &str) {
    for mut t in q.iter_mut() {
        if t.0 != value {
            t.0 = value.to_string();
        }
    }
}

/// Smoke-test hunting/gathering uses the same controls, finite ammunition,
/// damage, carcasses and pickup transactions as a player. Repositioning still
/// makes this automation evidence, not manual traversal or UX validation.
#[allow(clippy::too_many_arguments)]
fn autopilot_loot(
    host: Option<NonSendMut<StoryHost>>,
    time: Res<Time<Virtual>>,
    ui: Res<StoryUi>,
    cinema: Res<crate::cinema::Cinema>,
    qtes: Res<crate::qte::Qtes>,
    busy: Res<crate::fp::HandsBusy>,
    scripted: Res<ScriptedPlayer>,
    catalog: Res<crate::progression::LootCatalog>,
    progress: Res<crate::progression::Progress>,
    heights: Res<crate::terrain::Heightfield>,
    mut players: Query<(&mut Transform, &mut Player, &crate::combat::Health, &crate::weapons::Inventory)>,
    animals: Query<(Entity, &Transform, &crate::wildlife::WildAnimal, &crate::combat::Health, Option<&crate::wildlife::Carcass>, Option<&crate::story_actors::StoryActor>), Without<Player>>,
    plants: Query<(&crate::pickups::Pickup, &GlobalTransform)>,
    mut actions: ResMut<crate::controls::ActionState>,
    mut last: Local<Option<(u64, &'static str)>>,
) {
    if std::env::var("FC3_STORY_AUTOPILOT").is_err() { return }
    let Some(mut host) = host else { return };
    if time.is_paused() || !ui.boxes.is_empty() || cinema.view.is_some() || qtes.active() || busy.busy() { return }
    let Ok((mut at, mut player, health, inventory)) = players.single_mut() else { return };
    if health.dead || player.spawning || player.driving || player.noclip { return }
    if host.reload_at.is_some() { return }
    let wanted = host.story.watched_loot_filters();
    if wanted.is_empty() { *last = None; return }
    let regions = host.story.watched_exit_regions();
    let inside = |at: Vec3| regions.iter().all(|r| r.contains(to_dunia(at)));
    // A persistent listener need not disable after its checklist is satisfied.
    // Balance actual carried ingredients instead of hunting every boar before
    // ever considering a leaf. This changes target choice, not mission state.
    let available = animals.iter().filter(|(_, t, _, h, carcass, actor)| {
        scripted.animal_looting && inside(t.translation) && actor.is_none_or(|a| !a.script_driven())
            && (!h.dead || carcass.is_some_and(|c| c.can_skin()))
    }).filter_map(|(_, _, a, _, _, _)| catalog.item(crate::progression::LootKind::Skin(a.animal())).map(|i| i.name_id))
        .chain(plants.iter().filter_map(|(p, t)| {
            let crate::pickups::PickupKind::Plant(plant) = p.kind else { return None };
            if !scripted.plant_looting || !inside(t.translation()) { return None }
            catalog.item(crate::progression::LootKind::Plant(plant)).map(|i| i.name_id)
        }));
    let quantities = catalog.quantities(&progress);
    let chosen = available.filter(|id| wanted.contains(id))
        .min_by_key(|id| (quantities.get(id).copied().unwrap_or(0), *id));
    let wants = |kind| catalog.item(kind).is_some_and(|i| Some(i.name_id) == chosen);
    // Take existing carcasses before shooting another animal. Script-driven
    // animals remain under their action/scene's ownership.
    let target = animals.iter().filter(|(_, at, animal, h, carcass, actor)| {
        wants(crate::progression::LootKind::Skin(animal.animal())) && scripted.animal_looting
            && inside(at.translation)
            && actor.is_none_or(|a| !a.script_driven())
            && (!h.dead || carcass.is_some_and(|c| c.can_skin()))
    }).min_by(|a, b| b.3.dead.cmp(&a.3.dead)
        .then(a.1.translation.distance(at.translation).total_cmp(&b.1.translation.distance(at.translation))))
        .map(|(e, t, _, h, _, actor)| (actor.map_or(e.to_bits(), |a| a.id), t.translation,
            if h.dead { "skin" } else { "hunt" }, if h.dead { 1.3 } else { 6.0 }));
    let target = target.or_else(|| plants.iter().filter_map(|(pickup, t)| {
        let crate::pickups::PickupKind::Plant(plant) = pickup.kind else { return None };
        (scripted.plant_looting && inside(t.translation()) && wants(crate::progression::LootKind::Plant(plant)))
            .then_some((pickup.id, t.translation() + Vec3::Y * crate::player::HALF_HEIGHT, "gather", 1.0))
    }).min_by(|a, b| a.1.distance(at.translation).total_cmp(&b.1.distance(at.translation))));
    let Some((id, target, kind, distance)) = target else { return };
    if *last != Some((id, kind)) {
        info!("story autopilot: {kind} {id}");
        *last = Some((id, kind));
    }
    let delta = target - at.translation;
    let range = Vec2::new(delta.x, delta.z).length();
    if range > if kind == "hunt" { 12.0 } else { 1.8 } || delta.y.abs() > 2.0 {
        let away = Vec3::new(-delta.x, 0.0, -delta.z).normalize_or(Vec3::Z);
        let mut feet = target + away * distance;
        feet.y = heights.height_at(feet).unwrap_or(target.y - crate::player::HALF_HEIGHT);
        if !inside(feet) { return }
        teleport(&mut at, &mut player, to_dunia(feet), 0.0);
        host.quiet = 0.0;
        return;
    }
    let eye = at.translation + Vec3::Y * (crate::player::EYE_HEIGHT - crate::player::HALF_HEIGHT);
    let aim = target - eye;
    player.yaw = (-aim.x).atan2(-aim.z);
    player.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length());
    use crate::controls::Action;
    if kind == "hunt" {
        if scripted.ignored.contains("updateshooting") { return }
        if inventory.slot().mag == 0 { actions.press(Action::Reload); }
        else { actions.hold(Action::Aim); actions.press(Action::Fire); }
    } else if !scripted.ignored.contains("use") {
        actions.press(Action::Interact);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::{Action, ActionState, Binding};
    use crate::settings::Settings;

    #[test]
    fn script_medpack_adjustments_obey_capacity_underflow_and_player_identity() {
        let player = fc3_story::PLAYER.to_string();
        let mut count = 0;
        let give = "CAPI_Player.GiveMedpacksToPlayer";
        let remove = "CAPI_Player.RemoveMedpacksToPlayer";
        assert!(apply_medpack_command(&mut count, 3, give, &[player.clone(), "2".into()]));
        assert_eq!(count, 2);
        assert!(apply_medpack_command(&mut count, 3, give, &[player.clone(), u32::MAX.to_string()]));
        assert_eq!(count, 3);
        assert!(apply_medpack_command(&mut count, 3, remove, &[player.clone(), "1".into()]));
        assert_eq!(count, 2);
        assert!(apply_medpack_command(&mut count, 3, remove, &[player.clone(), "5".into()]));
        assert_eq!(count, 0);
        for args in [vec!["123".into(), "2".into()], vec![player.clone(), "-1".into()], vec![player.clone(), "NaN".into()], vec![player]] {
            assert!(!apply_medpack_command(&mut count, 3, give, &args));
            assert_eq!(count, 0);
        }
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_medpack_controller_changes_inventory_without_recipe_events() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&std::fs::read(cache.join(fc3_formats::scripts::DOMINO_PACK_REL)).unwrap()).unwrap();
        let mut story = Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        let controller = story.domino().create("Domino/System/Player/ManageInventory_v4.lua").unwrap();
        controller.set("Finished", story.domino().lua().globals().get::<mlua::Function>("DummyFunction").unwrap()).unwrap();
        let mut count = 0;
        for (method, amount, expected) in [("AddMedPack", Some(2), 2), ("AddMedPack", None, 3), ("RemoveMedPack", Some(1), 2), ("RemoveMedPack", None, 0)] {
            controller.set("ammoCount", amount).unwrap();
            story.domino().call(&controller, method).unwrap();
            let commands = story.take_commands();
            assert_eq!(commands.len(), 1);
            let Command::Native { name, args } = &commands[0] else { panic!("expected a medical inventory command") };
            assert!(apply_medpack_command(&mut count, 3, name, args));
            assert_eq!(count, expected);
            let mut view = View::default();
            view.player_state.medpacks = count;
            story.update(0.0, view);
            assert_eq!(story.domino().lua().load(r#"return CAPI_Player.GetPlayerMedPackCount("1")"#).eval::<u32>().unwrap(), expected);
            assert!(story.take_commands().is_empty());
        }
        assert_eq!(story.errors(), 0);
    }

    #[test]
    fn paused_menu_events_are_retained_and_exit_callbacks_see_committed_crafts_once() {
        use crate::progression::{JournalPage, PauseMenu};
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        story.domino().lua().load(r#"
            menus = { exit = 0, opened = 0, crafting = 0, skills = 0, crafts = 0 }
            menus._type = {
                Exit = function(self) assert(self.crafts == 3); self.exit = self.exit + 1 end,
                Open = function(self) self.opened = self.opened + 1 end,
                Crafting = function(self) self.crafting = self.crafting + 1 end,
                Skills = function(self) self.skills = self.skills + 1 end,
                Crafted = function(self) self.crafts = self.crafts + 1 end
            }
            local c = CScriptCallbackSystem_GetInstance()
            c:RegisterGenericCallback(menus, "Exit", "CDominoExitPauseMenuEvent")
            c:RegisterGenericCallback(menus, "Open", "CDominoOpenPauseMenuEvent")
            c:RegisterGenericCallback(menus, "Crafting", "CDominoCraftingPageOpenedEvent")
            c:RegisterGenericCallback(menus, "Skills", "CDominoSkillPageOpenedEvent")
            c:RegisterEventCallback("1", menus, "Crafted", "DominoEvent_CraftRecipe")
        "#).exec().unwrap();
        fn resume(mut host: NonSendMut<StoryHost>, mut menu: ResMut<StoryMenuEvents>, mut crafts: ResMut<StoryCrafts>) {
            host.forward_crafts(&mut crafts);
            host.forward_menu_events(&mut menu);
        }
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).insert_resource(State::new(GameState::Playing)).init_resource::<PauseMenu>()
            .init_resource::<StoryMenuEvents>().init_resource::<StoryCrafts>().add_message::<crate::crafting::ItemCrafted>()
            .insert_non_send(StoryHost::new(story, None)).add_systems(PreUpdate, (collect_menu_events, collect_crafts))
            .add_systems(Update, resume.run_if(in_state(GameState::Playing)));
        app.update();
        app.insert_resource(State::new(GameState::Journal));
        app.update();
        for _ in 0..4 { app.update(); }
        assert_eq!(app.world().resource::<StoryMenuEvents>().0, ["CDominoOpenPauseMenuEvent", "CDominoCraftingPageOpenedEvent"]);
        app.world_mut().resource_mut::<PauseMenu>().page = JournalPage::Skills;
        app.update();
        for label in ["Medkit small", "Medkit small", "Loot pouch small"] {
            app.world_mut().write_message(crate::crafting::ItemCrafted { kind: if label == "Medkit small" { 3 } else { 2 }, label: label.into() });
            app.update();
        }
        for _ in 0..5 { app.update(); }
        app.insert_resource(State::new(GameState::Playing));
        app.update();
        let menus = app.world().non_send::<StoryHost>().story.domino().lua().globals().get::<mlua::Table>("menus").unwrap();
        assert_eq!(menus.get::<u32>("opened").unwrap(), 1);
        assert_eq!(menus.get::<u32>("crafting").unwrap(), 1);
        assert_eq!(menus.get::<u32>("skills").unwrap(), 1);
        assert_eq!(menus.get::<u32>("exit").unwrap(), 1);
        for _ in 0..4 { app.update(); }
        assert_eq!(menus.get::<u32>("exit").unwrap(), 1);
        assert!(app.world().resource::<StoryMenuEvents>().0.is_empty());
        assert_eq!(app.world().non_send::<StoryHost>().story.errors(), 0);
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_ui_listener_receives_menu_page_and_exit_events_once() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&std::fs::read(cache.join(fc3_formats::scripts::DOMINO_PACK_REL)).unwrap()).unwrap();
        let story = Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        let listener = story.domino().create("Domino/System/UI/UIListener.lua").unwrap();
        let lua = story.domino().lua();
        for output in ["Enabled", "Disabled", "OpenPhotoMap"] {
            listener.set(output, lua.globals().get::<mlua::Function>("DummyFunction").unwrap()).unwrap();
        }
        for (output, field) in [("OpenWorldMap", "map"), ("OpenPauseMenu", "open"), ("OpenCraftingPage", "crafting"), ("OpenSkillPage", "skills"), ("ExitPauseMenu", "exit")] {
            listener.set(output, lua.load(format!("return function(self) self.{field} = (self.{field} or 0) + 1 end")).eval::<mlua::Function>().unwrap()).unwrap();
        }
        story.domino().call(&listener, "Enable").unwrap();
        let mut host = StoryHost::new(story, None);
        let mut events = StoryMenuEvents(vec!["CDominoWorldMapOpenedEvent", "CDominoOpenPauseMenuEvent", "CDominoCraftingPageOpenedEvent",
            "CDominoSkillPageOpenedEvent", "CDominoExitPauseMenuEvent"]);
        host.forward_menu_events(&mut events);
        host.forward_menu_events(&mut events);
        for field in ["map", "open", "crafting", "skills", "exit"] { assert_eq!(listener.get::<u32>(field).unwrap(), 1); }
        host.story.generic_event("CDominoExitPauseMenuEvent", vec![]);
        assert_eq!(listener.get::<u32>("exit").unwrap(), 1, "authored auto-disable must unregister the exit callback");
        assert_eq!(host.story.errors(), 0);
    }

    #[test]
    fn crafted_items_survive_journal_frames_and_reach_the_story_once() {
        use crate::crafting::ItemCrafted;
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        story.domino().lua().load(r#"
            crafts = { count = 0 }
            crafts._type = { Got = function(self, kind, name)
                assert(kind == 3 and name == "Medkit small")
                self.count = self.count + 1
            end }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("1", crafts, "Got", "DominoEvent_CraftRecipe")
        "#).exec().unwrap();
        fn forward(mut host: NonSendMut<StoryHost>, mut pending: ResMut<StoryCrafts>) { host.forward_crafts(&mut pending); }
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).insert_resource(State::new(GameState::Journal))
            .insert_non_send(StoryHost::new(story, None)).init_resource::<StoryCrafts>()
            .add_message::<ItemCrafted>().add_systems(PreUpdate, collect_crafts)
            .add_systems(Update, forward.run_if(in_state(GameState::Playing)));
        app.world_mut().write_message(ItemCrafted { kind: 3, label: "Medkit small".into() });
        for _ in 0..8 { app.update(); }
        assert_eq!(app.world().resource::<StoryCrafts>().0.len(), 1);
        let count = |app: &App| app.world().non_send::<StoryHost>().story.domino().lua().globals()
            .get::<mlua::Table>("crafts").unwrap().get::<u32>("count").unwrap();
        assert_eq!(count(&app), 0);
        app.insert_resource(State::new(GameState::Playing));
        for _ in 0..4 { app.update(); }
        assert_eq!(count(&app), 1);
        assert!(app.world().resource::<StoryCrafts>().0.is_empty());
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_crafting_listener_receives_authored_type_and_recipe_name() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&std::fs::read(cache.join(fc3_formats::scripts::DOMINO_PACK_REL)).unwrap()).unwrap();
        let story = Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        let listener = story.domino().create("Domino/System/Player/CraftingListener.lua").unwrap();
        let lua = story.domino().lua();
        for output in ["Enabled", "Disabled"] {
            listener.set(output, lua.globals().get::<mlua::Function>("DummyFunction").unwrap()).unwrap();
        }
        listener.set("typeFilter", 3).unwrap();
        listener.set("nameFilter", "Medkit small").unwrap();
        listener.set("OnCrafted", lua.load("return function(self) self.received = (self.received or 0) + 1 end")
            .eval::<mlua::Function>().unwrap()).unwrap();
        story.domino().call(&listener, "Enable").unwrap();
        let mut host = StoryHost::new(story, None);
        let mut pending = StoryCrafts(vec![crate::crafting::ItemCrafted { kind: 3, label: "Medkit small".into() }]);
        host.forward_crafts(&mut pending);
        assert_eq!(listener.get::<u32>("received").unwrap(), 1);
        host.forward_crafts(&mut pending);
        assert_eq!(listener.get::<u32>("received").unwrap(), 1);
        assert_eq!(host.story.errors(), 0);
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_loot_listener_receives_the_authored_three_argument_event() {
        use crate::progression::{ItemLooted, LootCatalog, LootKind, Progress, Animal};
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&std::fs::read(cache.join(fc3_formats::scripts::DOMINO_PACK_REL)).unwrap()).unwrap();
        let story = Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        let listener = story.domino().create("Domino/System/LootItemListener.lua").unwrap();
        let lua = story.domino().lua();
        for output in ["Enabled", "Disabled"] {
            listener.set(output, lua.globals().get::<mlua::Function>("DummyFunction").unwrap()).unwrap();
        }
        let filter = lua.create_table().unwrap();
        filter.set("section", "Name").unwrap();
        filter.set("id", "126007").unwrap();
        listener.set("filterItemID", filter).unwrap();
        listener.set("ItemLooted", lua.load(r#"return function(self)
            assert(self.lootedItemID.id == "126007")
            assert(CAPI_Pawn.GetLootQuantity("1", "126007") == 2)
            self.received = (self.received or 0) + 1
        end"#).eval::<mlua::Function>().unwrap()).unwrap();
        story.domino().call(&listener, "Enable").unwrap();
        let metadata = fc3_formats::progression::Crafting::from_text(&std::fs::read_to_string(cache.join("progression/recipes.txt")).unwrap()).unwrap();
        let catalog = LootCatalog::from_metadata(&metadata);
        let mut host = StoryHost::new(story, None);
        let mut progress = Progress::default();
        progress.skins.insert(Animal::Boar, 2);
        let mut view = View::default();
        view.player_state.loot = catalog.quantities(&progress);
        host.story.update(0.0, view);
        let mut pending = StoryLoot(vec![ItemLooted(LootKind::Skin(Animal::Boar))]);
        host.forward_loot(&mut pending, &catalog);
        assert_eq!(listener.get::<u32>("received").unwrap(), 1);
        host.forward_loot(&mut pending, &catalog);
        assert_eq!(listener.get::<u32>("received").unwrap(), 1);
        host.story.domino().call(&listener, "Disable").unwrap();
        assert!(host.story.watched_loot_filters().is_empty());
        assert_eq!(host.story.errors(), 0);
    }

    #[test]
    fn gathered_loot_survives_paused_frames_and_callbacks_read_the_updated_inventory_once() {
        use crate::progression::{ItemLooted, LootCatalog, LootKind, Progress, Animal};
        use crate::syringes::Plant;
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        story.domino().lua().load(r#"
            loot = { count = 0, quantities = {} }
            loot._type = { Got = function(self, item, field, id)
                assert(item ~= nil and item ~= "Name")
                assert(field == "Name")
                self.count = self.count + 1
                self.quantities[tonumber(id)] = CAPI_Pawn.GetLootQuantity("1", id)
            end }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("1", loot, "Got", "CPawnLootItemEvent")
        "#).exec().unwrap();
        fn forward(mut host: NonSendMut<StoryHost>, mut pending: ResMut<StoryLoot>, catalog: Res<LootCatalog>, progress: Res<Progress>) {
            let mut view = View::default();
            view.player_state.loot = catalog.quantities(&progress);
            host.story.update(0.0, view);
            host.forward_loot(&mut pending, &catalog);
        }
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(State::new(GameState::Journal))
            .insert_non_send(StoryHost::new(story, None))
            .insert_resource(LootCatalog::from_metadata(&crate::progression::loot_tests::metadata()))
            .init_resource::<Progress>()
            .init_resource::<StoryLoot>()
            .add_message::<ItemLooted>()
            .add_systems(PreUpdate, collect_loot)
            .add_systems(Update, forward.run_if(in_state(GameState::Playing)));
        app.world_mut().resource_mut::<Progress>().skins.insert(Animal::Boar, 2);
        app.world_mut().resource_mut::<Progress>().plants[Plant::ScrewPine.index()] = 1;
        for kind in [LootKind::Skin(Animal::Boar), LootKind::Plant(Plant::ScrewPine), LootKind::Skin(Animal::Chicken)] {
            app.world_mut().write_message(ItemLooted(kind));
        }
        let count = |app: &App| app.world().non_send::<StoryHost>().story.domino().lua()
            .globals().get::<mlua::Table>("loot").unwrap().get::<u32>("count").unwrap();
        for _ in 0..8 { app.update(); }
        assert_eq!(count(&app), 0);
        assert_eq!(app.world().resource::<StoryLoot>().0.len(), 3);
        app.insert_resource(State::new(GameState::Playing));
        app.update();
        assert_eq!(count(&app), 2);
        assert!(app.world().resource::<StoryLoot>().0.is_empty());
        let quantities = app.world().non_send::<StoryHost>().story.domino().lua().globals()
            .get::<mlua::Table>("loot").unwrap().get::<mlua::Table>("quantities").unwrap();
        assert_eq!(quantities.get::<u32>(900).unwrap(), 2);
        assert_eq!(quantities.get::<u32>(901).unwrap(), 1);
        for _ in 0..4 { app.update(); }
        assert_eq!(count(&app), 2);
        // Consumed loot leaves the inventory without fabricating another event.
        app.world_mut().resource_mut::<Progress>().skins.clear();
        app.update();
        assert_eq!(app.world().non_send::<StoryHost>().story.domino().lua()
            .load(r#"return CAPI_Pawn.GetLootQuantity("1", "900")"#).eval::<u32>().unwrap(), 0);
        assert_eq!(count(&app), 2);
    }

    #[test]
    fn a_shop_purchase_survives_paused_frames_and_reaches_the_mission_once() {
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        story.domino().lua().load(r#"
            purchase = { count = 0 }
            purchase._type = { Got = function(self, archetype, field, id)
                self.count = self.count + 1
                self.archetype, self.field, self.id = archetype, field, id
            end }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("1", purchase, "Got", "CPawnBuyItemEvent")
        "#).exec().unwrap();
        let mut host = StoryHost::new(story, None);
        host.shop_items.insert("pistol.xbg".into(), ("weapons:TestPistol".into(), 999));
        fn forward(mut host: NonSendMut<StoryHost>, mut pending: ResMut<StoryPurchases>) {
            host.forward_purchases(&mut pending);
        }
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(State::new(GameState::Shop))
            .insert_non_send(host)
            .init_resource::<StoryPurchases>()
            .add_message::<crate::progression::ItemBought>()
            .add_systems(PreUpdate, collect_shop_purchases)
            .add_systems(Update, forward.run_if(in_state(GameState::Playing)));
        app.world_mut().write_message(crate::progression::ItemBought { model: Some("pistol.xbg".into()) });
        app.world_mut().write_message(crate::progression::ItemBought { model: None });
        for _ in 0..8 { app.update(); }
        assert_eq!(app.world().resource::<StoryPurchases>().0.len(), 2);
        let count = |app: &App| app.world().non_send::<StoryHost>().story.domino().lua()
            .globals().get::<mlua::Table>("purchase").unwrap().get::<u32>("count").unwrap();
        assert_eq!(count(&app), 0);
        app.insert_resource(State::new(GameState::Playing));
        app.update();
        assert!(app.world().resource::<StoryPurchases>().0.is_empty());
        assert_eq!(count(&app), 1);
        let purchase = app.world().non_send::<StoryHost>().story.domino().lua().globals().get::<mlua::Table>("purchase").unwrap();
        assert_eq!(purchase.get::<String>("archetype").unwrap(), "weapons:TestPistol");
        assert_eq!(purchase.get::<String>("field").unwrap(), "Name");
        assert_eq!(purchase.get::<String>("id").unwrap(), "999");
        for _ in 0..4 { app.update(); }
        assert_eq!(count(&app), 1);
    }

    #[test]
    fn campaign_prompts_follow_rebinding_and_device() {
        let mut settings = Settings::default();
        settings.rebind(Action::Interact, Binding::Key(KeyCode::KeyV));
        let mut actions = ActionState::default();
        assert_eq!(prompt_text("[Interact] Use — [ThrowRock] Distract [unknown] [unfinished", &actions, &settings),
            "[V] Use — [MMB] Distract [unknown] [unfinished");
        actions.gamepad_active = true;
        assert_eq!(prompt_text("[Interact] Use / [Crouch] Hide", &actions, &settings), "[X] Use / [B] Hide");
        assert_eq!(prompt_text("[WorldMap] Map / [MapFastTravel] Travel", &actions, &settings), "[Select] Map / [A] Travel");
    }

    #[test]
    fn mission_timer_rounding_and_zero() {
        let mut timer = fc3_story::Timer { id: 1, label: "Escape".into(), seconds: 60.2, increasing: false, append_time: true };
        assert_eq!(timer_text(&timer), "Escape  1:01");
        timer.increasing = true;
        assert_eq!(timer_text(&timer), "Escape  1:00");
        timer.seconds = -5.0;
        assert_eq!(timer_text(&timer), "Escape  0:00");
        timer.seconds = 3601.0;
        assert_eq!(timer_text(&timer), "Escape  1:00:01");
        timer.append_time = false;
        assert_eq!(timer_text(&timer), "Escape");
    }

    #[test]
    fn script_inventory_uses_archetypes_and_excludes_empty_hands() {
        let defs = crate::weapons::weapon_defs();
        let index = |name| defs.iter().position(|d| d.name == name).unwrap();
        let slots = vec![
            crate::weapons::WeaponSlot { def: index("Machete"), mag: 0, reserve: 0 },
            crate::weapons::WeaponSlot { def: index("1911"), mag: 7, reserve: 21 },
            crate::weapons::WeaponSlot { def: index(crate::weapons::UNARMED), mag: 0, reserve: 0 },
        ];
        let shop = [(defs[index("1911")].model_path(), ("weapons:FC3/Kimber".to_string(), 123))].into_iter().collect();
        let (items, held) = story_weapons(&defs, &slots, 1, &shop);
        assert_eq!(items.len(), 2);
        assert_eq!(held.as_deref(), Some("weapons:FC3/Kimber"));
        assert_eq!(items[1].count, 1);
        assert_eq!(items[1].category, "handgun");
        assert!(story_weapons(&defs, &slots, 2, &shop).1.is_none());
    }

    #[test]
    fn mission_inventory_backup_restores_gadgets_ammo_and_the_selected_weapon_once() {
        let defs = crate::weapons::weapon_defs();
        let machete = defs.iter().position(|d| d.name == "Machete").unwrap();
        let pistol = defs.iter().position(|d| d.name == "1911").unwrap();
        let slots = vec![
            crate::weapons::WeaponSlot { def: machete, mag: 0, reserve: 0 },
            crate::weapons::WeaponSlot { def: pistol, mag: 4, reserve: 19 },
        ];
        let mut inv = crate::weapons::Inventory {
            defs, models: Vec::new(), slots, current: 1,
            grenades: 2, molotovs: 1, c4: 3, mines: 2,
            cooldown: 0.0, reloading: 0.0, switching: 0.0, aiming: false,
            aim_blend: 0.0, recoil_kick: 0.0, spread_bloom: 0.0, shots_fired: 0,
            recoil_offset: 0.0, since_shot: 0.0, reserve_scale: 1.0,
            attachments: Vec::new(), attachment_models: Vec::new(), mounted_from: None,
            auto_tail: 0, arrow_kind: 0, special_arrows: [0; 2],
            paints: Vec::new(), paint_options: Vec::new(),
        };
        let mut scripted = ScriptedPlayer::default();
        scripted.backup_inventory(&inv);
        scripted.gadgets.clear();
        inv.slots.clear();
        inv.current = 0;
        (inv.grenades, inv.molotovs, inv.c4, inv.mines) = (0, 0, 0, 0);
        crate::weapons::settle_unarmed(&mut inv);
        scripted.restore_inventory(&mut inv);
        assert!(scripted.has_gadget("gadgets:FC3/Rock"));
        assert!(scripted.has_gadget("gadgets:Equipped/Tablet"));
        assert_eq!(inv.slots.len(), 2);
        assert_eq!(inv.current, 1);
        assert_eq!((inv.slots[1].def, inv.slots[1].mag, inv.slots[1].reserve), (pistol, 4, 19));
        assert_eq!((inv.grenades, inv.molotovs, inv.c4, inv.mines), (2, 1, 3, 2));
        scripted.gadgets.clear();
        inv.slots[1].mag = 1;
        scripted.restore_inventory(&mut inv);
        assert!(scripted.gadgets.is_empty());
        assert_eq!(inv.slots[1].mag, 1);
    }

    fn modal_app() -> App {
        let mut app = App::new();
        app.init_resource::<StoryUi>().init_resource::<ActionState>()
            .init_resource::<Settings>().init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<Time<Virtual>>()
            .insert_resource(State::new(GameState::Playing))
            .add_systems(Update, story_boxes);
        app.world_mut().resource_mut::<StoryUi>().boxes.push_back(StoryBox {
            id: 1, title: "Tutorial".into(), body: "[Interact] Continue".into(), choice: false, prompt: None,
        });
        app
    }

    #[test]
    fn closing_tutorial_does_not_unpause_pause_menu() {
        let mut app = modal_app();
        app.update();
        assert!(app.world().resource::<Time<Virtual>>().is_paused());
        app.world_mut().resource_mut::<StoryUi>().boxes.clear();
        app.insert_resource(State::new(GameState::Paused));
        app.update();
        assert!(app.world().resource::<Time<Virtual>>().is_paused());
    }

    #[test]
    fn tutorial_consumes_interact_and_resumes_after_closing() {
        let mut app = modal_app();
        app.world_mut().resource_mut::<Settings>().rebind(Action::Interact, Binding::Key(KeyCode::KeyV));
        app.update();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyV);
        app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
        app.update();
        assert!(app.world().resource::<StoryUi>().boxes.is_empty());
        assert!(!app.world().resource::<ActionState>().just_pressed(Action::Interact));
        app.update();
        assert!(!app.world().resource::<Time<Virtual>>().is_paused());
    }

    #[test]
    fn holding_interact_does_not_dismiss_the_next_tutorial() {
        let mut app = modal_app();
        app.world_mut().resource_mut::<StoryUi>().boxes.push_back(StoryBox {
            id: 2, title: "Next".into(), body: "Release and press again".into(), choice: false, prompt: None,
        });
        app.update();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyE);
        app.update();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
        app.update();
        assert_eq!(app.world().resource::<StoryUi>().boxes.front().unwrap().id, 2);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().release(KeyCode::KeyE);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyE);
        app.update();
        assert!(app.world().resource::<StoryUi>().boxes.is_empty());
    }

    #[test]
    fn scripted_prop_teleports_preserve_scale_and_apply_once_per_revision() {
        let mut app = App::new();
        app.init_resource::<EntityPlacements>().init_resource::<crate::nav::NavMesh>()
            .add_systems(Update, apply_entity_poses);
        let parent_pose = Transform::from_xyz(10.0, 0.0, 0.0).with_rotation(Quat::from_rotation_y(0.5));
        let parent = app.world_mut().spawn((parent_pose, GlobalTransform::from(parent_pose))).id();
        let root = app.world_mut().spawn((crate::objects::PlacedId(10), Transform::default().with_scale(Vec3::splat(2.0)), ChildOf(parent))).id();
        app.world_mut().resource_mut::<EntityPlacements>().set(10, fc3_story::Pose { position: [20.0, 30.0, 5.0], angles: [0.0; 3] });
        app.update();
        let local = *app.world().get::<Transform>(root).unwrap();
        assert_eq!(local.scale, Vec3::splat(2.0));
        assert!((parent_pose.transform_point(local.translation) - to_bevy([20.0, 30.0, 5.0])).length() < 1e-4);
        app.world_mut().get_mut::<Transform>(root).unwrap().translation = Vec3::ZERO;
        app.update();
        assert_eq!(app.world().get::<Transform>(root).unwrap().translation, Vec3::ZERO);
        // An identical later teleport is a fresh command, not a no-op.
        app.world_mut().resource_mut::<EntityPlacements>().set(10, fc3_story::Pose { position: [20.0, 30.0, 5.0], angles: [0.0; 3] });
        app.update();
        assert!((app.world().get::<Transform>(root).unwrap().translation - local.translation).length() < 1e-4);
        let streamed = app.world_mut().spawn((crate::objects::PlacedId(10), Transform::default())).id();
        app.update();
        assert_eq!(app.world().get::<Transform>(streamed).unwrap().translation, to_bevy([20.0, 30.0, 5.0]));
    }

    #[test]
    fn mission_use_hold_cancels_on_release_or_target_change_and_requires_a_fresh_press() {
        let mut hold = StoryInteraction::default();
        assert!(!hold.advance(Some(10), true, 0.3));
        assert!(!hold.advance(Some(20), true, 0.3));
        assert!(hold.advance(Some(20), true, 0.2));
        assert!(!hold.advance(Some(20), true, 1.0));
        assert!(!hold.advance(Some(30), true, 1.0));
        assert!(!hold.advance(None, false, 0.0));
        assert!(!hold.advance(Some(20), true, 0.0));
        assert!(hold.advance(Some(20), true, USE_HOLD_SECONDS));
        hold.advance(None, false, 0.0);
        hold.advance(Some(20), true, 0.3);
        hold.advance(None, true, 0.2);
        assert!(!hold.advance(Some(20), true, 0.3));
    }

    #[test]
    fn scripted_collision_control_survives_streaming_and_can_be_reenabled() {
        use avian3d::prelude::{Collider, ColliderDisabled};
        let mut app = App::new();
        app.init_resource::<EntityPlacements>().add_systems(Update, apply_entity_physics);
        app.world_mut().resource_mut::<EntityPlacements>().set_physics(10, false);
        // The disable can arrive before either the entity or its collider.
        let root = app.world_mut().spawn(crate::objects::PlacedId(10)).id();
        app.update();
        assert!(app.world().get::<ColliderDisabled>(root).is_some());
        app.world_mut().entity_mut(root).insert(Collider::cuboid(1.0, 1.0, 1.0));
        app.update();
        assert!(app.world().get::<ColliderDisabled>(root).is_some());
        let later = app.world_mut().spawn((crate::objects::PlacedId(10), Collider::cuboid(1.0, 1.0, 1.0))).id();
        app.update();
        assert!(app.world().get::<ColliderDisabled>(later).is_some());
        app.world_mut().resource_mut::<EntityPlacements>().set_physics(10, true);
        app.update();
        assert!(app.world().get::<ColliderDisabled>(root).is_none());
        assert!(app.world().get::<ColliderDisabled>(later).is_none());
    }
}
