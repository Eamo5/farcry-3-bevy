//! The game's interface movies (converted from `.feu` to standard SWF by
//! `fc3_formats::feu`), run by Ruffle's ActionScript 2 player and drawn by
//! its wgpu renderer.

mod host;

pub use host::{string as text_value, EngineCall, Host, STAGE};
pub use ruffle_core::external::Value as ExternalValue;
/// Keyboard and mouse for the movies (`Host::input`).
pub use ruffle_core::events as input;
pub use ruffle_render_wgpu::wgpu;

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use ruffle_core::limits::ExecutionLimit;
use ruffle_core::{Player, PlayerBuilder};
use ruffle_render_wgpu::backend::{create_wgpu_instance, request_adapter_and_device, WgpuRenderBackend};
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::{RenderTarget, RenderTargetFrame, TextureTarget};

/// A movie playing off screen, for tools and tests.
pub struct OffscreenMovie {
    player: Arc<Mutex<Player>>,
}

/// A GPU of our own for off-screen movies.
pub fn offscreen_descriptors() -> Result<Arc<Descriptors>> {
    let instance = create_wgpu_instance(wgpu::Backends::PRIMARY, wgpu::BackendOptions::default(), None);
    let (adapter, device, queue) =
        futures::executor::block_on(request_adapter_and_device(wgpu::Backends::PRIMARY, &instance, None, wgpu::PowerPreference::HighPerformance))
            .map_err(|e| anyhow!(e.to_string()))?;
    Ok(Arc::new(Descriptors::new(instance, adapter, device, queue)))
}

impl OffscreenMovie {
    /// A movie (a standard SWF) at a size, or its own.
    pub fn open(descriptors: Arc<Descriptors>, swf: &Path, size: Option<(u32, u32)>) -> Result<Self> {
        let movie = ruffle_core::tag_utils::movie_from_path(swf, None).map_err(|e| anyhow!(e.to_string()))?;
        let (w, h) = size.unwrap_or((movie.width().to_pixels().round() as u32, movie.height().to_pixels().round() as u32));
        let target = TextureTarget::new(&descriptors.device, (w, h)).map_err(|e| anyhow!(e.to_string()))?;
        let renderer = WgpuRenderBackend::new(descriptors, target).map_err(|e| anyhow!(e.to_string()))?;
        let player = PlayerBuilder::new().with_renderer(renderer).with_movie(movie).with_viewport_dimensions(w, h, 1.0).build();
        Ok(Self { player })
    }

    /// Plays a frame (the movie's scripts and timeline).
    pub fn run_frame(&self) {
        let mut player = self.player.lock().unwrap();
        player.preload(&mut ExecutionLimit::none());
        player.run_frame();
    }

    /// The frame as drawn now.
    pub fn capture(&self) -> Result<image::RgbaImage> {
        capture(&self.player)
    }
}

/// Ruffle's state for a GPU someone else made (the game's), from its wgpu
/// handles.
pub fn shared_descriptors(instance: wgpu::Instance, adapter: wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Arc<Descriptors> {
    Arc::new(Descriptors::new(instance, adapter, device, queue))
}

/// Draws into a texture made elsewhere: an sRGB one the game shows,
/// through a plain RGBA view of it, so what Ruffle writes (sRGB already) is
/// read back as sRGB. The texture's size is the target's; a new size wants
/// a new texture and target.
#[derive(Debug)]
pub struct SharedTarget {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

#[derive(Debug)]
pub struct SharedFrame(wgpu::TextureView);

impl RenderTargetFrame for SharedFrame {
    fn into_view(self) -> wgpu::TextureView {
        self.0
    }

    fn view(&self) -> &wgpu::TextureView {
        &self.0
    }
}

impl SharedTarget {
    /// The view Ruffle draws through.
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    pub fn new(texture: wgpu::Texture) -> Self {
        let (width, height) = (texture.width(), texture.height());
        Self { texture, width, height }
    }
}

impl RenderTarget for SharedTarget {
    type Frame = SharedFrame;

    fn resize(&mut self, _device: &wgpu::Device, width: u32, height: u32) {
        self.width = width;
        self.height = height;
    }

    fn format(&self) -> wgpu::TextureFormat {
        Self::FORMAT
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn get_next_texture(&mut self) -> Option<SharedFrame> {
        Some(SharedFrame(self.texture.create_view(&wgpu::TextureViewDescriptor { format: Some(Self::FORMAT), ..Default::default() })))
    }

    fn submit<I: IntoIterator<Item = wgpu::CommandBuffer>>(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        command_buffers: I,
        _frame: SharedFrame,
    ) -> wgpu::SubmissionIndex {
        queue.submit(command_buffers)
    }
}

/// A renderer drawing into a texture made elsewhere (see [`SharedTarget`]).
pub fn shared_renderer(descriptors: Arc<Descriptors>, texture: wgpu::Texture) -> Result<WgpuRenderBackend<SharedTarget>> {
    WgpuRenderBackend::new(descriptors, SharedTarget::new(texture)).map_err(|e| anyhow!(e.to_string()))
}

/// A renderer drawing off screen at a size, on a GPU of our own.
pub fn offscreen_renderer(descriptors: Arc<Descriptors>, (w, h): (u32, u32)) -> Result<WgpuRenderBackend<TextureTarget>> {
    let target = TextureTarget::new(&descriptors.device, (w, h)).map_err(|e| anyhow!(e.to_string()))?;
    WgpuRenderBackend::new(descriptors, target).map_err(|e| anyhow!(e.to_string()))
}

/// What a player drawing off screen shows now.
pub fn capture(player: &Arc<Mutex<Player>>) -> Result<image::RgbaImage> {
    let mut player = player.lock().unwrap();
    player.render();
    let renderer =
        <dyn std::any::Any>::downcast_mut::<WgpuRenderBackend<TextureTarget>>(player.renderer_mut()).ok_or_else(|| anyhow!("not an off-screen renderer"))?;
    renderer.capture_frame().ok_or_else(|| anyhow!("no frame captured"))
}
