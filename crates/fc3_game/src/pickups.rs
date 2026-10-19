//! World pickups (ammo, health, weapons, loot, herbs) and explosive props.
//!
//! Pickup and explosive entities come from the world-sector entity lists;
//! `objects` tags them when a sector spawns. Collected pickups and destroyed
//! explosives are remembered so they stay gone when a sector reloads. All
//! rewards and rules here are this project's own design.

use std::collections::HashSet;

use bevy::light::NotShadowCaster;
use bevy::prelude::*;

use crate::cache_assets::Fc3Model;
use crate::combat::{Died, Health, PlayerStats};
use crate::hud::HudState;
use crate::player::Player;
use crate::weapons::{grant_weapon, model_handle, Explosion, Inventory, Rng, WeaponClass};

pub struct PickupsPlugin;

impl Plugin for PickupsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WorldState>().add_message::<PickedUp>().add_systems(
            Update,
            (
                attach_pickup_models,
                spin_weapon_pickups,
                glint,
                (mark_takedown_loot, interact, loot_bodies).chain().before(crate::hud::HudSet).after(crate::turrets::man_turrets),
                detonate_explosives,
            ),
        );
    }
}

/// A takedown's victim, searched as he falls (`SP_LootTakedown`).
#[derive(Component)]
pub struct AutoLoot;

/// Takedowns (the blows that kill outright) mark their victims to be
/// searched once `SP_LootTakedown` is learned.
fn mark_takedown_loot(
    perks: Res<crate::progression::Perks>,
    mut hits: MessageReader<crate::combat::DamageEvent>,
    players: Query<(), With<Player>>,
    mut commands: Commands,
) {
    for hit in hits.read() {
        if perks.loot_takedown && hit.amount >= 10_000.0 && hit.instigator.is_some_and(|i| players.contains(i)) {
            if let Ok(mut e) = commands.get_entity(hit.target) {
                e.insert(AutoLoot);
            }
        }
    }
}

/// A fallen enemy that hasn't been searched yet.
#[derive(Component)]
pub struct LootableBody {
    pub role: crate::npc::Role,
    /// The body's rig (its graphic kit picks the game's loot for it).
    pub rig: Option<Entity>,
    /// An animal's, skinned (a hunt's rare one: its own loot, its skin).
    pub skin: bool,
}

/// A character's own loot groups (its entity's `CFCXPawnLootComponent`:
/// group and chance in percent), which its body gives instead of its kit's:
/// a mission's operator carries the codes the mission wants.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct OwnLoot(pub Vec<(u32, f32)>);

impl OwnLoot {
    /// The groups a loot component names (none for the unset ones).
    pub fn from_component(loot: &fc3_formats::fcb::Object) -> Option<OwnLoot> {
        let groups: Vec<(u32, f32)> = loot
            .child("LootGroups")
            .iter()
            .flat_map(|l| l.children_named("Group"))
            .filter_map(|g| Some((g.u32("lootGroup").filter(|&id| id != 0 && id != u32::MAX)?, g.f32("percent").unwrap_or(100.0).clamp(0.0, 100.0))))
            .collect();
        (!groups.is_empty()).then_some(OwnLoot(groups))
    }

    /// Whether one of its groups can give one of the items `wanted`, by the
    /// name id scripts know an item by (`name_of` gives a group item's; a
    /// group names the catalog's own ids: JAS_200's role sheet is item 997,
    /// named 104540).
    pub fn gives(&self, groups: &[fc3_formats::progression::LootGroup], name_of: impl Fn(u32) -> Option<u32>, wanted: &[u32]) -> bool {
        !wanted.is_empty()
            && self.0.iter().any(|&(group, chance)| {
                chance > 0.0 && groups.iter().any(|g| g.id == group && g.items.iter().any(|&(id, _)| name_of(id).is_some_and(|n| wanted.contains(&n))))
            })
    }
}

/// A body the player has searched (the story hears it: `OnLooting`).
#[derive(Component)]
pub struct Searched;

/// Searching a body: cash, ammo for the guns carried, and sometimes what the
/// enemy fought with.
#[allow(clippy::too_many_arguments)]
pub(crate) fn loot_bodies(
    actions: Res<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    mut hud: ResMut<HudState>,
    mut progress: ResMut<crate::progression::Progress>,
    perks: Res<crate::progression::Perks>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
    bodies: Query<(Entity, &LootableBody, &GlobalTransform, Option<&OwnLoot>)>,
    mut players: Query<(&Transform, &Player, &mut Inventory, &Health, &mut PlayerStats)>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    (busy, mut gestures, mut looted): (Res<crate::fp::HandsBusy>, MessageWriter<crate::fp::FpAction>, MessageWriter<crate::progression::ItemLooted>),
    (crafting, kits, auto): (Option<Res<crate::crafting::CraftCatalog>>, Query<&crate::characters::KitId>, Query<(), With<AutoLoot>>),
    text: Option<Res<crate::hud::GameText>>,
) {
    use crate::npc::Role;
    let Ok((pt, player, mut inv, health, mut stats)) = players.single_mut() else { return };
    // A takedown's victim is searched as he falls, with no prompt.
    let taken = bodies.iter().find(|(e, _, _, _)| auto.contains(*e));
    // Another interaction is on offer (a pickup is nearer).
    if taken.is_none() && (hud.prompt.is_some() || health.dead || player.noclip || player.driving || player.swimming || busy.busy()) {
        return;
    }
    // (Bodies, like the player, are placed by their capsule's centre.)
    let centre = pt.translation;
    let Some((e, body, own)) = taken.map(|(e, b, _, own)| (e, b, own)).or_else(|| {
        bodies
            .iter()
            .map(|(e, b, gt, own)| {
                (e, b, own, Vec2::new(gt.translation().x - centre.x, gt.translation().z - centre.z).length() + (gt.translation().y - centre.y).abs() * 0.5)
            })
            .filter(|(_, _, _, d)| *d < 2.4)
            .min_by(|a, b| a.3.total_cmp(&b.3))
            .map(|(e, b, own, _)| (e, b, own))
    }) else {
        return;
    };
    if taken.is_none() {
        let (what, game) = if body.skin { ("skin the animal", crate::hud::prompt::SKIN_ANIMAL) } else { ("search the body", crate::hud::prompt::LOOT_BODY) };
        hud.prompt = Some(
            text.as_deref()
                .and_then(|t| t.prompt(game, &actions, &settings))
                .unwrap_or_else(|| format!("{}  {what}", actions.label(crate::controls::Action::Interact, &settings))),
        );
        if !actions.used() {
            return;
        }
        gestures.write(if body.skin { crate::fp::FpAction::Skin { large: true } } else { crate::fp::FpAction::LootBody { crouch: player.crouching } });
    }
    *progress.stats.entry(crate::progression::stat::BODIES_LOOTED).or_insert(0) += 1;
    // What the game gives this kind of enemy: cash, loot to sell, ammunition.
    let kit = body.rig.and_then(|r| kits.get(r).ok()).map_or(0, |k| k.0);
    // (Its own loot when its entity names some, else its kit's.)
    let rolled = crafting.as_ref().and_then(|c| match own {
        Some(own) => Some(c.roll_groups(&own.0, &mut rng)),
        None => c.roll_body(kit, &mut rng),
    });
    if let Some(loot) = rolled {
        let (found, full, items) = take_loot(&loot, crafting.as_ref().unwrap(), &mut progress, &mut inv, &mut stats, &perks);
        // The story hears of each item taken (`CPawnLootItemEvent`).
        for id in items {
            looted.write(crate::progression::ItemLooted(crate::progression::LootKind::Item(id)));
        }
        let mut note = if found.is_empty() { "Nothing useful".to_string() } else { format!("Found {}", found.join(", ")) };
        if full {
            note += " (rucksack full)";
            hud.loot_notes.push(LOOT_RUCKSACK_FULL);
        }
        // (The original HUD shows the loot and the cash its own way.)
        hud.notify_native(note);
        sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.6 });
        commands.entity(e).remove::<LootableBody>().insert(Searched);
        return;
    }
    let mut found = Vec::new();
    // (`SP_LootDoubleCash`: twice the cash.)
    let cash = progress.add_money((5 + (rng.f32() * 40.0) as u32) * if perks.double_cash { 2 } else { 1 });
    if cash > 0 {
        found.push(format!("${cash}"));
    }
    let rounds = give_ammo(&mut inv, 0.6);
    if rounds > 0 {
        found.push(format!("{rounds} rounds"));
    }
    let extra = match body.role {
        Role::Rocket if inv.add_ammo(WeaponClass::Launcher, 1) > 0 => Some("a rocket"),
        Role::Molotov if inv.molotovs < crate::weapons::MOLOTOV_CAP => {
            inv.molotovs += 1;
            Some("a molotov")
        }
        Role::Flamer if inv.add_ammo(WeaponClass::Flamethrower, 60) > 0 => Some("fuel"),
        _ if rng.f32() < 0.12 && inv.grenades < perks.grenade_cap => {
            inv.grenades += 1;
            Some("a grenade")
        }
        _ if rng.f32() < 0.08 && stats.medkits < perks.medkit_cap => {
            stats.medkits += 1;
            Some("a medkit")
        }
        _ => None,
    };
    found.extend(extra.map(str::to_string));
    hud.notify_native(if found.is_empty() { "Nothing useful".to_string() } else { format!("Found {}", found.join(", ")) });
    sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.6 });
    commands.entity(e).remove::<LootableBody>().insert(Searched);
}

/// Takes what was rolled: the cash (doubled by `SP_LootDoubleCash`), loot items
/// into the rucksack as far as it holds, and ammunition and gadgets as far as
/// there is room. Returns what was found, whether the rucksack filled, and
/// the items taken.
fn take_loot(
    loot: &crate::crafting::BodyLoot,
    crafting: &crate::crafting::CraftCatalog,
    progress: &mut crate::progression::Progress,
    inv: &mut Inventory,
    stats: &mut PlayerStats,
    perks: &crate::progression::Perks,
) -> (Vec<String>, bool, Vec<u32>) {
    let mut found = Vec::new();
    let mut items = Vec::new();
    let cash = progress.add_money(loot.cash * if perks.double_cash { 2 } else { 1 });
    if cash > 0 {
        found.push(format!("${cash}"));
    }
    let room = crafting.loot_capacity(progress).saturating_sub(crate::crafting::loot_count(progress));
    let mut full = false;
    let mut taken = 0;
    for id in &loot.items {
        // (A mission's items take no room.)
        let quest = crafting.item(*id).is_some_and(|i| i.key.starts_with(fc3_formats::progression::QUEST_ITEMS));
        if !quest && taken >= room {
            full = true;
            continue;
        }
        taken += u32::from(!quest);
        items.push(*id);
        *progress.loot.entry(*id).or_insert(0) += 1;
        found.extend(crafting.item(*id).map(|i| i.name.clone()).filter(|n| !n.is_empty()));
    }
    for &(id, n) in &loot.supplies {
        let Some(item) = crafting.item(id) else { continue };
        let got = crate::progression::give_supply(item, n, inv, stats, perks, progress);
        if got > 0 {
            found.push(if item.gadget != 0 || n == 1 { item.name.clone() } else { format!("{got} rounds") });
        }
    }
    (found, full, items)
}

/// Persistent world changes made by the player.
#[derive(Resource, Default)]
pub struct WorldState {
    /// Entity ids of collected pickups and destroyed props.
    pub removed: HashSet<u64>,
    /// Herbs gathered toward the next medkit.
    pub herbs: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickupKind {
    Ammo,
    Explosives,
    Health,
    Grenade,
    Weapon(&'static str),
    Loot,
    Plant(crate::syringes::Plant),
    Molotov,
    /// Collectibles.
    Relic,
    Letter,
    MemoryCard,
    /// Body armour.
    Armour,
    /// A mission's own item, a weapon pickup we don't know as a weapon
    /// (WIL_010's tracking device): taken for the scripts, nothing kept.
    Mission,
}

/// How many of each collectible the island holds.
/// The original HUD's loot line for a full rucksack ("Loot Rucksack Full").
pub const LOOT_RUCKSACK_FULL: u32 = 172057;
pub const RELICS: u32 = 120;
pub const LETTERS: u32 = 20;
pub const MEMORY_CARDS: u32 = 16;

impl PickupKind {
    fn label(self) -> String {
        match self {
            PickupKind::Ammo => "ammo".into(),
            PickupKind::Explosives => "explosives".into(),
            PickupKind::Health => "medkit".into(),
            PickupKind::Grenade => "grenade".into(),
            PickupKind::Weapon(w) => w.to_string(),
            PickupKind::Loot => "loot".into(),
            PickupKind::Mission => "item".into(),
            PickupKind::Plant(p) => p.name().to_ascii_lowercase(),
            PickupKind::Molotov => "molotov".into(),
            PickupKind::Relic => "relic".into(),
            PickupKind::Letter => "lost letter".into(),
            PickupKind::MemoryCard => "memory card".into(),
            PickupKind::Armour => "body armor".into(),
        }
    }
}

pub enum Classified {
    Pickup(PickupKind),
    Explosive,
}

/// Classifies a world entity by name (pickup categories and explosive props).
pub fn classify(name: &str) -> Option<Classified> {
    let n = name.to_ascii_lowercase();
    if n.starts_with("explosives/") && (n.contains("barrel") || n.contains("tank")) {
        return Some(Classified::Explosive);
    }
    let kind = if n.starts_with("pickups/ammo/") {
        if n.contains("explosive") {
            PickupKind::Explosives
        } else if n.contains("arrow") {
            return None;
        } else {
            PickupKind::Ammo
        }
    } else if n.starts_with("pickups/health/") {
        PickupKind::Health
    } else if n.starts_with("pickups/weapons/") {
        let w = &n["pickups/weapons/".len()..];
        if w.starts_with("molotov") {
            PickupKind::Molotov
        } else if w.starts_with("frag") {
            PickupKind::Grenade
        } else if w.starts_with("proximity") || w.starts_with("ied") {
            PickupKind::Explosives
        } else if w.starts_with("ak") {
            PickupKind::Weapon("AK-74M Rifle")
        } else if w.starts_with("6p") || w.starts_with("p_") || w.starts_with("p.") {
            PickupKind::Weapon("MK23 Pistol")
        } else if w.starts_with("vz") {
            PickupKind::Weapon("MP5 SMG")
        } else if w.starts_with("rpg") {
            PickupKind::Weapon("RPG-7")
        } else if w.starts_with("hunter") {
            PickupKind::Weapon("700 Sniper")
        } else if w.starts_with("rock") {
            return None;
        } else {
            PickupKind::Ammo
        }
    } else if n.starts_with("pickups/loot/") {
        PickupKind::Loot
    } else if n.starts_with("pickups/armor/") {
        PickupKind::Armour
    } else if n.starts_with("pickups/plants/") {
        PickupKind::Plant(crate::syringes::Plant::from_name(&n)?)
    } else if n.starts_with("pickups/collectibles/") {
        let c = &n["pickups/collectibles/".len()..];
        if c.starts_with("relic") {
            PickupKind::Relic
        } else if c.starts_with("japcorpse") {
            PickupKind::Letter
        } else if c.starts_with("sdpickup") {
            PickupKind::MemoryCard
        } else {
            // Medicinal plants.
            PickupKind::Plant(crate::syringes::Plant::from_name(&n)?)
        }
    } else {
        return None;
    };
    Some(Classified::Pickup(kind))
}

/// A loot container's prototype name (its authored loot groups).
#[derive(Component)]
pub struct LootSource(pub String);

/// A placed pickup was picked up (its editor id): the story hears of it.
#[derive(Message, Clone, Copy)]
pub struct PickedUp(pub u64);

#[derive(Component)]
pub struct Pickup {
    pub kind: PickupKind,
    pub id: u64,
    /// The placed mesh representing it in the world, if any.
    pub visual: Option<Entity>,
}

/// Pickups and unsearched bodies this near the player catch the light now
/// and then: a brief white glint, each on its own beat.
const GLINT_RANGE: f32 = 28.0;
const GLINT_EVERY: f32 = 2.4;

fn glint(
    time: Res<Time>,
    players: Query<&Transform, With<Player>>,
    pickups: Query<(Entity, &GlobalTransform, &Pickup)>,
    visuals: Query<&GlobalTransform>,
    bodies: Query<(Entity, &GlobalTransform), With<LootableBody>>,
    mut particles: ResMut<crate::particles::Particles>,
) {
    use crate::particles::{Particle, Sheet};
    let Ok(pt) = players.single() else { return };
    let t = time.elapsed_secs();
    let mut flash = |e: Entity, at: Vec3| {
        let d = at.distance(pt.translation);
        if d > GLINT_RANGE {
            return;
        }
        // Its beat, from the entity (so neighbours don't glint together).
        let phase = ((e.to_bits() & 0xffff_ffff) as f32 * 0.618).fract();
        let beat = ((t / GLINT_EVERY + phase).fract() * GLINT_EVERY).min(GLINT_EVERY);
        if beat > 0.35 {
            return;
        }
        let k = (beat / 0.35 * std::f32::consts::PI).sin();
        // A little toward the eye, so the thing itself doesn't hide it.
        let at = at + (pt.translation - at).normalize_or_zero() * 0.45;
        let mut g = Particle::new(Sheet::Glow, at, 1.0, 0.22 + 0.16 * k + d * 0.005);
        g.colour = Vec4::new(1.0, 0.97, 0.88, 0.9 * k);
        particles.sprite(g);
    };
    for (e, gt, p) in &pickups {
        let at = p.visual.and_then(|v| visuals.get(v).ok()).map_or(gt.translation(), |v| v.translation());
        flash(e, at + Vec3::Y * 0.45);
    }
    for (e, gt) in &bodies {
        flash(e, gt.translation() + Vec3::Y * 0.1);
    }
}

/// A prop that explodes when destroyed.
#[derive(Component)]
pub struct Explosive {
    pub id: u64,
    /// Its blast (`authored_fx::blast::of_prop`).
    pub effect: &'static str,
}

/// Weapon pickups have no world mesh in the data; show the weapon's model.
#[derive(Component)]
struct PickupModel(Handle<Fc3Model>);

fn attach_pickup_models(
    mut commands: Commands,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    added: Query<(Entity, &Pickup), Added<Pickup>>,
    pending: Query<(Entity, &PickupModel)>,
    defs: Local<Vec<crate::weapons::WeaponDef>>,
) {
    let defs: &Vec<_> = &defs;
    let defs = if defs.is_empty() { crate::weapons::weapon_defs() } else { defs.clone() };
    for (e, p) in &added {
        let path = match p.kind {
            PickupKind::Weapon(name) => defs.iter().find(|d| d.name == name).map(|d| format!(r"graphics\__fc3_graphics\_common\weapons\{}", d.model)),
            PickupKind::Grenade => Some(crate::weapons::GRENADE_MODEL.to_string()),
            _ => None,
        };
        if let (Some(path), None) = (path, p.visual) {
            commands.entity(e).insert(PickupModel(model_handle(&server, &path)));
        }
    }
    for (e, pm) in &pending {
        let Some(m) = models.get(&pm.0) else { continue };
        if let Some(lod) = m.lods.first() {
            for part in lod.parts.iter().filter(|p| p.shown(false)) {
                commands.spawn((
                    Mesh3d(part.mesh.clone()),
                    MeshMaterial3d(part.material.clone()),
                    NotShadowCaster,
                    Transform::from_xyz(0.0, 0.25, 0.0),
                    ChildOf(e),
                ));
            }
        }
        commands.entity(e).remove::<PickupModel>().insert(SpinningPickup);
    }
}

#[derive(Component)]
struct SpinningPickup;

fn spin_weapon_pickups(time: Res<Time>, mut q: Query<&mut Transform, With<SpinningPickup>>) {
    for mut t in &mut q {
        t.rotate_y(time.delta_secs() * 1.2);
    }
}

#[allow(clippy::too_many_arguments)]
fn interact(
    mut actions: ResMut<crate::controls::ActionState>,
    (settings, text): (Res<crate::settings::Settings>, Option<Res<crate::hud::GameText>>),
    mut world: ResMut<WorldState>,
    mut hud: ResMut<HudState>,
    mut progress: ResMut<crate::progression::Progress>,
    perks: Res<crate::progression::Perks>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
    pickups: Query<(Entity, &Pickup, &GlobalTransform)>,
    mut players: Query<(&Transform, &Player, &mut Inventory, &Health, &mut PlayerStats)>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    (busy, mut gestures, mut xp, mut looted, scripted): (Res<crate::fp::HandsBusy>, MessageWriter<crate::fp::FpAction>, MessageWriter<crate::progression::XpGain>, MessageWriter<crate::progression::ItemLooted>, Res<crate::story::ScriptedPlayer>),
    (crafting, sources, owns, mut picked): (Option<Res<crate::crafting::CraftCatalog>>, Query<&LootSource>, Query<&OwnLoot>, MessageWriter<PickedUp>),
) {
    let Ok((pt, player, mut inv, health, mut stats)) = players.single_mut() else { return };
    if health.dead || player.noclip || player.driving || busy.busy() {
        return;
    }
    let fwd = Vec3::new(-player.yaw.sin(), 0.0, -player.yaw.cos());
    let feet = pt.translation - Vec3::Y * crate::player::HALF_HEIGHT;
    let mut best: Option<(f32, Entity)> = None;
    for (e, pickup, gt) in &pickups {
        if matches!(pickup.kind, PickupKind::Plant(_)) && !scripted.plant_looting {
            continue;
        }
        let to = gt.translation() - feet;
        let flat = Vec3::new(to.x, 0.0, to.z);
        let d = flat.length();
        if d > 2.6 || to.y.abs() > 2.0 {
            continue;
        }
        // Prefer what the player is facing.
        let score = d - flat.normalize_or_zero().dot(fwd) * 0.8;
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, e));
        }
    }
    let Some((_, e)) = best else { return };
    let Ok((_, p, _)) = pickups.get(e) else { return };
    if matches!(p.kind, PickupKind::Plant(_)) && crate::crafting::loot_count(&progress) >= crafting.as_ref().map_or(16, |c| c.loot_capacity(&progress)) {
        hud.prompt = Some(
            text.as_deref()
                .and_then(|t| t.get(crate::hud::prompt::RUCKSACK_FULL))
                .map_or_else(|| "Rucksack full - use or sell ingredients".into(), str::to_string),
        );
        if actions.used() || actions.just_pressed(crate::controls::Action::Interact) {
            actions.consume(crate::controls::Action::Interact);
        }
        return;
    }
    // (In the game's words: what it says for each kind of thing taken.)
    use crate::hud::prompt as gp;
    let game = match p.kind {
        PickupKind::Plant(_) => gp::GATHER_PLANT,
        PickupKind::Relic => gp::TAKE_RELIC,
        PickupKind::Letter => gp::TAKE_LETTER,
        PickupKind::MemoryCard => gp::TAKE_MEMORY_CARD,
        PickupKind::Loot => gp::LOOT_CONTAINER,
        PickupKind::Mission => gp::TAKE_ITEM,
        _ => gp::PICK_UP,
    };
    let said = text.as_deref().and_then(|t| t.prompt(game, &actions, &settings));
    hud.prompt = Some(said.unwrap_or_else(|| format!("{}  pick up {}", actions.label(crate::controls::Action::Interact, &settings), p.kind.label())));
    if !actions.used() {
        return;
    }
    // Plants are picked by hand.
    if matches!(p.kind, PickupKind::Plant(_)) && !player.swimming {
        gestures.write(crate::fp::FpAction::LootPlant { crouch: player.crouching });
    }
    // (Its own loot when its entity names some: a mission's diamonds.)
    let rolled = match (p.kind, owns.get(e), sources.get(e), crafting.as_ref()) {
        (PickupKind::Loot, Ok(own), _, Some(c)) => Some((c.roll_groups(&own.0, &mut rng), c)),
        (PickupKind::Loot, _, Ok(source), Some(c)) => c.roll_container(&source.0, &mut rng).map(|loot| (loot, c)),
        _ => None,
    };
    let msg = if let Some((loot, c)) = rolled {
        let (found, full, items) = take_loot(&loot, c, &mut progress, &mut inv, &mut stats, &perks);
        for id in items {
            looted.write(crate::progression::ItemLooted(crate::progression::LootKind::Item(id)));
        }
        let mut note = if found.is_empty() { "Nothing useful".to_string() } else { format!("Found {}", found.join(", ")) };
        if full {
            note += " (rucksack full)";
        }
        note
    } else if p.kind == PickupKind::Armour {
        let value = crafting.as_ref().map_or(0.0, |c| c.armour_value());
        progress.armour = progress.armour.max(value);
        "Body armor".to_string()
    } else {
        apply_reward(p.kind, &mut inv, &mut stats, &mut world, &mut progress, &perks, &mut rng)
    };
    if let PickupKind::Plant(plant) = p.kind {
        looted.write(crate::progression::ItemLooted(crate::progression::LootKind::Plant(plant)));
    }
    actions.consume(crate::controls::Action::Interact);
    hud.notify(msg);
    // Collectibles are worth experience as well as cash.
    // (Their XP is the game's, by kind.)
    let collectible = |what: &str| crafting.as_ref().map_or(0, |c| c.collectible_xp(what));
    let found = match p.kind {
        PickupKind::Relic => Some((collectible("relic"), "relic")),
        PickupKind::Letter => Some((collectible("letter"), "lost letter")),
        PickupKind::MemoryCard => Some((collectible("sd card"), "memory card")),
        _ => None,
    };
    if let Some((amount, reason)) = found.filter(|(amount, _)| *amount > 0) {
        xp.write(crate::progression::XpGain { amount, reason });
    }
    sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.6 });
    world.removed.insert(p.id);
    picked.write(PickedUp(p.id));
    if let Some(v) = p.visual {
        commands.entity(v).despawn();
    }
    commands.entity(e).despawn();
}

fn give_ammo(inv: &mut Inventory, mags: f32) -> u32 {
    let mut total = 0;
    let current = inv.def().class;
    let classes: Vec<WeaponClass> = inv.slots.iter().map(|s| inv.defs[s.def].class).collect();
    for class in classes {
        if matches!(class, WeaponClass::Melee | WeaponClass::Launcher | WeaponClass::GrenadeLauncher) {
            continue;
        }
        let mag = inv.defs.iter().find(|d| d.class == class).map(|d| d.magazine).unwrap_or(10);
        let share = if class == current { mags } else { mags * 0.5 };
        total += inv.add_ammo(class, (mag as f32 * share).ceil() as u32);
    }
    total
}

fn apply_reward(
    kind: PickupKind,
    inv: &mut Inventory,
    stats: &mut PlayerStats,
    world: &mut WorldState,
    progress: &mut crate::progression::Progress,
    perks: &crate::progression::Perks,
    rng: &mut Rng,
) -> String {
    let (grenade_cap, medkit_cap) = (perks.grenade_cap, perks.medkit_cap);
    match kind {
        // (Worn in `interact`, at the converted armour value.)
        PickupKind::Armour => String::new(),
        // (The scripts hear it was picked up; they give what they give.)
        PickupKind::Mission => String::new(),
        PickupKind::Ammo => format!("+{} rounds", give_ammo(inv, 1.0)),
        PickupKind::Explosives => {
            inv.grenades = (inv.grenades + 1).min(grenade_cap);
            inv.molotovs = (inv.molotovs + 1).min(crate::weapons::MOLOTOV_CAP);
            inv.add_ammo(WeaponClass::Launcher, 1);
            inv.add_ammo(WeaponClass::GrenadeLauncher, 2);
            "+1 grenade, +1 molotov, +1 rocket".into()
        }
        PickupKind::Grenade => {
            inv.grenades = (inv.grenades + 1).min(grenade_cap);
            "+1 grenade".into()
        }
        PickupKind::Health => {
            stats.medkits = (stats.medkits + 1).min(medkit_cap);
            "+1 medkit".into()
        }
        PickupKind::Molotov => {
            inv.molotovs = (inv.molotovs + 1).min(crate::weapons::MOLOTOV_CAP);
            "+1 molotov".into()
        }
        PickupKind::Relic => {
            progress.relics += 1;
            progress.add_money(100);
            format!("Relic {} / {RELICS}   +$100", progress.relics)
        }
        PickupKind::Letter => {
            progress.letters += 1;
            progress.add_money(150);
            format!("Lost letter {} / {LETTERS}   +$150", progress.letters)
        }
        PickupKind::MemoryCard => {
            progress.cards += 1;
            progress.add_money(250);
            format!("Memory card {} / {MEMORY_CARDS}   +$250", progress.cards)
        }
        PickupKind::Weapon(name) => match grant_weapon(inv, name, perks.weapon_slots) {
            crate::weapons::Granted::New => format!("New weapon: {name}"),
            crate::weapons::Granted::Swapped(old) => format!("Swapped the {old} for the {name}"),
            crate::weapons::Granted::Ammo => format!("{name} ammo"),
        },
        PickupKind::Loot => {
            let r = rng.f32();
            if r < 0.5 {
                let cash = 15 + (rng.f32() * 60.0) as u32;
                let got = progress.add_money(cash);
                if got == 0 {
                    "Loot: wallet full".into()
                } else {
                    format!("Loot: +${got}")
                }
            } else if r < 0.7 {
                format!("Loot: +{} rounds", give_ammo(inv, 0.5))
            } else if r < 0.85 {
                stats.medkits = (stats.medkits + 1).min(medkit_cap);
                "Loot: +1 medkit".into()
            } else if r < 0.92 {
                inv.grenades = (inv.grenades + 1).min(grenade_cap);
                "Loot: +1 grenade".into()
            } else if r < 0.97 {
                inv.molotovs = (inv.molotovs + 1).min(crate::weapons::MOLOTOV_CAP);
                "Loot: +1 molotov".into()
            } else {
                "Loot: nothing useful".into()
            }
        }
        PickupKind::Plant(plant) => {
            let _ = &world;
            // (Horticulture 101: two of each plant picked.)
            let n = if perks.double_plants { 2 } else { 1 };
            progress.plants[plant.index()] += n;
            crate::statistics::add(progress, crate::statistics::stat::PLANTS_HARVESTED, n);
            crate::statistics::add(progress, crate::statistics::plant_count(plant), n);
            let have = progress.plants[plant.index()];
            format!("{} ({have}) - craft in the journal", plant.name())
        }
    }
}

/// An explosive barrel goes up: gone for good, its broken shell left for
/// now where it has one.
#[allow(clippy::too_many_arguments)]
fn detonate_explosives(
    mut died: MessageReader<Died>,
    mut world: ResMut<WorldState>,
    mut explosions: MessageWriter<Explosion>,
    mut commands: Commands,
    q: Query<(&Explosive, &GlobalTransform, Option<&crate::objects::WorldObject>)>,
    models: Res<Assets<crate::cache_assets::Fc3Model>>,
    mut breaks: ResMut<crate::breakables::Breaks>,
) {
    for d in died.read() {
        let Ok((ex, gt, obj)) = q.get(d.entity) else { continue };
        world.removed.insert(ex.id);
        explosions.write(Explosion { at: gt.translation() + Vec3::Y * 0.5, radius: 9.0, damage: 220.0, instigator: d.killer, effect: Some(ex.effect) });
        if obj.and_then(|o| models.get(&o.model)).is_some_and(crate::breakables::has_broken_state) {
            breaks.break_now(ex.id);
            commands.entity(d.entity).remove::<(Explosive, crate::combat::Health)>();
        } else {
            commands.entity(d.entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::{Action, ActionState};
    use crate::progression::{ItemLooted, LootKind, Progress};
    use crate::syringes::Plant;

    #[test]
    fn plant_interaction_respects_the_mission_lock_and_updates_inventory_and_loot_once() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ActionState>().init_resource::<crate::settings::Settings>()
            .init_resource::<WorldState>().init_resource::<HudState>().init_resource::<Progress>()
            .init_resource::<crate::progression::Perks>().init_resource::<crate::fp::HandsBusy>()
            .init_resource::<crate::story::ScriptedPlayer>().insert_resource(Rng::seeded(1))
            .add_message::<ItemLooted>().add_message::<crate::fp::FpAction>()
            .add_message::<crate::progression::XpGain>().add_message::<crate::sound::PlaySfx>()
            .add_message::<PickedUp>()
            .add_systems(Update, interact);
        let defs = crate::weapons::weapon_defs();
        let unarmed = defs.iter().position(|d| d.name == crate::weapons::UNARMED).unwrap();
        let inv = Inventory {
            defs, models: Vec::new(), slots: vec![crate::weapons::WeaponSlot { def: unarmed, mag: 0, reserve: 0 }], current: 0,
            grenades: 0, molotovs: 0, c4: 0, mines: 0,
            cooldown: 0.0, reloading: 0.0, switching: 0.0, aiming: false,
            aim_blend: 0.0, recoil_kick: 0.0, spread_bloom: 0.0, shots_fired: 0,
            recoil_offset: 0.0, since_shot: 0.0, reserve_scale: 1.0,
            attachments: Vec::new(), attachment_models: Vec::new(), mounted_from: None,
            auto_tail: 0, arrow_kind: 0, special_arrows: [0; 2], paints: Vec::new(), paint_options: Vec::new(),
        };
        let player = app
            .world_mut()
            .spawn((
                Player {
                    velocity: Vec3::ZERO,
                    yaw: 0.0,
                    pitch: 0.0,
                    grounded: true,
                    crouching: false,
                    swimming: false,
                    noclip: false,
                    spawning: false,
                    keep_height: false,
                    last_landing_speed: 0.0,
                    sprinting: false,
                    spawned_at: 0.0,
                    driving: false,
                    armed_passenger: false,
                    eye: crate::player::EYE_HEIGHT,
                    air: crate::player::AirMode::None,
                    mantle: None,
                    mounted: None,
                    sliding: None,
                    dragging: false,
                },
                Transform::from_xyz(0.0, crate::player::HALF_HEIGHT, 0.0),
                inv,
                Health::new(100.0),
                PlayerStats {
                    medkits: 0,
                    kills: 0,
                    deaths: 0,
                    respawn_timer: 0.0,
                    spawn_point: Vec3::ZERO,
                    hurt_dir: None,
                    burning: 0.0,
                    last_hurt: crate::combat::DamageKind::Bullet,
                },
            ))
            .id();
        let pickup =
            app.world_mut().spawn((Pickup { kind: PickupKind::Plant(Plant::ScrewPine), id: 42, visual: None }, GlobalTransform::from_xyz(0.0, 0.0, -1.0))).id();
        let mut cursor = bevy::ecs::message::MessageCursor::<ItemLooted>::default();
        app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
        app.world_mut().resource_mut::<crate::story::ScriptedPlayer>().plant_looting = false;
        app.update();
        assert_eq!(app.world().resource::<Progress>().plants[Plant::ScrewPine.index()], 0);
        assert!(app.world().get::<Pickup>(pickup).is_some());
        app.world_mut().resource_mut::<crate::story::ScriptedPlayer>().plant_looting = true;
        app.world_mut().get_mut::<Health>(player).unwrap().dead = true;
        app.update();
        assert_eq!(cursor.read(app.world().resource::<Messages<ItemLooted>>()).count(), 0);
        app.world_mut().get_mut::<Health>(player).unwrap().dead = false;
        app.update();
        assert_eq!(app.world().resource::<Progress>().plants[Plant::ScrewPine.index()], 1);
        assert!(app.world().get::<Pickup>(pickup).is_none());
        assert!(app.world().resource::<WorldState>().removed.contains(&42));
        assert!(!app.world().resource::<ActionState>().just_pressed(Action::Interact));
        assert_eq!(cursor.read(app.world().resource::<Messages<ItemLooted>>()).map(|e| e.0).collect::<Vec<_>>(), vec![LootKind::Plant(Plant::ScrewPine)]);
        app.update();
        assert_eq!(app.world().resource::<Progress>().plants[Plant::ScrewPine.index()], 1);
        assert_eq!(cursor.read(app.world().resource::<Messages<ItemLooted>>()).count(), 0);
        // Gather the tutorial's two green leaves even with room for medkits.
        // Neither a reward nor its inventory snapshot may auto-consume them.
        for id in [43, 44] {
            app.world_mut().spawn((Pickup { kind: PickupKind::Plant(Plant::Arrowroot), id, visual: None }, GlobalTransform::from_xyz(0.0, 0.0, -1.0)));
            app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
            app.update();
        }
        assert_eq!(app.world().resource::<Progress>().plants[Plant::Arrowroot.index()], 2);
        assert_eq!(app.world().get::<PlayerStats>(player).unwrap().medkits, 0);
        assert_eq!(cursor.read(app.world().resource::<Messages<ItemLooted>>()).count(), 2);
    }
}
