//! The tattoo on the player's arm: it grows as skills are learned.
//!
//! The game's arm materials multiply the skin by the tattoo's design on the
//! second UV set; the engine reveals only the pieces the player has earned.
//! Each pixel of the tattoo's mask holds the id of the piece it belongs to,
//! and the skill tree (`fc3_formats::progression::TattooPart`) says which ids
//! each part covers: a skill part shows one id per ability of the part
//! learned, the story's pieces (Dennis's, Citra's, the bosses') show once the
//! scripts open them, and the relics part shows one dot per relic found.
//!
//! Materials that use the tattoo's design (marked as they load) are swapped
//! for [`TattooMaterial`], the standard material with the revealed pieces
//! multiplied in. `CAPI_Player.ShowTattoo` hides or shows it all.

use std::collections::HashSet;
use std::sync::Mutex;

use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};

use crate::cache_assets::cache_path;
use crate::progression::{Progress, Skills};

pub struct TattooPlugin;

impl Plugin for TattooPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<TattooMaterial>::default())
            .init_resource::<TattooShown>()
            .init_resource::<TattooArt>()
            .add_systems(Update, (load_art, update_reveal, swap_materials).chain())
            .add_systems(PreUpdate, crate::original_shaders::finish_swaps::<TattooMaterial>);
        bevy::asset::embedded_asset!(app, "tattoo.wgsl");
    }
}

pub type TattooMaterial = ExtendedMaterial<StandardMaterial, TattooExt>;

/// The pieces shown: a bit per mask id (256).
#[derive(Clone, Copy, Debug, Default, ShaderType, PartialEq)]
pub struct TattooReveal {
    pub bits: [UVec4; 2],
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone, Default)]
pub struct TattooExt {
    #[uniform(100)]
    pub reveal: TattooReveal,
    #[texture(101)]
    #[sampler(102)]
    pub design: Option<Handle<Image>>,
    #[texture(103)]
    pub mask: Option<Handle<Image>>,
}

impl MaterialExtension for TattooExt {
    fn fragment_shader() -> bevy::shader::ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("tattoo.wgsl")).with_source("embedded").into()
    }
}

/// Material assets (by asset path) whose shader draws the tattoo, as the
/// material loader finds them.
pub static TATTOO_MATERIALS: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// Material assets whose shader would draw the downloadable tribal tattoo:
/// they keep their standard material (the skin without it).
pub static DLC_TATTOO_MATERIALS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// A part drawn by this module's material, or kept standard: never put on
/// the original shaders (which would draw the whole tattoo).
#[derive(Component)]
pub struct NoOriginal;

/// Notes a material that draws the tattoo's design (its original samplers
/// name the design's texture).
pub fn note_material(path: &str, samplers: &[(u8, String, bool)]) {
    let design = fc3_formats::cache::texture_rel(fc3_formats::progression::TATTOO_IMAGE);
    if samplers.iter().any(|s| s.1 == design) {
        debug!("tattoo: material {path} draws the tattoo");
        TATTOO_MATERIALS.lock().unwrap().get_or_insert_with(HashSet::new).insert(path.to_string());
    }
    let dlc = fc3_formats::cache::texture_rel(fc3_formats::progression::TATTOO_DLC_IMAGE);
    if samplers.iter().any(|s| s.1 == dlc) {
        debug!("tattoo: material {path} would draw the downloadable tattoo");
        DLC_TATTOO_MATERIALS.lock().unwrap().get_or_insert_with(HashSet::new).insert(path.to_string());
    }
}

/// Whether the tattoo shows at all (the scripts hide it, e.g. before Dennis
/// gives it).
#[derive(Resource)]
pub struct TattooShown(pub bool);

impl Default for TattooShown {
    fn default() -> Self {
        TattooShown(true)
    }
}

/// The design and mask textures, and the pieces shown now.
#[derive(Resource, Default)]
struct TattooArt {
    design: Option<Handle<Image>>,
    mask: Option<Handle<Image>>,
    reveal: TattooReveal,
}

fn load_art(mut art: ResMut<TattooArt>, server: Res<AssetServer>) {
    if art.design.is_some() {
        return;
    }
    art.design = Some(
        server
            .load_builder()
            // (Wrapping, as the game samples it: the arm's second UVs run
            // past the 0..1 square.)
            .with_settings(|s: &mut ImageLoaderSettings| {
                s.is_srgb = true;
                s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                    address_mode_u: ImageAddressMode::Repeat,
                    address_mode_v: ImageAddressMode::Repeat,
                    mag_filter: ImageFilterMode::Linear,
                    min_filter: ImageFilterMode::Linear,
                    mipmap_filter: ImageFilterMode::Linear,
                    ..default()
                });
            })
            .load(cache_path(&fc3_formats::cache::texture_rel(fc3_formats::progression::TATTOO_IMAGE))),
    );
    // Ids, read exactly: no filtering, no colour conversion.
    art.mask = Some(
        server
            .load_builder()
            .with_settings(|s: &mut ImageLoaderSettings| {
                s.is_srgb = false;
                s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                    mag_filter: ImageFilterMode::Nearest,
                    min_filter: ImageFilterMode::Nearest,
                    ..default()
                });
            })
            .load(cache_path(&fc3_formats::cache::texture_rel(fc3_formats::progression::TATTOO_MASK))),
    );
}

/// The mask ids shown for this progress (see the module notes).
pub fn revealed(progress: &Progress, skills: &Skills) -> TattooReveal {
    let mut r = TattooReveal::default();
    let mut set = |id: u8| r.bits[(id / 128) as usize][((id / 32) % 4) as usize] |= 1 << (id % 32);
    for part in &skills.0.tattoo {
        let learned = skills.0.abilities.iter().filter(|a| a.tattoo_part == part.name);
        let owned = learned.clone().count() > 0;
        let shown = if progress.whole_tattoo {
            part.ids.len()
        } else if owned {
            learned.filter(|a| progress.has(&a.id)).count()
        } else if part.name.eq_ignore_ascii_case("Relics") {
            progress.relics as usize
        } else if progress.upgradable.contains(&part.name) || !progress.story {
            // The story's pieces (all of them in free roam).
            part.ids.len()
        } else {
            0
        };
        for &id in part.ids.iter().take(shown) {
            set(id);
        }
    }
    r
}

fn update_reveal(
    progress: Res<Progress>,
    skills: Res<Skills>,
    shown: Res<TattooShown>,
    mut art: ResMut<TattooArt>,
    mut materials: ResMut<Assets<TattooMaterial>>,
) {
    if !(progress.is_changed() || skills.is_changed() || shown.is_changed()) {
        return;
    }
    let reveal = if shown.0 { revealed(&progress, &skills) } else { TattooReveal::default() };
    if reveal == art.reveal {
        return;
    }
    art.reveal = reveal;
    for (_, m) in materials.iter_mut() {
        m.extension.reveal = reveal;
    }
}

/// Swaps the standard material of meshes that draw the tattoo for the
/// tattooed one (once per material).
fn swap_materials(
    mut commands: Commands,
    server: Res<AssetServer>,
    art: Res<TattooArt>,
    standard: Res<Assets<StandardMaterial>>,
    mut materials: ResMut<Assets<TattooMaterial>>,
    meshes: Query<(Entity, &MeshMaterial3d<StandardMaterial>), Added<MeshMaterial3d<StandardMaterial>>>,
    mut made: Local<std::collections::HashMap<AssetId<StandardMaterial>, Handle<TattooMaterial>>>,
) {
    let marked = TATTOO_MATERIALS.lock().unwrap();
    let dlc = DLC_TATTOO_MATERIALS.lock().unwrap();
    for (e, m) in &meshes {
        let Some(path) = server.get_path(m.0.id()) else { continue };
        let path = path.to_string();
        // (Off the original shaders, whichever got there first.)
        if dlc.as_ref().is_some_and(|d| d.contains(&path)) {
            commands
                .entity(e)
                .insert(NoOriginal)
                .remove::<(crate::original_shaders::OriginalSwap, MeshMaterial3d<crate::original_shaders::OriginalMaterial>)>();
            continue;
        }
        if !marked.as_ref().is_some_and(|m| m.contains(&path)) {
            continue;
        }
        let Some(base) = standard.get(&m.0) else { continue };
        debug!("tattoo: on {e:?} ({path})");
        let handle = made
            .entry(m.0.id())
            .or_insert_with(|| {
                materials
                    .add(TattooMaterial { base: base.clone(), extension: TattooExt { reveal: art.reveal, design: art.design.clone(), mask: art.mask.clone() } })
            })
            .clone();
        crate::original_shaders::swap_next_frame(&mut commands, e, handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::progression::{Ability, SkillTree, TattooPart};

    #[test]
    fn pieces_follow_skills_story_and_relics() {
        let tree = SkillTree {
            abilities: vec![
                Ability { id: "A".into(), tattoo_part: "Heron1".into(), ..Default::default() },
                Ability { id: "B".into(), tattoo_part: "Heron1".into(), ..Default::default() },
            ],
            tattoo: vec![
                TattooPart { name: "Heron1".into(), ids: vec![9, 10] },
                TattooPart { name: "Dennis1".into(), ids: vec![1] },
                TattooPart { name: "Relics".into(), ids: vec![62, 63, 64] },
            ],
            ..Default::default()
        };
        let skills = Skills(tree);
        let bit = |r: &TattooReveal, id: u8| r.bits[(id / 128) as usize][((id / 32) % 4) as usize] >> (id % 32) & 1 == 1;
        let mut p = Progress { story: true, ..Default::default() };
        let r = revealed(&p, &skills);
        assert!(!bit(&r, 1) && !bit(&r, 9) && !bit(&r, 62));
        p.upgradable.insert("Dennis1".into());
        p.skills.insert("B".into());
        p.relics = 2;
        let r = revealed(&p, &skills);
        assert!(bit(&r, 1) && bit(&r, 9) && !bit(&r, 10) && bit(&r, 62) && bit(&r, 63) && !bit(&r, 64));
    }
}
