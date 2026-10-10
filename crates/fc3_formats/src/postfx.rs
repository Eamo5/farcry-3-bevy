//! Gameplay effect names resolve through the entity-library PostFX database,
//! not directly through cinematic sequence names. Keep the authored fades and
//! priority alongside that mapping; a similarly named sequence is not an alias.

use anyhow::{Context, Result, ensure};
use crate::fcb::Object;

pub const DATABASE: &str = "PostFXs:PostFXs/Database";

#[derive(Debug, Clone, PartialEq)]
pub struct Fade {
    pub kind: u32,
    pub time: f32,
    pub sequence: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub name: String,
    pub id: u32,
    pub priority: f32,
    pub sequence: String,
    /// Preserve the native selector until each intensity mode is implemented.
    pub intensity_control: u32,
    pub fade_in: Fade,
    pub fade_out: Fade,
}

fn fade(o: &Object, name: &str) -> Result<Fade> {
    let o = o.children.iter().find(|o| o.is(name)).with_context(|| format!("missing {name}"))?;
    let time = o.f32("fFadeTime").context("missing fade time")?;
    ensure!(time.is_finite() && time >= 0.0, "invalid fade time");
    Ok(Fade {
        kind: o.u32("selFadeType").context("missing fade type")?, time,
        sequence: o.str("pfxSequence").filter(|s| !s.is_empty()),
    })
}

pub fn parse_library(bytes: &[u8]) -> Result<Vec<Effect>> {
    let root = crate::fcb::parse(bytes)?;
    let mut database = None;
    root.walk(&mut |o, _| {
        if o.is("Entity") && o.str("hidName").as_deref() == Some(DATABASE) {
            database = Some(o.clone());
        }
    });
    let database = database.context("entity library has no PostFX database")?;
    let mut entries = Vec::new();
    database.walk(&mut |o, _| {
        if o.name_hash == 0x824a23ba { entries.push(o.clone()); }
    });
    entries.iter().map(|o| {
        let priority = o.f32("fPriority").context("missing effect priority")?;
        ensure!(priority.is_finite(), "invalid effect priority");
        Ok(Effect {
            name: o.str_h(0x79df125a).context("missing effect name")?,
            id: o.u32("sFX").context("missing effect ID")?, priority,
            sequence: o.str("pfxLoopSequence").context("missing effect sequence")?,
            intensity_control: o.u32("selIntensityControl").context("missing intensity selector")?,
            fade_in: fade(o, "FadeIn")?, fade_out: fade(o, "FadeOut")?,
        })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires installed archives and FC3_TEST_CACHE after full conversion"]
    fn installed_blur_masks_are_cached_without_replacing_authored_pixels() {
        let root = std::path::PathBuf::from(std::env::var_os("FC3_TEST_CACHE").expect("FC3_TEST_CACHE"));
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let sequences = crate::cinema::parse_postfx_library(
            std::str::from_utf8(&vfs.read(crate::cinema::POSTFX_LIBRARY).unwrap()).unwrap()).unwrap();
        let masks: std::collections::BTreeSet<_> = sequences.into_iter().flat_map(|s| s.groups).flat_map(|g| g.clips)
            .filter_map(|c| match c.kind {
                crate::cinema::ClipKind::Blur { texture, .. } if !texture.is_empty() => Some(texture.to_ascii_lowercase()),
                _ => None,
            }).collect();
        assert!(masks.contains(r"graphics\postfx\tunnel.xbt"));
        assert!(masks.contains(r"graphics\postfx\mushbuzz_vignette_01.xbt"));
        for path in masks {
            let main = vfs.read(&path).unwrap();
            let xbt = crate::texture::parse(&main).unwrap();
            let mip0 = xbt.mip0_path.as_deref().map(|p| vfs.read(p).unwrap());
            let expected = crate::texture::assemble_dds(&main, mip0.as_deref()).unwrap();
            let actual = std::fs::read(root.join(crate::cache::texture_rel(&path))).unwrap();
            assert_eq!(actual, expected, "mask {path}");
        }
    }

    #[test]
    #[ignore = "requires locally installed Far Cry 3 archives"]
    fn installed_vine_effect_names_resolve_to_authored_sequences_and_fades() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let effects = parse_library(&vfs.read(crate::progression::ENTITY_LIBRARY).unwrap()).unwrap();
        let sequences = crate::cinema::parse_postfx_library(
            std::str::from_utf8(&vfs.read(crate::cinema::POSTFX_LIBRARY).unwrap()).unwrap()).unwrap();
        let dolly = effects.iter().find(|e| e.name == "dolly_zoom").unwrap();
        assert_eq!(dolly.id, 0x730fb317);
        assert_eq!(dolly.priority, 1.0);
        assert_eq!(dolly.intensity_control, 2);
        assert_eq!(dolly.fade_in.kind, 0);
        assert_eq!(dolly.fade_out.kind, 0);
        assert!(dolly.sequence.eq_ignore_ascii_case("postfx:Mushroom_Sequence.Vine_Climbing"));
        let transition = effects.iter().find(|e| e.name == "mush_sequence_transitions").unwrap();
        assert_eq!(transition.id, 0x9ede26c1);
        assert_eq!(transition.priority, 1000.0);
        assert_eq!(transition.sequence, "postfx:mush_postfx.transition");
        assert_eq!(transition.fade_in.kind, 1);
        assert_eq!(transition.fade_in.sequence.as_deref(), Some("postfx:mush_postfx.transition_IN"));
        assert_eq!(transition.fade_out.kind, 1);
        assert_eq!(transition.fade_out.sequence.as_deref(), Some("postfx:mush_postfx.transition_OUT"));
        for effect in [dolly, transition] {
            for name in std::iter::once(&effect.sequence).chain(effect.fade_in.sequence.iter()).chain(effect.fade_out.sequence.iter()) {
                assert!(sequences.iter().any(|s| s.name.eq_ignore_ascii_case(name)), "missing {name}");
            }
        }
    }
}
