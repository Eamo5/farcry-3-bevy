//! Sniping challenges (`CSnipingChallenge`): a scripted gameplay that hands
//! the player a rifle and holds them crouched where they used it (WIL_050's
//! M93 above Oliver's guards), the view kept within the angles the library
//! and the script give about where they are to look, until the script
//! destroys it. The rifle is lent as a mounted gun is (the weapon held
//! before is put by), so it is neither switched away from nor saved.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::controls::{Action, ActionState};
use crate::player::{Player, ViewArc, ViewLimits, HALF_HEIGHT};
use crate::weapons::{Inventory, WeaponSlot};

pub struct SnipingPlugin;

impl Plugin for SnipingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Sniping>()
            .add_systems(PreUpdate, hold_controls.after(crate::controls::apply_action_maps))
            .add_systems(Update, run.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(Update, hold_place.after(crate::player::player_move));
    }
}

/// A sniping challenge the story asks for.
pub struct Start {
    pub id: u64,
    pub name: String,
    pub params: HashMap<String, String>,
}

#[derive(Resource, Default)]
pub struct Sniping {
    pub pending: Vec<Start>,
    pub cancelled: Vec<u64>,
    active: Option<Active>,
}

impl Sniping {
    /// A checkpoint reload: what runs ends (the inventory comes back with
    /// the save).
    pub fn reset(&mut self) {
        self.pending.clear();
        if let Some(a) = &self.active {
            self.cancelled.push(a.id);
        }
    }

    /// Whether a challenge holds the player at its rifle.
    pub fn holds_player(&self) -> bool {
        self.active.is_some()
    }
}

struct Active {
    id: u64,
    settings: fc3_formats::gsf::SnipingSettings,
    /// Where the player is held (x, z).
    at: Vec2,
    /// The rifle lent (its def), if one was.
    lent: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut sniping: ResMut<Sniping>,
    host: Option<NonSend<crate::story::StoryHost>>,
    info: Option<Res<crate::CacheInfo>>,
    mut music: ResMut<crate::music::Music>,
    mut limits: ResMut<ViewLimits>,
    mut players: Query<(&Transform, &mut Player, &crate::combat::Health, &mut Inventory)>,
) {
    let Ok((t, mut p, health, mut inv)) = players.single_mut() else { return };
    let mut cancelled = std::mem::take(&mut sniping.cancelled);
    if health.dead {
        cancelled.extend(sniping.active.as_ref().map(|a| a.id));
    }
    for start in std::mem::take(&mut sniping.pending) {
        if cancelled.contains(&start.id) {
            continue;
        }
        // (A block's challenge started over another's, WIL_050's B20 over
        // B10's: the one before ends.)
        if let Some(a) = sniping.active.take() {
            end(a, &mut inv, &mut music, &mut limits);
        }
        let Some(host) = host.as_ref() else { continue };
        let settings = (|| -> anyhow::Result<_> {
            use anyhow::Context;
            let root = &info.as_ref().context("cache unavailable")?.root;
            let mut s = fc3_formats::gsf::SnipingSettings::parse(&std::fs::read(root.join(fc3_formats::gsf::CACHE_REL))?, &start.name)?;
            for (k, v) in &start.params {
                s.set(k, v);
            }
            Ok(s)
        })();
        let settings = match settings {
            Ok(s) => s,
            Err(e) => {
                warn!("sniping: {} not played: {e:#}", start.name);
                continue;
            }
        };
        let data = host.story.data();
        let entity =
            |key: &str| start.params.get(key).and_then(|v| v.parse::<u64>().ok()).filter(|&id| id != 0 && id != u64::MAX).and_then(|id| data.entities.get(&id));
        let eye = t.translation + Vec3::Y * (p.eye - HALF_HEIGHT);
        // Towards what it is to look at; else where the rifle points (its
        // model's front, Dunia +Y).
        let (yaw, pitch) = if let Some(e) = entity("EntityToLookAt") {
            let aim = crate::data::to_bevy(e.pos) - eye;
            ((-aim.x).atan2(-aim.z), aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.1)))
        } else if let Some(e) = entity("InteractiveWeapon/EntityID") {
            let front = Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles)) * Vec3::NEG_Z;
            ((-front.x).atan2(-front.z), 0.0)
        } else {
            (p.yaw, 0.0)
        };
        // The rifle, ours by its model's folder, lent unless something else
        // is (a mounted gun).
        let def = settings.weapon_folder().and_then(|f| inv.defs.iter().position(|d| d.model.to_ascii_lowercase().split('\\').next() == Some(f.as_str())));
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
        if !settings.music.is_empty() {
            music.request(&settings.music, true);
        }
        info!(
            "sniping: {} ({}): {} lent, view yaw {:.0}..{:.0} pitch {:.0}..{:.0} about {:.1} {:.1} deg",
            start.name,
            start.id,
            lent.map_or("no rifle", |d| inv.defs[d].name),
            settings.yaw[0],
            settings.yaw[1],
            settings.pitch[0],
            settings.pitch[1],
            yaw.to_degrees(),
            pitch.to_degrees()
        );
        // The view's centre: towards what to look at, else the rifle's facing.
        p.yaw = yaw;
        p.pitch = pitch;
        p.velocity = Vec3::ZERO;
        limits.0 = Some(ViewArc { yaw, pitch, yaw_range: settings.yaw.map(f32::to_radians), pitch_range: settings.pitch.map(f32::to_radians) });
        sniping.active = Some(Active { id: start.id, settings, at: Vec2::new(t.translation.x, t.translation.z), lent });
    }
    if sniping.active.as_ref().is_some_and(|a| cancelled.contains(&a.id)) {
        let a = sniping.active.take().unwrap();
        info!("sniping: {} over", a.id);
        end(a, &mut inv, &mut music, &mut limits);
    }
}

/// The rifle given back (the weapon put by in hand again), the view let go
/// and the music stopped.
fn end(a: Active, inv: &mut Inventory, music: &mut crate::music::Music, limits: &mut ViewLimits) {
    limits.0 = None;
    if let (Some(def), Some(from)) = (a.lent, inv.mounted_from) {
        if inv.slots.last().is_some_and(|s| s.def == def) {
            inv.slots.pop();
            inv.mounted_from = None;
            inv.current = from.min(inv.slots.len().saturating_sub(1));
            inv.reloading = 0.0;
            inv.switching = 0.35;
        }
    }
    if !a.settings.music.is_empty() {
        music.request(&a.settings.music, false);
    }
}

/// At the rifle: no walking, jumping or other weapons, crouched if it asks.
fn hold_controls(sniping: Res<Sniping>, mut actions: ResMut<ActionState>) {
    let Some(a) = &sniping.active else { return };
    actions.movement = Vec2::ZERO;
    actions.cycle = 0;
    for x in
        [Action::Jump, Action::NextWeapon, Action::PrevWeapon, Action::Grenade, Action::Molotov, Action::Melee, Action::ThrowRock, Action::C4, Action::Mine]
    {
        actions.suppress(x);
    }
    if a.settings.crouch {
        actions.hold(Action::Crouch);
    }
}

/// The player kept where they took the rifle.
fn hold_place(sniping: Res<Sniping>, mut players: Query<(&mut Transform, &mut Player)>) {
    let Some(a) = &sniping.active else { return };
    let Ok((mut t, mut p)) = players.single_mut() else { return };
    t.translation.x = a.at.x;
    t.translation.z = a.at.y;
    p.velocity.x = 0.0;
    p.velocity.z = 0.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The view stops at the challenge's limits either side of its centre
    /// (left positive), across the turn's wrap.
    #[test]
    fn the_view_stops_at_the_challenges_angles() {
        let a = ViewArc { yaw: 3.0, pitch: 0.1, yaw_range: [-18f32.to_radians(), 92f32.to_radians()], pitch_range: [-15f32.to_radians(), 7f32.to_radians()] };
        let clamp_view = |a: &ViewArc, yaw, pitch| a.clamp(yaw, pitch);
        let (yaw, pitch) = clamp_view(&a, 3.0 + 100f32.to_radians(), 1.0);
        assert!((yaw - (3.0 + 92f32.to_radians())).abs() < 1e-4);
        assert!((pitch - (0.1 + 7f32.to_radians())).abs() < 1e-4);
        let (yaw, _) = clamp_view(&a, 3.0 - 30f32.to_radians(), 0.1);
        assert!((yaw - (3.0 - 18f32.to_radians())).abs() < 1e-4);
        // (Inside, a full turn round is the same view.)
        let (yaw, _) = clamp_view(&a, 3.0 + 10f32.to_radians() + std::f32::consts::TAU, 0.1);
        assert!((yaw - (3.0 + 10f32.to_radians())).abs() < 1e-4);
    }
}
