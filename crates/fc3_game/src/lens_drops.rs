//! Water on the lens, coming up out of the water: the player's own droplet
//! effect (the player prototype's `ScreenDropletParticle`, cached as
//! `particles/screen-droplets.xml`). Its drops are worked out from the
//! effect's emitter: how many, how long each lasts, its size, where it starts
//! in front of the eye (the emitter's volume, turned by its `RotOffset`), how
//! its gravity carries it (in the eye's own axes, at the authored strength)
//! and how it fades. They are drawn as a pass over the finished frame that
//! bends the view through their normal map (`DistortionStrength`), blurs it a
//! little through the water and lets their rims catch the surroundings
//! (`EnvMapTexture`).
//!
//! The emitter's `StaticSize` isn't a scale: the drops' sizes come from
//! `InitSizeKF` alone (17 times it would make one drop wider than the view).

use bevy::core_pipeline::{Core3d, Core3dSystems, FullscreenShader};
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer};
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::texture::GpuImage;
use bevy::render::uniform::{ComponentUniforms, DynamicUniformIndex, UniformComponentPlugin};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use fc3_formats::particle_effects::{Effect, Emitter};

use crate::player::PlayerCamera;
use crate::vine_puff::{births, colour, curve, curve_integrals, random, variant};

pub struct LensDropsPlugin;
impl Plugin for LensDropsPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "lens_drops.wgsl");
        app.add_plugins((
            ExtractComponentPlugin::<LensDrops>::default(),
            UniformComponentPlugin::<LensDrops>::default(),
            ExtractComponentPlugin::<LensDropImages>::default(),
        ))
        .init_resource::<Drops>()
        .add_systems(OnExit(crate::menu::GameState::Loading), load)
        .add_systems(Update, run.run_if(in_state(crate::menu::GameState::Playing)));
        let Some(render) = app.get_sub_app_mut(RenderApp) else { return };
        render
            .init_gpu_resource::<SpecializedRenderPipelines<DropsPipeline>>()
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(Render, prepare.in_set(RenderSystems::Prepare))
            .add_systems(Core3d, draw.in_set(Core3dSystems::PostProcess).after(bevy::core_pipeline::tonemapping::tonemapping));
    }
}

/// Most drops drawn at once.
pub const MAX_DROPS: usize = 32;

/// The drops on the lens this frame (on the player's camera).
#[derive(Component, Clone, Copy, ExtractComponent, ShaderType)]
pub struct LensDrops {
    /// How far the drops bend the view, how much they blur it, their own
    /// colour's share and how much of the surroundings they catch.
    pub strengths: Vec4,
    /// Their tint (rgb) and how many there are (w).
    pub tint: Vec4,
    /// The drop atlas's columns and rows.
    pub grid: Vec4,
    /// Each drop's centre and half size, in screen UV.
    pub rects: [Vec4; MAX_DROPS],
    /// Each drop's atlas column and row, mirroring (1: across, 2: up and
    /// down) and alpha.
    pub cells: [Vec4; MAX_DROPS],
}

/// The drop shapes, their normal map and the surroundings they reflect.
#[derive(Component, Clone, ExtractComponent)]
pub struct LensDropImages {
    shapes: Handle<Image>,
    normals: Handle<Image>,
    surroundings: Handle<Image>,
}

#[derive(Resource, Default)]
struct Drops {
    emitter: Option<Emitter>,
    images: Option<LensDropImages>,
    /// Seconds since the drops went on, while they last.
    elapsed: Option<f32>,
    was_under: bool,
}

fn load(mut drops: ResMut<Drops>, server: Res<AssetServer>) {
    let result = (|| -> anyhow::Result<(Emitter, [String; 3])> {
        use anyhow::Context;
        let xml = std::fs::read_to_string(fc3_formats::cache::default_cache_root().join(fc3_formats::particle_effects::SCREEN_DROPLETS_REL))?;
        let e = Effect::parse(&xml)?.emitters.into_iter().find(|e| e.active).context("no active emitter")?;
        let shapes = e.texture_of("DistortionTexture").unwrap_or_else(|| e.texture_path())?;
        let normals = e.texture_of("NormalTexture").context("no normal map")??;
        let surroundings = e.texture_of("EnvMapTexture").context("no surroundings")??;
        Ok((e, [shapes, normals, surroundings]))
    })();
    let (e, [shapes, normals, surroundings]) = match result {
        Ok(found) => found,
        Err(e) => {
            warn!("lens drops: {e:#}");
            return;
        }
    };
    let image = |path: &str, srgb: bool| -> Handle<Image> {
        server
            .load_builder()
            .with_settings(move |s: &mut bevy::image::ImageLoaderSettings| s.is_srgb = srgb)
            .load(crate::cache_assets::cache_path(&fc3_formats::cache::texture_rel(path)))
    };
    drops.images = Some(LensDropImages { shapes: image(&shapes, false), normals: image(&normals, false), surroundings: image(&surroundings, true) });
    drops.emitter = Some(e);
}

fn run(
    time: Res<Time>,
    heights: Res<crate::terrain::Heightfield>,
    mut drops: ResMut<Drops>,
    cameras: Query<(Entity, &GlobalTransform, &Projection), With<PlayerCamera>>,
    mut test: Local<(f32, Option<Option<f32>>)>,
    mut commands: Commands,
) {
    let Ok((entity, eye, projection)) = cameras.single() else { return };
    let under = crate::water::submerged(eye.translation(), &heights, 0.0);
    // `FC3_LENS_DROPS_TEST=<secs>`: the drops go on that long into play, to
    // look at them without a swim.
    let (played, at) = &mut *test;
    let at = *at.get_or_insert_with(|| std::env::var("FC3_LENS_DROPS_TEST").ok().and_then(|s| s.parse::<f32>().ok()));
    let before = *played;
    *played += time.delta_secs();
    let tested = at.is_some_and(|at| before < at && *played >= at);
    if (drops.was_under && !under || tested) && drops.emitter.is_some() {
        drops.elapsed = Some(0.0);
    }
    drops.was_under = under;
    let Drops { emitter: Some(e), images: Some(images), elapsed: Some(elapsed), .. } = &mut *drops else { return };
    *elapsed += time.delta_secs();
    let Projection::Perspective(lens) = projection else { return };
    if *elapsed > lasts(e) {
        drops.elapsed = None;
        commands.entity(entity).remove::<(LensDrops, LensDropImages)>();
        return;
    }
    let list = drops_at(e, *elapsed, (lens.fov * 0.5).tan(), lens.aspect_ratio);
    commands.entity(entity).insert((uniform(e, &list), images.clone()));
    // (`FC3_LENS_DROPS_CAPTURE=<path>`: a picture of the test's drops, a fifth
    // of a second in.)
    if at.is_some() && *elapsed >= 0.2 && *elapsed - time.delta_secs() < 0.2 {
        if let Some(path) = std::env::var_os("FC3_LENS_DROPS_CAPTURE") {
            use bevy::render::view::window::screenshot::{save_to_disk, Screenshot};
            info!("lens drops: {} drops, capture to {}", list.len(), std::path::Path::new(&path).display());
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(std::path::PathBuf::from(path)));
        }
    }
}

/// A drop on the lens, in screen UV.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Drop {
    pub centre: Vec2,
    pub half: Vec2,
    pub cell: UVec2,
    /// 1: mirrored across, 2: up and down.
    pub mirror: u32,
    pub alpha: f32,
}

/// The longest the emitter's drops can last.
fn lasts(e: &Emitter) -> f32 {
    e.delay + e.duration + e.base_value("InitLifeTimeKF").unwrap_or(0.0) + variant(e, "InitLifeTimeKF").abs()
}

fn numbers<const N: usize>(e: &Emitter, attribute: &str) -> [f32; N] {
    let values: Vec<f32> = e.source.attr(attribute).unwrap_or("").split(',').filter_map(|s| s.trim().parse().ok()).collect();
    values.try_into().unwrap_or([0.0; N])
}

/// The emitter's drops `elapsed` seconds after it went off, through a lens
/// with the given tangent of half its height's field and aspect. The emitter
/// sits in the eye's own axes (x right, y ahead, z up, as the game's).
pub(crate) fn drops_at(e: &Emitter, elapsed: f32, tan_half: f32, aspect: f32) -> Vec<Drop> {
    let to_eye = crate::data::to_bevy;
    let turn = Quat::from_array(fc3_formats::world::angles_to_bevy_quat(numbers(e, "RotOffset")));
    let volume = Vec3::new(
        e.source.f32("EmitVolHalfSizeX").unwrap_or(0.0),
        e.source.f32("EmitVolHalfSizeY").unwrap_or(0.0),
        e.source.f32("EmitVolHalfSizeZ").unwrap_or(0.0),
    );
    // (The authored gravity keeps its length: it is the drops' pull.)
    let gravity = to_eye(numbers(e, "InitGravityDir"));
    let frames = (e.frames[0] * e.frames[1]).max(1);
    let mut out = Vec::new();
    for (index, birth) in births(e, elapsed).into_iter().enumerate().take(MAX_DROPS) {
        let index = index as u32;
        let spread = |name: &str, lane: u32| e.base_value(name).unwrap_or(0.0) + (random(index, lane) * 2.0 - 1.0) * variant(e, name);
        let (age, life) = (elapsed - birth, spread("InitLifeTimeKF", 8));
        if life <= 0.0 || age < 0.0 || age >= life {
            continue;
        }
        let t = age / life;
        let start = Vec3::new(random(index, 1), random(index, 2), random(index, 3)) * 2.0 - Vec3::ONE;
        let at = to_eye(e.offset)
            + turn * to_eye((volume * start).to_array())
            + gravity * spread("InitGravityIntensityKF", 9) * life * life * curve_integrals(e, "PartGravityKF", t, 1.0).y;
        let depth = -at.z;
        if depth < 0.01 {
            continue;
        }
        // Metres across at that depth, to UV.
        let scale = Vec2::new(0.5 / (depth * tan_half * aspect), 0.5 / (depth * tan_half));
        let size = spread("InitSizeKF", 10) * curve(&e.source, "PartSizeKF", t, 1.0);
        let first = if e.source.bool("FrameRandomStart").unwrap_or(false) {
            (random(index, 5) * frames as f32) as u32
        } else {
            e.source.u32("StartFrame").unwrap_or(0)
        };
        let fps = (e.fps + (random(index, 4) * 2.0 - 1.0) * e.fps_variant).max(0.0);
        let frame = (first + (age * fps) as u32) % frames;
        let mirror = u32::from(e.source.bool("RandomMirrorU").unwrap_or(false) && random(index, 11) < 0.5)
            | (u32::from(e.source.bool("RandomMirrorV").unwrap_or(false) && random(index, 12) < 0.5) << 1);
        out.push(Drop {
            centre: Vec2::new(0.5 + at.x * scale.x, 0.5 - at.y * scale.y),
            half: Vec2::splat(size.max(0.0) * 0.5) * scale,
            cell: UVec2::new(frame % e.frames[0], frame / e.frames[0]),
            mirror,
            alpha: curve(&e.source, "PartAlphaKF", t, 1.0).clamp(0.0, 1.0),
        });
    }
    out
}

fn uniform(e: &Emitter, list: &[Drop]) -> LensDrops {
    let strength = |name: &str| e.source.f32(name).unwrap_or(0.0);
    let mut drops = LensDrops {
        strengths: Vec4::new(strength("DistortionStrength"), strength("DistortionBlurStrength"), strength("DiffuseMapStrength"), strength("EnvMapStrength")),
        tint: colour(e).extend(list.len() as f32),
        grid: Vec4::new(e.frames[0] as f32, e.frames[1] as f32, 0.0, 0.0),
        rects: [Vec4::ZERO; MAX_DROPS],
        cells: [Vec4::ZERO; MAX_DROPS],
    };
    for (i, d) in list.iter().take(MAX_DROPS).enumerate() {
        drops.rects[i] = Vec4::new(d.centre.x, d.centre.y, d.half.x, d.half.y);
        drops.cells[i] = Vec4::new(d.cell.x as f32, d.cell.y as f32, d.mirror as f32, d.alpha);
    }
    drops
}

#[derive(Resource)]
struct DropsPipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
    shader: Handle<Shader>,
}
#[derive(Component, Deref)]
struct PipelineId(CachedRenderPipelineId);

fn init_pipeline(mut commands: Commands, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>, server: Res<AssetServer>) {
    let texture = || texture_2d(TextureSampleType::Float { filterable: true });
    commands.insert_resource(DropsPipeline {
        layout: BindGroupLayoutDescriptor::new(
            "lens drops",
            &BindGroupLayoutEntries::sequential(
                ShaderStages::FRAGMENT,
                (texture(), sampler(SamplerBindingType::Filtering), texture(), texture(), texture(), uniform_buffer::<LensDrops>(true)),
            ),
        ),
        sampler: device.create_sampler(&SamplerDescriptor {
            min_filter: FilterMode::Linear,
            mag_filter: FilterMode::Linear,
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            ..default()
        }),
        fullscreen: fullscreen.clone(),
        shader: bevy::asset::load_embedded_asset!(server.as_ref(), "lens_drops.wgsl"),
    });
}
impl SpecializedRenderPipeline for DropsPipeline {
    type Key = TextureFormat;
    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("lens drops".into()),
            layout: vec![self.layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
                ..default()
            }),
            ..default()
        }
    }
}
fn prepare(
    mut commands: Commands,
    cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<DropsPipeline>>,
    pipeline: Res<DropsPipeline>,
    views: Query<(Entity, &ExtractedView), With<LensDrops>>,
) {
    for (entity, view) in &views {
        commands.entity(entity).insert(PipelineId(pipelines.specialize(&cache, &pipeline, view.target_format)));
    }
}
fn draw(
    view: ViewQuery<(&ViewTarget, &PipelineId, &LensDrops, &LensDropImages, &DynamicUniformIndex<LensDrops>)>,
    cache: Res<PipelineCache>,
    pipeline: Res<DropsPipeline>,
    uniforms: Res<ComponentUniforms<LensDrops>>,
    images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
) {
    let (target, id, drops, textures, index) = view.into_inner();
    if drops.tint.w < 1.0 {
        return;
    }
    let (Some(render_pipeline), Some(binding), Some(shapes), Some(normals), Some(surroundings)) = (
        cache.get_render_pipeline(**id),
        uniforms.uniforms().binding(),
        images.get(&textures.shapes),
        images.get(&textures.normals),
        images.get(&textures.surroundings),
    ) else {
        return;
    };
    let post = target.post_process_write();
    let bind = ctx.render_device().create_bind_group(
        Some("lens drops"),
        &cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((post.source, &pipeline.sampler, &shapes.texture_view, &normals.texture_view, &surroundings.texture_view, binding)),
    );
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("lens drops"),
        color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(render_pipeline);
    pass.set_bind_group(0, &bind, &[index.index()]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up burst: four drops 0.25 m ahead, spread over a screen-wide
    /// volume turned upright, drawn towards the eye and fading out.
    fn emitter() -> Emitter {
        let xml = r"<PartSys Name='test' disLibItemId='1' Priority='1'><PartEmit Name='drops' Active='1' FrameCount='2' FrameCountY='2'
            PosOffset='0,0.25,0' RotOffset='0,90,90' FramePerSecond='0' FramePerSecondVariant='0' EmitDuration='0.01' SpawnDelay='0'
            NbInitPart='4' EmitVolHalfSizeX='0.05' EmitVolHalfSizeY='0.1' InitGravityDir='0,-0.4,0' FrameRandomStart='1'
            RandomMirrorU='1' DiffuseTexture='fx\d.png' DistortionStrength='0.03'>
            <InitLifeTimeKF><ValueKF Value='1'/></InitLifeTimeKF>
            <InitSizeKF><ValueKF Value='0.02'/></InitSizeKF>
            <InitGravityIntensityKF><ValueKF Value='1'/></InitGravityIntensityKF>
            <PartAlphaKF><Knot Value='0,1'/><Knot Value='0.5,1'/><Knot Value='1,0'/></PartAlphaKF>
            </PartEmit></PartSys>";
        Effect::parse(xml).unwrap().emitters.remove(0)
    }

    #[test]
    fn drops_spread_over_the_view_come_closer_and_fade() {
        let e = emitter();
        let (tan_half, aspect) = (0.4, 1.6);
        let early = drops_at(&e, 0.05, tan_half, aspect);
        let late = drops_at(&e, 0.9, tan_half, aspect);
        assert_eq!((early.len(), late.len()), (4, 4));
        for (a, b) in early.iter().zip(&late) {
            // On the view, in the atlas, the same drop throughout.
            assert!(a.centre.x.abs() <= 1.0 && a.centre.y.abs() <= 1.0 && a.cell.x < 2 && a.cell.y < 2);
            assert_eq!((a.cell, a.mirror), (b.cell, b.mirror));
            // Nearer the eye: bigger and further from the middle.
            assert!(b.half.x > a.half.x && (b.centre - Vec2::splat(0.5)).length() > (a.centre - Vec2::splat(0.5)).length());
            assert!(a.alpha == 1.0 && b.alpha < 0.3);
            // Round drops on a wide screen are narrower in UV across.
            assert!((a.half.y / a.half.x - aspect).abs() < 1e-4);
        }
        // Gone once they've lived their life.
        assert!(drops_at(&e, 1.01, tan_half, aspect).is_empty());
        assert!(lasts(&e) >= 1.0);
    }

    #[test]
    fn the_turned_volume_spans_the_view_sideways_more_than_up() {
        // Unturned, the volume's long side (y) would run away from the eye.
        let e = emitter();
        let drops = drops_at(&e, 0.0, 0.4, 1.0);
        let reach = |f: fn(&Drop) -> f32| drops.iter().map(|d| (f(d) - 0.5).abs()).fold(0.0, f32::max);
        let (across, up) = (reach(|d| d.centre.x), reach(|d| d.centre.y));
        // 0.1 m across and 0.05 m up at 0.25 m: at most 0.5 and 0.25 of the view.
        assert!(across <= 0.5 + 1e-4 && up <= 0.25 + 1e-4 && across > up, "across {across} up {up}");
        let uniform = uniform(&e, &drops);
        assert_eq!((uniform.tint.w, uniform.grid.x, uniform.strengths.x), (4.0, 2.0, 0.03));
    }
}
