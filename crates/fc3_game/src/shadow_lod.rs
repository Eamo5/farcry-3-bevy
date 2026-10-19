//! Keeping the sun's shadow pass affordable.
//!
//! Every distinct non-bindless material in the cascades is a batch of its
//! own in each cascade, and each translated original material is its own
//! pipeline and bind group: a few hundred props and terrain sectors on the
//! original shaders meant a few hundred batches per cascade, which cost more
//! than everything else in the frame. So they cast through proxies: the same
//! mesh with one shared material, on a render layer only the sun sees, which
//! all batch together. Cut-out surfaces keep casting themselves (their
//! shadow needs the diffuse map's alpha), as do skinned meshes.
//!
//! Small casters also stop casting a little way out, as in the original,
//! whose small props and bushes drop their shadows long before trees and
//! buildings do.

use bevy::camera::visibility::{RenderLayers, VisibilityRange};
use bevy::light::NotShadowCaster;
use bevy::prelude::*;

use crate::original_shaders::OriginalMaterial;
use crate::settings::{Quality, Settings};

pub struct ShadowLodPlugin;

impl Plugin for ShadowLodPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, |mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>| {
            commands.insert_resource(ProxyMaterial(materials.add(StandardMaterial::default())));
        })
        .add_systems(
            PostUpdate,
            (update_proxies, follow_meshes, cull_shadow_casters).chain().before(bevy::camera::visibility::VisibilitySystems::CheckVisibility),
        );
    }
}

/// A shadow caster's world-space bounding radius; it casts only while the
/// camera is within `shadow_range` of it.
#[derive(Component, Clone, Copy)]
pub struct ShadowSize(pub f32);

/// Set on casters whose shadow is switched off for distance.
#[derive(Component)]
struct DistanceCulled;

/// The render layer only the sun sees: shadow proxies.
pub const SHADOW_LAYER: usize = 1;

/// The one material every proxy shares.
#[derive(Resource)]
struct ProxyMaterial(Handle<StandardMaterial>);

/// On a caster casting through a proxy: the proxy, and the caster's own size
/// (if it has one) to give back when it leaves the original shaders.
#[derive(Component)]
struct ShadowProxy {
    proxy: Entity,
    size: Option<f32>,
}

/// Gives opaque props and terrain on the original shaders a shadow proxy,
/// and takes it away again when they go back to their standard material.
#[allow(clippy::type_complexity)]
fn update_proxies(
    proxy_material: Res<ProxyMaterial>,
    originals: Res<Assets<OriginalMaterial>>,
    new: Query<
        (Entity, &Mesh3d, &MeshMaterial3d<OriginalMaterial>, Option<&ShadowSize>, Option<&VisibilityRange>, Has<NotShadowCaster>),
        (Without<ShadowProxy>, Without<bevy::mesh::skinning::SkinnedMesh>),
    >,
    old: Query<(Entity, &ShadowProxy), Without<MeshMaterial3d<OriginalMaterial>>>,
    mut commands: Commands,
) {
    // FC3_SHADOW_PROXY=0 (tests): every material casts itself.
    if std::env::var("FC3_SHADOW_PROXY").as_deref() == Ok("0") {
        return;
    }
    for (e, mesh, material, size, range, no_shadow) in &new {
        // Only casters (sized ones may be culled for distance just now).
        // Skinned meshes cast themselves: a proxy wouldn't follow the skin.
        if size.is_none() && no_shadow {
            continue;
        }
        // Cut-out surfaces cast themselves (their alpha shapes the shadow).
        let Some(m) = originals.get(&material.0) else { continue };
        if m.cutout.z > 0.0 {
            continue;
        }
        let mut proxy = commands.spawn((Mesh3d(mesh.0.clone()), MeshMaterial3d(proxy_material.0.clone()), RenderLayers::layer(SHADOW_LAYER), ChildOf(e)));
        if let Some(s) = size {
            proxy.insert(*s);
        }
        if let Some(r) = range {
            proxy.insert(r.clone());
        }
        let proxy = proxy.id();
        commands.entity(e).remove::<(ShadowSize, DistanceCulled)>().insert((NotShadowCaster, ShadowProxy { proxy, size: size.map(|s| s.0) }));
    }
    for (e, p) in &old {
        commands.entity(p.proxy).despawn();
        let mut c = commands.entity(e);
        c.remove::<(ShadowProxy, NotShadowCaster)>();
        if let Some(s) = p.size {
            c.insert(ShadowSize(s));
        }
    }
}

/// A caster's mesh changed (terrain changing its level of detail): so does
/// its proxy's.
fn follow_meshes(changed: Query<(&Mesh3d, &ShadowProxy), Changed<Mesh3d>>, mut proxies: Query<&mut Mesh3d, Without<ShadowProxy>>) {
    for (mesh, p) in &changed {
        if let Ok(mut m) = proxies.get_mut(p.proxy) {
            if m.0 != mesh.0 {
                m.0 = mesh.0.clone();
            }
        }
    }
}

/// Metres of shadow distance per metre of caster radius (High).
const RANGE_PER_METRE: f32 = 30.0;
/// The nearest any caster stops casting.
const MIN_RANGE: f32 = 40.0;
/// Casting resumes this much nearer than it stops, so a caster at the edge
/// doesn't flicker.
const HYSTERESIS: f32 = 0.9;

/// How far a caster of this world radius casts at a shadow quality.
pub fn shadow_range(radius: f32, quality: Quality) -> f32 {
    let scale = match quality {
        Quality::Low | Quality::Medium => 0.6,
        Quality::High => 1.0,
        Quality::VeryHigh => 1.5,
        Quality::Ultra => 2.0,
    };
    (radius * RANGE_PER_METRE * scale).max(MIN_RANGE * scale)
}

fn cull_shadow_casters(
    time: Res<Time<Real>>,
    mut next: Local<f32>,
    settings: Res<Settings>,
    cameras: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    casters: Query<(Entity, &GlobalTransform, &ShadowSize, Has<DistanceCulled>)>,
    mut commands: Commands,
) {
    // FC3_SHADOW_LOD=0 (tests): every caster casts at any distance.
    if std::env::var("FC3_SHADOW_LOD").as_deref() == Ok("0") {
        return;
    }
    let now = time.elapsed_secs();
    if now < *next && !settings.is_changed() {
        return;
    }
    *next = now + 0.25;
    let Ok(cam) = cameras.single() else { return };
    let eye = cam.translation();
    for (e, t, size, culled) in &casters {
        let range = shadow_range(size.0, settings.shadows);
        let d2 = t.translation().distance_squared(eye);
        if culled && d2 < (range * HYSTERESIS).powi(2) {
            commands.entity(e).remove::<(NotShadowCaster, DistanceCulled)>();
        } else if !culled && d2 > range * range {
            commands.entity(e).insert((NotShadowCaster, DistanceCulled));
        }
    }
}
