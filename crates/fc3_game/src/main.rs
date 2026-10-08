//! Far Cry 3 reimplemented on Bevy, loading data from an installed copy of the game.

mod birds;
mod bloom;
mod cache_assets;
mod camera;
mod characters;
mod cinema;
mod clouds;
mod combat;
mod controls;
mod data;
mod dof;
mod explosives;
mod far_terrain;
mod fire;
mod fish;
mod fp;
mod freefall;
mod grade;
mod guard_spots;
mod horizon;
mod hud;
mod lamps;
mod lens_flare;
mod light_shafts;
mod map;
mod menu;
mod mesh_slabs;
mod metal;
mod mission_layers;
mod music;
mod nav;
mod npc;
mod objects;
mod options;
mod original_shaders;
mod outposts;
mod particles;
mod patrols;
mod physics;
mod pickups;
mod player;
mod player_audio;
mod profile;
mod progression;
mod faces;
mod qte;
mod retire;
mod save;
mod settings;
mod setup;
mod shadow_lod;
mod sky;
mod sky_probe;
mod sound;
mod sound_source;
mod splines;
mod stealth;
mod story;
mod story_actors;
mod surfaces;
mod syringes;
mod terrain;
mod traversal;
mod turrets;
mod ui_nav;
mod upload_budget;
mod vegetation;
mod vehicles;
mod video;
mod villagers;
mod vista;
mod water;
mod weapon_wheel;
mod weapons;
mod wildlife;
mod world_fx;

use bevy::asset::io::AssetSourceBuilder;
use bevy::prelude::*;
use bevy::window::PresentMode;

use crate::data::{to_bevy, CacheInfo};

/// Command-line options. Positions are in game (Dunia, Z-up) coordinates.
#[derive(Resource, Clone)]
pub struct Args {
    pub start: Vec3,
    /// `--pos` was given (a test at a place: no story).
    pub placed: bool,
    pub yaw: f32,
    pub pitch: f32,
    pub time_of_day: f32,
    pub screenshot: Option<String>,
    pub settle: f32,
    /// Start holding this weapon (by name).
    pub start_weapon: Option<String>,
    pub noclip: bool,
    /// Test: spawn on the floor below the `--pos` height (in a cave) instead
    /// of on the top surface.
    pub underground: bool,
    /// Scripted test: hold "forward" (and sprint/jump periodically) for this many seconds.
    pub walk_secs: f32,
    /// Scripted test: hold the trigger at a target for this many seconds.
    pub fire_secs: f32,
    /// Debug: spawn one frozen enemy in front of the player playing this clip.
    pub npc_test: Option<String>,
    /// Keep the main menu up during automated screenshots.
    pub show_menu: bool,
    /// Soak test: fly a fixed route across the island for this many seconds, logging stats.
    pub tour_secs: f32,
    /// Uncapped frame rate (for profiling).
    pub novsync: bool,
    /// Start from the save (as the menu's CONTINUE would).
    pub continue_save: bool,
    /// Render props with the game's own (translated) shaders (default on); F4 toggles.
    pub original_shaders: bool,
    /// Test: open the full-screen map once playing (for screenshots).
    pub open_map: bool,
    /// Test: show the options screen over the menu (for screenshots).
    pub open_options: bool,
    /// Scripted test: alert, alarm and clear the nearest outpost, logging each step.
    pub outpost_test: bool,
    /// Scripted test: sneak up on the `--npc-test` enemy and take it down.
    pub takedown_test: bool,
    /// Test: open the journal once playing (for screenshots).
    pub open_journal: bool,
    /// Open the shop once playing (tests), with this tab.
    pub open_shop: Option<usize>,
    /// Test: a row of frozen animals in front of the player playing this clip (idle|walk|run|eat).
    pub animal_test: Option<String>,
    /// Scripted test: get into the nearest vehicle and drive it, logging speed.
    pub drive_test: bool,
    /// Test: stand beside the nearest vehicle looking at it (orientation checks).
    pub vehicle_look: bool,
    /// Test: start a road patrol on the nearest route (even close by) and log it.
    pub patrol_test: bool,
}

impl Args {
    /// Screenshot and scripted test runs (which start straight into play).
    pub fn automated(&self) -> bool {
        self.screenshot.is_some() || self.scripted_test()
    }

    /// A scripted test (walking, firing, driving...), which plays without
    /// the story.
    pub fn scripted_test(&self) -> bool {
        self.walk_secs > 0.0
            || self.fire_secs > 0.0
            || self.npc_test.is_some()
            || self.tour_secs > 0.0
            || self.outpost_test
            || self.animal_test.is_some()
            || self.drive_test
            || self.patrol_test
    }

    fn parse() -> Args {
        let mut a = Args {
            start: to_bevy([40.0, 2060.0, 180.0]),
            placed: false,
            yaw: 0.0,
            pitch: -0.25,
            time_of_day: 10.5,
            screenshot: None,
            settle: 4.0,
            start_weapon: None,
            noclip: false,
            underground: false,
            walk_secs: 0.0,
            fire_secs: 0.0,
            npc_test: None,
            show_menu: false,
            tour_secs: 0.0,
            novsync: false,
            continue_save: false,
            open_map: false,
            open_options: false,
            outpost_test: false,
            takedown_test: false,
            open_journal: false,
            open_shop: None,
            animal_test: None,
            drive_test: false,
            vehicle_look: false,
            patrol_test: false,
            original_shaders: true,
        };
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < argv.len() {
            let next = argv.get(i + 1).cloned().unwrap_or_default();
            match argv[i].as_str() {
                "--pos" => {
                    let v: Vec<f32> = next.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if v.len() == 3 {
                        a.start = to_bevy([v[0], v[1], v[2]]);
                        a.placed = true;
                    }
                    i += 1;
                }
                "--yaw" => {
                    a.yaw = next.parse::<f32>().unwrap_or(0.0).to_radians();
                    i += 1;
                }
                "--pitch" => {
                    a.pitch = next.parse::<f32>().unwrap_or(0.0).to_radians();
                    i += 1;
                }
                "--time" => {
                    a.time_of_day = next.parse().unwrap_or(10.5);
                    i += 1;
                }
                "--screenshot" => {
                    a.screenshot = Some(next);
                    i += 1;
                }
                "--settle" => {
                    a.settle = next.parse().unwrap_or(4.0);
                    i += 1;
                }
                "--noclip" | "--fly" => a.noclip = true,
                "--underground" => a.underground = true,
                "--menu" => a.show_menu = true,
                "--novsync" => a.novsync = true,
                "--continue" => a.continue_save = true,
                "--original-shaders" => a.original_shaders = true,
                "--standard-shaders" => a.original_shaders = false,
                "--open-map" => a.open_map = true,
                "--outpost-test" => a.outpost_test = true,
                "--journal" => a.open_journal = true,
                "--shop" => {
                    a.open_shop = next.parse().ok();
                    i += 1;
                }
                "--drive-test" => a.drive_test = true,
                "--weapon" => {
                    a.start_weapon = Some(next.clone());
                    i += 1;
                }
                "--vehicle-look" => {
                    a.drive_test = true;
                    a.vehicle_look = true;
                }
                "--patrol-test" => a.patrol_test = true,
                "--animal-test" => {
                    a.animal_test = Some(next.clone());
                    i += 1;
                }
                "--takedown-test" => {
                    a.takedown_test = true;
                    a.npc_test.get_or_insert_with(|| "idle".into());
                }
                "--options" => {
                    a.open_options = true;
                    a.show_menu = true;
                }
                "--tour" => {
                    a.tour_secs = next.parse().unwrap_or(120.0);
                    a.noclip = true;
                    i += 1;
                }
                "--npc-test" => {
                    a.npc_test = Some(next.clone());
                    i += 1;
                }
                "--fire-test" => {
                    a.fire_secs = next.parse().unwrap_or(6.0);
                    i += 1;
                }
                "--walk" => {
                    a.walk_secs = next.parse().unwrap_or(5.0);
                    i += 1;
                }
                other => eprintln!("ignoring unknown argument {other}"),
            }
            i += 1;
        }
        a
    }
}

fn main() {
    let args = Args::parse();
    let root = fc3_formats::cache::default_cache_root();
    let info = CacheInfo::load(root.clone()).ok();
    let mut app = App::new();
    match &info {
        Some(info) => {
            app.insert_resource(info.clone());
        }
        // First launch (or an outdated cache): convert in the background.
        None => match fc3_formats::find_install() {
            Some(data) => {
                app.insert_resource(setup::start_conversion(data, root.clone(), setup::filelist_dir()));
            }
            None => {
                app.insert_resource(setup::SetupError(
                    "Far Cry 3 data could not be found.\nSet the FC3_DATA environment variable to the game's install\n(or its data_win32 folder) and restart."
                        .into(),
                ));
            }
        },
    }
    // A command aimed at an entity that streaming already removed is harmless:
    // log it rather than crash the game.
    app.set_error_handler(bevy::ecs::error::warn);
    app.register_asset_source(cache_assets::SOURCE, AssetSourceBuilder::platform_default(&root.to_string_lossy(), None));
    app.register_asset_source(cache_assets::SKINNED_SOURCE, AssetSourceBuilder::platform_default(&root.to_string_lossy(), None));
    app.register_asset_source(original_shaders::RAW_SOURCE, AssetSourceBuilder::platform_default(&root.to_string_lossy(), None));
    app.insert_resource(args.clone())
        .add_plugins(
            DefaultPlugins.set(bevy::log::LogPlugin { custom_layer: if cfg!(feature = "profile") { profile::layer } else { |_| None }, ..default() }).set(
                WindowPlugin {
                    primary_window: Some(Window {
                        title: "Far Cry 3 — Bevy".into(),
                        // FC3_RESOLUTION=WxH (tests): the window's size.
                        resolution: std::env::var("FC3_RESOLUTION")
                            .ok()
                            .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))))
                            .unwrap_or((1600, 900))
                            .into(),
                        present_mode: if args.novsync { PresentMode::AutoNoVsync } else { PresentMode::AutoVsync },
                        ..default()
                    }),
                    ..default()
                },
            ),
        )
        .add_plugins((
            cache_assets::CacheAssetsPlugin,
            terrain::TerrainPlugin,
            objects::ObjectsPlugin,
            sky::SkyPlugin,
            physics::WorldPhysicsPlugin,
            player::PlayerPlugin,
            camera::ScreenshotPlugin,
        ))
        .add_plugins((
            combat::CombatPlugin,
            weapons::WeaponsPlugin,
            hud::HudPlugin,
            characters::CharactersPlugin,
            npc::NpcPlugin,
            menu::MenuPlugin,
            sound::SoundPlugin,
            vegetation::VegetationPlugin,
            pickups::PickupsPlugin,
        ))
        .add_plugins((
            setup::SetupPlugin,
            save::SavePlugin,
            original_shaders::OriginalShadersPlugin,
            map::MapPlugin,
            settings::SettingsPlugin,
            controls::ControlsPlugin,
            options::OptionsPlugin,
            outposts::OutpostsPlugin,
            stealth::StealthPlugin,
            nav::NavPlugin,
            progression::ProgressionPlugin,
            wildlife::WildlifePlugin,
            water::WaterPlugin,
            vehicles::VehiclesPlugin,
            clouds::CloudsPlugin,
        ))
        .add_plugins((
            guard_spots::GuardSpotsPlugin,
            lens_flare::LensFlarePlugin,
            surfaces::SurfacesPlugin,
            player_audio::PlayerAudioPlugin,
            weapon_wheel::WeaponWheelPlugin,
            light_shafts::LightShaftsPlugin,
            splines::SplinesPlugin,
            fish::FishPlugin,
            horizon::HorizonPlugin,
            vista::VistaPlugin,
            far_terrain::FarTerrainPlugin,
            shadow_lod::ShadowLodPlugin,
            retire::RetirePlugin,
            sky_probe::SkyProbePlugin,
            mesh_slabs::MeshSlabsPlugin,
        ))
        .add_plugins((
            grade::GradePlugin,
            dof::DofPlugin,
            bloom::GameBloomPlugin,
            upload_budget::UploadBudgetPlugin,
            story::StoryPlugin,
            mission_layers::MissionLayersPlugin,
            story_actors::StoryActorsPlugin,
            cinema::CinemaPlugin,
            music::MusicPlugin,
            video::VideoPlugin,
            qte::QtePlugin,
            faces::FacesPlugin,
        ))
        .add_plugins((
            lamps::LampsPlugin,
            particles::ParticlesPlugin,
            metal::MetalPlugin,
            fire::FirePlugin,
            traversal::TraversalPlugin,
            ui_nav::UiNavPlugin,
            patrols::PatrolsPlugin,
            syringes::SyringesPlugin,
            fp::FpPlugin,
            freefall::FreefallPlugin,
            explosives::ExplosivesPlugin,
            turrets::TurretsPlugin,
            villagers::VillagersPlugin,
            birds::BirdsPlugin,
            world_fx::WorldFxPlugin,
        ))
        .insert_resource(original_shaders::OriginalShaders { enabled: args.original_shaders })
        .add_systems(Startup, setup);
    if cfg!(feature = "profile") {
        app.add_systems(Last, profile::report);
    }
    app.add_systems(Update, throttle_uploads);
    // FC3_GPU_TIMING (profiling): logs each render pass's CPU and GPU time.
    if std::env::var("FC3_GPU_TIMING").is_ok() {
        app.add_plugins((
            bevy::render::diagnostic::RenderDiagnosticsPlugin,
            bevy::diagnostic::LogDiagnosticsPlugin { wait_duration: std::time::Duration::from_secs(10), ..default() },
        ));
    }
    app.run();
}

/// Texture data uploaded to the GPU per frame (MB). Bevy's limit counts
/// meshes too, but its mesh allocator has written them by then (see
/// `upload_budget` for theirs).
const UPLOAD_MB_PER_FRAME: usize = 24;

/// Sends textures to the GPU a slice per frame once play is under
/// way: a sector's worth at once held up frames by tens of milliseconds while
/// driving, and a slice spread over several frames only delays when a far
/// texture sharpens. Not while the world first loads: Bevy's atmosphere
/// expects its environment map on the GPU the frame it's made.
/// FC3_UPLOAD_MB=megabytes per frame (0: no limit).
fn throttle_uploads(
    time: Res<Time<Real>>,
    cameras: Query<(), With<player::PlayerCamera>>,
    mut since: Local<Option<f32>>,
    mut limit: ResMut<bevy::render::render_asset::RenderAssetBytesPerFrame>,
) {
    if limit.max_bytes.is_some() || cameras.is_empty() {
        return;
    }
    let now = time.elapsed_secs();
    if now - *since.get_or_insert(now) < 10.0 {
        return;
    }
    let mb = std::env::var("FC3_UPLOAD_MB").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(UPLOAD_MB_PER_FRAME);
    if mb > 0 {
        limit.max_bytes = Some(mb << 20);
    }
}

fn setup(mut commands: Commands, args: Res<Args>, mut tod: ResMut<sky::TimeOfDay>) {
    tod.hours = args.time_of_day;
    // The clock runs in play; automated runs keep a fixed time for repeatable shots.
    tod.speed = if args.automated() { 0.0 } else { sky::CLOCK_SPEED };
    let _ = &mut commands;
}
