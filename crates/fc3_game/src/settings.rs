//! Player options, saved to `settings.txt` beside the cache and applied live:
//! look sensitivity, field of view, volume, view distance, shadows, window
//! mode, the original shaders and key bindings.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use bevy::audio::{GlobalVolume, Volume};
use bevy::prelude::*;
use bevy::window::{Monitor, MonitorSelection, PresentMode, PrimaryMonitor, PrimaryWindow, VideoModeSelection, WindowMode};

use crate::controls::{default_bindings, Action, Binding};
use crate::data::CacheInfo;

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Settings::default())
            .init_resource::<SettingsFile>()
            .init_resource::<Displays>()
            .init_resource::<LetterboxFov>()
            .add_systems(First, narrow_fov)
            .add_systems(PostUpdate, widen_fov.before(bevy::camera::CameraUpdateSystems))
            .add_systems(Update, (load_settings.run_if(resource_exists::<CacheInfo>), list_displays, apply_settings, apply_effects, save_settings).chain());
    }
}

/// The game's difficulty levels: how hard enemies hit and how quickly they
/// spot the player.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Difficulty {
    Casual,
    Adventurer,
    Warrior,
    Infamous,
}

impl Difficulty {
    pub const ALL: [Difficulty; 4] = [Difficulty::Casual, Difficulty::Adventurer, Difficulty::Warrior, Difficulty::Infamous];

    pub fn name(self) -> &'static str {
        match self {
            Difficulty::Casual => "Casual",
            Difficulty::Adventurer => "Adventurer",
            Difficulty::Warrior => "Warrior",
            Difficulty::Infamous => "Infamous",
        }
    }

    /// Its place among the game's levels (casual 0 ... infamous 3).
    pub fn index(self) -> usize {
        Difficulty::ALL.iter().position(|d| *d == self).unwrap_or(1)
    }

    pub fn from_name(s: &str) -> Option<Difficulty> {
        Difficulty::ALL.into_iter().find(|d| d.name().eq_ignore_ascii_case(s))
    }

    /// Multiplier on damage the player takes, without the game's table
    /// (`combat::DamageTable`: its health bars' scale by difficulty).
    pub fn damage_taken(self) -> f32 {
        match self {
            Difficulty::Casual => 0.5,
            Difficulty::Adventurer => 1.0,
            Difficulty::Warrior => 1.6,
            Difficulty::Infamous => 3.0,
        }
    }

    /// Multiplier on how fast enemies notice the player.
    pub fn spotting(self) -> f32 {
        match self {
            Difficulty::Casual => 0.7,
            Difficulty::Adventurer => 1.0,
            Difficulty::Warrior | Difficulty::Infamous => 1.35,
        }
    }
}

/// A quality level, as the options' video quality rows have them (some rows
/// stop at High or Very high).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Quality {
    Low,
    Medium,
    High,
    VeryHigh,
    Ultra,
}

impl Quality {
    pub const ALL: [Quality; 5] = [Quality::Low, Quality::Medium, Quality::High, Quality::VeryHigh, Quality::Ultra];

    pub fn name(self) -> &'static str {
        match self {
            Quality::Low => "Low",
            Quality::Medium => "Medium",
            Quality::High => "High",
            Quality::VeryHigh => "Very high",
            Quality::Ultra => "Ultra",
        }
    }

    /// Its name in the settings file (one word).
    fn id(self) -> &'static str {
        match self {
            Quality::VeryHigh => "VeryHigh",
            q => q.name(),
        }
    }

    fn from_name(s: &str) -> Option<Quality> {
        Quality::ALL.into_iter().find(|q| q.id().eq_ignore_ascii_case(s) || q.name().eq_ignore_ascii_case(s))
    }

    pub fn index(self) -> usize {
        Quality::ALL.iter().position(|q| *q == self).unwrap_or(0)
    }
}

/// How the game fills the screen (the options' window mode, in its order).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowKind {
    /// Exclusive, at the display resolution.
    Fullscreen,
    Windowed,
    /// A window the size of the screen.
    Borderless,
}

impl WindowKind {
    pub const ALL: [WindowKind; 3] = [WindowKind::Fullscreen, WindowKind::Windowed, WindowKind::Borderless];

    pub fn name(self) -> &'static str {
        match self {
            WindowKind::Fullscreen => "Fullscreen",
            WindowKind::Windowed => "Windowed",
            WindowKind::Borderless => "Borderless",
        }
    }

    fn from_name(s: &str) -> Option<WindowKind> {
        WindowKind::ALL.into_iter().find(|k| k.name().eq_ignore_ascii_case(s))
    }
}

#[derive(Resource, Clone, PartialEq, Debug)]
pub struct Settings {
    /// Mouse look multiplier.
    pub sensitivity: f32,
    pub gamepad_sensitivity: f32,
    pub invert_y: bool,
    /// The gamepad's rumble.
    pub vibration: bool,
    /// The game's aim assist with a gamepad.
    pub aim_assist: bool,
    /// Horizontal-ish field of view in degrees (vertical FOV of the camera).
    pub fov: f32,
    pub master_volume: f32,
    /// The music's share of the volume.
    pub music_volume: f32,
    /// The video quality rows (the options' order: textures, ambient
    /// lighting, shadow, post FX, geometry, vegetation, terrain, water).
    /// Textures, ambient lighting and terrain go to High, vegetation and
    /// water to Very high.
    pub textures: Quality,
    /// Screen-space ambient occlusion: off (Low), plain (Medium), by
    /// `ssao_method` (High). The game has it on at every PC quality; here it
    /// costs about 2.5 ms a frame (its depth and normal prepass draws the
    /// scene again).
    pub ambient: Quality,
    pub shadows: Quality,
    /// Post-processing: bloom (Medium), and sun shafts (High and up).
    pub effects: Quality,
    /// Geometry: how far the world streams in.
    pub view_distance: Quality,
    pub vegetation: Quality,
    pub terrain: Quality,
    pub water: Quality,
    /// The options' anti-aliasing: off, 2, 4 or 8 samples (here SMAA's
    /// low, medium and high presets).
    pub antialiasing: u8,
    /// The options' occlusion method: SSAO, HDAO, HBAO (here the occlusion's
    /// medium, high and ultra quality).
    pub ssao_method: u8,
    pub vsync: bool,
    /// The refreshes V-Sync waits a frame for (1 or 2, as the options show
    /// it; both present at every refresh here).
    pub vsync_frames: u8,
    /// How many frames the GPU may queue (1-5).
    pub buffered_frames: u32,
    pub window: WindowKind,
    /// The display resolution (none: the desktop's, or the window's own).
    pub resolution: Option<UVec2>,
    /// Shows a 16:9 band on screens narrower than that.
    pub letterbox: bool,
    pub original_shaders: bool,
    pub show_fps: bool,
    /// Subtitles for spoken lines.
    pub subtitles: bool,
    /// A new game plays the story (else the island is free to roam).
    pub story: bool,
    pub difficulty: Difficulty,
    /// The calibration popup's brightness, contrast and gamma (its sliders,
    /// 0-100, 50 neutral).
    pub brightness: f32,
    pub contrast: f32,
    pub gamma: f32,
    pub bindings: HashMap<Action, Binding>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sensitivity: 1.0,
            gamepad_sensitivity: 1.0,
            invert_y: false,
            vibration: true,
            aim_assist: true,
            fov: 75.0,
            master_volume: 0.8,
            music_volume: 0.6,
            textures: Quality::High,
            ambient: Quality::High,
            shadows: Quality::High,
            effects: Quality::High,
            view_distance: Quality::High,
            vegetation: Quality::High,
            terrain: Quality::High,
            water: Quality::High,
            antialiasing: 2,
            ssao_method: 0,
            vsync: true,
            vsync_frames: 1,
            buffered_frames: 2,
            window: WindowKind::Windowed,
            resolution: None,
            letterbox: false,
            original_shaders: true,
            show_fps: true,
            subtitles: true,
            story: true,
            difficulty: Difficulty::Adventurer,
            brightness: 50.0,
            contrast: 50.0,
            gamma: 50.0,
            bindings: default_bindings(),
        }
    }
}

impl Settings {
    pub fn binding(&self, a: Action) -> Option<Binding> {
        self.bindings.get(&a).copied()
    }

    /// Binds `a`, swapping with whichever action had that input.
    pub fn rebind(&mut self, a: Action, b: Binding) {
        let old = self.bindings.get(&a).copied();
        if let Some(other) = self.bindings.iter().find(|(k, v)| **k != a && **v == b).map(|(k, _)| *k) {
            match old {
                Some(o) => self.bindings.insert(other, o),
                None => self.bindings.remove(&other),
            };
        }
        self.bindings.insert(a, b);
    }

    /// The calibration as the game's final passes take it
    /// (`ColorAdjustment`): an offset (brightness), a scale (contrast) and an
    /// exponent (one over gamma). The engine maps the sliders to brightness
    /// 0.9-1.1, contrast 0-2 and gamma 0.8-1.2, then the offset is a sixth
    /// of brightness - 1 and the scale 1 + (contrast - 1) / 2.
    pub fn color_adjustment(&self) -> [f32; 3] {
        let brightness = (0.9 + self.brightness * 0.002).clamp(0.9, 1.1);
        let contrast = self.contrast * 0.02;
        let gamma = (0.8 + self.gamma * 0.004).clamp(0.8, 1.2);
        [(brightness - 1.0) / 6.0, 1.0 + 0.5 * (contrast - 1.0).clamp(-1.0, 1.0), 1.0 / gamma]
    }

    /// Streaming radii scale for the view distance setting.
    pub fn distance_scale(&self) -> f32 {
        [0.65, 0.85, 1.0, 1.15, 1.35][self.view_distance.index()]
    }

    /// Sets every video quality row to an overall level (the options'
    /// overall quality), each row at its own highest where it stops short.
    pub fn apply_preset(&mut self, level: Quality) {
        let up_to = |max: Quality| level.min(max);
        self.textures = up_to(Quality::High);
        self.ambient = up_to(Quality::High);
        self.shadows = level;
        self.effects = level;
        self.view_distance = level;
        self.vegetation = up_to(Quality::VeryHigh);
        self.terrain = up_to(Quality::High);
        self.water = up_to(Quality::VeryHigh);
    }

    /// The overall level the video quality rows make, if they make one.
    pub fn preset(&self) -> Option<Quality> {
        Quality::ALL.into_iter().find(|&q| {
            let mut s = self.clone();
            s.apply_preset(q);
            s == *self
        })
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from("fc3settings 1\n");
        out += &format!("sensitivity {}\ngamepad_sensitivity {}\ninvert_y {}\nfov {}\n", self.sensitivity, self.gamepad_sensitivity, self.invert_y, self.fov);
        out += &format!("master_volume {}\n", self.master_volume);
        for (key, q) in [
            ("textures", self.textures),
            ("ambient", self.ambient),
            ("shadows", self.shadows),
            ("effects", self.effects),
            ("view_distance", self.view_distance),
            ("vegetation", self.vegetation),
            ("terrain", self.terrain),
            ("water", self.water),
        ] {
            out += &format!("{key} {}\n", q.id());
        }
        out += &format!("antialiasing {}\nssao_method {}\n", self.antialiasing, self.ssao_method);
        out += &format!("vsync {}\nvsync_frames {}\nbuffered_frames {}\n", self.vsync, self.vsync_frames, self.buffered_frames);
        let resolution = self.resolution.map_or_else(|| "desktop".to_string(), |r| format!("{}x{}", r.x, r.y));
        out += &format!("window {}\nresolution {resolution}\nletterbox {}\n", self.window.name(), self.letterbox);
        out += &format!("original_shaders {}\nshow_fps {}\n", self.original_shaders, self.show_fps);
        out += &format!("subtitles {}\nstory {}\ndifficulty {}\nmusic_volume {}\n", self.subtitles, self.story, self.difficulty.name(), self.music_volume);
        out += &format!("vibration {}\naim_assist {}\n", self.vibration, self.aim_assist);
        out += &format!("brightness {}\ncontrast {}\ngamma {}\n", self.brightness, self.contrast, self.gamma);
        for a in Action::ALL {
            if let Some(b) = self.binding(a) {
                out += &format!("bind {} {}\n", a.id(), b.id());
            }
        }
        out
    }

    pub fn from_text(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines().skip(1) {
            let mut it = line.split_whitespace();
            let (Some(key), Some(value)) = (it.next(), it.next()) else { continue };
            let f = |v: &str, d: f32| v.parse::<f32>().unwrap_or(d);
            let b = |v: &str, d: bool| v.parse::<bool>().unwrap_or(d);
            match key {
                "sensitivity" => s.sensitivity = f(value, 1.0).clamp(0.1, 5.0),
                "gamepad_sensitivity" => s.gamepad_sensitivity = f(value, 1.0).clamp(0.1, 5.0),
                "invert_y" => s.invert_y = b(value, false),
                "vibration" => s.vibration = b(value, true),
                "aim_assist" => s.aim_assist = b(value, true),
                "fov" => s.fov = f(value, 75.0).clamp(55.0, 110.0),
                "master_volume" => s.master_volume = f(value, 0.8).clamp(0.0, 1.0),
                "music_volume" => s.music_volume = f(value, 0.6).clamp(0.0, 1.0),
                "textures" => s.textures = Quality::from_name(value).unwrap_or(Quality::High).min(Quality::High),
                "ambient" => s.ambient = Quality::from_name(value).unwrap_or(Quality::High).min(Quality::High),
                "shadows" => s.shadows = Quality::from_name(value).unwrap_or(Quality::High),
                "effects" => s.effects = Quality::from_name(value).unwrap_or(Quality::High),
                "view_distance" => s.view_distance = Quality::from_name(value).unwrap_or(Quality::High),
                "vegetation" => s.vegetation = Quality::from_name(value).unwrap_or(Quality::High).min(Quality::VeryHigh),
                "terrain" => s.terrain = Quality::from_name(value).unwrap_or(Quality::High).min(Quality::High),
                "water" => s.water = Quality::from_name(value).unwrap_or(Quality::High).min(Quality::VeryHigh),
                "antialiasing" => s.antialiasing = value.parse::<u8>().unwrap_or(2).min(3),
                "ssao_method" => s.ssao_method = value.parse::<u8>().unwrap_or(0).min(2),
                "vsync" => s.vsync = b(value, true),
                "vsync_frames" => s.vsync_frames = value.parse::<u8>().unwrap_or(1).clamp(1, 2),
                "buffered_frames" => s.buffered_frames = value.parse::<u32>().unwrap_or(2).clamp(1, 5),
                "window" => s.window = WindowKind::from_name(value).unwrap_or(WindowKind::Windowed),
                // (Older files: fullscreen meant a borderless window.)
                "fullscreen" => s.window = if b(value, false) { WindowKind::Borderless } else { WindowKind::Windowed },
                "resolution" => {
                    s.resolution = value.split_once('x').and_then(|(w, h)| Some(UVec2::new(w.parse().ok()?, h.parse().ok()?))).filter(|r| r.min_element() > 0)
                }
                "letterbox" => s.letterbox = b(value, false),
                "original_shaders" => s.original_shaders = b(value, true),
                "show_fps" => s.show_fps = b(value, true),
                "subtitles" => s.subtitles = b(value, true),
                "story" => s.story = b(value, true),
                "difficulty" => s.difficulty = Difficulty::from_name(value).unwrap_or(Difficulty::Adventurer),
                "brightness" => s.brightness = f(value, 50.0).clamp(0.0, 100.0),
                "contrast" => s.contrast = f(value, 50.0).clamp(0.0, 100.0),
                "gamma" => s.gamma = f(value, 50.0).clamp(0.0, 100.0),
                "bind" => {
                    if let (Some(a), Some(binding)) = (Action::from_id(value), it.next().and_then(Binding::from_id)) {
                        s.rebind(a, binding);
                    }
                }
                _ => {}
            }
        }
        s
    }
}

/// The letterbox's band: the share of the window's height a 16:9 picture
/// takes when the window is narrower than that (none: no bars).
pub fn letterbox_band(settings: &Settings, window: &Window) -> Option<f32> {
    let (w, h) = (window.width(), window.height());
    (settings.letterbox && h > 0.0 && w / h < 16.0 / 9.0 - 0.01).then(|| (w / h) / (16.0 / 9.0))
}

/// The field of view while rendering under the letterbox, widened so the
/// band shows what a 16:9 screen would; the frame's own systems see the
/// unwidened one (put back in `First`).
#[derive(Resource, Default)]
struct LetterboxFov(Option<(f32, f32)>);

fn widen_fov(
    settings: Res<Settings>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<&mut Projection, With<crate::player::PlayerCamera>>,
    mut fov: ResMut<LetterboxFov>,
) {
    let Some(band) = windows.single().ok().and_then(|w| letterbox_band(&settings, w)) else { return };
    for mut projection in &mut cameras {
        if let Projection::Perspective(p) = &mut *projection {
            let wide = 2.0 * ((p.fov * 0.5).tan() / band).atan();
            fov.0 = Some((p.fov, wide));
            p.fov = wide;
        }
    }
}

fn narrow_fov(mut cameras: Query<&mut Projection, With<crate::player::PlayerCamera>>, mut fov: ResMut<LetterboxFov>) {
    let Some((narrow, wide)) = fov.0.take() else { return };
    for mut projection in &mut cameras {
        if let Projection::Perspective(p) = &mut *projection {
            if p.fov == wide {
                p.fov = narrow;
            }
        }
    }
}

/// The display's resolutions (smallest first) and video modes, and the
/// renderer's graphics API, for the options' video rows.
#[derive(Resource, Default)]
pub struct Displays {
    pub sizes: Vec<UVec2>,
    pub modes: Vec<bevy::window::VideoMode>,
    pub api: String,
}

impl Displays {
    /// The exclusive mode for a resolution: the fastest refresh at it.
    fn mode(&self, size: UVec2) -> Option<bevy::window::VideoMode> {
        self.modes.iter().filter(|m| m.physical_size == size).max_by_key(|m| (m.refresh_rate_millihertz, m.bit_depth)).copied()
    }
}

fn list_displays(
    monitors: Query<&Monitor, With<PrimaryMonitor>>,
    adapter: Option<Res<bevy::render::renderer::RenderAdapterInfo>>,
    mut displays: ResMut<Displays>,
) {
    if displays.api.is_empty() {
        if let Some(a) = adapter {
            let backend = format!("{:?}", a.backend);
            displays.api = match backend.as_str() {
                "Dx12" => "DirectX 12".into(),
                "Gl" => "OpenGL".into(),
                _ => backend,
            };
        }
    }
    let Ok(m) = monitors.single() else { return };
    if m.video_modes.len() == displays.modes.len() && !displays.sizes.is_empty() {
        return;
    }
    let mut sizes: Vec<UVec2> = m.video_modes.iter().map(|v| v.physical_size).collect();
    sizes.push(m.physical_size());
    sizes.sort_by_key(|s| (s.x, s.y));
    sizes.dedup();
    displays.sizes = sizes;
    displays.modes = m.video_modes.clone();
}

#[derive(Resource, Default)]
struct SettingsFile {
    path: Option<PathBuf>,
    loaded: bool,
    /// What's on disk, to save only real changes.
    saved: Option<Settings>,
}

fn load_settings(info: Res<CacheInfo>, args: Res<crate::Args>, mut file: ResMut<SettingsFile>, mut settings: ResMut<Settings>) {
    if file.loaded {
        return;
    }
    file.loaded = true;
    let path = info.root.parent().unwrap_or(&info.root).join("settings.txt");
    if let Ok(text) = std::fs::read_to_string(&path) {
        *settings = Settings::from_text(&text);
    }
    // Command-line switches win for this session (and aren't saved).
    if !args.original_shaders {
        settings.original_shaders = false;
    }
    if args.novsync {
        settings.vsync = false;
    }
    // FC3_DIFFICULTY=Casual|Adventurer|Warrior|Infamous (tests).
    if let Some(d) = std::env::var("FC3_DIFFICULTY").ok().and_then(|v| Difficulty::from_name(&v)) {
        settings.difficulty = d;
    }
    crate::cache_assets::set_texture_quality(settings.textures.index());
    file.saved = Some(settings.clone());
    // Automated runs never write the user's settings.
    file.path = (!args.automated()).then_some(path);
}

fn save_settings(settings: Res<Settings>, mut file: ResMut<SettingsFile>) {
    if !settings.is_changed() || file.saved.as_ref() == Some(&*settings) {
        return;
    }
    file.saved = Some(settings.clone());
    if let Some(path) = &file.path {
        if let Err(e) = std::fs::write(path, settings.to_text()) {
            warn!("could not save settings to {}: {e}", path.display());
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// Puts the effects setting on the player's camera (when it changes, or the
/// camera appears).
fn apply_effects(settings: Res<Settings>, cameras: Query<(Entity, Ref<crate::player::PlayerCamera>)>, mut commands: Commands) {
    use bevy::anti_alias::smaa::{Smaa, SmaaPreset};
    use bevy::core_pipeline::prepass::{DepthPrepass, NormalPrepass};
    use bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel as SsaoQuality};
    // FC3_EFFECTS (tests) sets the post-processing, anti-aliasing and
    // occlusion together, from one level.
    let forced = std::env::var("FC3_EFFECTS").ok().and_then(|v| Quality::from_name(&v));
    let effects = forced.unwrap_or(settings.effects);
    let smaa = match forced {
        Some(Quality::Low) => None,
        Some(Quality::Ultra) => Some(SmaaPreset::High),
        Some(_) => Some(SmaaPreset::Medium),
        None => [None, Some(SmaaPreset::Low), Some(SmaaPreset::Medium), Some(SmaaPreset::High)][settings.antialiasing.min(3) as usize],
    };
    // Screen-space ambient occlusion, as the game has (FC3_SSAO=0 leaves it off).
    let ssao = match forced {
        Some(q) if q >= Quality::High => Some(if q == Quality::Ultra { SsaoQuality::High } else { SsaoQuality::Medium }),
        Some(_) => None,
        None => match settings.ambient {
            Quality::Low => None,
            Quality::Medium => Some(SsaoQuality::Low),
            _ => Some([SsaoQuality::Medium, SsaoQuality::High, SsaoQuality::Ultra][settings.ssao_method.min(2) as usize]),
        },
    }
    .filter(|_| std::env::var("FC3_SSAO").as_deref() != Ok("0"));
    for (e, cam) in &cameras {
        if !settings.is_changed() && !cam.is_added() {
            continue;
        }
        let mut c = commands.entity(e);
        match smaa {
            Some(preset) => c.insert(Smaa { preset }),
            None => c.remove::<Smaa>(),
        };
        match ssao {
            Some(quality_level) => {
                c.insert((DepthPrepass, NormalPrepass, ScreenSpaceAmbientOcclusion { quality_level, ..default() }));
            }
            None => {
                c.remove::<(ScreenSpaceAmbientOcclusion, NormalPrepass)>();
                // (The depth of field reads the depth prepass.)
                if effects >= Quality::High {
                    c.insert(DepthPrepass);
                } else {
                    c.remove::<DepthPrepass>();
                }
            }
        }
        if effects >= Quality::Medium {
            // Bloom: the game's or Bevy's (`bloom.rs` picks).
            c.insert(crate::bloom::WantsBloom);
        } else {
            c.remove::<crate::bloom::WantsBloom>();
        }
        if effects >= Quality::High {
            c.insert(crate::light_shafts::LightShafts::default());
        } else {
            c.remove::<crate::light_shafts::LightShafts>();
        }
    }
}

/// `FC3_MUTE` (tests): every sound silent, but still played and mixed
/// through (the mixer's levels and limiter as they would be).
pub fn muted() -> bool {
    static MUTED: LazyLock<bool> = LazyLock::new(|| std::env::var_os("FC3_MUTE").is_some());
    *MUTED
}

fn apply_settings(
    settings: Res<Settings>,
    displays: Res<Displays>,
    mut volume: ResMut<GlobalVolume>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut original: ResMut<crate::original_shaders::OriginalShaders>,
    mut lights: Query<(&mut DirectionalLight, &mut bevy::light::CascadeShadowConfig)>,
) {
    if !settings.is_changed() {
        return;
    }
    // (Muted: silent from their start, videos' soundtracks too.)
    volume.volume = Volume::Linear(if muted() { 0.0 } else { settings.master_volume });
    if let Ok(mut w) = windows.single_mut() {
        let present = if settings.vsync { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync };
        if w.present_mode != present {
            w.present_mode = present;
        }
        let latency = std::num::NonZero::new(settings.buffered_frames.clamp(1, 5));
        if w.desired_maximum_frame_latency != latency {
            w.desired_maximum_frame_latency = latency;
        }
        let mode = match settings.window {
            WindowKind::Fullscreen => {
                let video = settings.resolution.and_then(|r| displays.mode(r)).map_or(VideoModeSelection::Current, VideoModeSelection::Specific);
                WindowMode::Fullscreen(MonitorSelection::Current, video)
            }
            WindowKind::Windowed => WindowMode::Windowed,
            WindowKind::Borderless => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
        };
        if w.mode != mode {
            w.mode = mode;
        }
        if let (WindowKind::Windowed, Some(r)) = (settings.window, settings.resolution) {
            if w.resolution.physical_size() != r {
                w.resolution.set_physical_resolution(r.x, r.y);
            }
        }
    }
    if original.enabled != settings.original_shaders {
        original.enabled = settings.original_shaders;
    }
    for (mut light, mut cascades) in &mut lights {
        light.shadow_maps_enabled = settings.shadows != Quality::Low;
        let (count, distance) = match settings.shadows {
            Quality::Low | Quality::Medium => (2, 150.0),
            Quality::High => (4, 400.0),
            Quality::VeryHigh => (4, 600.0),
            Quality::Ultra => (4, 800.0),
        };
        *cascades =
            bevy::light::CascadeShadowConfigBuilder { num_cascades: count, first_cascade_far_bound: 12.0, maximum_distance: distance, ..default() }.build();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip() {
        let mut s = Settings {
            sensitivity: 1.7,
            fov: 90.0,
            invert_y: true,
            view_distance: Quality::Ultra,
            difficulty: Difficulty::Warrior,
            brightness: 70.0,
            gamma: 20.0,
            vegetation: Quality::VeryHigh,
            window: WindowKind::Fullscreen,
            resolution: Some(UVec2::new(1600, 900)),
            buffered_frames: 4,
            ..Default::default()
        };
        s.rebind(Action::Jump, Binding::Key(KeyCode::KeyV));
        s.rebind(Action::Fire, Binding::Key(KeyCode::KeyW));
        // Taking W from "forward" hands it fire's old binding.
        assert_eq!(s.binding(Action::Forward), Some(Binding::Mouse(MouseButton::Left)));
        assert_eq!(Settings::from_text(&s.to_text()), s);
    }

    #[test]
    fn overall_quality_sets_each_row_up_to_its_own_highest() {
        let mut s = Settings::default();
        assert_eq!(s.preset(), Some(Quality::High));
        s.apply_preset(Quality::Ultra);
        assert_eq!((s.textures, s.vegetation, s.shadows), (Quality::High, Quality::VeryHigh, Quality::Ultra));
        assert_eq!(s.preset(), Some(Quality::Ultra));
        s.apply_preset(Quality::Low);
        assert_eq!(s.preset(), Some(Quality::Low));
        // A row of its own makes it custom.
        s.water = Quality::Medium;
        assert_eq!(s.preset(), None);
        // Older files: fullscreen was a borderless window.
        assert_eq!(Settings::from_text("fc3settings 1\nfullscreen true\n").window, WindowKind::Borderless);
    }

    #[test]
    fn calibration_maps_the_sliders_as_the_engine_does() {
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4);
        // 50 everywhere leaves the picture.
        assert!(near(Settings::default().color_adjustment(), [0.0, 1.0, 1.0]));
        // The far ends: brightness 1.1 (an offset of 0.1 / 6), contrast 2
        // (a scale of 1.5), gamma 1.2 (an exponent of 1 / 1.2).
        let top = Settings { brightness: 100.0, contrast: 100.0, gamma: 100.0, ..Default::default() };
        assert!(near(top.color_adjustment(), [0.1 / 6.0, 1.5, 1.0 / 1.2]));
        let bottom = Settings { brightness: 0.0, contrast: 0.0, gamma: 0.0, ..Default::default() };
        assert!(near(bottom.color_adjustment(), [-0.1 / 6.0, 0.5, 1.0 / 0.8]));
    }
}
