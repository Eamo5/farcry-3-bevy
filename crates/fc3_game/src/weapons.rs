//! Weapons: definitions, inventory, first-person view model, firing,
//! projectiles, explosions and simple effects.
//!
//! Models come from the converted cache; all stats here are this project's own
//! tuning.

use std::collections::HashMap;

use avian3d::prelude::*;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use fc3_formats::cache;

use crate::cache_assets::{cache_path, Fc3Model};
use crate::combat::{DamageEvent, DamageKind, Health};
use crate::data::CacheInfo;
use crate::player::{InputCaptured, Player, PlayerCamera};

pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Rng::seeded(0x9E37_79B9_7F4A_7C15))
            .init_resource::<PaintTable>()
            .init_resource::<TestScript>()
            .init_resource::<ScopeSway>()
            .add_systems(Update, (fire_test_setup, fire_test_log, scope_sway, test_ammo))
            .add_message::<Explosion>()
            .add_message::<HitMarker>()
            .add_systems(Startup, load_weapon_stats)
            .add_systems(OnExit(crate::menu::GameState::Loading), (setup_effects, give_player_loadout).chain())
            .add_systems(
                Update,
                (
                    switch_weapon,
                    reload,
                    fire,
                    throw_grenade,
                    (update_view_model, spawn_view_model_parts).in_set(ViewModelSystems),
                    (load_paints, find_paint_options).chain(),
                    update_projectiles,
                    explode,
                    fade_effects,
                )
                    .chain(),
            );
    }
}

// ------------------------------------------------------------------ data ---

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WeaponClass {
    Melee,
    Pistol,
    Smg,
    Rifle,
    Shotgun,
    Sniper,
    Lmg,
    Launcher,
    /// Silent; fires arrows that drop and stick.
    Bow,
    /// A short stream of fire that sets things alight.
    Flamethrower,
    /// Lobs grenades that burst on impact.
    GrenadeLauncher,
    /// A mounted machine gun, only while manning one (never carried).
    Mounted,
}

#[derive(Clone, Debug)]
pub struct WeaponDef {
    pub name: &'static str,
    pub model: &'static str,
    pub class: WeaponClass,
    pub damage: f32,
    /// Seconds between shots.
    pub interval: f32,
    pub automatic: bool,
    pub magazine: u32,
    pub max_reserve: u32,
    pub pellets: u32,
    /// Hip-fire cone half-angle in degrees (ADS uses a fraction).
    pub spread: f32,
    pub range: f32,
    pub reload_time: f32,
    pub ads_fov: f32,
    /// Upward view kick per shot, radians.
    pub recoil: f32,
    /// Hand position of the view model relative to the camera.
    pub hold: Vec3,
    /// Quiet: little noise for enemies to hear, no flash.
    pub silenced: bool,
    /// Seconds to put it away and to bring it out (a swap is the one's
    /// holster and the other's draw).
    pub holster: f32,
    pub draw: f32,
    /// Damage over range, the game's bands (from, to metres, multiplier at
    /// each end); none keeps it whole.
    pub ranges: Vec<[f32; 4]>,
    /// The game's spread by stance (else `spread` and its bloom).
    pub spread_table: Option<fc3_formats::weapons::Spread>,
    /// The game's aiming down the sights: its FOV (put in `ads_fov`), how
    /// long the sights take to come up, and how much slower the player moves
    /// and turns aimed.
    pub aim: Option<fc3_formats::weapons::Aim>,
    /// The game's attachments for it (their gadgets' modifiers), and how
    /// far a suppressed shot is heard (times the open one's).
    pub gadgets: Vec<fc3_formats::weapons::Gadget>,
    pub noise: f32,
}

const MODEL_ROOT: &str = r"graphics\__fc3_graphics\_common\weapons\";

impl WeaponDef {
    /// The weapon's model as the archives name it.
    pub fn model_path(&self) -> String {
        format!("{MODEL_ROOT}{}", self.model).to_ascii_lowercase()
    }

    /// The cone a shot strays within (degrees off the aim) with the game's
    /// spread: the stance's (hip, crouched, in the air) from its base to its
    /// most as shots follow each other (`bloom`, 0 to 1), toward the aimed
    /// one as the sights come up, wider on the move.
    pub fn spread_cone(&self, aim: f32, crouched: bool, airborne: bool, bloom: f32, moving: f32) -> Option<f32> {
        let s = self.spread_table?;
        let (hip, aimed) = if airborne {
            (s.jumping, s.jumping)
        } else if crouched {
            (s.crouched, s.crouched_aimed)
        } else {
            (s.hip, s.aimed)
        };
        let at = |r: [f32; 2]| r[0] + (r[1] - r[0]) * bloom.clamp(0.0, 1.0);
        let cone = at(hip) + (at(aimed) - at(hip)) * aim.clamp(0.0, 1.0);
        Some(cone * (1.0 + (s.moving - 1.0) * moving.clamp(0.0, 1.0)))
    }

    /// How much of the way to its most spread a shot takes it, and how fast
    /// the spread settles back (both of the 0 to 1 bloom), with the game's
    /// spread.
    fn spread_steps(&self) -> Option<(f32, f32)> {
        let s = self.spread_table?;
        Some((1.0 / s.shots.max(1.0), s.contraction / (s.hip[1] - s.hip[0]).max(0.25)))
    }

    /// The damage multiplier at a distance.
    pub fn range_scale(&self, distance: f32) -> f32 {
        fc3_formats::weapons::WeaponStats { ranges: self.ranges.clone(), ..Default::default() }.range_scale(distance)
    }
}

/// The game's own handling for the player's weapons (`fc3_formats::weapons`),
/// once read from the cache: [`weapon_defs`] applies it.
static WEAPON_STATS: std::sync::OnceLock<Vec<fc3_formats::weapons::WeaponStats>> = std::sync::OnceLock::new();

fn load_weapon_stats(info: Option<Res<crate::data::CacheInfo>>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::weapons::STATS_REL)).map_err(anyhow::Error::from).and_then(|t| fc3_formats::weapons::from_text(&t)) {
        Ok(stats) => {
            let n = apply_stats(&mut base_weapon_defs(), &stats);
            info!("weapons: the game's handling for {n} weapons");
            let _ = WEAPON_STATS.set(stats);
        }
        Err(e) => warn!("weapons: no handling table ({e:#}); re-run the converter"),
    }
}

/// Puts the game's handling on the weapons whose model it names: damage
/// a bullet (or the bow's arrow) and its fall-off with range, fire rate,
/// magazine, reload time, pellets and silence (the launchers and
/// flamethrower keep their own damage; mounted guns their endless belts).
/// Returns how many matched.
pub fn apply_stats(defs: &mut [WeaponDef], stats: &[fc3_formats::weapons::WeaponStats]) -> usize {
    use WeaponClass::*;
    let mut n = 0;
    for d in defs.iter_mut().filter(|d| !d.model.is_empty() && d.class != Melee) {
        let key = fc3_formats::weapons::model_key(&d.model_path());
        let Some(s) = stats.iter().find(|s| fc3_formats::weapons::model_key(&s.model) == key) else { continue };
        n += 1;
        if s.damage > 0.0 && !matches!(d.class, Launcher | GrenadeLauncher | Flamethrower) {
            d.damage = s.damage;
        }
        d.ranges = s.ranges.clone();
        d.spread_table = s.spread;
        d.aim = s.aim;
        if let Some(a) = s.aim {
            d.ads_fov = a.fov;
        }
        d.gadgets = s.gadgets.clone();
        if s.fire_rate > 0.0 {
            d.interval = 60.0 / s.fire_rate;
        }
        if s.magazine > 0 && d.class != Mounted {
            d.magazine = s.magazine;
        }
        if s.reload > 0.0 {
            d.reload_time = s.reload;
        }
        if s.pellets > 1 {
            d.pellets = s.pellets;
        }
        if s.draw > 0.0 {
            d.draw = s.draw;
        }
        if s.holster > 0.0 {
            d.holster = s.holster;
        }
        d.silenced = s.silent;
    }
    n
}

/// The weapons, with the game's handling once it is loaded.
pub fn weapon_defs() -> Vec<WeaponDef> {
    let mut defs = base_weapon_defs();
    if let Some(stats) = WEAPON_STATS.get() {
        apply_stats(&mut defs, stats);
    }
    defs
}

fn base_weapon_defs() -> Vec<WeaponDef> {
    use WeaponClass::*;
    let w = |name, model, class, damage, rpm: f32, automatic, magazine, max_reserve, spread, reload_time, ads_fov, recoil, hold| WeaponDef {
        name,
        model,
        class,
        damage,
        interval: 60.0 / rpm,
        automatic,
        magazine,
        max_reserve,
        pellets: 1,
        spread,
        range: 400.0,
        reload_time,
        ads_fov,
        recoil,
        hold,
        silenced: false,
        holster: 0.1,
        draw: 0.25,
        ranges: Vec::new(),
        spread_table: None,
        aim: None,
        gadgets: Vec::new(),
        noise: 1.0,
    };
    // Hand positions by kind of weapon.
    let pistol = Vec3::new(0.16, -0.17, -0.42);
    let smg = Vec3::new(0.14, -0.17, -0.3);
    let rifle = Vec3::new(0.14, -0.17, -0.27);
    let shotgun = Vec3::new(0.14, -0.18, -0.27);
    let sniper = Vec3::new(0.14, -0.17, -0.24);
    let lmg = Vec3::new(0.15, -0.2, -0.28);
    vec![
        WeaponDef { range: 2.4, ..w("Machete", r"machete\machete.xbg", Melee, 85.0, 90.0, false, 0, 0, 0.0, 0.0, 75.0, 0.0, Vec3::new(0.22, -0.32, -0.38)) },
        w("MK23 Pistol", r"mk23\mk23.xbg", Pistol, 34.0, 330.0, false, 12, 72, 0.9, 1.5, 60.0, 0.035, Vec3::new(0.16, -0.17, -0.42)),
        w("MP5 SMG", r"mp5\mp5.xbg", Smg, 21.0, 800.0, true, 30, 180, 1.6, 2.1, 58.0, 0.012, Vec3::new(0.14, -0.17, -0.3)),
        w("AK-74M Rifle", r"ak74m\ak74m.xbg", Rifle, 31.0, 640.0, true, 30, 210, 1.1, 2.4, 55.0, 0.018, Vec3::new(0.14, -0.17, -0.27)),
        WeaponDef {
            pellets: 8,
            range: 60.0,
            ..w("870 Shotgun", r"870shotgun\870shotgun.xbg", Shotgun, 15.0, 70.0, false, 6, 36, 4.5, 3.4, 62.0, 0.08, Vec3::new(0.14, -0.18, -0.27))
        },
        w("700 Sniper", r"700sniperrifle\700sniperrifle.xbg", Sniper, 140.0, 48.0, false, 5, 35, 3.0, 3.2, 18.0, 0.07, Vec3::new(0.14, -0.17, -0.24)),
        w("PKM LMG", r"pkm\pkm.xbg", Lmg, 33.0, 620.0, true, 100, 300, 1.9, 4.6, 55.0, 0.016, Vec3::new(0.15, -0.2, -0.28)),
        WeaponDef { range: 300.0, ..w("RPG-7", r"rpg7\rpg7.xbg", Launcher, 260.0, 30.0, false, 1, 5, 0.6, 3.0, 60.0, 0.06, Vec3::new(0.16, -0.12, 0.05)) },
        // Handguns.
        w("1911", r"kimber\kimber.xbg", Pistol, 38.0, 300.0, false, 7, 70, 0.9, 1.4, 60.0, 0.04, pistol),
        WeaponDef { silenced: true, ..w("6P9", r"6p9\6p9.xbg", Pistol, 30.0, 330.0, false, 8, 72, 1.0, 1.5, 60.0, 0.03, pistol) },
        w("D50", r"deserteagle\desert_eagle.xbg", Pistol, 70.0, 160.0, false, 7, 49, 1.2, 1.7, 58.0, 0.085, pistol),
        // Submachine guns.
        WeaponDef { silenced: true, ..w("A2000", r"agram2000\agram2000.xbg", Smg, 19.0, 750.0, true, 32, 192, 1.7, 2.2, 58.0, 0.011, smg) },
        w("Vector .45 ACP", r"kriss\kriss.xbg", Smg, 20.0, 1100.0, true, 25, 200, 1.5, 2.0, 58.0, 0.009, smg),
        w("Skorpion", r"vz61\vz61.xbg", Smg, 17.0, 850.0, true, 20, 160, 1.9, 1.9, 60.0, 0.012, smg),
        w("PP-19 Bizon", r"bison\bison.xbg", Smg, 19.0, 700.0, true, 64, 256, 1.7, 2.6, 58.0, 0.011, smg),
        // Assault rifles.
        w("F1", r"famas\famas.xbg", Rifle, 27.0, 900.0, true, 30, 210, 1.2, 2.5, 55.0, 0.015, rifle),
        w("P416", r"p416\p416.xbg", Rifle, 30.0, 750.0, true, 30, 210, 1.0, 2.3, 54.0, 0.016, rifle),
        w("MS16", r"mk16\mk16.xbg", Rifle, 42.0, 420.0, false, 20, 160, 0.8, 2.3, 50.0, 0.022, rifle),
        w("ACE", r"galilace52\galilace52.xbg", Rifle, 32.0, 650.0, true, 35, 245, 1.1, 2.5, 55.0, 0.018, rifle),
        w("STG-90", r"sg551\sg551.xbg", Rifle, 30.0, 700.0, true, 30, 210, 1.0, 2.4, 52.0, 0.016, rifle),
        // Shotguns.
        WeaponDef { pellets: 8, range: 55.0, ..w("1887", r"1887\1887.xbg", Shotgun, 17.0, 55.0, false, 5, 30, 4.8, 3.6, 62.0, 0.09, shotgun) },
        WeaponDef { pellets: 8, range: 60.0, ..w("SPAS-12", r"spas12\spas12.xbg", Shotgun, 14.0, 130.0, false, 8, 40, 4.2, 3.8, 62.0, 0.07, shotgun) },
        // Sniper rifles.
        w("SVD", r"svd\svd.xbg", Sniper, 105.0, 120.0, false, 10, 50, 2.6, 3.0, 20.0, 0.05, sniper),
        w("Z93", r"m93\m93.xbg", Sniper, 230.0, 40.0, false, 5, 25, 3.2, 3.6, 16.0, 0.1, sniper),
        // Light machine guns.
        w("U100", r"u100\u100.xbg", Lmg, 30.0, 700.0, true, 100, 300, 1.9, 4.8, 55.0, 0.015, lmg),
        w("MKG", r"lmg\minimi.xbg", Lmg, 31.0, 750.0, true, 100, 300, 2.0, 4.8, 55.0, 0.015, lmg),
        // Specials.
        WeaponDef { range: 200.0, ..w("GL-94", r"gm94grenadelauncher\gm94.xbg", GrenadeLauncher, 170.0, 55.0, false, 3, 12, 0.8, 3.2, 58.0, 0.06, shotgun) },
        WeaponDef { range: 11.0, ..w("Flamethrower", r"flamethrower\flamethrower.xbg", Flamethrower, 5.0, 1000.0, true, 100, 400, 0.0, 3.0, 70.0, 0.0, lmg) },
        WeaponDef {
            silenced: true,
            range: 200.0,
            ..w("Recurve Bow", r"bow\bow2.xbg", Bow, 160.0, 70.0, false, 1, 24, 0.3, 0.6, 50.0, 0.0, Vec3::new(-0.2, -0.02, -0.8))
        },
        // Mounted guns (see `turrets`): the hold is the gun's place in front
        // of the view, its grips where the arms' clips put the hands.
        WeaponDef { range: 600.0, ..w("DShK", r"dshk\dshk.xbg", Mounted, 75.0, 600.0, true, 999, 0, 1.0, 0.0, 50.0, 0.01, Vec3::new(0.0, -0.255, -0.79)) },
        WeaponDef { range: 500.0, ..w("M249", r"lmg\minimi.xbg", Mounted, 36.0, 750.0, true, 999, 0, 1.3, 0.0, 50.0, 0.007, Vec3::new(0.0, -0.175, -0.66)) },
        // Empty hands, when a mission has taken everything (not a weapon of
        // its own: it goes as soon as there is one).
        WeaponDef { range: 1.8, ..w(UNARMED, "", Melee, 20.0, 90.0, false, 0, 0, 0.0, 0.0, 75.0, 0.0, Vec3::new(0.22, -0.32, -0.38)) },
    ]
}

/// The empty-hands entry (see `settle_unarmed`).
pub const UNARMED: &str = "Unarmed";

/// Empty hands when there is nothing else, and not otherwise.
pub fn settle_unarmed(inv: &mut Inventory) {
    let Some(unarmed) = inv.defs.iter().position(|d| d.name == UNARMED) else { return };
    if inv.slots.iter().any(|s| s.def != unarmed) {
        inv.slots.retain(|s| s.def != unarmed);
    } else if inv.slots.is_empty() {
        inv.slots.push(WeaponSlot { def: unarmed, mag: 0, reserve: 0 });
    }
    inv.current = inv.current.min(inv.slots.len().saturating_sub(1));
}

pub const GRENADE_MODEL: &str = r"graphics\__fc3_graphics\_common\weapons\grenade\frag_grenade.xbg";
pub const ROCKET_MODEL: &str = r"graphics\__fc3_graphics\_common\weapons\rpg7\rpg7_rocket.xbg";
pub const ARROW_MODEL: &str = r"graphics\__fc3_graphics\_common\weapons\bow\arrow.xbg";
pub const MOLOTOV_MODEL: &str = r"graphics\__fc3_graphics\_common\weapons\molotov\molotov.xbg";
/// Launched grenades and rockets arm this far from the muzzle (closer, they bounce off).
const ARMING_DISTANCE: f32 = 5.0;

/// Molotovs carried at most.
pub const MOLOTOV_CAP: u32 = 4;

pub fn model_handle(server: &AssetServer, path: &str) -> Handle<Fc3Model> {
    server.load(cache_path(&cache::model_rel(path)))
}

#[derive(Clone)]
pub struct WeaponSlot {
    pub def: usize,
    pub mag: u32,
    pub reserve: u32,
}

/// The player's weapons and ammo.
#[derive(Component)]
pub struct Inventory {
    pub defs: Vec<WeaponDef>,
    pub models: Vec<Handle<Fc3Model>>,
    pub slots: Vec<WeaponSlot>,
    pub current: usize,
    pub grenades: u32,
    pub molotovs: u32,
    /// C4 charges and proximity mines carried.
    pub c4: u32,
    pub mines: u32,
    pub cooldown: f32,
    pub reloading: f32,
    /// Seconds into the current switch animation (lowering then raising).
    pub switching: f32,
    pub aiming: bool,
    /// 0 = hip, 1 = fully aimed.
    pub aim_blend: f32,
    pub recoil_kick: f32,
    pub spread_bloom: f32,
    pub shots_fired: u32,
    /// View pitch added by recoil that hasn't been recovered yet (radians).
    pub recoil_offset: f32,
    /// Seconds since the last shot.
    pub since_shot: f32,
    /// Reserve ammo capacity multiplier (ammo pouch upgrades).
    pub reserve_scale: f32,
    /// Attachments for each weapon kind (by def): the bought ones'
    /// `Attachment` bits in the low half, the fitted sight's in the high.
    pub attachments: Vec<u32>,
    /// Attachment models (suppressors and sights), by `Attachment::model`.
    pub attachment_models: Vec<Handle<Fc3Model>>,
    /// On a mounted gun (the last slot while on it): the slot to go back to.
    pub mounted_from: Option<usize>,
    /// An automatic weapon's stop event (the tail and its echo), due once the
    /// trigger is let go.
    pub auto_tail: u32,
    /// The bow's special arrows: the kind nocked (`ARROW_NAMES` index, 0
    /// for plain) and how many fire and explosive arrows are carried.
    pub arrow_kind: u8,
    pub special_arrows: [u32; 2],
    /// The paint each weapon kind wears (`PAINT_NAMES` index), by def.
    pub paints: Vec<Option<u8>>,
    /// The paints each weapon kind's model can take (bits of `PAINT_NAMES`),
    /// by def; filled in once the models load.
    pub paint_options: Vec<u8>,
}

/// The bow's arrows: plain, fire (sets alight where it lands) and explosive.
pub const ARROW_NAMES: [&str; 3] = ["Arrow", "Fire arrow", "Explosive arrow"];
/// Special arrows carried, of each kind.
pub const ARROW_CAP: u32 = 10;

impl Inventory {
    /// The next arrow kind with arrows left, after the one nocked (plain
    /// arrows always).
    pub fn next_arrow(&self) -> u8 {
        (1..=ARROW_NAMES.len() as u8)
            .map(|k| (self.arrow_kind + k) % ARROW_NAMES.len() as u8)
            .find(|&k| k == 0 || self.special_arrows[k as usize - 1] > 0)
            .unwrap_or(0)
    }
}

/// Paint jobs, as the converter names them (see `fc3_convert::PAINT_PATTERNS`).
pub const PAINT_NAMES: [&str; 4] = ["Tribal", "Urban", "Tiger", "Desert"];

/// The paint table from the cache: each material that can be painted, with
/// its painted versions (`PAINT_NAMES` index, cache-relative material).
#[derive(Resource, Default)]
pub struct PaintTable(HashMap<String, Vec<(u8, String)>>);

fn load_paints(info: Option<Res<CacheInfo>>, mut table: ResMut<PaintTable>, mut done: Local<bool>) {
    let Some(info) = info else { return };
    if *done {
        return;
    }
    *done = true;
    let Ok(text) = std::fs::read_to_string(info.root.join(fc3_formats::cache::PAINTS_REL)) else { return };
    for p in fc3_formats::cache::paints_from_text(&text) {
        if let Some(i) = PAINT_NAMES.iter().position(|n| *n == p.name) {
            table.0.entry(p.base).or_default().push((i as u8, p.variant));
        }
    }
}

/// The painted version of a model part's material, if it has one in `paint`.
fn painted(server: &AssetServer, table: &PaintTable, material: &Handle<StandardMaterial>, paint: u8) -> Option<String> {
    let path = server.get_path(material)?;
    let rel = path.path().to_string_lossy().replace('\\', "/");
    table.0.get(&rel)?.iter().find(|(p, _)| *p == paint).map(|(_, v)| v.clone())
}

/// Works out which paints each weapon kind can take, once its model is in.
fn find_paint_options(server: Res<AssetServer>, table: Res<PaintTable>, models: Res<Assets<Fc3Model>>, mut players: Query<&mut Inventory>) {
    if table.0.is_empty() {
        return;
    }
    for mut inv in &mut players {
        let n = inv.defs.len();
        if inv.paint_options.len() != n {
            inv.paint_options = vec![0; n];
            inv.paints.resize(n, None);
        }
        for d in 0..inv.defs.len() {
            if inv.paint_options[d] != 0 {
                continue;
            }
            let Some(model) = models.get(&inv.models[d]) else { continue };
            let parts = model.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]);
            let mut bits = 0u8;
            for part in parts {
                for p in 0..PAINT_NAMES.len() as u8 {
                    if painted(&server, &table, &part.material, p).is_some() {
                        bits |= 1 << p;
                    }
                }
            }
            if bits != 0 {
                inv.paint_options[d] = bits;
            }
        }
        // FC3_PAINT=<name> (tests) paints the weapon in hand, if it can take it.
        if let Some(p) = std::env::var("FC3_PAINT").ok().and_then(|n| PAINT_NAMES.iter().position(|x| x.eq_ignore_ascii_case(&n))) {
            let d = inv.slot().def;
            if inv.paint_options[d] & (1 << p) != 0 && inv.paints[d].is_none() {
                inv.paints[d] = Some(p as u8);
            }
        }
    }
}

/// Weapon attachments from the shop. They stay with the weapon kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Attachment {
    Suppressor,
    /// The sight a weapon showed as before the shop's own sights (old
    /// saves): its class's usual one.
    Optic,
    ExtendedMag,
    // The sights the game's shop sells (one fitted at a time; the others
    // bought stay in hand to swap back to), and the bow's longer barrel.
    ReflexA,
    ReflexB,
    Optical,
    Acog,
    IronSight,
    Scope,
    IlluminatedScope,
    Barrel,
}

/// Attachment models under the weapons folder.
const ATTACHMENT_MODELS: [&str; 5] =
    [r"_shared\silencer_handgun.xbg", r"_shared\silencer_smg.xbg", r"_shared\silencer_sniper.xbg", r"_shared\reflex_a.xbg", r"_shared\acog.xbg"];
const OPTICAL_MODEL: &str = r"_shared\optical.xbg";
const REFLEX_B_MODEL: &str = r"_shared\reflex_b.xbg";

/// Weapons whose models have no mount for a sight or suppressor.
const NO_OPTIC: [&str; 3] = ["mk23", "6p9", "1887"];
const NO_SUPPRESSOR: [&str; 1] = ["mk23"];

impl Attachment {
    /// What the shop can sell (the old `Optic` only comes from saves).
    pub const ALL: [Attachment; 10] = [
        Attachment::Suppressor,
        Attachment::Barrel,
        Attachment::ExtendedMag,
        Attachment::ReflexA,
        Attachment::ReflexB,
        Attachment::Optical,
        Attachment::Acog,
        Attachment::IronSight,
        Attachment::Scope,
        Attachment::IlluminatedScope,
    ];

    /// Its bit among a weapon's attachments (bought ones in the low half; the
    /// sight fitted in the high half, see `Inventory::attachments`).
    pub fn bit(self) -> u32 {
        match self {
            Attachment::Suppressor => 1,
            Attachment::Optic => 2,
            Attachment::ExtendedMag => 4,
            Attachment::ReflexA => 8,
            Attachment::ReflexB => 16,
            Attachment::Optical => 32,
            Attachment::Acog => 64,
            Attachment::IronSight => 128,
            Attachment::Scope => 256,
            Attachment::IlluminatedScope => 512,
            Attachment::Barrel => 1024,
        }
    }

    /// A sight (one fitted at a time).
    pub fn is_sight(self) -> bool {
        !matches!(self, Attachment::Suppressor | Attachment::ExtendedMag | Attachment::Barrel)
    }

    /// The sight an old save's `Optic` stands for on this weapon: a sniper's
    /// high-power scope, a rifle's ACOG (or optical sight), else reflex A.
    fn usual_sight(d: &WeaponDef) -> Attachment {
        let candidates: &[Attachment] = match d.class {
            WeaponClass::Sniper => &[Attachment::Scope],
            WeaponClass::Rifle => &[Attachment::Acog, Attachment::Optical, Attachment::ReflexA],
            _ => &[Attachment::ReflexA],
        };
        candidates.iter().copied().find(|a| a.fits(d)).unwrap_or(candidates[0])
    }

    /// Whether a weapon takes this attachment (as in the game: suppressors on
    /// handguns, SMGs, rifles and snipers; sights on most guns; bigger
    /// magazines on everything that has one).
    pub fn fits(self, d: &WeaponDef) -> bool {
        use WeaponClass::*;
        // A weapon the game's shop knows sells only the attachments it lists.
        if let Some(stats) = WEAPON_STATS.get() {
            let key = fc3_formats::weapons::model_key(&d.model_path());
            if stats.iter().any(|s| fc3_formats::weapons::model_key(&s.model) == key) {
                return self.offered(d, stats).is_some();
            }
        }
        let folder = d.model.split('\\').next().unwrap_or("");
        let sight = matches!(d.class, Pistol | Smg | Rifle | Shotgun | Sniper | Lmg) && !NO_OPTIC.contains(&folder);
        match self {
            Attachment::Suppressor => matches!(d.class, Pistol | Smg | Rifle | Sniper) && !d.silenced && !NO_SUPPRESSOR.contains(&folder),
            Attachment::Optic => sight,
            Attachment::ExtendedMag => matches!(d.class, Pistol | Smg | Rifle | Shotgun | Sniper | Lmg) && d.magazine > 1,
            // (Without the shop's list, its class's usual sight only.)
            Attachment::Scope => sight && d.class == Sniper,
            Attachment::Acog => sight && d.class == Rifle,
            Attachment::ReflexA => sight && !matches!(d.class, Sniper | Rifle),
            _ => false,
        }
    }

    pub fn name(self, d: &WeaponDef) -> &'static str {
        match self {
            Attachment::Suppressor => "Suppressor",
            Attachment::Optic => Attachment::usual_sight(d).name(d),
            Attachment::ExtendedMag => "Extended magazine",
            Attachment::ReflexA => "Reflex sight",
            Attachment::ReflexB => "Red-dot sight",
            Attachment::Optical => "Optical sight",
            Attachment::Acog => "ACOG sight",
            Attachment::IronSight => "Illuminated iron sights",
            Attachment::Scope => "High-power scope",
            Attachment::IlluminatedScope => "Illuminated scope",
            Attachment::Barrel => "Barrel extension",
        }
    }

    pub fn price(self, d: &WeaponDef) -> u32 {
        // The shop's own price for this weapon's attachment of the kind, where
        // the game sells one: a suppressor, the larger magazine, or the sight
        // this one shows as (a sniper's scope, a rifle's ACOG, a reflex).
        if let Some(cost) = WEAPON_STATS.get().and_then(|stats| self.offered(d, stats)) {
            return cost;
        }
        match (self, d.class) {
            (Attachment::Suppressor, WeaponClass::Sniper) => 450,
            (Attachment::Suppressor, _) => 300,
            (Attachment::ExtendedMag, _) => 250,
            (_, WeaponClass::Sniper | WeaponClass::Rifle) => 350,
            _ => 200,
        }
    }

    fn offered(self, d: &WeaponDef, stats: &[fc3_formats::weapons::WeaponStats]) -> Option<u32> {
        use fc3_formats::weapons::{ATTACH_BARREL, ATTACH_MAGAZINE, ATTACH_SCOPE, ATTACH_SIGHT};
        let key = fc3_formats::weapons::model_key(&d.model_path());
        let offers = &stats.iter().find(|s| fc3_formats::weapons::model_key(&s.model) == key)?.attachments;
        let named = |kind: u32, word: &str| offers.iter().filter(|o| o.kind == kind && o.name.to_ascii_lowercase().contains(word)).map(|o| o.cost).min();
        match self {
            Attachment::Suppressor => named(ATTACH_BARREL, "suppress"),
            Attachment::Barrel => named(ATTACH_BARREL, "barrel"),
            Attachment::ExtendedMag => named(ATTACH_MAGAZINE, ""),
            Attachment::Optic => named(ATTACH_SCOPE, "").or_else(|| named(ATTACH_SIGHT, "acog")).or_else(|| named(ATTACH_SIGHT, "reflex")),
            Attachment::ReflexA => named(ATTACH_SIGHT, "reflex a"),
            Attachment::ReflexB => named(ATTACH_SIGHT, "reflex b"),
            Attachment::Optical => named(ATTACH_SIGHT, "optical sight"),
            Attachment::Acog => named(ATTACH_SIGHT, "acog"),
            Attachment::IronSight => named(ATTACH_SIGHT, "iron"),
            Attachment::Scope => named(ATTACH_SCOPE, "high power"),
            Attachment::IlluminatedScope => named(ATTACH_SCOPE, "illuminated"),
        }
    }

    /// The model it shows as (an index into `ATTACHMENT_MODELS`, or the sniper
    /// scope) and the weapon node it mounts on.
    fn model(self, d: &WeaponDef) -> Option<(usize, &'static str)> {
        use WeaponClass::*;
        match (self, d.class) {
            (Attachment::Suppressor, Pistol) => Some((0, "ATTACH_SILENCER")),
            (Attachment::Suppressor, Smg | Rifle) => Some((1, "ATTACH_SILENCER")),
            (Attachment::Suppressor, Sniper) => Some((2, "ATTACH_SILENCER")),
            (Attachment::Optic, _) => Attachment::usual_sight(d).model(d),
            (Attachment::Scope | Attachment::IlluminatedScope, _) => Some((5, "ATTACH_UPGRADE")),
            (Attachment::Acog, _) => Some((4, "ATTACH_SCOPE")),
            (Attachment::Optical, _) => Some((5, "ATTACH_SCOPE")),
            (Attachment::ReflexA, _) => Some((3, "ATTACH_SCOPE")),
            (Attachment::ReflexB, _) => Some((6, "ATTACH_SCOPE")),
            _ => None,
        }
    }

    /// The game's attachment this one is for the weapon (the sight it shows
    /// as: a sniper's high-power scope, a rifle's ACOG, else reflex A).
    fn gadget(self, d: &WeaponDef) -> Option<fc3_formats::weapons::Gadget> {
        let key = |k: &str| d.gadgets.iter().find(|g| g.key.starts_with(k)).cloned();
        match self {
            Attachment::Suppressor => key("silencer"),
            Attachment::ExtendedMag => key("magupgrade"),
            Attachment::Barrel => key("barrel"),
            Attachment::Optic => Attachment::usual_sight(d).gadget(d),
            Attachment::ReflexA => key("reflexa"),
            Attachment::ReflexB => key("reflexb"),
            Attachment::Optical => key("optical"),
            Attachment::Acog => key("acog"),
            Attachment::IronSight => key("ironsightupgrade"),
            Attachment::Scope => key("highpowerscope"),
            Attachment::IlluminatedScope => key("illuminatedscope"),
        }
    }

    /// What it does to the weapon: the game's gadget where it has one (a
    /// sight's narrower aimed view, slower aimed turn and longer sight time;
    /// a suppressor's hush; the larger magazine's size; the spread on the
    /// move and the reload), else ours.
    fn apply(self, d: &mut WeaponDef) {
        let gadget = self.gadget(d);
        match (self, &gadget) {
            (Attachment::Suppressor, Some(g)) => {
                d.silenced = true;
                d.damage *= g.damage;
                d.noise = g.noise;
            }
            (Attachment::Suppressor, None) => {
                d.silenced = true;
                d.damage *= 0.9;
                d.range *= 0.85;
            }
            (a, Some(g)) if a.is_sight() => {
                d.ads_fov = (d.ads_fov + g.fov).max(5.0);
                if let Some(a) = d.aim.as_mut() {
                    a.transition += g.sight_time;
                    a.look_pad *= g.look[0];
                    a.look_vertical *= g.look[1];
                    a.look_mouse *= g.look[2];
                }
            }
            (a, None) if a.is_sight() => {
                d.ads_fov = match d.class {
                    WeaponClass::Sniper => d.ads_fov * 0.6,
                    WeaponClass::Rifle => d.ads_fov.min(32.0),
                    _ => d.ads_fov.min(52.0),
                };
            }
            (Attachment::ExtendedMag, Some(g)) if g.magazine > d.magazine => {
                let extra = g.magazine - d.magazine;
                d.magazine = g.magazine;
                d.max_reserve += extra * 2;
            }
            (Attachment::ExtendedMag, _) => {
                let extra = (d.magazine / 2).max(1);
                d.magazine += extra;
                d.max_reserve += extra * 2;
            }
            (Attachment::Barrel, Some(g)) => d.damage *= g.damage,
            _ => {}
        }
        if let Some(g) = gadget {
            if let Some(s) = d.spread_table.as_mut() {
                s.moving *= g.moving_spread;
            }
            d.reload_time *= g.reload;
        }
    }
}

/// Aim blend past which a scoped weapon is seen through its scope.
pub const SCOPE_IN: f32 = 0.9;

/// A scope drifts with the shooter's breathing. Holding the sprint key holds
/// the breath (`sndInhaleSound`) and steadies it for a few seconds; then it
/// comes out (`sndExhaleSound`) and the next needs a moment.
#[derive(Resource, Default)]
pub struct ScopeSway {
    /// View offset (yaw, pitch; radians) the camera adds.
    pub offset: Vec2,
    /// Looking through a scope (the sprint key holds the breath instead).
    pub scoped: bool,
    t: f32,
    held: f32,
    recover: f32,
    holding: bool,
}

/// Drift (radians), how long a breath can be held (s) and the pause after.
const SWAY: f32 = 0.0045;
const BREATH_HOLD: f32 = 5.0;
const BREATH_RECOVER: f32 = 3.0;
const INHALE: u32 = 0x0002_EBDD;
const EXHALE: u32 = 0x0002_EBDC;

fn scope_sway(
    time: Res<Time>,
    actions: Res<crate::controls::ActionState>,
    perks: Res<crate::progression::Perks>,
    body: Res<crate::player::PlayerBody>,
    players: Query<&Inventory, With<Player>>,
    mut sway: ResMut<ScopeSway>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let dt = time.delta_secs();
    // The game's hold (else ours): how long, the catch after, how still it
    // gets (over how long) and how much worse out of breath.
    let hold =
        body.0.breath_hold.unwrap_or(fc3_formats::body::BreathHold { steady: 0.0, hold: BREATH_HOLD, catch: BREATH_RECOVER, held_sway: 0.06, out_sway: 1.8 });
    sway.scoped = players.single().is_ok_and(|inv| inv.scoped());
    sway.recover = (sway.recover - dt).max(0.0);
    let want = sway.scoped && actions.pressed(crate::controls::Action::Sprint) && sway.recover <= 0.0;
    if want && !sway.holding {
        sway.holding = true;
        sway.held = 0.0;
        sfx.write(crate::sound::PlaySfx { event: INHALE, at: None, volume: 0.6 });
    }
    if sway.holding {
        sway.held += dt;
        let out = sway.held > hold.hold * perks.scope_breath;
        if out || !want {
            sway.holding = false;
            sway.recover = if out { hold.catch } else { 0.6 };
            sfx.write(crate::sound::PlaySfx { event: EXHALE, at: None, volume: if out { 0.8 } else { 0.5 } });
        }
    }
    if !sway.scoped {
        sway.offset *= 1.0 - (dt * 8.0).min(1.0);
        return;
    }
    sway.t += dt;
    let t = sway.t;
    let amp = SWAY
        * perks.recoil
        * if sway.holding {
            // Steadying over the first moments of the hold.
            let settled = if hold.steady > 0.0 { (sway.held / hold.steady).min(1.0) } else { 1.0 };
            1.0 + (hold.held_sway - 1.0) * settled
        } else if sway.recover > 0.6 {
            hold.out_sway
        } else {
            1.0
        };
    let target = Vec2::new((t * 0.7).sin() + 0.5 * (t * 1.9).sin(), 0.8 * (t * 0.9).cos() + 0.4 * (t * 2.3).sin()) * amp;
    sway.offset = sway.offset.lerp(target, (dt * 4.0).min(1.0));
}

impl Inventory {
    /// Looking through a rifle scope (the view model is put away for it).
    pub fn scoped(&self) -> bool {
        self.def().class == WeaponClass::Sniper && self.aim_blend > SCOPE_IN
    }

    pub fn slot(&self) -> &WeaponSlot {
        &self.slots[self.current]
    }

    /// Fitted to the weapon kind (a sight: the one fitted).
    pub fn has_attachment(&self, def: usize, a: Attachment) -> bool {
        let bits = self.attachments.get(def).copied().unwrap_or(0);
        if a.is_sight() {
            (bits >> 16) & a.bit() != 0
        } else {
            bits & a.bit() != 0
        }
    }

    /// Bought for the weapon kind (a sight perhaps not the one fitted).
    pub fn owns(&self, def: usize, a: Attachment) -> bool {
        self.attachments.get(def).is_some_and(|bits| bits & a.bit() != 0)
    }

    /// Fits an attachment to a weapon kind: buys it, and a sight goes on in
    /// place of the one fitted (a bought one swapping back on); returns
    /// whether anything changed.
    pub fn attach(&mut self, def: usize, a: Attachment) -> bool {
        if def >= self.defs.len() {
            return false;
        }
        let a = if a == Attachment::Optic { Attachment::usual_sight(&self.defs[def]) } else { a };
        if self.has_attachment(def, a) || !a.fits(&self.defs[def]) {
            return false;
        }
        if self.attachments.len() < self.defs.len() {
            self.attachments.resize(self.defs.len(), 0);
        }
        self.attachments[def] |= a.bit();
        if a.is_sight() {
            self.attachments[def] = (self.attachments[def] & 0xFFFF) | (a.bit() << 16);
        }
        self.refit(def, a);
        true
    }

    /// The weapon kind as its fitted attachments make it: from its own
    /// handling again where the defs are the game's (a sight swapped off
    /// takes its changes with it), else with `added` put on.
    fn refit(&mut self, def: usize, added: Attachment) {
        let base = weapon_defs();
        match base.get(def).filter(|b| b.name == self.defs[def].name) {
            Some(b) => {
                self.defs[def] = b.clone();
                for a in Attachment::ALL.into_iter().filter(|a| !a.is_sight()).chain(Attachment::ALL.into_iter().filter(|a| a.is_sight())) {
                    if self.has_attachment(def, a) {
                        a.apply(&mut self.defs[def]);
                    }
                }
            }
            None => added.apply(&mut self.defs[def]),
        }
    }

    pub fn def(&self) -> &WeaponDef {
        &self.defs[self.slots[self.current].def]
    }

    /// Gives ammo for any owned weapon of this class; returns rounds added.
    pub fn add_ammo(&mut self, class: WeaponClass, rounds: u32) -> u32 {
        let mut added = 0;
        for s in &mut self.slots {
            let d = &self.defs[s.def];
            if d.class == class {
                let cap = (d.max_reserve as f32 * self.reserve_scale) as u32;
                let space = cap.saturating_sub(s.reserve);
                let n = rounds.min(space);
                s.reserve += n;
                added += n;
            }
        }
        added
    }
}

/// Root of the first-person weapon (child of the player camera).
#[derive(Component)]
pub struct ViewModel {
    /// Which slot's model is currently attached, and with which attachments
    /// and paint.
    shown: Option<usize>,
    shown_attachments: u32,
    shown_paint: Option<u8>,
    bob_phase: f32,
    sway: Vec2,
}

impl ViewModel {
    /// Rebuilds the weapon model (it moves between the arms and the plain view).
    pub fn reshow(&mut self) {
        self.shown = None;
    }
}

/// The view model systems (the first-person arms run before them).
#[derive(SystemSet, Clone, PartialEq, Eq, Hash, Debug)]
pub struct ViewModelSystems;

#[derive(Component)]
pub struct ViewModelPart;

/// Muzzle point marker, positioned from the model's bounds.
#[derive(Component)]
pub struct Muzzle;

#[derive(Resource)]
pub struct Rng(u64);

impl Rng {
    pub fn seeded(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }

    /// Uniform in [0, 1).
    pub fn f32(&mut self) -> f32 {
        self.next_u32() as f32 / 4_294_967_296.0
    }

    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f32()
    }
}

#[derive(Resource)]
pub struct EffectAssets {
    pub tracer_mesh: Handle<Mesh>,
    pub tracer_mat: Handle<StandardMaterial>,
    pub grenade: Handle<Fc3Model>,
    pub rocket: Handle<Fc3Model>,
    pub molotov: Handle<Fc3Model>,
    pub arrow: Handle<Fc3Model>,
}

/// Short-lived effect entity: fades/scales over its lifetime, then despawns.
#[derive(Component)]
pub struct Effect {
    pub age: f32,
    pub life: f32,
    pub grow: f32,
    pub light: bool,
}

/// Damage ping for the HUD hit marker.
#[derive(Message, Clone, Copy)]
pub struct HitMarker {
    pub kill: bool,
    pub headshot: bool,
}

#[derive(Message, Clone, Copy)]
pub struct Explosion {
    pub at: Vec3,
    pub radius: f32,
    pub damage: f32,
    pub instigator: Option<Entity>,
}

/// A hit this far below the head's centre is in the legs (m).
const LEGS_BELOW_HEAD: f32 = 0.85;

/// An explosive arrow's blast (about a grenade's).
const EXPLOSIVE_ARROW_RADIUS: f32 = 5.0;
const EXPLOSIVE_ARROW_DAMAGE: f32 = 220.0;

#[derive(Component)]
pub struct Projectile {
    /// Bursts into flame on impact instead of exploding (molotov).
    pub incendiary: bool,
    /// An arrow: wounds what it hits, or sticks where it lands.
    pub arrow: bool,
    pub velocity: Vec3,
    pub gravity: f32,
    pub fuse: Option<f32>,
    pub explode_on_impact: bool,
    pub radius: f32,
    pub damage: f32,
    pub owner: Entity,
    pub age: f32,
    /// Distance flown so far (launched rounds arm after a few metres).
    pub travelled: f32,
}

// --------------------------------------------------------------- systems ---

fn setup_effects(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>, server: Res<AssetServer>) {
    let glow = |c: LinearRgba| StandardMaterial { base_color: Color::from(c), emissive: c * 4.0, unlit: true, alpha_mode: AlphaMode::Add, ..default() };
    commands.insert_resource(EffectAssets {
        tracer_mesh: meshes.add(Cuboid::new(0.012, 0.012, 1.0)),
        tracer_mat: materials.add(glow(LinearRgba::rgb(4.0, 3.0, 1.2))),
        grenade: model_handle(&server, GRENADE_MODEL),
        rocket: model_handle(&server, ROCKET_MODEL),
        molotov: model_handle(&server, MOLOTOV_MODEL),
        arrow: model_handle(&server, ARROW_MODEL),
    });
}

fn give_player_loadout(
    mut commands: Commands,
    args: Res<crate::Args>,
    server: Res<AssetServer>,
    players: Query<Entity, With<Player>>,
    cameras: Query<Entity, With<PlayerCamera>>,
) {
    let defs = weapon_defs();
    let models = defs.iter().map(|d| if d.model.is_empty() { Handle::default() } else { model_handle(&server, &format!("{MODEL_ROOT}{}", d.model)) }).collect();
    let attachment_models: Vec<Handle<Fc3Model>> =
        ATTACHMENT_MODELS.iter().chain([&OPTICAL_MODEL, &REFLEX_B_MODEL]).map(|m| model_handle(&server, &format!("{MODEL_ROOT}{m}"))).collect();
    // Start with the machete, pistol and rifle; the rest are found in the world.
    let starting = ["Machete", "MK23 Pistol", "AK-74M Rifle", "870 Shotgun"];
    let wanted = args.start_weapon.as_deref();
    let slots: Vec<WeaponSlot> = defs
        .iter()
        .enumerate()
        .filter(|(_, d)| starting.contains(&d.name) || wanted.is_some_and(|w| d.name.eq_ignore_ascii_case(w)))
        .map(|(i, d)| WeaponSlot { def: i, mag: d.magazine, reserve: d.max_reserve / 2 })
        .collect();
    let current = wanted.and_then(|w| slots.iter().position(|s| defs[s.def].name.eq_ignore_ascii_case(w))).unwrap_or(2);
    // FC3_ATTACH=all (tests): every weapon starts with everything it takes.
    // (Its first sight fitted; FC3_ATTACH=<name>, e.g. `reflexb`: that one.)
    let mut fitted = vec![0u32; defs.len()];
    let mut defs = defs;
    if let Ok(which) = std::env::var("FC3_ATTACH") {
        let which = which.to_ascii_lowercase();
        let wanted = |a: Attachment| which == "all" || format!("{a:?}").to_ascii_lowercase() == which;
        for (d, bits) in fitted.iter_mut().enumerate() {
            for a in Attachment::ALL {
                if wanted(a) && a.fits(&defs[d]) && (!a.is_sight() || *bits >> 16 == 0) {
                    *bits |= a.bit();
                    if a.is_sight() {
                        *bits |= a.bit() << 16;
                    }
                    a.apply(&mut defs[d]);
                }
            }
        }
    }
    for p in &players {
        commands.entity(p).insert(Inventory {
            defs: defs.clone(),
            models: Vec::clone(&models),
            slots: Vec::clone(&slots),
            current,
            grenades: 3,
            molotovs: 2,
            c4: if std::env::var("FC3_EXPLOSIVES").is_ok() { 3 } else { 0 },
            mines: if std::env::var("FC3_EXPLOSIVES").is_ok() { 3 } else { 0 },
            cooldown: 0.0,
            reloading: 0.0,
            switching: 0.0,
            aiming: false,
            aim_blend: 0.0,
            recoil_kick: 0.0,
            spread_bloom: 0.0,
            shots_fired: 0,
            recoil_offset: 0.0,
            since_shot: 99.0,
            reserve_scale: 1.0,
            attachments: fitted.clone(),
            attachment_models: attachment_models.clone(),
            mounted_from: None,
            auto_tail: 0,
            // FC3_ARROWS=fire|explosive (tests): a quiver of them, nocked.
            arrow_kind: match std::env::var("FC3_ARROWS").as_deref() {
                Ok("fire") => 1,
                Ok("explosive") => 2,
                _ => 0,
            },
            special_arrows: if std::env::var("FC3_ARROWS").is_ok() { [ARROW_CAP; 2] } else { [0; 2] },
            paints: Vec::new(),
            paint_options: Vec::new(),
        });
    }
    for cam in &cameras {
        commands.spawn((
            ViewModel { shown: None, shown_attachments: 0, shown_paint: None, bob_phase: 0.0, sway: Vec2::ZERO },
            Transform::default(),
            Visibility::Visible,
            ChildOf(cam),
            Name::new("view model"),
        ));
    }
}

/// What [`grant_weapon`] did.
pub enum Granted {
    /// Added to a free holster.
    New,
    /// Holsters were full: replaced the named weapon.
    Swapped(&'static str),
    /// Already carried: topped up its ammo.
    Ammo,
}

/// Gives the player a weapon by name (pickups, the shop). With every holster
/// in use (`max_guns`, not counting the machete) it replaces the gun in hand,
/// or the first gun if the machete is out.
pub fn grant_weapon(inv: &mut Inventory, name: &str, max_guns: usize) -> Granted {
    let Some(def) = inv.defs.iter().position(|d| d.name == name) else { return Granted::Ammo };
    if let Some(s) = inv.slots.iter_mut().find(|s| s.def == def) {
        let max = inv.defs[def].max_reserve;
        s.reserve = (s.reserve + inv.defs[def].magazine * 2).min(max);
        return Granted::Ammo;
    }
    let d = &inv.defs[def];
    let slot = WeaponSlot { def, mag: d.magazine, reserve: d.magazine * 2 };
    let is_gun = |inv: &Inventory, s: &WeaponSlot| inv.defs[s.def].class != WeaponClass::Melee;
    let guns = inv.slots.iter().filter(|s| is_gun(inv, s)).count();
    let mut result = Granted::New;
    if d.class != WeaponClass::Melee && guns >= max_guns.max(1) {
        let replace = if is_gun(inv, &inv.slots[inv.current]) { Some(inv.current) } else { inv.slots.iter().position(|s| is_gun(inv, s)) };
        if let Some(i) = replace {
            result = Granted::Swapped(inv.defs[inv.slots[i].def].name);
            inv.slots[i] = slot;
        }
    } else {
        inv.slots.push(slot);
    }
    // Keep slots in catalogue order and hold the new weapon.
    settle_unarmed(inv);
    inv.slots.sort_by_key(|s| s.def);
    inv.current = inv.slots.iter().position(|s| s.def == def).unwrap_or(0);
    result
}

/// Clock and state for the scripted `--fire-test` run.
#[derive(Resource, Default)]
pub struct TestScript {
    pub start: Option<f32>,
    target: Option<Entity>,
    reloaded: bool,
    switched: bool,
    last_log: f32,
}

impl TestScript {
    /// Seconds since the test began, if it is running.
    fn t(&self, time: &Time, args: &crate::Args) -> Option<f32> {
        let t = time.elapsed_secs() - self.start?;
        (args.fire_secs > 0.0 && t <= args.fire_secs).then_some(t)
    }
}

#[derive(Component)]
pub struct FireTestTarget;

fn fire_test_setup(
    args: Res<crate::Args>,
    time: Res<Time>,
    mut script: ResMut<TestScript>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut players: Query<(&mut Player, &Transform)>,
    heights: Res<crate::terrain::Heightfield>,
) {
    if args.fire_secs <= 0.0 || script.start.is_some() {
        return;
    }
    let Ok((mut p, t)) = players.single_mut() else { return };
    if p.spawning {
        return;
    }
    let forward = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let eye = t.translation + Vec3::Y * (crate::player::EYE_HEIGHT - crate::player::HALF_HEIGHT);
    // Stand the target on the ground 8 m ahead and aim at its centre, so
    // hip fire lands on it whatever the slope.
    let mut at = eye + forward * 8.0;
    if let Some(ground) = heights.height_at(at) {
        at.y = ground + 1.0;
    }
    p.pitch = (at.y - eye.y).atan2(8.0);
    let target = commands
        .spawn((
            FireTestTarget,
            Health::new(300.0),
            RigidBody::Static,
            Collider::cuboid(1.2, 2.0, 0.6),
            Mesh3d(meshes.add(Cuboid::new(1.2, 2.0, 0.6))),
            MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.8, 0.15, 0.1), ..default() })),
            Transform::from_translation(at).looking_to(forward, Vec3::Y),
            Name::new("fire-test target"),
        ))
        .id();
    script.start = Some(time.elapsed_secs());
    script.target = Some(target);
    info!("fire-test: target spawned at {at:?}");
}

fn fire_test_log(
    args: Res<crate::Args>,
    time: Res<Time>,
    mut script: ResMut<TestScript>,
    players: Query<&Inventory>,
    targets: Query<&Health, With<FireTestTarget>>,
    mut died: MessageReader<crate::combat::Died>,
) {
    for d in died.read() {
        if Some(d.entity) == script.target {
            info!("fire-test: target destroyed");
        }
    }
    let Some(start) = script.start else { return };
    let now = time.elapsed_secs();
    if args.fire_secs <= 0.0 || now - start > args.fire_secs + 1.5 || now - script.last_log < 1.0 {
        return;
    }
    script.last_log = now;
    let Ok(inv) = players.single() else { return };
    let hp = script.target.and_then(|e| targets.get(e).ok()).map(|h| h.current).unwrap_or(-1.0);
    info!(
        "fire-test t={:.1}s weapon={} mag={} reserve={} reloading={:.2} shots={} target_hp={:.0}",
        now - start,
        inv.def().name,
        inv.slot().mag,
        inv.slot().reserve,
        inv.reloading,
        inv.shots_fired,
        hp
    );
}

/// `FC3_TEST_AMMO` (tests, like `FC3_GOD` for health): the player's guns
/// never run dry; their spare ammunition stays full. `FC3_TEST_LOADOUT`
/// (weapon names, comma separated): he carries those too, whatever a save
/// gave him.
fn test_ammo(mut players: Query<&mut Inventory, With<Player>>) {
    let Ok(mut inv) = players.single_mut() else { return };
    let inv = &mut *inv;
    if let Ok(names) = std::env::var("FC3_TEST_LOADOUT") {
        for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            let Some(def) = inv.defs.iter().position(|d| d.name.eq_ignore_ascii_case(name)) else { continue };
            if !inv.slots.iter().any(|s| s.def == def) {
                let d = &inv.defs[def];
                inv.slots.push(WeaponSlot { def, mag: d.magazine, reserve: d.max_reserve });
            }
        }
    }
    if std::env::var("FC3_TEST_AMMO").is_err() {
        return;
    }
    for slot in &mut inv.slots {
        let d = &inv.defs[slot.def];
        if d.class != WeaponClass::Melee {
            slot.reserve = slot.reserve.max(d.max_reserve.max(d.magazine * 3));
        }
    }
}

fn switch_weapon(
    args: Res<crate::Args>,
    mut script: ResMut<TestScript>,
    keys: Res<ButtonInput<KeyCode>>,
    actions: Res<crate::controls::ActionState>,
    captured: Res<InputCaptured>,
    time: Res<Time>,
    mut q: Query<(&mut Inventory, &Health), With<Player>>,
    mut wheel: ResMut<crate::weapon_wheel::WeaponWheel>,
    perks: Res<crate::progression::Perks>,
) {
    use crate::controls::Action;
    let Ok((mut inv, health)) = q.single_mut() else { return };
    let (tap, pick) = (std::mem::take(&mut wheel.tap), wheel.pick.take());
    let dt = time.delta_secs();
    if inv.switching > 0.0 {
        inv.switching = (inv.switching - dt).max(0.0);
    }
    // No switching away from a mounted gun (getting off does that).
    if health.dead || inv.mounted_from.is_some() {
        return;
    }
    let digits = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7, KeyCode::Digit8];
    let mut target = None;
    for (i, k) in digits.iter().enumerate() {
        if captured.0 && keys.just_pressed(*k) && i < inv.slots.len() {
            target = Some(i);
        }
    }
    let n = inv.slots.len();
    if n == 0 { return; }
    if actions.cycle < 0 || actions.just_pressed(Action::PrevWeapon) {
        target = Some((inv.current + n - 1) % n);
    } else if actions.cycle > 0 {
        target = Some((inv.current + 1) % n);
    } else if tap {
        target = wheel.quick_swap(&inv.slots, inv.current);
    }
    if let Some(p) = pick.filter(|&p| p < n) {
        target = Some(p);
    }
    if actions.just_pressed(Action::Melee) {
        // Quick melee: swap to the machete.
        target = inv.slots.iter().position(|s| inv.defs[s.def].class == WeaponClass::Melee);
    }
    if let Some(t) = script.t(&time, &args) {
        if !script.switched && t > args.fire_secs * 0.7 {
            script.switched = true;
            target = inv.slots.iter().position(|s| inv.defs[s.def].class == WeaponClass::Shotgun);
        }
    }
    if let Some(t) = target {
        if t != inv.current {
            let away = inv.def().holster;
            wheel.previous = Some(inv.slots[inv.current].def);
            inv.current = t;
            inv.reloading = 0.0;
            // The one put away, the other drawn (the swap bonus: faster).
            let swap = (away + inv.def().draw) / perks.swap_speed.max(0.1);
            inv.switching = swap;
            inv.cooldown = inv.cooldown.max(swap);
        }
    }
}

fn reload(
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    args: Res<crate::Args>,
    mut script: ResMut<TestScript>,
    actions: Res<crate::controls::ActionState>,
    hud: Res<crate::hud::HudState>,
    perks: Res<crate::progression::Perks>,
    time: Res<Time>,
    mut q: Query<(&mut Inventory, &Health, &Player)>,
    busy: Res<crate::fp::HandsBusy>,
    foley: Res<crate::characters::ArmsFoley>,
) {
    let Ok((mut inv, health, player)) = q.single_mut() else { return };
    let dt = time.delta_secs();
    // Sprinting breaks off a reload (until `SP_SprintReload` is learned).
    if inv.reloading > 0.0 && player.sprinting && !perks.sprint_reload {
        inv.reloading = 0.0;
        return;
    }
    if inv.reloading > 0.0 {
        // Any reload (manual or automatic) satisfies the scripted test's reload step.
        script.reloaded = true;
        inv.reloading -= dt;
        if inv.reloading <= 0.0 {
            inv.reloading = 0.0;
            let cur = inv.current;
            let mag_size = inv.defs[inv.slots[cur].def].magazine;
            let slot = &mut inv.slots[cur];
            let need = mag_size.saturating_sub(slot.mag);
            let take = need.min(slot.reserve);
            slot.mag += take;
            slot.reserve -= take;
        }
        return;
    }
    if health.dead {
        return;
    }
    let d = inv.def().clone();
    let s = inv.slot().clone();
    // On a gamepad, reload shares its button with interacting.
    let interacting = actions.gamepad_active && hud.prompt.is_some();
    let mut want = actions.just_pressed(crate::controls::Action::Reload) && !interacting;
    if let Some(t) = script.t(&time, &args) {
        if !script.reloaded && t > args.fire_secs * 0.4 {
            script.reloaded = true;
            want = true;
        }
    }
    // With an arrow nocked, reload picks the next kind of arrow instead.
    if d.class == WeaponClass::Bow && want && s.mag >= d.magazine {
        let next = inv.next_arrow();
        if next != inv.arrow_kind {
            inv.arrow_kind = next;
            sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.5 });
        }
        return;
    }
    if d.class != WeaponClass::Melee && (want || s.mag == 0) && s.mag < d.magazine && s.reserve > 0 && inv.switching <= 0.0 && !busy.busy() {
        let bow = if d.class == WeaponClass::Bow { perks.bow_speed } else { 1.0 };
        inv.reloading = d.reload_time / perks.reload_speed / bow;
        // `SP_FastAimedReload` keeps a pistol or sniper rifle in the sights.
        inv.aiming &= aimed_reload(&perks, d.class);
        // The arms' reload clip sounds its own magazine and bolt.
        if !foley.reload {
            sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::RELOAD, at: None, volume: 0.8 });
        }
    }
}

/// Whether a reload keeps the sights up (`SP_FastAimedReload`, pistols and sniper
/// rifles).
fn aimed_reload(perks: &crate::progression::Perks, class: WeaponClass) -> bool {
    perks.aimed_reload && matches!(class, WeaponClass::Pistol | WeaponClass::Sniper)
}

fn spread_dir(rng: &mut Rng, forward: Vec3, right: Vec3, up: Vec3, deg: f32) -> Vec3 {
    let r = deg.to_radians() * rng.f32().sqrt();
    let a = rng.f32() * std::f32::consts::TAU;
    (forward + right * (r * a.cos()) + up * (r * a.sin())).normalize()
}

/// Finds the damageable entity owning a hit collider (itself or an ancestor).
pub fn damage_target(e: Entity, health: &Query<&Health>, parents: &Query<&ChildOf>) -> Option<Entity> {
    let mut cur = e;
    for _ in 0..6 {
        if health.contains(cur) {
            return Some(cur);
        }
        cur = parents.get(cur).ok()?.parent();
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn fire(
    inputs: (
        Res<crate::stealth::Binoculars>,
        Res<crate::controls::ActionState>,
        Res<InputCaptured>,
        Res<crate::progression::Perks>,
        Res<crate::fp::HandsBusy>,
        Query<&crate::npc::HeadPos>,
        Res<crate::weapon_wheel::WeaponWheel>,
        Res<crate::aim_assist::AimAssist>,
    ),
    args: Res<crate::Args>,
    script: Res<TestScript>,
    time: Res<Time>,
    mut rng: ResMut<Rng>,
    spatial: SpatialQuery,
    fx: Option<Res<EffectAssets>>,
    mut commands: Commands,
    mut players: Query<(Entity, &mut Inventory, &mut Player, &Health)>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    muzzle: Query<&GlobalTransform, With<Muzzle>>,
    healths: Query<&Health>,
    parents: Query<&ChildOf>,
    writers: (MessageWriter<DamageEvent>, MessageWriter<HitMarker>, MessageWriter<crate::npc::Noise>),
    sounds: (MessageWriter<crate::sound::PlaySfx>, MessageWriter<crate::sound::LoopSfx>, crate::surfaces::SurfaceProbe),
    (mut shot_fx, boosts, mount, driving, vehicles): (
        crate::particles::ShotFx,
        Option<Res<crate::syringes::Boosts>>,
        Res<crate::traversal::Mount>,
        Res<crate::vehicles::Driving>,
        Query<&crate::vehicles::Vehicle>,
    ),
) {
    use crate::controls::Action;
    use crate::particles::fx;
    let (bino, actions, captured, perks, busy, heads, wheel, assist) = inputs;
    // (The steady-aim syringe: far less kick.)
    let steady = if boosts.as_ref().is_some_and(|b| b.active(crate::syringes::Syringe::Steady)) { 0.3 } else { 1.0 };
    let (mut damage, mut markers, mut noise) = writers;
    let (mut sfx, mut loops, probe) = sounds;
    let Some(fx) = fx else { return };
    let Ok((player_e, mut inv, mut player, health)) = players.single_mut() else { return };
    let Ok(cam) = camera.single() else { return };
    let dt = time.delta_secs();
    inv.cooldown = (inv.cooldown - dt).max(0.0);
    // The spread settles back (at the game's rate, where it has one).
    let settle = if inv.slots.is_empty() { None } else { inv.def().spread_steps().map(|(_, back)| back) };
    inv.spread_bloom = (inv.spread_bloom - dt * settle.unwrap_or(3.0)).max(0.0);
    inv.recoil_kick = (inv.recoil_kick - dt * 6.0).max(0.0);
    // Recoil recovery: the view settles back toward the original aim, quickly
    // once the trigger is released and partially during sustained fire.
    inv.since_shot += dt;
    // A burst ends: the tail and its echo off the ground around.
    if inv.auto_tail != 0 && inv.since_shot > inv.def().interval * 1.6 {
        let event = crate::sound::switched(inv.auto_tail, probe.terrain(cam.translation()), 0);
        sfx.write(crate::sound::PlaySfx { event, at: None, volume: if inv.def().silenced { 0.4 } else { 0.9 } });
        inv.auto_tail = 0;
    }
    let rate = if inv.since_shot > 0.15 { 7.0 } else { 2.5 };
    let recover = inv.recoil_offset * (dt * rate).min(1.0);
    inv.recoil_offset -= recover;
    player.pitch -= recover;
    let scripted = script.t(&time, &args).is_some();
    // Automated screenshots deliberately don't capture the OS cursor. The
    // story driver supplies ordinary actions, not scripted infinite firing.
    let story_input = args.automated() && (std::env::var("FC3_STORY_AUTOPILOT").is_ok()
        || std::env::var("FC3_OUTPOST_FIRE_TEST").is_ok());
    // On a zip line only one-handed guns fire, with `SP_ShootOnZipline`.
    let zip_ok = !matches!(*mount, crate::traversal::Mount::Zip { .. }) || (perks.zip_shoot && inv.def().class == WeaponClass::Pistol);
    let can_act = (captured.0 || scripted || story_input)
        && !health.dead
        && !player.noclip
        && !player.swimming
        && (!player.driving || player.armed_passenger)
        && bino.blend < 0.3
        && !busy.busy()
        && !wheel.open
        && zip_ok;
    // FC3_AIM (tests): hold aim down the sights.
    let aim_held = actions.pressed(Action::Aim) || std::env::var("FC3_AIM").is_ok();
    inv.aiming = (can_act || std::env::var("FC3_AIM").is_ok())
        && aim_held
        && (inv.reloading <= 0.0 || aimed_reload(&perks, inv.def().class))
        && inv.def().class != WeaponClass::Melee;
    let target_aim = if inv.aiming { 1.0 } else { 0.0 };
    // The sights come up (and go down) in the weapon's transition time.
    match inv.def().aim.map(|a| a.transition).filter(|t| *t > 0.0) {
        Some(t) => inv.aim_blend += (target_aim - inv.aim_blend).clamp(-dt / t, dt / t),
        None => inv.aim_blend += (target_aim - inv.aim_blend) * (dt * 12.0).min(1.0),
    }
    if (scripted || story_input) && std::env::var("FC3_FIRE_DEBUG").is_ok() && (time.elapsed_secs() * 4.0) as u32 != ((time.elapsed_secs() - dt) * 4.0) as u32 {
        info!(
            "fire gate: can_act {can_act} dead {} swimming {} driving {} noclip {} captured {} bino {:.2} reloading {:.2} switching {:.2} cooldown {:.2} mag {}",
            health.dead,
            player.swimming,
            player.driving,
            player.noclip,
            captured.0,
            bino.blend,
            inv.reloading,
            inv.switching,
            inv.cooldown,
            inv.slot().mag
        );
    }
    if !can_act || inv.reloading > 0.0 || inv.switching > 0.0 || inv.cooldown > 0.0 {
        return;
    }
    let d = inv.def().clone();
    // Scripted runs hold the trigger; semi-auto weapons fire at their cycle rate.
    let trigger = scripted || if d.automatic { actions.pressed(Action::Fire) } else { actions.just_pressed(Action::Fire) };
    if !trigger {
        return;
    }
    if d.class != WeaponClass::Melee && inv.slot().mag == 0 {
        if actions.just_pressed(Action::Fire) {
            let empty = weapon_sounds(d.name).empty;
            sfx.write(crate::sound::PlaySfx { event: if empty != 0 { empty } else { crate::sound::ev::DRY_FIRE }, at: None, volume: 0.8 });
        }
        return;
    }
    if let Some(tail) = weapon_sound(&d, player_e, &mut sfx, &mut loops) {
        inv.auto_tail = tail;
    }
    inv.cooldown = if d.class == WeaponClass::Bow { d.interval / perks.bow_speed } else { d.interval };
    inv.shots_fired += 1;
    let cur = inv.current;
    if d.class != WeaponClass::Melee {
        inv.slots[cur].mag -= 1;
    }
    let origin = cam.translation();
    // (With a pad, the game's correction bends the shot toward a target.)
    let forward = assist.correct(origin, cam.forward().as_vec3(), inv.aim_blend > 0.5, d.aim.as_ref());
    let right = cam.right().as_vec3();
    let up = cam.up().as_vec3();
    // (Not the vehicle he's riding in: shots from a seat go past its sides.)
    let riding = driving.vehicle.and_then(|v| vehicles.get(v).ok().map(|vh| [v, vh.body()]));
    let filter = SpatialQueryFilter::from_excluded_entities(std::iter::once(player_e).chain(riding.into_iter().flatten()));
    let muzzle_pos = muzzle.single().map(|m| m.translation()).unwrap_or(origin + forward * 0.6 - up * 0.1);
    // (The weapon held small near the eye in a seat: its flash too.)
    let flash = if player.armed_passenger { SEATED_VIEW_SCALE } else { 1.0 };

    if d.class == WeaponClass::Launcher {
        commands.spawn((
            Projectile {
                incendiary: false,
                arrow: false,
                velocity: forward * 70.0,
                gravity: 2.0,
                fuse: None,
                explode_on_impact: true,
                radius: 7.0,
                damage: d.damage,
                owner: player_e,
                age: 0.0,
                travelled: 0.0,
            },
            Transform::from_translation(muzzle_pos).looking_to(forward, Vec3::Y),
            Visibility::Visible,
            ProjectileModel(fx.rocket.clone()),
        ));
        spawn_muzzle_light(&mut commands, muzzle_pos);
        fx::muzzle_flash(&mut shot_fx.particles, &mut rng, muzzle_pos, forward, 1.8 * flash);
        for _ in 0..4 {
            fx::barrel_smoke(&mut shot_fx.particles, &mut rng, muzzle_pos);
        }
        player.pitch += d.recoil;
        crate::npc::player_noise(&mut noise, origin, 150.0, crate::npc::NoiseKind::Threatening);
        return;
    }
    if d.class == WeaponClass::GrenadeLauncher || d.class == WeaponClass::Bow {
        let bow = d.class == WeaponClass::Bow;
        let cone = d.spread_cone(inv.aim_blend, player.crouching, !player.grounded, 0.0, 0.0).unwrap_or(d.spread * (1.0 - 0.9 * inv.aim_blend));
        let dir = if bow { spread_dir(&mut rng, forward, right, up, cone * perks.bow_spread) } else { forward };
        // A special arrow nocked: one fewer of them; plain arrows once they run out.
        let kind = if bow { inv.arrow_kind } else { 0 };
        if kind > 0 {
            let left = &mut inv.special_arrows[kind as usize - 1];
            *left = left.saturating_sub(1);
            if *left == 0 {
                inv.arrow_kind = 0;
            }
        }
        commands.spawn((
            Projectile {
                incendiary: kind == 1,
                arrow: bow,
                velocity: if bow { dir * 75.0 } else { (dir + up * 0.06).normalize() * 40.0 },
                gravity: if bow { 4.0 } else { 9.81 },
                fuse: None,
                explode_on_impact: !bow || kind == 2,
                radius: if kind == 1 { 3.0 } else { 6.0 },
                damage: d.damage,
                owner: player_e,
                age: 0.0,
                travelled: 0.0,
            },
            Transform::from_translation(if bow { origin + dir * 0.3 } else { muzzle_pos }).looking_to(dir, Vec3::Y),
            Visibility::Visible,
            ProjectileModel(if bow { fx.arrow.clone() } else { fx.grenade.clone() }),
        ));
        if !bow {
            spawn_muzzle_light(&mut commands, muzzle_pos);
            fx::muzzle_flash(&mut shot_fx.particles, &mut rng, muzzle_pos, forward, 1.3 * flash);
            crate::npc::player_noise(&mut noise, origin, 120.0, crate::npc::NoiseKind::Threatening);
        }
        player.pitch += d.recoil;
        return;
    }
    if d.class == WeaponClass::Flamethrower {
        // A stream of flame: burns what it reaches and lights the ground.
        let reach = spatial.cast_ray(muzzle_pos, Dir3::new(forward).unwrap_or(Dir3::NEG_Z), d.range, true, &filter);
        let end = muzzle_pos + forward * reach.map(|h| h.distance).unwrap_or(d.range);
        fx::flame_stream(&mut shot_fx.particles, &mut rng, muzzle_pos, forward, player.velocity, muzzle_pos.distance(end));
        if let Some(h) = reach {
            if let Some(target) = damage_target(h.entity, &healths, &parents) {
                damage.write(DamageEvent {
                    target,
                    amount: d.damage,
                    kind: DamageKind::Fire,
                    source: Some(origin),
                    instigator: Some(player_e),
                    headshot: false,
                    hit: crate::combat::Hit::Whole,
                });
                markers.write(HitMarker { kill: false, headshot: false });
            }
            if rng.f32() < 0.2 {
                shot_fx.ignite.write(crate::fire::Ignite { at: end, radius: 1.5, instigator: Some(player_e) });
            }
        }
        crate::npc::player_noise(&mut noise, origin, 40.0, crate::npc::NoiseKind::Threatening);
        return;
    }

    let aim = inv.aim_blend;
    let move_penalty = if player.grounded { (player.velocity.length() / 6.0).min(1.0) } else { 1.0 };
    let cone = d
        .spread_cone(aim, player.crouching, !player.grounded, inv.spread_bloom, move_penalty)
        .unwrap_or(d.spread * (1.0 - 0.8 * aim) * (1.0 + inv.spread_bloom + move_penalty * 0.8));
    let pellets = d.pellets.max(1);
    // With the game's spread a shotgun's shot strays as one, and each pellet
    // about it by its own angle.
    let scatter = d.spread_table.and_then(|s| s.pellets).map(|p| p[0].max(p[1]));
    let shot = if scatter.is_some() { spread_dir(&mut rng, forward, right, up, cone) } else { forward };
    let mut any_hit = None;
    for _ in 0..pellets {
        let dir = match scatter {
            _ if d.class == WeaponClass::Melee => forward,
            Some(a) => {
                let r = shot.cross(up).normalize_or(right);
                spread_dir(&mut rng, shot, r, r.cross(shot), a)
            }
            None => spread_dir(&mut rng, forward, right, up, cone),
        };
        let Ok(dir3) = Dir3::new(dir) else { continue };
        let hit = spatial.cast_ray(origin, dir3, d.range, true, &filter);
        let end = hit.map(|h| origin + dir * h.distance).unwrap_or(origin + dir * d.range);
        if d.class != WeaponClass::Melee && rng.f32() < 0.6 {
            spawn_tracer(&mut commands, &fx, muzzle_pos, end);
        }
        if std::env::var("FC3_FIRE_DEBUG").is_ok() {
            if let Some(h) = hit {
                let (e, dist) = (h.entity, h.distance);
                commands.queue(move |world: &mut World| {
                    let name = world.get::<Name>(e).map(|n| n.to_string());
                    let parent = world.get::<ChildOf>(e).map(|c| c.parent());
                    let parent_name = parent.and_then(|p| world.get::<Name>(p)).map(|n| n.to_string());
                    let pos = world.get::<GlobalTransform>(e).map(|t| t.translation());
                    info!("shot hit {e:?} {name:?} at {dist:.2} m (entity at {pos:?}, parent {parent:?} {parent_name:?})");
                });
            }
        }
        let Some(h) = hit else {
            if d.class != WeaponClass::Melee {
                shot_fx.bullet_end(&mut rng, origin, dir, d.range, None);
            }
            continue;
        };
        let point = origin + dir * h.distance;
        let target = damage_target(h.entity, &healths, &parents);
        // The body is a standing capsule: a hit well above the head of a
        // crouched (or falling) enemy goes over it.
        let over = target.and_then(|t| heads.get(t).ok()).is_some_and(|head| point.y > head.0.y + 0.3);
        if let Some(target) = target.filter(|_| !over) {
            // Head hits: within a skull's width of the rig's head; legs
            // below the hips.
            let head = heads.get(target).ok();
            let headshot = head.is_some_and(|head| head.0.distance(point) < 0.2);
            let zone = match head {
                _ if headshot => crate::combat::Zone::Head,
                Some(head) if point.y < head.0.y - LEGS_BELOW_HEAD => crate::combat::Zone::Legs,
                _ => crate::combat::Zone::Torso,
            };
            let mult = if headshot { perks.headshot } else { 1.0 };
            // The game's fall-off with range (a shotgun's without it).
            let falloff = if !d.ranges.is_empty() {
                d.range_scale(h.distance)
            } else if d.class == WeaponClass::Shotgun {
                (1.0 - h.distance / d.range).clamp(0.2, 1.0)
            } else {
                1.0
            };
            let melee = d.class == WeaponClass::Melee;
            damage.write(DamageEvent {
                target,
                amount: d.damage * mult * falloff,
                kind: if melee { DamageKind::Melee } else { DamageKind::Bullet },
                source: Some(origin),
                instigator: Some(player_e),
                headshot,
                hit: if melee { crate::combat::Hit::Whole } else { crate::combat::Hit::Bullet(zone) },
            });
            any_hit = Some(headshot);
            fx::blood(&mut shot_fx.particles, &mut rng, point, dir);
            let impact = if d.class == WeaponClass::Melee { crate::sound::ev::MELEE_IMPACT } else { crate::sound::ev::IMPACT };
            let event = crate::sound::switched(impact, crate::surfaces::value::FLESH, 0);
            sfx.write(crate::sound::PlaySfx { event, at: Some(point), volume: 0.6 });
            // Splatter on whatever is just behind.
            if let Ok(d3) = Dir3::new(dir) {
                let behind = SpatialQueryFilter::from_excluded_entities([player_e, h.entity]);
                if let Some(b) = spatial.cast_ray(point, d3, 2.5, true, &behind) {
                    shot_fx.decals.add(crate::particles::DecalSheet::Blood, point + dir * b.distance, b.normal, 0.6, rng.f32());
                }
            }
        } else {
            if d.class != WeaponClass::Melee {
                shot_fx.bullet_end(&mut rng, origin, dir, d.range, Some((h.entity, h.distance, h.normal)));
            }
            if d.class == WeaponClass::Melee || rng.f32() < 0.5 {
                let impact = if d.class == WeaponClass::Melee { crate::sound::ev::MELEE_IMPACT } else { crate::sound::ev::IMPACT };
                let event = crate::sound::switched(impact, probe.at(h.entity, point, h.normal), 0);
                sfx.write(crate::sound::PlaySfx { event, at: Some(point), volume: 0.6 });
            }
        }
    }
    if let Some(headshot) = any_hit {
        markers.write(HitMarker { kill: false, headshot });
    }
    if d.class != WeaponClass::Melee && d.silenced {
        // Suppressed: a faint flash and little noise (as little as the
        // game's suppressor makes).
        let heard = if d.noise < 1.0 { 120.0 * d.noise } else { 18.0 };
        crate::npc::player_noise(&mut noise, origin, heard, crate::npc::NoiseKind::Threatening);
        fx::muzzle_flash(&mut shot_fx.particles, &mut rng, muzzle_pos, forward, 0.35 * flash);
        inv.spread_bloom = match d.spread_steps() {
            Some((step, _)) => (inv.spread_bloom + step).min(1.0),
            None => (inv.spread_bloom + 0.3).min(2.5),
        };
        inv.recoil_kick = (inv.recoil_kick + 0.8).min(1.6);
        let kick = d.recoil * (1.0 - 0.4 * aim) * perks.recoil * steady;
        player.pitch = (player.pitch + kick).min(1.5);
        inv.recoil_offset += kick;
        inv.since_shot = 0.0;
    } else if d.class != WeaponClass::Melee {
        crate::npc::player_noise(&mut noise, origin, 120.0, crate::npc::NoiseKind::Threatening);
        spawn_muzzle_light(&mut commands, muzzle_pos);
        let scale = match d.class {
            WeaponClass::Shotgun => 1.5,
            WeaponClass::Pistol => 0.8,
            WeaponClass::Sniper | WeaponClass::Lmg => 1.3,
            _ => 1.0,
        };
        fx::muzzle_flash(&mut shot_fx.particles, &mut rng, muzzle_pos, forward, scale * flash);
        if rng.f32() < 0.35 {
            fx::barrel_smoke(&mut shot_fx.particles, &mut rng, muzzle_pos);
        }
        inv.spread_bloom = match d.spread_steps() {
            Some((step, _)) => (inv.spread_bloom + step).min(1.0),
            None => (inv.spread_bloom + 0.35).min(2.5),
        };
        inv.recoil_kick = (inv.recoil_kick + 1.0).min(1.6);
        let kick = d.recoil * (1.0 - 0.4 * aim) * perks.recoil * steady;
        player.pitch = (player.pitch + kick).min(1.5);
        inv.recoil_offset += kick;
        inv.since_shot = 0.0;
        player.yaw += rng.range(-0.3, 0.3) * kick;
    } else {
        inv.recoil_kick = 1.0;
    }
}

/// A weapon's first-person sounds from its `WeaponProperties` (see
/// `docs/audio.md`): one shot, or an automatic weapon's fire loop and the stop
/// event that ends it with a tail and an echo; the suppressed versions; the
/// empty click. 0 = none (the class's sound).
#[derive(Clone, Copy, Default)]
pub struct WeaponSounds {
    pub fire: u32,
    pub stop: u32,
    pub silenced: u32,
    pub silenced_stop: u32,
    pub empty: u32,
}

pub fn weapon_sounds(name: &str) -> WeaponSounds {
    let s = |fire, stop, silenced, silenced_stop, empty| WeaponSounds { fire, stop, silenced, silenced_stop, empty };
    match name {
        "MK23 Pistol" => s(0x0001_706E, 0, 0x0009_FA29, 0, 0x0002_EC35),
        "1911" => s(0x0004_6E1D, 0, 0x0005_258B, 0, 0x0044_939E),
        "6P9" => s(0x0001_706E, 0, 0x0009_FA29, 0, 0x0002_EC35),
        "D50" => s(0x0018_F690, 0, 0, 0, 0x0044_939E),
        "MP5 SMG" => s(0x0009_FA70, 0x0018_F643, 0x0017_3176, 0x0018_F644, 0x0044_93A0),
        "A2000" => s(0x0004_786E, 0x0018_F63A, 0x0017_77D6, 0x0018_F63B, 0x0044_93A0),
        "Vector .45 ACP" => s(0x0004_7874, 0x0018_F641, 0x0017_3179, 0x0018_F640, 0x0002_EC34),
        "Skorpion" => s(0x0004_6CE9, 0x0018_F647, 0x0017_316C, 0x0018_F648, 0x0002_EC36),
        "PP-19 Bizon" => s(0x0010_35A3, 0x0018_F63C, 0x0017_317F, 0x0018_F63D, 0x0044_93A0),
        "AK-74M Rifle" => s(0x0004_6CB3, 0x0004_6CB8, 0x0017_77D2, 0x0017_77D4, 0x0002_EC34),
        "F1" => s(0x0017_2E90, 0x0018_F63F, 0x0017_317C, 0x0018_F63E, 0x0002_EC34),
        "P416" => s(0x0010_35CD, 0x0018_F645, 0x0017_3173, 0x0018_F646, 0x0002_EC34),
        "MS16" => s(0x0010_34AC, 0, 0x0017_789D, 0, 0x0002_EC34),
        "ACE" => s(0x0017_2E8A, 0x0018_F638, 0x0017_77D9, 0x0018_F639, 0x0002_EC34),
        "STG-90" => s(0x0017_2E94, 0x0004_6CDA, 0x0017_316F, 0x0017_316E, 0x0002_EC34),
        "870 Shotgun" => s(0x0001_7076, 0, 0, 0, 0x0002_EC3E),
        "1887" => s(0x0018_F36C, 0, 0, 0, 0x0002_EC3E),
        "SPAS-12" => s(0x0009_FA86, 0, 0, 0, 0x0002_EC3E),
        "700 Sniper" => s(0x0001_7025, 0, 0x0017_788C, 0, 0x0002_EC3D),
        "SVD" => s(0x0002_62EE, 0, 0x0017_7894, 0, 0x0002_EC3D),
        "Z93" => s(0x0017_311F, 0, 0x0017_7897, 0, 0x0002_EC3D),
        "PKM LMG" => s(0x0002_E680, 0x0009_FA75, 0, 0, 0x0002_EC36),
        "U100" => s(0x0017_314F, 0x0018_F649, 0, 0, 0x0002_EC34),
        "MKG" | "M249" => s(0x0010_35D3, 0x0018_F642, 0, 0, 0x0002_EC36),
        "DShK" => s(0x0004_6D2C, 0x0004_6D2B, 0, 0, 0x0044_93A3),
        "RPG-7" => s(0x0018_9801, 0, 0, 0, 0x0044_9403),
        "GL-94" => s(0x0017_3146, 0, 0, 0, 0x0002_EC34),
        _ => WeaponSounds::default(),
    }
}

/// Player weapon sound: automatic weapons keep a fire loop alive (returning
/// the stop event to play when the burst ends), others play one-shots.
fn weapon_sound(
    d: &WeaponDef,
    player: Entity,
    sfx: &mut MessageWriter<crate::sound::PlaySfx>,
    loops: &mut MessageWriter<crate::sound::LoopSfx>,
) -> Option<u32> {
    use crate::sound::ev;
    let own = weapon_sounds(d.name);
    let (fire, stop) = if d.silenced && own.silenced != 0 { (own.silenced, own.silenced_stop) } else { (own.fire, own.stop) };
    // The weapon's own suppressed shot is already quiet.
    let volume = if d.silenced && own.silenced == 0 { 0.3 } else { 0.9 };
    if fire != 0 {
        if d.automatic && stop != 0 {
            loops.write(crate::sound::LoopSfx { key: player.to_bits(), event: fire, at: None, volume, ..default() });
            return Some(stop);
        }
        sfx.write(crate::sound::PlaySfx { event: fire, at: None, volume });
        return None;
    }
    let (event, looping) = match d.class {
        WeaponClass::Bow | WeaponClass::Flamethrower => return None,
        WeaponClass::GrenadeLauncher => (ev::SHOTGUN, false),
        WeaponClass::Rifle => (ev::AK_LOOP, true),
        WeaponClass::Smg => (ev::SMG_LOOP, true),
        WeaponClass::Lmg | WeaponClass::Mounted => (ev::LMG_LOOP, true),
        WeaponClass::Pistol => (ev::PISTOL, false),
        WeaponClass::Shotgun => (ev::SHOTGUN, false),
        WeaponClass::Sniper => (ev::SNIPER, false),
        WeaponClass::Launcher => (ev::RPG, false),
        WeaponClass::Melee => return None,
    };
    if looping {
        loops.write(crate::sound::LoopSfx { key: player.to_bits(), event, at: None, volume, ..default() });
    } else {
        sfx.write(crate::sound::PlaySfx { event, at: None, volume });
    }
    None
}

#[derive(Component)]
pub struct ProjectileModel(pub Handle<Fc3Model>);

/// Seconds from the pin to the blast.
const GRENADE_FUSE: f32 = 3.0;

fn throw_grenade(
    actions: Res<crate::controls::ActionState>,
    fx: Option<Res<EffectAssets>>,
    mut fp: ResMut<crate::fp::FpState>,
    mut commands: Commands,
    mut players: Query<(Entity, &mut Inventory, &Player, &Health)>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    busy: Res<crate::fp::HandsBusy>,
    (perks, time, mut cooking): (Res<crate::progression::Perks>, Res<Time>, Local<Option<f32>>),
) {
    let Some(fx) = fx else { return };
    let Ok((e, mut inv, player, health)) = players.single_mut() else { return };
    let Ok(cam) = camera.single() else { return };
    if health.dead || player.noclip || player.swimming || player.driving || player.mounted.is_some() || busy.busy() {
        *cooking = None;
        return;
    }
    let molotov = actions.just_pressed(crate::controls::Action::Molotov) && inv.molotovs > 0;
    if molotov {
        inv.molotovs -= 1;
        fp.gesture = Some("grenade|throw_molotov");
        let f = cam.forward().as_vec3();
        commands.spawn((
            Projectile {
                incendiary: true,
                arrow: false,
                velocity: (f + Vec3::Y * 0.2).normalize() * 18.0 + player.velocity * 0.5,
                gravity: 9.81,
                fuse: None,
                explode_on_impact: false,
                radius: 4.0,
                damage: 0.0,
                owner: e,
                age: 0.0,
                travelled: 0.0,
            },
            Transform::from_translation(cam.translation() + f * 0.5),
            Visibility::Visible,
            ProjectileModel(fx.molotov.clone()),
        ));
        return;
    }
    // `SP_CookThrowables`: the pin comes out as the key goes down and the grenade
    // flies as it comes up, with what is left of its fuse (held too long, it
    // goes off in hand). Without it, it is thrown at once.
    let mut fuse = GRENADE_FUSE;
    let mut at_feet = false;
    if let Some(t) = cooking.as_mut() {
        *t += time.delta_secs();
        if actions.pressed(crate::controls::Action::Grenade) && *t < GRENADE_FUSE {
            return;
        }
        fuse = (GRENADE_FUSE - *t).max(0.0);
        at_feet = fuse <= 0.0;
        *cooking = None;
    } else {
        if !actions.just_pressed(crate::controls::Action::Grenade) || inv.grenades == 0 {
            return;
        }
        inv.grenades -= 1;
        if perks.cook_grenades {
            *cooking = Some(0.0);
            return;
        }
    }
    fp.gesture = Some("grenade");
    let f = cam.forward().as_vec3();
    let velocity = if at_feet { Vec3::ZERO } else { (f + Vec3::Y * 0.25).normalize() * 17.0 + player.velocity * 0.5 };
    commands.spawn((
        Projectile {
            incendiary: false,
            arrow: false,
            velocity,
            gravity: 9.81,
            fuse: Some(fuse.max(0.01)),
            explode_on_impact: false,
            radius: 8.0,
            damage: 180.0,
            owner: e,
            age: 0.0,
            travelled: 0.0,
        },
        Transform::from_translation(cam.translation() + f * 0.5),
        Visibility::Visible,
        ProjectileModel(fx.grenade.clone()),
    ));
}

fn update_projectiles(
    time: Res<Time>,
    spatial: SpatialQuery,
    models: Res<Assets<Fc3Model>>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Projectile, &mut Transform, Option<&ProjectileModel>, Option<&Children>)>,
    mut explosions: MessageWriter<Explosion>,
    mut ignite: MessageWriter<crate::fire::Ignite>,
    mut particles: ResMut<crate::particles::Particles>,
    mut rng: ResMut<Rng>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    targets: (Query<&Health>, Query<&ChildOf>, MessageWriter<DamageEvent>, MessageWriter<HitMarker>),
) {
    let (healths, parents, mut damage, mut markers) = targets;
    let dt = time.delta_secs();
    for (e, mut p, mut t, model, children) in &mut q {
        // Attach the model's first LOD once it has loaded.
        if let (Some(m), None) = (model, children) {
            if let Some(m) = models.get(&m.0) {
                if let Some(lod) = m.lods.first() {
                    for part in &lod.parts {
                        commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), NotShadowCaster, ChildOf(e)));
                    }
                }
            }
        }
        p.age += dt;
        p.velocity.y -= p.gravity * dt;
        let step = p.velocity * dt;
        let filter = SpatialQueryFilter::from_excluded_entities([p.owner, e]);
        let mut exploded = false;
        if let Ok(dir) = Dir3::new(step) {
            if let Some(hit) = spatial.cast_ray(t.translation, dir, step.length(), true, &filter) {
                let point = t.translation + step.normalize() * hit.distance;
                // Fire and explosive arrows go off where they strike.
                if p.arrow && (p.incendiary || p.explode_on_impact) {
                    if std::env::var("FC3_FIRE_DEBUG").is_ok() {
                        info!("{} arrow went off at {point:.1?}", if p.incendiary { "fire" } else { "explosive" });
                    }
                    if let Some(target) = damage_target(hit.entity, &healths, &parents) {
                        damage.write(DamageEvent {
                            target,
                            amount: p.damage,
                            kind: DamageKind::Bullet,
                            source: Some(t.translation),
                            instigator: Some(p.owner),
                            headshot: false,
                            hit: crate::combat::Hit::Whole,
                        });
                        markers.write(HitMarker { kill: false, headshot: false });
                    }
                    if p.incendiary {
                        ignite.write(crate::fire::Ignite { at: point, radius: p.radius, instigator: Some(p.owner) });
                        crate::particles::fx::fire_burst(&mut particles, &mut rng, point);
                        sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::IMPACT, at: Some(point), volume: 0.6 });
                    } else {
                        explosions.write(Explosion { at: point, radius: EXPLOSIVE_ARROW_RADIUS, damage: EXPLOSIVE_ARROW_DAMAGE, instigator: Some(p.owner) });
                    }
                    commands.entity(e).despawn();
                    continue;
                }
                if p.arrow {
                    if let Some(target) = damage_target(hit.entity, &healths, &parents) {
                        damage.write(DamageEvent {
                            target,
                            amount: p.damage,
                            kind: DamageKind::Bullet,
                            source: Some(t.translation),
                            instigator: Some(p.owner),
                            headshot: false,
                            hit: crate::combat::Hit::Arrow(crate::combat::Zone::Torso),
                        });
                        markers.write(HitMarker { kill: false, headshot: false });
                        crate::particles::fx::blood(&mut particles, &mut rng, point, step.normalize());
                        commands.entity(e).despawn();
                    } else {
                        // Stuck where it landed, for a while.
                        t.translation = point - step.normalize() * 0.3;
                        commands.entity(e).remove::<Projectile>().insert(Effect { age: 0.0, life: 30.0, grow: 0.0, light: false });
                    }
                    continue;
                }
                if p.incendiary {
                    ignite.write(crate::fire::Ignite { at: point, radius: p.radius, instigator: Some(p.owner) });
                    crate::particles::fx::fire_burst(&mut particles, &mut rng, point);
                    sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::IMPACT, at: Some(point), volume: 0.8 });
                    exploded = true;
                } else if p.explode_on_impact && p.travelled + hit.distance > ARMING_DISTANCE {
                    if std::env::var("FC3_FIRE_DEBUG").is_ok() {
                        info!("projectile hit {:?} at {point:?} after {:.2}s (from {:?})", hit.entity, p.age, t.translation);
                    }
                    explosions.write(Explosion { at: point, radius: p.radius, damage: p.damage, instigator: Some(p.owner) });
                    exploded = true;
                } else {
                    // A launched round that strikes before arming is a dud.
                    if p.explode_on_impact {
                        p.explode_on_impact = false;
                        p.fuse = None;
                    }
                    // Bounce with damping.
                    let n = hit.normal;
                    p.velocity = (p.velocity - 2.0 * p.velocity.dot(n) * n) * 0.35;
                    t.translation = point + n * 0.05;
                }
            } else {
                t.translation += step;
                p.travelled += step.length();
            }
        }
        if !exploded && p.velocity.length_squared() > 1.0 {
            t.rotation = if p.incendiary && !p.arrow {
                Quat::from_rotation_x(p.age * 9.0) * Quat::from_rotation_z(p.age * 4.0)
            } else {
                Quat::from_rotation_arc(Vec3::NEG_Z, p.velocity.normalize())
            };
        }
        if p.incendiary && !exploded && p.age < 4.0 {
            // A burning rag trails the bottle.
            let mut f = crate::particles::Particle::new(crate::particles::Sheet::Fire, t.translation, 0.18, 0.35);
            f.frame = crate::particles::Frame::Fixed((rng.f32() * 64.0) as u32);
            f.colour = Vec4::new(1.0, 0.4, 0.1, 0.8);
            particles.spawn(f);
        }
        if let Some(fuse) = p.fuse {
            if p.age >= fuse {
                explosions.write(Explosion { at: t.translation, radius: p.radius, damage: p.damage, instigator: Some(p.owner) });
                exploded = true;
            }
        }
        if exploded || p.age > 20.0 {
            commands.entity(e).despawn();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn explode(
    mut ignite: MessageWriter<crate::fire::Ignite>,
    mut events: MessageReader<Explosion>,
    mut rng: ResMut<Rng>,
    mut particles: ResMut<crate::particles::Particles>,
    mut decals: ResMut<crate::particles::Decals>,
    mut commands: Commands,
    targets: Query<(Entity, &GlobalTransform), With<Health>>,
    spatial: SpatialQuery,
    mut damage: MessageWriter<DamageEvent>,
    mut markers: MessageWriter<HitMarker>,
    players: Query<Entity, With<Player>>,
    mut noise: MessageWriter<crate::npc::Noise>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    heights: Res<crate::terrain::Heightfield>,
) {
    let player = players.single().ok();
    for ex in events.read() {
        crate::npc::player_noise(&mut noise, ex.at, 200.0, crate::npc::NoiseKind::Explosion);
        let blast = if ex.radius > 7.5 { crate::sound::ev::GRENADE_BLAST } else { crate::sound::ev::ROCKET_BLAST };
        sfx.write(crate::sound::PlaySfx { event: blast, at: Some(ex.at), volume: 1.0 });
        // Visual: fireball, smoke, sparks, a scorch mark and a brief light.
        let water = crate::water::surface_at(ex.at, &heights);
        if ex.at.y < water + 0.5 && ex.at.y > water - 3.0 {
            crate::particles::fx::splash(&mut particles, &mut rng, Vec3::new(ex.at.x, water, ex.at.z), 4.0);
        } else {
            crate::particles::fx::explosion(&mut particles, &mut rng, ex.at, ex.radius);
            // Blasts set the grass around them alight.
            ignite.write(crate::fire::Ignite { at: ex.at, radius: ex.radius * 0.4, instigator: ex.instigator });
            if let Some(h) = spatial.cast_ray(ex.at + Vec3::Y * 0.5, Dir3::NEG_Y, 2.5, true, &SpatialQueryFilter::default()) {
                decals.add(crate::particles::DecalSheet::Sand, ex.at + Vec3::Y * (0.5 - h.distance), h.normal, ex.radius * 0.45, rng.f32());
            }
        }
        commands.spawn((
            PointLight { color: Color::srgb(1.0, 0.6, 0.25), intensity: 4.0e7, range: ex.radius * 6.0, shadow_maps_enabled: false, ..default() },
            Transform::from_translation(ex.at + Vec3::Y * 0.5),
            Effect { age: 0.0, life: 0.35, grow: 0.0, light: true },
        ));
        let mut hit_any = false;
        for (e, gt) in &targets {
            let p = gt.translation();
            let dist = p.distance(ex.at);
            if dist > ex.radius {
                continue;
            }
            // Blocked by solid geometry?
            let to = p + Vec3::Y * 0.8 - ex.at;
            if let Ok(dir) = Dir3::new(to) {
                let filter = SpatialQueryFilter::from_excluded_entities([e]);
                if let Some(h) = spatial.cast_ray(ex.at + Vec3::Y * 0.3, dir, to.length(), true, &filter) {
                    if h.distance < to.length() - 0.5 {
                        continue;
                    }
                }
            }
            let amount = ex.damage * (1.0 - dist / ex.radius).powf(0.7);
            damage.write(DamageEvent {
                target: e,
                amount,
                kind: DamageKind::Explosion,
                source: Some(ex.at),
                instigator: ex.instigator,
                headshot: false,
                hit: crate::combat::Hit::Whole,
            });
            if Some(e) != player {
                hit_any = true;
            }
        }
        if hit_any && ex.instigator == player {
            markers.write(HitMarker { kill: false, headshot: false });
        }
    }
}

/// The brief light a shot throws around the muzzle.
fn spawn_muzzle_light(commands: &mut Commands, at: Vec3) {
    commands.spawn((
        PointLight { color: Color::srgb(1.0, 0.75, 0.4), intensity: 3.0e5, range: 8.0, shadow_maps_enabled: false, ..default() },
        Transform::from_translation(at),
        Effect { age: 0.0, life: 0.05, grow: 0.0, light: true },
    ));
}

fn spawn_tracer(commands: &mut Commands, fx: &EffectAssets, from: Vec3, to: Vec3) {
    let len = from.distance(to);
    if len < 1.0 {
        return;
    }
    let mid = (from + to) * 0.5;
    commands.spawn((
        Mesh3d(fx.tracer_mesh.clone()),
        MeshMaterial3d(fx.tracer_mat.clone()),
        Transform::from_translation(mid).looking_at(to, Vec3::Y).with_scale(Vec3::new(1.0, 1.0, len)),
        Effect { age: 0.0, life: 0.04, grow: 0.0, light: false },
        NotShadowCaster,
    ));
}

fn fade_effects(time: Res<Time>, mut commands: Commands, mut q: Query<(Entity, &mut Effect, &mut Transform, Option<&mut PointLight>)>) {
    let dt = time.delta_secs();
    for (e, mut fx, mut t, light) in &mut q {
        fx.age += dt;
        if fx.age >= fx.life {
            commands.entity(e).despawn();
            continue;
        }
        if fx.grow > 0.0 {
            t.scale += Vec3::splat(fx.grow * dt / fx.life);
        }
        if let (true, Some(mut l)) = (fx.light, light) {
            l.intensity *= 0.6f32.powf(dt * 60.0);
        }
    }
}

/// Positions the view model: hold pose, ADS, bob, sway, recoil, reload and switch motion.
/// The held weapon's size seated in a vehicle (see `update_view_model`).
pub(crate) const SEATED_VIEW_SCALE: f32 = 0.3;

fn update_view_model(
    game_state: Res<State<crate::menu::GameState>>,
    time: Res<Time>,
    motion: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    settings: Res<crate::settings::Settings>,
    bino: Res<crate::stealth::Binoculars>,
    perks: Res<crate::progression::Perks>,
    players: Query<(&Inventory, &Player, &Health)>,
    mut vm: Query<(&mut ViewModel, &mut Transform, &mut Visibility)>,
    mut cams: Query<&mut Projection, With<PlayerCamera>>,
    fp: Res<crate::fp::FpState>,
    speed_fov: Option<Res<crate::vehicles::SpeedFov>>,
) {
    let Ok((inv, player, health)) = players.single() else { return };
    let Ok((mut v, mut t, mut vis)) = vm.single_mut() else { return };
    let dt = time.delta_secs();
    let d = inv.def();
    let in_menu = *game_state.get() != crate::menu::GameState::Playing;
    // Weapons are stowed while swimming, as in the original.
    // Swimming and driving put the weapon away; the arms may still show what they're doing.
    let busy = (player.swimming || (player.driving && !player.armed_passenger)) && !fp.in_context();
    *vis = if in_menu || health.dead || player.noclip || busy || bino.blend > 0.5 || inv.scoped() { Visibility::Hidden } else { Visibility::Visible };
    let speed = Vec2::new(player.velocity.x, player.velocity.z).length();
    if player.grounded {
        v.bob_phase += dt * speed * 1.7;
    }
    let bob_amp = (speed / 6.0).min(1.2) * (1.0 - inv.aim_blend * 0.85);
    let bob = Vec3::new(v.bob_phase.sin() * 0.012, (v.bob_phase * 2.0).sin().abs() * -0.012, 0.0) * bob_amp;
    let target_sway = (-motion.delta * 0.0006).clamp(Vec2::splat(-0.03), Vec2::splat(0.03));
    v.sway = v.sway.lerp(target_sway, (dt * 10.0).min(1.0));
    let ads_pos = Vec3::new(0.0, -0.075, d.hold.z + 0.05);
    let mut pos = d.hold.lerp(ads_pos, inv.aim_blend) + bob + Vec3::new(v.sway.x, -v.sway.y, 0.0);
    // Recoil pushes back and tilts up.
    pos.z += inv.recoil_kick * 0.035;
    let mut rot = Quat::from_rotation_x(inv.recoil_kick * 0.06) * Quat::from_rotation_y(v.sway.x * 2.0);
    // Reload and switch: dip the weapon out of view and back.
    let dip = if inv.reloading > 0.0 {
        let t = 1.0 - inv.reloading / (d.reload_time / perks.reload_speed);
        (t * std::f32::consts::PI).sin()
    } else if inv.switching > 0.0 {
        (inv.switching / 0.35 * std::f32::consts::PI).sin()
    } else {
        0.0
    };
    pos.y -= dip * 0.22;
    rot = Quat::from_rotation_x(-dip * 0.6) * Quat::from_rotation_z(dip * 0.3) * rot;
    if d.class == WeaponClass::Melee {
        // Swing arc on attack.
        let swing = inv.recoil_kick.min(1.0);
        rot = Quat::from_rotation_x(-swing * 1.3) * Quat::from_rotation_z(swing * 0.8) * Quat::from_rotation_x(-0.5) * rot;
    }
    if d.class == WeaponClass::Bow {
        // Held canted, off to the left, the arrow lined up with the sights.
        rot = Quat::from_rotation_z(0.4 * (1.0 - 0.6 * inv.aim_blend)) * rot;
    }
    if fp.active {
        // The arms' clips hold, aim, recoil and reload the weapon: only bob and sway here.
        t.translation = bob * 0.6 + Vec3::new(v.sway.x, -v.sway.y, 0.0) * 0.5;
        t.rotation = Quat::from_rotation_y(v.sway.x * 1.5);
    } else {
        t.translation = pos;
        t.rotation = rot;
    }
    // In a seat the vehicle's own parts (a seat back, the bed's walls) come
    // nearer the eye than the weapon held out in front: drawn smaller toward
    // the eye it looks the same and stays in front of them, as the game
    // draws its weapon over the world.
    let k = if player.armed_passenger { SEATED_VIEW_SCALE } else { 1.0 };
    t.translation *= k;
    t.scale = Vec3::splat(k);
    if let Ok(mut proj) = cams.single_mut() {
        if let Projection::Perspective(p) = &mut *proj {
            // Aiming narrows from the player's FOV toward the weapon's.
            let base = settings.fov;
            let fov = base + (d.ads_fov.min(base) - base) * inv.aim_blend;
            // Binoculars zoom further still; a vehicle's camera widens with
            // speed.
            let fov = (fov + (18.0 - fov) * bino.blend) * speed_fov.as_ref().map_or(1.0, |s| s.0);
            let fov = fov.min(120.0).to_radians();
            p.fov = fov;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_view_model_parts(
    mut commands: Commands,
    server: Res<AssetServer>,
    paint_table: Res<PaintTable>,
    models: Res<Assets<Fc3Model>>,
    players: Query<&Inventory>,
    mut vm: Query<(Entity, &mut ViewModel)>,
    parts: Query<Entity, Or<(With<ViewModelPart>, With<Muzzle>)>>,
    fp: Res<crate::fp::FpState>,
) {
    let Ok(inv) = players.single() else { return };
    let Ok((view, mut v)) = vm.single_mut() else { return };
    let def_index = inv.slot().def;
    let fitted = inv.attachments.get(def_index).copied().unwrap_or(0);
    let paint = inv.paints.get(def_index).copied().flatten();
    if v.shown == Some(def_index) && v.shown_attachments == fitted && v.shown_paint == paint {
        return;
    }
    let Some(model) = models.get(&inv.models[def_index]) else { return };
    for e in &parts {
        commands.entity(e).despawn();
    }
    // In the arms' hand when they hold this weapon, else the plain view model.
    let root = fp.mount.filter(|_| fp.active).unwrap_or(view);
    let mounted = inv.defs[def_index].class == WeaponClass::Mounted;
    if let Some(lod) = model.lods.first() {
        for part in lod.parts.iter().filter(|p| p.shown(mounted)) {
            // A painted part takes its painted material, drawn by the game's
            // own shader (which blends the pattern by the weapon's mask);
            // the rest read their metal mask.
            match paint.and_then(|p| painted(&server, &paint_table, &part.material, p)) {
                Some(variant) => {
                    let material: Handle<StandardMaterial> = server.load(crate::cache_assets::cache_path(&variant));
                    commands.spawn((
                        Mesh3d(part.mesh.clone()),
                        MeshMaterial3d(material),
                        NotShadowCaster,
                        ViewModelPart,
                        crate::original_shaders::PropPart,
                        ChildOf(root),
                    ));
                }
                None => {
                    commands.spawn((
                        Mesh3d(part.mesh.clone()),
                        MeshMaterial3d(part.material.clone()),
                        NotShadowCaster,
                        ViewModelPart,
                        crate::metal::MaskedMetal,
                        ChildOf(root),
                    ));
                }
            }
        }
    }
    // Attachments on their mounts (the weapon's own nodes).
    let d = &inv.defs[def_index];
    for a in Attachment::ALL {
        let Some((mi, node)) = a.model(d).filter(|_| inv.has_attachment(def_index, a)) else { continue };
        let (Some(at), Some(part_model)) = (node_pose(model, node), inv.attachment_models.get(mi).and_then(|h| models.get(h))) else { continue };
        for part in part_model.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]) {
            commands.spawn((
                Mesh3d(part.mesh.clone()),
                MeshMaterial3d(part.material.clone()),
                NotShadowCaster,
                ViewModelPart,
                crate::metal::MaskedMetal,
                at,
                ChildOf(root),
            ));
        }
    }
    // The model's muzzle-flash node, else the front of its bounds.
    let (mn, mx) = (model.aabb_min, model.aabb_max);
    let muzzle = node_pose(model, "FX_FIRE").map(|t| t.translation).unwrap_or(Vec3::new((mn.x + mx.x) * 0.5, mn.y + (mx.y - mn.y) * 0.62, mn.z));
    commands.spawn((Muzzle, Transform::from_translation(muzzle), ChildOf(root)));
    v.shown = Some(def_index);
    v.shown_attachments = fitted;
    v.shown_paint = paint;
}

/// A node's model-space pose, from the model's bone hierarchy.
pub fn node_pose(model: &Fc3Model, name: &str) -> Option<Transform> {
    let mut i = model.bones.iter().position(|b| b.name.eq_ignore_ascii_case(name))?;
    let mut m = Mat4::IDENTITY;
    for _ in 0..model.bones.len() {
        let b = &model.bones[i];
        m = Mat4::from_rotation_translation(Quat::from_array(b.rotation), Vec3::from_array(b.translation)) * m;
        if b.parent < 0 || b.parent as usize == i {
            break;
        }
        i = b.parent as usize;
    }
    Some(Transform::from_matrix(m))
}

/// An unarmed inventory (tests).
#[cfg(test)]
pub(crate) fn test_inventory() -> Inventory {
    let defs = weapon_defs();
    let unarmed = defs.iter().position(|d| d.name == UNARMED).unwrap();
    Inventory {
        defs,
        models: Vec::new(),
        slots: vec![WeaponSlot { def: unarmed, mag: 0, reserve: 0 }],
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
    }
}

#[cfg(test)]
mod stats_tests {
    use super::*;

    #[test]
    fn the_games_handling_goes_on_the_weapons_its_model_names() {
        let stats = vec![
            fc3_formats::weapons::WeaponStats {
                name: "AK47".into(),
                model: r"graphics\__fc3_graphics\_common\weapons\ak74m\ak74m_tp.xbg".into(),
                damage: 40.0,
                fire_rate: 600.0,
                magazine: 30,
                reload: 2.4,
                pellets: 1,
                ..Default::default()
            },
            fc3_formats::weapons::WeaponStats {
                name: "RPG7".into(),
                model: r"graphics\__fc3_graphics\_common\weapons\rpg7\rpg7_tp.xbg".into(),
                damage: 5.0,
                fire_rate: 30.0,
                magazine: 1,
                ..Default::default()
            },
        ];
        let mut defs = base_weapon_defs();
        assert_eq!(apply_stats(&mut defs, &stats), 2);
        let ak = defs.iter().find(|d| d.name == "AK-74M Rifle").unwrap();
        assert_eq!((ak.damage, ak.magazine, ak.reload_time), (40.0, 30, 2.4));
        assert!((ak.interval - 0.1).abs() < 1e-6);
        // Launchers keep their own (blast) damage.
        let rpg = defs.iter().find(|d| d.name == "RPG-7").unwrap();
        assert!(rpg.damage > 5.0);
    }

    #[test]
    fn attachments_take_the_games_gadgets() {
        use fc3_formats::weapons::{Aim, Gadget};
        let plain = |key: &str| Gadget { key: key.into(), damage: 1.0, noise: 1.0, moving_spread: 1.0, reload: 1.0, look: [1.0; 3], ..Default::default() };
        let stats = vec![fc3_formats::weapons::WeaponStats {
            name: "AK47".into(),
            model: r"graphics\__fc3_graphics\_common\weapons\ak74m\ak74m_tp.xbg".into(),
            magazine: 30,
            aim: Some(Aim { fov: 65.0, transition: 0.35, look_pad: 0.3, look_mouse: 0.7, look_vertical: 1.0, ..Default::default() }),
            gadgets: vec![
                Gadget { fov: -30.0, sight_time: 0.1, look: [0.6, 1.0, 0.7], ..plain("acog") },
                Gadget { noise: 0.02, ..plain("silencerassault") },
                Gadget { magazine: 40, ..plain("magupgrade") },
            ],
            ..Default::default()
        }];
        let mut defs = base_weapon_defs();
        apply_stats(&mut defs, &stats);
        let ak = defs.iter().find(|d| d.name == "AK-74M Rifle").unwrap().clone();
        assert_eq!(ak.ads_fov, 65.0);
        // The rifle's sight is the ACOG: 30 degrees narrower, slower aimed.
        let mut sighted = ak.clone();
        Attachment::Optic.apply(&mut sighted);
        let a = sighted.aim.unwrap();
        assert_eq!(sighted.ads_fov, 35.0);
        assert!((a.transition - 0.45).abs() < 1e-6 && (a.look_pad - 0.18).abs() < 1e-6 && (a.look_mouse - 0.49).abs() < 1e-6);
        // The suppressor hushes it without weakening it; the larger
        // magazine holds the game's 40.
        let mut quiet = ak.clone();
        Attachment::Suppressor.apply(&mut quiet);
        assert!(quiet.silenced && quiet.noise == 0.02 && quiet.damage == ak.damage && quiet.range == ak.range);
        let mut mag = ak.clone();
        Attachment::ExtendedMag.apply(&mut mag);
        assert_eq!(mag.magazine, 40);
    }
}
