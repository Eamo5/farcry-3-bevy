//! Reinforcements that come by vehicle: the point's vehicle (`arkVehicle`)
//! is made on its way in (`entVehiclePathId`), its mercs board it (the first
//! at the wheel), it drives the path to its end, and there they get out and
//! search the battlefield. If the vehicle can't be made or driven (no model
//! for it, no ground under it, its driver killed), whoever is left comes on
//! foot from where they are.

use std::collections::HashSet;

use avian3d::prelude::*;
use bevy::prelude::*;

use crate::cache_assets::Fc3Model;
use crate::player::{Player, HALF_HEIGHT};
use crate::story_actors::StoryActor;
use crate::vehicles::Vehicle;

pub struct StoryRidesPlugin;

impl Plugin for StoryRidesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Rides>()
            .add_systems(Update, ride_reinforcements.after(crate::story_actors::ActorSet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// Giving up on a vehicle that can't be put down (s), on boarding, and on
/// the drive (a car caught for good).
const PLACE_WAIT: f32 = 30.0;
const BOARD_WAIT: f32 = 12.0;
const DRIVE_WAIT: f32 = 240.0;
/// After this long boarding, the vehicle leaves with whoever is aboard.
const BOARD_GRACE: f32 = 4.0;
/// Held up this long (s) under walking pace on the way (a boat run up the
/// beach its path carries on over, a car caught for good), the crew get
/// out where they are.
const STUCK_AFTER: f32 = 5.0;
const STUCK_SPEED: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Placing,
    Boarding,
    Driving,
    Landing,
}

/// A vehicle bringing a reinforcement point's mercs.
pub struct Ride {
    /// The vehicle's id (the story's) and archetype.
    pub vehicle: u64,
    pub archetype: u64,
    /// Its definition (`vehicles`).
    pub def: usize,
    /// The path entity and its points from where it starts.
    pub path: u64,
    pub points: Vec<Vec3>,
    pub speed: f32,
    /// The mercs, the driver first.
    pub crew: Vec<u64>,
    /// Where they head once out.
    pub field: Option<Vec3>,
    stage: Stage,
    since: f32,
    /// How long it has been held up on the way (s).
    still: f32,
    asked: HashSet<u64>,
    released: HashSet<u64>,
}

impl Ride {
    #[allow(clippy::too_many_arguments)]
    pub fn new(vehicle: u64, archetype: u64, def: usize, path: u64, points: Vec<Vec3>, speed: f32, crew: Vec<u64>, field: Option<Vec3>) -> Self {
        Self {
            vehicle,
            archetype,
            def,
            path,
            points,
            speed,
            crew,
            field,
            stage: Stage::Placing,
            since: 0.0,
            still: 0.0,
            asked: HashSet::new(),
            released: HashSet::new(),
        }
    }

    fn enter(&mut self, stage: Stage) {
        debug!("story ride {}: {:?} -> {stage:?} after {:.1} s", self.vehicle, self.stage, self.since);
        self.stage = stage;
        self.since = 0.0;
    }

    /// Facing along the path's first leg.
    fn start(&self) -> Transform {
        let at = self.points[0];
        let ahead = (self.points[1] - at).with_y(0.0);
        let yaw = if ahead.length() > 0.1 { (-ahead.x).atan2(-ahead.z) } else { 0.0 };
        Transform::from_translation(at).with_rotation(Quat::from_rotation_y(yaw))
    }
}

/// Reinforcements on their way by vehicle.
#[derive(Resource, Default)]
pub struct Rides(pub Vec<Ride>);

/// The model an entity is drawn with (its graphic's first `.xbg`).
fn model_of(entry: &fc3_story::data::Entry) -> Option<String> {
    let mut model = None;
    entry.component("CGraphicComponent")?.walk(&mut |o, _| {
        if model.is_none() {
            model = o.str_h(0x3182_766C).filter(|m| m.to_ascii_lowercase().ends_with(".xbg"));
        }
    });
    model
}

/// The vehicle definition an archetype is drawn with.
pub fn vehicle_def(archetype: &fc3_story::data::Entry) -> Option<usize> {
    model_of(archetype).and_then(|m| crate::vehicles::definition_for_model(&m))
}

/// A vehicle archetype's seats. The library's prototypes leave them out
/// (the editor fills them in where a vehicle is placed), so they are taken
/// from the placed vehicle drawn with the same model that has the most.
fn layout_of(data: &fc3_story::data::StoryData, archetype: &fc3_story::data::Entry) -> fc3_story::vehicles::Layout {
    let own = fc3_story::vehicles::Layout::from_entry(archetype).unwrap_or_default();
    if !own.seats.is_empty() {
        return own;
    }
    let Some(model) = model_of(archetype) else { return own };
    data.entities
        .values()
        .filter(|e| e.component("CFCXVehicle").is_some() && model_of(e).is_some_and(|m| m.eq_ignore_ascii_case(&model)))
        .filter_map(fc3_story::vehicles::Layout::from_entry)
        .max_by_key(|l| l.seats.len())
        .filter(|l| !l.seats.is_empty())
        .unwrap_or(own)
}

#[allow(clippy::too_many_arguments)]
fn ride_reinforcements(
    time: Res<Time>,
    mut rides: ResMut<Rides>,
    host: Option<NonSend<crate::story::StoryHost>>,
    (assets, server, models, spatial): (Option<Res<crate::vehicles::VehicleAssets>>, Res<AssetServer>, Res<Assets<Fc3Model>>, SpatialQuery),
    vehicles: Query<(&Vehicle, &LinearVelocity)>,
    mut actors: Query<(Entity, &StoryActor, &Transform, Option<&mut crate::npc::Npc>, Has<crate::npc::Scripted>)>,
    players: Query<&Transform, With<Player>>,
    mut arrivals: ResMut<crate::story_actors::Arrivals>,
    mut seats: ResMut<crate::vehicles::StorySeatRequests>,
    mut paths: ResMut<crate::vehicle_paths::StoryVehiclePaths>,
    mut commands: Commands,
) {
    let Some(host) = host else { return };
    if rides.0.is_empty() {
        return;
    }
    let dt = time.delta_secs();
    let data = host.story.data();
    let player = players.single().map(|t| t.translation).ok();
    rides.0.retain_mut(|ride| {
        ride.since += dt;
        // The dead drop out.
        ride.crew.retain(|id| !host.story.is_dead(*id));
        if ride.crew.is_empty() {
            debug!("story ride {}: no one left", ride.vehicle);
            return false;
        }
        let (vehicle, speed) = vehicles.iter().find(|v| v.0.story_id() == ride.vehicle).map_or((None, 0.0), |(v, l)| (Some(v), l.0.length()));
        let seated = |id: u64| vehicle.is_some_and(|v| v.carries(id));
        match ride.stage {
            Stage::Placing => {
                // Made once its driver is in the world (the crew wait at the start).
                if !actors.iter().any(|a| a.1.id == ride.crew[0]) {
                    if ride.since > PLACE_WAIT * 2.0 {
                        ride.enter(Stage::Landing);
                    }
                    return true;
                }
                let model = assets.as_ref().map(|a| &a.models[ride.def]).filter(|h| crate::cache_assets::usable(&server, h.id())).and_then(|h| models.get(h));
                let start = ride.start();
                // Land vehicles wait for the ground under them to have its collider.
                let ground = crate::vehicles::is_boat(ride.def)
                    || spatial.cast_ray(start.translation + Vec3::Y * 1.5, Dir3::NEG_Y, 6.0, true, &SpatialQueryFilter::default()).is_some();
                match model.filter(|m| !m.lods.is_empty() && ground) {
                    Some(model) => {
                        let layout = data.archetypes.get(&ride.archetype).map(|a| layout_of(&data, a)).unwrap_or_default();
                        crate::vehicles::build_story_vehicle(
                            &mut commands,
                            ride.def,
                            model,
                            start.translation + Vec3::Y * 0.4,
                            start.rotation,
                            ride.vehicle,
                            &layout,
                        );
                        info!("story ride {}: vehicle made at {:.1}, {} aboard to come", ride.vehicle, start.translation, ride.crew.len());
                        ride.enter(Stage::Boarding);
                    }
                    None if ride.since > PLACE_WAIT => {
                        warn!("story ride {}: its vehicle couldn't be put down; the crew come on foot", ride.vehicle);
                        ride.enter(Stage::Landing);
                    }
                    None => {}
                }
            }
            Stage::Boarding => {
                let Some(v) = vehicle else {
                    if ride.since > BOARD_WAIT {
                        ride.enter(Stage::Landing);
                    }
                    return true;
                };
                for (i, id) in ride.crew.iter().enumerate() {
                    let Some((entity, _, _, _, scripted)) = actors.iter().find(|a| a.1.id == *id) else { continue };
                    if ride.asked.insert(*id) {
                        seats.board(ride.vehicle, *id, i == 0);
                        // (Riding, not fighting: the seat holds them.)
                        if !scripted {
                            commands.entity(entity).insert(crate::npc::Scripted);
                        }
                    }
                }
                let aboard = ride.crew.iter().filter(|id| seated(**id)).count();
                // (Once all are in the world, a moment for them to get in.)
                let all_here = ride.crew.iter().all(|id| ride.asked.contains(id));
                let leave = aboard == ride.crew.len() || (all_here && ride.since > BOARD_GRACE) || ride.since > BOARD_WAIT;
                if v.driver() == Some(ride.crew[0]) && leave {
                    paths.follow(ride.vehicle, ride.path, ride.points.clone(), ride.speed);
                    info!("story ride {}: off along {} ({} aboard)", ride.vehicle, ride.path, aboard);
                    ride.enter(Stage::Driving);
                } else if ride.since > BOARD_WAIT {
                    ride.enter(Stage::Landing);
                }
            }
            Stage::Driving => {
                let arrived = paths.arrived(ride.vehicle);
                ride.still = if speed < STUCK_SPEED { ride.still + dt } else { 0.0 };
                let stuck = ride.still > STUCK_AFTER && ride.since > STUCK_AFTER;
                let stranded = vehicle.is_none_or(|v| v.driver().is_none()) || ride.since > DRIVE_WAIT;
                if arrived || stuck || stranded {
                    let why = if arrived {
                        "arrived"
                    } else if stuck {
                        "held up"
                    } else {
                        "stranded"
                    };
                    info!("story ride {}: {why}; the crew get out", ride.vehicle);
                    paths.stop(ride.vehicle);
                    ride.enter(Stage::Landing);
                }
            }
            Stage::Landing => {}
        }
        if ride.stage != Stage::Landing {
            return true;
        }
        // Out of their seats, they come looking (the battlefield, or
        // wherever the player is).
        let field = ride.field.or(player);
        for id in ride.crew.clone() {
            if ride.released.contains(&id) {
                continue;
            }
            if seated(id) {
                seats.leave(ride.vehicle, id);
                continue;
            }
            let Some((entity, a, t, npc, scripted)) = actors.iter_mut().find(|a| a.1.id == id) else { continue };
            if a.seated {
                continue;
            }
            if scripted {
                commands.entity(entity).remove::<crate::npc::Scripted>();
            }
            if let Some(mut npc) = npc {
                npc.state = crate::npc::NpcState::Search;
                npc.last_known = field;
                npc.post = t.translation - Vec3::Y * HALF_HEIGHT;
            }
            ride.released.insert(id);
        }
        let done = ride.crew.iter().all(|id| ride.released.contains(id));
        if !done && ride.since > BOARD_WAIT {
            // Those not in the world yet come looking when they are.
            for (e, to) in arrivals.0.iter_mut().filter(|(e, _)| ride.crew.contains(&e.id)) {
                debug!("story ride {}: {} comes on foot", ride.vehicle, e.id);
                *to = field;
            }
            return false;
        }
        !done
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ride's vehicle starts at its path's first point, facing its first
    /// leg.
    #[test]
    fn a_ride_starts_facing_its_first_leg() {
        let ride = Ride::new(1, 2, 0, 3, vec![Vec3::new(10.0, 2.0, 0.0), Vec3::new(20.0, 2.0, 0.0)], 10.0, vec![4], None);
        let start = ride.start();
        assert_eq!(start.translation, Vec3::new(10.0, 2.0, 0.0));
        let ahead = start.rotation * Vec3::NEG_Z;
        assert!((ahead - Vec3::X).length() < 1e-5, "{ahead}");
    }
}
