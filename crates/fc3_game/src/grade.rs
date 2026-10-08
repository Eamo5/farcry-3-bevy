//! The game's own tone mapping and colour grade, in place of Bevy's: each
//! exposed HDR channel goes through the game's tone curve and the result is
//! looked up in the world's colour cube, as the game's final pass does (both
//! read off the installed game by the converter; see `fc3_formats::grade`).
//! The cube gives display values; the pass writes them out linear, so the
//! window's sRGB encoding shows them as the game would.
//!
//! Without the cache's grade (an older cache) the camera keeps Bevy's AgX.
//! FC3_GRADE=0 keeps AgX too, for comparison.

use bevy::asset::RenderAssetUsages;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::core_pipeline::{Core3d, Core3dSystems, FullscreenShader};
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{sampler, texture_2d, texture_3d, uniform_buffer};
use bevy::render::render_resource::{
    AddressMode, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId, ColorTargetState, ColorWrites, Extent3d,
    FilterMode, FragmentState, Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler,
    SamplerBindingType, SamplerDescriptor, ShaderStages, ShaderType, SpecializedRenderPipeline, SpecializedRenderPipelines, TextureDimension, TextureFormat,
    TextureSampleType, UniformBuffer,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::GpuImage;
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use fc3_formats::grade::{Grade, CURVE_LEN};

use crate::player::PlayerCamera;

pub struct GradePlugin;

impl Plugin for GradePlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "grade.wgsl");
        app.add_plugins((ExtractComponentPlugin::<GameGraded>::default(), ExtractResourcePlugin::<GameGrade>::default()))
            .add_systems(Startup, load_grade)
            .add_systems(Update, attach_grade);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<GradePipeline>>()
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(Render, (prepare_pipelines, prepare_uniform).in_set(RenderSystems::Prepare))
            .add_systems(
                Core3d,
                draw_grade
                    .in_set(Core3dSystems::PostProcess)
                    .after(bevy::post_process::bloom::bloom)
                    .after(crate::bloom::GameBloomPass)
                    .before(bevy::core_pipeline::tonemapping::tonemapping),
            );
    }
}

/// The grade from the cache: the tone curve and the colour cube.
#[derive(Resource, Clone, ExtractResource)]
pub struct GameGrade {
    pub cube: Handle<Image>,
    uniform: GradeUniform,
    /// For the CPU side (the sky's colours).
    pub grade: std::sync::Arc<Grade>,
}

/// On the player's camera while the game's grade replaces Bevy's tone mapping.
#[derive(Component, Clone, Copy, Default, ExtractComponent)]
pub struct GameGraded;

#[derive(Clone, Copy, ShaderType)]
struct GradeUniform {
    /// x: the curve's top input, y: the cube's edge, z: the bloom's strength.
    params: Vec4,
    /// The curve's entries, four to an element.
    curve: [Vec4; CURVE_LEN / 4],
}

fn load_grade(mut commands: Commands, mut images: ResMut<Assets<Image>>, env: Res<crate::sky::GameEnvironment>) {
    let path = fc3_formats::cache::default_cache_root().join(fc3_formats::cache::GRADE_REL);
    let grade = match std::fs::read(&path).map_err(anyhow::Error::from).and_then(|b| Grade::from_bytes(&b)) {
        Ok(g) => g,
        Err(e) => {
            warn!("no colour grade ({}: {e:#}); keeping Bevy's tone mapping", path.display());
            return;
        }
    };
    let n = grade.size;
    let cube = Image::new(
        Extent3d { width: n, height: n, depth_or_array_layers: n },
        TextureDimension::D3,
        grade.cube.iter().flatten().copied().collect(),
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    let curve = std::array::from_fn(|i| Vec4::from_slice(&grade.curve[i * 4..i * 4 + 4]));
    // The final pass scales the summed bloom by the bloom preset's intensity.
    let bloom = env.0.as_ref().map_or(1.0, |e| e.bloom_intensity);
    let uniform = GradeUniform { params: Vec4::new(grade.x_max, n as f32, bloom, 0.0), curve };
    info!("colour grade: curve to {:.2}, {n}³ cube", grade.x_max);
    commands.insert_resource(GameGrade { cube: images.add(cube), uniform, grade: std::sync::Arc::new(grade) });
}

/// Whether the game's grade is in use (FC3_GRADE=0 keeps Bevy's AgX).
pub fn enabled(grade: Option<&GameGrade>) -> bool {
    grade.is_some() && std::env::var("FC3_GRADE").as_deref() != Ok("0")
}

fn attach_grade(grade: Option<Res<GameGrade>>, cams: Query<Entity, (With<PlayerCamera>, Without<GameGraded>)>, mut commands: Commands) {
    if !enabled(grade.as_deref()) {
        return;
    }
    for e in &cams {
        commands.entity(e).insert((GameGraded, Tonemapping::None));
    }
}

#[derive(Resource)]
struct GradePipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
    shader: Handle<Shader>,
}

#[derive(Component, Deref)]
struct GradePipelineId(CachedRenderPipelineId);

/// The uniform on the GPU, written once.
#[derive(Resource)]
struct GradeBuffer(UniformBuffer<GradeUniform>);

fn init_pipeline(mut commands: Commands, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>, server: Res<AssetServer>) {
    let layout = BindGroupLayoutDescriptor::new(
        "colour grade",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                texture_3d(TextureSampleType::Float { filterable: true }),
                uniform_buffer::<GradeUniform>(false),
                // The game bloom's four levels (`bloom.rs`; black without it).
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
            ),
        ),
    );
    let sampler = device.create_sampler(&SamplerDescriptor {
        min_filter: FilterMode::Linear,
        mag_filter: FilterMode::Linear,
        address_mode_u: AddressMode::ClampToEdge,
        address_mode_v: AddressMode::ClampToEdge,
        address_mode_w: AddressMode::ClampToEdge,
        ..default()
    });
    commands.insert_resource(GradePipeline {
        layout,
        sampler,
        fullscreen: fullscreen.clone(),
        shader: bevy::asset::load_embedded_asset!(server.as_ref(), "grade.wgsl"),
    });
}

impl SpecializedRenderPipeline for GradePipeline {
    type Key = TextureFormat;

    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some("colour grade".into()),
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
    mut pipelines: ResMut<SpecializedRenderPipelines<GradePipeline>>,
    pipeline: Res<GradePipeline>,
    views: Query<(Entity, &ExtractedView), With<GameGraded>>,
) {
    for (e, view) in &views {
        let id = pipelines.specialize(&cache, &pipeline, view.target_format);
        commands.entity(e).insert(GradePipelineId(id));
    }
}

fn prepare_uniform(
    mut commands: Commands,
    grade: Option<Res<GameGrade>>,
    buffer: Option<Res<GradeBuffer>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    let (Some(grade), None) = (grade, buffer) else { return };
    let mut b = UniformBuffer::from(grade.uniform);
    b.set_label(Some("colour grade"));
    b.write_buffer(&device, &queue);
    commands.insert_resource(GradeBuffer(b));
}

#[allow(clippy::too_many_arguments)]
fn draw_grade(
    view: ViewQuery<(&ViewTarget, &GradePipelineId, Option<&crate::bloom::BloomLevels>), With<GameGraded>>,
    black: Res<bevy::render::texture::FallbackImageZero>,
    cache: Res<PipelineCache>,
    pipeline: Res<GradePipeline>,
    grade: Option<Res<GameGrade>>,
    buffer: Option<Res<GradeBuffer>>,
    images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
) {
    let (target, id, bloom) = view.into_inner();
    let (Some(grade), Some(buffer)) = (grade, buffer) else { return };
    let (Some(render_pipeline), Some(cube), Some(binding)) = (cache.get_render_pipeline(**id), images.get(&grade.cube), buffer.0.binding()) else { return };
    let post = target.post_process_write();
    let bind_group = ctx.render_device().create_bind_group(
        Some("colour grade"),
        &cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((
            post.source,
            &pipeline.sampler,
            &cube.texture_view,
            binding,
            bloom.map_or(&black.texture_view, |b| &b.0[0]),
            bloom.map_or(&black.texture_view, |b| &b.0[1]),
            bloom.map_or(&black.texture_view, |b| &b.0[2]),
            bloom.map_or(&black.texture_view, |b| &b.0[3]),
        )),
    );
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("colour grade"),
        color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(render_pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.draw(0..3, 0..1);
}
