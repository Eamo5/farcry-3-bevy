//! Authored particle definitions. Keep the source emitter tree: particle
//! curves use paired knots and their own tangent metadata, not cinema keys.

use anyhow::{Context, Result, ensure};
use crate::xml::Element;

pub const VINE_CACHE_REL: &str = "particles/vine-puff.xml";
/// The player's water on the lens (coming up out of the water): the effect
/// the player prototype names in its `ScreenDropletParticle`.
pub const SCREEN_DROPLETS_REL: &str = "particles/screen-droplets.xml";

/// Authored effects the game plays as the game did (`fc3_game::authored_fx`),
/// each cached whole with its textures: the race gates' flares.
pub const AUTHORED_EFFECTS: &[&str] = &["fc3_gameplay_exotic.racing_challenge.waypointflare", "fc3_gameplay_exotic.racing_challenge.finishflare"];

/// Where an authored effect is cached, by its name.
pub fn authored_rel(name: &str) -> String {
    format!("particles/{}.xml", crate::cache::hash_name(&name.to_ascii_lowercase()))
}

/// The particle effect the player's lens drops come from: the player
/// prototype's `ScreenDropletParticle` `psScreenDroplet`.
pub fn player_screen_droplets(library: &crate::fcb::Object) -> Option<String> {
    let mut out = None;
    library.walk(&mut |o, _| {
        if out.is_some() || !o.is("EntityPrototype") {
            return;
        }
        let Some(player) = o.child("Entity").filter(|e| e.str("hidName").as_deref() == Some("player:MainCharacter/PawnPlayer")) else { return };
        player.walk(&mut |c, _| {
            if out.is_none() && c.is("ScreenDropletParticle") {
                out = c.str("psScreenDroplet").filter(|s| s.len() > 1);
            }
        });
    });
    out
}

/// Locate one complete definition without constructing the entire (45 MB)
/// library tree. PartSys records are siblings in the installed library.
pub fn effect_xml<'a>(xml: &'a str, name: &str) -> Result<&'a str> {
    let mut offset = 0;
    while let Some(at) = xml[offset..].find("<PartSys") {
        let start = offset + at;
        offset = start + "<PartSys".len();
        if !xml[offset..].starts_with(char::is_whitespace) { continue; }
        let head_end = offset + xml[offset..].find('>').context("unterminated PartSys header")? + 1;
        let header = crate::xml::parse(&format!("{}</PartSys>", &xml[start..head_end])).context("invalid PartSys header")?;
        let end = head_end + xml[head_end..].find("</PartSys>").context("unterminated PartSys record")? + "</PartSys>".len();
        if header.attr("Name").is_some_and(|n| n.eq_ignore_ascii_case(name)) { return Ok(&xml[start..end]); }
        offset = end;
    }
    anyhow::bail!("particle effect {name} not found")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Emitter {
    pub name: String,
    pub active: bool,
    pub texture: String,
    pub frames: [u32; 2],
    pub fps: f32,
    pub fps_variant: f32,
    pub duration: f32,
    pub delay: f32,
    pub looping: bool,
    pub initial_count: u32,
    pub offset: [f32; 3],
    /// Complete flags, curves, knots, vertices and unsupported metadata.
    pub source: Element,
}

impl Emitter {
    /// A base value is not the evaluated curve: RatioUI and VariantKF must
    /// still be interpreted by the particle simulator.
    pub fn base_value(&self, curve: &str) -> Option<f32> {
        self.source.child(curve)?.child("ValueKF")?.f32("Value").filter(|v| v.is_finite())
    }

    pub fn texture_path(&self) -> Result<String> {
        archive_texture(&self.texture)
    }

    /// The archive path of another of the emitter's textures (its
    /// `NormalTexture`, `DistortionTexture`, `EnvMapTexture`), if it has one.
    pub fn texture_of(&self, attribute: &str) -> Option<Result<String>> {
        self.source.attr(attribute).filter(|t| !t.is_empty()).map(archive_texture)
    }
}

fn archive_texture(texture: &str) -> Result<String> {
    let (stem, extension) = texture.rsplit_once('.').context("particle texture lacks extension")?;
    ensure!(["png", "dds", "xbt"].iter().any(|e| extension.eq_ignore_ascii_case(e)), "unsupported particle texture extension");
    Ok(format!("{}.xbt", stem.to_ascii_lowercase()))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub name: String,
    pub id: u64,
    pub priority: u32,
    pub emitters: Vec<Emitter>,
}

impl Effect {
    pub fn parse(xml: &str) -> Result<Self> {
        let root = crate::xml::parse(xml).context("invalid particle XML")?;
        ensure!(root.name == "PartSys", "particle definition must be a PartSys");
        let emitters = root.children.iter().filter(|e| e.name == "PartEmit").map(|e| -> Result<Emitter> {
            let number = |name: &str| -> Result<f32> {
                let value = e.f32(name).with_context(|| format!("missing emitter {name}"))?;
                ensure!(value.is_finite() && value >= 0.0, "invalid emitter {name}");
                Ok(value)
            };
            // (No grid is a single image, as the glows have it.)
            let frames = [e.u32("FrameCount").context("missing frame columns")?.max(1), e.u32("FrameCountY").context("missing frame rows")?.max(1)];
            ensure!(frames[0].checked_mul(frames[1]).is_some(), "invalid frame grid");
            let offset: Vec<f32> = e.attr("PosOffset").context("missing particle offset")?.split(',')
                .map(|s| s.trim().parse()).collect::<Result<_, _>>()?;
            ensure!(offset.len() == 3 && offset.iter().all(|v| v.is_finite()), "invalid particle offset");
            Ok(Emitter {
                name: e.attr("Name").context("missing emitter name")?.into(), active: e.bool("Active").unwrap_or(false),
                texture: e.attr("DiffuseTexture").context("missing particle texture")?.into(), frames,
                fps: number("FramePerSecond")?, fps_variant: number("FramePerSecondVariant")?,
                duration: number("EmitDuration")?, delay: number("SpawnDelay")?, looping: e.bool("EmitLoop").unwrap_or(false),
                initial_count: e.u32("NbInitPart").context("missing initial particle count")?,
                offset: offset.try_into().unwrap(), source: e.clone(),
            })
        }).collect::<Result<Vec<_>>>()?;
        ensure!(!emitters.is_empty(), "particle definition has no emitters");
        Ok(Self { name: root.attr("Name").context("missing particle name")?.into(),
            id: root.u64("disLibItemId").context("missing particle ID")?, priority: root.u32("Priority").context("missing particle priority")?, emitters })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_matches_full_names_and_preserves_raw_curve_data() {
        let xml = "<Library><PartSys Name='puff_extra'><PartEmit/></PartSys><PartSys Name='puff'><Unknown a='1'/></PartSys></Library>";
        assert_eq!(effect_xml(xml, "PUFF").unwrap(), "<PartSys Name='puff'><Unknown a='1'/></PartSys>");
        assert!(effect_xml(xml, "puf").is_err());
        assert!(effect_xml("<PartSys Name='puff'>", "puff").is_err());
    }

    #[test]
    fn an_emitter_names_its_other_textures_by_archive_path() {
        let xml = r"<PartSys Name='drops' disLibItemId='1' Priority='2'><PartEmit Name='a' FrameCount='2' FrameCountY='1'
            PosOffset='0,0.5,0' FramePerSecond='0' FramePerSecondVariant='0' EmitDuration='1' SpawnDelay='0' NbInitPart='3'
            DiffuseTexture='fx\Drop.png' NormalTexture='fx\Drop_N.png' EnvMapTexture=''/></PartSys>";
        let e = &Effect::parse(xml).unwrap().emitters[0];
        assert_eq!(e.texture_path().unwrap(), r"fx\drop.xbt");
        assert_eq!(e.texture_of("NormalTexture").unwrap().unwrap(), r"fx\drop_n.xbt");
        assert!(e.texture_of("EnvMapTexture").is_none());
        assert!(e.texture_of("DistortionTexture").is_none());
    }

    #[test]
    #[ignore = "requires installed Far Cry 3 archives"]
    fn installed_vine_puff_resolves_gsf_reference_and_preserves_particle_curves() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let settings = crate::gsf::VineSettings::parse(&vfs.read(crate::gsf::LIBRARY).unwrap()).unwrap();
        let library = vfs.read(crate::lights::PARTICLE_LIBRARY).unwrap();
        let xml = effect_xml(std::str::from_utf8(&library).unwrap(), &settings.teleport_particle).unwrap();
        let effect = Effect::parse(xml).unwrap();
        assert_eq!(effect.name, settings.teleport_particle);
        assert_eq!(effect.id, 7667675800744);
        assert_eq!(effect.emitters.len(), 1);
        let e = &effect.emitters[0];
        assert!(e.active && !e.looping);
        assert_eq!(e.frames, [5, 5]);
        assert_eq!((e.fps, e.fps_variant, e.duration, e.delay, e.initial_count), (10.0, 2.0, 1.0, 0.0, 1));
        assert_eq!(e.offset, [0.0, 0.0, 0.7]);
        assert_eq!(e.base_value("EmitRateKF"), Some(4.0));
        assert_eq!(e.base_value("InitLifeTimeKF"), Some(10.0));
        assert_eq!(e.base_value("InitSpeedKF"), Some(0.2));
        let alpha = e.source.child("PartAlphaKF").unwrap();
        assert_eq!(alpha.children.len(), 6);
        assert_eq!(alpha.children[1], alpha.children[2]); // Paired knots must not be deduplicated.
        assert_eq!(e.source.child("VertexBuffer").unwrap().children.len(), 6);
        assert_eq!(e.source.child("Flags").unwrap().bool("LocalEmitterSpace"), Some(true));
        assert!(vfs.exists(&e.texture_path().unwrap()));
        assert_eq!(Effect::parse(&xml.replace("FrameCount=\"5\"", "FrameCount=\"0\"")).unwrap().emitters[0].frames, [1, 5]);
    }

    #[test]
    #[ignore = "requires installed archives and FC3_TEST_CACHE after full conversion"]
    fn installed_vine_particle_cache_keeps_exact_xml_and_atlas_pixels() {
        let root = std::path::PathBuf::from(std::env::var_os("FC3_TEST_CACHE").expect("FC3_TEST_CACHE"));
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let settings = crate::gsf::VineSettings::parse(&vfs.read(crate::gsf::LIBRARY).unwrap()).unwrap();
        let library = vfs.read(crate::lights::PARTICLE_LIBRARY).unwrap();
        let expected = effect_xml(std::str::from_utf8(&library).unwrap(), &settings.teleport_particle).unwrap();
        let actual = std::fs::read_to_string(root.join(VINE_CACHE_REL)).unwrap();
        assert_eq!(actual, expected);
        for emitter in Effect::parse(&actual).unwrap().emitters.iter().filter(|e| e.active) {
            let path = emitter.texture_path().unwrap();
            let main = vfs.read(&path).unwrap();
            let xbt = crate::texture::parse(&main).unwrap();
            let mip0 = xbt.mip0_path.as_deref().map(|p| vfs.read(p).unwrap());
            let expected = crate::texture::assemble_dds(&main, mip0.as_deref()).unwrap();
            assert_eq!(std::fs::read(root.join(crate::cache::texture_rel(&path))).unwrap(), expected);
        }
    }
}
