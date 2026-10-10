//! Authored masked radial blur. Opt-in sequence fixture while the native
//! kernel/strength calibration and gameplay effect lifecycle are verified.

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
use fc3_formats::cinema::{ClipKind, Sequence, FOV_STEP};
use crate::player::PlayerCamera;

pub struct ScreenBlurPlugin;
impl Plugin for ScreenBlurPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "screen_blur.wgsl");
        app.add_plugins((ExtractComponentPlugin::<ScreenBlur>::default(), UniformComponentPlugin::<ScreenBlur>::default(),
            ExtractComponentPlugin::<BlurMask>::default()))
            .add_systems(Startup, load_fixture)
            .add_systems(Update, fixture.run_if(in_state(crate::menu::GameState::Playing)));
        let Some(render) = app.get_sub_app_mut(RenderApp) else { return };
        render.init_gpu_resource::<SpecializedRenderPipelines<BlurPipeline>>()
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(Render, prepare.in_set(RenderSystems::Prepare))
            .add_systems(Core3d, draw.in_set(Core3dSystems::PostProcess)
                .after(bevy::core_pipeline::tonemapping::tonemapping));
    }
}

#[derive(Component, Clone, Copy, ExtractComponent, ShaderType)]
pub struct ScreenBlur {
    /// Authored factor, texture alpha, mask offset, reserved.
    pub params: Vec4,
    pub colour: Vec4,
    pub direction: Vec4,
}

#[derive(Component, Clone, ExtractComponent)]
pub struct BlurMask(pub Handle<Image>);

#[derive(Resource)]
struct Fixture { sequence: Sequence, mask: Handle<Image>, elapsed: f32, fixed: Option<f32> }

fn load_fixture(mut commands: Commands, server: Res<AssetServer>) {
    let Ok(name) = std::env::var("FC3_POSTFX_TEST") else { return };
    let load = (|| -> anyhow::Result<Fixture> {
        use anyhow::{Context, ensure};
        let xml = std::fs::read_to_string(fc3_formats::cache::default_cache_root().join(fc3_formats::cinema::POSTFX_CACHE_REL))?;
        let sequence = fc3_formats::cinema::parse_postfx_library(&xml)?.into_iter()
            .find(|s| s.name.eq_ignore_ascii_case(&name)).context("missing post-effect sequence")?;
        let texture = sequence.groups.iter().flat_map(|g| &g.clips).find_map(|c| match &c.kind {
            ClipKind::Blur { texture, kind: 1, .. } if !texture.is_empty() => Some(texture), _ => None,
        }).context("fixture requires an authored masked radial-blur clip")?;
        ensure!(sequence.groups.iter().flat_map(|g| &g.clips).all(|c| match &c.kind {
            ClipKind::Blur { texture: other, kind: 1, .. } => other == texture,
            _ => true,
        }), "fixture cannot combine different blur masks");
        let mask = server.load_with_settings(crate::cache_assets::cache_path(&fc3_formats::cache::texture_rel(texture)),
            |settings: &mut bevy::image::ImageLoaderSettings| { settings.is_srgb = false; });
        let fixed = std::env::var("FC3_POSTFX_TEST_TIME").ok().and_then(|s| s.parse::<f32>().ok())
            .filter(|t| t.is_finite() && *t >= 0.0);
        Ok(Fixture { sequence, mask, elapsed: 0.0, fixed })
    })();
    match load {
        Ok(fixture) => { info!("postfx fixture: {} at {:?}", fixture.sequence.name, fixture.fixed); commands.insert_resource(fixture); }
        Err(e) => warn!("postfx fixture: {e:#}"),
    }
}

fn sample(sequence: &Sequence, time: f32) -> Option<ScreenBlur> {
    if !time.is_finite() || time < 0.0 || sequence.duration <= 0.0 { return None; }
    let time = if sequence.looping { time % sequence.duration } else { time };
    if time > sequence.duration { return None; }
    sequence.groups.iter().flat_map(|g| &g.clips).filter_map(|c| {
        let ClipKind::Blur { kind: 1, samples, priority, .. } = &c.kind else { return None };
        if time < c.start || time > c.start + c.duration { return None; }
        let v = samples.get(((time - c.start) / FOV_STEP) as usize)?;
        Some((*priority, ScreenBlur { params: Vec4::new(v[0], v[7], v[8], 0.0),
            colour: Vec4::from_slice(&v[1..5]), direction: Vec4::new(v[5], v[6], 0.0, 0.0) }))
    }).max_by_key(|(priority, _)| *priority).map(|(_, blur)| blur)
}

fn fixture(time: Res<Time>, fixture: Option<ResMut<Fixture>>, cameras: Query<Entity, With<PlayerCamera>>, mut commands: Commands) {
    let Some(mut fixture) = fixture else { return };
    fixture.elapsed += time.delta_secs();
    let blur = sample(&fixture.sequence, fixture.fixed.unwrap_or(fixture.elapsed));
    for entity in &cameras {
        match blur {
            Some(blur) => { commands.entity(entity).insert((blur, BlurMask(fixture.mask.clone()))); }
            None => { commands.entity(entity).remove::<(ScreenBlur, BlurMask)>(); }
        }
    }
}

#[derive(Resource)]
struct BlurPipeline { layout: BindGroupLayoutDescriptor, sampler: Sampler, fullscreen: FullscreenShader, shader: Handle<Shader> }
#[derive(Component, Deref)]
struct PipelineId(CachedRenderPipelineId);

fn init_pipeline(mut commands: Commands, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>, server: Res<AssetServer>) {
    commands.insert_resource(BlurPipeline {
        layout: BindGroupLayoutDescriptor::new("masked radial blur", &BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, (
            texture_2d(TextureSampleType::Float { filterable: true }), sampler(SamplerBindingType::Filtering),
            texture_2d(TextureSampleType::Float { filterable: true }), uniform_buffer::<ScreenBlur>(true),
        ))),
        sampler: device.create_sampler(&SamplerDescriptor { min_filter: FilterMode::Linear, mag_filter: FilterMode::Linear, ..default() }),
        fullscreen: fullscreen.clone(), shader: bevy::asset::load_embedded_asset!(server.as_ref(), "screen_blur.wgsl"),
    });
}
impl SpecializedRenderPipeline for BlurPipeline {
    type Key = TextureFormat;
    fn specialize(&self, format: TextureFormat) -> RenderPipelineDescriptor {
        RenderPipelineDescriptor { label: Some("masked radial blur".into()), layout: vec![self.layout.clone()],
            vertex: self.fullscreen.to_vertex_state(), fragment: Some(FragmentState { shader: self.shader.clone(),
                targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })], ..default() }), ..default() }
    }
}
fn prepare(mut commands: Commands, cache: Res<PipelineCache>, mut pipelines: ResMut<SpecializedRenderPipelines<BlurPipeline>>,
    pipeline: Res<BlurPipeline>, views: Query<(Entity, &ExtractedView), With<ScreenBlur>>) {
    for (entity, view) in &views { commands.entity(entity).insert(PipelineId(pipelines.specialize(&cache, &pipeline, view.target_format))); }
}
fn draw(view: ViewQuery<(&ViewTarget, &PipelineId, &ScreenBlur, &BlurMask, &DynamicUniformIndex<ScreenBlur>)>,
    cache: Res<PipelineCache>, pipeline: Res<BlurPipeline>, uniforms: Res<ComponentUniforms<ScreenBlur>>,
    images: Res<RenderAssets<GpuImage>>, mut ctx: RenderContext) {
    let (target, id, blur, mask, index) = view.into_inner();
    if blur.params.x <= 0.0 || blur.params.y <= 0.0 { return; }
    let (Some(render_pipeline), Some(binding), Some(mask)) =
        (cache.get_render_pipeline(**id), uniforms.uniforms().binding(), images.get(&mask.0)) else { return };
    let post = target.post_process_write();
    let bind = ctx.render_device().create_bind_group(Some("masked radial blur"), &cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((post.source, &pipeline.sampler, &mask.texture_view, binding)));
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor { label: Some("masked radial blur"),
        color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
    pass.set_render_pipeline(render_pipeline);
    pass.set_bind_group(0, &bind, &[index.index()]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires FC3_TEST_CACHE with the authored post-effect library"]
    fn installed_blur_fixture_samples_fades_loops_and_removes_expired_components() {
        let root = std::path::PathBuf::from(std::env::var_os("FC3_TEST_CACHE").expect("FC3_TEST_CACHE"));
        let sequences = fc3_formats::cinema::parse_postfx_library(&std::fs::read_to_string(
            root.join(fc3_formats::cinema::POSTFX_CACHE_REL)).unwrap()).unwrap();
        let get = |suffix: &str| sequences.iter().find(|s| s.name == format!("postfx:mush_postfx.transition{suffix}")).unwrap();
        let intro = get("_IN");
        let outro = get("_OUT");
        let near_start = sample(intro, 0.001).unwrap();
        let near_end = sample(intro, 3.0).unwrap();
        assert!(near_start.params.y < 0.001 && near_start.params.z < 0.001);
        assert!(near_end.params.y > 0.99 && near_end.params.z > 0.99);
        assert!(sample(outro, 3.0).unwrap().params.y < 0.01);
        assert!(sample(outro, 3.1).is_none());
        assert!(sample(outro, f32::NAN).is_none());
        assert!(sample(outro, -1.0).is_none());
        assert_eq!(sample(get(""), 0.5).unwrap().params, sample(get(""), 4.5).unwrap().params);

        let mut app = App::new();
        app.init_resource::<Time>().add_systems(Update, fixture).insert_resource(Fixture {
            sequence: outro.clone(), mask: Handle::default(), elapsed: 0.0, fixed: Some(0.1),
        });
        let camera = app.world_mut().spawn(PlayerCamera).id();
        app.update();
        assert!(app.world().get::<ScreenBlur>(camera).is_some());
        assert!(app.world().get::<BlurMask>(camera).is_some());
        app.world_mut().resource_mut::<Fixture>().fixed = Some(3.1);
        app.update();
        assert!(app.world().get::<ScreenBlur>(camera).is_none());
        assert!(app.world().get::<BlurMask>(camera).is_none());
    }
}
