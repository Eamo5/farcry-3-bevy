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
            .add_systems(Update, stream_world_scripts.after(StorySet).run_if(in_state(GameState::Playing)))
            .add_systems(Update, apply_entity_poses.after(StorySet).in_set(EntityPosesSet))
            .add_systems(Update, apply_entity_physics.after(StorySet).in_set(EntityPosesSet))
            .add_systems(Update, apply_entity_scales.after(apply_entity_poses).in_set(EntityPosesSet))
            .add_systems(Update, draw_markers.after(StorySet))
            .add_systems(Update, (weapon_events, shot_events).after(StorySet).run_if(in_state(GameState::Playing)))
            .add_systems(Update, (player_landings, autopilot_drop).after(StorySet).run_if(in_state(GameState::Playing)))
            .add_systems(Update, story_blasts.after(StorySet).before(crate::breakables::BreakablesSet))
            .add_systems(Update, (story_damage, story_searches).after(StorySet))
            .add_systems(PreUpdate, story_boxes.after(crate::controls::update_actions))
            .add_systems(PreUpdate, story_interactions.after(story_boxes))
            .add_systems(PreUpdate, autopilot_loot.after(story_interactions).before(crate::controls::apply_action_maps).run_if(in_state(GameState::Playing)))
            .add_systems(PreUpdate, collect_shop_purchases)
            .add_systems(PreUpdate, collect_loot)
            .add_systems(Update, forward_pickups)
            .add_systems(Update, forward_camera)
            .add_systems(PreUpdate, collect_crafts)
            .add_systems(PreUpdate, collect_menu_events)
            .add_systems(Startup, setup_story_ui)
            .add_systems(Update, fade_screen)
            .add_systems(Update, start_story.after(crate::save::continue_game).run_if(in_state(GameState::Playing)))
            .add_systems(Update, run_story.in_set(StorySet).run_if(|state: Res<State<GameState>>| {
                matches!(state.get(), GameState::Playing | GameState::Paused | GameState::Map | GameState::Journal | GameState::Shop)
            }))
            .add_systems(Update, show_story_ui)
            .add_systems(Update, (apply_sides, apply_tags, scripted_controls, scripted_ambience, apply_damage_rates).after(StorySet));
        if std::env::var_os("FC3_STORY_RETRY_TEST").is_some() {
            app.add_systems(Update, retry_inventory_test.after(StorySet).run_if(in_state(GameState::Playing)));
        }
    }
}

/// The sides scripts choose: each character's army (`CAPI_NPC.SwitchArmy`):
/// red the player's foes (back to its own side, if it was his), grey his
/// side, neutral out of the fight (G3_SC_05's sitting ducks until the player
/// comes near: their reinforcement points', passed to the mercs they call
/// in; blue and green aren't the player's business); and the throwable they
/// choose for the HUD.
fn apply_sides(
    host: Option<NonSendMut<StoryHost>>,
    mut npcs: Query<(&crate::story_actors::StoryActor, &mut crate::npc::Npc)>,
    mut thrown: ResMut<crate::controls::ThrowChoice>,
    mut orders: ResMut<crate::barks::OrderCalls>,
) {
    use crate::npc::Faction;
    let Some(mut host) = host else { return };
    if let Some(choice) = host.throw_choice.take() {
        thrown.0 = Some(choice);
    }
    orders.0.append(&mut host.order_calls);
    // Allies sent to a battlefield take up their posts round its point
    // (each its own, a few metres off).
    if !host.battlefield_posts.is_empty() {
        let data = host.story.data();
        for (actor, mut npc) in &mut npcs {
            let Some(at) = host.battlefield_posts.get(&actor.id).and_then(|s| data.entities.get(s)).map(|s| to_bevy(s.pos)) else { continue };
            let turn = (actor.id % 360) as f32 * std::f32::consts::TAU / 360.0;
            let post = at + Vec3::new(turn.cos(), 0.0, turn.sin()) * (2.0 + (actor.id % 5) as f32);
            if npc.post.distance(post) > 0.5 {
                debug!("story: {} posted at its battlefield {post}", actor.id);
                npc.post = post;
                npc.post_yaw = turn;
                npc.at_spot = false;
            }
        }
    }
    // Whom scripts set them against or beside (SQC_010's rebels turned on
    // the player, CIT_020's held back from the pirates a while).
    if !host.relations.is_empty() {
        for (actor, mut npc) in &mut npcs {
            let Some(&relations) = host.relations.get(&actor.id) else { continue };
            if npc.relations != relations {
                let was = npc.hostile();
                npc.relations = relations;
                debug!("story: {} relations {relations:?}", actor.id);
                // (Its fight looked at afresh: a foe it now spares is let be.)
                npc.foe = None;
                npc.foe_seen = false;
                if was && !npc.hostile() {
                    npc.awareness = 0.0;
                    npc.state = crate::npc::NpcState::Idle;
                }
            }
        }
    }
    // Cover looked for afresh.
    if !host.recover.is_empty() {
        let recover = std::mem::take(&mut host.recover);
        for (actor, mut npc) in &mut npcs {
            if recover.contains(&actor.id) {
                npc.reevaluate_cover();
            }
        }
    }
    // Senses a script holds: a captive tied up forgets the fight.
    if !host.impaired.is_empty() {
        for (actor, mut npc) in &mut npcs {
            let Some(&state) = host.impaired.get(&actor.id) else { continue };
            let impaired = crate::npc::Impaired::from_state(state);
            if npc.impaired != impaired {
                debug!("story: {} impaired {impaired:?}", actor.id);
                npc.impaired = impaired;
                if impaired == crate::npc::Impaired::Fully {
                    npc.foe = None;
                    npc.awareness = 0.0;
                    npc.state = crate::npc::NpcState::Idle;
                }
            }
        }
    }
    if host.armies.is_empty() {
        return;
    }
    for (actor, mut npc) in &mut npcs {
        // (Its own, else its reinforcement point's.)
        let army = host.armies.get(&actor.id).or_else(|| host.called_from.get(&actor.id).and_then(|p| host.armies.get(p)));
        let Some((faction, neutral)) = army.and_then(|&a| side_for(a, npc.faction)) else { continue };
        if npc.faction != faction || npc.neutral != neutral {
            info!(
                "story: {} now {}",
                actor.id,
                if neutral {
                    "neutral"
                } else if faction == Faction::Rebel {
                    "on the player's side"
                } else {
                    "the player's foe"
                }
            );
            if npc.faction != faction {
                npc.armies = crate::npc::Armies::of(faction);
            }
            npc.faction = faction;
            npc.neutral = neutral;
            // (Out of the fight: what it was after is forgotten.)
            if neutral {
                npc.foe = None;
                npc.awareness = 0.0;
                npc.state = crate::npc::NpcState::Idle;
            }
        }
    }
}

/// The tags scripts give and take (`TaggingModifier`): a character kept from
/// being tagged loses its tag and the camera passes it by; one tagged
/// outright (WIL_050's guards round Oliver) is marked as if the player had.
fn apply_tags(
    host: Option<NonSendMut<StoryHost>>,
    actors: Query<(Entity, &crate::story_actors::StoryActor, Has<crate::stealth::Tagged>, Has<crate::stealth::NoTag>)>,
    mut commands: Commands,
) {
    let Some(mut host) = host else { return };
    if host.taggable.is_empty() && host.tag_now.is_empty() {
        return;
    }
    for (e, actor, tagged, kept) in &actors {
        let allowed = host.taggable.get(&actor.id).copied().unwrap_or(true);
        if !allowed && !kept {
            commands.entity(e).insert(crate::stealth::NoTag).remove::<crate::stealth::Tagged>();
        } else if allowed && kept {
            commands.entity(e).remove::<crate::stealth::NoTag>();
        }
        if allowed && host.tag_now.remove(&actor.id) && !tagged {
            commands.entity(e).insert(crate::stealth::Tagged);
        }
    }
}

/// The side an army (`CAPI_NPC.SwitchArmy`'s number) puts a character of a
/// faction on, and whether it is out of the fight: red the player's foes
/// (a pirate, if it was on his side), grey his side, neutral as it was but
/// out of it; none for blue and green.
fn side_for(army: u8, faction: crate::npc::Faction) -> Option<(crate::npc::Faction, bool)> {
    use crate::npc::Faction;
    match army {
        1 if faction == Faction::Rebel => Some((Faction::Pirate, false)),
        1 => Some((faction, false)),
        2 => Some((Faction::Rebel, false)),
        3 => Some((faction, true)),
        _ => None,
    }
}

#[cfg(test)]
mod side_tests {
    use super::side_for;
    use crate::npc::Faction;

    #[test]
    fn armies_put_characters_on_sides() {
        assert_eq!(side_for(3, Faction::Pirate), Some((Faction::Pirate, true)));
        assert_eq!(side_for(1, Faction::Privateer), Some((Faction::Privateer, false)));
        assert_eq!(side_for(1, Faction::Rebel), Some((Faction::Pirate, false)));
        assert_eq!(side_for(2, Faction::Pirate), Some((Faction::Rebel, false)));
        assert_eq!(side_for(0, Faction::Pirate), None);
    }
}

/// The story's frame; systems acting on what it asked run after it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorySet;

/// Sector-owned Domino graphs include outpost flag controllers and their
/// authored unlock cinematics. Start them only after their object sector is
/// present; the runtime tears them down when the sector or layer leaves.
fn stream_world_scripts(
    host: Option<NonSendMut<StoryHost>>,
    sectors: Query<&crate::objects::SectorObjects>,
    time: Res<Time>,
    mut remaining: Local<f32>,
) {
    *remaining -= time.delta_secs();
    if *remaining > 0.0 { return; }
    *remaining = 0.5;
    let Some(mut host) = host else { return };
    let loaded = sectors.iter().map(|s| s.coord.id()).collect();
    host.story.stream_scripts(Some(&loaded));
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityPosesSet;

/// Opt-in fault injection: exercise combat death and the ordinary mission
/// retry, without pretending this is uninterrupted player-input evidence.
fn retry_inventory_test(
    time: Res<Time>,
    host: Option<NonSend<StoryHost>>,
    mut stage: Local<(u8, f32)>,
    mut players: Query<(Entity, &crate::combat::Health, &mut crate::combat::PlayerStats, &mut crate::weapons::Inventory), With<Player>>,
    mut progress: ResMut<crate::progression::Progress>,
    menu: Res<crate::progression::PauseMenu>,
    mut damage: MessageWriter<crate::combat::DamageEvent>,
    ambient: Query<Entity, (With<crate::wildlife::WildAnimal>, Without<crate::story_actors::StoryActor>)>,
    mut previous_wildlife: Local<Vec<Entity>>,
) {
    let Some(host) = host else { return };
    let Some(save) = &host.checkpoint_game else { return };
    let Ok((entity, health, mut stats, mut inv)) = players.single_mut() else { return };
    let now = time.elapsed_secs();
    if stage.0 == 0 && now > 12.0 && !health.dead {
        if std::env::var("FC3_STORY_RETRY_WILDLIFE_TEST").is_ok() && ambient.is_empty() {
            assert!(now < 30.0, "retry test: no ambient wildlife spawned to validate cleanup");
            return;
        }
        *previous_wildlife = ambient.iter().collect();
        for slot in &mut inv.slots { slot.mag = 0; slot.reserve = 0; }
        inv.grenades = 0;
        stats.medkits = 0;
        progress.money = 0;
        progress.plants.fill(0);
        damage.write(crate::combat::DamageEvent {
            target: entity, amount: 100000.0, kind: crate::combat::DamageKind::Fall,
            source: None, instigator: None, headshot: false,
            hit: crate::combat::Hit::Whole,
        });
        *stage = (1, now);
        info!("retry test: spent carried supplies and requested lethal combat damage");
    } else if stage.0 == 1 || stage.0 == 2 {
        assert!(now - stage.1 < 30.0, "retry test: normal checkpoint reload did not complete");
        if health.dead { stage.0 = 2; }
        if stage.0 == 1 { return; }
        if now - stage.1 < 8.0 || health.dead || host.was_dead || host.reload_at.is_some() { return; }
        let mut actual: Vec<_> = inv.slots.iter().filter(|s| inv.defs[s.def].name != crate::weapons::UNARMED)
            .map(|s| (inv.defs[s.def].name.to_string(), s.mag, s.reserve)).collect();
        let mut expected = save.weapons.clone();
        expected.retain(|s| s.0 != crate::weapons::UNARMED);
        actual.sort(); expected.sort();
        assert_eq!(actual, expected, "retry test: carried ammunition");
        assert_eq!(stats.medkits, save.medkits, "retry test: medkits");
        assert_eq!(inv.grenades, save.grenades, "retry test: grenades");
        assert_eq!(*progress, save.progress, "retry test: progression and ingredients");
        assert_eq!(menu.tutorial, save.craft_tutorial, "retry test: crafting tutorial");
        assert!(previous_wildlife.iter().all(|e| !ambient.contains(*e)), "retry test: previous attempt's ambient wildlife survived");
        stage.0 = 3;
        info!("retry test: PASS exact ammunition, supplies, progression and crafting tutorial restored after combat death");
        info!("retry test: cleared {} previous ambient wildlife entities", previous_wildlife.len());
    }
}

/// Scripted placements survive streaming, so a character teleported before
/// its models load spawns at the destination rather than its editor position.
#[derive(Resource, Default)]
pub struct EntityPlacements {
    poses: HashMap<u64, (u64, Transform)>,
    revision: u64,
    physics: HashMap<u64, (u64, bool)>,
    scales: HashMap<u64, f32>,
}

impl EntityPlacements {
    pub fn pose(&self, id: u64) -> Option<Transform> {
        self.poses.get(&id).map(|(_, t)| *t)
    }

    /// Everything a script or a scene has put somewhere, and where.
    pub fn moved(&self) -> impl Iterator<Item = (u64, Transform)> + '_ {
        self.poses.iter().map(|(id, (_, t))| (*id, *t))
    }

    /// Where a scene left a character (`cinema`): it stays there, and comes
    /// into the world there if it isn't in it yet.
    pub fn place(&mut self, id: u64, at: Transform) {
        self.revision += 1;
        self.poses.insert(id, (self.revision, Transform { scale: Vec3::ONE, ..at }));
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
        // (Back to their own sizes.)
        for scale in self.scales.values_mut() {
            *scale = 1.0;
        }
    }

    fn set_physics(&mut self, id: u64, enabled: bool) {
        self.revision += 1;
        self.physics.insert(id, (self.revision, enabled));
    }

    /// An entity a script resized (`CAPI_Entity.SetScale`): its size times
    /// this, from now on.
    fn set_scale(&mut self, id: u64, scale: f32) {
        self.scales.insert(id, scale);
    }
}

/// An entity's size before a script resized it.
#[derive(Component)]
struct BaseScale(Vec3);

/// Resizes what scripts resized (visuals only, as the engine's box says:
/// a body keeps its size).
#[allow(clippy::type_complexity)]
fn apply_entity_scales(
    placements: Res<EntityPlacements>,
    mut roots: Query<
        (Entity, Option<&crate::objects::PlacedId>, Option<&crate::story_actors::StoryActor>, &mut Transform, Option<&BaseScale>),
        Or<(With<crate::objects::PlacedId>, With<crate::story_actors::StoryActor>)>,
    >,
    mut commands: Commands,
) {
    if placements.scales.is_empty() {
        return;
    }
    for (e, prop, actor, mut t, base) in &mut roots {
        let Some(id) = prop.map(|p| p.0).or_else(|| actor.map(|a| a.id)) else { continue };
        let Some(&scale) = placements.scales.get(&id) else { continue };
        let base = match base {
            Some(b) => b.0,
            None => {
                commands.entity(e).insert(BaseScale(t.scale));
                t.scale
            }
        };
        let want = base * scale.max(1e-3);
        if t.scale != want {
            t.scale = want;
        }
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

/// Pickups picked up: the scripts waiting on them hear of it.
fn forward_pickups(host: Option<NonSendMut<StoryHost>>, mut picked: MessageReader<crate::pickups::PickedUp>) {
    let Some(mut host) = host else {
        picked.clear();
        return;
    };
    for p in picked.read() {
        debug!("story: pickup {} picked up", p.0);
        host.story.pickup_picked(p.0);
    }
}

/// The player touching ground after a fall or a flight (a wingsuit drop, a
/// parachute): scripts waiting for it hear `DominoEvent_PlayerLanded`
/// (JAS_170's drop onto the beach).
fn player_landings(host: Option<NonSendMut<StoryHost>>, players: Query<&Player>, time: Res<Time>, mut air: Local<f32>) {
    let (Some(mut host), Ok(p)) = (host, players.single()) else { return };
    if p.grounded || p.swimming || p.driving || p.spawning {
        if p.grounded && *air >= LANDING_AFTER {
            info!("story: the player landed after {:.1} s in the air", *air);
            host.story.entity_event(fc3_story::PLAYER, "DominoEvent_PlayerLanded", vec![]);
        }
        *air = 0.0;
        return;
    }
    *air += time.delta_secs();
}

/// How long in the air makes touching ground a landing.
const LANDING_AFTER: f32 = 1.5;

/// The player in the air where a script waits for him to land: the
/// autopilot's other steps wait for the landing (one setting him down by a
/// foe left JAS_170's drop zone waiting for a landing it never heard).
fn landing_awaited(host: &StoryHost, p: &Player) -> bool {
    !p.grounded && !p.swimming && !p.driving && host.story.awaits_event(fc3_story::PLAYER, "DominoEvent_PlayerLanded")
}

/// The story autopilot, falling from high up (JAS_170's wingsuit drop, from
/// 300 m over the sea): down to 60 m over the objective marker nearest, to
/// land there, as a player would steer the wingsuit to the drop zone. Not
/// before the scripts listen for the landing, as long as a flight lasts:
/// JAS_170 arms its drop zone some 7 s after the jump, and a landing before
/// that goes unheard (held in the air meanwhile, gliding).
fn autopilot_drop(
    host: Option<NonSend<StoryHost>>,
    cinema: Res<crate::cinema::Cinema>,
    heights: Res<crate::terrain::Heightfield>,
    time: Res<Time>,
    mut players: Query<(&mut Transform, &mut Player)>,
    mut dropped: Local<bool>,
    mut held: Local<f32>,
) {
    if std::env::var_os("FC3_STORY_AUTOPILOT").is_none() {
        return;
    }
    let (Some(host), Ok((mut t, mut p))) = (host, players.single_mut()) else { return };
    if p.grounded || p.swimming {
        *dropped = false;
        *held = 0.0;
        return;
    }
    let surface = heights.height_at(t.translation).unwrap_or(0.0).max(crate::sky::SEA_LEVEL);
    let high = t.translation.y - crate::player::HALF_HEIGHT - surface > DROP_FROM && p.velocity.y < 0.0;
    if *dropped || p.spawning || p.driving || cinema.plays_player() {
        return;
    }
    if !host.story.awaits_event(fc3_story::PLAYER, "DominoEvent_PlayerLanded") {
        if !high && *held == 0.0 {
            return;
        }
        if *held < GLIDE_FOR {
            if *held == 0.0 {
                info!("story autopilot: gliding till a script waits for the landing");
            }
            *held += time.delta_secs().max(1e-3);
            p.velocity = Vec3::ZERO;
            return;
        }
    }
    let here = to_dunia(t.translation);
    let Some((id, at)) = host.story.objective_markers().into_iter().min_by(|a, b| {
        let d = |m: &[f32; 3]| (m[0] - here[0]).hypot(m[1] - here[1]);
        d(&a.1).total_cmp(&d(&b.1))
    }) else {
        return;
    };
    *dropped = true;
    let ground = heights.height_at(to_bevy(at)).unwrap_or(at[2]).max(crate::sky::SEA_LEVEL);
    info!("story autopilot: down to land at {}", host.story.data().name_of(id).unwrap_or("?"));
    teleport(&mut t, &mut p, [at[0], at[1], ground + DROP_HEIGHT], 0.0);
}

/// How high over the drop zone the autopilot puts the player to land there,
/// and how high over the ground a fall must be for the autopilot to steer it.
const DROP_HEIGHT: f32 = 60.0;
const DROP_FROM: f32 = 100.0;
/// How long the autopilot glides, at most, for the scripts to listen.
const GLIDE_FOR: f32 = 30.0;

/// The camera took in one of the story's characters (a VIP's body), or a
/// thing the scripts wait to see: the scripts watching for it hear it
/// discovered, and tagged if alive. (What they wait to see, for the camera.)
fn forward_camera(
    host: Option<NonSendMut<StoryHost>>,
    mut saw: MessageReader<crate::stealth::CameraSaw>,
    mut targets: ResMut<crate::stealth::CameraTargets>,
    time: Res<Time>,
    mut next: Local<f32>,
) {
    let Some(mut host) = host else {
        saw.clear();
        return;
    };
    *next -= time.delta_secs();
    if *next <= 0.0 {
        *next = 0.5;
        let awaited = host.story.awaited_discoveries();
        if targets.awaited != awaited {
            let data = host.story.data();
            targets.spots = awaited.iter().filter_map(|id| data.entities.get(id).map(|e| (*id, to_bevy(e.pos)))).collect();
            targets.awaited = awaited;
        }
    }
    let mut taken = false;
    for s in saw.read() {
        debug!("story: camera took in {}", s.id);
        host.story.entity_event(s.id, "Entity_Discovered", vec![Arg::id(s.id)]);
        if s.tagged {
            host.story.entity_event(s.id, "Entity_Tagged", vec![Arg::id(s.id)]);
        }
        taken = true;
    }
    // (The shutter went: a picture taken, on the player, `GadgetCameraListener`:
    // JAS_200 B30's barks over the pictures.)
    if taken {
        host.story.entity_event(fc3_story::PLAYER, "Picture_Taken", vec![]);
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
        let event = match current.1 {
            JournalPage::Crafting => Some("CDominoCraftingPageOpenedEvent"),
            JournalPage::Skills => Some("CDominoSkillPageOpenedEvent"),
            JournalPage::Handbook => None,
        };
        events.0.extend(event);
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
    /// The autopilot's: the missions running and the last block completed,
    /// and since when they've been so.
    focus_since: Option<((Vec<String>, Option<String>), f32)>,
    /// Whether a scene held the view during the running block, and during
    /// the last one completed (its scene, played, left the player there).
    scene_in_block: bool,
    scene_in_last: bool,
    /// Tests (`FC3_STORY_AUTOPILOT`): seconds since anything happened, and
    /// the triggers already walked into.
    quiet: f32,
    /// When the autopilot last threw a rock to lure a guard off (tests).
    distracted_at: f32,
    /// What last kept the story from going quiet (tests: the stall dump).
    stirred_by: String,
    /// When the autopilot last went near a place a script waits for the
    /// player to come near, by the place.
    near_at: HashMap<u64, f32>,
    /// Scenes by file: when last started and how often (a script's own
    /// loop isn't the story moving on), and the files of their meta ids.
    replays: std::collections::HashMap<String, (f32, u32)>,
    replay_metas: std::collections::HashMap<String, String>,
    /// When each command (as it reads) last came.
    recent: std::collections::HashMap<String, f32>,
    used: std::collections::HashSet<(u64, String)>,
    /// The trigger last stepped into and when; those that failed a mission
    /// at once (CIT_020 B30's escaping-on-foot box), left alone since, over
    /// reloads.
    last_step: Option<(u64, String, f32)>,
    fatal: std::collections::HashSet<(u64, String)>,
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
    pub(crate) checkpoint_game: Option<crate::save::SaveGame>,
    pub(crate) checkpoint_game_pending: bool,
    /// The player's progress when the story saved: the game's half of the
    /// checkpoint is taken later in the frame, after the script's further
    /// commands (JAS_190 takes the takedowns away right after the save its
    /// acknowledgement makes, and a reload then found them gone).
    pub(crate) checkpoint_progress: Option<crate::progression::Progress>,
    /// The shop's weapons as scripts know them: model path -> (archetype,
    /// shop name id).
    shop_items: HashMap<String, (String, u32)>,
    /// The character last sent on an action (tests: the one to follow).
    last_ordered: Option<u64>,
    /// The hidden entities were set from the story's removed ones.
    hidden_synced: bool,
    /// Reinforcements the story has called in, not yet in the world.
    pub reinforcements: Vec<crate::story_actors::Reinforcement>,
    /// AI event points the story has logged (`LogEventInAIDiary`), not yet
    /// put to the army.
    pub ai_events: Vec<u64>,
    /// Explosives scripts set off (`CAPI_Entity.SendDetonateEvent`), not
    /// yet gone off.
    pub detonations: Vec<u64>,
    /// GSF generic events scripts send (`SendGSFGenericEventToEntity`'s
    /// first parameter: `StartPokerJAS220`, `CinemaSequenceFinished`), for
    /// the minigames to read.
    pub gsf_events: Vec<String>,
    /// Each specific gameplay's parameters (`CreateSpecificGameplay`'s,
    /// by its id), for the minigames that read them.
    pub gameplay_params: std::collections::HashMap<u64, Vec<(String, String)>>,
    /// The armies scripts put their characters in (`CAPI_NPC.SwitchArmy`:
    /// 1 red, 2 grey, 3 neutral), by story id, and the throwable a script
    /// chose for the player (`CAPI_Player.SelectThrowGadget`).
    pub armies: std::collections::HashMap<u64, u8>,
    pub throw_choice: Option<crate::controls::Action>,
    /// The reinforcement point each called-in merc came from.
    pub called_from: std::collections::HashMap<u64, u64>,
    /// Whether scripts let each character be tagged
    /// (`CAPI_Entity.ChangeTagActivation`), and those they tag outright
    /// (`CAPI_Entity.ValidateTag`) until they are in the world.
    pub taggable: std::collections::HashMap<u64, bool>,
    pub tag_now: std::collections::HashSet<u64>,
    /// The game is over: the end credits are due (`PopUpEndOfGame`; the
    /// story waits for `CreditsFinished`).
    pub end_of_game: bool,
    /// Harm scripts deal (`HealthModifier`'s `DamagePawn`: JAS_190's
    /// fall into the cenote's depths kills), by pawn.
    pub damages: Vec<(u64, f32)>,
    /// Characters' senses as scripts hold them (`SetImpairedState`), by id.
    pub impaired: HashMap<u64, u8>,
    /// Whom scripts set characters against or beside
    /// (`PawnRelationshipModifier`), by id.
    pub relations: HashMap<u64, crate::npc::Relations>,
    /// How much of other characters' gunfire characters take
    /// (`NPC_SetDamage`): the level last set, and whether it is in force.
    pub npc_damage: HashMap<u64, (u8, bool)>,
    /// Characters scripts made invincible (`PawnInvincibleState`), or let
    /// go again, by id (the player's own).
    pub invincible: HashMap<u64, bool>,
    /// Vehicles scripts blow up, for `vehicles`.
    pub vehicle_explosions: Vec<u64>,
    /// Characters told to look for cover afresh.
    pub recover: Vec<u64>,
    /// Orders scripts have someone call out, for `barks`.
    pub order_calls: Vec<(u8, Vec<u64>, &'static str)>,
    /// Characters vehicles can't hurt by ramming them, by id.
    pub collision_immune: std::collections::HashSet<u64>,
    /// Placed particle systems scripts started (true) or stopped
    /// (`ParticleSystem`), for `story_fx`.
    pub fx_switched: HashMap<u64, bool>,
    /// Lights scripts switched on (true) or off (`DynamicLightModifier`),
    /// for `lamps`.
    pub lights_switched: HashMap<u64, bool>,
    /// Allied fighters sent to a battlefield's point (`SetBattlefields`).
    pub battlefield_posts: HashMap<u64, u64>,
    /// Alarm boxes scripts switched (`SetAlarmState`), for `story_alarms`.
    pub alarm_sets: Vec<(u64, u8)>,
    /// Health scripts set (`SetPawnHealth`), by pawn.
    pub health_sets: Vec<(u64, f32)>,
    /// Props scripts spawned, not yet drawn.
    pub spawned_props: Vec<crate::story_props::SpawnedProp>,
    /// Specific gameplays the game doesn't play, and how long each has run
    /// (they end a moment after they start).
    unplayed: HashMap<u64, f32>,
}

impl StoryHost {
    /// Back to the last checkpoint as play resumes (the pause menu's "last
    /// checkpoint").
    pub(crate) fn reload_checkpoint(&mut self, now: f32) {
        if self.checkpoint.is_some() {
            self.reload_at = Some(now);
        }
    }

    /// How long the story has gone without anything happening (tests).
    pub fn quiet(&self) -> f32 {
        self.quiet
    }

    /// Something happened (a test's step).
    pub fn reset_quiet(&mut self) {
        self.quiet = 0.0;
    }

    /// A library weapon's def (`weapon_def`), else the def drawing the model
    /// the shop's table gives its family (a shooting gallery's
    /// `weapons:FC3/R700`: the 700's, as `FC3/R700.R700_NPC` and the rest).
    pub(crate) fn weapon_def(&self, inv: &crate::weapons::Inventory, archetype: &str) -> Option<usize> {
        weapon_def(inv, archetype, &self.story.data()).or_else(|| {
            let family = |a: &str| a.split_once(':').map_or(a, |(_, n)| n).split('.').next().unwrap_or_default().to_ascii_lowercase();
            let wanted = family(archetype);
            let model = self.shop_items.iter().find(|(_, (a, _))| family(a) == wanted).map(|(m, _)| m)?;
            let folder = model.rsplit('\\').nth(1)?;
            inv.defs.iter().position(|d| d.model.split('\\').next().is_some_and(|f| f.eq_ignore_ascii_case(folder)))
        })
    }

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

    /// Whether a command repeats what the scripts did within the last half
    /// minute, as their own loops do: the very same command again (an
    /// action switched on again, a fire's particles played again), or a
    /// scene started again (under a new meta id) or stopped.
    fn replayed(&mut self, c: &Command, now: f32) -> bool {
        const WITHIN: f32 = 30.0;
        // (A sound is the same one again whatever its instance number, the
        // first argument: BAM_040's collapsing temple rumbles every second.)
        let key = match c {
            Command::Native { name, args } if name == "PlaySound" => format!("{name}{:?}", args.get(1..)),
            _ => format!("{c:?}"),
        };
        let same = self.recent.insert(key, now).is_some_and(|at| now - at < WITHIN);
        if self.recent.len() > 1024 {
            self.recent.retain(|_, at| now - *at < WITHIN);
        }
        let Command::Native { name, args } = c else { return same };
        let a = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
        let again = |r: Option<&(f32, u32)>| r.is_some_and(|&(at, n)| n > 1 && now - at < WITHIN);
        same || match name.as_str() {
            "Sequence" if a(0).starts_with("play") => {
                let r = self.replays.entry(a(2).to_string()).or_insert((now, 0));
                r.1 = if now - r.0 < WITHIN { r.1 + 1 } else { 1 };
                r.0 = now;
                self.replay_metas.insert(a(3).to_string(), a(2).to_string());
                again(Some(r))
            }
            "Sequence" => again(self.replays.get(a(2))),
            "CSequenceManager:StopCinematicMetaSequence" => {
                let file = self.replay_metas.remove(a(0));
                again(file.and_then(|f| self.replays.get(&f)))
            }
            _ => false,
        }
    }

    pub(crate) fn new(story: Story, checkpoint: Option<fc3_story::save::StorySave>) -> StoryHost {
        StoryHost {
            story,
            due: Vec::new(),
            last_completed: None,
            focus_since: None,
            scene_in_block: false,
            scene_in_last: false,
            quiet: 0.0,
            distracted_at: f32::MIN,
            stirred_by: String::new(),
            near_at: HashMap::new(),
            replays: Default::default(),
            replay_metas: Default::default(),
            recent: Default::default(),
            used: Default::default(),
            last_step: None,
            fatal: fatal_learned(),
            checkpoint,
            reload_at: None,
            was_dead: false,
            to_respawn: false,
            kept: 0,
            checkpoint_game_paths: Vec::new(),
            checkpoint_game: None,
            checkpoint_game_pending: false,
            checkpoint_progress: None,
            shop_items: HashMap::new(),
            last_ordered: None,
            hidden_synced: false,
            reinforcements: Vec::new(),
            ai_events: Vec::new(),
            detonations: Vec::new(),
            gsf_events: Vec::new(),
            gameplay_params: Default::default(),
            armies: Default::default(),
            throw_choice: None,
            called_from: Default::default(),
            taggable: Default::default(),
            tag_now: Default::default(),
            end_of_game: false,
            damages: Vec::new(),
            impaired: HashMap::new(),
            relations: HashMap::new(),
            battlefield_posts: HashMap::new(),
            npc_damage: HashMap::new(),
            invincible: HashMap::new(),
            vehicle_explosions: Vec::new(),
            recover: Vec::new(),
            order_calls: Vec::new(),
            collision_immune: std::collections::HashSet::new(),
            fx_switched: HashMap::new(),
            lights_switched: HashMap::new(),
            alarm_sets: Vec::new(),
            health_sets: Vec::new(),
            spawned_props: Vec::new(),
            unplayed: HashMap::new(),
        }
    }
}

enum Due {
    Sound(u64),
    /// A HUD popup's time is up (`DisplayCustomUIMsg`'s timer).
    HidePopup,
}

/// The hint box's key for a HUD popup (`DisplayCustomUIMsg`: the mission
/// area's "leaving" warning), above every script's hint so it shows first.
const POPUP_HINT: u64 = u64::MAX;

/// A mission's card for the original HUD (`original_ui`): its title as
/// it starts, its end.
#[derive(Clone, Debug, PartialEq)]
pub enum MissionCard {
    Start { title: String },
    Complete { title: String },
    Failed { text: String },
    /// A world event's (an outpost liberated, a radio tower taken), by the
    /// game's strings: its text, and under it a count (`%d` in the string).
    Event {
        text: u32,
        count: Option<(u32, usize)>,
    },
    /// A script's success screen, by the game's string: dropped when an
    /// event's card has just shown the same news (an outpost the player
    /// takes shows its own, with how it was taken and the count, and its
    /// flag controller then plays the taking).
    Success {
        text: u32,
    },
}

/// What the story shows on screen.
#[derive(Resource, Default)]
pub struct StoryUi {
    pub objective: Option<String>,
    /// The mission under way's name (its title as it started).
    pub mission: Option<String>,
    /// Mission cards the original HUD hasn't shown yet.
    pub cards: Vec<MissionCard>,
    /// A subtitle and how long it stays.
    pub subtitle: Option<(String, f32)>,
    /// A large centre banner (mission title, complete, failed).
    pub banner: Option<(String, f32)>,
    /// A checkpoint the story has just reached (the game is saved with it).
    pub checkpoint_reached: bool,
    /// Hint texts by id (tutorial boxes): title and text.
    pub hints: HashMap<u64, (String, String)>,
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
    /// A weapon the scripts give is drawn at once (`SetAutoDraw`: F3's
    /// trial hands the player one weapon after another), and the last they
    /// gave (drawn when they ask for that after giving it).
    pub auto_draw: bool,
    last_granted: Option<usize>,
    /// The weapon in hand is drawn again for the scripts (the one they gave
    /// was already in hand).
    redraw: bool,
    /// Non-weapon inventory entries missions give and take (rock, camera).
    pub gadgets: std::collections::HashSet<String>,
    /// Weapons scripts gave by an archetype of their own (the Ink Monster
    /// fight's `BOW.citraBossFightBow`), by weapon: the story knows them by
    /// that name while they are carried.
    pub granted: HashMap<usize, String>,
    /// A script drew the camera (`CAPI_Player.DrawGadget`): it stays up
    /// until a script holsters it or the player puts it away.
    pub camera_drawn: bool,
    /// The camera is up (its view over the screen), for the scripts.
    pub camera_up: bool,
    pub animal_looting: bool,
    pub plant_looting: bool,
    /// The HUD's parts scripts have hidden (`UISendEventToPlayer` to the
    /// HUD, `HideUIElement Radar|Reticule|Ammo`, until `ShowUIElement`),
    /// lower case.
    pub hud_parts_hidden: std::collections::BTreeSet<String>,
    /// The character or vehicle whose health a script has the HUD show
    /// (`DisplayHealthBar`: an escort, HUR_040's barge, JAS_220's
    /// helicopter).
    pub health_bar: Option<HealthBar>,
    /// The player's weapon put away for a scene (`CAPI_Pawn.SetNoWeaponMode`:
    /// the narrative scenes' setup, until their cleanup).
    pub no_weapon: bool,
    /// The player's weapon out of sight, though still in hand
    /// (`CAPI_Pawn.SetWeaponVisibility`: an outpost's flag being raised).
    pub weapon_hidden: bool,
    /// Where the maps show the player while a script says (Bevy space;
    /// `OverridePlayerPosition`: BAM_020's sunken ship and the other
    /// places under the world, shown where they lie on it).
    pub map_position: Option<Vec3>,
    /// The player's field of view while a script sets it (degrees;
    /// `CAPI_Player.SetPlayerFOV`: JAS_100's car ride), and the seconds it
    /// takes to get there or back.
    pub fov: (Option<f32>, f32),
    /// The player's air as a script last set it (a share of the most), to
    /// put on once; and whether the player breathes at all
    /// (`CAPI_Player.SetOxygenLevel`: BAM_020's oxygen bottles and the
    /// drowning pirate's struggle). Without air to use he can't drown.
    pub oxygen: Option<f32>,
    pub no_drowning: bool,
    /// A stance a script put the player in (`CAPI_Pawn.SetPawnStance`:
    /// crouched or standing), and a sprint it stopped
    /// (`CAPI_Player.SetSprinting` false, the speed box's ForceWalk), for
    /// the controls to take up.
    pub stance: Option<bool>,
    pub break_sprint: bool,
}

impl ScriptedPlayer {
    /// The player's place on the maps: theirs, or where a script shows
    /// them.
    pub fn on_map(&self, t: &Transform) -> Transform {
        Transform { translation: self.map_position.unwrap_or(t.translation), ..*t }
    }
}

/// A health bar a script put up: whose, the name it shows (a string id) and
/// the health left (0 to 1).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HealthBar {
    pub entity: u64,
    pub name: u32,
    pub fraction: f32,
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
            ignored: Default::default(),
            movement: 1.0,
            god: false,
            hud_hidden: false,
            fade: (0.0, 0.0, 0.0),
            black_for: 0.0,
            backup: None,
            auto_draw: false,
            last_granted: None,
            redraw: false,
            gadgets: ["gadgets:FC3/Rock".into(), "gadgets:Equipped/Tablet".into()].into_iter().collect(),
            granted: HashMap::new(),
            animal_looting: true, plant_looting: true, camera_drawn: false, camera_up: false,
            hud_parts_hidden: Default::default(),
            health_bar: None,
            no_weapon: false,
            weapon_hidden: false,
            map_position: None,
            fov: (None, 0.0),
            oxygen: None,
            no_drowning: false,
            stance: None,
            break_sprint: false,
        }
    }
}

/// The share of other characters' gunfire scripts let characters take, and
/// who they make invincible.
#[allow(clippy::type_complexity)]
fn apply_damage_rates(
    host: Option<NonSend<StoryHost>>,
    actors: Query<(Entity, &crate::story_actors::StoryActor, Option<&crate::combat::NpcDamageRate>)>,
    mut spared: Query<(Entity, Option<&crate::story_actors::StoryActor>, Has<Player>, Option<&mut crate::combat::Invincible>), Or<(With<crate::story_actors::StoryActor>, With<Player>)>>,
    immune: Query<(Entity, &crate::story_actors::StoryActor, Has<crate::vehicles::ImmuneToCollisions>)>,
    mut commands: Commands,
) {
    let Some(host) = host else { return };
    for (e, actor, rammed) in &immune {
        let want = host.collision_immune.contains(&actor.id);
        if want && !rammed {
            commands.entity(e).insert(crate::vehicles::ImmuneToCollisions);
        } else if !want && rammed {
            commands.entity(e).remove::<crate::vehicles::ImmuneToCollisions>();
        }
    }
    for (e, actor, is_player, inv) in &mut spared {
        let id = if is_player { fc3_story::PLAYER } else { actor.map_or(u64::MAX, |a| a.id) };
        let held = host.invincible.get(&id).copied().unwrap_or(false);
        match inv {
            Some(mut i) if i.scripted != held => i.scripted = held,
            None if held => {
                commands.entity(e).insert(crate::combat::Invincible { scripted: true, ..default() });
            }
            _ => {}
        }
    }
    for (e, actor, rate) in &actors {
        if host.npc_damage.is_empty() && rate.is_none() {
            continue;
        }
        let want = host.npc_damage.get(&actor.id).filter(|(_, on)| *on).map(|(l, _)| crate::combat::NpcDamageRate::of_level(*l));
        if want.as_ref() != rate {
            match want {
                Some(r) => commands.entity(e).insert(r),
                None => commands.entity(e).remove::<crate::combat::NpcDamageRate>(),
            };
        }
    }
}

/// The ambience a script plays in place of the place's (a boss fight's),
/// and the sea where a script has moved it.
fn scripted_ambience(host: Option<NonSend<StoryHost>>, regions: Option<ResMut<crate::sound::SoundRegions>>) {
    let Some(host) = host else { return };
    let sea = host.story.ocean_level().unwrap_or(crate::sky::SEA_LEVEL);
    if crate::water::ocean_level() != sea {
        crate::water::set_ocean_level(sea);
    }
    let Some(mut regions) = regions else { return };
    let forced = host.story.ambience_override().and_then(|i| u8::try_from(i).ok());
    if regions.forced != forced {
        debug!("story: ambience of sound region {forced:?}");
        regions.forced = forced;
    }
}

/// The stance and sprint scripts set go to the controls (held there until
/// the player changes them himself).
fn scripted_controls(mut scripted: ResMut<ScriptedPlayer>, mut actions: ResMut<crate::controls::ActionState>) {
    if let Some(crouch) = scripted.stance.take() {
        actions.script_stance(crouch);
    }
    if std::mem::take(&mut scripted.break_sprint) {
        actions.break_sprint();
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
    (scale, minigame, races, trials): (Res<UiScale>, Option<Res<crate::hud::Minigame>>, Option<Res<crate::races::Races>>, Option<Res<crate::trials::Trials>>),
    original: Res<crate::hud::OriginalHud>,
) {
    for e in &icons {
        commands.entity(e).despawn();
    }
    // (The original HUD shows them its own way: `original_ui::drive_indicators`.)
    if original.0 {
        return;
    }
    // (A race's next gate with them; while a race holds the player, the gate
    // alone.)
    let racing = races.as_ref().is_some_and(|r| r.holds_player()) || trials.is_some_and(|t| t.holds_player());
    let all: Vec<Vec3> = markers.0.iter().copied().filter(|_| !racing).chain(races.and_then(|r| r.marker)).collect();
    if all.is_empty() || scripted.hud_hidden || cinema.view.is_some() || minigame.is_some_and(|m| m.holds_player()) {
        if !markers.0.is_empty() {
            trace!("markers hidden: hud {} scene view {}", scripted.hud_hidden, cinema.view.is_some());
        }
        return;
    }
    let Ok((cam, cam_t)) = cameras.single() else { return };
    let Some(size) = cam.logical_viewport_size() else { return };
    let size = crate::ui_nav::viewport_to_ui(size, scale.0);
    // Off screen markers sit on a frame inside the compass and the HUD.
    let (lo, hi) = (Vec2::new(40.0, 90.0), size - Vec2::new(40.0, 150.0));
    let mid = (lo + hi) * 0.5;
    let view = cam_t.affine().inverse();
    for &at in &all {
        let to = at - cam_t.translation();
        let local = view.transform_point3(at);
        let seen = cam.world_to_viewport(cam_t, at).ok().map(|p| crate::ui_nav::viewport_to_ui(p, scale.0))
            .filter(|p| local.z < 0.0 && p.cmpge(lo).all() && p.cmple(hi).all());
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
pub(crate) fn weapon_def(inv: &crate::weapons::Inventory, archetype: &str, data: &fc3_story::data::StoryData) -> Option<usize> {
    let short = archetype.rsplit(['/', ':']).next().unwrap_or(archetype).to_ascii_lowercase();
    let in_folder = |folder: &str| inv.defs.iter().position(|d| d.model.split('\\').next().is_some_and(|f| f.eq_ignore_ascii_case(folder)));
    if let Some(def) = inv.defs.iter().position(|d| d.name.to_ascii_lowercase() == short).or_else(|| in_folder(&short)) {
        return Some(def);
    }
    // By its family (`LPO-50` of `LPO-50.LPO-50_NPC`): the folder of the
    // model the game's weapons of that family draw (or the family's own
    // folder: the Ink Monster fight's `BOW.citraBossFightBow`).
    let family = short.split('.').next().filter(|f| !f.is_empty())?;
    if let Some(def) = in_folder(family) {
        return Some(def);
    }
    // (Or its model's own name, letters and digits alike: JAS_130's
    // `GM-94`, ours the `GL-94` drawn by `gm94grenadelauncher\gm94.xbg`.)
    let plain = |s: &str| s.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
    let want = plain(family);
    if let Some(def) = inv.defs.iter().position(|d| d.model.rsplit('\\').next().and_then(|f| f.split('.').next()).is_some_and(|stem| plain(stem) == want)) {
        return Some(def);
    }
    data.entities.values().filter(|e| e.has("CFCXWeapon") && e.name.to_ascii_lowercase().contains(family)).find_map(|e| {
        let mut folder = None;
        e.component("CGraphicComponent")?.walk(&mut |o, _| {
            if folder.is_none() {
                folder = o.str_h(0x3182_766C).filter(|m| m.to_ascii_lowercase().ends_with(".xbg")).and_then(|m| m.rsplit('\\').nth(1).map(str::to_string));
            }
        });
        in_folder(&folder?)
    })
}

impl ScriptedPlayer {
    pub fn has_gadget(&self, archetype: &str) -> bool {
        self.gadgets.iter().any(|g| g.eq_ignore_ascii_case(archetype))
    }

    pub fn has_camera(&self) -> bool {
        self.gadgets.iter().any(|g| camera_gadget(g))
    }

    /// The camera the player carries, as the scripts name it.
    fn camera(&self) -> Option<&String> {
        self.gadgets.iter().filter(|g| camera_gadget(g)).min()
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
    granted: &HashMap<usize, String>,
) -> (Vec<fc3_story::Item>, Option<String>) {
    use crate::weapons::{WeaponClass as W, UNARMED};
    let mut items = Vec::new();
    let mut held = None;
    for (i, slot) in slots.iter().enumerate() {
        let Some(def) = defs.get(slot.def).filter(|d| d.name != UNARMED) else { continue };
        let name = granted
            .get(&slot.def)
            .or_else(|| shop.get(&def.model_path()).map(|(a, _)| a))
            .cloned()
            .unwrap_or_else(|| format!("weapons:FC3/{}", def.model.split('\\').next().unwrap_or(def.name)));
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
pub(crate) fn prompt_text(text: &str, actions: &crate::controls::ActionState, settings: &crate::settings::Settings) -> String {
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
        } else if rest[1..close].eq_ignore_ascii_case("WeaponWheelSelect") {
            out.push_str(if actions.gamepad_active { "[Right stick]" } else { "[Mouse]" });
        } else {
            out.push_str(&rest[..=close]);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn timer_text(timer: &fc3_story::Timer) -> String {
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
    // FC3_STORY_CHECKPOINTS), at its respawn point (unless `--pos` places
    // the player: an outpost to take, from the story as it stands).
    let test_load = std::env::var("FC3_STORY_LOAD").ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| fc3_story::save::StorySave::from_text(&t));
    let to_respawn = test_load.is_some() && world.get_resource::<crate::Args>().is_none_or(|a| !a.placed);
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
    // Its completed missions open abilities in the skill tree.
    let done: Vec<String> = story.missions().iter().filter(|m| m.1 == "Completed").map(|m| m.0.to_ascii_lowercase()).collect();
    if let Some(mut progress) = world.get_resource_mut::<crate::progression::Progress>() {
        progress.story = true;
        progress.missions.extend(done);
    }
    let mut host = StoryHost::new(story, saved);
    // Continue runs before this host exists during startup. Retain its paired
    // inventory here too, so death before the next checkpoint can roll back.
    if continued {
        host.checkpoint_game = world.get_resource::<crate::save::SaveSlot>().and_then(|s| s.existing.clone());
    }
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
    (mut players, mut stats, perks, mount, state, actor_health, mut pawn_fx, mut shimmers, mut light_switches): (
        Query<(&mut Transform, &mut Player, Option<&mut crate::combat::Health>)>,
        Query<&mut crate::combat::PlayerStats, With<Player>>,
        Res<crate::progression::Perks>,
        Res<crate::traversal::Mount>,
        Res<State<GameState>>,
        Query<(&crate::story_actors::StoryActor, &crate::combat::Health), Without<Player>>,
        ResMut<crate::pawn_fx::PawnFx>,
        ResMut<crate::shimmer::Shimmers>,
        ResMut<crate::light_switch::LightSwitches>,
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
    (mut area, mut cinema, mut placements, original_hud, mut mixings): (
        ResMut<MissionArea>,
        ResMut<crate::cinema::Cinema>,
        ResMut<EntityPlacements>,
        Res<crate::hud::OriginalHud>,
        ResMut<crate::mixer::Mixings>,
    ),
    (actions, mut markers, heights, mut voices, driving, vehicle_views, mut seat_requests, mut vehicle_paths, mut drive, guns, helis): (
        Res<crate::controls::ActionState>,
        ResMut<ObjectiveMarkers>,
        Res<crate::terrain::Heightfield>,
        ResMut<crate::faces::Voices>,
        Res<crate::vehicles::Driving>,
        Query<(Entity, &crate::vehicles::Vehicle, &Transform, Option<&crate::combat::Health>), Without<Player>>,
        ResMut<crate::vehicles::StorySeatRequests>,
        ResMut<crate::vehicle_paths::StoryVehiclePaths>,
        ResMut<crate::vehicles::AutopilotDrive>,
        Query<&crate::turrets::Turret>,
        Query<(&crate::story_helis::Heli, &Transform, &crate::combat::Health), Without<Player>>,
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
    (checkpoint_actors, mut commands_out, mut hidden, tagged, mut tattoo, mut shakes): (
        Query<Entity, Or<(With<crate::story_actors::StoryActor>, With<crate::wildlife::WildAnimal>)>>, Commands, ResMut<HiddenEntities>,
        Query<&crate::story_actors::StoryActor, With<crate::stealth::Tagged>>,
        ResMut<crate::tattoo::TattooShown>,
        MessageWriter<crate::shake::Shake>,
    ),
    (
        mut xp_gain,
        mut progress,
        mut music,
        mut rock_test,
        mut videos,
        mut qtes,
        mut vines,
        mut postfx,
        mut pawn_anims,
        mut action_maps,
        mut breaks,
        mut trees,
        mut doors,
        mut stims,
        (
            mut sniping,
            mut boss_fight,
            mut binds,
            mut unlimited_ammo,
            mut intoxication,
            mut prompts,
            mut chopper,
            mut car_chase,
            mut gallery,
            (poker, minigame, mut races, mut trials),
            detection,
            mut env_overrides,
        ),
    ): (
        MessageWriter<crate::progression::XpGain>,
        ResMut<crate::progression::Progress>,
        ResMut<crate::music::Music>,
        Option<ResMut<crate::stealth::RockAimTest>>,
        ResMut<crate::video::Videos>,
        ResMut<crate::qte::Qtes>,
        ResMut<crate::vine::Vines>,
        ResMut<crate::post_effects::PostFx>,
        ResMut<crate::pawn_anims::PawnAnims>,
        ResMut<crate::controls::ScriptActionMaps>,
        ResMut<crate::breakables::Breaks>,
        ResMut<crate::tree_growth::TreeGrowth>,
        ResMut<crate::doors::Doors>,
        ResMut<crate::stims::Stims>,
        (
            ResMut<crate::sniping::Sniping>,
            ResMut<crate::boss_fight::BossFight>,
            ResMut<crate::binds::Binds>,
            ResMut<crate::weapons::UnlimitedAmmo>,
            ResMut<crate::intoxication::Intoxication>,
            ResMut<crate::input_prompts::InputPrompts>,
            ResMut<crate::chopper_fight::ChopperFight>,
            ResMut<crate::car_chase::CarChase>,
            ResMut<crate::shooting_gallery::Gallery>,
            (Res<crate::poker::PokerTable>, Res<crate::hud::Minigame>, ResMut<crate::races::Races>, ResMut<crate::trials::Trials>),
            Res<crate::stealth::Detection>,
            ResMut<crate::sky::EnvOverrides>,
        ),
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
            Due::Sound(id) => host.story.sound_finished(id),
            Due::HidePopup => {
                ui.hints.remove(&POPUP_HINT);
            }
        }
    }
    // Screen effects started and ended, for the scripts that listen
    // (`PostFxListener`).
    for (start, id) in postfx.take_events() {
        let event = if start { "DominoEvent_FxStart" } else { "DominoEvent_FxStop" };
        if host.story.awaits_event(fc3_story::PLAYER, event) {
            host.story.entity_event(fc3_story::PLAYER, event, vec![fc3_story::Arg::Num(id as f64)]);
        }
    }
    let Ok((mut t, mut p, mut health)) = players.single_mut() else { return };
    let mut heal = false;
    host.scene_in_block |= cinema.view.is_some();
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
            if let Some(save) = &host.checkpoint_game {
                if let Ok(mut inv) = inventory.single_mut() { crate::save::restore_inventory(&mut inv, save); }
                if let Ok(mut stats) = stats.single_mut() {
                    stats.medkits = save.medkits;
                    stats.kills = save.kills;
                    stats.burning = 0.0;
                    stats.hurt_dir = None;
                    stats.respawn_timer = 0.0;
                }
                *progress = save.progress.clone();
                if let Some(h) = health.as_mut() { **h = crate::combat::Health::new(h.max); }
                info!("story checkpoint: restored {} carried weapons, {} medkits, ${}", save.weapons.len(), save.medkits, save.progress.money);
            }
            *vehicle_paths = Default::default();
            if let Some(test) = rock_test.as_mut() { test.0 = None; }
            placements.reset(&host.story.data());
            host.hidden_synced = false;
            host.due.clear();
            // (What the autopilot did since is to do again, but for what
            // failed the mission: JAS_020 B15's guard, lured off before the
            // reload, was never lured again.)
            host.used.clear();
            host.near_at.clear();
            host.impaired.clear();
            host.relations.clear();
            host.battlefield_posts.clear();
            host.npc_damage.clear();
            host.invincible.clear();
            host.collision_immune.clear();
            host.fx_switched.clear();
            host.lights_switched.clear();
            env_overrides.active.clear();
            env_overrides.seconds = 0.0;
            env_overrides.storm = None;
            pawn_fx.clear = true;
            shimmers.script.clear();
            light_switches.0.clear();
            layers.present.clear();
            layers.reset = true;
            orders.0.clear();
            cinema.reset();
            *scripted = ScriptedPlayer::default();
            shop_override.0 = None;
            *pause_menu = crate::progression::PauseMenu::from_story(&host.story);
            if let Some(save) = &host.checkpoint_game {
                pause_menu.tutorial = save.craft_tutorial;
                pause_menu.tutorial_completed = save.craft_tutorial == Some(crate::progression::CraftTutorial::Done);
            }
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
            stims.reset();
            pawn_anims.reset();
            action_maps.reset();
            trees.reset();
            vines.reset();
            sniping.reset();
            boss_fight.reset();
            binds.reset();
            unlimited_ammo.0 = false;
            intoxication.reset();
            prompts.reset();
            chopper.reset();
            car_chase.reset();
            gallery.reset();
            races.reset();
            trials.reset();
            postfx.reset();
            breaks.release_all();
            ui.subtitle = None;
            // Rebuild mission actors and discard transient wildlife together.
            // Leaving an ambient predator at its post-death position makes
            // retries inherit the previous attempt's chase and immediate hits.
            let reset_count = checkpoint_actors.iter().count();
            for e in &checkpoint_actors {
                commands_out.entity(e).despawn();
            }
            info!("story checkpoint: reset {reset_count} mission/wildlife actors");
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
        difficulty: fov.difficulty.index() as u8,
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
    view.player_state.in_qte = qtes.active();
    view.player_state.loot = loot_catalog.quantities(&progress);
    view.player_state.medpacks = stats.single().map_or(0, |s| s.medkits);
    view.player_state.weapon_slots = perks.weapon_slots as u32;
    // (Skills by name, and the bonus plans in force: scripts ask after
    // either, `HasBonus`.)
    view.player_state.skills = progress.skills.iter().map(|s| s.to_ascii_lowercase()).chain(perks.plans.iter().cloned()).collect();
    view.player_state.on_zipline = matches!(*mount, crate::traversal::Mount::Zip { .. });
    // (Yaw as an entity's and `GetAnglesFromDir`'s: about up from north.)
    view.player_state.look_angles = [p.pitch.to_degrees(), 0.0, p.yaw.to_degrees()];
    // (Story helicopters too: G3's trial's, flying in.)
    for (heli, at, health) in &helis {
        view.spawned.insert(
            heli.vehicle,
            fc3_story::Live { pos: to_dunia(at.translation), alive: !health.dead, health: health.current, max_health: health.max, ai_state: 0 },
        );
    }
    for (entity, vehicle, at, health) in &vehicle_views {
        view.spawned.insert(
            vehicle.story_id(),
            fc3_story::Live {
                pos: to_dunia(at.translation),
                alive: health.is_none_or(|h| !h.dead),
                health: health.map_or(1.0, |h| h.current),
                max_health: health.map_or(1.0, |h| h.max),
                ai_state: 0,
            },
        );
        view.pawn_vehicles.extend(vehicle.npc_occupants());
        // (On its mounted gun, in its gunner seat: HUR_040's barge.)
        let gunning = p.mounted.and_then(|m| guns.get(m.turret).ok()).and_then(|g| g.on_vehicle()) == Some(entity);
        let occupant = if p.driving && driving.vehicle == Some(entity) {
            vehicle.occupant(driving.seat)
        } else if gunning {
            vehicle.gunner_occupant()
        } else {
            None
        };
        if let Some(occupant) = occupant {
            view.player_in_vehicle = true;
            view.player_state.vehicle = Some(occupant.vehicle);
            view.player_state.seat = occupant.kind;
            view.player_state.seat_index = occupant.index;
            view.player_state.vehicle_type = occupant.vehicle_type;
        }
    }
    if let Ok(inv) = inventory.single() {
        if !scripted.granted.is_empty() {
            scripted.granted.retain(|def, _| inv.slots.iter().any(|s| s.def == *def));
        }
        (view.player_state.weapons, view.player_state.in_hands) = story_weapons(&inv.defs, &inv.slots, inv.current, &host.shop_items, &scripted.granted);
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
    // The camera up is the gadget in the player's hands.
    if let Some(camera) = scripted.camera().filter(|_| scripted.camera_up) {
        view.player_state.in_hands = Some(camera.clone());
        view.player_state.gadget = Some(fc3_story::Item { name: camera.clone(), count: 1, category: "binoculars".into(), class: "binoculars".into() });
    }
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
        view.spawned.insert(
            a.id,
            fc3_story::Live { pos: to_dunia(at.translation - Vec3::Y * crate::player::HALF_HEIGHT), alive: !a.dead, health: 1.0, max_health: 1.0, ai_state },
        );
    }
    // The health a script's bar shows: a character's, else a vehicle's
    // (none left once it's gone).
    if let Some(bar) = scripted.health_bar.as_mut() {
        let character = actor_health.iter().find(|(a, _)| a.id == bar.entity).map(|(a, h)| if a.dead { 0.0 } else { h.fraction() });
        let vehicle = || view.spawned.get(&bar.entity).map(|l| if l.alive && l.max_health > 0.0 { (l.health / l.max_health).clamp(0.0, 1.0) } else { 0.0 });
        bar.fraction = character.or_else(vehicle).unwrap_or(bar.fraction);
    }
    // (Not swimming while held after a move or played by a scene: the flag
    // is the last the movement saw, the sea a reload took him from;
    // JAS_170's plane-crash scene failed at once for "landing" in it.)
    view.player_state.stealth = detection.0;
    view.player_state.stance = if p.swimming && !p.spawning && !cinema.plays_player() {
        2
    } else if p.crouching {
        1
    } else {
        0
    };
    // The player's inputs, as the signals scripts listen for (not while a
    // cutscene holds the view: the game takes no gameplay input then, and a
    // scene's skip listener waits for `move`, which would end it at once).
    if gameplay && cinema.view.is_none() {
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
    if time.elapsed_secs() as u32 != (time.elapsed_secs() - dt) as u32 {
        if let Some(id) = std::env::var("FC3_STORY_TRIGGER_DEBUG").ok().and_then(|s| s.parse::<u64>().ok()) {
            info!("story trigger diagnostic {id}: {}", host.story.trigger_debug(id));
        }
    }
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
    // A specific gameplay the game doesn't play yet ends a moment after it
    // starts: a script waiting for it to stop would otherwise hold the story
    // for good (JAS_130's car chase, JAS_150's chopper fight).
    if gameplay {
        let unplayed: Vec<(u64, String)> = host.story.gameplays().into_iter().filter(|(id, name)| !played_gameplay(&host, *id, name)).collect();
        host.unplayed.retain(|id, _| unplayed.iter().any(|(u, _)| u == id));
        let mut over = Vec::new();
        for (id, name) in unplayed {
            let ran = host.unplayed.entry(id).or_insert(0.0);
            *ran += dt;
            if *ran >= UNPLAYED_GRACE {
                over.push((id, name));
            }
        }
        for (id, name) in over {
            info!("story: {name} ({id}) ends: not played");
            host.unplayed.remove(&id);
            host.story.gameplay_ended(id);
        }
    }
    // Tests: when nothing has happened for a while, step into a trigger a
    // script waits on.
    host.quiet += dt;
    // (Nor does a script holding the time of day, set again every few
    // seconds: BAM_020's ship; nor one repeating itself: BNG_010's club
    // replays its light scenes, each start clearing arrows first, and
    // switches the same action on again and again; BAM_030's ruins play
    // and stop their particles; nor a warning on the screen coming and
    // going: HOY_010's to stay close to Sam.)
    let now = time.elapsed_secs();
    let mut stirred = None;
    for c in &commands {
        let quiet = matches!(c, Command::Layer { .. } | Command::Print(_) | Command::Dialog { .. })
            || matches!(c, Command::Native { name, .. }
                if name == "SetTimeOfDay" || name == "DeleteAllArrowPickups" || name == "HudText" || name == "HudTextHidden");
        if !quiet && !host.replayed(c, now) {
            stirred.get_or_insert(c);
        }
    }
    if let Some(c) = stirred {
        host.quiet = 0.0;
        host.stirred_by = format!("{c:?}").chars().take(120).collect();
    }
    // (With FC3_STORY_WAIT_DEBUG, a minute on too: a script's looping bark
    // can keep the story from ever going quiet.)
    let minute = std::env::var("FC3_STORY_WAIT_DEBUG").is_ok() && (time.elapsed_secs() / 60.0) as u32 != ((time.elapsed_secs() - dt) / 60.0) as u32;
    if gameplay
        && (std::env::var("FC3_STORY_AUTOPILOT").is_ok() || std::env::var("FC3_STORY_WAIT_DEBUG").is_ok())
        && ((host.quiet > 20.0 && host.quiet - dt <= 20.0) || minute)
    {
        info!(
            "story autopilot: stalled ({:.1} s quiet, last stirred by {}; scene view {}, scene plays the player {}); triggers {:?}, signals {:?}",
            host.quiet,
            host.stirred_by,
            cinema.view.is_some(),
            cinema.plays_player(),
            host.story.watched_triggers(),
            host.story.watched_signals()
        );
        for w in host.story.waiting_on() {
            info!("story autopilot: waiting on {w}");
        }
        for p in host.story.proximity_debug() {
            info!("story autopilot: distance check: {p}");
        }
        for w in host.story.watched_triggers() {
            info!("story autopilot: trigger {} {}: {}", w.0, w.1, host.story.trigger_debug(w.0));
        }
        for g in host.story.counted_report() {
            info!("story autopilot: counting {g}");
        }
    }
    // Slow inspection runs deliberately pause between actions. Chase/escape
    // fixtures can request a shorter pause without changing gameplay AI.
    let autopilot_delay = std::env::var("FC3_STORY_AUTOPILOT_DELAY").ok()
        .and_then(|s| s.parse::<f32>().ok()).filter(|s| s.is_finite())
        .unwrap_or(6.0).max(0.5);
    // (At the wheel, the autopilot drives into the boxes scripts wait on.)
    let at_wheel = p.driving && vehicle_views.iter().any(|(e, v, _, _)| driving.vehicle == Some(e) && v.is_driver_seat(driving.seat));
    // (Not while a sniping challenge holds the player at its rifle: WIL_050's
    // B20 has him cover Oliver from it on his way to the boat.)
    if gameplay
        && health.as_ref().is_none_or(|h| !h.dead)
        && (!p.driving || at_wheel)
        && !sniping.holds_player()
        && !boss_fight.holds_player()
        && !gallery.holds_player()
        && !races.holds_player()
        && !poker.holds_player()
        && !minigame.holds_player()
        && cinema.view.is_none()
        && !cinema.plays_player()
        && std::env::var("FC3_STORY_AUTOPILOT").is_ok()
        && !landing_awaited(&host, &p)
        && host.quiet > autopilot_delay
        && crafting.still_wanted(host.story.watched_loot_filters(), &progress.loot).is_empty()
    {
        let graph = std::env::var("FC3_STORY_AUTOPILOT_GRAPH").ok();
        // The missions running go first (CIT_020 holds the player on its
        // water tower while quests' starts wait): their triggers before the
        // others', and no other mission is started meanwhile (HUR_040
        // started across the island failed for CIT_020's triggers drawing
        // the player out of its zone).
        let focus = host.story.missions_running();
        let focused = if focus.is_empty() { Vec::new() } else { host.story.watched_triggers_of(graph.as_deref(), &focus) };
        // (Unless those missions have long gone nowhere, nothing of theirs
        // left it can do: a supply run it can't drive held CIT_010 and
        // BNG_010 back for good.)
        let marker = (focus.clone(), host.last_completed.clone());
        if host.focus_since.as_ref().is_none_or(|(m, _)| *m != marker) {
            host.focus_since = Some((marker, now));
        }
        let stuck = host.focus_since.as_ref().is_some_and(|(_, since)| now - since > FOCUS_GIVE_UP);
        let given_up = stuck && !focused.iter().any(|w| w.3 && !host.fatal.contains(&(w.0, w.1.clone())) && !host.used.contains(&(w.0, w.1.clone())));
        if given_up && minute {
            info!("story autopilot: {focus:?} gone nowhere a while; the others' triggers too");
        }
        let watched = if focus.is_empty() || given_up {
            host.story.watched_triggers_in_graph(graph.as_deref())
        } else {
            host.story.watched_triggers_unowned(graph.as_deref())
        };
        let awaiting_rock = host.story.watched_signals().iter().any(|s| s == "throw_rock");
        // Stuck: triggers stepped into before they were armed go again.
        if host.quiet > 25.0 {
            host.used.retain(|u| !focused.iter().chain(watched.iter()).any(|w| w.0 == u.0 && w.1 == u.1));
        }
        let tails: Vec<u64> = host.story.tail_targets().into_iter().map(|t| t.0).collect();
        // (Not while a companion leads: going on ahead leaves it behind.)
        // (While a companion leads, none; while one follows, only those
        // near it: going on ahead leaves it behind. One told to wait stays
        // where it is: HOY_010's Sam, waiting by the last bomb, kept the
        // player from JAS_200's start across the island.)
        let companions = host.story.companions();
        // (A leader in the world: one whose mission is over and who is gone,
        // its lead never ended, leads no one. JAS_100's Dennis held the
        // autopilot back through DOC_020.)
        let leading = companions.iter().any(|c| matches!(c.1, fc3_story::Companion::Lead { .. }) && actors.iter().any(|(a, _, _)| a.id == c.0 && !a.dead));
        let kept: Vec<Vec3> = actors
            .iter()
            .filter(|(a, _, _)| !a.dead && companions.iter().any(|c| c.0 == a.id && matches!(c.1, fc3_story::Companion::Follow)))
            .map(|(_, t, _)| t.translation)
            .collect();
        // (Unless the story has long been quiet: the companion follows the
        // player, who waits by him for nothing. JAS_220's Riley, to be
        // brought to the chopper, stood with the player 40 m off it.)
        let near_companion = |p: [f32; 3]| kept.is_empty() || host.quiet > NEAR_DESPITE_FIGHT || kept.iter().any(|c| c.distance(to_bevy(p)) < COMPANION_REACH);
        // A door a script waits to see opened goes first: the triggers past
        // it (BAM_060's inside Buck's hut, which lock the door behind him)
        // are for walking through it into.
        let doors_ahead: Vec<Vec3> = host
            .story
            .awaited_doors()
            .into_iter()
            .filter(|d| !doors.walked.contains(d))
            .filter_map(|d| data.entities.get(&d).map(|e| to_bevy(e.pos)))
            .collect();
        let past_door = |p: [f32; 3]| doors_ahead.iter().any(|d| d.distance(to_bevy(p)) < DOOR_REACH);
        // Something a script waits to see destroyed, near (BAM_030's boards
        // across the flooded tunnel): gone up to and broken, as the player
        // would with the machete.
        let here = t.translation;
        let destructible = host
            .story
            .watched_destructibles()
            .into_iter()
            .filter(|(id, _)| !host.used.contains(&(*id, "destroy".to_string())))
            .find(|(_, at)| to_bevy(*at).distance(here) < DESTROY_REACH);
        // A region a running mission waits for the player to leave, him in
        // it (JAS_220's start: out of Hoyt's compound for the airport), the
        // story quiet a while: just outside it.
        let leave = if host.quiet > LEAVE_AFTER && !p.driving && !vines.autopilot_climbing() {
            let feet = to_dunia(t.translation - Vec3::Y * crate::player::HALF_HEIGHT);
            host.story.watched_leaves_of(&focus).into_iter().find_map(|(id, shape)| {
                let key = (id, "Leave".to_string());
                (shape.contains(feet) && !host.used.contains(&key) && !host.fatal.contains(&key))
                    .then(|| exit_point(&shape, feet, |x, y| heights.height_at(to_bevy([x, y, 0.0])).map(|h| to_dunia(Vec3::Y * h)[2])))
                    .flatten()
                    .map(|at| (id, at))
            })
        } else {
            None
        };
        let next = focused.iter().chain(watched.iter()).find(|w| {
            !awaiting_rock
                // (A vine being climbed: its gameplay goes on from the top.)
                && !vines.autopilot_climbing()
                // (Led, the leader's way is followed; but a while quiet, it
                // has brought the player where the script wants him: JAS_100's
                // Dennis, stopped by the outpost's building the player is to
                // go into.)
                && (!leading || host.quiet > LED_STEP_AFTER)
                && near_companion(w.2)
                && !past_door(w.2)
                && w.3
                && !host.used.contains(&(w.0, w.1.clone()))
                && !host.fatal.contains(&(w.0, w.1.clone()))
                && !(w.1 == "EnterFOV" && tails.contains(&w.0))
                && (!p.driving || matches!(w.1.as_str(), "Enter" | "EnterFOV"))
        });
        // (In the stall report: the triggers it may step into and what holds
        // each back.)
        if (host.quiet > 20.0 && host.quiet - dt <= 20.0) || minute {
            info!(
                "story autopilot: trigger step: {} focused (of {focus:?}), {} watched, rock {awaiting_rock}, led {leading} ({:?}), kept near {} companions",
                focused.len(),
                watched.len(),
                companions.iter().filter(|c| matches!(c.1, fc3_story::Companion::Lead { .. })).map(|c| c.0).collect::<Vec<_>>(),
                kept.len()
            );
            for w in focused.iter().chain(watched.iter()) {
                info!(
                    "story autopilot: candidate {} {} {}: near companion {}, past door {}, on {}, used {}, fatal {}, tailed {}",
                    w.0,
                    w.1,
                    data.name_of(w.0).unwrap_or("?"),
                    near_companion(w.2),
                    past_door(w.2),
                    w.3,
                    host.used.contains(&(w.0, w.1.clone())),
                    host.fatal.contains(&(w.0, w.1.clone())),
                    tails.contains(&w.0)
                );
            }
        }
        if p.driving {
            // At the wheel (CIT_020's chase after the convoy): the vehicle
            // goes into the box, the player with it; or before a spot to be
            // seen, facing it. Not while a scene drives the vehicle chased:
            // the player follows (cutting ahead to where it stops stops it
            // short of the ambushes it sets off on the way).
            if let Some((id, event, point, _)) = next.filter(|_| !cinema.drives_vehicle()) {
                info!("story autopilot: drive {} {}", if event == "EnterFOV" { "up to" } else { "into" }, data.name_of(*id).unwrap_or("?"));
                host.used.insert((*id, event.clone()));
                host.last_step = Some((*id, event.clone(), now));
                if event == "EnterFOV" {
                    let at = to_bevy(host.story.look_at_point(*id).unwrap_or(*point));
                    let mut from = at + Vec3::Z * FOV_DRIVE_BACK;
                    from.y = heights.height_at(from).unwrap_or(at.y);
                    drive.0 = Some((from, Some(0.0)));
                } else {
                    let ground = |x: f32, y: f32| heights.height_at(to_bevy([x, y, 0.0]));
                    drive.0 = Some((to_bevy(host.story.trigger_ground_point(*id, ground).unwrap_or(*point)), None));
                }
                host.quiet = 0.0;
            } else if let Some((id, radius, point)) = host
                .story
                .watched_vehicle_proximity(graph.as_deref())
                .into_iter()
                .filter(|w| !host.used.contains(&(w.0, "drive near".to_string())))
                .min_by(|a, b| to_bevy(a.2).distance(t.translation).total_cmp(&to_bevy(b.2).distance(t.translation)))
                .filter(|_| host.quiet > VEHICLE_NEAR_AFTER && !cinema.drives_vehicle())
            {
                // A place a script waits for the vehicle to come near (a supply
                // run's flares, nearest first, and its drop point, against
                // the clock): there.
                info!("story autopilot: drive near {} ({radius} m)", data.name_of(id).unwrap_or("?"));
                host.used.insert((id, "drive near".into()));
                let at = to_bevy(point);
                drive.0 = Some((at.with_y(heights.height_at(at).unwrap_or(at.y)), None));
                host.quiet = 0.0;
            } else if host.quiet > 20.0 && !cinema.drives_vehicle() {
                // Nothing to drive into: on to an objective marker near
                // (SAM_015's, Sam to be brought to a building: its box
                // listens only once the vehicle has stopped there).
                let here = t.translation;
                let marker = host
                    .story
                    .objective_markers()
                    .into_iter()
                    .map(|(id, p)| (id, to_bevy(p)))
                    .filter(|(id, at)| {
                        (MARKER_DRIVE_NEAR..MARKER_DRIVE_FAR).contains(&at.with_y(0.0).distance(here.with_y(0.0)))
                            && !host.used.contains(&(*id, "drive to".to_string()))
                    })
                    .min_by(|a, b| a.1.distance(here).total_cmp(&b.1.distance(here)));
                if let Some((id, at)) = marker {
                    info!("story autopilot: drive to the marker {}", data.name_of(id).unwrap_or("?"));
                    host.used.insert((id, "drive to".into()));
                    drive.0 = Some((at.with_y(heights.height_at(at).unwrap_or(at.y)), None));
                    host.quiet = 0.0;
                }
            }
        } else if let Some((id, at)) = leave {
            info!("story autopilot: Leave {}", data.name_of(id).unwrap_or("?"));
            host.used.insert((id, "Leave".into()));
            host.last_step = Some((id, "Leave".into(), now));
            teleport(&mut t, &mut p, at, 0.0);
            host.quiet = 0.0;
        } else if let Some((id, at)) = destructible {
            info!("story autopilot: breaking {}", data.name_of(id).unwrap_or("?"));
            host.used.insert((id, "destroy".into()));
            teleport(&mut t, &mut p, at, 0.0);
            breaks.break_now(id);
            host.quiet = 0.0;
        } else if let Some((id, event, point, _)) = next {
            info!("story autopilot: {event} {}", data.name_of(*id).unwrap_or("?"));
            host.used.insert((*id, event.clone()));
            host.last_step = Some((*id, event.clone(), now));
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
                // Stand back from the spot and look at it (where it is now:
                // one riding on a character has gone with him).
                let at = to_bevy(host.story.look_at_point(*id).unwrap_or(point));
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
            // Aim at a trigger selecting this guard, then use the normal rock
            // input and projectile. A nearby arbitrary noise cannot prove the
            // guard physically reached the mission's distraction zone.
            let target = watched.iter().filter(|w| w.3 && w.1 == "Enter" && host.story.trigger_selects(w.0, id))
                .min_by(|a, b| to_bevy(a.2).distance_squared(at).total_cmp(&to_bevy(b.2).distance_squared(at)))
                .map(|w| host.story.trigger_ground_point(w.0, |x, y| heights.height_at(to_bevy([x, y, 0.0]))).unwrap_or(w.2))
                .map(to_bevy).unwrap_or(at + (t.translation - at).with_y(0.0).normalize_or_zero() * 3.0);
            info!("story autopilot: aim a rock to distract {} at {target:?}", data.name_of(id).unwrap_or("?"));
            host.used.insert((id, "distract".into()));
            host.distracted_at = time.elapsed_secs();
            if let Some(test) = rock_test.as_mut() { test.0 = Some(target); }
            host.quiet = 0.0;
        } else if let Some((id, radius, point)) = host
            .story
            .watched_proximity_in_graph(graph.as_deref())
            .into_iter()
            // (Again a while later, gone out of its radius since: HOY_010's
            // Sam, defusing a bomb with the player kept near him.)
            .find(|w| {
                // (Not one that failed the mission: DEN_040's camp is a
                // failing zone round HUR_020's briefing spot.)
                !host.fatal.contains(&(w.0, format!("near {}", w.1)))
                    && (!host.used.contains(&(w.0, format!("near {}", w.1)))
                        || (to_bevy(w.2).distance(t.translation) > w.1 + 2.0 && now - host.near_at.get(&w.0).copied().unwrap_or(f32::MIN) > NEAR_AGAIN))
            })
            .filter(|_| tails.is_empty())
            .filter(|_| {
                // (Not with a fight on: a block's counted foes close by, the
                // reinforcements it waits on still coming. Unless the story
                // has long been quiet: JAS_220's last two guards at Hoyt's,
                // out of the autopilot's reach, held it from the airport till
                // the search for Riley ran out of time.)
                let counted = host.story.counted_members();
                host.quiet > NEAR_DESPITE_FIGHT
                    || !actors.iter().any(|(a, at, npc)| {
                        !a.dead && counted.contains(&a.id) && npc.is_some_and(|n| n.hostile()) && at.translation.distance(t.translation) < 200.0
                    })
            })
        {
            // A place a script waits for the player to come near (a quest
            // giver's spot): just inside its radius, on the side he's on
            // (again for a nearer radius: a companion who wants him closer
            // still at the end).
            info!("story autopilot: near {} ({radius} m)", data.name_of(id).unwrap_or("?"));
            host.used.insert((id, format!("near {radius}")));
            host.last_step = Some((id, format!("near {radius}"), now));
            host.near_at.insert(id, now);
            let at = to_bevy(point);
            let side = (t.translation - at).with_y(0.0).normalize_or(Vec3::Z);
            teleport(&mut t, &mut p, to_dunia(at + side * (radius * 0.5).min(1.5)), 0.0);
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
        } else if let Some((id, name)) = host.story.gameplays().into_iter().find(|g| !played_gameplay(&host, g.0, &g.1)) {
            // A minigame or QTE the game doesn't play yet: done. (A sniping
            // challenge is played: its targets shot, see `autopilot_loot`.)
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
            // (Within a tower's height and more: JAS_060's player, put on
            // its top, came down 98 m off and 80 m below the panel.)
            .filter(|at| host.quiet > 12.0 && at.distance(t.translation) < 200.0)
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
        } else if let Some(signal) = host.story.watched_signals().into_iter().find(|s| s != "throw_rock" && !host.used.contains(&(0, s.clone()))) {
            // No trigger left: give an input a script waits for.
            info!("story autopilot: input {signal}");
            host.used.insert((0, signal.clone()));
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
                // (The original HUD's objective box announces it itself.)
                if let Some(t) = text.as_ref().filter(|_| !original_hud.0) {
                    hud.notify_native(format!("New objective: {}", plain(t)));
                }
                ui.objective = text.map(|t| plain(&t));
            }
            Command::MissionTitle { title, .. } => {
                if let Some(t) = title {
                    ui.banner = Some((plain(&t), 5.0));
                    ui.mission = Some(plain(&t));
                    ui.cards.push(MissionCard::Start { title: plain(&t) });
                }
            }
            Command::MissionComplete { title, xp, money, .. } => {
                // (A mission without its own text for it is named as it
                // started.)
                let name = title.map(|t| plain(&t)).filter(|t| !t.is_empty()).or_else(|| ui.mission.clone()).unwrap_or_default();
                ui.banner = Some((format!("{name}\nMission complete"), 6.0));
                ui.cards.push(MissionCard::Complete { title: name.clone() });
                if xp > 0 {
                    xp_gain.write(crate::progression::XpGain { amount: xp, reason: "Mission complete" });
                }
                if money > 0 {
                    let got = progress.add_money(money);
                    hud.notify_native(format!("+${got}"));
                }
            }
            Command::MissionFailed { text, reload } => {
                info!("story: mission failed ({:?}), reload {reload}", text.as_deref().map(plain));
                if let Some((id, event, _)) = host.last_step.take().filter(|s| now - s.2 < FATAL_WITHIN) {
                    info!("story autopilot: {event} {} failed the mission; left alone from now", host.story.data().name_of(id).unwrap_or("?"));
                    learn_fatal(&mut host.fatal, id, event);
                }
                let text = text.map(|t| plain(&t)).unwrap_or_default();
                ui.banner = Some((format!("Mission failed\n{text}"), 5.0));
                ui.cards.push(MissionCard::Failed { text });
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
                host.checkpoint_game_pending = true;
                host.checkpoint_progress = Some(progress.clone());
                ui.checkpoint_reached = true;
            }
            Command::MissionState { name, state, block } => {
                info!("story: {name} {state}");
                // A mission done opens the abilities it gates.
                if block.is_none() && state == "Completed" && !progress.missions.contains(&name.to_ascii_lowercase()) {
                    // (Counted for the handbook's progress by its type.)
                    if let Some((kind, giver)) = host.story.mission_type(&name) {
                        crate::statistics::mission_done(&mut progress, &kind, giver);
                    }
                    progress.missions.insert(name.to_ascii_lowercase());
                }
                if block.is_some() && state == "Completed" {
                    host.last_completed = Some(name);
                    host.scene_in_last = std::mem::take(&mut host.scene_in_block);
                }
            }
            // A script's `Door` box locks a swing door, and unlocks it.
            Command::EntityEvent { entity, event } if event == "CDoor_Disable" => {
                doors.locked.insert(entity);
            }
            Command::EntityEvent { entity, event } if event == "CDoor_Enable" => {
                doors.locked.remove(&entity);
            }
            // A script's `StimsEmitter` box switches a stims emitter.
            Command::EntityEvent { entity, event } if event == "CStimsEmitterComponent_Enable" || event == "CStimsEmitterComponent_Disable" => {
                stims.switched.push((entity, event.ends_with("_Enable")));
            }
            // A compound object kept whole against harm and let go
            // (`CompoundObjectModifier`'s SetUnDamageable and
            // SetDamageable: DEN_040's caches, the temple until it
            // collapses), or broken on to its next state (NextState).
            Command::EntityEvent { entity, event } if event == "CCompoundPhysComponent_SetAsNotDamageable" || event == "CCompoundPhysComponent_SetAsDamageable" => {
                breaks.set_immune(entity, event.ends_with("NotDamageable"));
            }
            Command::EntityEvent { entity, event } if event == "CCompoundPhysComponent_ChangeState" => {
                breaks.break_now(entity);
            }
            // A placed particle system a script starts, or stops, pauses or
            // cleans away (`ParticleSystem`).
            Command::EntityEvent { entity, event } if event.starts_with("CNewParticlesComponent_") => {
                debug!("story: effect {entity} {event}");
                host.fx_switched.insert(entity, event.ends_with("_PlayParticles"));
            }
            // A light a script switches (`DynamicLightModifier`).
            Command::EntityEvent { entity, event } if event == "CDynamicLightComponent_ActivateLight" || event == "CDynamicLightComponent_DeactivateLight" => {
                host.lights_switched.insert(entity, event.ends_with("_ActivateLight"));
            }
            // A vehicle a script blows up (`VehicleModifier`'s SetExplosion).
            Command::EntityEvent { entity, event } if event == "CVehicle_SetExplosion" => {
                host.vehicle_explosions.push(entity);
            }
            // A character (or the player) made invincible and back
            // (`PawnInvincibleState`).
            Command::EntityEvent { entity, event } if event == "CFCXCountersComponent_SetInvincible" || event == "CFCXCountersComponent_UnsetInvincible" => {
                host.invincible.insert(entity, event.ends_with("_SetInvincible"));
            }
            // Removed by a script: gone from the world (the story
            // remembers it in its saves).
            Command::Remove(id) => {
                hidden.0.insert(id);
            }
            Command::Teleport { entity, pose } => {
                if entity == fc3_story::PLAYER {
                    info!("story: a script moves the player to {:?}", pose.position);
                    teleport(&mut t, &mut p, pose.position, pose.angles[2]);
                } else {
                    placements.set(entity, pose);
                }
            }
            Command::RespawnPoint(spawn) => {
                // A cinematic block hands over where its scene left the
                // player: there, when its scene didn't play (one that did
                // left him where it ended).
                if host.last_completed.as_deref().is_some_and(|n| n.ends_with('C')) && !host.scene_in_last {
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
                // (Not an animal's: a farm's pig sent to eat and sniff is
                // no one to follow. JAS_100's player was taken from Dennis,
                // leading him to the outpost, to the pig, and back, for
                // good.)
                let animal = data
                    .entities
                    .get(&pawn)
                    .is_some_and(|e| e.component("CAnimalNetworkComponent").is_some() || e.component("CFCXCountersComponentAnimal").is_some());
                if action.is_some() && !animal {
                    host.last_ordered = Some(pawn);
                }
                orders.0.insert(pawn, action);
            }
            Command::Print(text) => debug!("story: {text}"),
            Command::Message(m) if m == "PhoneCallEnd" => ui.phone = None,
            Command::Native { name, args } => {
                // (RUST_LOG=story_natives=debug: every native the host is asked.)
                debug!(target: "story_natives", "{name} {args:?}");
                let a = |i: usize| args.get(i).cloned().unwrap_or_default();
                if name == "CreateSpecificGameplay" {
                    let params = args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
                    host.gameplay_params.insert(a(0).parse().unwrap_or(0), params);
                }
                match name.as_str() {
                    "OverridePauseMenu" | "OverridePauseMenuTutorial" => {
                        pause_menu.override_page(a(1) == "true", a(2) == "true", name == "OverridePauseMenuTutorial");
                        if a(0) == "true" { next.set(GameState::Journal); }
                    }
                    // `group, battlefield, kind`: a character who isn't a
                    // fighter (Oliver being freed) goes to the battlefield
                    // he is given to be at (kind 0), as an action with no
                    // move of its own. (Fighters keep to their own AI.)
                    "SetBattlefields" if a(2) == "0" => {
                        if let Ok(field) = a(1).parse::<u64>() {
                            let data = host.story.data();
                            if let Some(region) = data.entities.get(&field) {
                                // (To the battlefield's strategic point, where
                                // its characters take up their places, else
                                // its middle.)
                                let at = region.pos;
                                let spot = data
                                    .entities
                                    .values()
                                    .filter(|e| e.class == "CStrategicPoint")
                                    .map(|e| (e.id, (e.pos[0] - at[0]).hypot(e.pos[1] - at[1])))
                                    .filter(|(_, d)| *d < STRATEGIC_REACH)
                                    .min_by(|a, b| a.1.total_cmp(&b.1))
                                    .map_or(field, |(id, _)| id);
                                for pawn in host.story.group_members(&a(0)) {
                                    if crate::story_actors::walks_to_battlefields(&data, pawn) && host.story.current_action(pawn).is_none() {
                                        debug!("story: {pawn} to battlefield {field} ({spot})");
                                        orders.0.insert(pawn, Some(spot));
                                    } else if crate::story_actors::fights_beside_player(&data, pawn) {
                                        // (An ally's post moves there: the
                                        // player's foes are its AI's own.)
                                        host.battlefield_posts.insert(pawn, spot);
                                    }
                                }
                            }
                        }
                    }
                    // `point, mode, battlefield, vehicle id=archetype (or
                    // nothing), id=archetype...` (the mercs)
                    "SpawnReinforcement" => {
                        let pair = |m: &String| -> Option<(u64, u64)> {
                            let (id, ark) = m.split_once('=')?;
                            Some((id.parse().ok()?, u64::from_str_radix(ark, 16).ok()?))
                        };
                        let point = a(0).parse().unwrap_or(0);
                        let mercs: Vec<(u64, u64)> = args.iter().skip(4).filter_map(pair).collect();
                        // (A point's army is its mercs': G3_SC_05's ducks.)
                        host.called_from.extend(mercs.iter().map(|(id, _)| (*id, point)));
                        host.reinforcements.push(crate::story_actors::Reinforcement {
                            point,
                            battlefield: a(2).parse().ok().filter(|&b| b != u64::MAX),
                            vehicle: args.get(3).and_then(pair),
                            mercs,
                            relaxed: a(1) == "0",
                        });
                    }
                    "LogEventInAIDiary" => host.ai_events.extend(a(0).parse::<u64>().ok()),
                    "CAPI_Entity.SendDetonateEvent" => host.detonations.extend(a(0).parse::<u64>().ok()),
                    // `pawn, origin, amount`
                    "CAPI_Pawn.DamagePawn" => {
                        if let (Ok(pawn), Ok(amount)) = (a(0).parse::<u64>(), a(2).parse::<f32>()) {
                            // A killing blow right after the autopilot
                            // stepped into a trigger (a fall-death zone):
                            // that trigger is left alone from now.
                            if pawn == fc3_story::PLAYER && amount >= KILLING_BLOW {
                                if let Some((id, event, _)) = host.last_step.take().filter(|s| now - s.2 < FATAL_WITHIN) {
                                    info!("story autopilot: {event} {} killed the player; left alone from now", host.story.data().name_of(id).unwrap_or("?"));
                                    learn_fatal(&mut host.fatal, id, event);
                                }
                            }
                            host.damages.push((pawn, amount));
                        }
                    }
                    // `pawn, origin, health` (the `HealthModifier` box's Set:
                    // a beaten civilian left weak, a cage's door).
                    "CAPI_Pawn.SetPawnHealth" => {
                        if let (Ok(pawn), Ok(health)) = (a(0).parse::<u64>(), a(2).parse::<f32>()) {
                            host.health_sets.push((pawn, health));
                        }
                    }
                    // `id, archetype, x, y, z, angles`
                    "SpawnEntityFromArchetype" => {
                        let f = |i: usize| a(i).parse::<f32>().unwrap_or(0.0);
                        if let Ok(id) = a(0).parse::<u64>() {
                            host.spawned_props.push((id, a(1), [f(2), f(3), f(4)], [f(5), f(6), f(7)]));
                        }
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
                        // The screen it plays on: an entity and its material.
                        screen: a(1).parse::<u64>().ok().filter(|&id| id != fc3_story::PLAYER).map(|id| (id, a(2))),
                        backdrop: false,
                    }),
                    // Specific gameplays own their playback and completion.
                    // (A gameplay of its own name playing a QTE move is one
                    // too: JAS_150's escape from the ropes.)
                    "CreateSpecificGameplay" if a(1) == "QTEGSF" || args.iter().skip(2).any(|p| p.starts_with("QTE_move_file=")) => {
                        qtes.pending.push(crate::qte::QteStart {
                            id: a(0).parse().unwrap_or(0),
                            params: args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                        })
                    }
                    // (`FC3_VINE_GAMEPLAY_OFF`: left to the autopilot to end, as
                    // before the transition was played.)
                    "CreateSpecificGameplay" if a(1) == "CVineClimbingGameplay" && crate::vine::played() => vines.pending.push(crate::vine::Start {
                        id: a(0).parse().unwrap_or(0),
                        params: args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                    }),
                    "CreateSpecificGameplay" if a(1) == CHOPPER_FIGHT => chopper.pending.push(a(0).parse().unwrap_or(0)),
                    "CreateSpecificGameplay" if shooting_gallery(&a(1)) => gallery.pending.push(crate::shooting_gallery::Start {
                        id: a(0).parse().unwrap_or(0),
                        name: a(1),
                        params: args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                    }),
                    "CreateSpecificGameplay" if crate::races::race_gameplay(&a(1)) => races.pending.push(crate::races::Start {
                        id: a(0).parse().unwrap_or(0),
                        name: a(1),
                        params: args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                    }),
                    // A race's (or a gallery's) keeper calling the player over.
                    "CreateSpecificGameplay" if crate::races::entice_gameplay(&a(1)) => {
                        let param = |k: &str| args.iter().skip(2).find_map(|p| p.strip_prefix(k).and_then(|v| v.strip_prefix('=')));
                        if let Some(keeper) = param("EnticeSystemSettings/hidChallengeKeeper/EntityID").and_then(|v| v.parse().ok()) {
                            races.entices.retain(|e| e.keeper != keeper);
                            races.entices.push(crate::races::Entice {
                                id: a(0).parse().unwrap_or(0),
                                keeper,
                                set: param("hidAIDialogIndex").and_then(|v| v.parse().ok()).unwrap_or(0),
                                gallery: a(1) == crate::races::GALLERY_ENTICE,
                            });
                        }
                    }
                    "CreateSpecificGameplay" if a(1) == CAR_CHASE => car_chase.pending.push(crate::car_chase::Start {
                        id: a(0).parse().unwrap_or(0),
                        params: args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                    }),
                    "CreateSpecificGameplay" if a(1) == INPUT_PROMPTS => {
                        let params = args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
                        prompts.start(a(0).parse().unwrap_or(0), &params);
                    }
                    "CreateSpecificGameplay" if a(1) == INTOXICATED => intoxication.pending.push(crate::intoxication::Start {
                        id: a(0).parse().unwrap_or(0),
                        params: args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                    }),
                    "CreateSpecificGameplay" if a(1) == BOSS_FIGHT => boss_fight.pending.push(crate::boss_fight::Start {
                        id: a(0).parse().unwrap_or(0),
                        params: args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                    }),
                    "EnableUnlimitedAmmo" => unlimited_ammo.0 = a(0) == "true" || a(0) == "1",
                    // Bound to another (on one of its bones): `parent, child, bone`.
                    "CAPI_Entity.Bind" => {
                        if let (Ok(parent), Ok(child)) = (a(0).parse(), a(1).parse()) {
                            binds.bind(parent, child, &a(2));
                        }
                    }
                    "CAPI_Entity.UnBind" => {
                        if let Ok(child) = a(1).parse() {
                            binds.unbind(child);
                        }
                    }
                    "CreateSpecificGameplay" if a(1).starts_with("CSnipingChallenge") => sniping.pending.push(crate::sniping::Start {
                        id: a(0).parse().unwrap_or(0),
                        name: a(1),
                        params: args.iter().skip(2).filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                    }),
                    // A script decides a QTE: `QteSetResult <id> success|failure`.
                    "SendGSFGenericEventToEntity" if a(2) == "QteSetResult" => {
                        qtes.decided.push((a(3).parse().unwrap_or(0), a(4).eq_ignore_ascii_case("success")));
                    }
                    // A gallery's spots (its shooter's, its birds' boxes)
                    // registering, to the player: `<player>
                    // SpawnPointRegistration <kind> <spot>`.
                    "SendGSFGenericEventToEntity" if a(1) == "SpawnPointRegistration" => {
                        if let Ok(spot) = a(3).parse::<u64>() {
                            if !gallery.spawn_points.iter().any(|(_, s)| *s == spot) {
                                gallery.spawn_points.push((a(2).to_string(), spot));
                            }
                        }
                    }
                    // A race's gates registering, to the player: `<player>
                    // GateRegisterEvent <gate> <bone> <bone> <index> <tolerance>`.
                    // A trial's clock started, or the trial failed (its
                    // scripts: off the zone too long).
                    "StartChallengeTimer" | "NotifyChallengerFailure" => trials.natives.push((name.clone(), a(0).parse().unwrap_or(0))),
                    "SendGSFGenericEventToEntity" if a(1) == "GateRegisterEvent" => {
                        if let (Ok(id), Ok(index)) = (a(2).parse::<u64>(), a(5).parse::<u32>()) {
                            races.gates.retain(|g| g.id != id);
                            races.gates.push(crate::races::GateRegistration { id, index, tolerance: a(6).parse().unwrap_or(1.0) });
                        }
                    }
                    // A pulse of the pad (`Rumble <low> <high> <seconds>`:
                    // JAS_020's Grant's death scene, with each heartbeat).
                    "SendGSFGenericEventToEntity" if a(2) == "Rumble" => {
                        let f = |i| a(i).parse().unwrap_or(0.0f32);
                        shakes.write(crate::shake::Shake::pulse(f(3), f(4), f(5)));
                    }
                    "SendGSFGenericEventToEntity" => {
                        intoxication.events.push(a(2).to_string());
                        host.gsf_events.push(a(2).to_string());
                    }
                    "PopUpEndOfGame" => host.end_of_game = true,
                    "DestroySpecificGameplay" => {
                        let id = a(0).parse().unwrap_or(0);
                        qtes.cancelled.push(id);
                        vines.cancelled.push(id);
                        sniping.cancelled.push(id);
                        boss_fight.cancelled.push(id);
                        intoxication.cancelled.push(id);
                        prompts.end(id);
                        chopper.cancelled.push(id);
                        car_chase.cancelled.push(id);
                        gallery.cancelled.push(id);
                        races.cancelled.push(id);
                    }
                    // The scripts' sound mixings, by name.
                    "StartSoundMixingFromLua" => mixings.start(&a(0)),
                    "StopSoundMixingFromLua" => mixings.stop(&a(0)),
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
                        ui.hints.insert(a(0).parse().unwrap_or(0), (plain(&a(2)), plain(&a(3))));
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
                    // A HUD popup (the mission area's warning as the player
                    // leaves it): the game's string by its id in the hint
                    // box, until hidden (-1) or its timer (s) runs out.
                    "DisplayCustomUIMsg" => {
                        let text = a(0).parse::<u32>().ok().and_then(|id| host.story.data().strings.get(id).map(plain));
                        match text.filter(|_| a(1) == "true" || a(1) == "1") {
                            Some(text) => {
                                ui.hints.insert(POPUP_HINT, (String::new(), text));
                                if let Some(secs) = a(2).parse::<f32>().ok().filter(|s| *s > 0.0) {
                                    host.due.push((now + secs, Due::HidePopup));
                                }
                            }
                            None => {
                                ui.hints.remove(&POPUP_HINT);
                            }
                        }
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
                        if a(0) == fc3_story::PLAYER.to_string() { postfx.request(&a(1), on); }
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
                    // One of the player's camera shakes and pad rumbles.
                    "CAPI_Player.CameraShakeAndGamePadRumble" if a(0) == fc3_story::PLAYER.to_string() => {
                        shakes.write(crate::shake::Shake::new(&a(1), a(2).parse().unwrap_or(1.0)));
                    }
                    // The arm's tattoo shown or hidden.
                    "CAPI_Player.ShowTattoo" if a(0) == fc3_story::PLAYER.to_string() => {
                        let show = a(1) != "false" && a(1) != "0";
                        if tattoo.0 != show {
                            tattoo.0 = show;
                        }
                    }
                    // The scripts open an ability in the skill tree.
                    "CAPI_Player.SetAbilityUpgradable" if a(0) == fc3_story::PLAYER.to_string() => {
                        progress.upgradable.insert(a(1));
                    }
                    // A script group opened (shop items, handbook pages).
                    "UnlockItemsFromScript" => {
                        progress.unlocks.insert(a(0));
                    }
                    // Rewards and fees the scripts hand out.
                    "CAPI_Player.GivePlayerXP" | "CAPI_Player.GivePlayerOutpostXPBonus" if a(0) == fc3_story::PLAYER.to_string() => {
                        let amount = a(1).parse::<f64>().unwrap_or(0.0).max(0.0) as u32;
                        if amount > 0 {
                            let reason = if name == "CAPI_Player.GivePlayerXP" { "Bonus" } else { "Outpost bonus" };
                            xp_gain.write(crate::progression::XpGain { amount, reason });
                        }
                    }
                    "CAPI_Player.AddPlayerMoney" | "CAPI_Player.WithdrawPlayerMoney" | "CAPI_Player.SetPlayerMoney" if a(0) == fc3_story::PLAYER.to_string() => {
                        let amount = a(1).parse::<f64>().unwrap_or(0.0).max(0.0) as u32;
                        match name.as_str() {
                            "CAPI_Player.AddPlayerMoney" => {
                                progress.add_money(amount);
                            }
                            "CAPI_Player.WithdrawPlayerMoney" => progress.money = progress.money.saturating_sub(amount),
                            _ => progress.money = amount.min(progress.money_cap()),
                        }
                    }
                    // The camera drawn by a script, and put away.
                    "CAPI_Player.DrawGadget" if a(0) == fc3_story::PLAYER.to_string() && a(1).eq_ignore_ascii_case("binoculars") => {
                        scripted.camera_drawn = scripted.has_camera();
                    }
                    "CAPI_Pawn.Holster" | "CAPI_Pawn.ForceHolster" if a(0) == fc3_story::PLAYER.to_string() => scripted.camera_drawn = false,
                    // The player's field of view set, or given back (`player,
                    // degrees, seconds, on`).
                    // The player's stance (`player, stance`: 0 standing, 1
                    // crouched; the swimming and lying ones are the
                    // engine's own) and his sprint stopped.
                    "CAPI_Pawn.SetPawnStance" if a(0) == fc3_story::PLAYER.to_string() => match a(1).parse::<f32>().map(|s| s as u8) {
                        Ok(0) => scripted.stance = Some(false),
                        Ok(1) => scripted.stance = Some(true),
                        _ => {}
                    },
                    "CAPI_Player.SetSprinting" if a(0) == fc3_story::PLAYER.to_string() && matches!(a(1).as_str(), "false" | "0") => {
                        scripted.break_sprint = true;
                    }
                    // The player's air (`player, use oxygen, level 0..1`).
                    "CAPI_Player.SetOxygenLevel" if a(0) == fc3_story::PLAYER.to_string() => {
                        scripted.no_drowning = a(1) == "false" || a(1) == "0";
                        scripted.oxygen = Some(a(2).parse::<f32>().unwrap_or(1.0).clamp(0.0, 1.0));
                        debug!("story: oxygen {:?}{}", scripted.oxygen, if scripted.no_drowning { " (no drowning)" } else { "" });
                    }
                    "CAPI_Player.SetPlayerFOV" if a(0) == fc3_story::PLAYER.to_string() => {
                        let on = a(3) == "1" || a(3) == "true";
                        scripted.fov = (on.then(|| a(1).parse::<f32>().ok()).flatten(), a(2).parse().unwrap_or(0.0));
                    }
                    // The player shown elsewhere on the maps (`on, x, y, z`).
                    "OverridePlayerPosition" => {
                        let f = |i: usize| a(i).parse::<f32>().unwrap_or(0.0);
                        scripted.map_position = (a(0) == "true" || a(0) == "1").then(|| to_bevy([f(1), f(2), f(3)]));
                    }
                    "CAPI_Pawn.SetWeaponVisibility" if a(0) == fc3_story::PLAYER.to_string() => {
                        scripted.weapon_hidden = !(a(1) == "true" || a(1) == "1");
                    }
                    // A placed object's light switch: an outpost's flag in
                    // the rebels' colours as it's taken.
                    "CAPI_Entity.LightMaterialOveride" => {
                        if let Ok(id) = a(0).parse::<u64>() {
                            if a(1) == "true" || a(1) == "1" {
                                light_switches.0.insert(id);
                            } else {
                                light_switches.0.remove(&id);
                            }
                        }
                    }
                    // A script's success screen (an outpost's flag
                    // controller, as it's taken): the game's string as a card.
                    "DisplaySuccessScreenUI" => {
                        if let Ok(text) = a(0).parse::<u32>() {
                            ui.cards.push(MissionCard::Success { text });
                        }
                    }
                    "CAPI_Pawn.SetNoWeaponMode" if a(0) == fc3_story::PLAYER.to_string() => {
                        scripted.no_weapon = a(1) == "true" || a(1) == "1";
                        debug!("story: weapon {}", if scripted.no_weapon { "put away" } else { "back" });
                    }
                    "CAPI_Pawn.SetAutoDraw" if a(0) == fc3_story::PLAYER.to_string() => {
                        scripted.auto_draw = a(1) == "1" || a(1) == "true";
                        if let (true, Some(def), Ok(mut inv)) = (scripted.auto_draw, scripted.last_granted.take(), inventory.single_mut()) {
                            if let Some(i) = inv.slots.iter().position(|s| s.def == def) {
                                inv.current = i;
                            }
                            scripted.redraw = true;
                        }
                    }
                    "CAPI_Player.AddArmor" if a(0) == fc3_story::PLAYER.to_string() => {
                        progress.armour = progress.armour.max(a(1).parse::<f32>().unwrap_or(0.0));
                    }
                    "CAPI_Player.EnableWholeTattoo" if a(0) == fc3_story::PLAYER.to_string() => progress.whole_tattoo = true,
                    // Abilities lent for a while, by their bonus plan.
                    "CAPI_Player.AddBonusPlan" if a(0) == fc3_story::PLAYER.to_string() => {
                        progress.removed_plans.remove(&a(1).to_ascii_lowercase());
                        progress.bonus_plans.insert(a(1).to_ascii_lowercase());
                    }
                    "CAPI_Player.RemoveBonusPlan" if a(0) == fc3_story::PLAYER.to_string() => {
                        progress.bonus_plans.remove(&a(1).to_ascii_lowercase());
                        progress.removed_plans.insert(a(1).to_ascii_lowercase());
                    }
                    // Loot a mission needs kept back from the shop (`player,
                    // item, how many`), and let go.
                    "CAPI_Pawn.AddProtectedLootItem" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let (Ok(id), Ok(n)) = (a(1).parse::<f64>(), a(2).parse::<f64>()) {
                            progress.protected_loot.insert(id as u32, n.max(0.0) as u32);
                        }
                    }
                    "CAPI_Pawn.RemoveProtectedLootItem" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let Ok(id) = a(1).parse::<f64>() {
                            progress.protected_loot.remove(&(id as u32));
                        }
                    }
                    "CAPI_Player.RemoveLootItem" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let (Ok(id), Ok(n)) = (a(1).parse::<f64>(), a(2).parse::<f64>()) {
                            let id = id as u32;
                            let left = progress.loot.get(&id).copied().unwrap_or(0).saturating_sub(n.max(0.0) as u32);
                            if left == 0 {
                                progress.loot.remove(&id);
                            } else {
                                progress.loot.insert(id, left);
                            }
                        }
                    }
                    "CAPI_Player.GiveAmmoToPlayer" if a(0) == fc3_story::PLAYER.to_string() => {
                        // Magazines for every gun carried.
                        if let Ok(mut inv) = inventory.single_mut() {
                            let clips = a(1).parse::<f64>().unwrap_or(1.0).max(0.0) as u32;
                            let classes: Vec<(crate::weapons::WeaponClass, u32)> = inv.slots.iter().map(|s| (inv.defs[s.def].class, inv.defs[s.def].magazine)).collect();
                            for (class, magazine) in classes {
                                inv.add_ammo(class, magazine * clips);
                            }
                        }
                    }
                    "CAPI_Player.RefillGadgetAmmo" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let Ok(mut inv) = inventory.single_mut() {
                            match a(1).to_ascii_lowercase().as_str() {
                                "grenade" => inv.grenades = inv.grenades.max(perks.grenade_cap),
                                "molotov" => inv.molotovs = inv.molotovs.max(crate::weapons::MOLOTOV_CAP),
                                "c4" => inv.c4 = inv.c4.max(perks.explosive_cap),
                                "mine" => inv.mines = inv.mines.max(perks.explosive_cap),
                                _ => {}
                            }
                        }
                    }
                    "CAPI_Pawn.HealPawn" if a(0) == fc3_story::PLAYER.to_string() => {
                        heal = true;
                    }
                    // The player's ammunition topped up: every weapon's
                    // (no weapon named), else that weapon's, to the count
                    // given or its most.
                    "CAPI_Pawn.RefillWeaponAmmo" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let Ok(mut inv) = inventory.single_mut() {
                            let only = Some(a(1)).filter(|w| !w.is_empty()).and_then(|w| host.weapon_def(&inv, &w));
                            let count = a(2).parse::<u32>().ok().filter(|&n| n > 0);
                            let inv = &mut *inv;
                            for slot in inv.slots.iter_mut().filter(|s| only.is_none_or(|d| s.def == d)) {
                                let d = &inv.defs[slot.def];
                                slot.mag = d.magazine;
                                slot.reserve = count.unwrap_or(d.max_reserve).min(d.max_reserve.max(1));
                            }
                        }
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
                            // (By the shop's models too: a trial's
                            // `weapons:FC3/STGW-90`, the game's STG-90.)
                            if let Some(def) = host.weapon_def(&inv, &a(1)) {
                                let wname = inv.defs[def].name;
                                crate::weapons::grant_weapon(&mut inv, wname, 4);
                                scripted.last_granted = Some(def);
                                if scripted.auto_draw {
                                    if let Some(i) = inv.slots.iter().position(|s| s.def == def) {
                                        inv.current = i;
                                    }
                                }
                                // (Known to the story by the script's name:
                                // what waits for it to spawn hears it.)
                                if a(1).contains('.') {
                                    scripted.granted.insert(def, a(1));
                                }
                            } else {
                                // One the game carries as no weapon of its own
                                // (the IED's remote: the charges it sets off):
                                // carried as far as the story can tell, so what
                                // waits for it to spawn hears it.
                                if a(1).to_ascii_lowercase().contains("ied") {
                                    inv.c4 = (inv.c4 + 1).min(perks.explosive_cap.max(1));
                                }
                                scripted.gadgets.insert(a(1));
                            }
                        }
                    }
                    // A weapon the player lets go of (the `DropWeapon` box:
                    // WIL_020's briefing makes room for the flamethrower so;
                    // by the id scripts know it by), as one taken back.
                    "CAPI_Pawn.RemoveWeapon" | "CAPI_Pawn.DropWeapon" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let Ok(mut inv) = inventory.single_mut() {
                            if let Some(def) = weapon_def(&inv, &host.story.item_name(&a(1)), &data) {
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
                    // An entity resized (`entity, scale`).
                    "CAPI_Entity.SetScale" => {
                        if let (Ok(id), Ok(scale)) = (a(0).parse::<u64>(), a(1).parse::<f32>()) {
                            placements.set_scale(id, scale);
                        }
                    }
                    // The player's controls narrowed to an input action map.
                    "CAPI_Player.PushPlayerActionMap" if a(0) == fc3_story::PLAYER.to_string() => action_maps.push(&a(1)),
                    "CAPI_Player.RemovePlayerActionMap" if a(0) == fc3_story::PLAYER.to_string() => action_maps.remove(&a(1)),
                    "CAPI_Player.SetPlayerActionMap" if a(0) == fc3_story::PLAYER.to_string() => action_maps.set(&a(1)),
                    "CAPI_Player.LockPlayerActionMap" if a(0) == fc3_story::PLAYER.to_string() => action_maps.lock(&a(1)),
                    "CAPI_Player.UnlockPlayerActionMap" if a(0) == fc3_story::PLAYER.to_string() => action_maps.unlock(),
                    // A move played on a character by name, and stopped.
                    "CAPI_AI.StartPlayAnim" => {
                        if let Ok(pawn) = a(0).parse::<u64>() {
                            pawn_anims.requests.push(crate::pawn_anims::Request::Play { pawn, name: a(1), looping: a(2) == "true", quiet: false, done: None });
                        }
                    }
                    "CAPI_AI.StopPlayAnim" => {
                        if let Ok(pawn) = a(0).parse::<u64>() {
                            pawn_anims.requests.push(crate::pawn_anims::Request::Stop { pawn });
                        }
                    }
                    // The player turned to look at an entity.
                    "CAPI_Player.SetPlayerLookAngles" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let Some(at) = a(1).parse::<u64>().ok().and_then(|id| data.position(id)).map(to_bevy) {
                            let d = at - (t.translation + Vec3::Y * (p.eye - crate::player::HALF_HEIGHT));
                            p.yaw = (-d.x).atan2(-d.z);
                            p.pitch = d.y.atan2(Vec2::new(d.x, d.z).length());
                        }
                    }
                    // Turned to angles (`player, pitch, roll, yaw`, degrees;
                    // the yaw as an entity's, about up from north).
                    "CAPI_Player.SetPlayerLookAnglesFromAngles" if a(0) == fc3_story::PLAYER.to_string() => {
                        if let (Ok(pitch), Ok(yaw)) = (a(1).parse::<f32>(), a(3).parse::<f32>()) {
                            p.pitch = pitch.to_radians().clamp(-1.5, 1.5);
                            p.yaw = yaw.to_radians();
                        }
                    }
                    // A character's army: the player's foe (red), on his
                    // side (grey) or out of the fight (neutral).
                    "CAPI_NPC.SwitchArmy" => {
                        if let (Ok(id), Ok(army)) = (a(0).parse::<u64>(), a(1).parse::<u8>()) {
                            debug!("story: {id} to army {army}");
                            host.armies.insert(id, army);
                        }
                    }
                    // A pawn's hostility or alliance with armies set or
                    // cleared (`pawn, mask`: blue 1, red 2, grey 4, neutral
                    // 8, green 16, yellow 32).
                    "CAPI_NPC.SetHostileWithArmyMask"
                    | "CAPI_NPC.ClearHostileWithArmyMask"
                    | "CAPI_NPC.SetAlliedWithArmyMask"
                    | "CAPI_NPC.ClearAlliedWithArmyMask" => {
                        if let (Ok(id), Ok(mask)) = (a(0).parse::<u64>(), a(1).parse::<f32>()) {
                            let hostile = name.contains("Hostile");
                            let set = name.starts_with("CAPI_NPC.Set");
                            host.relations.entry(id).or_default().change(hostile, set, mask as u8);
                        }
                    }
                    // How much of other characters' gunfire a group takes
                    // (`group, level`: 0 none to 4 at once; 5 the level
                    // again, 6 all of it).
                    "NPC_SetDamage" => {
                        let level = a(1).parse::<f32>().unwrap_or(3.0) as u8;
                        for id in host.story.group_members(&a(0)) {
                            let entry = host.npc_damage.entry(id).or_insert((3, true));
                            match level {
                                5 => entry.1 = true,
                                6 => entry.1 = false,
                                l => *entry = (l, true),
                            }
                        }
                    }
                    // A character vehicles can't hurt by ramming (`pawn, on`).
                    "CAPI_Entity.SetImmuneToCollisions" => {
                        if let Ok(id) = a(0).parse::<u64>() {
                            if matches!(a(1).as_str(), "true" | "1") {
                                host.collision_immune.insert(id);
                            } else {
                                host.collision_immune.remove(&id);
                            }
                        }
                    }
                    // An order called out by someone of an army (`army,
                    // group, action`: 0 regroup, 1 disperse).
                    "PlayAIContextDialog" => {
                        let army = a(0).parse::<f32>().unwrap_or(1.0) as u8;
                        let group = if a(1) == fc3_domino::INVALID_ID { Vec::new() } else { host.story.group_members(&a(1)) };
                        let stimulus = if a(2).parse::<f32>().unwrap_or(0.0) as u8 == 1 { "inform_order_disperse" } else { "inform_order_regroup" };
                        host.order_calls.push((army, group, stimulus));
                    }
                    // A group told to look for cover afresh
                    // (`SetReEvaluateCover`: `group, fact, true`).
                    "SetAIFactBool" if a(1) == "ReEvaluateCover" => {
                        let members = host.story.group_members(&a(0));
                        host.recover.extend(members);
                    }
                    // An alarm box a script switched (`id, state`).
                    "SetAlarmState" => {
                        if let (Ok(id), Ok(state)) = (a(0).parse::<u64>(), a(1).parse::<u8>()) {
                            host.alarm_sets.push((id, state));
                        }
                    }
                    // A group's senses held (`PawnSetImpairedState`: 0 not,
                    // 1 fully, a captive tied up; 2 minimally, one held at
                    // gunpoint).
                    "SetImpairedState" => {
                        let state = a(1).parse::<f32>().unwrap_or(0.0) as u8;
                        for id in host.story.group_members(&a(0)) {
                            debug!("story: {id} impaired {state}");
                            host.impaired.insert(id, state);
                        }
                    }
                    // Tagging allowed on a character (1) or not (0, its tag
                    // taken off), or its tag put on at once.
                    "CAPI_Entity.ChangeTagActivation" => {
                        if let Ok(id) = a(0).parse::<u64>() {
                            let on = !matches!(a(1).as_str(), "0" | "false");
                            debug!("story: {id} taggable {on}");
                            host.taggable.insert(id, on);
                            if !on {
                                host.tag_now.remove(&id);
                            }
                        }
                    }
                    "CAPI_Entity.ValidateTag" => {
                        if let Ok(id) = a(0).parse::<u64>() {
                            debug!("story: {id} tagged");
                            host.tag_now.insert(id);
                        }
                    }
                    // The throwable the player holds ready: a trial's.
                    "CAPI_Player.SelectThrowGadget" if a(0) == fc3_story::PLAYER.to_string() => {
                        let gadget = a(1).to_ascii_lowercase();
                        host.throw_choice = if gadget.contains("molotov") {
                            Some(crate::controls::Action::Molotov)
                        } else if gadget.contains("grenade") {
                            Some(crate::controls::Action::Grenade)
                        } else {
                            host.throw_choice
                        };
                    }
                    // A static breakable broken outright (the
                    // `StaticBreakableBreaker` box: BAM_040's temple
                    // collapsing, DEN_040's caches).
                    "CAPI_Entity.DestroyCompoundObject" | "BreakMetaBreakable" => {
                        if let Ok(id) = a(0).parse::<u64>() {
                            breaks.break_now(id);
                        }
                    }
                    // A compound object forced to a state (`entity, state`:
                    // 0 its whole one, the first; past it broken, as
                    // SAM_020's door blown by its C4 and DOC_020's windows
                    // are).
                    "CAPI_Entity.SendForceState" => {
                        if let (Ok(id), Ok(state)) = (a(0).parse::<u64>(), a(1).parse::<f32>()) {
                            if state >= 1.0 {
                                breaks.break_now(id);
                            }
                        }
                    }
                    // Breakables kept whole (`MetaBreakableModifier`: a group
                    // switched off or made undamageable, and back).
                    "SetMetaBreakableEnabled" | "SetMetaBreakableImmuneToDamage" => {
                        let on = matches!(a(1).as_str(), "true" | "1");
                        let immune = if name == "SetMetaBreakableEnabled" { !on } else { on };
                        for id in host.story.group_members(&a(0)) {
                            breaks.set_immune(id, immune);
                        }
                    }
                    // A placed RealTree sized, grown or stopped.
                    "CAPI_Entity.UpdateRealTreeRegen" => {
                        if let (Ok(id), Ok(size)) = (a(0).parse::<u64>(), a(1).parse::<f32>()) {
                            debug!("tree growth: {id} sized {size}");
                            trees.set(id, size);
                        }
                    }
                    "CAPI_Entity.SetRealTreeGrowthSpeed" => {
                        if let (Ok(id), Ok(size), Ok(speed), Ok(growth)) = (a(0).parse::<u64>(), a(1).parse::<f32>(), a(2).parse::<f32>(), a(3).parse::<u64>())
                        {
                            debug!("tree growth: {id} to {size} at {speed} a second");
                            trees.grow(id, size, speed, growth);
                        }
                    }
                    "CAPI_Entity.StopRealTreeGrowth" => {
                        if let (Ok(id), Ok(growth)) = (a(0).parse::<u64>(), a(1).parse::<u64>()) {
                            trees.stop(id, growth);
                        }
                    }
                    "CAPI_Player.AddGadget" if a(0) == fc3_story::PLAYER.to_string() => {
                        let item = host.story.item_name(&a(1));
                        // (A throwable given is a full stack of it: a trial's
                        // grenades.)
                        if let Ok(mut inv) = inventory.single_mut() {
                            let lower = item.to_ascii_lowercase();
                            if lower.contains("frag_grenade") {
                                inv.grenades = inv.grenades.max(perks.grenade_cap);
                            } else if lower.contains("molotov") {
                                inv.molotovs = inv.molotovs.max(crate::weapons::MOLOTOV_CAP);
                            }
                        }
                        scripted.gadgets.insert(item);
                    }
                    "CAPI_Player.EnableAnimalLooting" if a(0) == fc3_story::PLAYER.to_string() => {
                        scripted.animal_looting = a(1) != "false" && a(1) != "0";
                    }
                    "CAPI_Player.EnablePlantLooting" if a(0) == fc3_story::PLAYER.to_string() => {
                        scripted.plant_looting = a(1) != "false" && a(1) != "0";
                    }
                    "CAPI_Vehicle.VehicleUseTakeSeat" | "CAPI_Vehicle.VehicleUseExit" => seat_requests.native(&name, &args),
                    // The player put out of a vehicle (`ForceExitVehicle`:
                    // `vehicle, pawn`; SAM_015's road, JAS_220's runway), as a
                    // script's exit.
                    "CAPI_Vehicle.ForceLeaveVehicle" | "CAPI_Vehicle.TryLeaveVehicle" => {
                        seat_requests.native("CAPI_Vehicle.VehicleUseExit", &[a(0), a(1)]);
                    }
                    "CAPI_Player.ForceSetPlayerInVehicle" => seat_requests.force_player(&args),
                    "CAPI_Vehicle.StartVehicleFollowPath" | "CAPI_Vehicle.StopVehicleFollowPath" | "CAPI_Vehicle.SetVehicleSpeed" => {
                        vehicle_paths.native(&name, &args, &host.story.data());
                    }
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
                    // A storm brought on, or the sky left fair, for a while
                    // (`<factor> <seconds>`), and let go (`<seconds>`).
                    "SetScriptedStormFactorOverride" => {
                        env_overrides.storm = Some((a(0).parse().unwrap_or(1.0), a(1).parse().unwrap_or(0.0)));
                    }
                    "RemoveScriptedStormFactorOverride" => env_overrides.storm = None,
                    // A preset put over the world's environment, or taken off
                    // (`Set<Kind>Override <guid or name> <seconds>`,
                    // `Remove<Kind>Override` with the same).
                    n if n.starts_with("Set") && n.ends_with("Override") && !a(0).trim().is_empty() => {
                        env_overrides.active.insert(n["Set".len()..n.len() - "Override".len()].to_string(), a(0));
                        env_overrides.seconds = a(1).parse().unwrap_or(0.0);
                    }
                    n if n.starts_with("Remove") && n.ends_with("Override") => {
                        let kind = &n["Remove".len()..n.len() - "Override".len()];
                        if env_overrides.active.get(kind).is_some_and(|g| g.trim().eq_ignore_ascii_case(a(0).trim())) {
                            env_overrides.active.remove(kind);
                            env_overrides.seconds = a(1).parse().unwrap_or(0.0);
                        }
                    }
                    // An effect on a character's bone (`pawn, effect, bone,
                    // handle, tag, turn, offset`), and stopped by its tag.
                    "CAPI_Pawn.SpawnParticle" => {
                        if let (Ok(pawn), Ok(effect)) = (a(0).parse::<u64>(), a(1).parse::<u64>()) {
                            let f = |i: usize| a(i).parse::<f32>().unwrap_or(0.0);
                            let (bone, tag) = (a(2).parse().unwrap_or(0), a(4).parse().unwrap_or(0));
                            pawn_fx.requests.push(crate::pawn_fx::Request { pawn, effect, bone, tag, offset: [f(6), f(7), f(8)] });
                        }
                    }
                    // Something flashing for the player's eye (`entity, on,
                    // r, g, b, flashes a second`), or not.
                    "CAPI_Entity.SetShimmerOnEntity" => {
                        if let Ok(id) = a(0).parse::<u64>() {
                            let f = |i: usize| a(i).parse::<f32>().unwrap_or(-1.0);
                            let on = a(1) == "true" || a(1) == "1";
                            shimmers.script.insert(id, on.then(|| ([f(2), f(3), f(4)], f(5))));
                        }
                    }
                    "CAPI_Pawn.StopToDestroyParticle" | "CAPI_Pawn.DestroyParticle" => {
                        if let Ok(pawn) = a(0).parse::<u64>() {
                            pawn_fx.stops.push((pawn, a(1).parse().unwrap_or(0)));
                        }
                    }
                    // A character's or vehicle's health on the HUD, by the
                    // name given (`show, entity, name id`), or taken off.
                    "DisplayHealthBar" => {
                        scripted.health_bar =
                            (a(0) == "true").then(|| HealthBar { entity: a(1).parse().unwrap_or(0), name: a(2).parse().unwrap_or(0), fraction: 1.0 });
                    }
                    // The HUD's parts hidden and shown again (a tutorial's,
                    // a scene's): `<movie> HideUIElement|ShowUIElement <parts>`.
                    "UISendEventToPlayer" if a(0).eq_ignore_ascii_case("SINGLE HUD") && matches!(a(1).as_str(), "HideUIElement" | "ShowUIElement") => {
                        let hide = a(1) == "HideUIElement";
                        for part in a(2).split('|').map(|p| p.trim().to_ascii_lowercase()).filter(|p| !p.is_empty()) {
                            if hide {
                                scripted.hud_parts_hidden.insert(part);
                            } else {
                                scripted.hud_parts_hidden.remove(&part);
                            }
                        }
                    }
                    // Nothing for the host to do: kept by the story itself
                    // (who holds a character, a companion's lead, contextual
                    // actions' states), or already so here (a weapon a script
                    // spawned is carried as it spawns; the camera's near
                    // plane is a centimetre already for close-ups; inputs
                    // aren't held over).
                    "CAPI_AI.AcquireControl"
                    | "CAPI_Companion.CompanionSetLead"
                    | "ToggleContextualActionState"
                    | "CAPI_Pawn.AddWeapon"
                    | "CAPI_Player.SetCloseUpCamera"
                    | "CAPI_Player.ResetInputs" => {}
                    // (Told once a run, for finding what scripts ask that
                    // nothing plays.)
                    other => {
                        static TOLD: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);
                        if TOLD.lock().is_ok_and(|mut t| t.get_or_insert_default().insert(other.to_string())) {
                            debug!("story: native {other} not played ({:?})", args.iter().take(4).collect::<Vec<_>>());
                        }
                    }
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
    // FC3_USE_DEBUG (tests): the usable pawns within 30 m, once a second.
    if std::env::var_os("FC3_USE_DEBUG").is_some() && time.elapsed_secs() as u32 != (time.elapsed_secs() - time.delta_secs()) as u32 {
        let here = to_dunia(at.translation);
        let near: Vec<_> = host.story.usable_pawns().into_iter().filter(|(_, p, _)| p.is_some_and(|p| (p[0] - here[0]).hypot(p[1] - here[1]) < 30.0)).collect();
        info!("use: pawns {near:?}, target {:?}", target.as_ref().map(|t| t.entity));
    }
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

/// How near a door a script waits to see opened a trigger must be for the
/// autopilot to walk through the door rather than step into the trigger.
const DOOR_REACH: f32 = 12.0;

/// How near something a script waits to see destroyed must be for the
/// autopilot to go and break it (not across the island).
const DESTROY_REACH: f32 = 80.0;

/// A mission failing this soon after the autopilot stepped into a trigger
/// failed for it.
const FATAL_WITHIN: f32 = 2.0;
/// How long the story is quiet before the autopilot leaves a region a
/// mission waits for the player to leave (short of the 25 s after which
/// it steps into triggers it used again).
const LEAVE_AFTER: f32 = 20.0;
/// How long the story is quiet before the autopilot goes near a place a
/// script waits for with a fight on still.
const NEAR_DESPITE_FIGHT: f32 = 60.0;

/// Just outside a region (Dunia), the nearest a few metres' steps away from
/// `from` in one of 16 directions, on the ground `ground` gives.
fn exit_point(shape: &fc3_story::triggers::Shape, from: [f32; 3], ground: impl Fn(f32, f32) -> Option<f32>) -> Option<[f32; 3]> {
    (0..16)
        .filter_map(|k| {
            let (s, c) = (k as f32 * std::f32::consts::TAU / 16.0).sin_cos();
            (1..100).map(|i| i as f32 * 4.0).find_map(|d| {
                let (x, y) = (from[0] + c * d, from[1] + s * d);
                let z = ground(x, y).unwrap_or(from[2]);
                (!shape.contains([x, y, z + 0.5])).then_some((d, [x + c * 2.0, y + s * 2.0, z]))
            })
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, p)| p)
}
/// How long (s) the story stays quiet, a companion leading, before the
/// autopilot steps into the triggers ahead itself.
const LED_STEP_AFTER: f32 = 15.0;
/// How far an objective marker may be for the autopilot at the wheel to
/// drive to it, nothing else to drive into: not one beside it (on a
/// passenger), nor one across the island (a quest giver's).
const MARKER_DRIVE_NEAR: f32 = 15.0;
const MARKER_DRIVE_FAR: f32 = 400.0;

/// The triggers the autopilot learned to leave alone in earlier runs, kept
/// in the file `FC3_STORY_FATAL` names (one `id event` a line), if any: a
/// run from a checkpoint need not fail the mission again to learn them.
fn fatal_learned() -> std::collections::HashSet<(u64, String)> {
    let Some(text) = std::env::var_os("FC3_STORY_FATAL").and_then(|p| std::fs::read_to_string(p).ok()) else { return Default::default() };
    parse_fatal(&text)
}

fn parse_fatal(text: &str) -> std::collections::HashSet<(u64, String)> {
    text.lines().filter_map(|l| l.trim().split_once(' ')).filter_map(|(id, event)| Some((id.parse().ok()?, event.trim().to_string()))).collect()
}

fn learn_fatal(fatal: &mut std::collections::HashSet<(u64, String)>, id: u64, event: String) {
    if fatal.insert((id, event.clone())) {
        if let Some(path) = std::env::var_os("FC3_STORY_FATAL") {
            use std::io::Write;
            let file = std::fs::OpenOptions::new().create(true).append(true).open(&path);
            if let Err(e) = file.and_then(|mut f| writeln!(f, "{id} {event}")) {
                warn!("story autopilot: can't keep the trigger left alone in {path:?}: {e}");
            }
        }
    }
}
/// How long before the autopilot goes near a place again, gone out of reach.
const NEAR_AGAIN: f32 = 20.0;
/// How long the running missions may go nowhere (no block completing)
/// before the autopilot, with nothing of theirs left to do, takes the
/// others' triggers too.
const FOCUS_GIVE_UP: f32 = 240.0;
/// Harm a script deals that kills whoever takes it.
const KILLING_BLOW: f32 = 1000.0;

/// A story character's body searched: scripts waiting for it hear
/// `OnLooting` (HOY_020's VIPs, searched for evidence).
fn story_searches(host: Option<NonSendMut<StoryHost>>, searched: Query<&crate::story_actors::StoryActor, Added<crate::pickups::Searched>>) {
    let Some(mut host) = host else { return };
    for a in &searched {
        debug!("story: {} searched", a.id);
        host.story.entity_event(a.id, "OnLooting", vec![]);
    }
}

/// Harm scripts deal lands on the player or the story's character it names.
fn story_damage(
    host: Option<NonSendMut<StoryHost>>,
    players: Query<Entity, With<Player>>,
    actors: Query<(Entity, &crate::story_actors::StoryActor)>,
    mut healths: Query<&mut crate::combat::Health>,
    mut damage: MessageWriter<crate::combat::DamageEvent>,
) {
    let Some(mut host) = host else { return };
    // Health a script sets: so much left (as much as it holds), or none, a
    // death as the harm below is.
    for (pawn, health) in std::mem::take(&mut host.health_sets) {
        let target = if pawn == fc3_story::PLAYER { players.single().ok() } else { actors.iter().find(|(_, a)| a.id == pawn).map(|(e, _)| e) };
        let Some(target) = target else { continue };
        let Ok(mut h) = healths.get_mut(target) else { continue };
        if h.dead {
            continue;
        }
        debug!("story: a script sets {pawn}'s health to {health}");
        if health > 0.0 {
            h.current = health.min(h.max);
        } else {
            host.damages.push((pawn, h.current + 1.0));
        }
    }
    for (pawn, amount) in std::mem::take(&mut host.damages) {
        let target = if pawn == fc3_story::PLAYER { players.single().ok() } else { actors.iter().find(|(_, a)| a.id == pawn).map(|(e, _)| e) };
        let Some(target) = target else { continue };
        debug!("story: a script deals {amount} to {pawn}");
        damage.write(crate::combat::DamageEvent {
            target,
            amount,
            kind: crate::combat::DamageKind::Fall,
            source: None,
            instigator: None,
            headshot: false,
            hit: crate::combat::Hit::Whole,
        });
    }
}

/// How far back from a spot to be seen the autopilot stops the vehicle it
/// drives, facing it.
const FOV_DRIVE_BACK: f32 = 10.0;

/// Explosives scripts set off go off where they are (a mission's bomb: a
/// C4 charge's blast), and blasts reach what scripts wait to see destroyed:
/// within the blast's reach it breaks, and the scripts hear
/// `CDestroyEvent`. Whatever else breaks (a script's
/// `DestroyCompoundObject`, a nearby breakable) they hear of too.
fn story_blasts(
    host: Option<NonSendMut<StoryHost>>,
    mut explosions: ParamSet<(MessageWriter<crate::weapons::Explosion>, MessageReader<crate::weapons::Explosion>)>,
    mut breaks: ResMut<crate::breakables::Breaks>,
) {
    let Some(mut host) = host else { return };
    for id in std::mem::take(&mut host.detonations) {
        let Some(at) = host.story.pose(id).map(|p| p.position) else { continue };
        info!("story: explosive {id} detonated");
        let (radius, damage) = crate::explosives::C4;
        explosions.p0().write(crate::weapons::Explosion { at: to_bevy(at), radius, damage, instigator: None, effect: Some(crate::authored_fx::blast::C4) });
    }
    let watched = host.story.watched_destructibles();
    for x in explosions.p1().read() {
        for (id, at) in &watched {
            if !breaks.is_immune(*id) && to_bevy(*at).distance(x.at) < x.radius * 0.5 + 2.0 {
                breaks.break_now(*id);
            }
        }
    }
    for id in std::mem::take(&mut breaks.news) {
        host.story.destroyed(id);
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
        || std::env::var("FC3_STORY_AUTOPILOT").is_ok()
        || std::env::var("FC3_STORY_RETRY_TEST").is_ok()
        || std::env::var("FC3_TRIAL_TEST").is_ok()
        || std::env::var("FC3_OUTPOST_FIRE_TEST").is_ok();
    let cancel = b.choice && (keys.any_just_pressed([KeyCode::Backspace, KeyCode::KeyQ]) || pad_pressed(GamepadButton::East));
    actions.clear();
    if !panels.iter().any(|(_, p)| p.0 == b.id) {
        let fallback = match (b.choice, pad) {
            (true, false) => "[Enter]  Accept        [Backspace]  Cancel",
            (true, true) => "A  Accept        B  Cancel",
            (false, false) => "[Enter]  Continue",
            (false, true) => "A  Continue",
        };
        // Authored tutorial captions (e.g. "New Tutorial") are not dismissal
        // instructions. Always expose the controls that this modal accepts.
        let prompt = b.prompt.as_ref().map(|p| prompt_text(p, &actions, &settings))
            .filter(|p| !p.trim().is_empty() && p != fallback)
            .map(|p| format!("{p}\n{fallback}")).unwrap_or_else(|| fallback.to_string());
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
        story.take_radio_tower(panel);
    }
}

pub(crate) fn teleport(t: &mut Transform, p: &mut Player, pos: [f32; 3], yaw_deg: f32) {
    t.translation = to_bevy(pos) + Vec3::Y * 1.0;
    p.swimming = false;
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
    original: Res<crate::hud::OriginalHud>,
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
    // (The original HUD shows these itself while it runs; without it, no
    // one takes its cards.)
    let own = !original.0;
    if own {
        ui.cards.clear();
    }
    let objective = ui.objective.as_ref().filter(|_| own).map(|s| prompt_text(s, &actions, &settings)).unwrap_or_default();
    let subtitle = if settings.subtitles && own { ui.subtitle.as_ref().map(|s| s.0.clone()).unwrap_or_default() } else { String::new() };
    let banner = ui.banner.as_ref().filter(|_| own).map(|s| s.0.clone()).unwrap_or_default();
    // (The original HUD's tutorial box shows the hints; checklists stay.)
    let mut hint_ids: Vec<_> = ui.hints.keys().copied().filter(|_| own).collect();
    hint_ids.sort_unstable();
    let joined: Vec<String> = hint_ids
        .iter()
        .map(|id| {
            let (title, text) = &ui.hints[id];
            [title, text].iter().filter(|s| !s.is_empty()).map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
        })
        .collect();
    let hints = joined.iter().chain(ui.checklist.values()).map(|text| prompt_text(text, &actions, &settings)).collect::<Vec<_>>().join("\n\n");
    set_text(&mut texts.p0(), &objective);
    set_text(&mut texts.p1(), &subtitle);
    set_text(&mut texts.p2(), &banner);
    set_text(&mut texts.p3(), &hints);
    // (The original HUD has its own timer.)
    let timers = ui.timers.iter().filter(|_| own).map(timer_text).collect::<Vec<_>>().join("\n");
    set_text(&mut texts.p5(), &timers);
    // The call: ringing first, then its running time (it ends with the
    // scripts' `PhoneCallEnd`, or after two minutes at most).
    if let Some((_, _, age)) = &mut ui.phone {
        *age += dt;
        if *age > 120.0 {
            ui.phone = None;
        }
    }
    // (Not over the original HUD, which has none: the call is its voices
    // and subtitles.)
    let phone = ui.phone.as_ref().filter(|_| own).map(|(who, incoming, age)| {
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

/// Tests: how far from a companion the autopilot steps into a trigger.
const COMPANION_REACH: f32 = 30.0;

/// Tests: seconds the autopilot tries to take a foe down without hurting
/// him before it shoots him instead (out of the knife's reach: in the
/// water, on a ledge).
const TAKEDOWN_PATIENCE: f32 = 6.0;

/// Tests: how far from a foe the autopilot shoots him from, when it has to
/// come nearer (m).
const SHOOT_FROM: f32 = 15.0;

/// Tests: beyond how far (m) the autopilot shoots through the sights.
const LONG_SHOT: f32 = 30.0;

/// Tests: how long (s) the autopilot lets a vehicle it set down settle before
/// the next place its distance checks wait for.
const VEHICLE_NEAR_AFTER: f32 = 2.0;

/// Tests: how far (m) the autopilot stands from a hunt's quarry, shooting
/// and with a blade.
const HUNT_FROM: f32 = 18.0;
const HUNT_MELEE: f32 = 1.4;
/// Tests: how long (s) the autopilot shoots at a hunt's quarry from one
/// spot before trying another, and the nearest it comes (m).
const HUNT_TRY: f32 = 12.0;
const HUNT_NEAREST: f32 = 6.0;

/// Our weapon class for a hunt's (`WeaponClass`).
fn hunt_class(class: &str) -> Option<crate::weapons::WeaponClass> {
    use crate::weapons::WeaponClass as W;
    Some(match class.to_ascii_lowercase().as_str() {
        "bow" => W::Bow,
        "shotgun" => W::Shotgun,
        "assault" => W::Rifle,
        "pistol" | "handgun" => W::Pistol,
        "smg" => W::Smg,
        "sniper" => W::Sniper,
        "lmg" => W::Lmg,
        "rpg" => W::Launcher,
        "flamethrower" => W::Flamethrower,
        "machete" | "melee" => W::Melee,
        _ => return None,
    })
}

/// Tests: how much nearer (m) another foe must be for the autopilot to turn
/// from the one it goes after (two about as near were swapped every frame).
const TARGET_STICK: f32 = 4.0;

/// Tests: how near (m) a foe must be for the autopilot to go after him,
/// and how long the story is quiet (s) before one a block counts down is
/// gone after wherever he is (HUR_040 B40's last boat crew, off the beach).
const FIGHT_REACH: f32 = 150.0;
/// How long (s) after the autopilot throws a rock to lure a guard it leaves
/// those not yet fighting alone (they've gone to look, as was wanted).
const LURE_GRACE: f32 = 20.0;
const FAR_FOE_AFTER: f32 = 30.0;

/// How far (m) from a battlefield's middle its strategic point may be.
const STRATEGIC_REACH: f32 = 15.0;

/// Tests: how far the autopilot keeps from a character it isn't fighting (a
/// captive) while it fights (m): the foes' grenades and firebombs come where
/// the player is.
const KEEP_CLEAR: f32 = 10.0;
/// Tests: how near the player's eye (m) someone not to be shot is beside
/// him rather than in the line of fire, unless right in front.
const BESIDE: f32 = 2.0;

/// Tests: how far the autopilot lets a companion leading the way get in a
/// fight (m), coming back to `KEEP_CLEAR` and a little more behind it: the
/// scripts want the player close (HOY_010's Sam).
const ESCORT_IN_FIGHT: f32 = 20.0;

/// CIT_030's fight with the Ink Monster: played (`boss_fight`), not ended.
const BOSS_FIGHT: &str = "CCitraBossFight";
/// Being intoxicated: played (`intoxication`) until its script stops it.
const INTOXICATED: &str = "CIntoxicatedMiniGame";
/// A script's button prompts: shown (`input_prompts`) until it ends them.
const INPUT_PROMPTS: &str = "CGSFInputManager";
/// JAS_150's ride, shot from: played (`chopper_fight`) until its scene
/// stops it.
const CHOPPER_FIGHT: &str = "CChopperFightE3";
/// JAS_130's climb from the 4x4's bed to its front seat: played
/// (`car_chase`), ending once he's there.
const CAR_CHASE: &str = "CExoticCarChase";
/// A sharpshooter challenge's gallery (`CShootingGalleryPistol_E6`...):
/// played (`shooting_gallery`), not its entice system or its resources.
fn shooting_gallery(name: &str) -> bool {
    name.starts_with("CShootingGallery") && !name.ends_with("EnticeSystem") && !name.ends_with("ResourceContainer")
}
/// The scripts hear the player's weapon drawn and put away
/// (`CWeaponDrawFinishedEvent`, `CWeaponHolsteredEvent` on the player's
/// groups: the wielder, the weapon's name id, its class, its ammo); F3's
/// trial waits on each weapon it gives being drawn.
fn weapon_events(
    host: Option<NonSendMut<StoryHost>>,
    players: Query<&crate::weapons::Inventory, With<crate::player::Player>>,
    mut scripted: ResMut<ScriptedPlayer>,
    mut last: Local<Option<(usize, bool)>>,
) {
    let (Some(mut host), Ok(inv)) = (host, players.single()) else { return };
    let Some(slot) = inv.slots.get(inv.current) else { return };
    if std::mem::take(&mut scripted.redraw) && *last == Some((slot.def, true)) {
        *last = Some((slot.def, false));
    }
    let args = |def: usize, ammo: u32| {
        let name = fc3_formats::hash::crc32(inv.defs[def].name.to_ascii_lowercase().as_bytes());
        vec![Arg::id(fc3_story::PLAYER), Arg::Num(name as f64), Arg::Num(0.0), Arg::Num(ammo as f64)]
    };
    match *last {
        Some((def, _)) if def == slot.def => {}
        Some((def, _)) => {
            host.story.group_event(fc3_story::PLAYER, "CWeaponHolsteredEvent", args(def, 0));
            *last = Some((slot.def, false));
        }
        None => *last = Some((slot.def, false)),
    }
    if *last == Some((slot.def, false)) && inv.switching == 0.0 {
        host.story.group_event(fc3_story::PLAYER, "CWeaponDrawFinishedEvent", args(slot.def, slot.mag + slot.reserve));
        *last = Some((slot.def, true));
    }
}

/// The scripts hear shots (`CFCXWeaponShootEvent` on the shooter's groups:
/// the wielder, the weapon's name, its class as `GetNoCaseStringID` gives
/// it, the rounds left in it), the player's and the story's characters'.
/// WIL_020's barks as the flamethrower runs low and over the burning fields
/// listen for the player's; WIL_040's stealth listener for its guards'.
fn shot_events(
    host: Option<NonSendMut<StoryHost>>,
    players: Query<&crate::weapons::Inventory, With<crate::player::Player>>,
    npcs: Query<(Entity, &crate::story_actors::StoryActor, &crate::npc::Npc)>,
    mut last: Local<(Option<u32>, HashMap<Entity, f32>)>,
) {
    let Some(mut host) = host else { return };
    let class = |c: &str| Arg::Num(fc3_formats::hash::crc32(c.to_ascii_lowercase().as_bytes()) as f64);
    let (shots, since) = &mut *last;
    if let Ok(inv) = players.single() {
        let fired = shots.is_some_and(|n| inv.shots_fired > n);
        *shots = Some(inv.shots_fired);
        let held = inv.slots.get(inv.current).map(|s| (&inv.defs[s.def], s.mag));
        if let Some((def, mag)) = held.filter(|_| fired) {
            if let Some(c) = shot_class(def.class) {
                let args = vec![Arg::id(fc3_story::PLAYER), Arg::Str(def.name.into()), class(c), Arg::Num(mag as f64)];
                host.story.group_event(fc3_story::PLAYER, "CFCXWeaponShootEvent", args);
            }
        }
    }
    // (A character's shot resets its time since firing.)
    let mut now = HashMap::new();
    for (e, actor, npc) in &npcs {
        now.insert(e, npc.since_fired);
        if since.get(&e).is_none_or(|&t| npc.since_fired >= t) {
            continue;
        }
        let Some((weapon, c)) = role_weapon(npc.role) else { continue };
        let args = vec![Arg::id(actor.id), Arg::Str(weapon.into()), class(c), Arg::Num(npc.mag as f64)];
        host.story.group_event(actor.id, "CFCXWeaponShootEvent", args);
    }
    *since = now;
}

/// A weapon class as the scripts' shot listeners name it (`classFilter`:
/// Pistol, Assault, SMG, Shotgun, Sniper, LMG, RPG, FlameThrower...); none
/// for a blade.
fn shot_class(class: crate::weapons::WeaponClass) -> Option<&'static str> {
    use crate::weapons::WeaponClass::*;
    Some(match class {
        Melee => return None,
        Pistol => "Pistol",
        Smg => "SMG",
        Rifle => "Assault",
        Shotgun => "Shotgun",
        Sniper => "Sniper",
        Lmg | Mounted => "LMG",
        Launcher => "RPG",
        Bow => "Bow",
        Flamethrower => "FlameThrower",
        GrenadeLauncher => "GrenadeLauncher",
    })
}

/// The weapon an enemy's role shoots with, and its class (none: a
/// firebomb's thrown, a machete's swung).
fn role_weapon(role: crate::npc::Role) -> Option<(&'static str, &'static str)> {
    use crate::npc::Role::*;
    match role {
        Assault => Some(("AK47", "Assault")),
        Shotgunner => Some(("M133", "Shotgun")),
        Sniper => Some(("SVD", "Sniper")),
        Heavy => Some(("PKM", "LMG")),
        Rocket => Some(("RPG7", "RPG")),
        Flamer => Some(("Flamethrower", "FlameThrower")),
        Molotov | Charger => None,
    }
}

/// Seconds a specific gameplay the game doesn't play runs before it ends.
const UNPLAYED_GRACE: f32 = 1.0;

/// The specific gameplays the game plays itself (QTEs, the sniping
/// challenge, poker, the Ink Monster, being drunk, button prompts, the
/// chopper ride, JAS_130's climb to the front seat, the shooting
/// galleries; the vines under their test).
fn played_gameplay(host: &StoryHost, id: u64, name: &str) -> bool {
    name == "QTEGSF"
        || host.story.is_qte_gameplay(id)
        || sniping_challenge(name)
        || crate::poker::poker_gameplay(name)
        || crate::knives::knives_gameplay(name)
        || crate::drinking::drinking_gameplay(name)
        || name == BOSS_FIGHT
        || name == INTOXICATED
        || name == INPUT_PROMPTS
        || name == CHOPPER_FIGHT
        || name == CAR_CHASE
        || shooting_gallery(name)
        || crate::races::race_gameplay(name)
        || crate::races::entice_gameplay(name)
        || (name == "CVineClimbingGameplay" && crate::vine::played())
}

/// A sniping set-piece (`CSnipingChallenge_...`): the player shoots its
/// targets from where they stand.
fn sniping_challenge(gameplay: &str) -> bool {
    gameplay.starts_with("CSnipingChallenge")
}

/// Tests: how far behind a character the autopilot keeps when tailing him
/// (it goes back there outside `TAIL_NEAR..TAIL_FAR` metres).
const TAIL_DISTANCE: f32 = 14.0;
/// Where the autopilot stands to photograph a thing.
#[derive(Default)]
struct PhotoStand {
    /// The thing, the feet, how often the player was put there, and how
    /// long the camera has been up at it from there.
    at: Option<(u64, Vec3, u8, f32)>,
    /// Stands that gave no photo (JAS_200's meeting, seen from a bridge
    /// beside it at first and then not: its people came between).
    tried: Vec<Vec3>,
}

/// How long the camera is held up at a thing from one stand before another.
const PHOTO_PATIENCE: f32 = 6.0;

const TAIL_NEAR: f32 = 8.0;
const TAIL_FAR: f32 = 30.0;

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
    mut players: Query<(Entity, &mut Transform, &mut Player, &crate::combat::Health, &crate::weapons::Inventory)>,
    animals: Query<(Entity, &Transform, &crate::wildlife::WildAnimal, &crate::combat::Health, Option<&crate::wildlife::Carcass>, Option<&crate::story_actors::StoryActor>), Without<Player>>,
    plants: Query<(&crate::pickups::Pickup, &GlobalTransform)>,
    mut actions: ResMut<crate::controls::ActionState>,
    mut last: Local<Option<(u64, &'static str)>>,
    (
        npcs,
        crafting,
        mut next_press,
        mut shooting,
        mut skipped,
        story_actors,
        action_maps,
        mut came_near,
        mut blocked,
        mut kept_clear,
        mut far_pickups,
        taken,
        placed,
        spatial,
        mut photo_stand,
        mut hunting,
    ): (
        Query<
            (
                &Transform,
                &crate::combat::Health,
                Option<&crate::pickups::LootableBody>,
                Option<&crate::pickups::OwnLoot>,
                &crate::story_actors::StoryActor,
                &crate::npc::Npc,
                Option<&crate::npc::HeadPos>,
            ),
            Without<Player>,
        >,
        Option<Res<crate::crafting::CraftCatalog>>,
        Local<f32>,
        // A foe shot at from a seat without being hurt (out of sight, behind
        // cover): when the shooting at it began and the health it had then,
        // and foes left alone a while (until when).
        Local<Option<(u64, f32, f32)>>,
        Local<HashMap<u64, f32>>,
        // The story's characters, fighters or not (a companion to keep up with).
        Query<(&Transform, &crate::story_actors::StoryActor, Has<crate::story_actors::StoryCreature>), Without<Player>>,
        Res<crate::controls::ScriptActionMaps>,
        // Foes shot at in vain from afar, gone right up to since.
        Local<std::collections::HashSet<u64>>,
        // Since when someone not to be shot has stood in the line of fire.
        Local<Option<f32>>,
        // When the player last stepped away from someone to keep safe.
        Local<f32>,
        // Awaited pickups gone to from afar, so that they stream in.
        Local<std::collections::HashSet<u64>>,
        // What the camera has taken in, and things placed in the world.
        Res<crate::stealth::CameraTargets>,
        Query<(&crate::objects::PlacedId, &GlobalTransform)>,
        avian3d::prelude::SpatialQuery,
        Local<PhotoStand>,
        // The quarry being hunted, and since when.
        Local<Option<(u64, f32)>>,
    ),
) {
    if std::env::var("FC3_STORY_AUTOPILOT").is_err() { return }
    let Some(mut host) = host else { return };
    // (Once the story has stalled a while: what holds this step back, if
    // anything.)
    let report = host.quiet > 20.0 && host.quiet - time.delta_secs() <= 20.0;
    if report {
        if let Ok((_, _, p, h, _)) = players.single() {
            info!(
                "story autopilot: loot step: paused {}, boxes {}, scene view {}, scene plays the player {}, qte {}, hands busy {}, dead {}, spawning {}, driving {}, landing awaited {}, reload due {}",
                time.is_paused(),
                ui.boxes.len(),
                cinema.view.is_some(),
                cinema.plays_player(),
                qtes.active(),
                busy.busy(),
                h.dead,
                p.spawning,
                p.driving && !p.armed_passenger,
                landing_awaited(&host, p),
                host.reload_at.is_some()
            );
        }
    }
    // (The Ink Monster's fight plays itself: `boss_fight`.)
    if host.story.gameplays().iter().any(|g| g.1 == BOSS_FIGHT) {
        return;
    }
    // (Nor while a scene plays the player through without its own view: a
    // quest giver's brief walked out of left its pickup untaken.)
    if time.is_paused() || !ui.boxes.is_empty() || cinema.view.is_some() || cinema.plays_player() || qtes.active() || busy.busy() {
        let t = time.elapsed_secs();
        if host.story.gameplays().iter().any(|g| sniping_challenge(&g.1)) && (t * 0.5) as u32 != ((t - time.delta_secs()) * 0.5) as u32 {
            info!(
                "story autopilot: sniping held: paused {}, boxes {}, scene view {}, scene plays the player {}, qte {}, hands busy {}",
                time.is_paused(),
                ui.boxes.len(),
                cinema.view.is_some(),
                cinema.plays_player(),
                qtes.active(),
                busy.busy()
            );
        }
        return;
    }
    let Ok((me, mut at, mut player, health, inventory)) = players.single_mut() else { return };
    // Shooting from where he is, not going after anyone: a passenger, gun in
    // hand, or on a mounted gun (HUR_040's barge).
    // (Or with a rifle lent where it was taken: a sniping challenge's.)
    let fixed = player.armed_passenger || inventory.mounted_from.is_some();
    // (Held up where a high trigger was stepped into, the player may still
    // go after a character: getting there sets him down.)
    if health.dead || player.spawning || (player.driving && !player.armed_passenger) || landing_awaited(&host, &player) {
        return;
    }
    if host.reload_at.is_some() {
        return;
    }
    // A camera a script drew and no longer holds the player in (no action
    // map left in force, or a sniping challenge on), with no one to
    // photograph: put away, as a player would.
    let sniping = host.story.gameplays().iter().any(|g| sniping_challenge(&g.1));
    if scripted.camera_drawn && (sniping || (action_maps.in_force().is_none() && host.quiet > 3.0)) && host.story.awaited_discoveries().is_empty() {
        info!("story autopilot: camera put away");
        actions.press(crate::controls::Action::Tag);
        return;
    }
    // A character to keep in sight (a tail: a look-at trigger riding on him
    // whose leaving the view a script watches): some way behind him, looking
    // at him, and nothing else meanwhile (a fight would give the player away).
    let tails = host.story.tail_targets();
    let nearest_tail = tails.iter().min_by(|a, b| to_bevy(a.1).distance(at.translation).total_cmp(&to_bevy(b.1).distance(at.translation)));
    if let Some(&(id, point, _)) = nearest_tail {
        let target = to_bevy(point);
        let flat = (at.translation - target).with_y(0.0);
        if *last != Some((id, "tail")) {
            info!("story autopilot: keep {} in sight", host.story.data().name_of(id).unwrap_or("?"));
            *last = Some((id, "tail"));
        }
        if !(TAIL_NEAR..TAIL_FAR).contains(&flat.length()) {
            let side = flat.normalize_or(Vec3::Z);
            let mut feet = target + side * TAIL_DISTANCE;
            feet.y = heights.height_at(feet).unwrap_or(target.y - crate::player::EYE_HEIGHT);
            teleport(&mut at, &mut player, to_dunia(feet), 0.0);
            return;
        }
        let eye = at.translation + Vec3::Y * (player.eye - crate::player::HALF_HEIGHT);
        let aim = target - eye;
        player.yaw = (-aim.x).atan2(-aim.z);
        player.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.3));
        return;
    }
    // A companion leading the way: kept up with (back to a few metres
    // behind it when it gets away), so it doesn't stop to wait; fights on
    // the way are still the takedown step's. In a fight, kept within
    // `ESCORT_IN_FIGHT`, back to clear of it and not turned to it (the
    // player put beside it, facing it, as a shotgun went off killed Sam in
    // HOY_010).
    let leading = host.story.companions().into_iter().find(|c| matches!(c.1, fc3_story::Companion::Lead { .. }));
    if let Some((id, fc3_story::Companion::Lead { distance, .. })) = leading {
        if let Some((t, _, _)) = story_actors.iter().find(|(_, a, _)| a.id == id && !a.dead) {
            let flat = (at.translation - t.translation).with_y(0.0);
            let fight =
                npcs.iter().any(|q| !q.1.dead && q.5.hostile() && q.5.state != crate::npc::NpcState::Idle && q.0.translation.distance(at.translation) < 80.0);
            let keep = if fight { ESCORT_IN_FIGHT.min(distance) } else { distance.min(12.0) };
            if flat.length() > keep {
                if *last != Some((id, "escort")) {
                    info!("story autopilot: keep up with {}", host.story.data().name_of(id).unwrap_or("?"));
                    *last = Some((id, "escort"));
                }
                let back = (if fight { KEEP_CLEAR + 2.0 } else { 4.0 }).min(keep * 0.6);
                let mut feet = t.translation + flat.normalize_or(Vec3::Z) * back;
                feet.y = heights.height_at(feet).unwrap_or(t.translation.y - crate::player::HALF_HEIGHT);
                teleport(&mut at, &mut player, to_dunia(feet), 0.0);
                if !fight {
                    let aim = t.translation - at.translation;
                    player.yaw = (-aim.x).atan2(-aim.z);
                }
                return;
            }
        }
    }
    // A mission's character carrying one of its items (a radio operator and
    // his codes): once the story has stalled, taken down from behind with an
    // ordinary melee press, then searched; not while the player is on a
    // scripted line (a mission's opening call), as a player would still be
    // sneaking up: a line the search sets off would cut the call, whose next
    // line cuts that one in turn.
    // (An item a script waits to see looted: JAS_200's visitor carries one
    // from the start, looted only at the end, and taking him down while the
    // meeting was to be watched unseen gave the player away.)
    let looted_wanted =
        crafting.as_ref().map_or_else(|| host.story.watched_loot_filters(), |c| c.still_wanted(host.story.watched_loot_filters(), &progress.loot));
    let carries_quest = |own: &crate::pickups::OwnLoot| {
        crafting.as_ref().is_some_and(|c| {
            let quest_name = |id| c.item(id).filter(|i| i.key.starts_with(fc3_formats::progression::QUEST_ITEMS)).map(|i| i.name_id);
            own.gives(&c.groups, quest_name, &looted_wanted)
        })
    };
    let engaged = last.is_some_and(|l| l.1 == "take down" || l.1 == "search");
    let quest_of = |own: Option<&crate::pickups::OwnLoot>| own.is_some_and(|o| carries_quest(o));
    let near = |t: &Transform| t.translation.distance(at.translation);
    // (Or one a script waits to see searched: HOY_020's VIPs. Not one it
    // waits to see photographed first: JAS_200's meeting, watched unseen.)
    let to_photograph = host.story.awaited_discoveries();
    let quest = npcs
        .iter()
        .filter(|(_, h, body, own, actor, _, _)| {
            (quest_of(*own) || host.story.awaits_event(actor.id, "OnLooting")) && (!h.dead || body.is_some()) && !to_photograph.contains(&actor.id)
        })
        .min_by(|a, b| b.1.dead.cmp(&a.1.dead).then(near(a.0).total_cmp(&near(b.0))));
    if report {
        let carriers: Vec<(u64, bool, bool)> = npcs.iter().filter(|q| quest_of(q.3)).map(|q| (q.4.id, q.1.dead, q.2.is_some())).collect();
        info!("story autopilot: items looked for {looted_wanted:?}, their carriers (id, dead, body) {carriers:?}, to go after {:?}", quest.map(|q| q.4.id));
    }
    // Then the story's hostile characters out after the player, or in a
    // group a block counts down (the reinforcements and guards it waits on
    // to die), taken down the same way.
    let counted = host.story.counted_members();
    // (Gone after unprovoked only when a block counts its group down: one
    // a health listener watches is fought once he fights, JAS_200's
    // visitor not before the meeting is photographed.)
    let to_kill = host.story.size_counted_members();
    // (A wanted poster's target: gone after wherever he is, and taken down
    // or cut down, never shot: its death check takes only a blade's.)
    let wanted = host.story.wanted_dead();
    // (Not while a script waits for an alarm to be switched off: that comes
    // first, before a guard runs for it.)
    let alarm_first = !host.story.alarms_to_disable().is_empty();
    let now = time.elapsed_secs();
    skipped.retain(|_, until| *until > now);
    // The story's people not in the fight (a captive): where the autopilot
    // goes to fight is kept `KEEP_CLEAR` from them (the foes' grenades come
    // where the player is).
    let kept: Vec<Vec3> = {
        let hostile: std::collections::HashSet<u64> = npcs.iter().filter(|q| q.5.hostile()).map(|q| q.4.id).collect();
        story_actors.iter().filter(|(_, a, creature)| !a.dead && !creature && !hostile.contains(&a.id)).map(|(t, _, _)| t.translation).collect()
    };
    let clear_of_kept = |mut feet: Vec3| {
        for p in &kept {
            let off = (feet - *p).with_y(0.0);
            if off.length() < KEEP_CLEAR {
                feet = *p + off.normalize_or(Vec3::X) * (KEEP_CLEAR + 2.0);
                feet.y = heights.height_at(feet).unwrap_or(feet.y);
            }
        }
        feet
    };
    let quest = quest.or_else(|| {
        if alarm_first {
            return None;
        }
        // (The ones a block counts down first.)
        npcs.iter()
            .filter(|(t, h, _, own, actor, npc, _)| {
                !h.dead
                    && !quest_of(*own)
                    // (A seated one only when a block counts him: JAS_200's
                    // boss, fleeing in his 4x4, is shot at the wheel.)
                    && (!actor.seated || counted.contains(&actor.id))
                    && !skipped.contains_key(&actor.id)
                    && npc.hostile()
                    // (Not one it threw a rock to lure off, gone to look:
                    // JAS_020's guard by the window; nor, a while after a
                    // throw, any other the rock drew. Only those who fight.)
                    && (npc.state == crate::npc::NpcState::Combat
                        || (!host.used.contains(&(actor.id, "distract".to_string())) && now - host.distracted_at > LURE_GRACE))
                    && (to_kill.contains(&actor.id) || wanted.contains(&actor.id) || npc.state != crate::npc::NpcState::Idle || sniping)
                    && (near(t) < FIGHT_REACH || wanted.contains(&actor.id) || (to_kill.contains(&actor.id) && host.quiet > FAR_FOE_AFTER))
            })
            .min_by(|a, b| {
                let held = |id: u64| if last.is_some_and(|l| l.0 == id) { TARGET_STICK } else { 0.0 };
                (!counted.contains(&a.4.id)).cmp(&!counted.contains(&b.4.id)).then((near(a.0) - held(a.4.id)).total_cmp(&(near(b.0) - held(b.4.id))))
            })
    });
    // (In a sniping challenge, every two seconds: whom it might shoot.)
    if sniping && (now * 0.5) as u32 != ((now - time.delta_secs()) * 0.5) as u32 {
        let foes: Vec<(u64, f32, bool, bool)> =
            npcs.iter().filter(|q| !q.1.dead && q.5.hostile()).map(|q| (q.4.id, near(q.0), q.4.seated, skipped.contains_key(&q.4.id))).collect();
        info!("story autopilot: sniping: camera {}, target {:?}, foes (id, m, seated, skipped) {foes:?}", scripted.camera_drawn, quest.map(|q| q.4.id));
    }
    // (Held in the camera by a script, the player watches: a scene plays out
    // before him that needs its characters.)
    let quest = quest.filter(|_| !scripted.camera_drawn);
    let on_call = host.story.is_speaking(fc3_story::PLAYER);
    // A take-down that hasn't hurt him in a while (`shooting` keeps when it
    // began and his health then): he is shot instead.
    let stubborn = quest.is_some_and(|q| {
        !q.1.dead
            && !wanted.contains(&q.4.id)
            && (*last == Some((q.4.id, "shoot"))
                || player.swimming
                // (One on his guard can't be taken down: shot at once.)
                || q.5.state != crate::npc::NpcState::Idle
                || (*last == Some((q.4.id, "take down")) && shooting.is_some_and(|(id, _, since)| id == q.4.id && now - since > TAKEDOWN_PATIENCE)))
    });
    // Riding as a passenger: the foe nearest is shot from the seat, gun in
    // hand (the knife put away); in a sniping challenge, from where he
    // stands.
    if fixed || stubborn || sniping {
        let Some((nt, nh, _, _, actor, _, head)) = quest.filter(|q| (stubborn || sniping || host.quiet > 3.0) && !q.1.dead) else { return };
        if *last != Some((actor.id, "shoot")) {
            info!("story autopilot: shoot {} ({})", host.story.data().name_of(actor.id).unwrap_or("?"), actor.id);
            *last = Some((actor.id, "shoot"));
            *shooting = None;
        }
        // Not hurt after a while of shooting: another one for a time.
        match *shooting {
            Some((id, hp, since)) if id == actor.id => {
                if nh.current < hp {
                    *shooting = Some((id, nh.current, now));
                } else if now - since > 8.0 {
                    // On foot, the first time: right up to him (a cliff or
                    // the water's edge between), on the ground beside him.
                    if !fixed && came_near.insert(actor.id) {
                        info!("story autopilot: {} out of reach; going up to him", actor.id);
                        let flat = (at.translation - nt.translation).with_y(0.0).normalize_or(Vec3::Z);
                        let mut feet = nt.translation + flat * 3.0;
                        feet.y =
                            heights.height_at(feet).unwrap_or(nt.translation.y - crate::player::HALF_HEIGHT).max(nt.translation.y - crate::player::HALF_HEIGHT);
                        teleport(&mut at, &mut player, to_dunia(clear_of_kept(feet)), 0.0);
                        host.quiet = 0.0;
                        *shooting = None;
                        return;
                    }
                    info!("story autopilot: {} out of reach; another for a while", actor.id);
                    skipped.insert(actor.id, now + 20.0);
                    *shooting = None;
                }
            }
            _ => *shooting = Some((actor.id, nh.current, now)),
        }
        // In the water no gun fires: out onto dry ground near him, the
        // nearest that sees him over the terrain (a foe at the waterline
        // under a cliff isn't seen from its top), else the highest.
        if player.swimming && !fixed {
            let chest = nt.translation + Vec3::Y * 0.4;
            let sees = |feet: Vec3| {
                let eye = feet + Vec3::Y * 1.6;
                (1..20).all(|i| {
                    let p = eye.lerp(chest, i as f32 / 20.0);
                    heights.height_at(p).is_none_or(|h| h < p.y - 0.2)
                })
            };
            let spots: Vec<(f32, Vec3)> = (0..8)
                .flat_map(|i| [6.0f32, 10.0, 15.0, 20.0].map(move |r| (i, r)))
                .filter_map(|(i, r)| {
                    let a = i as f32 * std::f32::consts::TAU / 8.0;
                    let p = nt.translation + Vec3::new(a.cos() * r, 0.0, a.sin() * r);
                    heights.height_at(p).filter(|h| *h > crate::water::surface(p, Some(*h)) + 0.2).map(|h| (r, p.with_y(h)))
                })
                .collect();
            let ground = spots
                .iter()
                .filter(|(_, p)| sees(*p))
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .or_else(|| spots.iter().max_by(|a, b| a.1.y.total_cmp(&b.1.y)))
                .map(|(_, p)| *p);
            if let Some(feet) = ground {
                teleport(&mut at, &mut player, to_dunia(clear_of_kept(feet)), 0.0);
                host.quiet = 0.0;
                *shooting = None;
                return;
            }
        }
        // On foot and beyond the guns' reach: nearer first.
        let reach = inventory
            .slots
            .iter()
            .map(|s| &inventory.defs[s.def])
            .filter(|d| d.class != crate::weapons::WeaponClass::Melee)
            .map(|d| d.range)
            .fold(0.0, f32::max);
        let flat = (nt.translation - at.translation).with_y(0.0);
        if !fixed && flat.length() > (reach * 0.6).max(SHOOT_FROM * 1.5) {
            let mut feet = nt.translation - flat.normalize_or(Vec3::Z) * SHOOT_FROM;
            feet.y = heights.height_at(feet).unwrap_or(nt.translation.y - crate::player::HALF_HEIGHT);
            teleport(&mut at, &mut player, to_dunia(clear_of_kept(feet)), 0.0);
            host.quiet = 0.0;
            *shooting = None;
            return;
        }
        let eye = at.translation + Vec3::Y * (player.eye - crate::player::HALF_HEIGHT);
        let aim = head.map_or(nt.translation + Vec3::Y * 0.4, |h| h.0 - Vec3::Y * 0.1) - eye;
        player.yaw = (-aim.x).atan2(-aim.z);
        player.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.3));
        if std::env::var("FC3_STORY_WAIT_DEBUG").is_ok() && (now - time.delta_secs()) as u32 / 3 != now as u32 / 3 {
            info!(
                "story autopilot: shooting {}: him at {:?} ({:.0} health), the player at {:?} (swimming {}), {} with {} in the magazine",
                actor.id,
                to_dunia(nt.translation),
                nh.current,
                to_dunia(at.translation),
                player.swimming,
                inventory.def().name,
                inventory.slot().mag
            );
        }
        let hostile: std::collections::HashSet<u64> = npcs.iter().filter(|q| q.5.hostile()).map(|q| q.4.id).collect();
        // Not beside someone to keep safe (`KEEP_CLEAR`; people, not the
        // story's animals): away from them first (now and then: a companion
        // keeps coming).
        let beside = story_actors
            .iter()
            .filter(|(_, a, creature)| !a.dead && !creature && a.id != actor.id && !hostile.contains(&a.id))
            .map(|(t, a, _)| (t.translation, a.id))
            .find(|(p, _)| p.with_y(0.0).distance(at.translation.with_y(0.0)) < KEEP_CLEAR);
        if let Some((by, who)) = beside.filter(|_| !fixed && now - *kept_clear > 15.0) {
            *kept_clear = now;
            let away = (at.translation - by).with_y(0.0).normalize_or(Vec3::X);
            let mut feet = by + away * (KEEP_CLEAR + 4.0);
            feet.y = heights.height_at(feet).unwrap_or(at.translation.y - crate::player::HALF_HEIGHT);
            info!("story autopilot: {who}, someone to keep safe, beside the player; stepping away");
            teleport(&mut at, &mut player, to_dunia(feet), 0.0);
            host.quiet = 0.0;
            *shooting = None;
            return;
        }
        // No shot past someone not to be shot (a captive among his
        // guards): held, and after a while taken from the foe's other side.
        let target = aim + eye;
        let in_line = story_actors.iter().filter(|(_, a, _)| !a.dead && a.id != actor.id && !hostile.contains(&a.id)).find_map(|(t, _, _)| {
            let p = t.translation + Vec3::Y * 0.3;
            let along = (p - eye).dot(aim.normalize_or_zero()).clamp(0.0, aim.length());
            // (Wider with range: the gun's spread. Right beside him, only
            // straight ahead: JAS_130's Liza at the wheel as he shoots from
            // the seat next to her.)
            let width = if along < BESIDE { 0.35 } else { 1.2 + along * 0.025 };
            ((eye + aim.normalize_or_zero() * along).distance(p) < width && along < aim.length() + 1.0).then_some(p)
        });
        if let Some(bystander) = in_line {
            let since = *blocked.get_or_insert(now);
            if now - since > 3.0 && !fixed {
                let away = (target - bystander).with_y(0.0).normalize_or(Vec3::X);
                let mut feet = nt.translation + away * 6.0;
                feet.y = heights.height_at(feet).unwrap_or(nt.translation.y - crate::player::HALF_HEIGHT);
                info!("story autopilot: {} in the line of fire; round to {}'s other side", "someone", actor.id);
                teleport(&mut at, &mut player, to_dunia(clear_of_kept(feet)), 0.0);
                host.quiet = 0.0;
                *blocked = None;
            }
            return;
        }
        *blocked = None;
        use crate::controls::Action;
        // A long shot (a sniping challenge's guards, 80 m off): through the
        // sights, a sniper rifle's breath held, fired once they are up.
        let class = inventory.def().class;
        let long = aim.length() > LONG_SHOT && class != crate::weapons::WeaponClass::Melee;
        if long {
            actions.hold(Action::Aim);
            if class == crate::weapons::WeaponClass::Sniper {
                actions.hold(Action::Sprint);
            }
        }
        // (Not straight after being moved: the aim was taken from where the
        // player stood.)
        if now >= *next_press && !player.spawning && now - player.spawned_at >= 0.5 && (!long || inventory.aim_blend > 0.95) {
            *next_press = now + 0.25;
            // The gun that harms him most from here, of those that reach him
            // (a heavy stood a pistol for minutes beside the rifle carried),
            // else the one that reaches furthest: the next weapon round, as
            // the wheel would turn it, until it's in hand.
            let dist = aim.length();
            let score = |slot: usize| {
                let d = &inventory.defs[inventory.slots[slot].def];
                if d.class == crate::weapons::WeaponClass::Melee {
                    (false, 0.0)
                } else if d.range >= dist {
                    (true, d.damage * d.pellets.max(1) as f32 / d.interval.max(0.05))
                } else {
                    (false, d.range)
                }
            };
            let best = (0..inventory.slots.len()).max_by(|a, b| score(*a).partial_cmp(&score(*b)).unwrap_or(std::cmp::Ordering::Equal));
            // (On a mounted gun, that gun.)
            if player.mounted.is_none() && best.is_some_and(|b| b != inventory.current && score(b) > score(inventory.current)) {
                actions.cycle = 1;
            } else {
                actions.press(Action::Fire);
            }
        }
        return;
    }
    if let Some((nt, nh, _, _, actor, _, head)) = quest.filter(|q| (engaged || host.quiet > 6.0 || wanted.contains(&q.4.id)) && !on_call) {
        let kind = if nh.dead { "search" } else { "take down" };
        if *last != Some((actor.id, kind)) {
            info!("story autopilot: {kind} {}", host.story.data().name_of(actor.id).unwrap_or("?"));
            *last = Some((actor.id, kind));
            *shooting = None;
        }
        // (How long he has stood it: see `stubborn`.)
        match *shooting {
            Some((id, hp, _)) if id == actor.id && nh.current < hp => *shooting = Some((id, nh.current, now)),
            Some((id, ..)) if id == actor.id => {}
            _ => *shooting = Some((actor.id, nh.current, now)),
        }
        if std::env::var("FC3_STORY_WAIT_DEBUG").is_ok() && (now - time.delta_secs()) as u32 / 3 != now as u32 / 3 {
            info!(
                "story autopilot: {kind} {}: him at {:?}, the player at {:?} (swimming {}, driving {})",
                actor.id,
                to_dunia(nt.translation),
                to_dunia(at.translation),
                player.swimming,
                player.driving
            );
        }
        // Behind him (the way he faces is his -z), or beside the body.
        let behind = nt.translation + (nt.rotation * Vec3::Z).with_y(0.0).normalize_or(Vec3::Z) * if nh.dead { 0.9 } else { 1.0 };
        let delta = nt.translation - at.translation;
        if Vec2::new(delta.x, delta.z).length() > 1.6 || delta.y.abs() > 1.5 {
            let mut feet = behind;
            feet.y = heights.height_at(feet).unwrap_or(nt.translation.y - crate::player::HALF_HEIGHT).max(nt.translation.y - crate::player::HALF_HEIGHT - 0.5);
            teleport(&mut at, &mut player, to_dunia(feet), 0.0);
            host.quiet = 0.0;
            return;
        }
        player.yaw = (-delta.x).atan2(-delta.z);
        // At his head (a seated man's is low: a level swing goes over it).
        let eye = at.translation.y + player.eye - crate::player::HALF_HEIGHT;
        player.pitch = head.filter(|_| !nh.dead).map_or(0.0, |h| (h.0.y - 0.15 - eye).atan2(Vec2::new(delta.x, delta.z).length().max(0.3)));
        let now = time.elapsed_secs();
        if now >= *next_press {
            *next_press = now + 0.6;
            use crate::controls::Action;
            // (Melee takes the machete out, or takes him down with the
            // skill; fire swings it.)
            let blade = inventory.def().class == crate::weapons::WeaponClass::Melee;
            actions.press(if nh.dead {
                Action::Interact
            } else if blade {
                Action::Fire
            } else {
                Action::Melee
            });
        }
        return;
    }
    // A pickup a script waits on (a mission's diamonds): gone to and taken
    // (from wherever a high trigger held him up).
    let awaited = host.story.awaited_pickups();
    // (Not in a hunt with its quarry about and the weapon for it, with
    // ammo: its ammo laid out is spare.)
    let hunt_ready = host.story.hunt_weapon().and_then(|c| hunt_class(&c)).is_some_and(|w| {
        story_actors.iter().any(|(_, a, creature)| creature && !a.dead && (to_kill.contains(&a.id) || counted.contains(&a.id)))
            && inventory.slots.iter().any(|s| inventory.defs[s.def].class == w && s.mag + s.reserve > 0)
    });
    if !awaited.is_empty() && host.quiet > 6.0 && !hunt_ready {
        let near = plants
            .iter()
            .filter(|(p, _)| awaited.contains(&p.id))
            .map(|(p, t)| (p.id, t.translation()))
            .min_by(|a, b| a.1.distance(at.translation).total_cmp(&b.1.distance(at.translation)));
        if let Some((id, spot)) = near {
            if *last != Some((id, "pick up")) {
                info!("story autopilot: pick up {}", host.story.data().name_of(id).unwrap_or("?"));
                *last = Some((id, "pick up"));
            }
            let feet = at.translation - Vec3::Y * crate::player::HALF_HEIGHT;
            let to = spot - feet;
            if Vec2::new(to.x, to.z).length() > 1.2 || to.y.abs() > 1.5 {
                // Beside it, on its level.
                let side = Vec3::new(-to.x, 0.0, -to.z).normalize_or(Vec3::Z);
                teleport(&mut at, &mut player, to_dunia(spot + side * 0.8), 0.0);
                host.quiet = 0.0;
                return;
            }
            let eye = at.translation + Vec3::Y * (player.eye - crate::player::HALF_HEIGHT);
            let aim = spot - eye;
            player.yaw = (-aim.x).atan2(-aim.z);
            player.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.3));
            let now = time.elapsed_secs();
            if now >= *next_press && !scripted.ignored.contains("use") {
                *next_press = now + 0.6;
                debug!(
                    "story autopilot: use at pickup {id}: {:.2} off, {:.2} up, noclip {}, spawning {}",
                    Vec2::new(to.x, to.z).length(),
                    to.y,
                    player.noclip,
                    player.spawning
                );
                actions.press(crate::controls::Action::Interact);
            }
            return;
        }
        if host.quiet - time.delta_secs() <= 20.0 && host.quiet > 20.0 {
            info!("story autopilot: awaited pickups {awaited:?} aren't in the world");
            // One across the island (WIL_010's tracking device, at Willis's
            // hut): beside it, so that it streams in.
            // (Not while a block still counts foes down: the fight first.)
            let fighting = npcs.iter().any(|(_, h, _, _, actor, npc, _)| !h.dead && counted.contains(&actor.id) && npc.hostile());
            let data = host.story.data();
            let far = awaited.iter().find(|id| !far_pickups.contains(*id)).and_then(|id| data.entities.get(id).map(|e| (*id, e.pos)));
            if let Some((id, pos)) = far.filter(|_| !fighting) {
                info!("story autopilot: go to pickup {}", data.name_of(id).unwrap_or("?"));
                far_pickups.insert(id);
                teleport(&mut at, &mut player, to_dunia(to_bevy(pos) + Vec3::Z * 1.5), 0.0);
                host.quiet = 0.0;
                return;
            }
        }
    }
    // A character or body a script waits to see through the camera (a VIP's
    // corpse to photograph): a few steps off, the camera raised at it.
    let photos: Vec<u64> = host.story.awaited_discoveries().into_iter().filter(|id| !taken.taken.contains(id)).collect();
    if !photos.is_empty() && host.quiet > 20.0 && host.quiet - time.delta_secs() <= 20.0 {
        let found: Vec<u64> = npcs.iter().filter(|q| photos.contains(&q.4.id)).map(|q| q.4.id).collect();
        info!("story autopilot: to photograph {photos:?} (in the world {found:?}, camera {})", scripted.has_camera());
    }
    // (Once begun, it keeps the camera up: the story stirring meanwhile, as
    // a script's timer does, mustn't drop it every few frames. An alarm to
    // switch off comes first, as for takedowns: its own autopilot goes there.)
    let photographing = last.is_some_and(|l| l.1 == "photograph");
    if !photos.is_empty() && !alarm_first && (host.quiet > 6.0 || photographing) && scripted.has_camera() {
        let by_distance = |a: &(u64, f32, Vec3), b: &(u64, f32, Vec3)| a.2.distance(at.translation).total_cmp(&b.2.distance(at.translation));
        // (Where to aim: a body's chest, a dead one's lower; a thing's
        // middle, WIL_010's packages' barcodes.)
        let near = npcs
            .iter()
            .filter(|(_, _, _, _, a, _, _)| photos.contains(&a.id))
            .map(|(t, _, _, _, a, _, _)| (a.id, if a.dead { -0.6 } else { 0.3 }, t.translation))
            .min_by(by_distance)
            .or_else(|| {
                // (Where it is now, if it is in the world: one on a truck
                // moves with it. Else where it was placed.)
                let data = host.story.data();
                photos
                    .iter()
                    .filter_map(|id| {
                        let live = placed.iter().find(|(p, _)| p.0 == *id).map(|(_, gt)| gt.translation());
                        live.or_else(|| data.entities.get(id).map(|e| to_bevy(e.pos))).map(|at| (*id, 0.1, at))
                    })
                    .min_by(by_distance)
            });
        if let Some((id, rise, spot)) = near {
            if *last != Some((id, "photograph")) {
                info!("story autopilot: photograph {}", host.story.data().name_of(id).unwrap_or("?"));
                *last = Some((id, "photograph"));
            }
            let flat = (at.translation - spot).with_y(0.0);
            // A thing (not a character): from where it can be seen, standing
            // on something (JAS_200's meeting, held on a catwalk over a
            // canyon: from the spot the player was sent to watch it from,
            // not off the catwalk's edge, down in the canyon under it).
            if !npcs.iter().any(|q| q.4.id == id) {
                let filter = avian3d::prelude::SpatialQueryFilter::from_excluded_entities([me]);
                let aim = spot + Vec3::Y * rise;
                // (Not steeply up or down at it: the view's pitch has its
                // limits.)
                let sees = |eye: Vec3| {
                    let to = aim - eye;
                    let dist = to.length();
                    to.y.abs() < to.with_y(0.0).length() * 1.7
                        && dist < 60.0
                        && Dir3::new(to).is_ok_and(|dir| spatial.cast_ray(eye, dir, dist, true, &filter).is_none_or(|hit| hit.distance > dist - 1.0))
                };
                let feet_now = at.translation - Vec3::Y * crate::player::HALF_HEIGHT;
                if photo_stand.at.is_none_or(|s| s.0 != id) {
                    if photo_stand.at.is_some() {
                        photo_stand.tried.clear();
                    }
                    let tried = &photo_stand.tried;
                    let fresh = |feet: Vec3| tried.iter().all(|t| t.distance(feet) > 3.0);
                    let toward = flat.normalize_or(Vec3::Z);
                    let stand = if !player.noclip && fresh(feet_now) && sees(feet_now + Vec3::Y * player.eye) {
                        Some(feet_now)
                    } else {
                        [10.0f32, 16.0, 24.0, 32.0].into_iter().flat_map(|d| (0..8).map(move |k| (d, k))).find_map(|(d, k)| {
                            let side = Quat::from_rotation_y(k as f32 * std::f32::consts::FRAC_PI_4) * toward;
                            let probe = spot + side * d + Vec3::Y * 6.0;
                            let floor = spatial.cast_ray(probe, Dir3::NEG_Y, 80.0, true, &filter)?;
                            let feet = probe - Vec3::Y * floor.distance;
                            (fresh(feet) && sees(feet + Vec3::Y * player.eye)).then_some(feet)
                        })
                    };
                    info!("story autopilot: photo stand {:?}", stand.map(to_dunia));
                    photo_stand.at = stand.map(|feet| (id, feet, 0, 0.0));
                }
                let PhotoStand { at: stand, tried } = &mut *photo_stand;
                if let Some((_, feet, tries, held)) = stand.as_mut() {
                    if feet_now.distance(*feet) > 2.0 && *tries < 2 {
                        *tries += 1;
                        teleport(&mut at, &mut player, to_dunia(*feet), 0.0);
                        host.quiet = 0.0;
                        return;
                    }
                    // (No photo from there after a while: another stand.)
                    *held += time.delta_secs();
                    if *held > PHOTO_PATIENCE {
                        info!("story autopilot: no photo from {:?}; another stand", to_dunia(*feet));
                        tried.push(*feet);
                        *stand = None;
                        return;
                    }
                    let eye = at.translation + Vec3::Y * (player.eye - crate::player::HALF_HEIGHT);
                    let to = aim - eye;
                    player.yaw = (-to.x).atan2(-to.z);
                    player.pitch = to.y.atan2(Vec2::new(to.x, to.z).length().max(0.3));
                    actions.hold(crate::controls::Action::Tag);
                    return;
                }
            }
            // (Held up in the air by a trigger, the camera won't come up.)
            if player.noclip || !(1.2..10.0).contains(&flat.length()) {
                if std::env::var("FC3_CAMERA_DEBUG").is_ok() {
                    info!("story autopilot: photo spot: {:.1} m off the body, noclip {}, spawning {}", flat.length(), player.noclip, player.spawning);
                }
                let side = flat.normalize_or(Vec3::Z);
                let mut feet = spot + side * 4.0;
                feet.y = heights.height_at(feet).unwrap_or(spot.y - crate::player::HALF_HEIGHT).max(spot.y - crate::player::HALF_HEIGHT - 0.5);
                teleport(&mut at, &mut player, to_dunia(feet), 0.0);
                host.quiet = 0.0;
                return;
            }
            let eye = at.translation + Vec3::Y * (player.eye - crate::player::HALF_HEIGHT);
            let aim = spot + Vec3::Y * rise - eye;
            player.yaw = (-aim.x).atan2(-aim.z);
            player.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.3));
            actions.hold(crate::controls::Action::Tag);
            return;
        }
    }
    if player.noclip {
        return;
    }
    // A hunt (the animal quests): the quarry a block counts, with the weapon
    // it asks for in hand (another fails the hunt). Or, no hunt on, a
    // creature a block counts down, near, with what is in hand: JAS_100's
    // outpost dog, which its pirates' count waits on (the fighters' step
    // goes only after men).
    let counted_beast =
        story_actors.iter().any(|(t, a, creature)| creature && !a.dead && counted.contains(&a.id) && t.translation.distance(at.translation) < FIGHT_REACH);
    if let Some(class) = host.story.hunt_weapon().or_else(|| counted_beast.then(|| "any".to_string())) {
        // (The story's creatures: a rare one isn't one of the wildlife's. The
        // one whose harm is weighed, the rare target, as well as those a
        // block counts down.)
        let quarry = story_actors
            .iter()
            .filter(|(_, a, creature)| *creature && !a.dead && (to_kill.contains(&a.id) || counted.contains(&a.id)))
            // (The one being hunted kept while it lives.)
            .min_by(|a, b| {
                let held = |id: u64| hunting.is_some_and(|h| h.0 == id);
                held(b.1.id).cmp(&held(a.1.id)).then(a.0.translation.distance(at.translation).total_cmp(&b.0.translation.distance(at.translation)))
            });
        if let Some((qt, actor, _)) = quarry {
            let id = actor.id;
            if *last != Some((id, "hunt")) {
                info!("story autopilot: hunt {id} with a {class}");
                *last = Some((id, "hunt"));
            }
            let want = hunt_class(&class);
            let melee = want == Some(crate::weapons::WeaponClass::Melee);
            if std::env::var("FC3_STORY_WAIT_DEBUG").is_ok() && (now - time.delta_secs()) as u32 / 5 != now as u32 / 5 {
                let have = want.is_some_and(|w| inventory.slots.iter().any(|s| inventory.defs[s.def].class == w));
                info!(
                    "story autopilot: hunting {id} at {:?}, {:.0} m off; a {class} ({want:?}) asked for, {} ({:?}) in hand with {} loaded, one carried {have}",
                    to_dunia(qt.translation),
                    qt.translation.distance(at.translation),
                    inventory.def().name,
                    inventory.def().class,
                    inventory.slot().mag
                );
            }
            // The weapon asked for, not carried: one of its kind laid out for
            // the hunt (its bow, by the gear-up spot) taken first.
            let carried = |w| inventory.slots.iter().any(|s| inventory.defs[s.def].class == w);
            let to_take = want.filter(|w| !carried(*w)).and_then(|w| {
                plants
                    .iter()
                    .filter(
                        |(p, _)| matches!(p.kind, crate::pickups::PickupKind::Weapon(name) if inventory.defs.iter().any(|d| d.name == name && d.class == w)),
                    )
                    .min_by(|a, b| a.1.translation().distance(at.translation).total_cmp(&b.1.translation().distance(at.translation)))
            });
            if let Some((pickup, pt)) = to_take {
                let spot = pt.translation();
                let feet = at.translation - Vec3::Y * crate::player::HALF_HEIGHT;
                if Vec2::new(spot.x - feet.x, spot.z - feet.z).length() > 1.5 || (spot.y - feet.y).abs() > 1.5 {
                    info!("story autopilot: to the {:?} for the hunt", pickup.kind);
                    let mut stand = spot + Vec3::X * 0.8;
                    stand.y = heights.height_at(stand).unwrap_or(spot.y).max(spot.y - 0.5);
                    teleport(&mut at, &mut player, to_dunia(stand), 0.0);
                    host.quiet = 0.0;
                } else if now >= *next_press {
                    *next_press = now + 0.5;
                    let aim = spot - (at.translation + Vec3::Y * (player.eye - crate::player::HALF_HEIGHT));
                    player.yaw = (-aim.x).atan2(-aim.z);
                    player.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.3));
                    actions.press(crate::controls::Action::Interact);
                }
                return;
            }
            let target = qt.translation + Vec3::Y * 0.3;
            // Not down after a while from one side (a rock in the way): from
            // another, a quarter round and nearer.
            let since = match *hunting {
                Some((hid, s)) if hid == id => s,
                _ => {
                    *hunting = Some((id, now));
                    now
                }
            };
            let tries = ((now - since) / HUNT_TRY) as u32;
            let reach = if melee { HUNT_MELEE } else { (HUNT_FROM / (1.0 + tries as f32 * 0.5)).max(HUNT_NEAREST) };
            let side = Quat::from_rotation_y(tries as f32 * std::f32::consts::FRAC_PI_2) * Vec3::Z;
            let mut stand = qt.translation + side * reach;
            stand.y = heights.height_at(stand).unwrap_or(qt.translation.y);
            let feet = at.translation - Vec3::Y * crate::player::HALF_HEIGHT;
            if Vec2::new(stand.x - feet.x, stand.z - feet.z).length() > if melee { 1.0 } else { 3.0 } {
                teleport(&mut at, &mut player, to_dunia(stand), 0.0);
                host.quiet = 0.0;
                return;
            }
            let eye = at.translation + Vec3::Y * (player.eye - crate::player::HALF_HEIGHT);
            let aim = target - eye;
            player.yaw = (-aim.x).atan2(-aim.z);
            player.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.3));
            if now >= *next_press && !player.spawning {
                *next_press = now + 0.25;
                use crate::controls::Action;
                let held = inventory.def().class;
                let has = |c| inventory.slots.iter().any(|s| inventory.defs[s.def].class == c);
                if want.is_some_and(|w| w != held && has(w)) {
                    actions.cycle = 1;
                } else if inventory.slot().mag == 0 && !melee {
                    actions.press(Action::Reload);
                } else {
                    if !melee {
                        actions.hold(Action::Aim);
                    }
                    actions.press(Action::Fire);
                }
            }
            return;
        }
        // The quarry down, skinned (its looting awaited).
        let skin = story_actors
            .iter()
            .filter(|(_, a, creature)| *creature && a.dead && host.story.awaits_event(a.id, "OnLooting"))
            .min_by(|a, b| a.0.translation.distance(at.translation).total_cmp(&b.0.translation.distance(at.translation)));
        if let Some((bt, body, _)) = skin {
            if *last != Some((body.id, "skin")) {
                info!("story autopilot: skin {}", body.id);
                *last = Some((body.id, "skin"));
            }
            let flat = (bt.translation - at.translation).with_y(0.0);
            if flat.length() > 1.6 || (bt.translation.y - at.translation.y).abs() > 1.5 {
                let mut feet = bt.translation - flat.normalize_or(Vec3::Z) * 1.0;
                feet.y = heights.height_at(feet).unwrap_or(bt.translation.y);
                teleport(&mut at, &mut player, to_dunia(feet), 0.0);
                host.quiet = 0.0;
            } else if now >= *next_press {
                *next_press = now + 0.6;
                actions.press(crate::controls::Action::Interact);
            }
            return;
        }
    }
    if looted_wanted.is_empty() {
        *last = None;
        return;
    }
    let wanted = looted_wanted.clone();
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

    /// A region is left by its nearest side, a step beyond it, on the ground.
    #[test]
    fn a_region_is_left_by_its_nearest_side() {
        let shape = fc3_story::triggers::Shape::Box { centre: [0.0, 0.0, 0.0], half: [10.0, 50.0, 20.0], yaw: 0.0 };
        let at = exit_point(&shape, [0.0, 0.0, 0.0], |_, _| Some(1.0)).unwrap();
        assert!(!shape.contains([at[0], at[1], 1.5]));
        assert!(at[0].abs() > 10.0 && at[0].abs() < 16.0 && at[1].abs() < 1.0 && at[2] == 1.0, "{at:?}");
    }

    #[test]
    fn triggers_left_alone_are_read_back_a_line_each() {
        let fatal = parse_fatal("2075603644094367013 Enter\n\nnot-an-id Enter\n2077037199833972030 Enter \n12\n");
        assert_eq!(fatal.len(), 2);
        assert!(fatal.contains(&(2075603644094367013, "Enter".to_string())));
        assert!(fatal.contains(&(2077037199833972030, "Enter".to_string())));
    }

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
        settings.rebind(Action::NextWeapon, Binding::Key(KeyCode::KeyT));
        assert_eq!(prompt_text("[GadgetWheel] / [WeaponWheelSelect] / [SwitchWeapon]", &actions, &settings), "[T] / [Mouse] / [T]");
        assert_eq!(prompt_text("[Interact] Use — [ThrowRock] Distract [unknown] [unfinished", &actions, &settings),
            "[V] Use — [MMB] Distract [unknown] [unfinished");
        actions.gamepad_active = true;
        assert_eq!(prompt_text("[GadgetWheel] / [WeaponWheelSelect] / [SwitchWeapon]", &actions, &settings), "[Y] / [Right stick] / [Y]");
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
        let (items, held) = story_weapons(&defs, &slots, 1, &shop, &HashMap::new());
        assert_eq!(items.len(), 2);
        assert_eq!(held.as_deref(), Some("weapons:FC3/Kimber"));
        assert_eq!(items[1].count, 1);
        assert_eq!(items[1].category, "handgun");
        assert!(story_weapons(&defs, &slots, 2, &shop, &HashMap::new()).1.is_none());
        // One a script gave by its own archetype goes by that name.
        let granted = [(index("1911"), "weapons:FC3/Kimber.Special".to_string())].into_iter().collect();
        assert_eq!(story_weapons(&defs, &slots, 1, &shop, &granted).1.as_deref(), Some("weapons:FC3/Kimber.Special"));
    }

    #[test]
    fn scripts_show_the_player_elsewhere_on_the_maps() {
        // BAM_020's sunken ship: on the maps where it lies, turned as the
        // player is.
        let mut scripted = ScriptedPlayer::default();
        let t = Transform::from_xyz(1.0, -20.0, 3.0).with_rotation(Quat::from_rotation_y(0.5));
        assert_eq!(scripted.on_map(&t), t);
        scripted.map_position = Some(to_bevy([3382.0, 2316.0, 108.0]));
        let shown = scripted.on_map(&t);
        assert_eq!(shown.translation, Vec3::new(3382.0, 108.0, -2316.0));
        assert_eq!(shown.rotation, t.rotation);
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
