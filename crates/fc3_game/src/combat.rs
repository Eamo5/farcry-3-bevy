//! Health, damage and death for the player and any damageable entity.

use bevy::prelude::*;

use crate::player::Player;

pub struct CombatPlugin;

impl Plugin for CombatPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<DamageEvent>()
            .add_message::<Died>()
            .init_resource::<LastHarm>()
            .init_resource::<DamageTable>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_damage_table)
            .add_systems(Startup, add_player_health.after(crate::player::PlayerSpawn))
            .add_systems(Update, (attach_counters, apply_damage, player_regen, fall_damage, use_medkit, respawn_player).chain())
            .add_systems(Update, (die_on_spawn, test_die).before(apply_damage));
    }
}

/// Hit points. The player's health regenerates up to the top of its current
/// segment after a short delay; medkits restore whole segments.
#[derive(Component, Clone)]
pub struct Health {
    pub current: f32,
    pub max: f32,
    pub since_damage: f32,
    pub dead: bool,
}

impl Health {
    pub fn new(max: f32) -> Self {
        Health { current: max, max, since_damage: 999.0, dead: false }
    }

    pub fn fraction(&self) -> f32 {
        (self.current / self.max).clamp(0.0, 1.0)
    }
}

/// Body armour: multiplies bullet damage that isn't a headshot.
#[derive(Component, Clone, Copy)]
pub struct Armour(pub f32);

/// How much bullet and explosion damage a target takes (vehicle hulls shrug
/// off rounds but not rockets).
#[derive(Component, Clone, Copy)]
pub struct DamageScale {
    pub bullet: f32,
    pub explosion: f32,
}

pub const PLAYER_MAX_HEALTH: f32 = 100.0;
pub const HEALTH_SEGMENT: f32 = 25.0;
const REGEN_DELAY: f32 = 4.0;
const REGEN_RATE: f32 = 6.0;
const MEDKIT_HEAL: f32 = 50.0;
/// How long the heal input is held to patch up without a medkit.
const MANUAL_HEAL_HOLD: f32 = 0.5;
/// Seconds from the player's death to the respawn.
pub const RESPAWN_SECS: f32 = 5.0;

/// Player-only stats.
#[derive(Component)]
pub struct PlayerStats {
    pub medkits: u32,
    pub kills: u32,
    pub deaths: u32,
    /// Seconds remaining on the death screen before respawning.
    pub respawn_timer: f32,
    pub spawn_point: Vec3,
    /// Recent damage direction (world space) and strength, for the HUD indicator.
    pub hurt_dir: Option<(Vec3, f32)>,
    /// Seconds of burning left on screen after fire damage.
    pub burning: f32,
    /// What last hurt (it picks the field surgery when out of medkits).
    pub last_hurt: DamageKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DamageKind {
    Bullet,
    Explosion,
    Melee,
    Fall,
    /// Out of breath underwater.
    Drown,
    Fire,
}

impl DamageKind {
    /// How the game names a harm's source to scripts (`HealthListener`'s
    /// `sourceType`).
    pub fn source_type(self) -> &'static str {
        match self {
            DamageKind::Bullet => "BulletImpact",
            DamageKind::Explosion => "explosion",
            DamageKind::Melee => "WeaponMeleeAttack",
            DamageKind::Fire => "FirePropagation",
            DamageKind::Fall | DamageKind::Drown => "Generic",
        }
    }
}

/// The last harm each thing took, for the story's health counter (what hurt
/// it: a hunt takes its kill only with the weapon it asks for).
#[derive(Resource, Default)]
pub struct LastHarm(pub std::collections::HashMap<Entity, (DamageKind, Hit)>);

#[derive(Message, Clone, Copy)]
pub struct DamageEvent {
    pub target: Entity,
    pub amount: f32,
    pub kind: DamageKind,
    /// Where the damage came from (for hit indicators and ragdoll impulses).
    pub source: Option<Vec3>,
    pub instigator: Option<Entity>,
    pub headshot: bool,
    /// The body part a round struck, and what kind of round.
    pub hit: Hit,
}

/// A body part.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    Head,
    Torso,
    Legs,
}

/// Where a round struck (characters take bullets and arrows differently,
/// and each body part only so much).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Hit {
    /// No one part: blasts, fire, blows, falls (and shots not traced to one).
    #[default]
    Whole,
    Bullet(Zone),
    Arrow(Zone),
}

/// A head shot's multiplier on targets without the game's counters.
const HEADSHOT: f32 = 2.5;

/// The game's health table and each enemy archetype's and animal's counters
/// (`fc3_formats::counters`), once read from the cache.
#[derive(Resource, Default)]
pub struct DamageTable(pub Option<fc3_formats::counters::Damage>);

impl DamageTable {
    /// An archetype's counters ("Pirate_HeavyGunner"), and its health.
    pub fn archetype(&self, name: &str) -> Option<(Counters, f32)> {
        let d = self.0.as_ref()?;
        Some(self.counters(d.get(name)?.clone()))
    }

    /// Counters from an archetype's (or a placed character's own) values,
    /// and its health: a character's the table's (or its override), each
    /// body part's share scaled with it; an animal its own, no parts.
    pub fn counters(&self, v: fc3_formats::counters::Vulnerability) -> (Counters, f32) {
        let t = self.0.as_ref().map(|d| d.table).unwrap_or_default();
        let animal = v.animal;
        let health = v.health.unwrap_or(t.global);
        let scale = health / t.global.max(1.0);
        let pools = (!animal).then(|| [t.head * scale, t.torso * scale, t.legs * scale]);
        (Counters { v, pools }, health)
    }
}

/// How a character or animal takes damage (the game's counters): its
/// multipliers, and what each body part can still lose.
#[derive(Component, Clone, Debug)]
pub struct Counters {
    pub v: fc3_formats::counters::Vulnerability,
    /// Head, torso and legs (characters; an animal has none, only its head's
    /// multiplier).
    pub pools: Option<[f32; 3]>,
}

impl Counters {
    /// What this takes of `amount` of `kind` landing as `hit`, and whether
    /// it kills outright (a body part with nothing left).
    pub fn take(&mut self, amount: f32, kind: DamageKind, hit: Hit) -> (f32, bool) {
        use fc3_formats::counters::Harm;
        let harm = match (kind, hit) {
            (_, Hit::Arrow(_)) => Some(Harm::Arrow),
            (DamageKind::Bullet, _) => Some(Harm::Pierce),
            (DamageKind::Explosion, _) => Some(Harm::Explosion),
            (DamageKind::Melee, _) => Some(Harm::Cut),
            (DamageKind::Fire, _) => Some(Harm::Burn),
            (DamageKind::Fall, _) => Some(Harm::Crush),
            (DamageKind::Drown, _) => None,
        };
        let mut amount = amount * self.v.body * harm.map_or(1.0, |h| self.v.harm(h));
        // A shot not traced to a part lands in the torso.
        let zone = match hit {
            Hit::Bullet(z) | Hit::Arrow(z) => Some(z),
            Hit::Whole if kind == DamageKind::Bullet => Some(Zone::Torso),
            Hit::Whole => None,
        };
        let Some(zone) = zone else { return (amount, false) };
        let i = zone as usize;
        match &mut self.pools {
            // A character: bullets are stopped by each part's armour, and a
            // part that has taken all it can is a kill.
            Some(pools) => {
                if matches!(hit, Hit::Bullet(_) | Hit::Whole) {
                    amount *= [self.v.head, self.v.torso, self.v.legs][i];
                }
                pools[i] -= amount;
                (amount, pools[i] <= 0.0)
            }
            // An animal: only its head multiplies.
            None => (if zone == Zone::Head { amount * self.v.head } else { amount }, false),
        }
    }
}

/// The game's counters (and health) on enemies and animals as they appear;
/// story characters bring their own.
#[allow(clippy::type_complexity)]
fn attach_counters(
    table: Res<DamageTable>,
    mut commands: Commands,
    mut npcs: Query<(Entity, &crate::npc::Npc, &mut Health), Without<Counters>>,
    mut animals: Query<(Entity, &crate::wildlife::WildAnimal, &mut Health), (Without<Counters>, Without<crate::npc::Npc>)>,
) {
    if table.0.is_none() {
        return;
    }
    let fit = |e: Entity, name: &str, h: &mut Health, commands: &mut Commands| {
        let Some((counters, health)) = table.archetype(name) else { return };
        if h.current >= h.max {
            h.current = health;
        }
        h.max = health;
        // (The counters' armour replaces the hand-set one.)
        commands.entity(e).insert(counters).remove::<Armour>();
    };
    for (e, npc, mut h) in &mut npcs {
        fit(e, npc.role.archetype(npc.faction), &mut h, &mut commands);
    }
    for (e, animal, mut h) in &mut animals {
        fit(e, animal.animal().archetype(), &mut h, &mut commands);
    }
}

fn load_damage_table(info: Option<Res<crate::data::CacheInfo>>, mut table: ResMut<DamageTable>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::counters::DAMAGE_REL))
        .map_err(anyhow::Error::from)
        .and_then(|t| fc3_formats::counters::from_text(&t))
    {
        Ok(d) => {
            info!("damage: {} archetypes' counters, characters' health {}", d.archetypes.len(), d.table.global);
            table.0 = Some(d);
        }
        Err(e) => warn!("damage: no counters table ({e:#}); re-run the converter"),
    }
}

#[derive(Message, Clone, Copy)]
pub struct Died {
    pub entity: Entity,
    pub killer: Option<Entity>,
    pub kind: DamageKind,
}

/// `FC3_PLAYER_DIE=<seconds>` (tests): the player dies once, that long into
/// play (the death and the respawn to be seen); `FC3_PLAYER_HURT=<seconds>,
/// <health>` leaves him that health then instead (and holds it there).
fn test_die(
    time: Res<Time>,
    state: Res<State<crate::menu::GameState>>,
    mut players: Query<(Entity, &mut Health), With<Player>>,
    mut died: MessageWriter<Died>,
    mut done: Local<bool>,
) {
    if *done || *state.get() != crate::menu::GameState::Playing {
        return;
    }
    let hurt = std::env::var("FC3_PLAYER_HURT").ok().and_then(|v| {
        let (a, b) = v.split_once(',')?;
        Some((a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?))
    });
    if let Some((after, left)) = hurt {
        if time.elapsed_secs() >= after {
            if let Ok((_, mut h)) = players.single_mut() {
                h.current = left;
                h.since_damage = 0.0;
            }
        }
        return;
    }
    let Some(after) = std::env::var("FC3_PLAYER_DIE").ok().and_then(|v| v.parse::<f32>().ok()) else {
        *done = true;
        return;
    };
    if time.elapsed_secs() < after {
        return;
    }
    *done = true;
    if let Ok((e, mut h)) = players.single_mut() {
        h.current = 0.0;
        h.dead = true;
        died.write(Died { entity: e, killer: None, kind: DamageKind::Bullet });
        info!("test: the player dies");
    }
}

/// Spawned dead (an encounter's bodies): it dies as it appears, by no one's
/// hand, and lies as anyone killed does.
#[derive(Component)]
pub struct DieOnSpawn;

fn die_on_spawn(mut fresh: Query<(Entity, &mut Health), Added<DieOnSpawn>>, mut died: MessageWriter<Died>, mut commands: Commands) {
    for (e, mut h) in &mut fresh {
        h.current = 0.0;
        h.dead = true;
        died.write(Died { entity: e, killer: None, kind: DamageKind::Bullet });
        if std::env::var_os("FC3_ENCOUNTER_DEBUG").is_some() {
            info!("encounter: a body lies at {e:?}");
        }
        commands.entity(e).remove::<DieOnSpawn>();
    }
}

fn add_player_health(mut commands: Commands, player: Query<(Entity, &Transform), With<Player>>) {
    for (e, t) in &player {
        commands.entity(e).insert((
            Health::new(PLAYER_MAX_HEALTH),
            PlayerStats {
                medkits: 2,
                kills: 0,
                deaths: 0,
                respawn_timer: 0.0,
                spawn_point: t.translation,
                hurt_dir: None,
                burning: 0.0,
                last_hurt: DamageKind::Bullet,
            },
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_damage(
    table: Res<DamageTable>,
    perks: Res<crate::progression::Perks>,
    settings: Res<crate::settings::Settings>,
    boosts: Res<crate::syringes::Boosts>,
    mut events: MessageReader<DamageEvent>,
    mut died: MessageWriter<Died>,
    mut targets: Query<(&mut Health, Option<&GlobalTransform>, Option<&mut PlayerStats>, Option<&Armour>, Option<&DamageScale>, Option<&mut Counters>)>,
    mut stats: Query<&mut PlayerStats, Without<Health>>,
    scripted: Res<crate::story::ScriptedPlayer>,
    instigators: Query<(Option<&crate::wildlife::WildAnimal>, Option<&crate::npc::Npc>, Option<&crate::story_actors::StoryActor>, Option<&Name>)>,
    players: Query<(), With<crate::player::Player>>,
    driving: Option<Res<crate::vehicles::Driving>>,
    mut progress: ResMut<crate::progression::Progress>,
    mut harm: ResMut<LastHarm>,
) {
    for ev in events.read() {
        // (Kept a while: those gone are let go now and then.)
        if harm.0.len() > 4096 {
            harm.0.clear();
        }
        harm.0.insert(ev.target, (ev.kind, ev.hit));
        let Ok((mut h, gt, ps, armour, scale, counters)) = targets.get_mut(ev.target) else { continue };
        if h.dead {
            continue;
        }
        // The one-hit syringe: whatever the player lands kills.
        if ps.is_none() && boosts.active(crate::syringes::Syringe::OneHitKill) && ev.instigator.is_some_and(|i| players.contains(i)) {
            h.current = 0.0;
        }
        // The player's skills soften incoming damage (blasts, fire and blows
        // each have their own).
        let resist = match ev.kind {
            DamageKind::Explosion => perks.explosion_taken,
            DamageKind::Fire => perks.fire_taken,
            DamageKind::Melee => perks.melee_taken,
            _ => 1.0,
        };
        // The difficulty: the game's health bars hold more or less of a hit.
        let difficulty = table.0.as_ref().map_or(settings.difficulty.damage_taken(), |d| 1.0 / d.player.bar_scale[settings.difficulty.index()].max(0.05));
        let mut amount = if ps.is_some() { ev.amount * perks.damage_taken * resist * difficulty } else { ev.amount };
        if let Some(a) = armour.filter(|_| ev.kind == DamageKind::Bullet && !ev.headshot && counters.is_none()) {
            amount *= a.0;
        }
        // The game's counters: each kind of harm and body part's armour, and
        // a part that has taken all it can kills.
        match counters {
            Some(mut c) if ps.is_none() => {
                let (taken, kill) = c.take(amount, ev.kind, ev.hit);
                if std::env::var_os("FC3_DAMAGE_DEBUG").is_some() {
                    info!(
                        "damage: {} {:?} {:?} {amount:.1} -> {taken:.1} of {:.1}, parts {:?}{}",
                        c.v.name,
                        ev.kind,
                        ev.hit,
                        h.current,
                        c.pools,
                        if kill { ", killed" } else { "" }
                    );
                }
                amount = if kill { taken.max(h.current) } else { taken };
            }
            _ if matches!(ev.hit, Hit::Bullet(Zone::Head) | Hit::Arrow(Zone::Head)) => amount *= HEADSHOT,
            _ => {}
        }
        if let Some(s) = scale {
            amount *= match ev.kind {
                DamageKind::Bullet | DamageKind::Melee => s.bullet,
                DamageKind::Explosion => s.explosion,
                _ => 1.0,
            };
        }
        // (`SP_VehicleExtraHealth`: the vehicle the player drives takes less.)
        if driving.as_ref().is_some_and(|d| d.vehicle == Some(ev.target)) {
            amount *= perks.vehicle_damage;
        }
        // The hunter's syringe: the player's shots and blows on animals do
        // more.
        if ps.is_none()
            && boosts.active(crate::syringes::Syringe::AnimalDamage)
            && ev.instigator.is_some_and(|i| players.contains(i))
            && instigators.get(ev.target).is_ok_and(|(animal, ..)| animal.is_some())
        {
            amount *= 1.0 + boosts.animal_damage;
        }
        // Syringes: the fireproof one stops burns, the invincible one all.
        if ps.is_some() {
            use crate::syringes::Syringe;
            if boosts.active(Syringe::Fireproof) && ev.kind == DamageKind::Fire {
                amount = 0.0;
            }
            if boosts.active(Syringe::Invincible) {
                amount = 0.0;
            }
        }
        // Body armour takes blows, blasts and bullets first (`SP_ArmorDefense` makes
        // it go further).
        if ps.is_some() && progress.armour > 0.0 && matches!(ev.kind, DamageKind::Bullet | DamageKind::Explosion | DamageKind::Melee) {
            let absorbed = amount.min(progress.armour * perks.armour_absorb);
            progress.armour = (progress.armour - absorbed / perks.armour_absorb).max(0.0);
            if progress.armour < 0.5 {
                progress.armour = 0.0;
            }
            amount -= absorbed;
        }
        // FC3_GOD (tests), or a mission's scene: the player takes no damage.
        if ps.is_some() && (scripted.god || std::env::var("FC3_GOD").is_ok()) {
            info!("player hit: {amount:.1} {:?}", ev.kind);
            amount = 0.0;
        }
        h.current -= amount;
        h.since_damage = 0.0;
        let is_player = ps.is_some();
        if let Some(mut ps) = ps {
            if amount > 0.0 {
                ps.last_hurt = ev.kind;
            }
            if ev.kind == DamageKind::Fire {
                ps.burning = 1.2;
            }
            if let (Some(src), Some(gt)) = (ev.source, gt) {
                let dir = (src - gt.translation()).normalize_or_zero();
                ps.hurt_dir = Some((dir, 1.0));
            }
        }
        if h.current <= 0.0 {
            h.current = 0.0;
            h.dead = true;
            if is_player {
                let attacker = ev.instigator.and_then(|e| instigators.get(e).ok()).map(|(animal, npc, story, name)| {
                    let kind = animal.map(|a| a.animal().name().to_string())
                        .or_else(|| npc.map(|n| format!("{:?}", n.faction)))
                        .or_else(|| name.map(|n| n.as_str().to_string())).unwrap_or_else(|| "unnamed entity".into());
                    story.map_or(kind.clone(), |s| format!("{kind} (story {})", s.id))
                }).unwrap_or_else(|| "environment/unknown".into());
                info!("player died: {:?}, attacker {attacker}, source {:?}", ev.kind, ev.source);
            }
            died.write(Died { entity: ev.target, killer: ev.instigator, kind: ev.kind });
        }
    }
    let _ = &mut stats;
}

fn player_regen(
    time: Res<Time>,
    perks: Res<crate::progression::Perks>,
    table: Res<DamageTable>,
    settings: Res<crate::settings::Settings>,
    mut q: Query<(&mut Health, &mut PlayerStats), With<Player>>,
) {
    // The game's delay for the difficulty.
    let delay = table.0.as_ref().map_or(REGEN_DELAY, |d| d.player.regen_delay[settings.difficulty.index()]);
    let dt = time.delta_secs();
    for (mut h, mut ps) in &mut q {
        ps.burning = (ps.burning - dt).max(0.0);
        if let Some((_, s)) = &mut ps.hurt_dir {
            *s -= dt * 0.8;
            if *s <= 0.0 {
                ps.hurt_dir = None;
            }
        }
        if h.dead {
            continue;
        }
        h.since_damage += dt;
        if h.since_damage > delay * perks.regen_delay {
            // Regenerate to the top of the current (partially filled) segment.
            let cap = ((h.current / HEALTH_SEGMENT).ceil() * HEALTH_SEGMENT).clamp(HEALTH_SEGMENT, h.max);
            if h.current < cap {
                h.current = (h.current + REGEN_RATE * dt).min(cap);
            }
        }
    }
}

fn fall_damage(
    perks: Res<crate::progression::Perks>,
    body: Res<crate::player::PlayerBody>,
    (time, mut lifted): (Res<Time>, Local<bool>),
    mut q: Query<(Entity, &mut Player, &Health, &mut Transform)>,
    mut damage: MessageWriter<DamageEvent>,
) {
    for (e, mut p, h, mut t) in &mut q {
        // FC3_FALL_TEST=metres: lifted that high 10 s in, to fall.
        if let Some(height) = std::env::var("FC3_FALL_TEST").ok().and_then(|v| v.parse::<f32>().ok()).filter(|_| !*lifted && time.elapsed_secs() > 10.0) {
            *lifted = true;
            t.translation.y += height;
            info!("fall-test: lifted {height} m");
        }
        let v = std::mem::take(&mut p.last_landing_speed);
        // The game's falls: by the height the landing speed comes from, a
        // share of his health, or death.
        if let Some(fall) = body.0.fall.filter(|_| body.0.gravity > 0.0 && v > 0.0) {
            let height = v * v / (2.0 * body.0.gravity);
            let share = fall.share(height);
            if std::env::var_os("FC3_FALL_TEST").is_some() {
                info!("fall-test: landed at {v:.1} m/s, from {height:.1} m: {:.0}% of health", share * 100.0);
            }
            if share > 0.0 {
                let amount = if share >= 1.0 { 100_000.0 } else { share * h.max * perks.fall_damage };
                damage.write(DamageEvent { target: e, amount, kind: DamageKind::Fall, source: None, instigator: None, headshot: false, hit: Hit::Whole });
            }
            continue;
        }
        // Safe up to ~3 m drops; lethal around 15 m.
        if v > 9.0 {
            let amount = (v - 9.0) * 9.0 * perks.fall_damage;
            damage.write(DamageEvent { target: e, amount, kind: DamageKind::Fall, source: None, instigator: None, headshot: false, hit: Hit::Whole });
        }
    }
}

/// Healing: a medkit is a syringe; out of medkits and badly hurt, the player
/// patches himself up (pulls the bullet, sets the finger...) back to two
/// segments. The arms play it and the health comes partway through.
#[allow(clippy::too_many_arguments)]
fn use_medkit(
    actions: Res<crate::controls::ActionState>,
    (settings, text, time): (Res<crate::settings::Settings>, Option<Res<crate::hud::GameText>>, Res<Time>),
    mut held: Local<f32>,
    perks: Res<crate::progression::Perks>,
    busy: Res<crate::fp::HandsBusy>,
    mut hud: ResMut<crate::hud::HudState>,
    mut heals: MessageWriter<crate::fp::FpAction>,
    mut q: Query<(&Health, &mut PlayerStats, &Player)>,
) {
    use crate::controls::Action;
    let Ok((h, mut ps, p)) = q.single_mut() else { return };
    if h.dead || busy.busy() || p.noclip {
        return;
    }
    // (Patching up is held, as the game's "Hold [Heal] to heal" asks; a
    // medkit is a press.)
    let manual = ps.medkits == 0 && h.current < HEALTH_SEGMENT;
    if manual && hud.prompt.is_none() {
        let prompt = text.as_deref().and_then(|t| t.prompt(crate::hud::prompt::HEAL, &actions, &settings));
        hud.prompt = Some(prompt.unwrap_or_else(|| format!("hold {}  heal", actions.label(Action::Medkit, &settings))));
    }
    let patch_up = manual
        && crate::hud::held_long_enough(&mut held, actions.pressed(Action::Medkit), actions.just_pressed(Action::Medkit), time.delta_secs(), MANUAL_HEAL_HOLD);
    if !(patch_up || ps.medkits > 0 && actions.just_pressed(Action::Medkit)) || h.current >= h.max {
        return;
    }
    if ps.medkits > 0 {
        ps.medkits -= 1;
        let to = if perks.medkit_full_heal { h.max } else { (h.current + MEDKIT_HEAL + perks.medkit_bonus).min(h.max) };
        heals.write(crate::fp::FpAction::Syringe { to });
    } else if manual {
        heals.write(crate::fp::FpAction::ManualHeal { kind: ps.last_hurt, to: (perks.manual_heal_bars * HEALTH_SEGMENT).min(h.max) });
    }
}

/// After a death the player wakes at the nearest liberated outpost (as at a
/// safe house), else where the game began.
fn respawn_player(
    time: Res<Time>,
    perks: Res<crate::progression::Perks>,
    outposts: Res<crate::outposts::Outposts>,
    mut hud: ResMut<crate::hud::HudState>,
    mut died: MessageReader<Died>,
    mut q: Query<(Entity, &mut Health, &mut PlayerStats, &mut Transform, &mut Player)>,
) {
    let deaths: Vec<Entity> = died.read().map(|d| d.entity).collect();
    for (e, mut h, mut ps, mut t, mut p) in &mut q {
        if deaths.contains(&e) {
            ps.deaths += 1;
            ps.respawn_timer = RESPAWN_SECS;
            p.velocity = Vec3::ZERO;
        }
        if h.dead {
            ps.respawn_timer -= time.delta_secs();
            if ps.respawn_timer <= 0.0 {
                *h = Health::new(perks.max_health);
                ps.medkits = ps.medkits.max(1);
                let died_at = t.translation;
                let safe = outposts
                    .list
                    .iter()
                    .enumerate()
                    .filter(|(_, o)| o.liberated)
                    .min_by(|a, b| a.1.centre.distance(died_at).total_cmp(&b.1.centre.distance(died_at)))
                    .map(|(i, _)| i);
                match safe {
                    Some(i) if crate::outposts::fast_travel(&outposts, i, &mut t, &mut p) => {
                        hud.notify("You came to at a rebel-held outpost");
                    }
                    _ => {
                        t.translation = ps.spawn_point + Vec3::Y * 60.0;
                        p.spawning = true;
                    }
                }
                p.velocity = Vec3::ZERO;
                info!("respawned at {:.0?} (died at {died_at:.0?})", t.translation);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::counters::{Damage, Vulnerability};

    fn table() -> DamageTable {
        let mut heavy = Vulnerability { name: "Pirate_HeavyGunner".into(), torso: 0.025, legs: 0.04, ..Default::default() };
        heavy.harms[3] = 0.5;
        let tiger = Vulnerability { name: "Tiger".into(), animal: true, health: Some(800.0), head: 2.3, ..Default::default() };
        let pirate = Vulnerability { name: "Pirate_Assaulter".into(), ..Default::default() };
        DamageTable(Some(Damage { archetypes: vec![heavy, tiger, pirate], ..Default::default() }))
    }

    #[test]
    fn parts_armour_and_harms_follow_the_counters() {
        let t = table();
        let (mut pirate, health) = t.archetype("Pirate_Assaulter_Commander").unwrap();
        assert_eq!(health, 100.0);
        // Any real head shot kills; two rifle rounds to the torso do.
        assert!(pirate.clone().take(40.0, DamageKind::Bullet, Hit::Bullet(Zone::Head)).1);
        assert!(!pirate.take(40.0, DamageKind::Bullet, Hit::Bullet(Zone::Torso)).1);
        assert!(pirate.take(40.0, DamageKind::Bullet, Hit::Bullet(Zone::Torso)).1);
        // A heavy's armour soaks bullets to the body, burns half.
        let (mut heavy, _) = t.archetype("Pirate_HeavyGunner").unwrap();
        assert_eq!(heavy.take(40.0, DamageKind::Bullet, Hit::Bullet(Zone::Torso)), (1.0, false));
        assert_eq!(heavy.take(10.0, DamageKind::Fire, Hit::Whole), (5.0, false));
        // An animal has its own health and only a head multiplier.
        let (mut tiger, health) = t.archetype("Tiger").unwrap();
        assert_eq!(health, 800.0);
        assert!(tiger.pools.is_none());
        assert!((tiger.take(100.0, DamageKind::Bullet, Hit::Bullet(Zone::Head)).0 - 230.0).abs() < 1e-3);
    }
}
