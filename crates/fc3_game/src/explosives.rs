//! C4 and proximity mines. C4 is thrown and sticks to whatever it hits
//! (vehicles included); holding the key sets off every charge out there.
//! Mines are laid on the ground and go off when an enemy, an animal or a
//! vehicle comes close.

use avian3d::prelude::*;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use fc3_formats::cache;

use crate::cache_assets::{cache_path, Fc3Model};
use crate::controls::{Action, ActionState};
use crate::player::{Player, PlayerCamera};
use crate::weapons::{Explosion, Inventory};

pub struct ExplosivesPlugin;

impl Plugin for ExplosivesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_models)
            .add_systems(PreUpdate, explosives_test.after(crate::controls::update_actions))
            .add_systems(Update, (use_explosives, fly_charges, trip_mines, show_models).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// How many of each can be carried.
pub const EXPLOSIVE_CAP: u32 = 3;
/// Holding the C4 key this long sets the charges off.
const DETONATE_HOLD: f32 = 0.4;
const C4: (f32, f32) = (7.0, 260.0);
const MINE: (f32, f32) = (5.0, 220.0);
/// A mine arms this long after it's laid, then trips within this distance.
const MINE_ARMING: f32 = 1.5;
const MINE_TRIP: f32 = 1.7;

#[derive(Resource)]
struct Models {
    c4: Handle<Fc3Model>,
    mine: Handle<Fc3Model>,
}

fn load_models(mut commands: Commands, server: Res<AssetServer>) {
    let load = |p: &str| server.load(cache_path(&cache::model_rel(p)));
    commands.insert_resource(Models {
        c4: load(r"graphics\__fc3_graphics\_common\weapons\ied\ied_c4_bomb_tp.xbg"),
        mine: load(r"graphics\__fc3_graphics\_common\weapons\proximitymine\proxmine_tp.xbg"),
    });
}

/// A thrown charge: flying, then stuck in place (a child of what it hit).
#[derive(Component)]
pub struct Charge {
    velocity: Vec3,
    stuck: bool,
    owner: Entity,
}

/// A laid mine and its age.
#[derive(Component)]
pub struct Mine {
    age: f32,
    owner: Entity,
}

/// Needs its model attached.
#[derive(Component)]
struct Dressed;

#[allow(clippy::too_many_arguments)]
fn use_explosives(
    time: Res<Time>,
    actions: Res<ActionState>,
    busy: Res<crate::fp::HandsBusy>,
    mut held: Local<f32>,
    // A charge and a mine on their way out of the hand (seconds left).
    mut leaving: Local<(Option<f32>, Option<f32>)>,
    mut fp: ResMut<crate::fp::FpState>,
    mut players: Query<(Entity, &mut Inventory, &Player, &Transform, &crate::combat::Health)>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    charges: Query<(Entity, &GlobalTransform), With<Charge>>,
    (spatial, heights): (SpatialQuery, Res<crate::terrain::Heightfield>),
    mut explosions: MessageWriter<Explosion>,
    mut commands: Commands,
) {
    let Ok((pe, mut inv, p, t, h)) = players.single_mut() else { return };
    let Ok(cam) = camera.single() else { return };
    if h.dead || p.noclip || p.swimming || p.driving || p.mounted.is_some() || busy.busy() {
        *held = 0.0;
        return;
    }
    let dt = time.delta_secs();
    let (charge_out, mine_out) = &mut *leaving;
    // C4: a tap throws one; holding sets them all off.
    if actions.pressed(Action::C4) {
        *held += dt;
        if *held >= DETONATE_HOLD && *held - dt < DETONATE_HOLD && !charges.is_empty() {
            for (e, gt) in &charges {
                explosions.write(Explosion { at: gt.translation(), radius: C4.0, damage: C4.1, instigator: Some(pe) });
                commands.entity(e).despawn();
            }
            fp.gesture = Some(if inv.c4 > 0 { r"special\ied|detonate" } else { r"special\ied|detonate_empty" });
        }
    } else {
        if *held > 0.0 && *held < DETONATE_HOLD && inv.c4 > 0 && charge_out.is_none() {
            inv.c4 -= 1;
            fp.gesture = Some(r"special\ied|throw");
            *charge_out = Some(crate::fp::release_time("throw", "ied_c4_bomb").unwrap_or(0.0));
        }
        *held = 0.0;
    }
    // A mine at the feet, a step ahead.
    if actions.just_pressed(Action::Mine) && inv.mines > 0 && p.grounded && mine_out.is_none() {
        inv.mines -= 1;
        fp.gesture = Some(r"special\proximitymine|place");
        *mine_out = Some(crate::fp::release_time("place", "proxmine").unwrap_or(0.0));
    }
    // Out of the hand: the charge flies along the view, the mine goes down
    // on whatever is underfoot.
    if let Some(left) = charge_out.as_mut() {
        *left -= dt;
        if *left <= 0.0 {
            *charge_out = None;
            let f = cam.forward().as_vec3();
            commands.spawn((
                Charge { velocity: (f + Vec3::Y * 0.2).normalize() * 14.0 + p.velocity * 0.5, stuck: false, owner: pe },
                Transform::from_translation(cam.translation() + f * 0.5),
                Visibility::Visible,
            ));
        }
    }
    if let Some(left) = mine_out.as_mut() {
        *left -= dt;
        if *left <= 0.0 {
            *mine_out = None;
            let fwd = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
            let above = t.translation + fwd * 1.2 + Vec3::Y * 0.5;
            let filter = SpatialQueryFilter::from_excluded_entities([pe]);
            let ground = spatial.cast_ray(above, Dir3::NEG_Y, 3.0, true, &filter).map(|hit| (above.y - hit.distance, hit.normal));
            let (y, normal) = ground.unwrap_or((heights.height_at(above).unwrap_or(t.translation.y - crate::player::HALF_HEIGHT), Vec3::Y));
            let rotation = Quat::from_rotation_arc(Vec3::Y, normal) * Quat::from_rotation_y(p.yaw);
            commands.spawn((
                Mine { age: 0.0, owner: pe },
                Transform::from_translation(Vec3::new(above.x, y, above.z)).with_rotation(rotation),
                Visibility::Visible,
            ));
        }
    }
}

/// Charges fly until they hit something, then stay stuck to it.
fn fly_charges(time: Res<Time>, spatial: SpatialQuery, mut charges: Query<(Entity, &mut Charge, &mut Transform)>, mut commands: Commands) {
    let dt = time.delta_secs();
    for (e, mut c, mut t) in &mut charges {
        if c.stuck {
            continue;
        }
        c.velocity.y -= 9.81 * dt;
        let step = c.velocity * dt;
        let Ok(dir) = Dir3::new(step) else { continue };
        let filter = SpatialQueryFilter::from_excluded_entities([c.owner, e]);
        match spatial.cast_ray(t.translation, dir, step.length(), true, &filter) {
            Some(hit) => {
                c.stuck = true;
                let at = t.translation + step.normalize() * hit.distance + hit.normal * 0.03;
                let rotation = Quat::from_rotation_arc(Vec3::Y, hit.normal);
                // Stuck to what it hit, so it rides along with vehicles.
                commands.queue(move |world: &mut World| {
                    let Some(parent) = world.get::<GlobalTransform>(hit.entity).copied() else { return };
                    let local = parent.affine().inverse().transform_point3(at);
                    let parent_rot = parent.to_scale_rotation_translation().1;
                    if let Ok(mut ent) = world.get_entity_mut(e) {
                        ent.insert((Transform::from_translation(local).with_rotation(parent_rot.inverse() * rotation), ChildOf(hit.entity)));
                    }
                });
            }
            None => t.translation += step,
        }
    }
}

/// Armed mines go off when anything but the player comes close.
fn trip_mines(
    time: Res<Time>,
    mut mines: Query<(Entity, &mut Mine, &GlobalTransform)>,
    walkers: Query<(&GlobalTransform, &crate::combat::Health), Without<Player>>,
    vehicles: Query<&GlobalTransform, With<crate::vehicles::Vehicle>>,
    mut explosions: MessageWriter<Explosion>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (e, mut m, gt) in &mut mines {
        m.age += dt;
        if m.age < MINE_ARMING {
            continue;
        }
        let at = gt.translation();
        let near = |p: Vec3| Vec2::new(p.x - at.x, p.z - at.z).length() < MINE_TRIP && (p.y - at.y).abs() < 2.5;
        let tripped = walkers.iter().any(|(w, h)| !h.dead && near(w.translation())) || vehicles.iter().any(|v| near(v.translation()));
        if tripped {
            explosions.write(Explosion { at: at + Vec3::Y * 0.2, radius: MINE.0, damage: MINE.1, instigator: Some(m.owner) });
            commands.entity(e).despawn();
        }
    }
}

/// Gives new charges and mines their models.
fn show_models(
    models: Option<Res<Models>>,
    assets: Res<Assets<Fc3Model>>,
    new: Query<(Entity, Has<Charge>), (Or<(With<Charge>, With<Mine>)>, Without<Dressed>)>,
    mut commands: Commands,
) {
    let Some(models) = models else { return };
    for (e, is_charge) in &new {
        let Some(m) = assets.get(if is_charge { &models.c4 } else { &models.mine }) else { continue };
        for part in m.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]) {
            commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), NotShadowCaster, ChildOf(e)));
        }
        commands.entity(e).insert(Dressed);
    }
}

/// `FC3_EXPLOSIVES_TEST` (with `FC3_EXPLOSIVES`, `--npc-test guards`): lays a
/// mine, throws a C4 up the slope at the guards and sets it off, then walks
/// the nearest survivor onto the mine, logging their health; `FC3_TD_SHOT`
/// takes screenshots that long into it.
#[allow(clippy::too_many_arguments)]
pub fn explosives_test(
    time: Res<Time>,
    args: Res<crate::Args>,
    mut state: Local<(Option<f32>, u32, usize, Option<Entity>)>,
    mut actions: ResMut<ActionState>,
    mut pads: Query<&mut Gamepad>,
    mut players: Query<&mut Player>,
    mut npcs: Query<(&mut Transform, &crate::combat::Health), With<crate::npc::Npc>>,
    mines: Query<&GlobalTransform, With<Mine>>,
    charges: Query<&GlobalTransform, With<Charge>>,
    mut commands: Commands,
) {
    let Ok(mode) = std::env::var("FC3_EXPLOSIVES_TEST") else { return };
    let Ok(mut p) = players.single_mut() else { return };
    if p.spawning {
        return;
    }
    let (start, step, shots, pad) = &mut *state;
    let t = time.elapsed_secs() - *start.get_or_insert(time.elapsed_secs());
    // "pad": the same through a virtual gamepad's D-pad (left held: mine,
    // right tapped: C4, right held: set off, left tapped: rock).
    let by_pad = mode == "pad";
    if by_pad {
        for a in [Action::Mine, Action::ThrowRock, Action::C4] {
            if actions.just_pressed(a) {
                info!("explosives-test: the pad gave {a:?} at {t:.2} s");
            }
        }
        let pad = *pad.get_or_insert_with(|| commands.spawn(Gamepad::default()).id());
        if let Ok(mut g) = pads.get_mut(pad) {
            let held = |a: f32, b: f32| t >= a && t < b;
            for (b, on) in [(GamepadButton::DPadLeft, held(3.0, 3.5) || held(8.0, 8.06)), (GamepadButton::DPadRight, held(4.0, 4.06) || held(6.0, 6.6))] {
                if on {
                    g.digital_mut().press(b);
                } else {
                    g.digital_mut().release(b);
                }
            }
        }
    }
    let mut press = |a: Action| {
        if !by_pad {
            actions.press(a);
        }
    };
    let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    if let (Some(&when), Some(path)) = (at.get(*shots), args.screenshot.as_ref()) {
        if t >= when {
            let stem = path.strip_suffix(".png").unwrap_or(path);
            crate::camera::capture(&mut commands, format!("{stem}_td{shots}.png"));
            *shots += 1;
        }
    }
    let health = |npcs: &Query<(&mut Transform, &crate::combat::Health), With<crate::npc::Npc>>| {
        npcs.iter().map(|(_, h)| format!("{:.0}", h.current)).collect::<Vec<_>>()
    };
    match *step {
        0 if t > 3.0 => {
            press(Action::Mine);
            *step = 1;
        }
        1 if t > 4.0 => {
            // Up the slope at them.
            p.pitch = 0.42;
            press(Action::C4);
            *step = 2;
        }
        // Released: the throw.
        2 if t > 4.05 => *step = 3,
        3 if t > 6.0 => {
            for c in &charges {
                let near = npcs.iter().map(|(nt, _)| nt.translation.distance(c.translation())).fold(f32::MAX, f32::min);
                info!("explosives-test: charge stuck at {:.1?}, {near:.1} m from the nearest enemy", c.translation());
            }
            info!("explosives-test: before; enemies {:?} at {:?}", health(&npcs), npcs.iter().map(|(t, _)| t.translation.round()).collect::<Vec<_>>());
            *step = 4;
        }
        // Held: set it off.
        4 if t < 6.6 => press(Action::C4),
        4 => {
            info!("explosives-test: C4 set off; enemies {:?}", health(&npcs));
            *step = 5;
        }
        5 if t > 7.5 => {
            // The nearest survivor wanders onto the mine.
            if let Some(m) = mines.iter().next().map(|m| m.translation()) {
                let near = npcs.iter_mut().filter(|(_, h)| !h.dead).min_by(|a, b| a.0.translation.distance(m).total_cmp(&b.0.translation.distance(m)));
                if let Some((mut nt, _)) = near {
                    nt.translation = m + Vec3::Y * crate::player::HALF_HEIGHT;
                }
            }
            *step = 6;
        }
        6 if t > 9.0 => {
            info!("explosives-test: after the mine ({} left); enemies {:?}", mines.iter().count(), health(&npcs));
            *step = 7;
        }
        _ => {}
    }
}
