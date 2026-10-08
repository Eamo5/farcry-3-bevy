//! Bink videos the story plays: full screen over the game (the opening reel,
//! the chapter quotes, logos), or on a screen in the world (a TV, a camera's
//! playback), which shows here as a picture in a corner. The converter keeps
//! the files as they are (`cache::video_rel`); they decode as they play
//! (`fc3_bink`), the sound from the stereo bed and the voice track.
//!
//! The story hears a video start at once and end when it has played out (or
//! was skipped or stopped). Full-screen ones skip with the skip keys and
//! hold the player still.

use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::audio::{ChannelCount, PlaybackMode, SampleRate, Volume};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::data::CacheInfo;
use crate::sound_source::Sfx;
use crate::story::StoryHost;

pub struct VideoPlugin;

impl Plugin for VideoPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Videos>().add_systems(Update, play_videos.after(crate::story::StorySet));
    }
}

/// A video the story asked for, by its archive path (or path id).
pub struct VideoRequest {
    pub path: String,
    pub fullscreen: bool,
    pub looped: bool,
    pub volume: f32,
}

#[derive(Resource, Default)]
pub struct Videos {
    pub requests: Vec<VideoRequest>,
    /// Stop the full-screen video (`true`) or the screens' (`false`).
    pub stops: Vec<bool>,
    playing: Vec<Playing>,
}

impl Videos {
    /// A full-screen video holds the screen (no HUD, the player still).
    pub fn fullscreen(&self) -> bool {
        self.playing.iter().any(|p| p.fullscreen)
    }

    /// Drops every video (a reload).
    pub fn reset(&mut self) {
        self.requests.clear();
        self.stops.clear();
        self.playing.clear();
    }
}

struct Playing {
    path: String,
    fullscreen: bool,
    looped: bool,
    bink: fc3_bink::Bink,
    image: Handle<Image>,
    /// Seconds played.
    t: f32,
    /// The frame on screen.
    shown: Option<u32>,
    /// The panel showing it and its sound.
    ui: Entity,
    sound: Option<Entity>,
}

#[derive(Component)]
struct VideoPanel;

/// The file in the cache for a script's path (or the path's id).
fn video_file(root: &std::path::Path, path: &str) -> std::path::PathBuf {
    let rel = match path.parse::<u64>() {
        Ok(id) => format!("video/{id:016x}.bik"),
        Err(_) => fc3_formats::cache::video_rel(path),
    };
    root.join(rel)
}

/// The video's sound: the stereo bed (track 0) with the voice track
/// (`FC3_VIDEO_VOICE`, by default 1), mixed to stereo.
fn soundtrack(bink: &fc3_bink::Bink) -> Option<Sfx> {
    let voice: u32 = std::env::var("FC3_VIDEO_VOICE").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let tracks = bink.audio_tracks();
    let pick: Vec<usize> = (0..tracks.len()).filter(|&i| tracks[i].id == 0 || tracks[i].id == voice).collect();
    let pick = if pick.is_empty() { (0..tracks.len().min(1)).collect() } else { pick };
    let rate = tracks.get(*pick.first()?)?.sample_rate;
    let mut mix: Vec<f32> = Vec::new();
    for i in pick {
        if tracks[i].sample_rate != rate {
            continue;
        }
        let Ok(samples) = bink.decode_audio(i) else { continue };
        let channels = tracks[i].channels.max(1) as usize;
        let frames = samples.len() / channels;
        if mix.len() < frames * 2 {
            mix.resize(frames * 2, 0.0);
        }
        for f in 0..frames {
            let (l, r) = if channels == 1 { (samples[f], samples[f]) } else { (samples[f * channels], samples[f * channels + 1]) };
            mix[f * 2] += l;
            mix[f * 2 + 1] += r;
        }
    }
    if mix.is_empty() {
        return None;
    }
    for s in &mut mix {
        *s = s.clamp(-1.0, 1.0);
    }
    Some(Sfx::Decoded { samples: mix.into(), channels: ChannelCount::new(2)?, rate: SampleRate::new(rate)? })
}

#[allow(clippy::too_many_arguments)]
fn play_videos(
    host: Option<NonSendMut<StoryHost>>,
    time: Res<Time<Real>>,
    info: Res<CacheInfo>,
    mut videos: ResMut<Videos>,
    mut images: ResMut<Assets<Image>>,
    mut sounds: ResMut<Assets<Sfx>>,
    keys: Res<ButtonInput<KeyCode>>,
    gamepads: Query<&Gamepad>,
    mut hold: ResMut<crate::fp::Cinematic>,
    mut commands: Commands,
) {
    let Some(mut host) = host else {
        videos.reset();
        return;
    };
    let videos = &mut *videos;
    for r in std::mem::take(&mut videos.requests) {
        host.story.video_event(&r.path, false, r.fullscreen);
        let file = video_file(&info.root, &r.path);
        let bink = std::fs::read(&file).map_err(anyhow::Error::from).and_then(|b| fc3_bink::Bink::new(Arc::from(b)));
        let bink = match bink {
            Ok(b) => b,
            Err(e) => {
                // Not converted (or unreadable): it ends at once.
                warn!("video {}: {e:#}", r.path);
                host.story.video_event(&r.path, true, r.fullscreen);
                continue;
            }
        };
        // A new full-screen video replaces one playing.
        if r.fullscreen {
            for p in videos.playing.iter_mut().filter(|p| p.fullscreen) {
                p.looped = false;
                p.t = f32::MAX;
            }
        }
        let (w, h) = (bink.width(), bink.height());
        let image = images.add(Image::new_fill(
            Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            TextureDimension::D2,
            &[0, 0, 0, 255],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
        ));
        let aspect = w as f32 / h.max(1) as f32;
        let ui = if r.fullscreen {
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BackgroundColor(Color::BLACK),
                    GlobalZIndex(30),
                    VideoPanel,
                    children![(
                        Node { width: Val::Percent(100.0), max_height: Val::Percent(100.0), aspect_ratio: Some(aspect), ..default() },
                        ImageNode::new(image.clone())
                    )],
                ))
                .id()
        } else {
            // A screen in the world: in a corner of the view.
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        right: Val::Px(36.0),
                        top: Val::Px(90.0),
                        width: Val::Percent(32.0),
                        aspect_ratio: Some(aspect),
                        border: UiRect::all(Val::Px(2.0)),
                        ..default()
                    },
                    BorderColor::all(Color::srgba(0.0, 0.0, 0.0, 0.8)),
                    GlobalZIndex(8),
                    VideoPanel,
                    ImageNode::new(image.clone()),
                ))
                .id()
        };
        let sound = soundtrack(&bink).map(|s| {
            let volume = r.volume.clamp(0.0, 1.0) * if r.fullscreen { 1.0 } else { 0.7 };
            commands
                .spawn((
                    AudioPlayer::<Sfx>(sounds.add(s)),
                    PlaybackSettings { mode: if r.looped { PlaybackMode::Loop } else { PlaybackMode::Despawn }, volume: Volume::Linear(volume), ..default() },
                ))
                .id()
        });
        info!("video: {} ({}x{}, {:.0} s{})", r.path, w, h, bink.frame_count() as f32 * frame_time(&bink), if r.fullscreen { ", full screen" } else { "" });
        videos.playing.push(Playing { path: r.path, fullscreen: r.fullscreen, looped: r.looped, bink, image, t: 0.0, shown: None, ui, sound });
    }
    for fullscreen in std::mem::take(&mut videos.stops) {
        for p in videos.playing.iter_mut().filter(|p| p.fullscreen == fullscreen) {
            p.looped = false;
            p.t = f32::MAX;
        }
    }
    // The skip keys end a full-screen video.
    let skip = keys.any_just_pressed([KeyCode::Space, KeyCode::Enter, KeyCode::Escape])
        || gamepads.iter().any(|g| g.just_pressed(GamepadButton::South) || g.just_pressed(GamepadButton::Start))
        || std::env::var("FC3_STORY_SKIP_SCENES").is_ok();
    let dt = time.delta_secs().min(0.1);
    let mut ended = Vec::new();
    for (i, p) in videos.playing.iter_mut().enumerate() {
        if p.fullscreen {
            hold.0 = hold.0.max(0.2);
            if skip {
                p.looped = false;
                p.t = f32::MAX;
            }
        }
        p.t = if p.t == f32::MAX { p.t } else { p.t + dt };
        let count = p.bink.frame_count().max(1);
        let mut frame = (p.t / frame_time(&p.bink)) as u64;
        if frame >= count as u64 {
            if !p.looped {
                ended.push(i);
                continue;
            }
            p.t %= count as f32 * frame_time(&p.bink);
            frame %= count as u64;
        }
        let frame = frame as u32;
        if p.shown == Some(frame) {
            continue;
        }
        let Ok(decoded) = p.bink.decode_frame(frame) else { continue };
        if let Some(mut img) = images.get_mut(&p.image) {
            if let Some(data) = img.data.as_mut() {
                if data.len() == (decoded.width() * decoded.height() * 4) as usize {
                    decoded.to_rgba(data);
                }
            }
        }
        p.shown = Some(frame);
    }
    for i in ended.into_iter().rev() {
        let p = videos.playing.remove(i);
        commands.entity(p.ui).despawn();
        if let Some(s) = p.sound {
            commands.entity(s).try_despawn();
        }
        images.remove(&p.image);
        host.story.video_event(&p.path, true, p.fullscreen);
    }
}

/// Seconds a frame shows.
fn frame_time(bink: &fc3_bink::Bink) -> f32 {
    let (num, den) = bink.frame_rate();
    if num == 0 {
        1.0 / 30.0
    } else {
        den as f32 / num as f32
    }
}
