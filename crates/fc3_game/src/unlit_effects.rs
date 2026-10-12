//! Additive `Unlit` effects (light shafts, glows) drawn as the game draws
//! them: the texture, dyed by `DiffuseColor1`, times `HDRMul`, faded toward
//! grazing angles by `NormalAttenuationPower` (|N·V| to that power, which
//! softens a shaft's edges to nothing) and moved by the material's UV
//! animation (`AnimType`: scrolling at `USpeed`/`VSpeed`, or turning at
//! `AngularSpeed`). The standard material stands in for every other part;
//! these are swapped over once their standard material is known to be an
//! additive or premultiplied `Unlit` one.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use fc3_formats::cache::CachedMaterial;
use fc3_formats::hash::name_hash;

pub struct UnlitEffectsPlugin;

impl Plugin for UnlitEffectsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<UnlitEffect>::default())
            .add_systems(Update, swap_effects)
            .add_systems(PreUpdate, crate::original_shaders::finish_swaps::<UnlitEffect>);
        bevy::asset::embedded_asset!(app, "unlit_effect.wgsl");
    }
}

pub type UnlitEffect = ExtendedMaterial<StandardMaterial, UnlitEffectExt>;

/// The effect's own figures.
#[derive(Clone, Copy, Debug, Default, ShaderType, PartialEq)]
pub struct UnlitEffectParams {
    /// `HDRMul`, `NormalAttenuationPower`, `AnimType`, `AngularSpeed`.
    pub shade: Vec4,
    /// `USpeed`, `VSpeed`.
    pub scroll: Vec4,
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone, Default)]
pub struct UnlitEffectExt {
    #[uniform(100)]
    pub params: UnlitEffectParams,
}

impl MaterialExtension for UnlitEffectExt {
    fn fragment_shader() -> bevy::shader::ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("unlit_effect.wgsl")).with_source("embedded").into()
    }
}

/// An `Unlit` material's effect figures, when it blends (additive or
/// premultiplied) rather than standing as a surface.
pub(crate) fn effect_params(cm: &CachedMaterial) -> Option<UnlitEffectParams> {
    if !matches!(crate::cache_assets::unlit_blend(&cm.shader, &cm.params), Some(AlphaMode::Add | AlphaMode::Premultiplied)) {
        return None;
    }
    if cm.flags & fc3_formats::cache::mat_flags::ALPHA_TEST != 0 {
        return None;
    }
    let raw = |name: &str| cm.params.iter().find(|(h, v)| *h == name_hash(name) && v.len() == 4).map(|(_, v)| <[u8; 4]>::try_from(&v[..4]).unwrap());
    let float = |name: &str, default: f32| raw(name).map_or(default, f32::from_le_bytes);
    let int = |name: &str, default: u32| raw(name).map_or(default, u32::from_le_bytes);
    Some(UnlitEffectParams {
        shade: Vec4::new(float("HDRMul", 1.0), float("NormalAttenuationPower", 0.0), int("AnimType", 1) as f32, float("AngularSpeed", 0.0)),
        scroll: Vec4::new(float("USpeed", 0.0), float("VSpeed", 0.0), 0.0, 0.0),
    })
}

/// Puts parts whose standard material is an unlit additive one on the effect
/// material (off the original shaders), once that material has loaded.
#[allow(clippy::type_complexity)]
fn swap_effects(
    mut commands: Commands,
    server: Res<AssetServer>,
    standard: Res<Assets<StandardMaterial>>,
    mut effects: ResMut<Assets<UnlitEffect>>,
    added: Query<(Entity, &MeshMaterial3d<StandardMaterial>), Added<MeshMaterial3d<StandardMaterial>>>,
    current: Query<&MeshMaterial3d<StandardMaterial>>,
    mut pending: Local<Vec<Entity>>,
    mut made: Local<std::collections::HashMap<AssetId<StandardMaterial>, Option<Handle<UnlitEffect>>>>,
) {
    // Only unlit additive stand-ins are candidates (a few among thousands).
    for (e, m) in &added {
        if made.get(&m.0.id()).is_some_and(|h| h.is_none()) {
            continue;
        }
        pending.push(e);
    }
    pending.retain(|&e| {
        let Ok(m) = current.get(e) else { return false };
        let Some(base) = standard.get(&m.0) else { return true };
        if !base.unlit || !matches!(base.alpha_mode, AlphaMode::Add | AlphaMode::Premultiplied) {
            made.insert(m.0.id(), None);
            return false;
        }
        let handle = made
            .entry(m.0.id())
            .or_insert_with(|| {
                let path = server.get_path(m.0.id())?;
                let bytes = std::fs::read(fc3_formats::cache::default_cache_root().join(path.path())).ok()?;
                let params = effect_params(&CachedMaterial::from_bytes(&bytes).ok()?)?;
                debug!("unlit effect: {path} {params:?}");
                Some(effects.add(UnlitEffect { base: base.clone(), extension: UnlitEffectExt { params } }))
            })
            .clone();
        if let Some(handle) = handle {
            crate::original_shaders::swap_next_frame(&mut commands, e, handle);
            commands.entity(e).insert(bevy::light::NotShadowCaster);
        }
        false
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_light_shaft_takes_its_multiplier_attenuation_and_scroll() {
        let f = |name: &str, v: f32| (name_hash(name), v.to_le_bytes().to_vec());
        let i = |name: &str, v: u32| (name_hash(name), v.to_le_bytes().to_vec());
        let params = vec![i("BlendingType", 2), f("HDRMul", 0.5), f("NormalAttenuationPower", 2.0), i("AnimType", 1), f("VSpeed", 0.25)];
        let shaft = CachedMaterial { shader: "Unlit".into(), params: params.clone(), ..Default::default() };
        let p = effect_params(&shaft).expect("an effect");
        assert_eq!(p.shade, Vec4::new(0.5, 2.0, 1.0, 0.0));
        assert_eq!(p.scroll, Vec4::new(0.0, 0.25, 0.0, 0.0));
        // Opaque, cut out or not `Unlit`: a surface, not an effect.
        let mut opaque = params.clone();
        opaque[0] = i("BlendingType", 0);
        assert!(effect_params(&CachedMaterial { params: opaque, ..shaft.clone() }).is_none());
        assert!(effect_params(&CachedMaterial { flags: fc3_formats::cache::mat_flags::ALPHA_TEST, ..shaft.clone() }).is_none());
        assert!(effect_params(&CachedMaterial { shader: "Generic_FC3".into(), ..shaft }).is_none());
    }
}
