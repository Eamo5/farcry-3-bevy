//! Bink videos the story plays: full screen over the game (the opening reel,
//! the chapter quotes, logos), or on a screen in the world (a TV, a laptop):
//! the screen entity's parts with the named material show it, self-lit, until
//! it ends (a picture in a corner stands in until the screen is found). The
//! converter indexes the screens' materials (`cache::VIDEO_SCREENS_REL`) and keeps
//! the files as they are (`cache::video_rel`); they decode as they play
//! (`fc3_bink`), the sound from the stereo bed and the voice track.
//!
//! The story hears a video start at once and end when it has played out (or
//! was skipped or stopped). Full-screen ones skip with the skip keys and
//! hold the player still. A backdrop (the main menu's loop) fills the window
//! under the interface, and neither the story nor the skip keys see it.

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
        app.init_resource::<Videos>().add_systems(Update, (screen_test, play_videos, show_on_screens).chain().after(crate::story::StorySet));
    }
}

/// A video the story asked for, by its archive path (or path id).
pub struct VideoRequest {
    pub path: String,
    pub fullscreen: bool,
    pub looped: bool,
    pub volume: f32,
    /// The screen in the world it plays on: an entity id and the name of
    /// the material showing it.
    pub screen: Option<(u64, String)>,
    /// Under the interface, filling the window (the main menu's).
    pub backdrop: bool,
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

    /// Ends the backdrop.
    pub fn stop_backdrop(&mut self) {
        for p in self.playing.iter_mut().filter(|p| p.backdrop) {
            p.looped = false;
            p.t = f32::MAX;
        }
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
    backdrop: bool,
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
    /// Its screen in the world, and whether it is showing there.
    screen: Option<(u64, String)>,
    on_screen: bool,
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
    // (The story hears each start and end, while there is one.)
    let mut host = host;
    let mut event = |path: &str, ended: bool, fullscreen: bool| {
        if let Some(h) = host.as_mut() {
            h.story.video_event(path, ended, fullscreen);
        }
    };
    let videos = &mut *videos;
    for r in std::mem::take(&mut videos.requests) {
        if !r.backdrop {
            event(&r.path, false, r.fullscreen);
        }
        let file = video_file(&info.root, &r.path);
        let bink = std::fs::read(&file).map_err(anyhow::Error::from).and_then(|b| fc3_bink::Bink::new(Arc::from(b)));
        let bink = match bink {
            Ok(b) => b,
            Err(e) => {
                // Not converted (or unreadable): it ends at once.
                warn!("video {}: {e:#}", r.path);
                if !r.backdrop {
                    event(&r.path, true, r.fullscreen);
                }
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
        let ui = if r.backdrop {
            // (Filling the window, cut to fit, under the interface's canvas.)
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        overflow: Overflow::clip(),
                        ..default()
                    },
                    BackgroundColor(Color::BLACK),
                    GlobalZIndex(20),
                    VideoPanel,
                    children![(
                        Node { min_width: Val::Percent(100.0), min_height: Val::Percent(100.0), aspect_ratio: Some(aspect), ..default() },
                        ImageNode::new(image.clone())
                    )],
                ))
                .id()
        } else if r.fullscreen {
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
        videos.playing.push(Playing {
            path: r.path,
            fullscreen: r.fullscreen,
            backdrop: r.backdrop,
            looped: r.looped,
            bink,
            image,
            t: 0.0,
            shown: None,
            ui,
            sound,
            screen: r.screen,
            on_screen: false,
        });
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
        if !p.backdrop {
            event(&p.path, true, p.fullscreen);
        }
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

/// A screen's part showing a video, and the material it had.
#[derive(Component)]
struct OnScreen {
    image: Handle<Image>,
    standard: Handle<StandardMaterial>,
}

/// Puts each playing video on its screen: the parts of the screen entity
/// whose material is the one named, self-lit; and gives the parts their own
/// material back when it ends.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn show_on_screens(
    info: Res<CacheInfo>,
    mut table: Local<Option<std::collections::HashMap<String, String>>>,
    mut screen_materials: Local<std::collections::HashMap<AssetId<Image>, Handle<StandardMaterial>>>,
    mut videos: ResMut<Videos>,
    server: Res<AssetServer>,
    placed: Query<(Entity, &crate::objects::PlacedId)>,
    children: Query<&Children>,
    parts: Query<(Option<&MeshMaterial3d<StandardMaterial>>, Option<&crate::original_shaders::OriginalSwap>), Without<OnScreen>>,
    shown: Query<(Entity, &OnScreen)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut panels: Query<&mut Visibility, With<VideoPanel>>,
    mut commands: Commands,
) {
    let table = table.get_or_insert_with(|| {
        std::fs::read_to_string(info.root.join(fc3_formats::cache::VIDEO_SCREENS_REL))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(n, rel)| (n.to_string(), rel.to_string()))
            .collect()
    });
    // Screens whose video has ended show their own material again.
    let playing: std::collections::HashSet<AssetId<Image>> = videos.playing.iter().map(|p| p.image.id()).collect();
    for (e, s) in &shown {
        if !playing.contains(&s.image.id()) {
            commands.entity(e).remove::<(OnScreen, crate::tattoo::NoOriginal)>().insert(MeshMaterial3d(s.standard.clone()));
        }
    }
    screen_materials.retain(|id, _| playing.contains(id));
    for p in videos.playing.iter_mut() {
        let Some((id, name)) = &p.screen else { continue };
        let Some(rel) = table.get(&name.to_ascii_lowercase()) else { continue };
        let mut found = false;
        for (root, _) in placed.iter().filter(|(_, pid)| pid.0 == *id) {
            for e in children.iter_descendants(root) {
                let Ok((standard, swap)) = parts.get(e) else { continue };
                let Some(own) = swap.map(|s| s.standard.clone()).or_else(|| standard.map(|m| m.0.clone())) else { continue };
                if !server.get_path(&own).is_some_and(|a| a.path().to_string_lossy().replace('\\', "/").ends_with(rel.as_str())) {
                    continue;
                }
                let video = screen_materials
                    .entry(p.image.id())
                    .or_insert_with(|| materials.add(StandardMaterial { base_color_texture: Some(p.image.clone()), unlit: true, ..default() }))
                    .clone();
                commands
                    .entity(e)
                    .remove::<(crate::original_shaders::OriginalSwap, MeshMaterial3d<crate::original_shaders::OriginalMaterial>, MeshMaterial3d<StandardMaterial>)>()
                    .insert((MeshMaterial3d(video), crate::tattoo::NoOriginal, OnScreen { image: p.image.clone(), standard: own }));
                found = true;
            }
        }
        // On its screen: the stand-in picture goes.
        if found && !p.on_screen {
            p.on_screen = true;
            info!("video: {} on screen {id} ({name})", p.path);
            if let Ok(mut v) = panels.get_mut(p.ui) {
                *v = Visibility::Hidden;
            }
        }
    }
}

/// `FC3_SCREEN_TEST=<video>`: 15 s in, plays the video on the nearest placed
/// object (within 40 m) that has a screen material.
#[allow(clippy::type_complexity)]
fn screen_test(
    time: Res<Time>,
    info: Res<CacheInfo>,
    mut done: Local<bool>,
    mut videos: ResMut<Videos>,
    server: Res<AssetServer>,
    players: Query<&GlobalTransform, With<crate::player::Player>>,
    placed: Query<(Entity, &crate::objects::PlacedId, &GlobalTransform)>,
    children: Query<&Children>,
    parts: Query<(Option<&MeshMaterial3d<StandardMaterial>>, Option<&crate::original_shaders::OriginalSwap>)>,
) {
    let Ok(video) = std::env::var("FC3_SCREEN_TEST") else { return };
    if *done || time.elapsed_secs() < 15.0 {
        return;
    }
    let Ok(eye) = players.single() else { return };
    let screens: Vec<(String, String)> = std::fs::read_to_string(info.root.join(fc3_formats::cache::VIDEO_SCREENS_REL))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(n, r)| (n.to_string(), r.to_string()))
        .collect();
    let mut best: Option<(f32, u64, String)> = None;
    for (root, id, t) in &placed {
        let d = t.translation().distance(eye.translation());
        if d > 40.0 || best.as_ref().is_some_and(|b| b.0 < d) {
            continue;
        }
        for e in children.iter_descendants(root) {
            let Ok((standard, swap)) = parts.get(e) else { continue };
            let Some(own) = swap.map(|s| s.standard.clone()).or_else(|| standard.map(|m| m.0.clone())) else { continue };
            let Some(path) = server.get_path(&own).map(|a| a.path().to_string_lossy().replace('\\', "/")) else { continue };
            if let Some((name, _)) = screens.iter().find(|(_, rel)| path.ends_with(rel.as_str())) {
                best = Some((d, id.0, name.clone()));
            }
        }
    }
    // (Looked for until the objects around have streamed in.)
    match best {
        Some((d, id, name)) => {
            *done = true;
            info!("screen-test: playing on {id} ({name}) {d:.1} m away");
            videos.requests.push(VideoRequest { path: video, fullscreen: false, looped: true, volume: 1.0, screen: Some((id, name)), backdrop: false });
        }
        None if time.elapsed_secs() > 60.0 => {
            *done = true;
            info!("screen-test: no screen within 40 m");
        }
        None => {}
    }
}
