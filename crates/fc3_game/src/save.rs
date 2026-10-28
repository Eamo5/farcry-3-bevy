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
            .init_resource::<BackToMenu>()
            .add_message::<ContinueGame>()
            .add_message::<GameSaved>()
            .add_message::<SaveRequest>()
            .add_systems(OnExit(GameState::Loading), find_save)
            .add_systems(OnEnter(GameState::Playing), (mark_played, continue_from_args))
            .add_systems(OnEnter(GameState::Paused), save_now)
            .add_systems(Update, (continue_game, autosave))
            .add_systems(Update, keep_story_checkpoint.after(crate::story::StorySet))
            .add_systems(Last, save_on_exit);
    }
}

/// Everything restored by CONTINUE.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveGame {
    /// Bevy-space capsule centre (re-grounded near its saved height on load).
    pub position: Vec3,
    pub yaw: f32,
    pub health: f32,
    pub medkits: u32,
    pub craft_tutorial: Option<crate::progression::CraftTutorial>,
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
    /// Liberated outposts: by their flag poles (`taken`), and by their
    /// index in older outpost tables (`liberated`).
    pub liberated: crate::outposts::Taken,
    /// Weapon attachments: weapon name and attachment bits (bought, and the
    /// fitted sight's in the high half).
    pub attachments: Vec<(String, u32)>,
    /// Weapon paint jobs: weapon name and paint name.
    pub paints: Vec<(String, String)>,
    pub progress: crate::progression::Progress,
    /// The inventory a mission has put aside, to give back at its end.
    pub backup: Option<SavedBackup>,
}

/// The inventory a mission put aside (`BackupInventory`: CIT_030's for the
/// bow it hands the player against the Ink Monster), kept with a checkpoint
/// taken meanwhile: reloaded without it, the mission gave nothing back, and
/// the player went on with the bow alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavedBackup {
    pub current: String,
    pub weapons: Vec<(String, u32, u32)>,
    /// Grenades, molotovs, C4 and mines.
    pub explosives: [u32; 4],
    pub gadgets: Vec<String>,
}

impl SaveGame {
    pub fn to_text(&self) -> String {
        let p = self.position;
        let mut out = format!("{FORMAT}\nposition {} {} {}\nyaw {}\n", p.x, p.y, p.z, self.yaw);
        out += &format!("health {}\nmedkits {}\nkills {}\nherbs {}\n", self.health, self.medkits, self.kills, self.herbs);
        use crate::progression::CraftTutorial;
        if let Some(stage) = self.craft_tutorial {
            out += &match stage {
                CraftTutorial::Medicine(n) => format!("craft_tutorial medicine {n}\n"),
                CraftTutorial::Rucksack => "craft_tutorial rucksack\n".into(),
                CraftTutorial::Done => "craft_tutorial done\n".into(),
            };
        }
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
        if !self.liberated.flags.is_empty() {
            let ids: Vec<String> = self.liberated.flags.iter().map(|id| format!("{id:x}")).collect();
            out += &format!("taken {}\n", ids.join(" "));
        }
        if !self.liberated.legacy.is_empty() {
            let ids: Vec<String> = self.liberated.legacy.iter().map(|i| i.to_string()).collect();
            out += &format!("liberated {}\n", ids.join(" "));
        }
        if let Some(b) = &self.backup {
            let [g, m, c4, mines] = b.explosives;
            out += &format!("backup {g} {m} {c4} {mines} {}\n", b.current);
            for (name, mag, reserve) in &b.weapons {
                out += &format!("backup_weapon {mag} {reserve} {name}\n");
            }
            for gadget in &b.gadgets {
                out += &format!("backup_gadget {gadget}\n");
            }
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
                "craft_tutorial" => {
                    use crate::progression::CraftTutorial;
                    let mut words = rest.split_whitespace();
                    s.craft_tutorial = match words.next() {
                        Some("medicine") => words.next().and_then(|n| n.parse::<u8>().ok()).filter(|&n| n < 2).map(CraftTutorial::Medicine),
                        Some("rucksack") => Some(CraftTutorial::Rucksack),
                        Some("done") => Some(CraftTutorial::Done),
                        _ => None,
                    };
                }
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
                "taken" => s.liberated.flags.extend(rest.split_whitespace().filter_map(|v| u64::from_str_radix(v, 16).ok())),
                "liberated" => s.liberated.legacy.extend(rest.split_whitespace().filter_map(|v| v.parse::<u32>().ok())),
                "backup" => {
                    let mut it = rest.splitn(5, ' ');
                    let mut n = || it.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                    let explosives = [n(), n(), n(), n()];
                    let current = it.next().unwrap_or("").to_string();
                    let b = s.backup.get_or_insert_with(Default::default);
                    (b.explosives, b.current) = (explosives, current);
                }
                "backup_weapon" => {
                    let mut it = rest.splitn(3, ' ');
                    let mag = it.next()?.parse().ok()?;
                    let reserve = it.next()?.parse().ok()?;
                    s.backup.get_or_insert_with(Default::default).weapons.push((it.next()?.to_string(), mag, reserve));
                }
                "backup_gadget" => s.backup.get_or_insert_with(Default::default).gadgets.push(rest.trim().to_string()),
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

/// A save written that the player is shown (the original HUD's spinner): at
/// a story checkpoint, now and then, or asked for.
#[derive(Message, Clone, Copy)]
pub struct GameSaved {
    pub checkpoint: bool,
}

/// A save asked for (the pause menu's "Save").
#[derive(Message, Clone, Copy)]
pub struct SaveRequest;

/// Leaving for the main menu (the pause menu's "Quit"): nothing here unloads
/// a session (the world, the story's machine), so, once saved, the game
/// starts again at its menu (`BOOT_MENU`: no logo, no title) and this one
/// goes.
#[derive(Resource, Default)]
pub struct BackToMenu(pub bool);

/// Set on a game started again for its menu.
pub const BOOT_MENU: &str = "FC3_BOOT_MENU";

/// The session went on from the save (the story then starts from its own).
#[derive(Resource, Default)]
pub struct SaveContinued(pub bool);

impl SaveSlot {
    /// When the save was written (its file's time), if there is one.
    pub fn saved_at(&self) -> Option<std::time::SystemTime> {
        self.existing.as_ref()?;
        std::fs::metadata(self.path.as_ref()?).ok()?.modified().ok()
    }

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
fn continue_from_args(args: Res<Args>, mut done: Local<bool>, mut requests: MessageWriter<ContinueGame>, mut slot: ResMut<SaveSlot>) {
    if *done { return; }
    *done = true;
    if let Some(path) = std::env::var_os("FC3_STORY_LOAD").map(PathBuf::from) {
        let path = path.with_extension("game.txt");
        if let Some(save) = std::fs::read_to_string(&path).ok().and_then(|s| SaveGame::from_text(&s)) {
            info!("story checkpoint: restore matching inventory from {}", path.display());
            slot.existing = Some(save);
            requests.write(ContinueGame);
            return;
        }
        warn!("story checkpoint has no matching inventory snapshot at {}; this is a story-only test load", path.display());
    }
    if args.continue_save {
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
    pause_menu: Res<'w, crate::progression::PauseMenu>,
    scripted: Option<Res<'w, crate::story::ScriptedPlayer>>,
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
            craft_tutorial: self.pause_menu.tutorial.or_else(|| self.pause_menu.tutorial_completed.then_some(crate::progression::CraftTutorial::Done)),
            kills: stats.kills,
            herbs: self.world.herbs,
            grenades: inv.grenades,
            molotovs: inv.molotovs,
            c4: inv.c4,
            arrows: inv.special_arrows,
            mines: inv.mines,
            hours: self.tod.hours,
            // A mounted gun isn't carried: the weapon held before it is.
            current: inv.slots.get(inv.mounted_from.unwrap_or(inv.current)).and_then(|s| inv.defs.get(s.def))
                .map(|d| d.name.to_string()).unwrap_or_default(),
            // (Nor is a weapon lent while another is put by: a sniping
            // challenge's rifle, the last slot.)
            weapons: inv
                .slots
                .iter()
                .enumerate()
                .filter(|(i, s)| inv.defs[s.def].class != crate::weapons::WeaponClass::Mounted && !(inv.mounted_from.is_some() && *i + 1 == inv.slots.len()))
                .map(|(_, s)| s)
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
            liberated: self.outposts.taken(),
            progress: self.progress.clone(),
            backup: self.scripted.as_ref().and_then(|s| s.saved_backup(inv)),
        })
    }
}

/// Explicit automated checkpoints need the same inventory as their story
/// graph: otherwise a hunting checkpoint reloads with no ingredients and
/// sandbox starting weapons/medkits. Ordinary saves remain disabled in tests.
fn keep_story_checkpoint(world: &mut World) {
    if world.get_non_send::<crate::story::StoryHost>().is_none_or(|h| !h.checkpoint_game_pending && h.checkpoint_game_paths.is_empty()) { return; }
    let mut sources = bevy::ecs::system::SystemState::<SaveSources>::new(world);
    let Some(mut save) = sources.get(world).ok().and_then(|s| s.snapshot()) else {
        return;
    };
    let paths = std::mem::take(&mut world.non_send_mut::<crate::story::StoryHost>().checkpoint_game_paths);
    {
        let mut host = world.non_send_mut::<crate::story::StoryHost>();
        if let Some(progress) = host.checkpoint_progress.take() {
            save.progress = progress;
        }
        host.checkpoint_game = Some(save.clone());
        host.checkpoint_game_pending = false;
    }
    let text = save.to_text();
    for path in paths {
        if let Err(e) = std::fs::write(&path, &text) {
            warn!("could not keep checkpoint inventory at {}: {e}", path.display());
        }
    }
}

/// Writes the save; whether it was.
fn write_save(slot: &mut SaveSlot, sources: &SaveSources) -> bool {
    if !slot.enabled || !slot.played {
        return false;
    }
    let (Some(path), Some(save)) = (slot.path.clone(), sources.snapshot()) else { return false };
    let tmp = path.with_extension("tmp");
    let result = std::fs::write(&tmp, save.to_text()).and_then(|_| std::fs::rename(&tmp, &path));
    match result {
        Ok(()) => {
            slot.since_save = 0.0;
            slot.existing = Some(save);
            true
        }
        Err(e) => {
            warn!("could not save to {}: {e}", path.display());
            false
        }
    }
}

/// Whether the player is on a mission: then, as in the game, only its
/// checkpoints save ("The Save option is unavailable during missions.
/// Progress will be checkpointed automatically."), and leaving loses what
/// came after the last. Saving the place and the kit as they stood, with
/// the story's progress at its checkpoint, went on from the save with the
/// mission begun again around a player who wasn't at its start. (So too
/// while a script has saving off: `SetEnableSaveGame 0`.)
pub fn on_mission(host: Option<&crate::story::StoryHost>) -> bool {
    host.is_some_and(|h| h.save_disabled || h.story.mission_in_progress().is_some())
}

fn save_now(mut slot: ResMut<SaveSlot>, sources: SaveSources, host: Option<NonSend<crate::story::StoryHost>>) {
    if !on_mission(host.as_deref()) {
        write_save(&mut slot, &sources);
    }
}

/// Saves now and then in play, at each story checkpoint (the game as it
/// stands with the story's) and when asked; each told (`GameSaved`). On a
/// mission, only at its checkpoints (`on_mission`).
#[allow(clippy::too_many_arguments)]
fn autosave(
    time: Res<Time<Real>>,
    state: Res<State<GameState>>,
    mut slot: ResMut<SaveSlot>,
    sources: SaveSources,
    mut story: ResMut<crate::story::StoryUi>,
    mut requests: MessageReader<SaveRequest>,
    mut saved: MessageWriter<GameSaved>,
    host: Option<NonSend<crate::story::StoryHost>>,
) {
    let asked = requests.read().count() > 0;
    let checkpoint = std::mem::take(&mut story.checkpoint_reached);
    let mission = on_mission(host.as_deref());
    if checkpoint || (asked && !mission) {
        if write_save(&mut slot, &sources) {
            saved.write(GameSaved { checkpoint });
        }
        return;
    }
    if *state.get() != GameState::Playing || mission {
        return;
    }
    slot.since_save += time.delta_secs();
    if slot.since_save >= AUTOSAVE_SECS && write_save(&mut slot, &sources) {
        saved.write(GameSaved { checkpoint: false });
    }
}

fn save_on_exit(
    mut exits: MessageReader<AppExit>,
    mut slot: ResMut<SaveSlot>,
    sources: SaveSources,
    back: Res<BackToMenu>,
    args: Res<Args>,
    host: Option<NonSend<crate::story::StoryHost>>,
) {
    if exits.read().count() == 0 {
        return;
    }
    // (On a mission its last checkpoint stands.)
    if !on_mission(host.as_deref()) {
        write_save(&mut slot, &sources);
    }
    // (An automated run only says so: it would start itself again and again.)
    if back.0 && args.automated() {
        info!("back to the main menu (an automated run ends here)");
    } else if back.0 {
        start_at_menu();
    }
}

/// The game again, at its menu, as it was started (`--continue` aside: the
/// menu offers that).
fn start_at_menu() {
    let args = std::env::args_os().skip(1).filter(|a| a != "--continue");
    match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).args(args).env(BOOT_MENU, "1").spawn()) {
        Ok(_) => info!("back to the main menu"),
        Err(e) => warn!("could not start again for the main menu ({e})"),
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
    mut pause_menu: ResMut<crate::progression::PauseMenu>,
    pickups: Query<(Entity, &Pickup)>,
    explosives: Query<(Entity, &Explosive)>,
    mut commands: Commands,
    (host, mut scripted): (Option<NonSendMut<crate::story::StoryHost>>, Option<ResMut<crate::story::ScriptedPlayer>>),
    args: Option<Res<Args>>,
) {
    if requests.read().count() == 0 {
        return;
    }
    let Some(save) = &slot.existing else { return };
    if let Some(mut host) = host { host.checkpoint_game = Some(save.clone()); }
    pause_menu.tutorial = save.craft_tutorial;
    pause_menu.tutorial_completed = save.craft_tutorial == Some(crate::progression::CraftTutorial::Done);
    continued.0 = true;
    let Ok((mut t, mut p, mut h, mut stats, mut inv)) = players.single_mut() else { return };
    // (A test's `--pos` over a kept checkpoint: placed there instead, facing
    // its `--yaw`.)
    let placed = args.filter(|a| a.placed);
    let position = placed.as_ref().map_or(save.position, |a| a.start);
    let yaw = placed.as_ref().map_or(save.yaw, |a| a.yaw);
    t.translation = position;
    t.rotation = Quat::from_rotation_y(yaw);
    p.yaw = yaw;
    p.pitch = 0.0;
    p.velocity = Vec3::ZERO;
    p.spawning = true;
    p.keep_height = true;
    p.noclip = false;
    h.current = save.health.clamp(1.0, h.max);
    h.dead = false;
    stats.medkits = save.medkits;
    stats.kills = save.kills;
    stats.spawn_point = position;
    restore_inventory(&mut inv, save);
    if let Some(s) = scripted.as_mut() {
        s.load_backup(&inv, save.backup.as_ref());
    }
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
    info!("continued from save at {position:?}");
}

/// Replace carried state, including upgrades and transient weapon actions.
pub(crate) fn restore_inventory(inv: &mut Inventory, save: &SaveGame) {
    let base = crate::weapons::weapon_defs();
    for def in &mut inv.defs {
        if let Some(original) = base.iter().find(|d| d.name == def.name) { *def = original.clone(); }
    }
    inv.attachments = vec![0; inv.defs.len()];
    inv.paints = vec![None; inv.defs.len()];
    let slots: Vec<WeaponSlot> = save
        .weapons
        .iter()
        .filter_map(|(name, mag, reserve)| {
            let def = inv.defs.iter().position(|d| d.name == name)?;
            Some(WeaponSlot { def, mag: *mag, reserve: *reserve })
        })
        .collect();
    // An empty checkpoint means empty hands, not permission to keep the
    // sandbox loadout (or weapons acquired after the checkpoint).
    inv.slots = slots;
    inv.slots.sort_by_key(|s| s.def);
    inv.current = inv.slots.iter().position(|s| inv.defs[s.def].name == save.current).unwrap_or(0);
    inv.mounted_from = None;
    crate::weapons::settle_unarmed(inv);
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
        // (An old save's sight bit comes back as the weapon's usual sight.)
        for a in crate::weapons::Attachment::ALL.into_iter().chain([crate::weapons::Attachment::Optic]) {
            if bits & a.bit() != 0 {
                inv.attach(d, a);
            }
        }
        // Then the sight that was fitted.
        if let Some(a) = crate::weapons::Attachment::ALL.into_iter().find(|a| a.is_sight() && (bits >> 16) & a.bit() != 0) {
            inv.attach(d, a);
        }
    }
    inv.reloading = 0.0;
    inv.switching = 0.0;
    inv.cooldown = 0.0;
    inv.aiming = false;
    inv.aim_blend = 0.0;
    inv.auto_tail = 0;
    inv.recoil_kick = 0.0;
    inv.recoil_offset = 0.0;
    inv.spread_bloom = 0.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_checkpoint_keeps_inventory_and_handles_an_empty_weapon_list() {
        let mut world = World::new();
        world.init_resource::<WorldState>();
        world.insert_resource(TimeOfDay { hours: 13.0, speed: 0.0 });
        world.init_resource::<crate::map::Explored>();
        world.init_resource::<crate::map::Waypoint>();
        world.init_resource::<crate::outposts::Outposts>();
        world.init_resource::<crate::progression::PauseMenu>();
        let mut progress = crate::progression::Progress::default();
        progress.plants[crate::syringes::Plant::Arrowroot.index()] = 2;
        progress.skins.insert(crate::progression::Animal::Boar, 2);
        world.insert_resource(progress.clone());
        world.spawn((
            Transform::from_xyz(1.0, 2.0, 3.0),
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
                eye: 0.0,
                air: crate::player::AirMode::None,
                mantle: None,
                mounted: None,
                sliding: None,
                dragging: false,
            },
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
            Inventory {
                defs: crate::weapons::weapon_defs(),
                models: Vec::new(),
                slots: Vec::new(),
                current: 0,
                grenades: 0,
                molotovs: 0,
                c4: 0,
                mines: 0,
                cooldown: 0.0,
                reloading: 0.0,
                switching: 0.0,
                aiming: false,
                aim_blend: 0.0,
                recoil_kick: 0.0,
                spread_bloom: 0.0,
                shots_fired: 0,
                recoil_offset: 0.0,
                since_shot: 0.0,
                reserve_scale: 1.0,
                attachments: Vec::new(),
                attachment_models: Vec::new(),
                mounted_from: None,
                auto_tail: 0,
                arrow_kind: 0,
                special_arrows: [0; 2],
                paints: Vec::new(),
                paint_options: Vec::new(),
            },
        ));
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = fc3_story::Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        let mut host = crate::story::StoryHost::new(story, None);
        let directory = std::env::temp_dir().join("opencode");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("fc3-paired-checkpoint-test-{}.game.txt", std::process::id()));
        let second = directory.join(format!("fc3-paired-checkpoint-test-{}-second.game.txt", std::process::id()));
        host.checkpoint_game_paths = vec![path.clone(), second.clone()];
        world.insert_non_send(host);
        keep_story_checkpoint(&mut world);
        let kept = SaveGame::from_text(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(kept.medkits, 0);
        assert_eq!(kept.progress, progress);
        assert!(kept.current.is_empty() && kept.weapons.is_empty());
        assert_eq!(kept.position, Vec3::new(1.0, 2.0, 3.0));
        assert!(world.non_send::<crate::story::StoryHost>().checkpoint_game_paths.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), std::fs::read_to_string(&second).unwrap());
        assert_eq!(world.non_send::<crate::story::StoryHost>().checkpoint_game.as_ref().unwrap().to_text(), kept.to_text());
        // Ordinary play retains the same snapshot without requiring a test
        // output directory. Consuming supplies cannot mutate that checkpoint.
        world.non_send_mut::<crate::story::StoryHost>().checkpoint_game_pending = true;
        keep_story_checkpoint(&mut world);
        assert!(!world.non_send::<crate::story::StoryHost>().checkpoint_game_pending);
        let mut query = world.query::<&mut Inventory>();
        let mut inv = query.single_mut(&mut world).unwrap();
        let pistol = inv.defs.iter().position(|d| d.name == "1911").unwrap();
        inv.slots.push(WeaponSlot { def: pistol, mag: 1, reserve: 0 });
        inv.mounted_from = Some(0);
        inv.reloading = 2.0;
        inv.grenades = 3;
        restore_inventory(&mut inv, &kept);
        assert_eq!(inv.def().name, crate::weapons::UNARMED);
        assert_eq!(inv.slots.len(), 1);
        assert_eq!((inv.grenades, inv.reloading, inv.mounted_from), (0, 0.0, None));
        let armed = SaveGame { current: "1911".into(), weapons: vec![("1911".into(), 5, 41)], grenades: 2, ..kept.clone() };
        restore_inventory(&mut inv, &armed);
        assert_eq!((inv.def().name, inv.slot().mag, inv.slot().reserve, inv.grenades), ("1911", 5, 41, 2));
        restore_inventory(&mut inv, &armed);
        assert_eq!(inv.slots.len(), 1);
        let rifle = inv.defs.iter().position(|d| d.name == "AK-74M Rifle").unwrap();
        let original_mag = inv.defs[rifle].magazine;
        let mut upgraded = armed.clone();
        upgraded.current = "AK-74M Rifle".into();
        upgraded.weapons = vec![("AK-74M Rifle".into(), original_mag + 1, 17)];
        upgraded.attachments.push(("AK-74M Rifle".into(), crate::weapons::Attachment::ExtendedMag.bit()));
        upgraded.paints.push(("AK-74M Rifle".into(), crate::weapons::PAINT_NAMES[0].into()));
        restore_inventory(&mut inv, &upgraded);
        let extended_mag = inv.defs[rifle].magazine;
        assert!(extended_mag > original_mag);
        restore_inventory(&mut inv, &upgraded);
        assert_eq!(inv.defs[rifle].magazine, extended_mag);
        assert_eq!((inv.slot().mag, inv.slot().reserve), (original_mag + 1, 17));
        assert_eq!(inv.paints[rifle], Some(0));
        restore_inventory(&mut inv, &armed);
        assert_eq!(inv.defs[rifle].magazine, original_mag);
        assert_eq!(inv.paints[rifle], None);
        assert!(!inv.has_attachment(rifle, crate::weapons::Attachment::ExtendedMag));
        // Exercise the menu's Continue transaction as well as carried-state
        // restoration. Saving a capsule centre must not lift it on each load
        // or send an indoor save through the highest-roof spawn search.
        world.insert_resource(SaveSlot { existing: Some(kept.clone()), ..Default::default() });
        world.init_resource::<SaveContinued>();
        world.init_resource::<Messages<ContinueGame>>();
        world.resource_mut::<Messages<ContinueGame>>().write(ContinueGame);
        use bevy::ecs::system::RunSystemOnce;
        world.run_system_once(continue_game).unwrap();
        let mut restored = world.query::<(&Transform, &Player)>();
        let (at, player) = restored.single(&world).unwrap();
        assert_eq!(at.translation, kept.position);
        assert!(player.spawning && player.keep_height && !player.noclip);
        assert!(world.resource::<SaveContinued>().0);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(second).unwrap();
    }

    #[test]
    fn save_roundtrip() {
        let s = SaveGame {
            position: Vec3::new(1.5, 120.0, -3.25),
            yaw: 0.5,
            health: 75.0,
            medkits: 2,
            craft_tutorial: Some(crate::progression::CraftTutorial::Done),
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
            liberated: crate::outposts::Taken { flags: vec![0x1cd5_0000_1234_abcd, 7], legacy: vec![0, 31] },
            attachments: vec![("AK-74M Rifle".into(), 5), ("MK23 Pistol".into(), 4)],
            paints: vec![("AK-74M Rifle".into(), "Tiger".into())],
            progress: {
                let mut p = crate::progression::Progress { xp: 4200, level: 4, skill_points: 2, money: 640, outposts: 3, ..Default::default() };
                p.skills.insert("tough_skin".into());
                p.upgrades.insert("wallet".into(), 1);
                p.skins.insert(crate::progression::Animal::Boar, 3);
                p.recipes.insert(crate::crafting::RUCKSACK.into());
                p.plants[crate::syringes::Plant::SacredLotus.index()] = 2;
                p
            },
            backup: Some(SavedBackup {
                current: "AK-74M Rifle".into(),
                weapons: vec![("Machete".into(), 0, 0), ("AK-74M Rifle".into(), 30, 90)],
                explosives: [2, 1, 0, 3],
                gadgets: vec!["gadgets:Equipped/Tablet".into()],
            }),
        };
        assert_eq!(SaveGame::from_text(&s.to_text()), Some(s));
        assert_eq!(SaveGame::from_text("not a save"), None);
    }

    /// On a mission (one taken on, running) only its checkpoints save; off
    /// one, and once it is given up, the game saves as ever.
    #[test]
    fn a_mission_in_progress_saves_only_at_its_checkpoints() {
        let pack = || fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let host = crate::story::StoryHost::new(fc3_story::Story::new(pack(), fc3_story::data::StoryData::default()).unwrap(), None);
        assert!(!on_mission(Some(&host)));
        // (Taken on in a save, registered as the master graph goes.)
        let mut s = host.story.save();
        s.missions.push(("WIL_050_MAIN".into(), "Running".into(), true));
        s.current = Some("WIL_050_MAIN".into());
        let mut story = fc3_story::Story::new(pack(), fc3_story::data::StoryData::default()).unwrap();
        story.boot_saved(&s);
        let mut host = crate::story::StoryHost::new(story, None);
        host.story
            .domino()
            .lua()
            .load("CMissionManager.GetInstance():RegisterMission('wil_050_main.lua', '', 0, '', false, true, true, true, '')")
            .exec()
            .unwrap();
        assert!(on_mission(Some(&host)));
        assert!(host.story.abandon_mission());
        assert!(!on_mission(Some(&host)));
        assert!(!on_mission(None));
        // (Nor while a script has saving off.)
        host.save_disabled = true;
        assert!(on_mission(Some(&host)));
    }

    #[test]
    fn crafting_tutorial_stages_round_trip_and_old_saves_remain_compatible() {
        use crate::progression::CraftTutorial;
        for stage in [None, Some(CraftTutorial::Medicine(0)), Some(CraftTutorial::Medicine(1)), Some(CraftTutorial::Rucksack), Some(CraftTutorial::Done)] {
            let save = SaveGame { craft_tutorial: stage, ..default() };
            assert_eq!(SaveGame::from_text(&save.to_text()), Some(save));
        }
        assert_eq!(SaveGame::from_text("fc3save 1\nmedkits 1\n").unwrap().craft_tutorial, None);
        for value in ["medicine 255", "medicine -1", "wrong"] {
            assert_eq!(SaveGame::from_text(&format!("fc3save 1\ncraft_tutorial {value}\n")).unwrap().craft_tutorial, None);
        }
    }
}
