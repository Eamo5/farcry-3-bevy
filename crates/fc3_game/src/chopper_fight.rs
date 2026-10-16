//! JAS_150's helicopter ride (`CChopperFightE3`): between its scene's
//! `GSF_Start` and `GSF_Stop` sync points the player, carried through the
//! ride by the scene, shoots with the gameplay's own pistol
//! (`m_weaponArchetype`, the 1911's, never out of rounds). The view turns
//! from where the scene looks, within the reticle's reach (`free_aim` on
//! the cinema), and the pistol fires where it points.

use bevy::prelude::*;

use crate::player::Player;
use crate::weapons::{Inventory, WeaponSlot};

pub struct ChopperFightPlugin;

impl Plugin for ChopperFightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChopperFight>()
            .add_systems(Update, run.after(crate::story::StorySet).before(crate::cinema::CinemaSet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The ride's gameplay the story starts and stops (by id).
#[derive(Resource, Default)]
pub struct ChopperFight {
    pub pending: Vec<u64>,
    pub cancelled: Vec<u64>,
    /// The gameplay under way, and the pistol lent for it.
    active: Option<(u64, Option<usize>)>,
}

impl ChopperFight {
    /// A checkpoint reload: over (the pistol given back).
    pub fn reset(&mut self) {
        self.pending.clear();
        if let Some((id, _)) = self.active {
            self.cancelled.push(id);
        }
    }
}

/// How far the view turns from the scene's (yaw, pitch; radians): the
/// reticle's reach over the screen (`ReticleXposLimit`, `ReticleYposLimit`).
const REACH: Vec2 = Vec2::new(0.6, 0.4);
/// The gameplay's library entry.
const GAMEPLAY: &str = "CChopperFightE3";

fn run(
    mut fight: ResMut<ChopperFight>,
    mut cinema: ResMut<crate::cinema::Cinema>,
    host: Option<NonSend<crate::story::StoryHost>>,
    info: Option<Res<crate::data::CacheInfo>>,
    mut players: Query<&mut Inventory, With<Player>>,
    (time, mut since, mut commands): (Res<Time>, Local<Option<f32>>, Commands),
) {
    let Ok(mut inv) = players.single_mut() else { return };
    let cancelled = std::mem::take(&mut fight.cancelled);
    for id in std::mem::take(&mut fight.pending) {
        if cancelled.contains(&id) {
            continue;
        }
        // The pistol: the library's archetype, ours by its family.
        let archetype = info.as_ref().and_then(|i| std::fs::read(i.root.join(fc3_formats::gsf::CACHE_REL)).ok()).and_then(|b| {
            let entry = fc3_formats::gsf::entry(&b, GAMEPLAY).ok()?;
            entry.str("m_weaponArchetype")
        });
        let def = archetype.as_deref().zip(host.as_ref()).and_then(|(a, h)| crate::story::weapon_def(&inv, a, &h.story.data()));
        let lent = def.filter(|_| inv.mounted_from.is_none()).map(|def| {
            let d = &inv.defs[def];
            let slot = WeaponSlot { def, mag: d.magazine, reserve: d.max_reserve };
            inv.mounted_from = Some(inv.current);
            inv.slots.push(slot);
            inv.current = inv.slots.len() - 1;
            inv.reloading = 0.0;
            inv.switching = 0.35;
            def
        });
        info!("chopper fight ({id}): {} lent ({archetype:?})", lent.map_or("no pistol", |d| inv.defs[d].name));
        fight.active = Some((id, lent));
    }
    if fight.active.is_some_and(|(id, _)| cancelled.contains(&id)) {
        let (id, lent) = fight.active.take().unwrap();
        info!("chopper fight ({id}): over");
        if let (Some(def), Some(from)) = (lent, inv.mounted_from) {
            if inv.slots.last().is_some_and(|s| s.def == def) {
                inv.slots.pop();
                inv.mounted_from = None;
                inv.current = from.min(inv.slots.len().saturating_sub(1));
                inv.reloading = 0.0;
                inv.switching = 0.35;
            }
        }
    }
    cinema.free_aim = fight.active.map(|_| REACH);
    // FC3_CHOPPER_SHOT=<png> (debug): a screenshot three seconds in.
    match (fight.active, *since) {
        (Some(_), None) => *since = Some(time.elapsed_secs()),
        (Some(_), Some(t)) if time.elapsed_secs() - t >= 3.0 && time.elapsed_secs() - time.delta_secs() - t < 3.0 => {
            if let Ok(path) = std::env::var("FC3_CHOPPER_SHOT") {
                crate::camera::capture(&mut commands, path);
            }
        }
        (None, _) => *since = None,
        _ => {}
    }
    // (`CheatAmmoInClip`: the magazine never runs dry.)
    if let Some((_, Some(def))) = fight.active {
        let full = inv.defs[def].magazine;
        if let Some(slot) = inv.slots.last_mut().filter(|s| s.def == def) {
            slot.mag = full;
        }
    }
}
