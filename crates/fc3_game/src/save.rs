//! Save and continue.
//!
//! While playing, the game autosaves every minute, on pausing and on quitting
//! to `save.txt` beside the converted cache. The main menu offers CONTINUE when
//! a save exists. Automated test runs never read or write saves, so they can't
//! clobber a real one, unless `FC3_SAVE` names a save file explicitly.

use std::path::PathBuf;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::combat::{Health, PlayerStats};
use crate::data::CacheInfo;
use crate::menu::GameState;
use crate::pickups::{Explosive, Pickup, WorldState};
use crate::player::Player;
use crate::sky::TimeOfDay;
use crate::weapons::{Inventory, WeaponSlot};
use crate::Args;

const AUTOSAVE_SECS: f32 = 60.0;
const FORMAT: &str = "fc3save 1";

pub struct SavePlugin;

impl Plugin for SavePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SaveSlot>()
            .init_resource::<SaveContinued>()
            .add_message::<ContinueGame>()
            .add_systems(OnExit(GameState::Loading), find_save)
            .add_systems(OnEnter(GameState::Playing), (mark_played, continue_from_args))
            .add_systems(OnEnter(GameState::Paused), save_now)
            .add_systems(Update, (continue_game, autosave))
            .add_systems(Last, save_on_exit);
    }
}

/// Everything restored by CONTINUE.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveGame {
    /// Bevy-space feet position (re-grounded on load).
    pub position: Vec3,
    pub yaw: f32,
    pub health: f32,
    pub medkits: u32,
    pub kills: u32,
    pub herbs: u32,
    pub grenades: u32,
    pub molotovs: u32,
    /// C4 charges and mines.
    pub c4: u32,
    pub mines: u32,
    /// Fire and explosive arrows carried.
    pub arrows: [u32; 2],
    pub hours: f32,
    /// Selected weapon name.
    pub current: String,
    /// Owned weapons: name, magazine, reserve.
    pub weapons: Vec<(String, u32, u32)>,
    /// Collected pickups and destroyed props.
    pub removed: Vec<u64>,
    /// Explored map sectors, one bit each.
    pub explored: Vec<u8>,
    /// Map waypoint (Bevy-space ground position).
    pub waypoint: Option<Vec3>,
    /// Liberated outposts (indices into the outpost table).
    pub liberated: Vec<u32>,
    /// Weapon attachments: weapon name and attachment bits.
    pub attachments: Vec<(String, u8)>,
    /// Weapon paint jobs: weapon name and paint name.
    pub paints: Vec<(String, String)>,
    pub progress: crate::progression::Progress,
}

impl SaveGame {
    pub fn to_text(&self) -> String {
        let p = self.position;
        let mut out = format!("{FORMAT}\nposition {} {} {}\nyaw {}\n", p.x, p.y, p.z, self.yaw);
        out += &format!("health {}\nmedkits {}\nkills {}\nherbs {}\n", self.health, self.medkits, self.kills, self.herbs);
        out += &format!("grenades {}\nmolotovs {}\nhours {}\ncurrent {}\n", self.grenades, self.molotovs, self.hours, self.current);
        out += &format!("explosives {} {}\n", self.c4, self.mines);
        if self.arrows != [0; 2] {
            out += &format!("arrows {} {}\n", self.arrows[0], self.arrows[1]);
        }
        for (name, mag, reserve) in &self.weapons {
            out += &format!("weapon {mag} {reserve} {name}\n");
        }
        for (name, bits) in &self.attachments {
            out += &format!("attachments {bits} {name}\n");
        }
        for (name, paint) in &self.paints {
            out += &format!("paint {paint} {name}\n");
        }
        for ids in self.removed.chunks(16) {
            let ids: Vec<String> = ids.iter().map(|id| format!("{id:x}")).collect();
            out += &format!("removed {}\n", ids.join(" "));
        }
        if self.explored.iter().any(|b| *b != 0) {
            let hex: String = self.explored.iter().map(|b| format!("{b:02x}")).collect();
            out += &format!("explored {hex}\n");
        }
        if let Some(w) = self.waypoint {
            out += &format!("waypoint {} {} {}\n", w.x, w.y, w.z);
        }
        if !self.liberated.is_empty() {
            let ids: Vec<String> = self.liberated.iter().map(|i| i.to_string()).collect();
            out += &format!("liberated {}\n", ids.join(" "));
        }
        out += &self.progress.to_lines();
        out
    }

    pub fn from_text(text: &str) -> Option<SaveGame> {
        let mut lines = text.lines();
        if lines.next()?.trim() != FORMAT {
            return None;
        }
        let mut s = SaveGame::default();
        for line in lines {
            let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
            let mut nums = rest.split_whitespace();
            let mut f = || nums.next().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            match key {
                "position" => s.position = Vec3::new(f(), f(), f()),
                "yaw" => s.yaw = f(),
                "health" => s.health = f(),
                "medkits" => s.medkits = f() as u32,
                "kills" => s.kills = f() as u32,
                "herbs" => s.herbs = f() as u32,
                "grenades" => s.grenades = f() as u32,
                "molotovs" => s.molotovs = f() as u32,
                "explosives" => {
                    s.c4 = f() as u32;
                    s.mines = f() as u32;
                }
                "arrows" => {
                    s.arrows = [f() as u32, f() as u32];
                }
                "hours" => s.hours = f(),
                "current" => s.current = rest.to_string(),
                "weapon" => {
                    let mut it = rest.splitn(3, ' ');
                    let mag = it.next()?.parse().ok()?;
                    let reserve = it.next()?.parse().ok()?;
                    s.weapons.push((it.next()?.to_string(), mag, reserve));
                }
                "attachments" => {
                    if let Some((bits, name)) = rest.split_once(' ') {
                        s.attachments.push((name.to_string(), bits.parse().unwrap_or(0)));
                    }
                }
                "paint" => {
                    if let Some((paint, name)) = rest.split_once(' ') {
                        s.paints.push((name.to_string(), paint.to_string()));
                    }
                }
                "removed" => s.removed.extend(rest.split_whitespace().filter_map(|id| u64::from_str_radix(id, 16).ok())),
                "explored" => {
                    s.explored = (0..rest.len() / 2).filter_map(|i| u8::from_str_radix(rest.get(i * 2..i * 2 + 2)?, 16).ok()).collect();
                }
                "waypoint" => s.waypoint = Some(Vec3::new(f(), f(), f())),
                "liberated" => s.liberated.extend(rest.split_whitespace().filter_map(|v| v.parse::<u32>().ok())),
                _ => {
                    s.progress.read_line(key, rest);
                }
            }
        }
        Some(s)
    }
}

#[derive(Resource, Default)]
pub struct SaveSlot {
    path: Option<PathBuf>,
    /// The save found at startup, offered as CONTINUE.
    pub existing: Option<SaveGame>,
    /// False for automated runs.
    enabled: bool,
    /// Whether this session has entered play (only then is there anything to save).
    played: bool,
    since_save: f32,
}

/// Restores `SaveSlot::existing` into the world (sent by the menu's CONTINUE).
#[derive(Message, Clone, Copy)]
pub struct ContinueGame;

/// The session went on from the save (the story then starts from its own).
#[derive(Resource, Default)]
pub struct SaveContinued(pub bool);

impl SaveSlot {
    /// Where the story's progress is kept, next to the save.
    pub fn story_path(&self) -> Option<PathBuf> {
        self.path.as_ref().filter(|_| self.enabled).map(|p| p.with_file_name("story.txt"))
    }
}

fn find_save(mut slot: ResMut<SaveSlot>, args: Res<Args>, info: Option<Res<CacheInfo>>) {
    let explicit = std::env::var_os("FC3_SAVE").map(PathBuf::from);
    slot.enabled = explicit.is_some() || (!args.automated() && !args.noclip);
    let Some(info) = info else { return };
    let path = explicit.unwrap_or_else(|| info.root.parent().unwrap_or(&info.root).join("save.txt"));
    if slot.enabled {
        slot.existing = std::fs::read_to_string(&path).ok().and_then(|t| SaveGame::from_text(&t));
    }
    slot.path = Some(path);
}

fn mark_played(mut slot: ResMut<SaveSlot>) {
    slot.played = true;
}

/// `--continue`: load the save on first entering play.
fn continue_from_args(args: Res<Args>, mut done: Local<bool>, mut requests: MessageWriter<ContinueGame>) {
    if args.continue_save && !*done {
        *done = true;
        requests.write(ContinueGame);
    }
}

#[derive(SystemParam)]
struct SaveSources<'w, 's> {
    players: Query<'w, 's, (&'static Transform, &'static Player, &'static Health, &'static PlayerStats, &'static Inventory)>,
    world: Res<'w, WorldState>,
    tod: Res<'w, TimeOfDay>,
    explored: Res<'w, crate::map::Explored>,
    waypoint: Res<'w, crate::map::Waypoint>,
    outposts: Res<'w, crate::outposts::Outposts>,
    progress: Res<'w, crate::progression::Progress>,
}

impl SaveSources<'_, '_> {
    fn snapshot(&self) -> Option<SaveGame> {
        let (t, p, h, stats, inv) = self.players.single().ok()?;
        if p.spawning {
            return None;
        }
        // Dead players come back at their respawn point.
        let (position, health) = if h.dead { (stats.spawn_point, h.max) } else { (t.translation, h.current) };
        let mut removed: Vec<u64> = self.world.removed.iter().copied().collect();
        removed.sort_unstable();
        Some(SaveGame {
            position,
            yaw: p.yaw,
            health,
            medkits: stats.medkits,
            kills: stats.kills,
            herbs: self.world.herbs,
            grenades: inv.grenades,
            molotovs: inv.molotovs,
            c4: inv.c4,
            arrows: inv.special_arrows,
            mines: inv.mines,
            hours: self.tod.hours,
            // A mounted gun isn't carried: the weapon held before it is.
            current: inv.defs[inv.slots[inv.mounted_from.unwrap_or(inv.current).min(inv.slots.len() - 1)].def].name.to_string(),
            weapons: inv
                .slots
                .iter()
                .filter(|s| inv.defs[s.def].class != crate::weapons::WeaponClass::Mounted)
                .map(|s| (inv.defs[s.def].name.to_string(), s.mag, s.reserve))
                .collect(),
            attachments: inv.attachments.iter().enumerate().filter(|(_, b)| **b != 0).map(|(d, b)| (inv.defs[d].name.to_string(), *b)).collect(),
            paints: inv
                .paints
                .iter()
                .enumerate()
                .filter_map(|(d, p)| Some((inv.defs[d].name.to_string(), crate::weapons::PAINT_NAMES.get((*p)? as usize)?.to_string())))
                .collect(),
            removed,
            explored: self.explored.to_bits(),
            waypoint: self.waypoint.0,
            liberated: self.outposts.liberated_ids(),
            progress: self.progress.clone(),
        })
    }
}

fn write_save(slot: &mut SaveSlot, sources: &SaveSources) {
    if !slot.enabled || !slot.played {
        return;
    }
    let (Some(path), Some(save)) = (slot.path.clone(), sources.snapshot()) else { return };
    let tmp = path.with_extension("tmp");
    let result = std::fs::write(&tmp, save.to_text()).and_then(|_| std::fs::rename(&tmp, &path));
    match result {
        Ok(()) => {
            slot.since_save = 0.0;
            slot.existing = Some(save);
        }
        Err(e) => warn!("could not save to {}: {e}", path.display()),
    }
}

fn save_now(mut slot: ResMut<SaveSlot>, sources: SaveSources) {
    write_save(&mut slot, &sources);
}

fn autosave(time: Res<Time<Real>>, state: Res<State<GameState>>, mut slot: ResMut<SaveSlot>, sources: SaveSources) {
    if *state.get() != GameState::Playing {
        return;
    }
    slot.since_save += time.delta_secs();
    if slot.since_save >= AUTOSAVE_SECS {
        write_save(&mut slot, &sources);
    }
}

fn save_on_exit(mut exits: MessageReader<AppExit>, mut slot: ResMut<SaveSlot>, sources: SaveSources) {
    if exits.read().count() > 0 {
        write_save(&mut slot, &sources);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn continue_game(
    mut continued: ResMut<SaveContinued>,
    mut requests: MessageReader<ContinueGame>,
    slot: Res<SaveSlot>,
    mut players: Query<(&mut Transform, &mut Player, &mut Health, &mut PlayerStats, &mut Inventory)>,
    mut world: ResMut<WorldState>,
    mut tod: ResMut<TimeOfDay>,
    mut explored: ResMut<crate::map::Explored>,
    mut waypoint: ResMut<crate::map::Waypoint>,
    mut outposts: ResMut<crate::outposts::Outposts>,
    mut progress: ResMut<crate::progression::Progress>,
    pickups: Query<(Entity, &Pickup)>,
    explosives: Query<(Entity, &Explosive)>,
    mut commands: Commands,
) {
    if requests.read().count() == 0 {
        return;
    }
    let Some(save) = &slot.existing else { return };
    continued.0 = true;
    let Ok((mut t, mut p, mut h, mut stats, mut inv)) = players.single_mut() else { return };
    t.translation = save.position + Vec3::Y * 0.5;
    t.rotation = Quat::from_rotation_y(save.yaw);
    p.yaw = save.yaw;
    p.pitch = 0.0;
    p.velocity = Vec3::ZERO;
    p.spawning = true;
    h.current = save.health.clamp(1.0, h.max);
    h.dead = false;
    stats.medkits = save.medkits;
    stats.kills = save.kills;
    stats.spawn_point = save.position;
    let slots: Vec<WeaponSlot> = save
        .weapons
        .iter()
        .filter_map(|(name, mag, reserve)| {
            let def = inv.defs.iter().position(|d| d.name == name)?;
            Some(WeaponSlot { def, mag: *mag, reserve: *reserve })
        })
        .collect();
    if !slots.is_empty() {
        inv.slots = slots;
        inv.slots.sort_by_key(|s| s.def);
        let current = inv.slots.iter().position(|s| inv.defs[s.def].name == save.current).unwrap_or(0);
        inv.current = current;
        inv.mounted_from = None;
    }
    inv.grenades = save.grenades;
    inv.molotovs = save.molotovs;
    inv.c4 = save.c4;
    inv.special_arrows = save.arrows;
    inv.mines = save.mines;
    for (name, paint) in &save.paints {
        let (Some(d), Some(p)) = (inv.defs.iter().position(|w| w.name == name), crate::weapons::PAINT_NAMES.iter().position(|n| n == paint)) else { continue };
        if inv.paints.len() <= d {
            inv.paints.resize(d + 1, None);
        }
        inv.paints[d] = Some(p as u8);
    }
    for (name, bits) in &save.attachments {
        let Some(d) = inv.defs.iter().position(|w| w.name == name) else { continue };
        for a in crate::weapons::Attachment::ALL {
            if bits & a.bit() != 0 {
                inv.attach(d, a);
            }
        }
    }
    inv.reloading = 0.0;
    world.herbs = save.herbs;
    world.removed = save.removed.iter().copied().collect();
    tod.hours = save.hours;
    if !save.explored.is_empty() {
        explored.set_bits(&save.explored);
    }
    waypoint.0 = save.waypoint;
    outposts.set_liberated(&save.liberated);
    *progress = save.progress.clone();
    // Pickups and props already streamed in around the start point.
    for (e, pickup) in &pickups {
        if world.removed.contains(&pickup.id) {
            if let Some(v) = pickup.visual {
                commands.entity(v).try_despawn();
            }
            commands.entity(e).try_despawn();
        }
    }
    for (e, ex) in &explosives {
        if world.removed.contains(&ex.id) {
            commands.entity(e).try_despawn();
        }
    }
    info!("continued from save at {:?}", save.position);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_roundtrip() {
        let s = SaveGame {
            position: Vec3::new(1.5, 120.0, -3.25),
            yaw: 0.5,
            health: 75.0,
            medkits: 2,
            kills: 9,
            herbs: 1,
            grenades: 3,
            molotovs: 1,
            c4: 2,
            arrows: [4, 1],
            mines: 3,
            hours: 14.5,
            current: "AK-74M Rifle".into(),
            weapons: vec![("MK23 Pistol".into(), 12, 40), ("AK-74M Rifle".into(), 30, 120)],
            removed: (0..40).map(|i| 0xABCDEF00 + i).collect(),
            explored: vec![0, 3, 255, 16],
            waypoint: Some(Vec3::new(10.0, 0.0, -20.5)),
            liberated: vec![0, 7, 31],
            attachments: vec![("AK-74M Rifle".into(), 5), ("MK23 Pistol".into(), 4)],
            paints: vec![("AK-74M Rifle".into(), "Tiger".into())],
            progress: {
                let mut p = crate::progression::Progress { xp: 4200, level: 4, skill_points: 2, money: 640, outposts: 3, ..Default::default() };
                p.skills.insert("tough_skin".into());
                p.upgrades.insert("wallet".into(), 1);
                p.skins.insert(crate::progression::Animal::Boar, 3);
                p
            },
        };
        assert_eq!(SaveGame::from_text(&s.to_text()), Some(s));
        assert_eq!(SaveGame::from_text("not a save"), None);
    }
}
