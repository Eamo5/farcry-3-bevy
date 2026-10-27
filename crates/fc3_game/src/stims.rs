//! Stims emitters (`CStimsEmitterComponent`): a place a script switches on
//! to hurt what stands near it, as the game's stims do (CIT_020's fuel
//! truck, burning its crew once the convoy is ambushed). Each of its stims
//! has a kind (a burn, ...), a level (the harm it does) and a radius; it
//! emits `fRateOfEmission` times a second, or once (`bSingleEmit`). The
//! script's `StimsEmitter` box switches it (`CStimsEmitterComponent_Enable`,
//! `_Disable`).

use std::collections::HashMap;

use bevy::prelude::*;

use crate::combat::{DamageEvent, DamageKind, Health};
use crate::data::to_bevy;

pub struct StimsPlugin;

impl Plugin for StimsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Stims>().add_systems(Update, run_stims.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The emitters switched on, and those the scripts just switched.
#[derive(Resource, Default)]
pub struct Stims {
    /// (emitter, on), from the story's commands.
    pub switched: Vec<(u64, bool)>,
    active: HashMap<u64, Emitter>,
}

impl Stims {
    pub fn reset(&mut self) {
        self.switched.clear();
        self.active.clear();
    }

    /// An emitter on that would hurt whoever stands at `p` (within its
    /// stims' reach and `margin` more): its place and its reach.
    pub fn harm_near(&self, p: Vec3, margin: f32) -> Option<(Vec3, f32)> {
        self.active.values().find_map(|em| {
            let reach = em.stims.iter().map(|s| s.radius).fold(0.0, f32::max);
            (em.at.distance(p) < reach + margin).then_some((em.at, reach))
        })
    }

    /// `p` (Dunia), or, within an emitter's harm, just out of it, the same
    /// way from it.
    pub fn out_of_harm(&self, p: [f32; 3]) -> [f32; 3] {
        let at = to_bevy(p);
        match self.harm_near(at, 1.0) {
            Some((em, reach)) => {
                let away = (at - em).with_y(0.0).normalize_or(Vec3::X);
                crate::data::to_dunia(em.with_y(at.y) + away * (reach + 2.0))
            }
            None => p,
        }
    }
}

struct Emitter {
    at: Vec3,
    stims: Vec<Stim>,
    /// Seconds between emissions, until the next; once only.
    every: f32,
    next: f32,
    single: bool,
}

#[derive(Debug, PartialEq)]
struct Stim {
    fire: bool,
    level: f32,
    radius: f32,
}

/// An emitter's stims, from its component (those with a radius and a level).
fn stims_of(component: &fc3_formats::fcb::Object) -> Vec<Stim> {
    let mut out = Vec::new();
    for list in component.children_named("Stims") {
        for s in list.children_named("Stim") {
            let (level, radius) = (s.f32("fLevel").unwrap_or(0.0), s.f32("fRadius").unwrap_or(0.0));
            if level > 0.0 && radius > 0.0 {
                // (Its kind's name: "burn".)
                let name = s.str_h(0xFC25_E1F1).unwrap_or_default().to_ascii_lowercase();
                out.push(Stim { fire: name.contains("burn") || name.contains("fire"), level, radius });
            }
        }
    }
    out
}

fn run_stims(
    host: Option<NonSend<crate::story::StoryHost>>,
    time: Res<Time>,
    mut stims: ResMut<Stims>,
    targets: Query<(Entity, &GlobalTransform, &Health)>,
    mut damage: MessageWriter<DamageEvent>,
) {
    let stims = &mut *stims;
    if let Some(host) = &host {
        let data = host.story.data();
        for (id, on) in std::mem::take(&mut stims.switched) {
            if !on {
                stims.active.remove(&id);
                continue;
            }
            let Some(e) = data.entities.get(&id) else { continue };
            let Some(c) = e.component("CStimsEmitterComponent") else { continue };
            let list = stims_of(c);
            info!("stims: {} on ({list:?})", data.name_of(id).unwrap_or("?"));
            let rate = c.f32("fRateOfEmission").filter(|r| *r > 0.0).unwrap_or(1.0);
            stims.active.insert(id, Emitter { at: to_bevy(e.pos), stims: list, every: 1.0 / rate, next: 0.0, single: c.bool("bSingleEmit").unwrap_or(false) });
        }
    }
    let dt = time.delta_secs();
    let mut spent = Vec::new();
    for (id, em) in &mut stims.active {
        em.next -= dt;
        if em.next > 0.0 {
            continue;
        }
        em.next = em.every;
        for s in &em.stims {
            for (e, gt, h) in &targets {
                if h.dead || gt.translation().distance(em.at) > s.radius {
                    continue;
                }
                damage.write(DamageEvent {
                    target: e,
                    amount: s.level,
                    kind: if s.fire { DamageKind::Fire } else { DamageKind::Explosion },
                    source: Some(em.at),
                    instigator: None,
                    headshot: false,
                    hit: crate::combat::Hit::Whole,
                });
            }
        }
        if em.single {
            spent.push(*id);
        }
    }
    for id in spent {
        stims.active.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_emitter_lists_its_stims_with_a_reach() {
        let field = |name: &str, v: Vec<u8>| (fc3_formats::hash::name_hash(name), v);
        let stim = |kind: &str, level: f32, radius: f32| {
            std::sync::Arc::new(fc3_formats::fcb::Object {
                name_hash: fc3_formats::hash::name_hash("Stim"),
                fields: vec![
                    (0xFC25_E1F1, [kind.as_bytes(), &[0]].concat()),
                    field("fLevel", level.to_le_bytes().to_vec()),
                    field("fRadius", radius.to_le_bytes().to_vec()),
                ],
                children: vec![],
            })
        };
        let list = std::sync::Arc::new(fc3_formats::fcb::Object {
            name_hash: fc3_formats::hash::name_hash("Stims"),
            fields: vec![],
            children: vec![stim("burn", 500.0, 3.0), stim("", 0.0, 0.0)],
        });
        let component = fc3_formats::fcb::Object { name_hash: fc3_formats::hash::name_hash("CStimsEmitterComponent"), fields: vec![], children: vec![list] };
        assert_eq!(stims_of(&component), [Stim { fire: true, level: 500.0, radius: 3.0 }]);
    }

    /// A place in an emitter's harm is moved just out of it, the same way
    /// from it; one clear of it stays.
    #[test]
    fn a_place_in_harms_way_is_moved_out_of_it() {
        let mut stims = Stims::default();
        let at = [100.0, 200.0, 10.0];
        stims.active.insert(1, Emitter { at: to_bevy(at), stims: vec![Stim { fire: true, level: 500.0, radius: 3.0 }], every: 1.0, next: 0.0, single: false });
        let moved = stims.out_of_harm([101.0, 200.0, 10.5]);
        assert!(((moved[0] - 105.0).abs() < 1e-3) && (moved[1] - 200.0).abs() < 1e-3 && (moved[2] - 10.5).abs() < 1e-3, "{moved:?}");
        assert!(stims.harm_near(to_bevy(moved), 1.0).is_none());
        assert_eq!(stims.out_of_harm([110.0, 200.0, 10.0]), [110.0, 200.0, 10.0]);
    }
}
