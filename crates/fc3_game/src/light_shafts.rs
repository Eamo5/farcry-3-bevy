//! Light shafts (the game's sun shafts): a full-screen pass after the main
//! pass that streaks the bright sky showing between leaves, branches and roofs
//! out from the sun (see `light_shafts.wgsl`). On with High effects and up,
//! while the sun is up, in front of the camera and not clouded over.

use bevy::core_pipeline::{Core3d, Core3dSystems, FullscreenShader};
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer};
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId, ColorTargetState, ColorWrites, FilterMode, FragmentState,
    Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor,
    ShaderStages, ShaderType, SpecializedRenderPipeline, SpecializedRenderPipelines, TextureFormat, TextureSampleType,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::uniform::{ComponentUniforms, DynamicUniformIndex, UniformComponentPlugin};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};

use crate::player::PlayerCamera;

pub struct LightShaftsPlugin;

impl Plugin for LightShaftsPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "light_shafts.wgsl");
        app.add_plugins((ExtractComponentPlugin::<LightShafts>::default(), UniformComponentPlugin::<LightShafts>::default()))
            .add_systems(PostUpdate, aim_shafts.after(TransformSystems::Propagate));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<ShaftsPipeline>>()
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(Render, prepare_pipelines.in_set(RenderSystems::Prepare))
            .add_systems(Core3d, draw_shafts.in_set(Core3dSystems::PostProcess).in_set(ShaftsPass).before(bevy::post_process::bloom::bloom));
    }
}

/// The shafts' pass, for ordering other post effects round it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShaftsPass;

/// On the player's camera while shafts are enabled (see `settings`); the
/// fields are set each frame.
#[derive(Component, Clone, Copy, Default, ExtractComponent, ShaderType)]
pub struct LightShafts {
    /// xy: the sun on screen (uv), z: strength, w: brightness threshold.
    sun: Vec4,
    /// rgb: the sunlight's colour, w: the view's aspect ratio.
    colour: Vec4,
}

/// Overall strength, and the brightness (after exposure) the light must pass
/// to throw rays: a little under the bright sky round the sun, so the sky
/// between leaves throws them as well as the sun's disc.
/// `FC3_SHAFTS=strength,threshold` overrides them (`FC3_SHAFTS=0` turns them
/// off), and `FC3_SHAFTS_DEBUG` logs the sun's place and the strength.
const STRENGTH: f32 = 2.0;
const THRESHOLD: f32 = 0.2;

fn tuning() -> (f32, f32) {
    let v: Vec<f32> = std::env::var("FC3_SHAFTS").unwrap_or_default().split(',').filter_map(|s| s.trim().parse().ok()).collect();
    (v.first().copied().unwrap_or(STRENGTH), v.get(1).copied().unwrap_or(THRESHOLD))
}

/// Puts the sun on screen and sets the strength: full with the sun ahead,
/// fading as it leaves the view, by daylight and through cloud.
fn aim_shafts(
    tod: Res<crate::sky::TimeOfDay>,
    env: Res<crate::sky::GameEnvironment>,
    weather: Res<crate::clouds::Weather>,
    heights: Res<crate::terrain::Heightfield>,
    suns: Query<&DirectionalLight, With<crate::sky::Sun>>,
    mut cams: Query<(&GlobalTransform, &Projection, &mut LightShafts), With<PlayerCamera>>,
    mut tune: Local<Option<(f32, f32)>>,
    time: Res<Time>,
    mut logged: Local<f32>,
) {
    let Ok((cam, projection, mut shafts)) = cams.single_mut() else { return };
    let Projection::Perspective(p) = projection else { return };
    let (strength, threshold) = *tune.get_or_insert_with(tuning);
    let sun = tod.sun_dir();
    let view = cam.rotation().inverse() * sun;
    let (tan_y, aspect) = ((p.fov * 0.5).tan(), p.aspect_ratio);
    let ahead = -view.z;
    let ndc = Vec2::new(view.x / (ahead.max(0.05) * tan_y * aspect), view.y / (ahead.max(0.05) * tan_y));
    // Full while the sun is near the view's centre, fading off to the side
    // (the world's god-ray preset: 40 to 90 degrees).
    let (from, to) = env.0.as_ref().map_or((40.0, 90.0), |e| e.shafts_fade);
    let off = ahead.clamp(-1.0, 1.0).acos().to_degrees();
    let facing = (1.0 - smoothstep(from, to, off)) * (1.0 - smoothstep(1.0, 1.8, ndc.abs().max_element()));
    let eye = cam.translation();
    let under = crate::water::submerged(eye, &heights, 0.0);
    let k = if under { 0.0 } else { strength * facing * tod.daylight().clamp(0.0, 1.0) * (1.0 - weather.overcast()) };
    let colour = suns.iter().next().map_or(LinearRgba::WHITE, |l| l.color.to_linear());
    shafts.sun = Vec4::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5, k, threshold);
    shafts.colour = Vec4::new(colour.red, colour.green, colour.blue, aspect);
    if std::env::var("FC3_SHAFTS_DEBUG").is_ok() && time.elapsed_secs() > *logged + 2.0 {
        *logged = time.elapsed_secs();
        info!("shafts: sun {:.2?} strength {k:.2} (facing {facing:.2}, overcast {:.2})", shafts.sun.truncate().truncate(), weather.overcast());
    }
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[derive(Resource)]
struct ShaftsPipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
    shader: Handle<Shader>,
}

#[derive(Component, Deref)]
struct ShaftsPipelineId(CachedRenderPipelineId);

fn init_pipeline(mut commands: Commands, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>, server: Res<AssetServer>) {
    let layout = BindGroupLayoutDescriptor::new(
        "light shafts",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (texture_2d(TextureSampleType::Float { filterable: true }), sampler(SamplerBindingType::Filtering), uniform_buffer::<LightShafts>(true)),
        ),
    );
    let sampler = device.create_sampler(&SamplerDescriptor { min_filter: FilterMode::Linear, mag_filter: FilterMode::Linear, ..default() });
    commands.insert_resource(ShaftsPipeline {
        layout,
        sampler,
        fullscreen: fullscreen.clone(),
        shader: bevy::asset::load_embedded_asset!(server.as_ref(), "light_shafts.wgsl"),
    });
}

impl SpecializedRenderPipeline for ShaftsPipeline {
    type Key = TextureFormat;

    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("light shafts".into()),
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

fn prepare_pipelines(
    mut commands: Commands,
    cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<ShaftsPipeline>>,
    pipeline: Res<ShaftsPipeline>,
    views: Query<(Entity, &ExtractedView), With<LightShafts>>,
) {
    for (e, view) in &views {
        let id = pipelines.specialize(&cache, &pipeline, view.target_format);
        commands.entity(e).insert(ShaftsPipelineId(id));
    }
}

fn draw_shafts(
    view: ViewQuery<(&ViewTarget, &ShaftsPipelineId, &LightShafts, &DynamicUniformIndex<LightShafts>)>,
    cache: Res<PipelineCache>,
    pipeline: Res<ShaftsPipeline>,
    uniforms: Res<ComponentUniforms<LightShafts>>,
    mut ctx: RenderContext,
) {
    let (target, id, shafts, index) = view.into_inner();
    if shafts.sun.z <= 0.001 {
        return;
    }
    let (Some(render_pipeline), Some(binding)) = (cache.get_render_pipeline(**id), uniforms.uniforms().binding()) else { return };
    let post = target.post_process_write();
    let bind_group = ctx.render_device().create_bind_group(
        Some("light shafts"),
        &cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((post.source, &pipeline.sampler, binding)),
    );
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("light shafts"),
        color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(render_pipeline);
    pass.set_bind_group(0, &bind_group, &[index.index()]);
    pass.draw(0..3, 0..1);
}
