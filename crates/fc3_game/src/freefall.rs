//! The parachute and the wingsuit, both bought at shops. Jump while falling
//! from a height to open the wingsuit (or the parachute, if that's all you
//! have); jump again in the wingsuit to open the parachute. The flight itself
//! is in `player::player_move`; this module shows the canopy, prompts and
//! logs the test.

use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use fc3_formats::cache;

use crate::cache_assets::{cache_path, Fc3Model};
use crate::player::{AirMode, Player, HALF_HEIGHT};

pub struct FreefallPlugin;

impl Plugin for FreefallPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_canopy).add_systems(
            Update,
            (canopy, freefall_prompt.before(crate::hud::HudSet), freefall_test.before(crate::player::player_move))
                .run_if(in_state(crate::menu::GameState::Playing)),
        );
    }
}

const CANOPY: &str = r"graphics\__fc3_graphics\_common\props\misc\parachute.xbg";

/// Height above the ground needed to open either.
pub const MIN_CLEARANCE: f32 = 8.0;

/// Shop items (upgrade ids and prices).
pub const PARACHUTE: &str = "parachute";
pub const WINGSUIT: &str = "wingsuit";

#[derive(Resource)]
struct CanopyModel(Handle<Fc3Model>);

#[derive(Component)]
struct Canopy;

fn load_canopy(mut commands: Commands, server: Res<AssetServer>) {
    commands.insert_resource(CanopyModel(server.load(cache_path(&cache::model_rel(CANOPY)))));
}

/// The open canopy over the player while the parachute is out.
fn canopy(
    mut commands: Commands,
    model: Option<Res<CanopyModel>>,
    models: Res<Assets<Fc3Model>>,
    players: Query<(Entity, &Player)>,
    canopies: Query<Entity, With<Canopy>>,
) {
    let Ok((pe, p)) = players.single() else { return };
    let open = p.air == AirMode::Chute;
    match (open, canopies.iter().next()) {
        (true, None) => {
            let Some(m) = model.as_ref().and_then(|m| models.get(&m.0)) else { return };
            // The model hangs from its toggles, which the arms' clip holds just
            // above the head, a hand either side.
            let hands = -HALF_HEIGHT + crate::player::EYE_HEIGHT + 0.34;
            let root = commands.spawn((Canopy, Transform::from_xyz(0.0, hands, 0.0), Visibility::Inherited, ChildOf(pe))).id();
            for part in m.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]) {
                commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), NotShadowCaster, ChildOf(root)));
            }
        }
        (false, Some(e)) => commands.entity(e).despawn(),
        _ => {}
    }
}

/// "Jump: open the wingsuit / parachute" while falling high enough.
fn freefall_prompt(
    actions: Res<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    progress: Res<crate::progression::Progress>,
    heights: Res<crate::terrain::Heightfield>,
    players: Query<(&Player, &Transform)>,
    text: Option<Res<crate::hud::GameText>>,
    mut hud: ResMut<crate::hud::HudState>,
) {
    let Ok((p, t)) = players.single() else { return };
    if p.grounded || p.swimming || p.driving || p.noclip || hud.prompt.is_some() || p.air == AirMode::Chute {
        return;
    }
    let clearance = t.translation.y - HALF_HEIGHT - heights.height_at(t.translation).unwrap_or(f32::MIN);
    if clearance < MIN_CLEARANCE || (p.air == AirMode::None && p.velocity.y > -4.0) {
        return;
    }
    // (In the game's words where it has them.)
    let (what, game) = match p.air {
        AirMode::None if progress.upgrade(WINGSUIT) > 0 => ("open the wingsuit", crate::hud::prompt::DEPLOY_WINGSUIT),
        AirMode::None | AirMode::Wingsuit if progress.upgrade(PARACHUTE) > 0 => ("open the parachute", crate::hud::prompt::DEPLOY_PARACHUTE),
        _ => return,
    };
    hud.prompt = Some(
        text.as_deref()
            .and_then(|t| t.prompt(game, &actions, &settings))
            .unwrap_or_else(|| format!("{}  {what}", actions.label(crate::controls::Action::Jump, &settings))),
    );
}

/// `FC3_FREEFALL=chute|wingsuit` (tests): drops the player from 120 m with the
/// gear and opens it, logging the flight each second. The wingsuit dives
/// for a second and a half, banks left for a second, then hands over to the
/// parachute (`FC3_FREEFALL_HOLD=secs` after the drop: 4.5).
fn freefall_test(
    time: Res<Time>,
    args: Res<crate::Args>,
    mut actions: ResMut<crate::controls::ActionState>,
    mut state: Local<(bool, f32, f32, usize)>,
    mut progress: ResMut<crate::progression::Progress>,
    mut players: Query<(&mut Player, &mut Transform)>,
    mut commands: Commands,
) {
    let Ok(mode) = std::env::var("FC3_FREEFALL") else { return };
    let Ok((mut p, mut t)) = players.single_mut() else { return };
    let (dropped, clock, last, shots) = &mut *state;
    if p.spawning {
        return;
    }
    if !*dropped {
        *dropped = true;
        progress.upgrades.insert(PARACHUTE.into(), 1);
        if mode == "wingsuit" {
            progress.upgrades.insert(WINGSUIT.into(), 1);
        }
        t.translation.y += 120.0;
        p.velocity = Vec3::new(0.0, -6.0, 0.0);
        info!("freefall-test: dropped from {:.0} m", t.translation.y);
        return;
    }
    *clock += time.delta_secs();
    if p.air == AirMode::None && !p.grounded && *clock > 1.0 && *clock < 2.0 {
        p.air = if mode == "wingsuit" { AirMode::Wingsuit } else { AirMode::Chute };
    }
    // The wingsuit dives, banks, and hands over to the parachute.
    let hold = std::env::var("FC3_FREEFALL_HOLD").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(4.5);
    if mode == "wingsuit" && p.air == AirMode::Wingsuit {
        actions.movement = match *clock {
            c if (2.0..3.5).contains(&c) => Vec2::new(0.0, 1.0),
            c if (3.5..4.5).contains(&c) => Vec2::new(-1.0, 0.0),
            _ => Vec2::ZERO,
        };
        if *clock > hold {
            p.air = AirMode::Chute;
        }
    }
    // FC3_TD_SHOT=2,6: screenshots that long after the drop.
    let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    if let (Some(&when), Some(path)) = (at.get(*shots), args.screenshot.as_ref()) {
        if *clock >= when {
            let stem = path.strip_suffix(".png").unwrap_or(path);
            crate::camera::capture(&mut commands, format!("{stem}_td{shots}.png"));
            *shots += 1;
        }
    }
    if *clock - *last >= 1.0 {
        *last = *clock;
        let h = Vec2::new(p.velocity.x, p.velocity.z).length();
        info!(
            "freefall-test t={:.0} {:?} y={:.1} horizontal {h:.1} m/s vertical {:.1} m/s heading {:.0} grounded {}",
            *clock,
            p.air,
            t.translation.y,
            p.velocity.y,
            p.velocity.x.atan2(-p.velocity.z).to_degrees() * -1.0,
            p.grounded
        );
    }
}
