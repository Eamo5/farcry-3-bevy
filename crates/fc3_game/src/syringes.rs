//! Plants and syringes, and holding your breath.
//!
//! Gathered plants stay in the satchel (`Progress::plants`) until explicitly
//! consumed by crafting; syringes are mixed in the journal
//! (or straight from the satchel): Torch Ginger for the hunter's syringe that
//! marks nearby animals, Star Rhododendron for the fighter's that halves
//! damage taken, Yellow Sage for the explorer's faster sprint, Stout Kelp for
//! the diver's long breath and quicker strokes, and Screw Pine with Torch
//! Ginger to shrug off fire. Y cycles the syringe on hand; holding it injects.
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Syringe {
    Hunter,
    Fighter,
    Explorer,
    Diver,
    Fireproof,
}

impl Syringe {
    pub const ALL: [Syringe; 5] = [Syringe::Hunter, Syringe::Fighter, Syringe::Explorer, Syringe::Diver, Syringe::Fireproof];

    pub fn name(self) -> &'static str {
        match self {
            Syringe::Hunter => "Hunter",
            Syringe::Fighter => "Fighter",
            Syringe::Explorer => "Explorer",
            Syringe::Diver => "Diver",
            Syringe::Fireproof => "Fireproof",
        }
    }

    pub fn effect(self) -> &'static str {
        match self {
            Syringe::Hunter => "Nearby animals are marked",
            Syringe::Fighter => "Take half damage",
            Syringe::Explorer => "Sprint 30% faster",
            Syringe::Diver => "Breathe 4x longer underwater, swim faster",
            Syringe::Fireproof => "Fire can't hurt you",
        }
    }

    /// Plants it takes, with the rare plants able to stand in for two of one.
    pub fn recipe(self) -> &'static [(Plant, u32)] {
        match self {
            Syringe::Hunter => &[(Plant::TorchGinger, 2)],
            Syringe::Fighter => &[(Plant::StarRhododendron, 2)],
            Syringe::Explorer => &[(Plant::YellowSage, 2)],
            Syringe::Diver => &[(Plant::StoutKelp, 2)],
            Syringe::Fireproof => &[(Plant::ScrewPine, 1), (Plant::TorchGinger, 1)],
        }
    }

    /// Seconds it lasts.
    pub fn duration(self) -> f32 {
        match self {
            Syringe::Fighter => 45.0,
            Syringe::Diver => 120.0,
            _ => 60.0,
        }
    }

    pub fn index(self) -> usize {
        Syringe::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// Seconds left on each syringe's effect.
#[derive(Resource, Default)]
pub struct Boosts {
    pub left: [f32; 5],
}

impl Boosts {
    pub fn active(&self, s: Syringe) -> bool {
        self.left[s.index()] > 0.0
    }
}

/// Whether the satchel holds a syringe's plants (a rare plant counts as two
/// of any one ingredient).
pub fn can_craft(p: &Progress, s: Syringe) -> bool {
    let rare = p.plants[Plant::GoblinsGold.index()] + p.plants[Plant::BreezeWakame.index()];
    let short: u32 = s.recipe().iter().map(|(plant, n)| n.saturating_sub(p.plants[plant.index()])).sum();
    short == 0 || (rare > 0 && short <= 2)
}

pub fn craft(p: &mut Progress, s: Syringe) -> bool {
    if !can_craft(p, s) {
        return false;
    }
    let mut rare_used = false;
    for (plant, n) in s.recipe() {
        let have = &mut p.plants[plant.index()];
        if *have >= *n {
            *have -= n;
        } else {
            *have = 0;
            rare_used = true;
        }
    }
    if rare_used {
        let i = if p.plants[Plant::GoblinsGold.index()] > 0 { Plant::GoblinsGold.index() } else { Plant::BreezeWakame.index() };
        p.plants[i] -= 1;
    }
    p.syringes[s.index()] += 1;
    true
}

pub fn inject(p: &mut Progress, boosts: &mut Boosts, s: Syringe) -> bool {
    if p.syringes[s.index()] == 0 {
        return false;
    }
    p.syringes[s.index()] -= 1;
    boosts.left[s.index()] = s.duration();
    true
}

fn tick_boosts(time: Res<Time>, mut boosts: ResMut<Boosts>) {
    for t in &mut boosts.left {
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
    mut selected: Local<usize>,
    mut progress: ResMut<Progress>,
    mut boosts: ResMut<Boosts>,
    mut hud: ResMut<crate::hud::HudState>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let owned: Vec<Syringe> = Syringe::ALL.into_iter().filter(|s| progress.syringes[s.index()] > 0).collect();
    if !owned.iter().any(|s| s.index() == *selected) {
        *selected = owned.first().map(|s| s.index()).unwrap_or(0);
    }
    if keys.pressed(KeyCode::KeyY) {
        *held += time.delta_secs();
        if *held >= 0.35 && *held - time.delta_secs() < 0.35 {
            let s = Syringe::ALL[*selected];
            if inject(&mut progress, &mut boosts, s) {
                hud.notify(format!("{} syringe: {}", s.name(), s.effect()));
                sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.6 });
            } else {
                hud.notify("No syringes - craft them from plants in the journal");
            }
        }
    } else {
        // A short tap cycles through the syringes on hand.
        if *held > 0.0 && *held < 0.35 && owned.len() > 1 {
            let at = owned.iter().position(|s| s.index() == *selected).unwrap_or(0);
            *selected = owned[(at + 1) % owned.len()].index();
        }
        *held = 0.0;
    }
    // Status: effects running, then the syringe on hand.
    let running: Vec<String> = Syringe::ALL.iter().filter(|s| boosts.active(**s)).map(|s| format!("{} {:.0}s", s.name(), boosts.left[s.index()])).collect();
    let on_hand = owned.iter().find(|s| s.index() == *selected).map(|s| format!("{} syringe x{} (hold Y)", s.name(), progress.syringes[s.index()]));
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
    air.max = if boosts.active(Syringe::Diver) { AIR * 4.0 } else { AIR };
    let under = p.swimming && cam.translation().y < crate::water::surface_at(cam.translation(), &heights) - 0.15 && !h.dead;
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

/// The hunter's syringe: a marker over every animal within 150 m.
fn hunter_marks(
    boosts: Res<Boosts>,
    cameras: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    animals: Query<(&GlobalTransform, &Health), With<crate::wildlife::WildAnimal>>,
    marks: Query<Entity, With<HunterMark>>,
    mut commands: Commands,
) {
    for e in &marks {
        commands.entity(e).despawn();
    }
    if !boosts.active(Syringe::Hunter) {
        return;
    }
    let Ok((cam, cam_t)) = cameras.single() else { return };
    for (gt, h) in &animals {
        let at = gt.translation() + Vec3::Y * 1.2;
        if h.dead || cam_t.translation().distance(at) > 150.0 {
            continue;
        }
        let Ok(px) = cam.world_to_viewport(cam_t, at) else { continue };
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(px.x - 6.0),
                top: Val::Px(px.y - 6.0),
                width: Val::Px(12.0),
                height: Val::Px(12.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.95, 0.75, 0.25, 0.85)),
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
    fn crafting_uses_plants_and_rare_ones() {
        let mut p = Progress::default();
        assert!(!craft(&mut p, Syringe::Hunter));
        p.plants[Plant::TorchGinger.index()] = 2;
        assert!(craft(&mut p, Syringe::Hunter));
        assert_eq!((p.plants[Plant::TorchGinger.index()], p.syringes[Syringe::Hunter.index()]), (0, 1));
        // A rare plant stands in for a missing pair.
        p.plants[Plant::GoblinsGold.index()] = 1;
        assert!(craft(&mut p, Syringe::Fighter));
        assert_eq!(p.plants[Plant::GoblinsGold.index()], 0);
        let mut b = Boosts::default();
        assert!(inject(&mut p, &mut b, Syringe::Fighter));
        assert!(b.active(Syringe::Fighter) && !inject(&mut p, &mut b, Syringe::Fighter));
    }
}
