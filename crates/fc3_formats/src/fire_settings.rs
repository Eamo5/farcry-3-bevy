//! The fire manager's settings (`firesettings_default.obj`): for each kind
//! of fire (a burning patch of ground and its smoke, a burning object, a
//! burning plant, the flamethrower's sticky stream...), the effects it plays
//! and how many of each may play at once (`iPoolMaxSize`). The converter
//! keeps them as [`FIRE_SETTINGS_REL`] and caches the effects.
//!
//! And the fire manager's region profiles (`fireregion.lib`): how long a
//! burning patch of ground burns, how long its smoke lingers and how far it
//! scares animals off, kept as [`FIRE_REGIONS_REL`]. And its object
//! profiles (`fireobject.lib`), how a burnable object burns, with the models
//! that burn as each (`burnablesettings_default.obj`), kept as
//! [`FIRE_OBJECTS_REL`].

use anyhow::{Context, Result};

pub const FIRE_SETTINGS: &str = r"generated\databases\generic\firesettings_default.obj";
pub const FIRE_SETTINGS_REL: &str = "particles/fire-settings.txt";

/// A burning patch of ground (a fire manager region's cells).
pub const REGION: &str = "FireRegionSettings";
/// The smoke over one.
pub const REGION_SMOKE: &str = "FireSmokeRegionSettings";
/// A burning plant (a bush; the RealTree vegetation's).
pub const REALTREE: &str = "FireRealtreeSettings";
/// The kinds of fire, by their blocks in the settings.
const KINDS: [&str; 8] = [
    REGION,
    "FireObjectSettings",
    "FireRealtreeSettings",
    "FireStickyStreamSettings",
    REGION_SMOKE,
    "FireSmokeObjectSettings",
    "FireSmokeStickySettings",
    "FirePostEffectSettings",
];

pub const FIRE_REGIONS: &str = r"generated\databases\generic\fireregion.lib";
pub const FIRE_REGIONS_REL: &str = "particles/fire-regions.txt";
/// The profile the open world's grass burns with (the savannah's; the
/// game picks one by the zone painted on the ground, `zoneLogicZone` in
/// `burnablesettings_default.obj`).
pub const SAVANNAH: &str = "Region/Savannah";

/// A region profile: how a burning patch of ground burns. A burnable
/// region names its own (`CIgnitorComponent`'s `fireregRegionProfile_SP`,
/// by key).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RegionProfile {
    pub key: u64,
    pub name: String,
    /// Seconds a cell's fire takes to grow, burns at its height, takes to
    /// die down, then smoulders.
    pub starting: f32,
    pub alive: f32,
    pub extinction: f32,
    pub dying: f32,
    /// How long (s) the smoke over it lingers.
    pub smoke: (f32, f32),
    /// Animals this near (m) are scared off (`Fire_FearStim`).
    pub fear: f32,
}

pub fn parse_regions(lib: &[u8]) -> Result<Vec<RegionProfile>> {
    let start = lib.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
    let root = crate::fcb::parse(&lib[start..])?;
    // (A zero is stored as a single byte.)
    let f = |o: Option<&std::sync::Arc<crate::fcb::Object>>, n: &str| o.and_then(|o| o.f32(n)).unwrap_or(0.0);
    Ok(root
        .children
        .iter()
        .filter_map(|p| {
            let smoke = p.child("Fire_SmokeParams");
            Some(RegionProfile {
                key: p.u64("hidKey")?,
                name: p.str("hidName")?,
                starting: f(Some(p), "fStartingTime"),
                alive: f(Some(p), "fAliveTime"),
                extinction: f(Some(p), "fExtinctionTime"),
                dying: f(Some(p), "fDyingTime"),
                smoke: (f(smoke, "fSmokeMinLifeTime"), f(smoke, "fSmokeMaxLifeTime")),
                fear: f(p.child("Fire_FearStim"), "fRadius"),
            })
        })
        .collect())
}

/// As cached ([`FIRE_REGIONS_REL`]): key, name and numbers per line.
pub fn regions_to_text(profiles: &[RegionProfile]) -> String {
    profiles
        .iter()
        .map(|p| format!("{:x}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n", p.key, p.name, p.starting, p.alive, p.extinction, p.dying, p.smoke.0, p.smoke.1, p.fear))
        .collect()
}

pub fn regions_from_text(text: &str) -> Vec<RegionProfile> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let n = |i: usize| f.get(i).and_then(|v| v.parse::<f32>().ok());
            Some(RegionProfile {
                key: u64::from_str_radix(f.first()?, 16).ok()?,
                name: f.get(1)?.to_string(),
                starting: n(2)?,
                alive: n(3)?,
                extinction: n(4)?,
                dying: n(5)?,
                smoke: (n(6)?, n(7)?),
                fear: n(8)?,
            })
        })
        .collect()
}

pub const FIRE_OBJECTS: &str = r"generated\databases\generic\fireobject.lib";
pub const BURNABLES: &str = r"generated\databases\generic\burnablesettings_default.obj";
pub const FIRE_OBJECTS_REL: &str = "particles/fire-objects.txt";
/// A burning object's fire and its smoke (and the black smoke's, by name).
pub const OBJECT: &str = "FireObjectSettings";
pub const OBJECT_SMOKE: &str = "FireSmokeObjectSettings";

/// An object profile: how a burnable object burns.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ObjectProfile {
    pub key: u64,
    pub name: String,
    /// Seconds its fire takes to grow, burns at its height, takes to die
    /// down, then smoulders.
    pub starting: f32,
    pub alive: f32,
    pub extinction: f32,
    pub dying: f32,
    /// How long (s) its smoke lingers.
    pub smoke: (f32, f32),
    /// What it's charred to (`BurnColor`'s diffuse).
    pub char: [f32; 3],
    /// Whether it smokes black (`BurnParams`' `bBlackSmoke`: tyres, fuel).
    pub black: bool,
}

/// What a burnable model burns as: its archive path and its profile's key.
#[derive(Clone, Debug, PartialEq)]
pub struct Burnable {
    pub model: String,
    pub profile: u64,
}

fn fcb_root(bytes: &[u8]) -> Result<std::sync::Arc<crate::fcb::Object>> {
    let start = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
    crate::fcb::parse(&bytes[start..])
}

pub fn parse_objects(lib: &[u8]) -> Result<Vec<ObjectProfile>> {
    let root = fcb_root(lib)?;
    // (A zero is stored as a single byte.)
    let f = |o: Option<&std::sync::Arc<crate::fcb::Object>>, n: &str| o.and_then(|o| o.f32(n)).unwrap_or(0.0);
    Ok(root
        .children
        .iter()
        .filter_map(|p| {
            let smoke = p.child("Fire_SmokeParams");
            let colour = p.child("BurnColor").and_then(|c| c.vec3("clrDiffuseColor")).unwrap_or([0.0; 3]);
            Some(ObjectProfile {
                key: p.u64("hidKey")?,
                name: p.str("hidName")?,
                starting: f(Some(p), "fStartingTime"),
                alive: f(Some(p), "fAliveTime"),
                extinction: f(Some(p), "fExtinctionTime"),
                dying: f(Some(p), "fDyingTime"),
                smoke: (f(smoke, "fSmokeMinLifeTime"), f(smoke, "fSmokeMaxLifeTime")),
                char: colour,
                black: p.child("BurnParams").and_then(|b| b.bool("bBlackSmoke")).unwrap_or(false),
            })
        })
        .collect())
}

pub fn parse_burnables(obj: &[u8]) -> Result<Vec<Burnable>> {
    // (The model's path sits under an unnamed field.)
    const MODEL: u32 = 0x4623_EB5D;
    let root = fcb_root(obj)?;
    let meshes = root.child("Meshes").context("no burnable meshes")?;
    Ok(meshes
        .children
        .iter()
        .filter_map(|m| {
            let model = m.str_h(MODEL).map(|s| s.trim_end_matches('\0').to_string()).filter(|s| s.to_ascii_lowercase().ends_with(".xbg"))?;
            let profile = m.u64("fireobjProfile").filter(|k| *k != u64::from(u32::MAX))?;
            Some(Burnable { model, profile })
        })
        .collect())
}

/// As cached ([`FIRE_OBJECTS_REL`]): a `profile` line for each profile
/// (key, name and numbers) and a `model` line for each burnable model.
pub fn objects_to_text(profiles: &[ObjectProfile], burnables: &[Burnable]) -> String {
    let mut s = String::new();
    for p in profiles {
        s += &format!(
            "profile\t{:x}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            p.key,
            p.name,
            p.starting,
            p.alive,
            p.extinction,
            p.dying,
            p.smoke.0,
            p.smoke.1,
            p.char[0],
            p.char[1],
            p.char[2],
            u8::from(p.black)
        );
    }
    for b in burnables {
        s += &format!("model\t{:x}\t{}\n", b.profile, b.model);
    }
    s
}

pub fn objects_from_text(text: &str) -> (Vec<ObjectProfile>, Vec<Burnable>) {
    let (mut profiles, mut burnables) = (Vec::new(), Vec::new());
    for l in text.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        let n = |i: usize| f.get(i).and_then(|v| v.parse::<f32>().ok());
        let key = f.get(1).and_then(|k| u64::from_str_radix(k, 16).ok());
        match (f.first().copied(), key) {
            (Some("profile"), Some(key)) if f.len() >= 13 => profiles.push(ObjectProfile {
                key,
                name: f[2].to_string(),
                starting: n(3).unwrap_or(0.0),
                alive: n(4).unwrap_or(0.0),
                extinction: n(5).unwrap_or(0.0),
                dying: n(6).unwrap_or(0.0),
                smoke: (n(7).unwrap_or(0.0), n(8).unwrap_or(0.0)),
                char: [n(9).unwrap_or(0.0), n(10).unwrap_or(0.0), n(11).unwrap_or(0.0)],
                black: f[12] == "1",
            }),
            (Some("model"), Some(profile)) if f.len() >= 3 => burnables.push(Burnable { model: f[2].to_string(), profile }),
            _ => {}
        }
    }
    (profiles, burnables)
}

/// One of a kind of fire's effects, and how many of it may play at once.
#[derive(Clone, Debug, PartialEq)]
pub struct FireEffect {
    pub kind: String,
    pub effect: String,
    pub pool: u32,
}

pub fn parse(obj: &[u8]) -> Result<Vec<FireEffect>> {
    // (Each resource's effect name sits next to its `psParticlesSys` id.)
    const EFFECT_NAME: u32 = 0x7F61_4501;
    let start = obj.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
    let root = crate::fcb::parse(&obj[start..])?;
    let mut out = Vec::new();
    for kind in KINDS {
        let Some(resources) = root.child(kind).and_then(|k| k.child("Resources")) else { continue };
        for d in &resources.children {
            let Some(effect) = d.str_h(EFFECT_NAME).map(|s| s.trim_end_matches('\0').to_string()).filter(|s| s.contains('.')) else { continue };
            out.push(FireEffect { kind: kind.to_string(), effect, pool: d.u32("iPoolMaxSize").unwrap_or(0) });
        }
    }
    Ok(out)
}

/// As cached ([`FIRE_SETTINGS_REL`]): kind, pool, effect per line.
pub fn to_text(effects: &[FireEffect]) -> String {
    effects.iter().map(|e| format!("{}\t{}\t{}\n", e.kind, e.pool, e.effect)).collect()
}

pub fn from_text(text: &str) -> Vec<FireEffect> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some(FireEffect { kind: f.next()?.to_string(), pool: f.next()?.parse().ok()?, effect: f.next()?.to_string() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_read_back() {
        let effects = vec![
            FireEffect { kind: REGION.into(), effect: "a.b.fire".into(), pool: 40 },
            FireEffect { kind: REGION_SMOKE.into(), effect: "a.b.smoke".into(), pool: 20 },
        ];
        assert_eq!(from_text(&to_text(&effects)), effects);
    }

    #[test]
    fn object_profiles_and_burnables_read_back() {
        let profiles = vec![ObjectProfile {
            key: 0xeda7_1cca,
            name: "StrawObjects/Small".into(),
            starting: 5.0,
            alive: 3.0,
            extinction: 1.0,
            dying: 5.0,
            smoke: (10.0, 20.0),
            char: [0.35, 0.24, 0.15],
            black: true,
        }];
        let burnables = vec![Burnable { model: r"graphics\objects\box.xbg".into(), profile: 0xeda7_1cca }];
        assert_eq!(objects_from_text(&objects_to_text(&profiles, &burnables)), (profiles, burnables));
    }

    #[test]
    fn region_profiles_read_back() {
        let profiles = vec![
            RegionProfile { key: 0x47fd_f013, name: SAVANNAH.into(), starting: 5.0, alive: 5.0, extinction: 5.0, dying: 13.0, smoke: (15.0, 25.0), fear: 20.0 },
            RegionProfile { key: 0x6c74, name: "Region/Other".into(), starting: 0.5, ..Default::default() },
        ];
        assert_eq!(regions_from_text(&regions_to_text(&profiles)), profiles);
    }

    /// The game's own settings (ignored: needs the game's data).
    #[test]
    #[ignore]
    fn the_games_settings_name_their_effects() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let effects = parse(&vfs.read(FIRE_SETTINGS).unwrap()).unwrap();
        assert!(effects.iter().filter(|e| e.kind == REGION).count() >= 2, "{effects:?}");
        assert!(effects.iter().all(|e| e.pool > 0));
    }

    /// The game's region profiles (ignored: needs the game's data).
    #[test]
    #[ignore]
    fn the_games_region_profiles_time_their_fires() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let profiles = parse_regions(&vfs.read(FIRE_REGIONS).unwrap()).unwrap();
        let savannah = profiles.iter().find(|p| p.name == SAVANNAH).expect("the savannah's");
        assert!(savannah.starting + savannah.alive + savannah.extinction > 0.0 && savannah.fear > 0.0, "{savannah:?}");
    }
}
