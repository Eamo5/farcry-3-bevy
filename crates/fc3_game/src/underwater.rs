//! Sunlight under the water: with the camera below the surface by day, the
//! ripples' focused light (the caustics the surface shows on the bed from
//! above) plays over whatever lies under it, and shafts of light slant down
//! from the surface toward the sun (see `underwater.wgsl`). A full-screen
//! pass on the HDR scene before bloom, reading the depth prepass, so with
//! High effects and up. `FC3_UNDERWATER=<caustics>,<shafts>` sets their
//! gains (`0,0` leaves both off).

use bevy::core_pipeline::prepass::{DepthPrepass, ViewPrepassTextures};
use bevy::core_pipeline::{Core3d, Core3dSystems, FullscreenShader};
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, texture_depth_2d, uniform_buffer};
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

pub struct UnderwaterPlugin;

impl Plugin for UnderwaterPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "underwater.wgsl");
        app.add_plugins((ExtractComponentPlugin::<UnderwaterLight>::default(), UniformComponentPlugin::<UnderwaterLight>::default()))
            .add_systems(PostUpdate, update_light.after(TransformSystems::Propagate));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<UnderwaterPipeline>>()
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(Render, prepare_pipelines.in_set(RenderSystems::Prepare))
            .add_systems(
                Core3d,
                draw_light.in_set(Core3dSystems::PostProcess).before(crate::light_shafts::ShaftsPass).before(bevy::post_process::bloom::bloom),
            );
    }
}

/// On the player's camera while it has a depth prepass; the fields are set
/// each frame (strength 0 above the water or by night).
#[derive(Component, Clone, Copy, Default, ExtractComponent, ShaderType)]
pub struct UnderwaterLight {
    world_from_clip: Mat4,
    /// xyz: the eye, w: the surface's height.
    eye: Vec4,
    /// xyz: toward the sun, w: strength.
    sun: Vec4,
    /// rgb: the sunlight's colour, w: time (s).
    colour: Vec4,
    /// x: the fog's density, y: the caustics' gain, z: the shafts' gain,
    /// w: 1 with the eye under the water.
    fog: Vec4,
    /// rgb: the water's fog colour (linear).
    tint: Vec4,
}

/// The caustics' and the shafts' gains.
const CAUSTICS: f32 = 2.0;
const SHAFTS: f32 = 0.03;

fn gains() -> (f32, f32) {
    let v: Vec<f32> = std::env::var("FC3_UNDERWATER").unwrap_or_default().split(',').filter_map(|s| s.trim().parse().ok()).collect();
    (v.first().copied().unwrap_or(CAUSTICS), v.get(1).copied().unwrap_or(SHAFTS))
}

#[allow(clippy::type_complexity)]
fn update_light(
    heights: Res<crate::terrain::Heightfield>,
    tod: Res<crate::sky::TimeOfDay>,
    (weather, daylight): (Res<crate::clouds::Weather>, Res<crate::clouds::Daylight>),
    time: Res<Time>,
    suns: Query<&DirectionalLight, With<crate::sky::Sun>>,
    mut cams: Query<(Entity, &GlobalTransform, &Projection, Has<DepthPrepass>, Option<&mut UnderwaterLight>), With<PlayerCamera>>,
    mut commands: Commands,
    mut tune: Local<Option<(f32, f32)>>,
) {
    let (caustics, shafts) = *tune.get_or_insert_with(gains);
    for (e, cam, projection, depth, light) in &mut cams {
        let Some(mut light) = light else {
            if depth && caustics + shafts > 0.0 {
                commands.entity(e).insert(UnderwaterLight::default());
            }
            continue;
        };
        if !depth {
            commands.entity(e).remove::<UnderwaterLight>();
            continue;
        }
        let eye = cam.translation();
        let level = crate::water::surface_at(eye, &heights);
        let under = crate::water::submerged(eye, &heights, 0.05);
        let sun = tod.sun_dir();
        let strength = if under && sun.y > 0.0 { tod.daylight().clamp(0.0, 1.0) * (1.0 - 0.7 * weather.overcast()) } else { 0.0 };
        // (Murkier in rivers and lakes, as the fog is.)
        let (density, fog) = crate::water::underwater_fog(level > crate::water::ocean_level(), daylight.0);
        light.tint = fog.to_linear().to_vec4();
        let colour = suns.iter().next().map_or(LinearRgba::WHITE, |l| l.color.to_linear());
        light.world_from_clip = cam.to_matrix() * projection.get_clip_from_view().inverse();
        light.eye = eye.extend(level);
        light.sun = sun.extend(strength);
        light.colour = Vec4::new(colour.red, colour.green, colour.blue, time.elapsed_secs_wrapped());
        light.fog = Vec4::new(density, caustics, shafts, if under { 1.0 } else { 0.0 });
    }
}

#[derive(Resource)]
struct UnderwaterPipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
    shader: Handle<Shader>,
}

#[derive(Component, Deref)]
struct UnderwaterPipelineId(CachedRenderPipelineId);

fn init_pipeline(mut commands: Commands, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>, server: Res<AssetServer>) {
    let layout = BindGroupLayoutDescriptor::new(
        "underwater light",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                texture_depth_2d(),
                uniform_buffer::<UnderwaterLight>(true),
            ),
        ),
    );
    let sampler = device.create_sampler(&SamplerDescriptor { min_filter: FilterMode::Linear, mag_filter: FilterMode::Linear, ..default() });
    commands.insert_resource(UnderwaterPipeline {
        layout,
        sampler,
        fullscreen: fullscreen.clone(),
        shader: bevy::asset::load_embedded_asset!(server.as_ref(), "underwater.wgsl"),
    });
}

impl SpecializedRenderPipeline for UnderwaterPipeline {
    type Key = TextureFormat;

    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("underwater light".into()),
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
    mut pipelines: ResMut<SpecializedRenderPipelines<UnderwaterPipeline>>,
    pipeline: Res<UnderwaterPipeline>,
    views: Query<(Entity, &ExtractedView), With<UnderwaterLight>>,
) {
    for (e, view) in &views {
        let id = pipelines.specialize(&cache, &pipeline, view.target_format);
        commands.entity(e).insert(UnderwaterPipelineId(id));
    }
}

fn draw_light(
    view: ViewQuery<(&ViewTarget, &ViewPrepassTextures, &UnderwaterPipelineId, &UnderwaterLight, &DynamicUniformIndex<UnderwaterLight>)>,
    cache: Res<PipelineCache>,
    pipeline: Res<UnderwaterPipeline>,
    uniforms: Res<ComponentUniforms<UnderwaterLight>>,
    mut ctx: RenderContext,
) {
    let (target, prepass, id, light, index) = view.into_inner();
    // (Nothing to do above the water.)
    if light.fog.w < 0.5 {
        return;
    }
    let (Some(render_pipeline), Some(binding), Some(depth)) = (cache.get_render_pipeline(**id), uniforms.uniforms().binding(), prepass.depth_view()) else {
        return;
    };
    let post = target.post_process_write();
    let bind_group = ctx.render_device().create_bind_group(
        Some("underwater light"),
        &cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((post.source, &pipeline.sampler, depth, binding)),
    );
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("underwater light"),
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
