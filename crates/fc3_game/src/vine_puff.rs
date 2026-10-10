//! Opt-in authored puff preview. Preserves the source geometry and knot
//! values; interpolation, SH lighting and soft intersections remain provisional.
use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use fc3_formats::{particle_effects::{Effect, Emitter}, xml::Element};
use crate::{particles::ParticleMaterial, player::PlayerCamera};

pub struct VinePuffPlugin;
impl Plugin for VinePuffPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Puff>().add_systems(Startup, load)
            .add_systems(PostUpdate, draw.after(crate::vine::VineView)
                .before(bevy::transform::TransformSystems::Propagate));
    }
}

#[derive(Resource, Default)]
pub struct Puff {
    source: Option<Emitter>,
    mesh: Option<Handle<Mesh>>,
    entity: Option<Entity>,
    was_active: bool,
}

fn load(mut puff: ResMut<Puff>, server: Res<AssetServer>, mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ParticleMaterial>>, mut commands: Commands) {
    if std::env::var("FC3_VINE_PUFF_TEST").as_deref() != Ok("1") { return; }
    let result = (|| -> anyhow::Result<Emitter> {
        use anyhow::{Context, ensure};
        let effect = Effect::parse(&std::fs::read_to_string(fc3_formats::cache::default_cache_root()
            .join(fc3_formats::particle_effects::VINE_CACHE_REL))?)?;
        ensure!(effect.emitters.len() == 1, "puff preview requires one emitter");
        let e = effect.emitters.into_iter().next().context("missing emitter")?;
        ensure!(e.active && !e.looping && e.source.u32("BlendType") == Some(0), "unsupported puff emitter");
        ensure!(e.source.child("Flags").and_then(|f| f.bool("LocalEmitterSpace")) == Some(true), "expected local emitter");
        ensure!(polygon(&e).len() >= 3, "missing sprite polygon");
        Ok(e)
    })();
    let e = match result { Ok(e) => e, Err(e) => { warn!("vine puff: {e:#}"); return } };
    let path = match e.texture_path() { Ok(path) => path, Err(e) => { warn!("vine puff: {e:#}"); return } };
    let texture = server.load_builder().with_settings(|s: &mut bevy::image::ImageLoaderSettings| { s.is_srgb = true; })
        .load(crate::cache_assets::cache_path(&fc3_formats::cache::texture_rel(&path)));
    let mesh = meshes.add(Mesh::from(Rectangle::new(0.001, 0.001)));
    let entity = commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(materials.add(ParticleMaterial::alpha_sheet(texture))),
        Transform::IDENTITY, Visibility::Hidden, NotShadowCaster, NotShadowReceiver,
        bevy::camera::visibility::NoFrustumCulling, Name::new("authored vine puff preview"))).id();
    puff.source = Some(e);
    puff.mesh = Some(mesh);
    puff.entity = Some(entity);
}

fn polygon(e: &Emitter) -> Vec<Vec2> {
    e.source.child("VertexBuffer").into_iter().flat_map(|b| &b.children)
        .filter_map(|v| Some(Vec2::new(v.f32("x")?, v.f32("y")?))).collect()
}

/// Preview interpolation only: keep paired tangent-bearing source knots in
/// the cache and interpolate their authored values without claiming native
/// particle-curve evaluation parity.
fn curve(e: &Element, name: &str, t: f32, fallback: f32) -> f32 {
    let points: Vec<Vec2> = e.child(name).into_iter().flat_map(|c| &c.children).filter(|c| c.name == "Knot")
        .filter_map(|k| {
            let values: Vec<f32> = k.attr("Value")?.split(',').filter_map(|s| s.parse().ok()).collect();
            (values.len() >= 2).then(|| Vec2::new(values[0], values[1]))
        }).collect();
    let Some(first) = points.first() else { return fallback };
    if t <= first.x { return first.y; }
    for pair in points.windows(2) {
        let [a, b] = [pair[0], pair[1]];
        if t <= b.x && b.x > a.x { return a.y + (b.y - a.y) * ((t - a.x) / (b.x - a.x)).clamp(0.0, 1.0); }
    }
    points.last().unwrap().y
}

fn colour(e: &Emitter) -> Vec3 {
    let values: Vec<f32> = e.source.child("PartColorKF").and_then(|c| c.child("Key")).and_then(|c| c.attr("Value"))
        .unwrap_or("1,1,1").split(',').filter_map(|s| s.parse().ok()).collect();
    if values.len() == 3 { Vec3::from_slice(&values) } else { Vec3::ONE }
}

// Deterministic per-particle variation makes fixture comparisons repeatable.
fn random(index: u32, lane: u32) -> f32 {
    let mut x = index.wrapping_add(1).wrapping_mul(0x9e3779b9) ^ lane.wrapping_mul(0x85ebca6b);
    x ^= x >> 16; x = x.wrapping_mul(0x7feb352d); x ^= x >> 15;
    (x & 0xffffff) as f32 / 16777216.0
}

fn births(e: &Emitter, elapsed: f32) -> Vec<f32> {
    if elapsed < e.delay { return Vec::new(); }
    let mut out = vec![e.delay; e.initial_count.min(256) as usize];
    let rate = e.base_value("EmitRateKF").unwrap_or(0.0);
    if rate > 0.0 {
        for n in 1..=256 {
            let at = e.delay + n as f32 / rate;
            if at > elapsed || at >= e.delay + e.duration { break; }
            out.push(at);
        }
    }
    out
}

fn puff_mesh(e: &Emitter, origin: Transform, camera: Quat, elapsed: f32) -> Mesh {
    let shape = polygon(e);
    let life = e.base_value("InitLifeTimeKF").unwrap_or(0.0);
    let mut positions = Vec::<[f32; 3]>::new();
    let mut normals = Vec::<[f32; 3]>::new();
    let mut uvs = Vec::<[f32; 2]>::new();
    let mut colours = Vec::<[f32; 4]>::new();
    let mut indices = Vec::<u32>::new();
    for (index, birth) in births(e, elapsed).into_iter().enumerate() {
        let age = elapsed - birth;
        if life <= 0.0 || age >= life { continue; }
        let index = index as u32;
        let t = age / life;
        let size = e.source.f32("StaticSize").unwrap_or(1.0) * e.base_value("InitSizeKF").unwrap_or(1.0)
            * curve(&e.source, "PartSizeKF", t, 1.0);
        let alpha = curve(&e.source, "PartAlphaKF", t, 1.0).clamp(0.0, 1.0);
        let rgb = colour(e);
        let rotation = (e.base_value("InitAnglesYKF").unwrap_or(0.0) + random(index, 0) * 360.0).to_radians();
        let orientation = camera * Quat::from_rotation_z(rotation);
        let radius = Vec3::new(e.source.f32("EmitVolHalfSizeX").unwrap_or(0.0),
            e.source.f32("EmitVolHalfSizeY").unwrap_or(0.0), e.source.f32("EmitVolHalfSizeZ").unwrap_or(0.0));
        let local = Vec3::from_array(e.offset) + radius * Vec3::new(random(index, 1) * 2.0 - 1.0,
            random(index, 2) * 2.0 - 1.0, random(index, 3) * 2.0 - 1.0);
        let centre = origin.transform_point(crate::data::to_bevy(local.to_array()));
        let fps = e.fps + (random(index, 4) * 2.0 - 1.0) * e.fps_variant;
        let frames = e.frames[0] * e.frames[1];
        let start = if e.source.bool("FrameRandomStart").unwrap_or(false) { (random(index, 5) * frames as f32) as u32 } else { 0 };
        let frame = ((age * fps) as u32 + start) % frames;
        let base = positions.len() as u32;
        for point in &shape {
            positions.push((centre + orientation * Vec3::new(point.x, point.y, 0.0) * size * 0.5).to_array());
            normals.push((camera * Vec3::Z).to_array());
            uvs.push([(frame % e.frames[0]) as f32 / e.frames[0] as f32 + (point.x * 0.5 + 0.5) / e.frames[0] as f32,
                (frame / e.frames[0]) as f32 / e.frames[1] as f32 + (0.5 - point.y * 0.5) / e.frames[1] as f32]);
            colours.push(rgb.extend(alpha).to_array());
        }
        for n in 1..shape.len().saturating_sub(1) { indices.extend([base, base + n as u32, base + n as u32 + 1]); }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colours);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

fn draw(vines: Res<crate::vine::Vines>, mut puff: ResMut<Puff>, players: Query<&Transform, With<crate::player::Player>>,
    cameras: Query<&Transform, With<PlayerCamera>>, mut meshes: ResMut<Assets<Mesh>>, mut visibility: Query<&mut Visibility>) {
    let (Some(entity), Some(mesh)) = (puff.entity, puff.mesh.clone()) else { return };
    let Ok(mut visible) = visibility.get_mut(entity) else { return };
    let Some((elapsed, offset)) = vines.puff_phase() else {
        if puff.was_active { info!("vine puff: release"); puff.was_active = false; }
        *visible = Visibility::Hidden; return;
    };
    let Ok(player) = players.single() else { return };
    let Ok(camera) = cameras.single() else { return };
    // Original VineClimbingFX attaches to Camera with local (0, offset, -0.8),
    // not to the pawn's feet. Compose current local transforms here because
    // GlobalTransform propagation has not run yet (notably on teleport).
    let view = *player * *camera;
    let mut origin = view;
    origin.translation = view.transform_point(Vec3::new(0.0, offset, -0.8));
    if !puff.was_active { info!("vine puff: start at {:?}", origin.translation); puff.was_active = true; }
    let Some(e) = &puff.source else { return };
    if let Some(mut mesh) = meshes.get_mut(&mesh) {
        *mesh = puff_mesh(e, origin, view.rotation, elapsed);
        *visible = Visibility::Inherited;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires FC3_TEST_CACHE with authored vine particle XML"]
    fn installed_preview_preserves_polygon_and_follows_anchor_without_new_emissions() {
        let root = std::path::PathBuf::from(std::env::var_os("FC3_TEST_CACHE").expect("FC3_TEST_CACHE"));
        let effect = Effect::parse(&std::fs::read_to_string(root.join(fc3_formats::particle_effects::VINE_CACHE_REL)).unwrap()).unwrap();
        let e = &effect.emitters[0];
        assert_eq!(births(e, 0.0), [0.0]);
        assert_eq!(births(e, 0.5), [0.0, 0.25, 0.5]);
        assert_eq!(births(e, 1.5), [0.0, 0.25, 0.5, 0.75]);
        assert_eq!(births(e, 8.0), births(e, 1.5));
        let mesh = puff_mesh(e, Transform::IDENTITY, Quat::IDENTITY, 0.5);
        assert_eq!(mesh.count_vertices(), 18); // Three authored six-vertex particles.
        assert_eq!(mesh.indices().unwrap().len(), 36);
        let shifted = puff_mesh(e, Transform::from_xyz(12.0, -5.0, 7.0), Quat::IDENTITY, 0.5);
        let positions = |m: &Mesh| match m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::mesh::VertexAttributeValues::Float32x3(p) => p.clone(), _ => panic!(),
        };
        for (a, b) in positions(&mesh).iter().zip(positions(&shifted)) {
            assert!((Vec3::from_array(b) - Vec3::from_array(*a)).abs_diff_eq(Vec3::new(12.0, -5.0, 7.0), 1e-5));
        }
        assert_eq!(puff_mesh(e, Transform::IDENTITY, Quat::IDENTITY, 11.0).count_vertices(), 0);
    }
}
