//! Enemy outposts: garrisons, alarms, reinforcements, liberation and fast travel.
//!
//! Outposts come from the converter (`world/outposts.txt`, see
//! `fc3_formats::outposts`). Every guard post inside one is manned. When a
//! guard spots the player, the nearest one runs for an alarm box; if they get
//! there the alarm sounds (the game's own two-tone alarm from every box) and reinforcements arrive. The player can sabotage
//! alarm boxes beforehand. Once every guard is dead the outpost is liberated:
//! it stops spawning enemies, shows as friendly on the map, and becomes a
//! fast-travel destination. Liberated outposts are saved.

use bevy::prelude::*;
use fc3_formats::outposts::Outpost;

use crate::combat::Health;
use crate::data::CacheInfo;
use crate::hud::HudState;
use crate::npc::{Npc, NpcState};
use crate::player::Player;

pub struct OutpostsPlugin;

impl Plugin for OutpostsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Outposts>().add_message::<AlarmRaised>().add_message::<OutpostLiberated>().add_systems(
            Update,
            (
                load_outposts.run_if(resource_exists::<CacheInfo>),
                assign_runners,
                sound_alarms,
                reinforce,
                sabotage_alarms.before(crate::hud::HudSet).after(crate::turrets::man_turrets),
                liberate,
                outpost_status.before(crate::hud::HudSet),
                outpost_test,
                convoy_test,
            )
                .chain()
                .run_if(in_state(crate::menu::GameState::Playing)),
        );
    }
}

/// Seconds between the alarm and the reinforcements setting off.
const REINFORCE_DELAY: f32 = 10.0;
/// Siren runs this long after the last guard in combat.
const ALARM_SECS: f32 = 50.0;

/// Marks an enemy belonging to an outpost's garrison (outpost index).
#[derive(Component, Clone, Copy)]
pub struct Garrison(pub usize);

/// How far a liberated outpost's hold on the country reaches (rebel patrols
/// drive the roads there).
const REBEL_COUNTRY: f32 = 700.0;

/// A guard reached an alarm box.
#[derive(Message, Clone, Copy)]
pub struct AlarmRaised {
    pub outpost: usize,
}

/// An outpost was taken (`undetected`: without the alarm being raised).
#[derive(Message, Clone, Copy)]
pub struct OutpostLiberated {
    pub outpost: usize,
    pub undetected: bool,
}

pub struct OutpostState {
    pub centre: Vec3,
    pub radius: f32,
    pub alarms: Vec<Vec3>,
    pub disabled: Vec<bool>,
    pub liberated: bool,
    /// Time the siren started (game seconds) while it sounds.
    pub alarm_since: Option<f32>,
    pub alarm_raised: bool,
    /// Reinforcements arrive at this time.
    reinforce_at: Option<f32>,
    /// When the last reinforcements were sent (they take a moment to appear).
    reinforced: f32,
    /// Garrison seen alive while the player was near (an assault is under way).
    engaged: bool,
    /// Guard posts inside (marker ids), filled from loaded sectors.
    pub posts: Vec<(u64, Vec3)>,
}

impl OutpostState {
    fn new(o: &Outpost) -> OutpostState {
        let v = |p: [f32; 3]| Vec3::from_array(p);
        OutpostState {
            centre: v(o.centre),
            radius: o.radius,
            alarms: o.alarms.iter().copied().map(v).collect(),
            disabled: vec![false; o.alarms.len()],
            liberated: false,
            alarm_since: None,
            alarm_raised: false,
            reinforce_at: None,
            reinforced: f32::MIN,
            engaged: false,
            posts: Vec::new(),
        }
    }

    pub fn contains(&self, p: Vec3) -> bool {
        Vec2::new(p.x - self.centre.x, p.z - self.centre.z).length() <= self.radius
    }

    fn working_alarms(&self) -> impl Iterator<Item = Vec3> + '_ {
        self.alarms.iter().zip(&self.disabled).filter(|(_, d)| !**d).map(|(a, _)| *a)
    }
}

#[derive(Resource, Default)]
pub struct Outposts {
    pub list: Vec<OutpostState>,
    loaded: bool,
    /// Liberated indices restored from a save before the table loaded.
    pending: Vec<u32>,
}

impl Outposts {
    /// The outpost containing a position, if any.
    pub fn at(&self, p: Vec3) -> Option<usize> {
        self.list.iter().position(|o| o.contains(p))
    }

    /// Whether the rebels hold the country round `p`: the nearest outpost
    /// within `REBEL_COUNTRY` is liberated.
    pub fn rebel_country(&self, p: Vec3) -> bool {
        self.list
            .iter()
            .map(|o| (o.centre.distance(p), o.liberated))
            .filter(|(d, _)| *d < REBEL_COUNTRY)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .is_some_and(|(_, liberated)| liberated)
    }

    pub fn liberated_ids(&self) -> Vec<u32> {
        self.list.iter().enumerate().filter(|(_, o)| o.liberated).map(|(i, _)| i as u32).collect()
    }

    pub fn set_liberated(&mut self, ids: &[u32]) {
        if !self.loaded {
            self.pending = ids.to_vec();
            return;
        }
        for (i, o) in self.list.iter_mut().enumerate() {
            o.liberated = ids.contains(&(i as u32));
        }
    }
}

/// The alarm box's sounds (`CombatHelpers/AlarmPoint`): the two-tone alarm it
/// sounds while raised, and the crackle as it's switched off.
const ALARM_LOOP: u32 = 0x0013_DA46;
const ALARM_OFF: u32 = 0x0013_DA49;

fn load_outposts(info: Res<CacheInfo>, mut outposts: ResMut<Outposts>) {
    if outposts.loaded {
        return;
    }
    outposts.loaded = true;
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::OUTPOSTS_REL)).unwrap_or_default();
    outposts.list = fc3_formats::outposts::from_text(&text).iter().map(OutpostState::new).collect();
    let pending = std::mem::take(&mut outposts.pending);
    if !pending.is_empty() {
        outposts.set_liberated(&pending);
    }
    info!("{} outposts", outposts.list.len());
}

/// Sends the nearest fighting guard to a working alarm box.
fn assign_runners(mut outposts: ResMut<Outposts>, mut npcs: Query<(&Transform, &mut Npc, &Health, &Garrison)>) {
    for (i, o) in outposts.list.iter_mut().enumerate() {
        if o.liberated || o.alarm_since.is_some() {
            continue;
        }
        let mut running = false;
        let mut best: Option<(f32, Vec3)> = None;
        for (t, npc, h, g) in &npcs {
            if g.0 != i || h.dead {
                continue;
            }
            if npc.alarm_target.is_some() {
                running = true;
            }
            if npc.state == NpcState::Combat {
                for a in o.working_alarms() {
                    let d = t.translation.distance(a);
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, a));
                    }
                }
            }
        }
        if running {
            continue;
        }
        let Some((_, alarm)) = best else { continue };
        // The closest guard in combat to that box goes.
        if let Some((_, mut npc, _, _)) = npcs
            .iter_mut()
            .filter(|(_, n, h, g)| g.0 == i && !h.dead && n.state == NpcState::Combat)
            .min_by(|a, b| a.0.translation.distance(alarm).total_cmp(&b.0.translation.distance(alarm)))
        {
            npc.alarm_target = Some(alarm);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sound_alarms(
    time: Res<Time>,
    mut loops: MessageWriter<crate::sound::LoopSfx>,
    mut raised: MessageReader<AlarmRaised>,
    mut outposts: ResMut<Outposts>,
    npcs: Query<(&Npc, &Health, &Garrison)>,
    mut hud: ResMut<HudState>,
) {
    let now = time.elapsed_secs();
    for r in raised.read() {
        let Some(o) = outposts.list.get_mut(r.outpost) else { continue };
        if o.liberated || o.alarm_since.is_some() {
            continue;
        }
        o.alarm_since = Some(now);
        o.alarm_raised = true;
        o.reinforce_at = Some(now + REINFORCE_DELAY);
        hud.notify("ALARM RAISED - reinforcements are coming");
    }
    // The siren stops once the garrison has stood down for a while.
    for (i, o) in outposts.list.iter_mut().enumerate() {
        let Some(since) = o.alarm_since else { continue };
        let fighting = npcs.iter().any(|(n, h, g)| g.0 == i && !h.dead && matches!(n.state, NpcState::Combat | NpcState::Search));
        if fighting {
            o.alarm_since = Some(since.max(now - 5.0));
        }
        if o.liberated || now - since > ALARM_SECS {
            o.alarm_since = None;
            continue;
        }
        // Every box sounds while the alarm is up.
        for (k, a) in o.alarms.iter().enumerate() {
            let key = 0x414C_0000_0000_0000 | (i as u64) << 8 | k as u64;
            loops.write(crate::sound::LoopSfx { key, event: ALARM_LOOP, at: Some(*a + Vec3::Y * 2.0), volume: 1.0, ..default() });
        }
    }
}

/// `FC3_CONVOY_TEST` (with `--patrol-test` for its log): reinforcements for
/// the nearest outpost, five seconds in.
fn convoy_test(
    time: Res<Time>,
    mut sent: Local<Option<f32>>,
    outposts: Res<Outposts>,
    players: Query<(&Transform, &Player)>,
    mut convoys: MessageWriter<crate::patrols::Convoy>,
) {
    if std::env::var("FC3_CONVOY_TEST").is_err() || sent.is_some_and(|t| t < 0.0) {
        return;
    }
    let Ok((pt, p)) = players.single() else { return };
    if p.spawning {
        return;
    }
    let start = *sent.get_or_insert(time.elapsed_secs());
    if time.elapsed_secs() < start + 5.0 {
        return;
    }
    let Some((i, o)) = outposts.list.iter().enumerate().min_by(|a, b| a.1.centre.distance(pt.translation).total_cmp(&b.1.centre.distance(pt.translation)))
    else {
        return;
    };
    convoys.write(crate::patrols::Convoy { to: o.centre, radius: o.radius, garrison: i });
    *sent = Some(-1.0);
}

/// Reinforcements arrive at the outpost's edge, hunting the player.
fn reinforce(time: Res<Time>, mut outposts: ResMut<Outposts>, mut convoys: MessageWriter<crate::patrols::Convoy>) {
    let now = time.elapsed_secs();
    for (i, o) in outposts.list.iter_mut().enumerate() {
        let Some(at) = o.reinforce_at else { continue };
        if now < at || o.liberated {
            continue;
        }
        o.reinforce_at = None;
        o.reinforced = now;
        // They drive in from the nearest road (on foot if there's none).
        convoys.write(crate::patrols::Convoy { to: o.centre, radius: o.radius, garrison: i });
    }
}

/// Alarm boxes can be switched off up close.
fn sabotage_alarms(
    actions: Res<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    mut outposts: ResMut<Outposts>,
    players: Query<(&Transform, &Health), With<Player>>,
    mut hud: ResMut<HudState>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    (mut xp, skills): (MessageWriter<crate::progression::XpGain>, Option<Res<crate::progression::Skills>>),
) {
    let Ok((pt, h)) = players.single() else { return };
    if h.dead || hud.prompt.is_some() {
        return;
    }
    for o in outposts.list.iter_mut().filter(|o| !o.liberated) {
        for (k, a) in o.alarms.iter().enumerate() {
            if o.disabled[k] || a.distance(pt.translation) > 2.6 {
                continue;
            }
            hud.prompt = Some(format!("{}  disable alarm", actions.label(crate::controls::Action::Interact, &settings)));
            if actions.just_pressed(crate::controls::Action::Interact) {
                o.disabled[k] = true;
                let left = o.disabled.iter().filter(|d| !**d).count();
                hud.notify(if left == 0 { "All alarms disabled".to_string() } else { format!("Alarm disabled ({left} left)") });
                sfx.write(crate::sound::PlaySfx { event: ALARM_OFF, at: Some(*a), volume: 1.0 });
                // (The game's reward for it.)
                let amount = skills.as_ref().and_then(|s| s.xp_reward("DisableAlarm")).unwrap_or(0.0) as u32;
                if amount > 0 {
                    xp.write(crate::progression::XpGain { amount, reason: "Alarm disabled" });
                }
            }
            return;
        }
    }
}

/// An outpost falls when its whole garrison is dead.
fn liberate(
    time: Res<Time>,
    mut outposts: ResMut<Outposts>,
    director: Res<crate::npc::NpcDirector>,
    players: Query<&Transform, With<Player>>,
    garrison: Query<(&Health, &Garrison)>,
    mut hud: ResMut<HudState>,
    mut done: MessageWriter<OutpostLiberated>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let Ok(pt) = players.single() else { return };
    for (i, o) in outposts.list.iter_mut().enumerate() {
        if o.liberated {
            continue;
        }
        let near = Vec2::new(pt.translation.x - o.centre.x, pt.translation.z - o.centre.z).length() < o.radius + 120.0;
        let alive = garrison.iter().filter(|(h, g)| g.0 == i && !h.dead).count();
        if alive > 0 && near {
            o.engaged = true;
        }
        if !o.engaged || alive > 0 || o.reinforce_at.is_some() || time.elapsed_secs() < o.reinforced + 3.0 {
            continue;
        }
        if !near {
            // The player left mid-assault: the outpost regroups.
            o.engaged = false;
            continue;
        }
        // Posts that never got a guard only count if they were too close to
        // the player to spawn one.
        let unmanned = o.posts.iter().filter(|(id, p)| !director.was_killed(*id) && p.distance(pt.translation) > 40.0).count();
        if unmanned > 0 {
            continue;
        }
        o.liberated = true;
        o.alarm_since = None;
        let undetected = !o.alarm_raised;
        hud.notify(if undetected { "OUTPOST LIBERATED - UNDETECTED" } else { "OUTPOST LIBERATED" });
        done.write(OutpostLiberated { outpost: i, undetected });
        sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 1.0 });
    }
}

/// HUD line while inside an enemy outpost.
fn outpost_status(
    outposts: Res<Outposts>,
    players: Query<(&Transform, &Player)>,
    garrison: Query<(&Health, &Garrison)>,
    mut hud: ResMut<HudState>,
    area: Res<crate::story::MissionArea>,
) {
    let Ok((pt, p)) = players.single() else { return };
    // The vehicle readout takes the line while driving.
    if p.driving {
        return;
    }
    // No outpost play inside a mission being played.
    if area.contains(pt.translation) {
        hud.status = None;
        return;
    }
    let Some(i) =
        outposts.list.iter().position(|o| !o.liberated && Vec2::new(pt.translation.x - o.centre.x, pt.translation.z - o.centre.z).length() < o.radius + 60.0)
    else {
        hud.status = None;
        return;
    };
    let o = &outposts.list[i];
    let alive = garrison.iter().filter(|(h, g)| g.0 == i && !h.dead).count();
    let alarms = if o.alarm_since.is_some() {
        "ALARM SOUNDING".to_string()
    } else if o.alarms.is_empty() {
        String::new()
    } else {
        format!("alarms {}/{} active", o.disabled.iter().filter(|d| !**d).count(), o.alarms.len())
    };
    hud.status = Some(format!("ENEMY OUTPOST   {alive} hostiles   {alarms}"));
}

/// Fast travel to a liberated outpost: drops the player at its centre.
pub fn fast_travel(outposts: &Outposts, index: usize, t: &mut Transform, p: &mut Player) -> bool {
    let Some(o) = outposts.list.get(index).filter(|o| o.liberated) else { return false };
    t.translation = Vec3::new(o.centre.x + 4.0, o.centre.y + 2.0, o.centre.z + 4.0);
    p.velocity = Vec3::ZERO;
    p.spawning = true;
    true
}

#[derive(Default)]
struct OutpostTest {
    phase: u8,
    t0: f32,
    target: Option<usize>,
}

/// `--outpost-test`: alerts the nearest outpost's garrison, waits for the
/// alarm, then kills the garrison and reinforcements until it is liberated.
#[allow(clippy::too_many_arguments)]
fn outpost_test(
    args: Res<crate::Args>,
    time: Res<Time>,
    mut test: Local<OutpostTest>,
    outposts: Res<Outposts>,
    players: Query<(Entity, &Transform, &Player)>,
    mut npcs: Query<(Entity, &mut Npc, &Health, &Garrison)>,
    mut damage: MessageWriter<crate::combat::DamageEvent>,
    mut liberated: MessageReader<OutpostLiberated>,
) {
    if !args.outpost_test {
        return;
    }
    let now = time.elapsed_secs();
    let Ok((pe, pt, p)) = players.single() else { return };
    if p.spawning {
        return;
    }
    let Some(i) = test.target.or_else(|| {
        outposts.list.iter().enumerate().min_by(|a, b| a.1.centre.distance(pt.translation).total_cmp(&b.1.centre.distance(pt.translation))).map(|(i, _)| i)
    }) else {
        return;
    };
    test.target = Some(i);
    let o = &outposts.list[i];
    let alive: Vec<Entity> = npcs.iter().filter(|(_, _, h, g)| g.0 == i && !h.dead).map(|(e, ..)| e).collect();
    for l in liberated.read() {
        info!("outpost-test: outpost {} liberated (undetected: {}) at t={:.1}", l.outpost, l.undetected, now - test.t0);
    }
    match test.phase {
        0 if !alive.is_empty() => {
            info!("outpost-test: outpost {i} at {:?}, garrison {} of {} posts", o.centre, alive.len(), o.posts.len());
            test.phase = 1;
            test.t0 = now;
        }
        1 if now - test.t0 > 2.0 => {
            for (_, mut n, h, g) in &mut npcs {
                if g.0 == i && !h.dead {
                    n.awareness = 1.0;
                    n.last_known = Some(pt.translation);
                }
            }
            info!("outpost-test: garrison alerted");
            test.phase = 2;
        }
        2 if o.alarm_raised || now - test.t0 > 40.0 => {
            info!("outpost-test: alarm raised: {} (t={:.1})", o.alarm_raised, now - test.t0);
            test.phase = 3;
        }
        3 => {
            if o.liberated {
                test.phase = 4;
                return;
            }
            for e in alive {
                damage.write(crate::combat::DamageEvent {
                    target: e,
                    amount: 1000.0,
                    kind: crate::combat::DamageKind::Bullet,
                    source: Some(pt.translation),
                    instigator: Some(pe),
                    headshot: false,
                });
            }
        }
        _ => {}
    }
}
