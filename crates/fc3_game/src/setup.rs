//! First-launch setup: converts the installed game into the cache in the
//! background while showing progress, then hands over to the main menu.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use fc3_convert::{Progress, Stage};

use crate::data::CacheInfo;

pub struct SetupPlugin;

impl Plugin for SetupPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_screen).add_systems(Update, poll_conversion);
    }
}

/// Shared state of a background conversion.
#[derive(Resource, Clone)]
pub struct Conversion {
    pub progress: Arc<Mutex<Option<Progress>>>,
    /// `Some(Ok)` when finished, `Some(Err(msg))` on failure.
    pub result: Arc<Mutex<Option<Result<(), String>>>>,
    pub cache_root: std::path::PathBuf,
}

/// A fatal setup problem to show on screen (e.g. game install not found).
#[derive(Resource, Clone)]
pub struct SetupError(pub String);

#[derive(Component)]
struct SetupRoot;

#[derive(Component)]
struct SetupText;

#[derive(Component)]
struct SetupBar;

/// Gibbed-style file name lists (used to enumerate every model and texture).
pub fn filelist_dir() -> Option<std::path::PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    [
        exe_dir.as_ref().map(|d| d.join("data/filelists")),
        exe_dir.as_ref().map(|d| d.join("../../data/filelists")),
        Some(std::path::PathBuf::from("data/filelists")),
        Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/filelists")),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.exists())
}

/// Starts converting `data_dir` into `cache_root` on a background thread.
pub fn start_conversion(data_dir: std::path::PathBuf, cache_root: std::path::PathBuf, filelists: Option<std::path::PathBuf>) -> Conversion {
    let conv = Conversion {
        progress: Arc::new(Mutex::new(None)),
        result: Arc::new(Mutex::new(None)),
        cache_root: cache_root.clone(),
    };
    let progress = conv.progress.clone();
    let result = conv.result.clone();
    std::thread::spawn(move || {
        let mut opts = fc3_convert::Options::new(data_dir, cache_root);
        opts.filelists = filelists;
        let ctx = fc3_convert::Context_::default();
        let report = |p: Progress| {
            *progress.lock().unwrap() = Some(p);
        };
        let r = fc3_convert::convert(&opts, &ctx, &report).map(|_| ()).map_err(|e| format!("{e:#}"));
        *result.lock().unwrap() = Some(r);
    });
    conv
}

fn setup_screen(mut commands: Commands, conversion: Option<Res<Conversion>>, error: Option<Res<SetupError>>) {
    if conversion.is_none() && error.is_none() {
        return;
    }
    let font = |size: f32| TextFont { font_size: bevy::text::FontSize::Px(size), ..default() };
    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(18.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.04, 0.05, 0.05)),
            GlobalZIndex(50),
            SetupRoot,
        ))
        .id();
    commands.spawn((Text::new("FC3 BEVY"), font(56.0), TextColor(Color::srgb(0.95, 0.93, 0.88)), ChildOf(root)));
    let msg = match &error {
        Some(e) => e.0.clone(),
        None => "Preparing your game data (first launch only)...".into(),
    };
    commands.spawn((
        Text::new(msg),
        font(20.0),
        TextColor(Color::srgba(0.9, 0.9, 0.85, 0.85)),
        TextLayout::new(Justify::Center, bevy::text::LineBreak::WordBoundary),
        SetupText,
        ChildOf(root),
    ));
    if error.is_none() {
        let track = commands
            .spawn((
                Node { width: Val::Px(520.0), height: Val::Px(10.0), ..default() },
                BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.12)),
                ChildOf(root),
            ))
            .id();
        commands.spawn((
            Node { width: Val::Percent(0.0), height: Val::Percent(100.0), ..default() },
            BackgroundColor(Color::srgb(0.85, 0.35, 0.15)),
            SetupBar,
            ChildOf(track),
        ));
    }
}

fn poll_conversion(
    mut commands: Commands,
    conversion: Option<Res<Conversion>>,
    mut text: Query<&mut Text, With<SetupText>>,
    mut bar: Query<&mut Node, With<SetupBar>>,
    roots: Query<Entity, With<SetupRoot>>,
) {
    let Some(conv) = conversion else { return };
    let finished = conv.result.lock().unwrap().clone();
    match finished {
        None => {
            let p = *conv.progress.lock().unwrap();
            if let Some(p) = p {
                if let Ok(mut t) = text.single_mut() {
                    let stage = if p.stage == Stage::Scan { "Reading game archives".to_string() } else { p.stage.label().to_string() };
                    t.0 = format!(
                        "Preparing your game data (first launch only)\n{stage}  {} / {}   ({:.0} MB written)",
                        p.done,
                        p.total,
                        p.bytes_written as f64 / 1e6
                    );
                }
                if let Ok(mut n) = bar.single_mut() {
                    n.width = Val::Percent(p.overall() * 100.0);
                }
            }
        }
        Some(Ok(())) => match CacheInfo::load(conv.cache_root.clone()) {
            Ok(info) => {
                commands.insert_resource(info);
                commands.remove_resource::<Conversion>();
                for r in &roots {
                    commands.entity(r).despawn();
                }
            }
            Err(e) => {
                if let Ok(mut t) = text.single_mut() {
                    t.0 = format!("Conversion finished but the cache could not be read:\n{e:#}");
                }
                commands.remove_resource::<Conversion>();
            }
        },
        Some(Err(e)) => {
            if let Ok(mut t) = text.single_mut() {
                t.0 = format!("Could not convert the game data:\n{e}");
            }
            commands.remove_resource::<Conversion>();
        }
    }
}
