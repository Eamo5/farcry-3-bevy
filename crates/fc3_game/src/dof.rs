//! The world's depth of field: the outdoor environment's preset
//! (`CEnvironmentDepthOfField`) softens what lies far off, from nothing at
//! its focus distance to its blur scale at its far distance, with a blur as
//! wide as its circle of confusion; the sky stays sharp unless the preset says
//! otherwise. Drawn on the HDR scene before bloom and sun shafts, from the
//! depth prepass, so with High effects and up (which have one, for ambient
//! occlusion). `FC3_DOF=0` leaves it off.

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

pub struct DofPlugin;

impl Plugin for DofPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "dof.wgsl");
        app.add_plugins((ExtractComponentPlugin::<DistanceBlur>::default(), UniformComponentPlugin::<DistanceBlur>::default()))
            .add_systems(Update, attach_blur);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<DofPipeline>>()
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(Render, prepare_pipelines.in_set(RenderSystems::Prepare))
            .add_systems(
                Core3d,
                draw_blur.in_set(Core3dSystems::PostProcess).before(crate::light_shafts::ShaftsPass).before(bevy::post_process::bloom::bloom),
            );
    }
}

/// On the player's camera while the preset's depth of field is on.
#[derive(Component, Clone, Copy, Default, ExtractComponent, ShaderType)]
pub struct DistanceBlur {
    /// x: focus distance, y: far distance (m), z: blur scale, w: blur radius
    /// (fraction of the view's height).
    params: Vec4,
    /// x: the camera's near plane, y: 1 to blur the sky too.
    extra: Vec4,
}

/// Puts the preset's depth of field on a camera with a depth prepass, and
/// takes it off one without.
#[allow(clippy::type_complexity)]
fn attach_blur(
    env: Res<crate::sky::GameEnvironment>,
    cams: Query<(Entity, &Projection, Has<DepthPrepass>, Option<&DistanceBlur>), With<PlayerCamera>>,
    mut commands: Commands,
) {
    let preset = env.0.as_ref().and_then(|e| e.dof).filter(|_| std::env::var("FC3_DOF").as_deref() != Ok("0"));
    for (e, projection, depth, blur) in &cams {
        match (preset, depth) {
            (Some(d), true) => {
                let near = match projection {
                    Projection::Perspective(p) => p.near,
                    _ => 0.1,
                };
                let want = DistanceBlur { params: Vec4::new(d[0], d[2].max(d[0] + 1.0), d[3], d[4]), extra: Vec4::new(near, d[5], 0.0, 0.0) };
                if blur.is_none_or(|b| b.params != want.params || b.extra != want.extra) {
                    commands.entity(e).insert(want);
                }
            }
            _ => {
                if blur.is_some() {
                    commands.entity(e).remove::<DistanceBlur>();
                }
            }
        }
    }
}

#[derive(Resource)]
struct DofPipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
    shader: Handle<Shader>,
}

#[derive(Component, Deref)]
struct DofPipelineId(CachedRenderPipelineId);

fn init_pipeline(mut commands: Commands, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>, server: Res<AssetServer>) {
    let layout = BindGroupLayoutDescriptor::new(
        "depth of field",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                texture_depth_2d(),
                uniform_buffer::<DistanceBlur>(true),
            ),
        ),
    );
    let sampler = device.create_sampler(&SamplerDescriptor { min_filter: FilterMode::Linear, mag_filter: FilterMode::Linear, ..default() });
    commands.insert_resource(DofPipeline { layout, sampler, fullscreen: fullscreen.clone(), shader: bevy::asset::load_embedded_asset!(server.as_ref(), "dof.wgsl") });
}

impl SpecializedRenderPipeline for DofPipeline {
    type Key = TextureFormat;

    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("depth of field".into()),
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
    mut pipelines: ResMut<SpecializedRenderPipelines<DofPipeline>>,
    pipeline: Res<DofPipeline>,
    views: Query<(Entity, &ExtractedView), With<DistanceBlur>>,
) {
    for (e, view) in &views {
        let id = pipelines.specialize(&cache, &pipeline, view.target_format);
        commands.entity(e).insert(DofPipelineId(id));
    }
}

fn draw_blur(
    view: ViewQuery<(&ViewTarget, &ViewPrepassTextures, &DofPipelineId, &DistanceBlur, &DynamicUniformIndex<DistanceBlur>)>,
    cache: Res<PipelineCache>,
    pipeline: Res<DofPipeline>,
    uniforms: Res<ComponentUniforms<DistanceBlur>>,
    mut ctx: RenderContext,
) {
    let (target, prepass, id, blur, index) = view.into_inner();
    if blur.params.z <= 0.0 || blur.params.w <= 0.0 {
        return;
    }
    let (Some(render_pipeline), Some(binding), Some(depth)) = (cache.get_render_pipeline(**id), uniforms.uniforms().binding(), prepass.depth_view()) else { return };
    let post = target.post_process_write();
    let bind_group = ctx.render_device().create_bind_group(
        Some("depth of field"),
        &cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((post.source, &pipeline.sampler, depth, binding)),
    );
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("depth of field"),
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
