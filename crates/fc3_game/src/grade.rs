//! The game's own tone mapping and colour grade, in place of Bevy's: each
//! exposed HDR channel goes through the game's tone curve and the result is
//! looked up in the world's colour cube, as the game's final pass does (both
//! read off the installed game by the converter; see `fc3_formats::grade`).
//! The cube gives display values; the pass writes them out linear, so the
//! window's sRGB encoding shows them as the game would.
//!
//! Without the cache's grade (an older cache) the camera keeps Bevy's AgX.
//! FC3_GRADE=0 keeps AgX too, for comparison.
//!
//! Before the tone curve the game's eye adapts (`CEnvironmentExposure`): it
//! takes the frame's log-average luminance and eases the exposure toward the
//! one that brings it to the preset's middle grey, kept within the preset's
//! least and most exposure, slower into the dark than into the light. The
//! camera is exposed at the least (`sky::advance_time`); a histogram of the
//! frame (`eye.wgsl`) gives the average, and the grade scales the exposed
//! colour by what the eye has adapted to over that. `FC3_ADAPT=0` holds it at
//! the least, as before; `FC3_EXPOSURE_DEBUG` logs it.

use bevy::asset::RenderAssetUsages;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::core_pipeline::{Core3d, Core3dSystems, FullscreenShader};
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{
    sampler, storage_buffer_read_only_sized, storage_buffer_sized, texture_2d, texture_3d, uniform_buffer,
};
use bevy::render::render_resource::{
    AddressMode, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer, BufferDescriptor, BufferUsages, CachedComputePipelineId,
    CachedRenderPipelineId, ColorTargetState, ColorWrites, ComputePassDescriptor, ComputePipelineDescriptor, Extent3d, FilterMode, FragmentState,
    Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor,
    ShaderStages, ShaderType, SpecializedRenderPipeline, SpecializedRenderPipelines, TextureDimension, TextureFormat, TextureSampleType, UniformBuffer,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::storage::{GpuShaderBuffer, ShaderBuffer};
use bevy::render::texture::GpuImage;
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use fc3_formats::grade::{Grade, CURVE_LEN};

use crate::player::PlayerCamera;

pub struct GradePlugin;

impl Plugin for GradePlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "grade.wgsl");
        bevy::asset::embedded_asset!(app, "eye.wgsl");
        app.add_plugins((ExtractComponentPlugin::<GameGraded>::default(), ExtractResourcePlugin::<GameGrade>::default(), ExtractResourcePlugin::<Eye>::default()))
            .init_resource::<Adapted>()
            .add_systems(Startup, (load_grade, make_eye))
            .add_systems(Update, (attach_grade, follow_bloom, follow_eye));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_gpu_resource::<SpecializedRenderPipelines<GradePipeline>>()
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(Render, (prepare_pipelines, prepare_uniform, prepare_eye).in_set(RenderSystems::Prepare))
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

/// The eye's adaptation: where it stands (on the GPU: the scale on the
/// exposed colour, the frame's average log2 luminance, the scale it eases
/// toward) and this frame's settings for it.
#[derive(Resource, Clone, ExtractResource)]
pub struct Eye {
    state: Handle<ShaderBuffer>,
    uniform: EyeUniform,
}

/// What the eye has adapted to, read back (`FC3_EXPOSURE_DEBUG`): the scale
/// on the camera's exposure, the frame's average log2 luminance (exposed)
/// and the scale it eases toward.
#[derive(Resource, Default, Clone, Copy)]
pub struct Adapted(pub Vec4);

#[derive(Clone, Copy, Default, ShaderType)]
struct EyeUniform {
    /// x: the histogram's lowest log2 luminance, y: its span in stops, z:
    /// log2 of the middle grey, w: the frame's seconds.
    range: Vec4,
    /// x, y: the least and most shift (stops) from the camera's exposure;
    /// z, w: the speeds to the dark and to the light.
    limits: Vec4,
    /// x: 1 to go straight to the target, y: 0 to hold at 1.
    flags: Vec4,
}

/// The histogram's span (log2 of the exposed luminance): the floor stands
/// for black (the delta the log-average adds), the top for the sun's glare.
const EYE_FLOOR: f32 = -13.0;
const EYE_SPAN: f32 = 18.0;
/// The adaption speeds when the cache doesn't have them (the presets'
/// usual: a 30 fps frame's share of the way, to the dark and to the light).
const ADAPTION: (f32, f32) = (0.025, 0.045);
/// How much brighter the game's eye reads a scene than our exposed colour's
/// mean. Its measure isn't known (its diffuse has no 1/π, its luminance may
/// be taken otherwise); read as ours, a sunny noon sat at a fifth of its
/// middle grey, at the most exposure and washed out. Set so an open sunny
/// scene, black volcanic sand and all, sits at the least exposure, as the
/// presets' narrow ranges suggest a typical one does, day and night; shade
/// (an alley, under the trees) and the dark open it up from there.
const GAME_LUMINANCE: f32 = 8.0;

fn make_eye(mut commands: Commands, mut buffers: ResMut<Assets<ShaderBuffer>>) {
    let mut state = ShaderBuffer::from(Vec4::new(1.0, 0.0, 1.0, 0.0));
    state.buffer_description.label = Some("eye adaptation");
    let state = buffers.add(state);
    if std::env::var_os("FC3_EXPOSURE_DEBUG").is_some() {
        commands.spawn(bevy::render::gpu_readback::Readback::buffer(state.clone())).observe(
            |ev: On<bevy::render::gpu_readback::ReadbackComplete>, mut adapted: ResMut<Adapted>| {
                adapted.0 = ev.to_shader_type();
            },
        );
    }
    commands.insert_resource(Eye { state, uniform: EyeUniform::default() });
}

/// This frame's settings for the eye, from the exposure preset for the hour:
/// its middle grey, and its least and most exposure as shifts from the
/// camera's (which sits at the least, opened by the weather and shelter).
#[allow(clippy::too_many_arguments)]
fn follow_eye(
    time: Res<Time>,
    env: Res<crate::sky::GameEnvironment>,
    tod: Res<crate::sky::TimeOfDay>,
    scene: Res<crate::sky::SceneExposure>,
    (weather, indoors): (Res<crate::clouds::Weather>, Res<crate::sky::Indoors>),
    players: Query<&crate::player::Player>,
    mut eye: ResMut<Eye>,
    adapted: Res<Adapted>,
    mut logged: Local<f32>,
) {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var("FC3_ADAPT").as_deref() != Ok("0"));
    let t = tod.hours / 24.0;
    let e = env.0.as_ref().filter(|e| !e.middle_grey.0.is_empty() && !e.exposure_min.0.is_empty() && scene.game.is_some());
    let uniform = match e.filter(|_| *ON) {
        Some(e) => {
            let least = e.exposure_min.at(t).max(0.05);
            let most = e.exposure_max.at(t).max(least);
            let opened = weather.exposure_shift() + 0.8 * indoors.0;
            let (dark, bright) = if e.adaption.0 > 0.0 && e.adaption.1 > 0.0 { e.adaption } else { ADAPTION };
            let reset = players.single().is_ok_and(|p| p.spawning);
            EyeUniform {
                range: Vec4::new(EYE_FLOOR, EYE_SPAN, (e.middle_grey.at(t).max(1e-6) / GAME_LUMINANCE).log2(), time.delta_secs()),
                limits: Vec4::new(-opened, (most / least).log2() - opened, dark, bright),
                flags: Vec4::new(if reset { 1.0 } else { 0.0 }, 1.0, 0.0, 0.0),
            }
        }
        None => EyeUniform { range: Vec4::new(EYE_FLOOR, EYE_SPAN, 0.0, time.delta_secs()), flags: Vec4::ZERO, ..default() },
    };
    eye.uniform = uniform;
    if adapted.0 != Vec4::ZERO && time.elapsed_secs() - *logged > 2.0 {
        *logged = time.elapsed_secs();
        let a = adapted.0;
        info!(
            "eye: scale {:.2} toward {:.2} (average {:.2} exposed, {:.4} to the game's eye at the camera's exposure), middle grey {:.3}, shifts {:.2}..{:.2}",
            a.x,
            a.z,
            a.y,
            a.y.exp2() * GAME_LUMINANCE / scene.game.map_or(1.0, |g| g.0),
            uniform.range.z.exp2() * GAME_LUMINANCE,
            uniform.limits.x,
            uniform.limits.y
        );
    }
}

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
/// The bloom's strength as the environment has it now (a scene's preset
/// over the world's: DOC_020's trip's, 30).
fn follow_bloom(env: Res<crate::sky::GameEnvironment>, grade: Option<ResMut<GameGrade>>) {
    let Some(mut grade) = grade.filter(|_| env.is_changed()) else { return };
    let bloom = env.0.as_ref().map_or(1.0, |e| e.bloom_intensity);
    if grade.uniform.params.z != bloom {
        grade.uniform.params.z = bloom;
    }
}

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
    /// The eye's passes: the frame's histogram, then its average and the
    /// adaptation; and the histogram's bins.
    eye_layout: BindGroupLayoutDescriptor,
    eye_passes: [CachedComputePipelineId; 2],
    histogram: Buffer,
}

/// The eye's settings on the GPU, written each frame.
#[derive(Resource)]
struct EyeBuffer(UniformBuffer<EyeUniform>);

#[derive(Component, Deref)]
struct GradePipelineId(CachedRenderPipelineId);

/// The uniform on the GPU, written again when the bloom changes.
#[derive(Resource)]
struct GradeBuffer(UniformBuffer<GradeUniform>);

fn init_pipeline(mut commands: Commands, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>, server: Res<AssetServer>, cache: Res<PipelineCache>) {
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
                // What the eye has adapted to.
                storage_buffer_read_only_sized(false, std::num::NonZero::new(16)),
            ),
        ),
    );
    let eye_layout = BindGroupLayoutDescriptor::new(
        "eye adaptation",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::COMPUTE,
            (
                uniform_buffer::<EyeUniform>(false),
                texture_2d(TextureSampleType::Float { filterable: false }),
                storage_buffer_sized(false, std::num::NonZero::new(64 * 4)),
                storage_buffer_sized(false, std::num::NonZero::new(16)),
            ),
        ),
    );
    let eye_shader: Handle<Shader> = bevy::asset::load_embedded_asset!(server.as_ref(), "eye.wgsl");
    let eye_passes = ["histogram_pass", "average_pass"].map(|entry| {
        cache.queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some(format!("eye adaptation {entry}").into()),
            layout: vec![eye_layout.clone()],
            shader: eye_shader.clone(),
            entry_point: Some(entry.into()),
            ..default()
        })
    });
    let histogram = device.create_buffer(&BufferDescriptor { label: Some("eye histogram"), size: 64 * 4, usage: BufferUsages::STORAGE, mapped_at_creation: false });
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
        eye_layout,
        eye_passes,
        histogram,
    });
}

fn prepare_eye(mut commands: Commands, eye: Option<Res<Eye>>, buffer: Option<ResMut<EyeBuffer>>, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    let Some(eye) = eye else { return };
    match buffer {
        Some(mut b) => {
            b.0.set(eye.uniform);
            b.0.write_buffer(&device, &queue);
        }
        None => {
            let mut b = UniformBuffer::from(eye.uniform);
            b.set_label(Some("eye adaptation"));
            b.write_buffer(&device, &queue);
            commands.insert_resource(EyeBuffer(b));
        }
    }
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
    buffer: Option<ResMut<GradeBuffer>>,
    post: Option<Res<crate::post_effects::PostLight>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    let Some(grade) = grade else { return };
    // A screen effect's bloom clip scales the bloom (a blast's glare).
    let mut uniform = grade.uniform;
    uniform.params.z *= post.map_or(1.0, |p| p.bloom.max(0.0));
    match buffer {
        Some(mut b) => {
            if b.0.get().params != uniform.params {
                b.0.set(uniform);
                b.0.write_buffer(&device, &queue);
            }
        }
        None => {
            let mut b = UniformBuffer::from(uniform);
            b.set_label(Some("colour grade"));
            b.write_buffer(&device, &queue);
            commands.insert_resource(GradeBuffer(b));
        }
    }
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
    (eye, eye_buffer, buffers): (Option<Res<Eye>>, Option<Res<EyeBuffer>>, Res<RenderAssets<GpuShaderBuffer>>),
    mut ctx: RenderContext,
) {
    let (target, id, bloom) = view.into_inner();
    let (Some(grade), Some(buffer)) = (grade, buffer) else { return };
    let (Some(render_pipeline), Some(cube), Some(binding)) = (cache.get_render_pipeline(**id), images.get(&grade.cube), buffer.0.binding()) else { return };
    let Some(state) = eye.as_ref().and_then(|e| buffers.get(&e.state)) else { return };
    // The eye first, from the frame as exposed (when its passes are ready:
    // till then it stays where it is, at 1 to begin with).
    if let (Some(histogram_pass), Some(average_pass), Some(settings)) = (
        cache.get_compute_pipeline(pipeline.eye_passes[0]),
        cache.get_compute_pipeline(pipeline.eye_passes[1]),
        eye_buffer.as_ref().and_then(|b| b.0.binding()),
    ) {
        let eye_group = ctx.render_device().create_bind_group(
            Some("eye adaptation"),
            &cache.get_bind_group_layout(&pipeline.eye_layout),
            &BindGroupEntries::sequential((settings, target.main_texture_view(), pipeline.histogram.as_entire_binding(), state.buffer.as_entire_binding())),
        );
        let size = target.main_texture().size();
        let mut pass = ctx.command_encoder().begin_compute_pass(&ComputePassDescriptor { label: Some("eye adaptation"), timestamp_writes: None });
        pass.set_bind_group(0, &eye_group, &[]);
        pass.set_pipeline(histogram_pass);
        pass.dispatch_workgroups(size.width.div_ceil(16), size.height.div_ceil(16), 1);
        pass.set_pipeline(average_pass);
        pass.dispatch_workgroups(1, 1, 1);
    }
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
            state.buffer.as_entire_binding(),
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
