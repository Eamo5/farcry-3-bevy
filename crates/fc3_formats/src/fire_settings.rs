//! The fire manager's settings (`firesettings_default.obj`): for each kind
//! of fire (a burning patch of ground and its smoke, a burning object, a
//! burning plant, the flamethrower's sticky stream...), the effects it plays
//! and how many of each may play at once (`iPoolMaxSize`). The converter
//! keeps them as [`FIRE_SETTINGS_REL`] and caches the effects.

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

    /// The game's own settings (ignored: needs the game's data).
    #[test]
    #[ignore]
    fn the_games_settings_name_their_effects() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let effects = parse(&vfs.read(FIRE_SETTINGS).unwrap()).unwrap();
        assert!(effects.iter().filter(|e| e.kind == REGION).count() >= 2, "{effects:?}");
        assert!(effects.iter().all(|e| e.pool > 0));
    }
}
