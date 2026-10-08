//! Health, damage and death for the player and any damageable entity.

use bevy::prelude::*;

use crate::player::Player;

pub struct CombatPlugin;

impl Plugin for CombatPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<DamageEvent>()
            .add_message::<Died>()
            .add_systems(Startup, add_player_health.after(crate::player::PlayerSpawn))
            .add_systems(Update, (apply_damage, player_regen, fall_damage, use_medkit, respawn_player).chain());
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

#[derive(Message, Clone, Copy)]
pub struct DamageEvent {
    pub target: Entity,
    pub amount: f32,
    pub kind: DamageKind,
    /// Where the damage came from (for hit indicators and ragdoll impulses).
    pub source: Option<Vec3>,
    pub instigator: Option<Entity>,
    pub headshot: bool,
}

#[derive(Message, Clone, Copy)]
pub struct Died {
    pub entity: Entity,
    pub killer: Option<Entity>,
    pub kind: DamageKind,
}

fn add_player_health(mut commands: Commands, player: Query<(Entity, &Transform), With<Player>>) {
    for (e, t) in &player {
        commands.entity(e).insert((
            Health::new(PLAYER_MAX_HEALTH),
            PlayerStats { medkits: 2, kills: 0, deaths: 0, respawn_timer: 0.0, spawn_point: t.translation, hurt_dir: None, burning: 0.0, last_hurt: DamageKind::Bullet },
        ));
    }
}

fn apply_damage(
    perks: Res<crate::progression::Perks>,
    settings: Res<crate::settings::Settings>,
    boosts: Res<crate::syringes::Boosts>,
    mut events: MessageReader<DamageEvent>,
    mut died: MessageWriter<Died>,
    mut targets: Query<(&mut Health, Option<&GlobalTransform>, Option<&mut PlayerStats>, Option<&Armour>, Option<&DamageScale>)>,
    mut stats: Query<&mut PlayerStats, Without<Health>>,
) {
    for ev in events.read() {
        let Ok((mut h, gt, ps, armour, scale)) = targets.get_mut(ev.target) else { continue };
        if h.dead {
            continue;
        }
        // The player's skills soften incoming damage.
        let mut amount = if ps.is_some() { ev.amount * perks.damage_taken * settings.difficulty.damage_taken() } else { ev.amount };
        if let Some(a) = armour.filter(|_| ev.kind == DamageKind::Bullet && !ev.headshot) {
            amount *= a.0;
        }
        if let Some(s) = scale {
            amount *= match ev.kind {
                DamageKind::Bullet | DamageKind::Melee => s.bullet,
                DamageKind::Explosion => s.explosion,
                _ => 1.0,
            };
        }
        // Syringes: the fighter's halves damage, the fireproof one stops burns.
        if ps.is_some() {
            if boosts.active(crate::syringes::Syringe::Fighter) && ev.kind != DamageKind::Drown {
                amount *= 0.5;
            }
            if boosts.active(crate::syringes::Syringe::Fireproof) && ev.kind == DamageKind::Fire {
                amount = 0.0;
            }
        }
        // FC3_GOD (tests): the player takes no damage, but it is logged.
        if ps.is_some() && std::env::var("FC3_GOD").is_ok() {
            info!("player hit: {amount:.1} {:?}", ev.kind);
            amount = 0.0;
        }
        h.current -= amount;
        h.since_damage = 0.0;
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
            died.write(Died { entity: ev.target, killer: ev.instigator, kind: ev.kind });
        }
    }
    let _ = &mut stats;
}

fn player_regen(time: Res<Time>, mut q: Query<(&mut Health, &mut PlayerStats), With<Player>>) {
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
        if h.since_damage > REGEN_DELAY {
            // Regenerate to the top of the current (partially filled) segment.
            let cap = ((h.current / HEALTH_SEGMENT).ceil() * HEALTH_SEGMENT).clamp(HEALTH_SEGMENT, h.max);
            if h.current < cap {
                h.current = (h.current + REGEN_RATE * dt).min(cap);
            }
        }
    }
}

fn fall_damage(perks: Res<crate::progression::Perks>, mut q: Query<(Entity, &mut Player)>, mut damage: MessageWriter<DamageEvent>) {
    for (e, mut p) in &mut q {
        let v = std::mem::take(&mut p.last_landing_speed);
        // Safe up to ~3 m drops; lethal around 15 m.
        if v > 9.0 {
            let amount = (v - 9.0) * 9.0 * perks.fall_damage;
            damage.write(DamageEvent { target: e, amount, kind: DamageKind::Fall, source: None, instigator: None, headshot: false });
        }
    }
}

/// Healing: a medkit is a syringe; out of medkits and badly hurt, the player
/// patches himself up (pulls the bullet, sets the finger...) back to two
/// segments. The arms play it and the health comes partway through.
#[allow(clippy::too_many_arguments)]
fn use_medkit(
    actions: Res<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
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
    let manual = ps.medkits == 0 && h.current < HEALTH_SEGMENT;
    if manual && hud.prompt.is_none() {
        hud.prompt = Some(format!("{}  heal", actions.label(Action::Medkit, &settings)));
    }
    if !actions.just_pressed(Action::Medkit) || h.current >= h.max {
        return;
    }
    if ps.medkits > 0 {
        ps.medkits -= 1;
        let to = if perks.medkit_full_heal { h.max } else { (h.current + MEDKIT_HEAL).min(h.max) };
        heals.write(crate::fp::FpAction::Syringe { to });
    } else if manual {
        heals.write(crate::fp::FpAction::ManualHeal { kind: ps.last_hurt, to: (2.0 * HEALTH_SEGMENT).min(h.max) });
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
            ps.respawn_timer = 5.0;
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
