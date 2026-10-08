//! The game's bloom, with its colour grade (`grade.rs`): the exposed scene,
//! shrunk to half size, keeps only as much of each pixel's brightness as the
//! world's bloom response curve gives it (`Render/WorldSettings`: nothing
//! below about 1, rising to a cap of about half by 3.6), and shrinks again to
//! a quarter, an eighth and a sixteenth, each step a 13-tap filter. The grade
//! pass adds the four levels together to the scene before its tone curve, as
//! the game's final pass adds its combined bloom (its combine pass decodes,
//! sums and re-encodes the levels, so the final pass adds exactly their sum),
//! scaled by the bloom preset's intensity (`fIntensity`, 1 outdoors).
//!
//! Without the game's grade, Bevy's own bloom stands in. The effects setting
//! asks for bloom with `WantsBloom`; this module picks which.

use bevy::core_pipeline::{Core3d, Core3dSystems, FullscreenShader};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer};
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId, ColorTargetState, ColorWrites, Extent3d, FilterMode,
    FragmentState, Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType,
    SamplerDescriptor, ShaderStages, ShaderType, TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages, TextureView,
    UniformBuffer,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::TextureCache;
use bevy::render::view::ViewTarget;
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

use crate::player::PlayerCamera;

pub struct GameBloomPlugin;

impl Plugin for GameBloomPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "bloom.wgsl");
        app.add_plugins((ExtractComponentPlugin::<GameBloom>::default(), ExtractResourcePlugin::<BloomResponse>::default()))
            .add_systems(Update, (load_response, pick_bloom));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(Render, (prepare_levels, prepare_uniform).in_set(RenderSystems::PrepareResources))
            .add_systems(
                Core3d,
                draw_bloom
                    .in_set(Core3dSystems::PostProcess)
                    .in_set(GameBloomPass)
                    .after(crate::light_shafts::ShaftsPass)
                    .after(bevy::post_process::bloom::bloom)
                    .before(bevy::core_pipeline::tonemapping::tonemapping),
            );
    }
}

/// The game bloom's passes, which the grade pass follows.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct GameBloomPass;

/// Put on the player's camera by the effects setting: bloom wanted.
#[derive(Component)]
pub struct WantsBloom;

/// On the player's camera while the game's bloom is drawn.
#[derive(Component, Clone, Copy, Default, ExtractComponent)]
pub struct GameBloom;

/// Bevy's bloom where the game's grade is off (as the effects setting had).
fn bevy_bloom() -> Bloom {
    Bloom { intensity: 0.08, ..Bloom::NATURAL }
}

/// Gives a camera that wants bloom the game's (with its grade) or Bevy's.
#[allow(clippy::type_complexity)]
fn pick_bloom(
    grade: Option<Res<crate::grade::GameGrade>>,
    response: Option<Res<BloomResponse>>,
    cams: Query<(Entity, Has<WantsBloom>, Has<GameBloom>, Has<Bloom>), With<PlayerCamera>>,
    mut commands: Commands,
) {
    let game = crate::grade::enabled(grade.as_deref()) && response.is_some() && std::env::var("FC3_GAME_BLOOM").as_deref() != Ok("0");
    for (e, wants, has_game, has_bevy) in &cams {
        let (want_game, want_bevy) = (wants && game, wants && !game);
        let mut c = commands.entity(e);
        if want_game != has_game {
            if want_game {
                c.insert(GameBloom)
            } else {
                c.remove::<GameBloom>()
            };
        }
        if want_bevy != has_bevy {
            if want_bevy {
                c.insert(bevy_bloom())
            } else {
                c.remove::<Bloom>()
            };
        }
    }
}

/// Entries in the response table.
const RESPONSE_LEN: usize = 64;

#[derive(Clone, Copy, ShaderType)]
struct BloomUniform {
    /// x: the brightness the table runs to.
    params: Vec4,
    /// The response curve, four entries to an element.
    table: [Vec4; RESPONSE_LEN / 4],
}

/// The world's bloom response, tabulated (from the cached environment).
#[derive(Resource, Clone, ExtractResource)]
struct BloomResponse(BloomUniform);

fn load_response(env: Res<crate::sky::GameEnvironment>, have: Option<Res<BloomResponse>>, mut commands: Commands) {
    if have.is_some() || !env.is_changed() {
        return;
    }
    let Some(curve) = env.0.as_ref().map(|e| &e.bloom_response).filter(|c| !c.0.is_empty()) else { return };
    let top = curve.0.last().map_or(1.0, |k| k.0).max(1.0);
    let entries: Vec<f32> = (0..RESPONSE_LEN).map(|i| curve.eval(top * i as f32 / (RESPONSE_LEN - 1) as f32)).collect();
    let table = std::array::from_fn(|i| Vec4::from_slice(&entries[i * 4..i * 4 + 4]));
    commands.insert_resource(BloomResponse(BloomUniform { params: Vec4::new(top, 0.0, 0.0, 0.0), table }));
}

#[derive(Resource)]
struct BloomPipelines {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    prefilter: CachedRenderPipelineId,
    downsample: CachedRenderPipelineId,
}

/// The four bloom levels of a view (half to a sixteenth of its size).
#[derive(Component)]
pub struct BloomLevels(pub [TextureView; 4]);

#[derive(Resource)]
struct BloomBuffer(UniformBuffer<BloomUniform>);

const FORMAT: TextureFormat = TextureFormat::Rgba16Float;

fn init_pipeline(mut commands: Commands, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>, server: Res<AssetServer>, cache: Res<PipelineCache>) {
    let layout = BindGroupLayoutDescriptor::new(
        "game bloom",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (texture_2d(TextureSampleType::Float { filterable: true }), sampler(SamplerBindingType::Filtering), uniform_buffer::<BloomUniform>(false)),
        ),
    );
    let sampler = device.create_sampler(&SamplerDescriptor { min_filter: FilterMode::Linear, mag_filter: FilterMode::Linear, ..default() });
    let shader: Handle<Shader> = bevy::asset::load_embedded_asset!(server.as_ref(), "bloom.wgsl");
    let pipeline = |entry: &'static str| RenderPipelineDescriptor {
        label: Some(format!("game bloom {entry}").into()),
        layout: vec![layout.clone()],
        vertex: fullscreen.to_vertex_state(),
        fragment: Some(FragmentState {
            shader: shader.clone(),
            entry_point: Some(entry.into()),
            targets: vec![Some(ColorTargetState { format: FORMAT, blend: None, write_mask: ColorWrites::ALL })],
            ..default()
        }),
        ..default()
    };
    let prefilter = cache.queue_render_pipeline(pipeline("prefilter"));
    let downsample = cache.queue_render_pipeline(pipeline("downsample"));
    commands.insert_resource(BloomPipelines { layout, sampler, prefilter, downsample });
}

fn prepare_levels(mut commands: Commands, mut textures: ResMut<TextureCache>, device: Res<RenderDevice>, views: Query<(Entity, &ViewTarget), With<GameBloom>>) {
    for (e, target) in &views {
        let size = target.main_texture().size();
        let levels = std::array::from_fn(|k| {
            let d = 2u32 << k;
            let texture = textures.get(
                &device,
                TextureDescriptor {
                    label: Some("game bloom level"),
                    size: Extent3d { width: (size.width / d).max(1), height: (size.height / d).max(1), depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: TextureDimension::D2,
                    format: FORMAT,
                    usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            );
            texture.default_view.clone()
        });
        commands.entity(e).insert(BloomLevels(levels));
    }
}

fn prepare_uniform(
    mut commands: Commands,
    response: Option<Res<BloomResponse>>,
    buffer: Option<Res<BloomBuffer>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    let (Some(response), None) = (response, buffer) else { return };
    let mut b = UniformBuffer::from(response.0);
    b.set_label(Some("game bloom"));
    b.write_buffer(&device, &queue);
    commands.insert_resource(BloomBuffer(b));
}

fn draw_bloom(
    view: ViewQuery<(&ViewTarget, &BloomLevels), With<GameBloom>>,
    cache: Res<PipelineCache>,
    pipelines: Option<Res<BloomPipelines>>,
    buffer: Option<Res<BloomBuffer>>,
    mut ctx: RenderContext,
) {
    let (target, levels) = view.into_inner();
    let (Some(pipelines), Some(buffer)) = (pipelines, buffer) else { return };
    let (Some(prefilter), Some(downsample), Some(binding)) =
        (cache.get_render_pipeline(pipelines.prefilter), cache.get_render_pipeline(pipelines.downsample), buffer.0.binding())
    else {
        return;
    };
    let layout = cache.get_bind_group_layout(&pipelines.layout);
    let run = |ctx: &mut RenderContext, source: &TextureView, destination: &TextureView, pipeline| {
        let bind_group =
            ctx.render_device().create_bind_group(Some("game bloom"), &layout, &BindGroupEntries::sequential((source, &pipelines.sampler, binding.clone())));
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("game bloom"),
            color_attachments: &[Some(RenderPassColorAttachment { view: destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_render_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    };
    // The scene as it stands (after the depth of field and sun shafts).
    run(&mut ctx, target.main_texture_view(), &levels.0[0], prefilter);
    for k in 1..4 {
        run(&mut ctx, &levels.0[k - 1], &levels.0[k], downsample);
    }
}
