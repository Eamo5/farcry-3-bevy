//! Scripted screenshots for automated visual checks (`--screenshot <png>`).

use bevy::prelude::*;
use bevy::render::view::window::screenshot::{save_to_disk, Screenshot};

use crate::Args;

pub struct ScreenshotPlugin;

impl Plugin for ScreenshotPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (auto_screenshot, tour, perf_log));
        if std::env::var_os("FC3_VIEW_MESH_DEBUG").is_some() {
            app.add_systems(PostUpdate, view_mesh_debug.after(bevy::transform::TransformSystems::Propagate));
        }
        if std::env::var("FC3_HITCH").is_ok() {
            add_hitch_log(app);
        }
    }
}

/// One-shot crosshair inspection of CPU render triangles, including meshes
/// without physics colliders. The value is elapsed gameplay seconds.
fn view_mesh_debug(time: Res<Time>, mut done: Local<bool>, server: Res<AssetServer>, meshes: Res<Assets<Mesh>>,
    cameras: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    parts: Query<(Entity, &Mesh3d, &GlobalTransform, &InheritedVisibility)>,
    owners: Query<(Option<&crate::objects::PlacedId>, Option<&ChildOf>)>) {
    let Some(at) = std::env::var("FC3_VIEW_MESH_DEBUG").ok().and_then(|s| s.parse::<f32>().ok()).filter(|v| v.is_finite()) else { return };
    if *done || time.elapsed_secs() < at { return; }
    let Ok(camera) = cameras.single() else { return };
    *done = true;
    let origin = camera.translation();
    let direction = camera.forward().as_vec3();
    let mut hits = Vec::new();
    let mut inspected = 0;
    for (entity, handle, transform, visible) in &parts {
        if !visible.get() { continue; }
        let Some(mesh) = meshes.get(&handle.0) else { continue };
        if !mesh.asset_usage.contains(bevy::asset::RenderAssetUsages::MAIN_WORLD) { continue; }
        if mesh.primitive_topology() != bevy::mesh::PrimitiveTopology::TriangleList { continue; }
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else { continue };
        inspected += 1;
        let inverse = transform.affine().inverse();
        let o = inverse.transform_point3(origin);
        let d = inverse.transform_vector3(direction);
        if !o.is_finite() || !d.is_finite() { continue; }
        let indices: Vec<usize> = mesh.indices().map(|i| i.iter().collect()).unwrap_or_else(|| (0..positions.len()).collect());
        let mut closest = 10.0f32;
        let mut triangle = None;
        for tri in indices.chunks_exact(3) {
            let [a, b, c] = [Vec3::from_array(positions[tri[0]]), Vec3::from_array(positions[tri[1]]), Vec3::from_array(positions[tri[2]])];
            let (e1, e2) = (b - a, c - a);
            let p = d.cross(e2);
            let det = e1.dot(p);
            if det.abs() < 1e-8 { continue; }
            let t = o - a;
            let u = t.dot(p) / det;
            let q = t.cross(e1);
            let v = d.dot(q) / det;
            let distance = e2.dot(q) / det;
            if u >= 0.0 && v >= 0.0 && u + v <= 1.0 && distance >= 0.0 && distance < closest {
                closest = distance;
                triangle = Some([a, b, c]);
            }
        }
        if let Some(triangle) = triangle {
            let mut owner = entity;
            let mut id = None;
            for _ in 0..32 {
                let Ok((placed, parent)) = owners.get(owner) else { break };
                if let Some(placed) = placed { id = Some(placed.0); break; }
                let Some(parent) = parent else { break };
                owner = parent.parent();
            }
            hits.push((closest, entity, id, server.get_path(handle.0.id()).map(|p| p.to_string()), triangle));
        }
    }
    hits.sort_by(|a, b| a.0.total_cmp(&b.0));
    info!("view mesh inspection: origin {origin:?} direction {direction:?}, {inspected} meshes, {} hits", hits.len());
    for hit in hits.iter().take(12) { info!("view mesh inspection: {hit:?}"); }
}

/// `FC3_HITCH`: for each frame over 25 ms, where the main thread's time went
/// (named stretches of the schedules, the physics loop summed over its
/// steps) and what came in with it: entities spawned (by objects and
/// vegetation), images and meshes added. A frame's length is only known at
/// the next one's start, so each frame's figures are kept until then.
#[derive(Resource, Default)]
struct Hitch {
    /// When the current stretch began, and the stretch.
    mark: Option<(std::time::Instant, usize)>,
    /// Time per stretch this frame (ms).
    spent: [f32; STRETCHES],
    steps: u32,
    /// Avian's own timings, summed over this frame's steps (ms): broad
    /// phase, narrow phase, solver, collider trees.
    avian: [f32; 4],
}

const STRETCHES: usize = 8;
const STRETCH_NAMES: [&str; STRETCHES] = ["pre", "fixed gameplay", "avian", "fixed after", "update", "transforms", "visibility", "post rest"];

/// Entities each streaming spawner made this frame (for `FC3_HITCH`).
pub static SPAWNED_OBJECTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static SPAWNED_VEGETATION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Ends the current stretch and starts stretch `next`.
fn stretch(next: usize) -> impl FnMut(ResMut<Hitch>) {
    move |mut h: ResMut<Hitch>| end_stretch(&mut h, next)
}

fn end_stretch(h: &mut Hitch, next: usize) {
    let now = std::time::Instant::now();
    if let Some((t, i)) = h.mark {
        h.spent[i] += (now - t).as_secs_f32() * 1000.0;
    }
    h.mark = Some((now, next));
}

fn add_hitch_log(app: &mut App) {
    use avian3d::schedule::PhysicsSystems;
    use bevy::camera::visibility::VisibilitySystems;
    use bevy::transform::TransformSystems;
    app.init_resource::<Hitch>()
        .add_systems(First, |mut h: ResMut<Hitch>| {
            h.spent = [0.0; STRETCHES];
            h.steps = 0;
            h.avian = [0.0; 4];
            h.mark = Some((std::time::Instant::now(), 0));
        })
        .add_systems(FixedFirst, (stretch(1), |mut h: ResMut<Hitch>| h.steps += 1).chain())
        .add_systems(
            FixedLast,
            |mut h: ResMut<Hitch>,
             collision: Option<Res<avian3d::collision::CollisionDiagnostics>>,
             solver: Option<Res<avian3d::dynamics::solver::SolverDiagnostics>>,
             tree: Option<Res<avian3d::collider_tree::ColliderTreeDiagnostics>>| {
                let ms = |d: std::time::Duration| d.as_secs_f32() * 1000.0;
                if let Some(c) = collision {
                    h.avian[0] += ms(c.broad_phase);
                    h.avian[1] += ms(c.narrow_phase);
                }
                if let Some(s) = solver {
                    h.avian[2] += ms(s.prepare_constraints
                        + s.update_velocity_increments
                        + s.integrate_velocities
                        + s.warm_start
                        + s.solve_constraints
                        + s.integrate_positions
                        + s.relax_velocities
                        + s.apply_restitution
                        + s.finalize
                        + s.store_impulses
                        + s.swept_ccd);
                }
                if let Some(t) = tree {
                    h.avian[3] += ms(t.optimize + t.update);
                }
            },
        )
        .add_systems(FixedPostUpdate, (stretch(2).before(PhysicsSystems::First), stretch(3).after(PhysicsSystems::Last)))
        .add_systems(Update, stretch(4).before(tour))
        .add_systems(
            PostUpdate,
            (
                stretch(5).before(TransformSystems::Propagate),
                stretch(6).after(TransformSystems::Propagate).before(VisibilitySystems::CheckVisibility),
                stretch(7).after(VisibilitySystems::CheckVisibility),
            ),
        )
        .add_systems(Last, hitch_log);
}

#[derive(Default)]
struct HitchState {
    entities: usize,
    /// The last frame's report.
    last: Option<String>,
}

fn hitch_log(
    time: Res<Time<Real>>,
    mut hitch: ResMut<Hitch>,
    mut state: Local<HitchState>,
    entities: Query<Entity>,
    mut images: MessageReader<AssetEvent<Image>>,
    mut meshes: MessageReader<AssetEvent<Mesh>>,
) {
    // This frame's start-to-start length is the previous frame's.
    if let Some(report) = state.last.take() {
        if time.delta_secs() > 0.025 && time.elapsed_secs() > 15.0 {
            info!("hitch: {:.1} ms frame | {report}", time.delta_secs() * 1000.0);
        }
    }
    end_stretch(&mut hitch, 0);
    let n = entities.iter().count();
    let spawned = n as isize - state.entities as isize;
    state.entities = n;
    let new_images = images.read().filter(|e| matches!(e, AssetEvent::Added { .. })).count();
    let new_meshes = meshes.read().filter(|e| matches!(e, AssetEvent::Added { .. })).count();
    use std::sync::atomic::Ordering::Relaxed;
    let (objects, vegetation) = (SPAWNED_OBJECTS.swap(0, Relaxed), SPAWNED_VEGETATION.swap(0, Relaxed));
    let total: f32 = hitch.spent.iter().sum();
    let parts: Vec<String> = STRETCH_NAMES.iter().zip(hitch.spent).filter(|(_, t)| *t >= 1.0).map(|(n, t)| format!("{n} {t:.1}")).collect();
    state.last = Some(format!(
        "main thread {total:.1} ms ({}; {} physics steps: broad {:.1}, narrow {:.1}, solver {:.1}, trees {:.1}) | entities {spawned:+} (objects {objects}, vegetation {vegetation}) | images +{new_images} | meshes +{new_meshes}",
        parts.join(", "),
        hitch.steps,
        hitch.avian[0],
        hitch.avian[1],
        hitch.avian[2],
        hitch.avian[3]
    ));
}

/// Soak test: flies the player along a loop of waypoints over the main
/// islands, logging frame time and entity counts every few seconds, then exits.
#[allow(clippy::too_many_arguments)]
fn tour(
    args: Res<Args>,
    time: Res<Time<Real>>,
    mut state: Local<(f32, f32, u32, f32, f32)>,
    mut players: Query<(&mut Transform, &mut crate::player::Player)>,
    entities: Query<Entity>,
    meshes: Query<(), With<Mesh3d>>,
    npcs: Query<(), With<crate::npc::Npc>>,
    terrain: Res<crate::terrain::TerrainStreamer>,
    mut exit: MessageWriter<AppExit>,
) {
    if args.tour_secs <= 0.0 {
        return;
    }
    // Dunia (x, y) waypoints: north island, the channel, south island, back.
    const ROUTE: [[f32; 2]; 6] = [[40.0, 2060.0], [1500.0, 2400.0], [2900.0, 2900.0], [1200.0, 200.0], [-1600.0, -1400.0], [-300.0, 1700.0]];
    let (elapsed, last_log, frames, worst, accum) = &mut *state;
    let dt = time.delta_secs();
    *elapsed += dt;
    *frames += 1;
    *worst = worst.max(dt);
    *accum += dt;
    let Ok((mut t, mut p)) = players.single_mut() else { return };
    p.noclip = true;
    // Fly at ~60 m/s (FC3_TOUR_SPEED=m/s), 80 m above the terrain-ish altitude band.
    let total = ROUTE.len() as f32;
    let speed = std::env::var("FC3_TOUR_SPEED").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(60.0);
    let progress = (*elapsed * speed) / 1700.0; // waypoint legs are ~1.7 km
    let leg = (progress as usize) % ROUTE.len();
    let f = progress.fract();
    let a = ROUTE[leg];
    let b = ROUTE[(leg + 1) % ROUTE.len()];
    let pos = crate::data::to_bevy([a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, 230.0]);
    let dir = crate::data::to_bevy([b[0] - a[0], b[1] - a[1], 0.0]).normalize_or_zero();
    t.translation = pos;
    p.yaw = (-dir.x).atan2(-dir.z);
    p.pitch = -0.35;
    let _ = total;
    if *elapsed - *last_log >= 5.0 {
        let avg = *accum / *frames as f32;
        info!(
            "tour t={:.0}s avg {:.1} ms ({:.0} fps) worst {:.1} ms | entities {} meshes {} npcs {} terrain {}",
            *elapsed,
            avg * 1000.0,
            1.0 / avg.max(1e-6),
            *worst * 1000.0,
            entities.iter().count(),
            meshes.iter().count(),
            npcs.iter().count(),
            terrain.loaded_count()
        );
        *last_log = *elapsed;
        *frames = 0;
        *worst = 0.0;
        *accum = 0.0;
    }
    if *elapsed > args.tour_secs {
        info!("tour complete");
        exit.write(AppExit::Success);
    }
}

/// `FC3_PERF`: logs frame times (average, worst, 99th percentile) and what's
/// about every five seconds, wherever the player is.
fn perf_log(
    time: Res<Time<Real>>,
    mut samples: Local<(Vec<f32>, f32)>,
    entities: Query<Entity>,
    meshes: Query<(), With<Mesh3d>>,
    npcs: Query<(), With<crate::npc::Npc>>,
    rigs: Query<(), With<crate::characters::CharacterRig>>,
    players: Query<&Transform, With<crate::player::Player>>,
    distinct: (
        Query<(&ViewVisibility, &MeshMaterial3d<StandardMaterial>, Has<bevy::light::NotShadowCaster>)>,
        Query<(&ViewVisibility, &MeshMaterial3d<crate::original_shaders::OriginalMaterial>, Has<bevy::light::NotShadowCaster>)>,
        Query<(&ViewVisibility, &MeshMaterial3d<crate::vegetation::FoliageMaterial>, Has<bevy::light::NotShadowCaster>)>,
        Query<(&ViewVisibility, &MeshMaterial3d<crate::cache_assets::TerrainMaterial>, Has<bevy::light::NotShadowCaster>)>,
    ),
    kinds: Query<
        (
            &ViewVisibility,
            Has<MeshMaterial3d<StandardMaterial>>,
            Has<MeshMaterial3d<crate::original_shaders::OriginalMaterial>>,
            Has<MeshMaterial3d<crate::vegetation::FoliageMaterial>>,
            Has<MeshMaterial3d<crate::cache_assets::TerrainMaterial>>,
            Has<MeshMaterial3d<crate::metal::MetalMaterial>>,
            Has<bevy::light::NotShadowCaster>,
        ),
        With<Mesh3d>,
    >,
) {
    if std::env::var("FC3_PERF").is_err() {
        return;
    }
    let (frames, since) = &mut *samples;
    frames.push(time.delta_secs());
    *since += time.delta_secs();
    if *since < 5.0 {
        return;
    }
    let mut sorted = std::mem::take(frames);
    sorted.sort_by(|a, b| a.total_cmp(b));
    let avg = sorted.iter().sum::<f32>() / sorted.len().max(1) as f32;
    let p99 = sorted[(sorted.len() as f32 * 0.99) as usize % sorted.len().max(1)];
    info!(
        "perf at {:.0?}: avg {:.1} ms ({:.0} fps) p99 {:.1} ms worst {:.1} ms | entities {} meshes {} npcs {} rigs {}",
        players.single().map(|t| t.translation).unwrap_or_default(),
        avg * 1000.0,
        1.0 / avg.max(1e-6),
        p99 * 1000.0,
        sorted.last().copied().unwrap_or(0.0) * 1000.0,
        entities.iter().count(),
        meshes.iter().count(),
        npcs.iter().count(),
        rigs.iter().count(),
    );
    *since = 0.0;
    // Meshes by material (all, visible to the camera) and shadow casters.
    let mut counts = [[0u32; 2]; 6];
    let mut casters = 0;
    for (vis, std_m, orig, foliage, terrain, metal, no_shadow) in &kinds {
        let k = [std_m, orig, foliage, terrain, metal].iter().position(|b| *b).unwrap_or(5);
        counts[k][0] += 1;
        counts[k][1] += vis.get() as u32;
        casters += !no_shadow as u32;
    }
    // Distinct materials among visible meshes (and visible shadow casters):
    // each is its own batch in every view unless the material is bindless.
    fn count_distinct<M: Asset>(q: impl Iterator<Item = (bool, AssetId<M>, bool)>) -> [usize; 2] {
        let (mut all, mut casting) = (std::collections::HashSet::new(), std::collections::HashSet::new());
        for (vis, id, no_shadow) in q.filter(|x| x.0) {
            let _ = vis;
            all.insert(id);
            if !no_shadow {
                casting.insert(id);
            }
        }
        [all.len(), casting.len()]
    }
    info!(
        "distinct visible materials (all/casting): standard {:?} original {:?} foliage {:?} terrain {:?}",
        count_distinct(distinct.0.iter().map(|(v, m, n)| (v.get(), m.id(), n))),
        count_distinct(distinct.1.iter().map(|(v, m, n)| (v.get(), m.id(), n))),
        count_distinct(distinct.2.iter().map(|(v, m, n)| (v.get(), m.id(), n))),
        count_distinct(distinct.3.iter().map(|(v, m, n)| (v.get(), m.id(), n))),
    );
    info!(
        "meshes (all/visible): standard {:?} original {:?} foliage {:?} terrain {:?} metal {:?} other {:?}; shadow casters {casters}",
        counts[0], counts[1], counts[2], counts[3], counts[4], counts[5]
    );
}

/// Captures the window to `path` (tests that time their own shots).
pub fn capture(commands: &mut Commands, path: String) {
    info!("capturing screenshot to {path}");
    commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
}

/// Waits for streaming to settle, captures the window, then exits.
fn auto_screenshot(
    args: Res<Args>,
    // Real time: the map and pause screens stop virtual time.
    time: Res<Time<Real>>,
    mut taken: Local<bool>,
    mut quit_at: Local<Option<f32>>,
    terrain: Res<crate::terrain::TerrainStreamer>,
    objects: Res<crate::objects::ObjectStreamer>,
    vegetation: Res<crate::vegetation::VegStreamer>,
    heights: Res<crate::terrain::Heightfield>,
    player: Query<&crate::player::Player>,
    game_state: Res<State<crate::menu::GameState>>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(path) = args.screenshot.clone() else { return };
    if args.tour_secs > 0.0 {
        return;
    }
    if let Some(t) = *quit_at {
        if time.elapsed_secs() > t {
            exit.write(AppExit::Success);
        }
        return;
    }
    let spawned = player.single().map(|p| !p.spawning).unwrap_or(true);
    // While converting on first launch, capture the progress screen itself.
    let loading = *game_state.get() == crate::menu::GameState::Loading;
    let settled =
        loading || spawned && terrain.pending_count() == 0 && objects.pending_count() == 0 && vegetation.pending_count() == 0 && !heights.sectors.is_empty();
    if !*taken && ((settled && time.elapsed_secs() > args.settle) || time.elapsed_secs() > args.settle + 60.0) {
        *taken = true;
        info!("capturing screenshot to {path}");
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        *quit_at = Some(time.elapsed_secs() + 2.0);
    }
}
