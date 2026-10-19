//! Authored particle definitions. Keep the source emitter tree: particle
//! curves use paired knots and their own tangent metadata, not cinema keys.

use anyhow::{Context, Result, ensure};
use crate::xml::Element;

pub const VINE_CACHE_REL: &str = "particles/vine-puff.xml";
/// The player's water on the lens (coming up out of the water): the effect
/// the player prototype names in its `ScreenDropletParticle`.
pub const SCREEN_DROPLETS_REL: &str = "particles/screen-droplets.xml";

/// Authored effects the game plays as the game did (`fc3_game::authored_fx`),
/// each cached whole with its textures: the race gates' flares, the blasts
/// the vehicles' engine fires (their archetypes' `EngineFire`: the smoking
/// stage and the burning one) and burning people's.
pub const AUTHORED_EFFECTS: &[&str] = &[
    "fc3_gameplay_exotic.racing_challenge.waypointflare",
    "fc3_gameplay_exotic.racing_challenge.finishflare",
    blast::FRAG,
    blast::ARROW,
    blast::ROCKET,
    blast::C4,
    blast::MINE,
    blast::MOLOTOV,
    blast::BURNING_CHEST,
    blast::UNDERWATER,
    blast::CAR,
    blast::BOAT,
    blast::HELI,
    blast::BARREL,
    blast::BARREL_DIRECTIONAL,
    blast::PROPANE,
    blast::LIQUID_PROPANE,
    blast::OXYGEN,
    "fc3_gameplay_vehicule_land.land_vehicule_4x4.engine_fire",
    "fc3_gameplay_vehicule_land.land_vehicule_4x4.engine_fire_burning",
    "fc3_gameplay_vehicule_land.land_vehicule_quad_bike.engine_fire",
    "fc3_gameplay_vehicule_land.land_vehicule_quad_bike.engine_fire_burning",
    "fc3_gameplay_vehicule_land.land_vehicule_japsedan.engine_fire",
    "fc3_gameplay_vehicule_land.land_vehicule_japsedan.engine_fire_burning",
    "fc3_gameplay_vehicule_land.land_vehicule_buggy.engine_fire",
    "fc3_gameplay_vehicule_land.land_vehicule_buggy.engine_fire_burning",
    "fc3_gameplay_vehicule_land.land_vehicule_cargotruck.engine_fire",
    "fc3_gameplay_vehicule_land.land_vehicule_cargotruck.engine_fire_burning",
    "fc3_gameplay_vehicule_sea.sea_vehicule_zodiac.engine_fire",
    "fc3_gameplay_vehicule_sea.sea_vehicule_jetski.engine_fire",
    "fc3_gameplay_vehicule_sea.sea_vehicule_patrolboat.engine_fire",
    "fc3_gameplay_fire.fc3_gameplay_pawnfire.fire_humantorch",
    "fc3_gameplay_fire.fc3_gameplay_pawnfire.water_smoke",
];

/// The blasts' effects: the weapons' and explosives' `psExplosion` (their
/// archetypes' `Particles`), the vehicles' engine explosions, the
/// helicopters' and the explosive props'.
pub mod blast {
    /// Frag grenades, thrown or launched (`Frag_Grenade`, `40mmGrenade`).
    pub const FRAG: &str = "fc3_grenade_impact.grenade_frag.grenade_frag_core";
    pub const ARROW: &str = "fc3_grenade_impact.grenade_arrow.grenade_arrow_core";
    pub const ROCKET: &str = "fc3_bullet_impact_rpg.bimp_rpg7.rpg7_core";
    /// C4 (`IED`).
    pub const C4: &str = "fc3_ied_impact.ied_explosion.exp_ied_core";
    pub const MINE: &str = "fc3_proximitymine_impact.proximitymine_explosion.exp_proximitymine_core";
    pub const MOLOTOV: &str = "fc3_weaponfire_generic.molotov.molotov_explosion";
    /// A burning man's chest going up (the molotov pirates' `CBurnComponent`).
    pub const BURNING_CHEST: &str = "fc3_weaponfire_generic.molotov.molotov_chestexplosion";
    /// Any of them in the water.
    pub const UNDERWATER: &str = "fc3_explosion_generic.exp_generic.generic_underwater_explosion";
    pub const CAR: &str = "fc3_gameplay_vehicule_land.land_vehicule_4x4.engine_explosion_newversion";
    pub const BOAT: &str = "fc3_gameplay_vehicule_sea.sea_vehicule_patrolboat.engine_explosion_newversion";
    pub const HELI: &str = "fc3_destruction_generic_props.explosion.choper_explosion";
    pub const BARREL: &str = "fc3_explosion_generic.exp_objects.barrel_explosion";
    pub const BARREL_DIRECTIONAL: &str = "fc3_explosion_generic.exp_objects.barrel_directional_explosion";
    pub const PROPANE: &str = "fc3_explosion_generic.exp_objects.propanetank01_explosion";
    pub const LIQUID_PROPANE: &str = "fc3_explosion_generic.exp_objects.liquidpropanetank_explosion";
    pub const OXYGEN: &str = "fc3_explosion_generic.exp_objects.oxygentank_explosion";

    /// An explosive prop's blast, by its entity name (`Explosives/...`, as
    /// its archetype has it).
    pub fn of_prop(name: &str) -> &'static str {
        let n = name.to_ascii_lowercase();
        if n.contains("oxygen") {
            OXYGEN
        } else if n.contains("liquidpropane") {
            LIQUID_PROPANE
        } else if n.contains("propane") {
            PROPANE
        } else if n.contains("directional") {
            BARREL_DIRECTIONAL
        } else {
            BARREL
        }
    }
}

/// The game's impacts per material (`materialimpacts.lib`): for each kind
/// of impact (`Weapon/Bullet`, `Weapon/MeleeSlash`, footsteps...), the
/// effect each logic material makes.
pub const MATERIAL_IMPACTS: &str = r"generated\databases\generic\materialimpacts.lib";
/// The kind of impact a bullet makes.
pub const BULLET_IMPACT: &str = "Weapon/Bullet";
/// The bullets' impacts, cached: one `<material id>\t<material>\t<effect>`
/// line each (the effects cached as authored effects).
pub const BULLET_IMPACTS_REL: &str = "particles/bullet-impacts.txt";

/// A logic material's effect for one kind of impact.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialImpact {
    /// The logic material's id (`disMatId`, its `disId` in
    /// `logicmaterials.lib`) and name (`Base/Dirt_NP`).
    pub material: u8,
    pub name: String,
    pub effect: String,
}

/// One kind of impact's effects, per logic material (those with one).
pub fn material_impacts(lib: &[u8], kind: &str) -> Result<Vec<MaterialImpact>> {
    // (The effect's name sits next to its id, `psParticlesSystem`.)
    const EFFECT_NAME: u32 = 0x3149_1B5F;
    let start = lib.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
    let root = crate::fcb::parse(&lib[start..])?;
    let entry = root.children.iter().find(|o| o.str("hidName").as_deref() == Some(kind)).with_context(|| format!("no impact kind {kind}"))?;
    let systems = entry.child("ParticlesSystems").context("impact kind without effects")?;
    Ok(systems
        .children
        .iter()
        .filter_map(|p| {
            let effect = p.str_h(EFFECT_NAME).map(|s| s.trim_end_matches('\0').to_string()).filter(|s| s.contains('.'))?;
            Some(MaterialImpact { material: *p.field("disMatId")?.first()?, name: p.str("disName")?.trim_end_matches('\0').to_string(), effect })
        })
        .collect())
}

/// Impacts as cached ([`BULLET_IMPACTS_REL`]).
pub fn impacts_to_text(impacts: &[MaterialImpact]) -> String {
    impacts.iter().map(|i| format!("{}\t{}\t{}\n", i.material, i.name, i.effect)).collect()
}

pub fn impacts_from_text(text: &str) -> Vec<MaterialImpact> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some(MaterialImpact { material: f.next()?.parse().ok()?, name: f.next()?.to_string(), effect: f.next()?.to_string() })
        })
        .collect()
}

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

    /// Its child emitters (`PartChildEmitUnit`s): each of its particles
    /// emits as they would. One that doesn't read is left out.
    pub fn children(&self) -> Vec<Emitter> {
        self.source.child("PartChildEmit").into_iter().flat_map(|c| &c.children).filter(|u| u.name == "PartChildEmitUnit")
            .filter_map(|u| Emitter::of(u).ok()).collect()
    }

    /// An emitter (`PartEmit`, or a child's `PartChildEmitUnit`) as it's defined.
    pub fn of(e: &Element) -> Result<Emitter> {
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
    /// The textures its active emitters (and their children) draw with:
    /// each one's sheet, and a heat haze's (`DistortionEnabled`) distortion
    /// map as well.
    pub fn textures(&self) -> Vec<Result<String>> {
        let mut out = Vec::new();
        let all: Vec<Emitter> = self.emitters.iter().flat_map(|e| std::iter::once(e.clone()).chain(e.children())).collect();
        for e in all.iter().filter(|e| e.active && !e.texture.is_empty()) {
            out.push(e.texture_path());
            if e.source.attr("DistortionEnabled") == Some("1") {
                out.extend(e.texture_of("DistortionTexture"));
            }
        }
        out
    }

    pub fn parse(xml: &str) -> Result<Self> {
        let root = crate::xml::parse(xml).context("invalid particle XML")?;
        ensure!(root.name == "PartSys", "particle definition must be a PartSys");
        let emitters = root.children.iter().filter(|e| e.name == "PartEmit").map(Emitter::of).collect::<Result<Vec<_>>>()?;
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
    fn cached_impacts_read_back() {
        let impacts = vec![
            MaterialImpact { material: 4, name: "Base/Dirt_NP".into(), effect: "fx.a.dirt".into() },
            MaterialImpact { material: 45, name: "Base/Asphalt_NP".into(), effect: "fx.a.concrete".into() },
        ];
        assert_eq!(impacts_from_text(&impacts_to_text(&impacts)), impacts);
    }

    #[test]
    fn explosive_props_name_their_cached_blasts() {
        assert_eq!(blast::of_prop("Explosives/Oxygen_Tank_01.TankA_0"), blast::OXYGEN);
        assert_eq!(blast::of_prop("Explosives/LiquidPropaneTank02_1"), blast::LIQUID_PROPANE);
        assert_eq!(blast::of_prop("Explosives/PropaneTank01_Propelled_9"), blast::PROPANE);
        assert_eq!(blast::of_prop("Explosives/BarrelDirectional_FC3_3"), blast::BARREL_DIRECTIONAL);
        assert_eq!(blast::of_prop("Explosives/ExplosiveBarrel_FC3_0"), blast::BARREL);
        for name in ["Explosives/Oxygen_Tank_01", "Explosives/LiquidPropaneTank02", "Explosives/PropaneTank02", "Explosives/BarrelDirectional", "x"] {
            assert!(AUTHORED_EFFECTS.contains(&blast::of_prop(name)));
        }
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
