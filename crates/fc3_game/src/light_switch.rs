//! Light switches: what a placed object's slots wear with its light switched
//! on (its `MaterialLightSwitch`, baked as its placement's light set). Scripts
//! switch one (`CAPI_Entity.LightMaterialOveride`): an outpost's flag goes
//! from the pirates' red to the rebels' blue as the outpost is taken. Every
//! liberated outpost's flag is switched on with it, so it stays blue after a
//! load (the flag controller only switches it as it plays the taking).

use std::collections::HashSet;

use bevy::camera::visibility::{NoFrustumCulling, VisibilityRange};
use bevy::light::NotShadowCaster;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::prelude::*;

use crate::objects::PlacedId;
use crate::original_shaders::{OriginalSwap, PropPart, SkinnedPart};
use crate::shadow_lod::{ShadowProxy, ShadowSize};

pub struct LightSwitchPlugin;

impl Plugin for LightSwitchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LightSwitches>().add_systems(Update, switch_lights);
    }
}

/// The placed entities scripts have switched on.
#[derive(Resource, Default)]
pub struct LightSwitches(pub HashSet<u64>);

/// A placed object with a light switch: whether it's on, and the materials
/// its parts swap (off, on). Matched by material, so the rig an animated
/// prop draws with (an outpost's waving flag) switches as its static parts
/// do.
#[derive(Component, Default)]
pub struct LightSwitch {
    pub on: bool,
    pub swaps: Vec<(Handle<StandardMaterial>, Handle<StandardMaterial>)>,
}

/// An outpost's flag (its placed entity's name), switched on while its
/// outpost is liberated.
#[derive(Component)]
pub struct OutpostFlag;

/// Marks a placed object an outpost's flag by its entity's name.
pub fn tag(commands: &mut Commands, obj: Entity, name: &str) {
    if name.to_ascii_lowercase().starts_with("flag") {
        commands.entity(obj).insert(OutpostFlag);
    }
}

/// How far (m) past an outpost's edge its flag may stand.
const FLAG_REACH: f32 = 40.0;

/// A part to put on again as a fresh entity: its mesh, place and how it's
/// drawn and casts.
#[derive(bevy::ecs::query::QueryData)]
struct PartCopy {
    mesh: &'static Mesh3d,
    parent: &'static ChildOf,
    transform: &'static Transform,
    visibility: Option<&'static Visibility>,
    range: Option<&'static VisibilityRange>,
    no_shadow: Has<NotShadowCaster>,
    size: Option<&'static ShadowSize>,
    proxy: Option<&'static ShadowProxy>,
    skin: Option<&'static SkinnedMesh>,
    prop: Has<PropPart>,
    skinned: Has<SkinnedPart>,
    unculled: Has<NoFrustumCulling>,
}

#[allow(clippy::type_complexity)]
fn switch_lights(
    switches: Res<LightSwitches>,
    outposts: Res<crate::outposts::Outposts>,
    mut objects: Query<(Entity, &PlacedId, &mut LightSwitch, &GlobalTransform, Has<OutpostFlag>, Option<Ref<crate::prop_anims::PropRig>>)>,
    tree: Query<&Children>,
    parts: Query<(Option<&MeshMaterial3d<StandardMaterial>>, Option<&OriginalSwap>, PartCopy)>,
    mut commands: Commands,
) {
    for (obj, id, mut switch, at, flag, rig) in &mut objects {
        let at = at.translation();
        let taken = || outposts.list.iter().any(|o| o.liberated && Vec2::new(at.x - o.centre.x, at.z - o.centre.z).length() < o.radius + FLAG_REACH);
        let want = switches.0.contains(&id.0) || (flag && taken());
        // (A rig just made wears the light off: switched again if it's on.)
        let fresh = rig.is_some_and(|r| r.is_added());
        if want == switch.on && !(fresh && want) {
            continue;
        }
        switch.on = want;
        debug!("light switch: {} {}{}", id.0, if want { "on" } else { "off" }, if flag { " (an outpost's flag)" } else { "" });
        let mut stack = vec![obj];
        while let Some(e) = stack.pop() {
            if let Ok(children) = tree.get(e) {
                stack.extend(children.iter());
            }
            let Ok((standard, original, copy)) = parts.get(e) else { continue };
            let Some(now) = standard.map(|m| m.0.id()).or(original.map(|o| o.standard.id())) else { continue };
            let Some((off, lit)) = switch.swaps.iter().find(|(off, lit)| if want { off.id() == now } else { lit.id() == now }) else { continue };
            let next = if want { lit.clone() } else { off.clone() };
            // (A fresh entity in its place: one that had been drawn with an
            // original material, given another, was drawn with the first's
            // still: the flag stayed red.)
            let (size, casts) = crate::shadow_lod::casting(copy.size, copy.proxy, copy.no_shadow);
            let mut part = commands.spawn((
                copy.mesh.clone(),
                MeshMaterial3d(next),
                *copy.transform,
                copy.visibility.copied().unwrap_or_default(),
                ChildOf(copy.parent.parent()),
            ));
            if let Some(r) = copy.range {
                part.insert(r.clone());
            }
            if let Some(s) = size {
                part.insert(s);
            }
            if !casts {
                part.insert(NotShadowCaster);
            }
            if let Some(skin) = copy.skin {
                part.insert(skin.clone());
            }
            if copy.prop {
                part.insert(PropPart);
            }
            if copy.skinned {
                part.insert(SkinnedPart);
            }
            if copy.unculled {
                part.insert(NoFrustumCulling);
            }
            commands.entity(e).despawn();
        }
    }
}