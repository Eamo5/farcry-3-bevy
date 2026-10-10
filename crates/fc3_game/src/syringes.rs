//! Plants and syringes, and holding your breath.
//!
//! Gathered plants stay in the satchel (`Progress::plants`) until explicitly
//! consumed by crafting. Syringes are the game's potion recipes (crafted in
//! the journal, `crafting`), kept by recipe in `Progress::potions`; what each
//! does is by recipe (`Syringe::of`) and how long it lasts is the recipe's.
//! Y cycles the syringe on hand; holding it injects.
//!
//! Underwater the player holds their breath; when it runs out they drown.

use bevy::prelude::*;

use crate::combat::{DamageEvent, DamageKind, Health};
use crate::player::{Player, PlayerCamera};
use crate::progression::Progress;

pub struct SyringesPlugin;

impl Plugin for SyringesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Boosts>().init_resource::<Breath>().add_systems(
            Update,
            (tick_boosts, breath.before(crate::hud::HudSet), syringe_key.before(crate::hud::HudSet), hunter_marks)
                .chain()
                .run_if(in_state(crate::menu::GameState::Playing)),
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Plant {
    Arrowroot,
    TorchGinger,
    StarRhododendron,
    YellowSage,
    StoutKelp,
    ScrewPine,
    GoblinsGold,
    BreezeWakame,
    Aloe,
    GoatsFoot,
    GoldenBeehive,
    GoodLuckPlant,
    Heliconia,
    Hibiscus,
    Palani,
    RedAlgae,
    SacredLotus,
    Tiare,
}

impl Plant {
    // Append new species: the first eight slots retain old save-file identities.
    pub const ALL: [Plant; 18] = [
        Plant::Arrowroot,
        Plant::TorchGinger,
        Plant::StarRhododendron,
        Plant::YellowSage,
        Plant::StoutKelp,
        Plant::ScrewPine,
        Plant::GoblinsGold,
        Plant::BreezeWakame,
        Plant::Aloe,
        Plant::GoatsFoot,
        Plant::GoldenBeehive,
        Plant::GoodLuckPlant,
        Plant::Heliconia,
        Plant::Hibiscus,
        Plant::Palani,
        Plant::RedAlgae,
        Plant::SacredLotus,
        Plant::Tiare,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Plant::Arrowroot => "Arrowroot",
            Plant::TorchGinger => "Torch Ginger",
            Plant::StarRhododendron => "Star Rhododendron",
            Plant::YellowSage => "Yellow Sage",
            Plant::StoutKelp => "Stout Kelp",
            Plant::ScrewPine => "Screw Pine",
            Plant::GoblinsGold => "Goblin's Gold",
            Plant::BreezeWakame => "Breeze Wakame",
            Plant::Aloe => "Aloe",
            Plant::GoatsFoot => "Goat's Foot",
            Plant::GoldenBeehive => "Golden Beehive",
            Plant::GoodLuckPlant => "Good Luck Plant",
            Plant::Heliconia => "Heliconia",
            Plant::Hibiscus => "Hibiscus",
            Plant::Palani => "Palani",
            Plant::RedAlgae => "Red Algae",
            Plant::SacredLotus => "Sacred Lotus",
            Plant::Tiare => "Tiare",
        }
    }

    /// Placed source family, not a guessed leaf or a substring in a prop name.
    /// Unknown pickups must not silently award green leaves.
    pub fn from_name(name: &str) -> Option<Plant> {
        let n = name.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
        Plant::ALL.into_iter().find(|p| {
            let family = p.name().to_ascii_lowercase().replace(['\'', ' '], "");
            n.strip_prefix(&family).is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(['_', '.']))
        })
    }

    pub fn index(self) -> usize {
        Plant::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }
}

/// What a syringe does, by the recipe it is made from.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Syringe {
    /// Animals nearby are marked.
    WildlifeVision,
    /// Enemies nearby are marked.
    EnemyVision,
    /// Plants nearby are marked.
    PlantVision,
    /// Fire can't hurt.
    Fireproof,
    /// Long breath underwater.
    NeverDrown,
    /// Faster sprint and strokes.
    SprintBurst,
    /// Animals hurt less.
    AnimalShield,
    /// Nothing hurts.
    Invincible,
    /// Animals leave the player be.
    AnimalRepellent,
    /// Every shot or blow the player lands kills.
    OneHitKill,
    /// Shots go where they are aimed.
    Steady,
    /// Extra health slots and a full heal (the temporary health boost).
    HealthBoost,
}

impl Syringe {
    /// The effect of a potion recipe (its id's name part), for the ones the
    /// game plays.
    pub fn of(recipe: &str) -> Option<Syringe> {
        let name = recipe.rsplit('/').next()?.to_ascii_lowercase();
        Some(match name.as_str() {
            "wildlife vision" => Syringe::WildlifeVision,
            "enemy vision" => Syringe::EnemyVision,
            "plantvision" => Syringe::PlantVision,
            "fireproof" => Syringe::Fireproof,
            "never drown" => Syringe::NeverDrown,
            "super sprint burst" => Syringe::SprintBurst,
            "animaldamage" => Syringe::AnimalShield,
            "invincible" => Syringe::Invincible,
            "animal repellant" => Syringe::AnimalRepellent,
            "onehitkill" => Syringe::OneHitKill,
            "super bullet spread reduction" => Syringe::Steady,
            "temphealthboost" => Syringe::HealthBoost,
            _ => return None,
        })
    }
}

/// Seconds left on each syringe effect running.
#[derive(Resource, Default)]
pub struct Boosts {
    pub left: std::collections::HashMap<Syringe, f32>,
    /// How much longer the player's syringes last (the potion duration bonus; 0 is
    /// taken as none).
    pub extend: f32,
    /// Health the boost syringe adds over the player's own (lost as it is
    /// hurt; it doesn't come back), and a full heal it owes.
    pub extra_health: f32,
    pub full_heal: bool,
}

impl Boosts {
    pub fn active(&self, s: Syringe) -> bool {
        self.left.get(&s).is_some_and(|t| *t > 0.0)
    }
}

/// Injects one of the syringes made from `recipe`, for as long as the recipe
/// says (and the skills stretch).
pub fn inject(p: &mut Progress, boosts: &mut Boosts, recipe: &fc3_formats::progression::Recipe) -> bool {
    let Some(effect) = Syringe::of(&recipe.id) else { return false };
    let Some(n) = p.potions.get_mut(&recipe.id).filter(|n| **n > 0) else { return false };
    *n -= 1;
    if effect == Syringe::HealthBoost {
        boosts.extra_health = recipe.health_slots as f32 * crate::combat::HEALTH_SEGMENT;
        boosts.full_heal = true;
        return true;
    }
    let seconds = if recipe.duration > 0.0 { recipe.duration } else { 40.0 };
    boosts.left.insert(effect, seconds * if boosts.extend > 0.0 { boosts.extend } else { 1.0 });
    true
}

fn tick_boosts(time: Res<Time>, perks: Option<Res<crate::progression::Perks>>, mut boosts: ResMut<Boosts>) {
    if let Some(p) = perks.filter(|p| p.is_changed()) {
        boosts.extend = p.potion_duration;
    }
    for t in boosts.left.values_mut() {
        *t = (*t - time.delta_secs()).max(0.0);
    }
}

/// Y: tap to pick the syringe on hand, hold to inject it. Also shows what's
/// running and what's on hand.
#[allow(clippy::too_many_arguments)]
fn syringe_key(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut held: Local<f32>,
    mut selected: Local<Option<String>>,
    mut progress: ResMut<Progress>,
    mut boosts: ResMut<Boosts>,
    mut hud: ResMut<crate::hud::HudState>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    crafting: Res<crate::crafting::CraftCatalog>,
) {
    // The syringes on hand, in the crafting menu's order.
    let owned: Vec<&fc3_formats::progression::Recipe> =
        crafting.recipes.iter().filter(|r| Syringe::of(&r.id).is_some() && progress.potions.get(&r.id).is_some_and(|n| *n > 0)).collect();
    if !owned.iter().any(|r| Some(&r.id) == selected.as_ref()) {
        *selected = owned.first().map(|r| r.id.clone());
    }
    if keys.pressed(KeyCode::KeyY) {
        *held += time.delta_secs();
        if *held >= 0.35 && *held - time.delta_secs() < 0.35 {
            match owned.iter().find(|r| Some(&r.id) == selected.as_ref()) {
                Some(recipe) if inject(&mut progress, &mut boosts, recipe) => {
                    hud.notify(format!("{}: {}", recipe.name, recipe.description));
                    sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.6 });
                }
                _ => hud.notify("No syringes - craft them from plants in the journal"),
            }
        }
    } else {
        // A short tap cycles through the syringes on hand.
        if *held > 0.0 && *held < 0.35 && owned.len() > 1 {
            let at = owned.iter().position(|r| Some(&r.id) == selected.as_ref()).unwrap_or(0);
            *selected = Some(owned[(at + 1) % owned.len()].id.clone());
        }
        *held = 0.0;
    }
    // Status: effects running, then the syringe on hand.
    let running: Vec<String> = crafting
        .recipes
        .iter()
        .filter_map(|r| Syringe::of(&r.id).map(|s| (r, s)))
        .filter(|(_, s)| boosts.active(*s))
        .map(|(r, s)| format!("{} {:.0}s", r.name, boosts.left[&s]))
        .collect();
    let on_hand = owned
        .iter()
        .find(|r| Some(&r.id) == selected.as_ref())
        .map(|r| format!("{} x{} (hold Y)", r.name, progress.potions.get(&r.id).copied().unwrap_or(0)));
    let line = running.into_iter().chain(on_hand).collect::<Vec<_>>().join("    ");
    if !line.is_empty() && hud.status.is_none() {
        hud.status = Some(line);
    }
}

/// Seconds of air underwater, and how long until drowning hurts again.
#[derive(Resource)]
pub struct Breath {
    pub left: f32,
    pub max: f32,
    hurt: f32,
}

impl Default for Breath {
    fn default() -> Self {
        Breath { left: AIR, max: AIR, hurt: 0.0 }
    }
}

/// Seconds a breath lasts (the diver's syringe multiplies it).
const AIR: f32 = 30.0;

fn breath(
    time: Res<Time>,
    boosts: Res<Boosts>,
    perks: Option<Res<crate::progression::Perks>>,
    mut air: ResMut<Breath>,
    players: Query<(Entity, &Player, &Health)>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    mut damage: MessageWriter<DamageEvent>,
    mut hud: ResMut<crate::hud::HudState>,
    heights: Res<crate::terrain::Heightfield>,
) {
    let dt = time.delta_secs();
    let Ok((pe, p, h)) = players.single() else { return };
    let Ok(cam) = camera.single() else { return };
    // (`SP_HoldBreathUnderwater`: longer; the diver's syringe longer still.)
    let lungs = perks.as_ref().map_or(1.0, |p| p.breath);
    air.max = AIR * lungs * if boosts.active(Syringe::NeverDrown) { 4.0 } else { 1.0 };
    let under = p.swimming && crate::water::submerged(cam.translation(), &heights, 0.15) && !h.dead;
    if !under {
        air.left = (air.left + dt * 8.0).min(air.max);
        air.hurt = 0.0;
        return;
    }
    air.left = (air.left - dt).max(0.0);
    // Air left, as a bar on the status line.
    let bars = ((air.left / air.max) * 20.0).ceil() as usize;
    hud.status = Some(format!("AIR  {}{}", "|".repeat(bars), ".".repeat(20 - bars.min(20))));
    if air.left <= 0.0 {
        air.hurt -= dt;
        if air.hurt <= 0.0 {
            air.hurt = 1.0;
            damage.write(DamageEvent { target: pe, amount: 12.0, kind: DamageKind::Drown, source: None, instigator: None, headshot: false });
        }
    }
}

#[derive(Component)]
struct HunterMark;

/// The vision syringes: a marker over every animal (wildlife), enemy or plant
/// within 150 m.
#[allow(clippy::type_complexity)]
fn hunter_marks(
    boosts: Res<Boosts>,
    cameras: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    animals: Query<(&GlobalTransform, &Health), With<crate::wildlife::WildAnimal>>,
    enemies: Query<(&GlobalTransform, &Health, &crate::npc::Npc)>,
    plants: Query<(&GlobalTransform, &crate::pickups::Pickup)>,
    marks: Query<Entity, With<HunterMark>>,
    mut commands: Commands,
    scale: Res<UiScale>,
) {
    for e in &marks {
        commands.entity(e).despawn();
    }
    let Ok((cam, cam_t)) = cameras.single() else { return };
    let mut spots: Vec<(Vec3, Color)> = Vec::new();
    if boosts.active(Syringe::WildlifeVision) {
        spots.extend(animals.iter().filter(|(_, h)| !h.dead).map(|(gt, _)| (gt.translation() + Vec3::Y * 1.2, Color::srgba(0.95, 0.75, 0.25, 0.85))));
    }
    if boosts.active(Syringe::EnemyVision) {
        spots.extend(
            enemies.iter().filter(|(_, h, n)| !h.dead && n.faction.hostile_to_player()).map(|(gt, _, _)| (gt.translation() + Vec3::Y * 1.0, Color::srgba(0.95, 0.25, 0.2, 0.85))),
        );
    }
    if boosts.active(Syringe::PlantVision) {
        spots.extend(
            plants
                .iter()
                .filter(|(_, p)| matches!(p.kind, crate::pickups::PickupKind::Plant(_)))
                .map(|(gt, _)| (gt.translation() + Vec3::Y * 0.6, Color::srgba(0.4, 0.95, 0.35, 0.85))),
        );
    }
    for (at, color) in spots {
        if cam_t.translation().distance(at) > 150.0 {
            continue;
        }
        let Ok(px) = cam.world_to_viewport(cam_t, at) else { continue };
        let px = crate::ui_nav::viewport_to_ui(px, scale.0);
        commands.spawn((
            Node { position_type: PositionType::Absolute, left: Val::Px(px.x - 6.0), top: Val::Px(px.y - 6.0), width: Val::Px(12.0), height: Val::Px(12.0), ..default() },
            BackgroundColor(color),
            UiTransform::from_rotation(Rot2::degrees(45.0)),
            HunterMark,
            crate::hud::HudElement,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plants_from_pickup_names() {
        assert_eq!(Plant::from_name("Pickups/Plants/TorchGinger_12"), Some(Plant::TorchGinger));
        assert_eq!(Plant::from_name("Pickups/Plants/GoblinsGold"), Some(Plant::GoblinsGold));
        assert_eq!(Plant::from_name("Pickups/Plants/StarRhododendron_3"), Some(Plant::StarRhododendron));
        assert_eq!(Plant::from_name("Pickups/Collectibles/GoodLuckPlant"), Some(Plant::GoodLuckPlant));
        assert_eq!(Plant::from_name("Pickups/Plants/SacredLotus_01_JAS_080"), Some(Plant::SacredLotus));
        for unknown in ["UnknownPlant_01", "FakeArrowroot_1", "ArrowrootDecoration", "TorchGingerPainting"] {
            assert_eq!(Plant::from_name(unknown), None, "{unknown}");
        }
        for plant in Plant::ALL {
            let name = format!("Pickups/Plants/{}_01", plant.name().replace(['\'', ' '], ""));
            assert_eq!(Plant::from_name(&name), Some(plant));
        }
    }

    #[test]
    fn syringes_by_recipe_and_their_durations() {
        use fc3_formats::progression::Recipe;
        assert_eq!(Syringe::of("FC3_Recipes:Potions/Wildlife vision"), Some(Syringe::WildlifeVision));
        assert_eq!(Syringe::of("FC3_Recipes:Potions/Med kit small"), None);
        let recipe = Recipe { id: "FC3_Recipes:Potions/Fireproof".into(), kind: 1, duration: 20.0, ..Default::default() };
        let mut p = Progress::default();
        let mut b = Boosts { extend: 1.5, ..Default::default() };
        assert!(!inject(&mut p, &mut b, &recipe));
        p.potions.insert(recipe.id.clone(), 1);
        assert!(inject(&mut p, &mut b, &recipe));
        assert!(b.active(Syringe::Fireproof) && (b.left[&Syringe::Fireproof] - 30.0).abs() < 1e-4);
        assert!(!inject(&mut p, &mut b, &recipe));
    }
}
