//! Light switches: what a placed object's slots wear with its light switched
//! on (its `MaterialLightSwitch`, baked as its placement's light set). Scripts
//! switch one (`CAPI_Entity.LightMaterialOveride`): an outpost's flag goes
//! from the pirates' red to the rebels' blue as the outpost is taken. Every
//! liberated outpost's flag is switched on with it, so it stays blue after a
//! load (the flag controller only switches it as it plays the taking).

use std::collections::HashSet;

use bevy::prelude::*;

use crate::objects::PlacedId;
use crate::original_shaders::{OriginalMaterial, OriginalSwap, SwapNextFrame};

pub struct LightSwitchPlugin;

impl Plugin for LightSwitchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LightSwitches>().add_systems(Update, switch_lights);
    }
}

/// The placed entities scripts have switched on.
#[derive(Resource, Default)]
pub struct LightSwitches(pub HashSet<u64>);

/// A placed object with a light switch, and whether it's on.
#[derive(Component, Default)]
pub struct LightSwitch {
    pub on: bool,
}

/// An outpost's flag (its placed entity's name), switched on while its
/// outpost is liberated.
#[derive(Component)]
pub struct OutpostFlag;

/// One of a switched object's parts: its material with the light off and on.
#[derive(Component)]
pub struct SwitchedPart {
    pub off: Handle<StandardMaterial>,
    pub lit: Handle<StandardMaterial>,
}

/// Marks a placed object an outpost's flag by its entity's name.
pub fn tag(commands: &mut Commands, obj: Entity, name: &str) {
    if name.to_ascii_lowercase().starts_with("flag") {
        commands.entity(obj).insert(OutpostFlag);
    }
}

/// How far (m) past an outpost's edge its flag may stand.
const FLAG_REACH: f32 = 40.0;

fn switch_lights(
    switches: Res<LightSwitches>,
    outposts: Res<crate::outposts::Outposts>,
    mut objects: Query<(&PlacedId, &mut LightSwitch, &GlobalTransform, Has<OutpostFlag>, &Children)>,
    parts: Query<&SwitchedPart>,
    mut commands: Commands,
) {
    for (id, mut switch, at, flag, children) in &mut objects {
        let at = at.translation();
        let taken = || outposts.list.iter().any(|o| o.liberated && Vec2::new(at.x - o.centre.x, at.z - o.centre.z).length() < o.radius + FLAG_REACH);
        let want = switches.0.contains(&id.0) || (flag && taken());
        if want == switch.on {
            continue;
        }
        switch.on = want;
        debug!("light switch: {} {}{}", id.0, if want { "on" } else { "off" }, if flag { " (an outpost's flag)" } else { "" });
        for c in children.iter() {
            let Ok(part) = parts.get(c) else { continue };
            // (Off its materials this frame, on the other the next: the
            // original shaders take it up again in the new one.)
            let next = if want { part.lit.clone() } else { part.off.clone() };
            commands.entity(c).remove::<(MeshMaterial3d<StandardMaterial>, OriginalSwap, MeshMaterial3d<OriginalMaterial>)>().insert(SwapNextFrame(next));
        }
    }
}
