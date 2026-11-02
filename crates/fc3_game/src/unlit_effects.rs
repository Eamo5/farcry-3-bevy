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
            .init_resource::<GlowBlends>()
            .add_systems(Update, (swap_effects, blend_glows).chain())
            .add_systems(PreUpdate, crate::original_shaders::finish_swaps::<UnlitEffect>);
        bevy::asset::embedded_asset!(app, "unlit_effect.wgsl");
    }
}

/// Glows a script turns up or down (`CAPI_Entity.UpdateMaterialFloat` on
/// `HDRMul`, the `MaterialModifier` box: CIT_015's murals lit one after
/// another as Citra tells her story), by the placed entity: the multiplier
/// wanted and how much of the way there a second goes (0: at once).
#[derive(Resource, Default)]
pub struct GlowBlends {
    wanted: std::collections::HashMap<u64, (f32, f32)>,
}

impl GlowBlends {
    pub fn set(&mut self, entity: u64, value: f32, rate: f32) {
        self.wanted.insert(entity, (value, rate.max(0.0)));
    }

    pub fn reset(&mut self) {
        self.wanted.clear();
    }
}

/// A part's own copy of its effect material, made for a glow a script
/// turns: the multiplier it set out from and how far along it is.
#[derive(Component)]
struct OwnGlow {
    from: f32,
    to: f32,
    along: f32,
}

/// Moves each turned glow's multiplier to what its script wants, on its own
/// copy of the material (the parts sharing it elsewhere keep theirs).
fn blend_glows(
    time: Res<Time>,
    blends: Res<GlowBlends>,
    roots: Query<(Entity, &crate::objects::PlacedId)>,
    children: Query<&Children>,
    mut parts: Query<(Entity, &mut MeshMaterial3d<UnlitEffect>, Option<&mut OwnGlow>)>,
    mut effects: ResMut<Assets<UnlitEffect>>,
    mut commands: Commands,
) {
    if blends.wanted.is_empty() {
        return;
    }
    let dt = time.delta_secs();
    for (root, id) in &roots {
        let Some(&(value, rate)) = blends.wanted.get(&id.0) else { continue };
        for part in std::iter::once(root).chain(children.iter_descendants(root)) {
            let Ok((e, mut handle, own)) = parts.get_mut(part) else { continue };
            let Some(mut glow) = own else {
                // (Its own copy first, set out from the authored figure.)
                let Some(m) = effects.get(&handle.0).cloned() else { continue };
                let from = m.extension.params.shade.x;
                handle.0 = effects.add(m);
                commands.entity(e).insert(OwnGlow { from, to: from, along: 1.0 });
                continue;
            };
            // (A new wish: from where it is now.)
            if glow.to != value {
                let now = glow.from + (glow.to - glow.from) * glow.along;
                *glow = OwnGlow { from: now, to: value, along: 0.0 };
            }
            if glow.along < 1.0 {
                glow.along = if rate > 0.0 { (glow.along + rate * dt).min(1.0) } else { 1.0 };
                if let Some(mut m) = effects.get_mut(&handle.0) {
                    m.extension.params.shade.x = glow.from + (glow.to - glow.from) * glow.along;
                }
            }
        }
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

    /// A glow a script turns goes from its authored multiplier to the one
    /// wanted at its rate, on its own copy of the material.
    #[test]
    fn a_turned_glow_blends_on_its_own_copy() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).init_resource::<GlowBlends>().insert_resource(Assets::<UnlitEffect>::default());
        let shared = app.world_mut().resource_mut::<Assets<UnlitEffect>>().add(UnlitEffect {
            base: StandardMaterial::default(),
            extension: UnlitEffectExt { params: UnlitEffectParams { shade: Vec4::new(75.0, 0.0, 1.0, 0.0), scroll: Vec4::ZERO } },
        });
        let root = app.world_mut().spawn(crate::objects::PlacedId(7)).id();
        let part = app.world_mut().spawn((MeshMaterial3d(shared.clone()), ChildOf(root))).id();
        app.world_mut().resource_mut::<GlowBlends>().set(7, 5.0, 2.0);
        let mut step = |secs: f32, app: &mut App| {
            app.world_mut().resource_mut::<Time>().advance_by(std::time::Duration::from_secs_f32(secs));
            app.world_mut().run_system_once(blend_glows).unwrap();
            let h = app.world().get::<MeshMaterial3d<UnlitEffect>>(part).unwrap().0.clone();
            app.world().resource::<Assets<UnlitEffect>>().get(&h).unwrap().extension.params.shade.x
        };
        assert_eq!(step(0.0, &mut app), 75.0);
        assert!((step(0.25, &mut app) - 40.0).abs() < 0.01);
        assert!((step(1.0, &mut app) - 5.0).abs() < 0.01);
        // (The shared one is as it was.)
        assert_eq!(app.world().resource::<Assets<UnlitEffect>>().get(&shared).unwrap().extension.params.shade.x, 75.0);
        // At once, with no rate.
        app.world_mut().resource_mut::<GlowBlends>().set(7, 0.0, 0.0);
        assert_eq!(step(0.01, &mut app), 0.0);
    }

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
