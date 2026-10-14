//! The original game's interface, its own Flash movies (`fc3_ui`, see
//! docs/ui.md), drawn over the game: Ruffle plays them on Bevy's GPU and
//! draws them each frame into an image a full-window UI node shows.
//!
//! The image is Bevy's; once the render world has made its texture, the
//! texture comes back to the main world and Ruffle draws into it through a
//! plain RGBA view (`fc3_ui::SharedTarget`). The UI shows it through the
//! same view with `original_ui.wgsl`, which undoes Ruffle's premultiplied
//! alpha and decodes its sRGB colours. The player lives in the main world
//! (it isn't `Send`); its draws go on Bevy's queue between frames.
//!
//! Built with the `original-ui` feature; `FC3_ORIGINAL_UI=0` leaves it off.
//! The main HUD starts as the engine starts it: loaded, then told the game
//! mode (`SetGameMode SPHud`), enabled and shown. Then the game drives its
//! clips by DPS id, as the engine does ([`Hud`]): the health slots round the
//! radar, the potion count, the ammo strip of the weapon in hand; the
//! game's own minimap moves into the radar disc (its placeholder hidden),
//! and the game's own health, ammo and compass hide (`hud::OriginalHud`).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{AsBindGroup, TextureFormat, TextureUsages, TextureViewDescriptor};
use bevy::render::renderer::{RenderAdapter, RenderDevice, RenderInstance, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::shader::ShaderRef;
use bevy::ui_render::prelude::{MaterialNode, UiMaterial, UiMaterialPlugin};
use bevy::window::PrimaryWindow;
use fc3_ui::wgpu;

pub struct OriginalUiPlugin;

impl Plugin for OriginalUiPlugin {
    fn build(&self, app: &mut App) {
        if std::env::var("FC3_ORIGINAL_UI").is_ok_and(|v| v == "0") {
            return;
        }
        bevy::asset::embedded_asset!(app, "original_ui.wgsl");
        app.add_plugins((ExtractResourcePlugin::<Canvas>::default(), UiMaterialPlugin::<CanvasMaterial>::default()))
            .insert_non_send(Interface::Waiting)
            .init_resource::<UiCalls>()
            .add_systems(Startup, make_canvas)
            .add_systems(
                Update,
                (
                    drive_hud,
                    drive_story,
                    drive_tutorial,
                    drive_timer,
                    drive_xp,
                    drive_loot,
                    drive_stealth,
                    drive_hits,
                    drive_grenades,
                    drive_radar,
                    drive_potions,
                    drive_camera,
                    drive_pause,
                    run_interface,
                )
                    .chain()
                    .after(crate::hud::HudSet)
                    .run_if(resource_exists::<Canvas>),
            );
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.add_systems(Render, hand_back_texture.in_set(RenderSystems::PrepareResources));
        }
    }

    fn finish(&self, app: &mut App) {
        // (Only the render world has the instance; Ruffle's state wants it.)
        let instance = app.get_sub_app(RenderApp).and_then(|r| r.world().get_resource::<RenderInstance>()).map(|i| (**i.0).clone());
        if let Some(instance) = instance {
            app.insert_resource(GpuInstance(instance));
        }
    }
}

#[derive(Resource)]
struct GpuInstance(wgpu::Instance);

/// Shows the canvas (`original_ui.wgsl`).
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
struct CanvasMaterial {
    #[texture(0)]
    #[sampler(1)]
    canvas: Handle<Image>,
}

impl UiMaterial for CanvasMaterial {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("original_ui.wgsl")).with_source("embedded").into()
    }
}

/// The image the interface is drawn into, and its texture once the render
/// world has made it.
#[derive(Resource, Clone, ExtractResource)]
struct Canvas {
    image: Handle<Image>,
    texture: Arc<Mutex<Option<wgpu::Texture>>>,
}

/// The interface player, once its texture is there (or why it isn't). It
/// isn't `Send`: a main-thread resource.
enum Interface {
    Waiting,
    Running(fc3_ui::Host),
    Failed,
}

fn make_canvas(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<CanvasMaterial>>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let Ok(window) = windows.single() else { return };
    let (w, h) = (window.physical_width().max(1), window.physical_height().max(1));
    let mut image = Image::new_target_texture(w, h, TextureFormat::Rgba8UnormSrgb, None);
    image.texture_descriptor.usage |= TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    image.texture_descriptor.view_formats = &[TextureFormat::Rgba8Unorm];
    // (Read as stored: the shader decodes.)
    image.texture_view_descriptor = Some(TextureViewDescriptor { format: Some(TextureFormat::Rgba8Unorm), ..default() });
    let handle = images.add(image);
    commands.spawn((
        MaterialNode(materials.add(CanvasMaterial { canvas: handle.clone() })),
        Node { width: Val::Percent(100.0), height: Val::Percent(100.0), position_type: PositionType::Absolute, ..default() },
        // (Over the game's own HUD.)
        GlobalZIndex(50),
        Pickable::IGNORE,
    ));
    commands.insert_resource(Canvas { image: handle, texture: Arc::new(Mutex::new(None)) });
    info!("original UI: drawing at {w}x{h}");
}

/// Hands the canvas's texture to the main world once Bevy has made it.
fn hand_back_texture(canvas: Option<Res<Canvas>>, images: Res<RenderAssets<GpuImage>>) {
    let Some(canvas) = canvas else { return };
    let Ok(mut slot) = canvas.texture.lock() else { return };
    if slot.is_none() {
        if let Some(gpu) = images.get(&canvas.image) {
            *slot = Some((*gpu.texture).clone());
        }
    }
}

/// Starts the player once the texture is there, then runs and draws it each
/// frame.
#[allow(clippy::too_many_arguments)]
/// What the movies asked of the engine last frame, for the systems that
/// answer (`drive_pause`).
#[derive(Resource, Default)]
struct UiCalls(Vec<fc3_ui::EngineCall>);

/// (The interface keeps the real clock: its menus run with the game paused.)
fn run_interface(
    mut state: NonSendMut<Interface>,
    mut calls: ResMut<UiCalls>,
    canvas: Res<Canvas>,
    time: Res<Time<Real>>,
    info: Res<crate::data::CacheInfo>,
    instance: Option<Res<GpuInstance>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    adapter: Res<RenderAdapter>,
) {
    let state = &mut *state;
    if let Interface::Waiting = state {
        let Some(texture) = canvas.texture.lock().ok().and_then(|t| t.clone()) else { return };
        let Some(instance) = instance else {
            warn!("original UI: no wgpu instance to share");
            *state = Interface::Failed;
            return;
        };
        let size = (texture.width(), texture.height());
        let descriptors = fc3_ui::shared_descriptors(instance.0.clone(), (**adapter.0).clone(), device.wgpu_device().clone(), (**queue.0).clone());
        let host = fc3_ui::shared_renderer(descriptors, texture).and_then(|r| fc3_ui::Host::new(r, &info.root, size));
        *state = match host {
            Ok(mut host) => {
                start_hud(&mut host, size);
                info!("original UI: running");
                Interface::Running(host)
            }
            Err(e) => {
                warn!("original UI: {e:#}");
                Interface::Failed
            }
        };
    }
    let Interface::Running(host) = state else { return };
    host.run(time.delta_secs_f64());
    for call in host.take_calls() {
        debug!("original UI: engine call {} {:?}", call.command, call.args);
        calls.0.push(call);
    }
    host.render();
}

/// The pause menu's movie (a page manager) and its level, over the HUD's.
const PAUSE_MOVIE: &str = r"UI\Singleplayer\Menu\Pause\M_PauseMenu.feu";
const PAUSE_LEVEL: u32 = 10;

/// The original pause menu, run as the engine ran it (docs/ui.md): loaded
/// over the HUD (hidden under it) as the game pauses, told to show its pause
/// page, given what that page asks for (the unlocked entries, the
/// objective, money and rucksack), the keyboard and the mouse; what is
/// chosen there resumes, goes to the journal, reloads the checkpoint or
/// quits. Pages it has no data for yet stay ours.
#[allow(clippy::too_many_arguments)]
fn drive_pause(
    mut state: NonSendMut<Interface>,
    game: Res<State<crate::menu::GameState>>,
    mut next: ResMut<NextState<crate::menu::GameState>>,
    mut original: ResMut<crate::menu::OriginalPause>,
    mut calls: ResMut<UiCalls>,
    (ui, progress, crafting, time): (Res<crate::story::StoryUi>, Res<crate::progression::Progress>, Option<Res<crate::crafting::CraftCatalog>>, Res<Time>),
    story: Option<NonSendMut<crate::story::StoryHost>>,
    (mut keys, mut moves, mut buttons): (
        MessageReader<bevy::input::keyboard::KeyboardInput>,
        MessageReader<CursorMoved>,
        MessageReader<bevy::input::mouse::MouseButtonInput>,
    ),
    mut windows: Query<(&Window, &mut bevy::window::CursorOptions), With<PrimaryWindow>>,
    mut exit: MessageWriter<AppExit>,
    (mut open, mut mouse): (Local<bool>, Local<Vec2>),
) {
    use crate::menu::GameState;
    use fc3_ui::input::{MouseButton, PlayerEvent};
    let Interface::Running(host) = &mut *state else { return };
    let paused = *game.get() == GameState::Paused;
    if paused != *open {
        *open = paused;
        original.0 = paused;
        keys.clear();
        moves.clear();
        buttons.clear();
        calls.0.clear();
        if paused {
            host.answer("ShowGamePadInterface", "0");
            host.answer("IsSplitscreen", "0");
            host.answer("IsInvertActivateCancel", "0");
            host.event("SetVisible", &["0"]);
            host.load(PAUSE_MOVIE, PAUSE_LEVEL);
            // (It draws its own cursor.)
            if let Ok((_, mut cursor)) = windows.single_mut() {
                cursor.visible = false;
            }
        } else {
            host.unload(PAUSE_LEVEL);
            host.event("SetVisible", &["1"]);
        }
        return;
    }
    if !*open {
        calls.0.clear();
        return;
    }
    for call in std::mem::take(&mut calls.0) {
        if call.command != "Send" {
            continue;
        }
        let arg = |i: usize| call.args.get(i).map(String::as_str).unwrap_or("");
        match arg(0) {
            "RequestPageToLoad" => host.event("SetPageToLoad", &["-1"]),
            "SinglePlayerMenuLoaded" => {
                let mission = story.as_ref().is_some_and(|s| s.story.missions().iter().any(|m| m.1 == "Running"));
                // crafting, skills, save, checkpoint, restart, abandon, debug
                host.event("SetUnlockedMenus", &["1", "1", "1", if mission { "1" } else { "0" }, "0", "0", "0", "", ""]);
                let money = format!("{}/{}", progress.money, progress.money_cap());
                let capacity = crafting.as_ref().map_or(16, |c| c.loot_capacity(&progress));
                let rucksack = format!("{}/{}", crate::crafting::loot_count(&progress), capacity);
                let title = ui.mission.clone().unwrap_or_default();
                let objective = ui.objective.clone().unwrap_or_default();
                host.event("SetObjective", &[&title, &objective, &money, &rucksack]);
                host.event("NewItems", &["0", "0", "0"]);
            }
            "Resume" | "QuitMenu" => next.set(GameState::Playing),
            "Quit" | "QuitToDesktop" => {
                exit.write(AppExit::Success);
            }
            // Skills (3) and crafting (8) are our journal's for now.
            "RequestPageChange" if matches!(arg(1), "3" | "8") => next.set(GameState::Journal),
            // 0 last checkpoint, 4 save (done on pausing), 6 crafting, 7 skills.
            "MissionOptionSelected" => match arg(1) {
                "0" => {
                    if let Some(mut story) = story {
                        story.reload_checkpoint(time.elapsed_secs());
                    }
                    next.set(GameState::Playing);
                    return;
                }
                "6" | "7" => next.set(GameState::Journal),
                _ => {}
            },
            other => debug!("original UI: pause menu {other} {:?} not handled yet", call.args.get(1..)),
        }
    }
    let scale = windows.single().map_or(1.0, |(w, _)| w.scale_factor()) as f64;
    for m in moves.read() {
        *mouse = m.position;
        host.input(PlayerEvent::MouseMove { x: m.position.x as f64 * scale, y: m.position.y as f64 * scale });
    }
    for b in buttons.read() {
        let button = match b.button {
            bevy::input::mouse::MouseButton::Left => MouseButton::Left,
            bevy::input::mouse::MouseButton::Right => MouseButton::Right,
            bevy::input::mouse::MouseButton::Middle => MouseButton::Middle,
            _ => continue,
        };
        let (x, y) = (mouse.x as f64 * scale, mouse.y as f64 * scale);
        host.input(if b.state.is_pressed() { PlayerEvent::MouseDown { x, y, button, index: None } } else { PlayerEvent::MouseUp { x, y, button } });
    }
    for k in keys.read() {
        let Some(key) = movie_key(k.key_code) else { continue };
        host.input(if k.state.is_pressed() { PlayerEvent::KeyDown { key } } else { PlayerEvent::KeyUp { key } });
    }
}

/// A key as the movies see it (Ruffle gives them Flash's key codes).
fn movie_key(code: KeyCode) -> Option<fc3_ui::input::KeyDescriptor> {
    use fc3_ui::input::{KeyDescriptor, KeyLocation, LogicalKey, NamedKey, PhysicalKey as P};
    let named = |p: P, n: NamedKey, location: KeyLocation| Some(KeyDescriptor { physical_key: p, logical_key: LogicalKey::Named(n), key_location: location });
    let character = |p: P, c: char| Some(KeyDescriptor { physical_key: p, logical_key: LogicalKey::Character(c), key_location: KeyLocation::Standard });
    let s = KeyLocation::Standard;
    match code {
        KeyCode::ArrowUp => named(P::ArrowUp, NamedKey::ArrowUp, s),
        KeyCode::ArrowDown => named(P::ArrowDown, NamedKey::ArrowDown, s),
        KeyCode::ArrowLeft => named(P::ArrowLeft, NamedKey::ArrowLeft, s),
        KeyCode::ArrowRight => named(P::ArrowRight, NamedKey::ArrowRight, s),
        KeyCode::Enter => named(P::Enter, NamedKey::Enter, s),
        KeyCode::NumpadEnter => named(P::NumpadEnter, NamedKey::Enter, KeyLocation::Numpad),
        KeyCode::Escape => named(P::Escape, NamedKey::Escape, s),
        KeyCode::Backspace => named(P::Backspace, NamedKey::Backspace, s),
        KeyCode::Tab => named(P::Tab, NamedKey::Tab, s),
        KeyCode::Delete => named(P::Delete, NamedKey::Delete, s),
        KeyCode::Home => named(P::Home, NamedKey::Home, s),
        KeyCode::End => named(P::End, NamedKey::End, s),
        KeyCode::PageUp => named(P::PageUp, NamedKey::PageUp, s),
        KeyCode::PageDown => named(P::PageDown, NamedKey::PageDown, s),
        KeyCode::ShiftLeft => named(P::ShiftLeft, NamedKey::Shift, KeyLocation::Left),
        KeyCode::ShiftRight => named(P::ShiftRight, NamedKey::Shift, KeyLocation::Right),
        KeyCode::ControlLeft => named(P::ControlLeft, NamedKey::Control, KeyLocation::Left),
        KeyCode::ControlRight => named(P::ControlRight, NamedKey::Control, KeyLocation::Right),
        KeyCode::Space => character(P::Space, ' '),
        _ => {
            const LETTERS: [(KeyCode, P); 26] = [
                (KeyCode::KeyA, P::KeyA),
                (KeyCode::KeyB, P::KeyB),
                (KeyCode::KeyC, P::KeyC),
                (KeyCode::KeyD, P::KeyD),
                (KeyCode::KeyE, P::KeyE),
                (KeyCode::KeyF, P::KeyF),
                (KeyCode::KeyG, P::KeyG),
                (KeyCode::KeyH, P::KeyH),
                (KeyCode::KeyI, P::KeyI),
                (KeyCode::KeyJ, P::KeyJ),
                (KeyCode::KeyK, P::KeyK),
                (KeyCode::KeyL, P::KeyL),
                (KeyCode::KeyM, P::KeyM),
                (KeyCode::KeyN, P::KeyN),
                (KeyCode::KeyO, P::KeyO),
                (KeyCode::KeyP, P::KeyP),
                (KeyCode::KeyQ, P::KeyQ),
                (KeyCode::KeyR, P::KeyR),
                (KeyCode::KeyS, P::KeyS),
                (KeyCode::KeyT, P::KeyT),
                (KeyCode::KeyU, P::KeyU),
                (KeyCode::KeyV, P::KeyV),
                (KeyCode::KeyW, P::KeyW),
                (KeyCode::KeyX, P::KeyX),
                (KeyCode::KeyY, P::KeyY),
                (KeyCode::KeyZ, P::KeyZ),
            ];
            const DIGITS: [(KeyCode, P); 10] = [
                (KeyCode::Digit0, P::Digit0),
                (KeyCode::Digit1, P::Digit1),
                (KeyCode::Digit2, P::Digit2),
                (KeyCode::Digit3, P::Digit3),
                (KeyCode::Digit4, P::Digit4),
                (KeyCode::Digit5, P::Digit5),
                (KeyCode::Digit6, P::Digit6),
                (KeyCode::Digit7, P::Digit7),
                (KeyCode::Digit8, P::Digit8),
                (KeyCode::Digit9, P::Digit9),
            ];
            if let Some(i) = LETTERS.iter().position(|(k, _)| *k == code) {
                return character(LETTERS[i].1, (b'a' + i as u8) as char);
            }
            let i = DIGITS.iter().position(|(k, _)| *k == code)?;
            character(DIGITS[i].1, (b'0' + i as u8) as char)
        }
    }
}

/// The main HUD, as the engine starts it.
fn start_hud(host: &mut fc3_ui::Host, (_w, h): (u32, u32)) {
    host.answer("IsHighDefinition", if h >= 1080 { "1" } else { "0" });
    host.answer("GetLanguage", "english");
    host.answer("Platform", "PC");
    host.run_frame();
    host.load(r"UI\Singleplayer\Hud\MainHud\MainHud.feu", 1);
    for _ in 0..3 {
        host.run_frame();
    }
    host.event("SetGameMode", &["SPHud"]);
    for _ in 0..3 {
        host.run_frame();
    }
    host.event("EnableHud", &["1"]);
    host.event("SetVisible", &["1"]);
    host.load(TUTORIAL_MOVIE, TUTORIAL_LEVEL);
}

/// What the HUD shows now, as last sent (only changes are sent).
#[derive(Default, PartialEq, Clone)]
struct Hud {
    bars: usize,
    slots: [u32; 10],
    potions: u32,
    /// The ammo strip shown, its background and foreground frames and
    /// whether rounds are left (a row each for the machine gun's), and the
    /// reserve.
    strip: Option<usize>,
    strip_frames: Vec<(u32, u32, bool)>,
    reserve: String,
    throwables: u32,
    /// The throwable shown (`ThrowableIcon`'s frame: `THROWABLE_FRAMES`).
    throwable: u32,
    weapon_hud: bool,
    /// Shown at all (not in menus, scenes, missions' no-HUD stretches or
    /// death).
    shown: bool,
    radar: Option<(Vec2, f32)>,
    /// The reticule's bars' distance from the centre (stage units, rounded),
    /// none when it's down; the hit marker.
    reticule: Option<i32>,
    hit: bool,
    /// The armour arc's frame (1 empty .. `ARMOUR_FRAMES` full) while some
    /// is worn.
    armour: Option<u32>,
}

/// The throwable beside the ammo (`ThrowableIcon`, its count in
/// `ThrowableCounter`): the one last thrown or laid, a grenade until then.
/// Its frames: 1 none, then molotov, grenade, C4, mine.
const THROWABLE_FRAMES: [(crate::controls::Action, u32); 4] =
    [(crate::controls::Action::Molotov, 2), (crate::controls::Action::Grenade, 3), (crate::controls::Action::C4, 4), (crate::controls::Action::Mine, 5)];

/// The armour arc round the health (`ArmorBar`, over `ArmorBarBG`): frames
/// from empty to full.
const ARMOUR_FRAMES: u32 = 61;

/// The reticule's bars' centres sit this far out at rest (stage units).
const RETICULE_REST: f32 = 18.0;

/// The ammo strips (`AmmoType<n>`, with `AmmoBGType<n>` behind): frames in
/// each, as the movie reports them (`AmmoLoaded`).
const STRIP_FRAMES: [u32; 7] = [20, 16, 80, 20, 50, 7, 4];
/// The machine gun's strip: rows of rounds.
const LMG_STRIP: usize = 7;
const LMG_ROWS: usize = 4;
const LMG_ROW: u32 = 50;
/// The fuel gauge (no rounds: a level).
const FUEL_STRIP: usize = 4;

/// The strip a weapon's ammo shows on (the movie's art: pistol rounds,
/// rifle rounds, tall sniper rounds, a fuel gauge, shells, launcher
/// grenades, the machine gun's rows).
fn strip_of(class: crate::weapons::WeaponClass) -> Option<usize> {
    use crate::weapons::WeaponClass::*;
    Some(match class {
        Pistol => 1,
        Smg | Rifle => 2,
        Sniper => 3,
        Flamethrower => FUEL_STRIP,
        Shotgun => 5,
        GrenadeLauncher => 6,
        Lmg => LMG_STRIP,
        _ => return None,
    })
}

/// A strip of `total` frames showing `capacity` slots with `rounds` left:
/// frame 1 is every slot, each later frame one fewer (background:
/// capacity, foreground: rounds; no foreground when empty).
fn strip_frames(total: u32, capacity: u32, rounds: u32) -> (u32, u32, bool) {
    let frame = |n: u32| (total + 1).saturating_sub(n.min(total)).clamp(1, total);
    (frame(capacity), frame(rounds), rounds > 0)
}

/// Sends the game's state to the HUD's clips.
fn drive_hud(
    state: NonSend<Interface>,
    mut last: Local<Option<Hud>>,
    players: Query<
        (&crate::combat::Health, &crate::combat::PlayerStats, Option<&crate::weapons::Inventory>, &crate::player::Player),
        With<crate::player::Player>,
    >,
    (game_state, hud_state): (Res<State<crate::menu::GameState>>, Res<crate::hud::HudState>),
    (scripted, cinema): (Res<crate::story::ScriptedPlayer>, Res<crate::cinema::Cinema>),
    windows: Query<&Window, With<PrimaryWindow>>,
    mut original: ResMut<crate::hud::OriginalHud>,
    mut placement: ResMut<crate::map::MinimapPlacement>,
    ui_scale: Res<UiScale>,
    (progress, mut armour_full): (Res<crate::progression::Progress>, Local<f32>),
    (actions, mut thrown): (Res<crate::controls::ActionState>, Local<Option<crate::controls::Action>>),
) {
    let Interface::Running(host) = &*state else { return };
    if !original.0 {
        original.0 = true;
    }
    let Ok((health, stats, inventory, player)) = players.single() else { return };
    let mut hud = last.clone().unwrap_or_default();
    // Health: a slot a segment, round the radar, each filling over its
    // frames (1 empty, 30 full).
    let bars = ((health.max / crate::combat::HEALTH_SEGMENT).round() as usize).clamp(1, 10);
    let per = health.max / bars as f32;
    hud.bars = bars;
    for i in 0..10 {
        let fill = ((health.current - i as f32 * per) / per).clamp(0.0, 1.0);
        hud.slots[i] = if i < bars { 1 + (fill * 29.0).round() as u32 } else { 0 };
    }
    hud.potions = stats.medkits;
    // Armour: what is left of the most worn since it was put on.
    *armour_full = if progress.armour > 0.0 { armour_full.max(progress.armour) } else { 0.0 };
    hud.armour = (progress.armour > 0.0)
        .then(|| 1 + (progress.armour / *armour_full * (ARMOUR_FRAMES - 1) as f32).round().clamp(0.0, (ARMOUR_FRAMES - 1) as f32) as u32);
    hud.shown = *game_state.get() == crate::menu::GameState::Playing && !scripted.hud_hidden && cinema.view.is_none() && !health.dead;
    // Ammo: the strip of the weapon in hand.
    hud.strip = None;
    hud.strip_frames.clear();
    hud.reserve.clear();
    hud.weapon_hud = false;
    if let Some(inv) = inventory.filter(|i| !i.defs.is_empty() && !i.slots.is_empty()) {
        let (def, slot) = (inv.def(), inv.slot());
        if let Some((a, _)) = THROWABLE_FRAMES.iter().find(|(a, _)| actions.just_pressed(*a)) {
            *thrown = Some(*a);
        }
        let kind = thrown.unwrap_or(crate::controls::Action::Grenade);
        hud.throwables = match kind {
            crate::controls::Action::Molotov => inv.molotovs,
            crate::controls::Action::C4 => inv.c4,
            crate::controls::Action::Mine => inv.mines,
            _ => inv.grenades,
        };
        hud.throwable = THROWABLE_FRAMES.iter().find(|(a, _)| *a == kind).map_or(1, |f| f.1);
        hud.strip = strip_of(def.class);
        // (No panel for a blade or a tool.)
        hud.weapon_hud = hud.strip.is_some();
        match hud.strip {
            Some(LMG_STRIP) => {
                for row in 0..LMG_ROWS as u32 {
                    let cap = def.magazine.saturating_sub(row * LMG_ROW).min(LMG_ROW);
                    let left = slot.mag.saturating_sub(row * LMG_ROW).min(LMG_ROW);
                    let (bg, fg, on) = strip_frames(LMG_ROW, cap, left);
                    hud.strip_frames.push((bg, fg, on && cap > 0));
                }
            }
            Some(FUEL_STRIP) => {
                let total = STRIP_FRAMES[FUEL_STRIP];
                let level = (slot.mag as f32 / def.magazine.max(1) as f32 * total as f32).round() as u32;
                hud.strip_frames.push(strip_frames(total, total, level));
            }
            Some(n) => hud.strip_frames.push(strip_frames(STRIP_FRAMES[n], def.magazine, slot.mag)),
            None => {}
        }
        if hud.strip.is_some() {
            hud.reserve = slot.reserve.to_string();
        }
        // The reticule opens as the game's crosshair does (its gap is in UI
        // pixels at 1080 lines; the stage has 720).
        let in_menu = *game_state.get() != crate::menu::GameState::Playing;
        // (The camera has its own cursor.)
        hud.reticule = crate::hud::crosshair_gap(inv, player, health, in_menu)
            .filter(|_| !scripted.camera_up)
            .map(|gap| (RETICULE_REST + (gap - 6.0) * fc3_ui::STAGE.1 as f32 / 1080.0).round() as i32);
    } else {
        hud.reticule = None;
    }
    hud.hit = hud_state.hit_timer > 0.0;
    // The radar disc on the window, for the game's own minimap.
    if let (Ok(window), Some([x0, y0, x1, y1])) = (windows.single(), host.dps_bounds("MaskMC")) {
        let (sw, sh) = (fc3_ui::STAGE.0 as f32, fc3_ui::STAGE.1 as f32);
        let scale = (window.width() / sw).min(window.height() / sh);
        let offset = Vec2::new(window.width() - sw * scale, window.height() - sh * scale) / 2.0;
        let centre = offset + Vec2::new((x0 + x1) as f32, (y0 + y1) as f32) / 2.0 * scale;
        // (In the UI's own pixels.)
        let to_ui = |v: Vec2| crate::ui_nav::viewport_to_ui(v, ui_scale.0);
        hud.radar = Some((to_ui(centre), to_ui(Vec2::splat((x1 - x0) as f32 * scale)).x));
    }
    let first = last.is_none();
    let old = last.clone().unwrap_or_default();
    if !first && old == hud {
        return;
    }
    // (Until every clip has taken what it was sent, all is sent again: the
    // movies load over a few frames.)
    let mut ok = true;
    let num = |n: u32| fc3_ui::ExternalValue::Number(n as f64);
    if first || old.bars != hud.bars {
        ok &= host.dps_frame("HealthBarBG", num(hud.bars as u32));
        ok &= host.dps_frame("HealthBarOutline", num(hud.bars as u32));
    }
    for i in 0..10 {
        if first || old.slots[i] != hud.slots[i] {
            let id = format!("HealthBarSlot{i}");
            ok &= host.dps_visible(&id, hud.slots[i] > 0);
            if hud.slots[i] > 0 {
                ok &= host.dps_frame(&id, num(hud.slots[i]));
            }
        }
    }
    if first || old.potions != hud.potions {
        ok &= host.dps_text("PotionCount", &hud.potions.to_string());
    }
    if first {
        // (The radar's placeholder gives way to the minimap, under the
        // canvas: where the engine drew its map over the radar's dark
        // backing, the disc erases the backing instead, its own layer
        // keeping the erasing to the radar.)
        ok &= host.dps_set_parent("MaskMC", "blendMode", fc3_ui::ExternalValue::String("layer".into()));
        ok &= host.dps_set("MaskMC", "blendMode", fc3_ui::ExternalValue::String("erase".into()));
    }
    if first || old.armour != hud.armour {
        ok &= host.dps_visible("ArmorBarBG", hud.armour.is_some());
        ok &= host.dps_visible("ArmorBar", hud.armour.is_some());
        if let Some(frame) = hud.armour {
            ok &= host.dps_frame("ArmorBar", num(frame));
        }
    }
    if first || old.strip != hud.strip || old.strip_frames != hud.strip_frames {
        for n in 0..STRIP_FRAMES.len() {
            let on = hud.strip == Some(n);
            let frames = hud.strip_frames.first().copied().filter(|_| on);
            ok &= host.dps_visible(&format!("AmmoType{n}"), frames.is_some_and(|f| f.2));
            ok &= host.dps_visible(&format!("AmmoBGType{n}"), on);
            if let Some((bg, fg, _)) = frames {
                ok &= host.dps_frame(&format!("AmmoBGType{n}"), num(bg));
                ok &= host.dps_frame(&format!("AmmoType{n}"), num(fg));
            }
        }
        let lmg = hud.strip == Some(LMG_STRIP);
        ok &= host.dps_visible("AmmoType7", lmg);
        ok &= host.dps_visible("AmmoBGType7", lmg);
        for row in 0..LMG_ROWS {
            let frames = hud.strip_frames.get(row).copied().filter(|_| lmg);
            ok &= host.dps_visible(&format!("AmmoLMG{row}"), frames.is_some_and(|f| f.2));
            ok &= host.dps_visible(&format!("AmmoBGLMG{row}"), frames.is_some());
            if let Some((bg, fg, _)) = frames {
                ok &= host.dps_frame(&format!("AmmoBGLMG{row}"), num(bg));
                ok &= host.dps_frame(&format!("AmmoLMG{row}"), num(fg));
            }
        }
    }
    if first || old.reserve != hud.reserve {
        ok &= host.dps_text("AmmoBarText", &hud.reserve);
    }
    if first || old.throwables != hud.throwables {
        ok &= host.dps_text("ThrowableCounter", &hud.throwables.to_string());
    }
    if first || old.throwable != hud.throwable {
        ok &= host.dps_frame("ThrowableIcon", num(hud.throwable.max(1)));
    }
    if first || old.shown != hud.shown {
        host.event("SetVisible", &[if hud.shown { "1" } else { "0" }]);
    }
    if first || old.weapon_hud != hud.weapon_hud {
        host.event("WeaponHudEvent", &["SetWeaponHudVisibility", "", if hud.weapon_hud { "1" } else { "0" }]);
    }
    if first || old.reticule != hud.reticule {
        let at = hud.reticule.unwrap_or(RETICULE_REST as i32);
        for (id, x, y) in [("reticuleLeft", -at, 0), ("reticuleRight", at, 0), ("reticuleUp", 0, -at), ("reticuleDown", 0, at)] {
            ok &= host.dps_visible(id, hud.reticule.is_some());
            ok &= host.dps_set(id, "_x", fc3_ui::ExternalValue::Number(x as f64));
            ok &= host.dps_set(id, "_y", fc3_ui::ExternalValue::Number(y as f64));
        }
    }
    if first || old.hit != hud.hit {
        ok &= host.dps_visible("reticuleHitEnemy", hud.hit);
    }
    if old.radar != hud.radar {
        debug!("original UI: radar disc at {:?} (window {:?})", hud.radar, windows.single().map(|w| (w.width(), w.height(), w.scale_factor())).ok());
        placement.0 = hud.radar;
    }
    if ok {
        *last = Some(hud);
    }
}

/// `ObjContainer`'s frame with the box open (1 is closed).
const OBJECTIVE_BOX_OPEN: u32 = 8;
/// `ObjTextBox` plays a new objective in from here (it stops shown).
const OBJECTIVE_IN: u32 = 2;

/// The story's texts on the HUD: the objective in its box (with the
/// mission's name over it; a new one plays in), subtitles, the interaction
/// prompt, and the mission cards (its title as it starts, complete, failed:
/// the objective movie's `SetMissionStart` and `SetMissionTitle`).
#[allow(clippy::type_complexity)]
fn drive_story(
    state: NonSend<Interface>,
    mut ui: ResMut<crate::story::StoryUi>,
    settings: Res<crate::settings::Settings>,
    hud: Res<crate::hud::HudState>,
    mut last: Local<Option<(Option<String>, Option<String>, Option<String>, Option<String>)>>,
) {
    let Interface::Running(host) = &*state else { return };
    for card in std::mem::take(&mut ui.cards) {
        match card {
            crate::story::MissionCard::Start { title } => host.event("SetMissionStart", &[&title]),
            crate::story::MissionCard::Complete { title } => host.event("SetMissionTitle", &[&title, "MISSION COMPLETE", "1"]),
            crate::story::MissionCard::Failed { text } => host.event("SetMissionTitle", &[&text, "MISSION FAILED", "0"]),
        }
    }
    let subtitle = ui.subtitle.as_ref().filter(|_| settings.subtitles).map(|s| s.0.clone());
    let now = (ui.objective.clone(), ui.mission.clone(), subtitle, hud.shown_prompt.clone());
    let first = last.is_none();
    let old = last.clone().unwrap_or_default();
    if !first && old == now {
        return;
    }
    let mut ok = true;
    if first || old.0 != now.0 || old.1 != now.1 {
        let frame = if now.0.is_some() { OBJECTIVE_BOX_OPEN } else { 1 };
        ok &= host.dps_frame("ObjContainer", fc3_ui::ExternalValue::Number(frame as f64));
        if let Some(text) = &now.0 {
            ok &= host.dps_text("ObjText", text);
            ok &= host.dps_text("MissionName", now.1.as_deref().unwrap_or(""));
            if old.0 != now.0 {
                ok &= host.dps_play("ObjTextBox", fc3_ui::ExternalValue::Number(OBJECTIVE_IN as f64));
            }
        }
    }
    if first || old.3 != now.3 {
        ok &= host.dps_visible("InteractionMC", now.3.is_some());
        ok &= host.dps_text("InteractionText", now.3.as_deref().unwrap_or(""));
    }
    if first || old.2 != now.2 {
        match &now.2 {
            Some(text) => host.event("ShowSubTitle", &[text]),
            None => host.event("HideSubTitle", &[]),
        }
    }
    if ok {
        *last = Some(now);
    }
}

/// The XP kicker's tiers (`XPKicker`): the frame each plays from and the
/// frame it holds on (after which it runs on to its "off" label), and the
/// least XP that earns each.
const XP_TIERS: [(u32, u32, u32); 3] = [(2, 37, 0), (42, 87, 100), (95, 155, 500)];
/// Seconds the kicker holds before it goes.
const XP_HOLD: f32 = 2.5;

/// XP gains on the HUD: the kicker plays the tier the amount earns, with
/// the amount and what it was for, holds, then goes.
fn drive_xp(state: NonSend<Interface>, time: Res<Time>, mut gains: MessageReader<crate::progression::XpGain>, mut showing: Local<Option<(f32, u32)>>) {
    let Interface::Running(host) = &*state else {
        gains.clear();
        return;
    };
    let mut total = 0;
    let mut reason = "";
    for g in gains.read() {
        total += g.amount;
        reason = g.reason;
    }
    if total > 0 {
        let (start, hold, _) = XP_TIERS.iter().rev().find(|t| total >= t.2).copied().unwrap_or(XP_TIERS[0]);
        host.dps_text("XPKickerText", &format!("{total} XP"));
        host.dps_text("XPBonusKickerText", &reason.to_uppercase());
        host.dps_play("XPKicker", fc3_ui::ExternalValue::Number(start as f64));
        *showing = Some((time.elapsed_secs() + XP_HOLD, hold));
    }
    if let Some((until, hold)) = *showing {
        if time.elapsed_secs() >= until {
            host.dps_play("XPKicker", fc3_ui::ExternalValue::Number((hold + 1) as f64));
            *showing = None;
        }
    }
}

/// The cash counter's frames (`CashContainer`): shown from 2 (it holds at
/// 9), gone from 10; a change's kicker (`CashUpdateAnim<n>`) plays a gain
/// from 2, a loss from 42. Loot lines (`LootContainer<n>`) play from 2 and
/// fade by themselves.
const CASH_SHOW: u32 = 2;
const CASH_HIDE: u32 = 10;
const CASH_GAIN: u32 = 2;
const CASH_LOSS: u32 = 42;
const LOOT_IN: u32 = 2;
/// Seconds the cash counter stays after a change.
const CASH_HOLD: f32 = 3.0;

/// What was looted, for its line.
fn loot_name(kind: crate::progression::LootKind) -> String {
    match kind {
        crate::progression::LootKind::Skin(a) => format!("{} Skin", a.name()),
        crate::progression::LootKind::Plant(p) => p.name().to_string(),
        crate::progression::LootKind::Item(_) => "Item".to_string(),
    }
}

/// Loot and money on the HUD: a line for each thing looted (the two lines
/// in turn), and the cash counter with what changed.
#[allow(clippy::type_complexity)]
fn drive_loot(
    state: NonSend<Interface>,
    time: Res<Time>,
    progress: Res<crate::progression::Progress>,
    mut looted: MessageReader<crate::progression::ItemLooted>,
    mut memory: Local<(Option<u32>, usize, usize, Option<f32>)>,
) {
    let Interface::Running(host) = &*state else {
        looted.clear();
        return;
    };
    let num = |n: u32| fc3_ui::ExternalValue::Number(n as f64);
    let (last_money, loot_line, cash_line, cash_until) = &mut *memory;
    for l in looted.read() {
        let n = *loot_line % 2;
        *loot_line += 1;
        host.dps_text(&format!("LootItemText_{n}"), &loot_name(l.0));
        host.dps_play(&format!("LootContainer_{n}"), num(LOOT_IN));
    }
    let money = progress.money;
    match *last_money {
        None => *last_money = Some(money),
        Some(before) if before != money => {
            let n = *cash_line % 2;
            *cash_line += 1;
            let (sign, delta, from) = if money > before { ('+', money - before, CASH_GAIN) } else { ('-', before - money, CASH_LOSS) };
            host.dps_text("CashText", &money.to_string());
            host.dps_text(&format!("CashUpdateText_{n}"), &format!("{sign}{delta}"));
            host.dps_play(&format!("CashUpdateAnim_{n}"), num(from));
            if cash_until.is_none() {
                host.dps_play("CashContainer", num(CASH_SHOW));
            }
            *cash_until = Some(time.elapsed_secs() + CASH_HOLD);
            *last_money = Some(money);
        }
        _ => {}
    }
    if cash_until.is_some_and(|t| time.elapsed_secs() >= t) {
        host.dps_play("CashContainer", num(CASH_HIDE));
        *cash_until = None;
    }
}

/// The stealth indicators (`StealthIndicator<n>`, three): each turns round
/// the screen's centre toward an enemy noticing the player, its
/// `StealthIndicatorAnim<n>` filling over 100 frames as the enemy's
/// suspicion grows and its `StealthIndicatorState<n>` white (frame 1) while
/// it does, red (frame 8) once the player is spotted.
const STEALTH_SLOTS: usize = 3;
const STEALTH_SUSPICIOUS: u32 = 1;
const STEALTH_SPOTTED: u32 = 8;

/// The indicators as last sent: per slot, its angle (degrees, rounded),
/// fill frame and state frame.
type Indicators = [Option<(i32, u32, u32)>; STEALTH_SLOTS];

fn drive_stealth(
    state: NonSend<Interface>,
    players: Query<(&GlobalTransform, &crate::player::Player)>,
    npcs: Query<(&GlobalTransform, &crate::npc::Npc, &crate::combat::Health)>,
    mut last: Local<Option<Indicators>>,
) {
    let Interface::Running(host) = &*state else { return };
    let Ok((at, player)) = players.single() else { return };
    let forward = Vec3::new(-player.yaw.sin(), 0.0, -player.yaw.cos());
    let right = Vec3::new(-forward.z, 0.0, forward.x);
    // The enemies noticing the player (not busy with rebels), most first.
    let mut noticing: Vec<(f32, Vec3, bool)> = npcs
        .iter()
        .filter(|(_, n, h)| !h.dead && n.faction.hostile_to_player() && n.foe.is_none())
        .filter_map(|(t, n, _)| {
            let spotted = n.state == crate::npc::NpcState::Combat && n.sees_player;
            (spotted || n.awareness > 0.02).then(|| (if spotted { 2.0 } else { n.awareness }, t.translation(), spotted))
        })
        .collect();
    noticing.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut now: Indicators = [None; STEALTH_SLOTS];
    for (slot, (awareness, pos, spotted)) in noticing.into_iter().take(STEALTH_SLOTS).enumerate() {
        let to = (pos - at.translation()).with_y(0.0);
        let angle = to.dot(right).atan2(to.dot(forward)).to_degrees().round() as i32;
        let fill = 1 + (awareness.clamp(0.0, 1.0) * 99.0).round() as u32;
        now[slot] = Some((angle, fill, if spotted { STEALTH_SPOTTED } else { STEALTH_SUSPICIOUS }));
    }
    if last.as_ref() == Some(&now) {
        return;
    }
    let old = last.unwrap_or([None; STEALTH_SLOTS]);
    let first = last.is_none();
    let mut ok = true;
    for slot in 0..STEALTH_SLOTS {
        if !first && old[slot] == now[slot] {
            continue;
        }
        ok &= host.dps_visible(&format!("StealthIndicator_{slot}"), now[slot].is_some());
        if let Some((angle, fill, state)) = now[slot] {
            ok &= host.dps_set(&format!("StealthIndicator_{slot}"), "_rotation", fc3_ui::ExternalValue::Number(angle as f64));
            ok &= host.dps_frame(&format!("StealthIndicatorAnim_{slot}"), fc3_ui::ExternalValue::Number(fill as f64));
            if old[slot].is_none_or(|o| o.2 != state) {
                ok &= host.dps_frame(&format!("StealthIndicatorState_{slot}"), fc3_ui::ExternalValue::Number(state as f64));
            }
        }
    }
    if ok {
        *last = Some(now);
    }
}

/// The camera's viewfinder (`H_Camera`), over the HUD while the camera is
/// up: its magnification (`CameraZoomLevel`, frames 1 to 42 for 1.0x to
/// 7.5x), its cursor (`CameraCursor`: 1 searching, 2 on something to tag or
/// take in) and its shutter (`ShutterAnim`: frame 2 blacks the view out a
/// moment as a picture is taken, frame 1 is open). Its legend reads the
/// game's strings (`fire.utils.Localizer`).
const CAMERA_MOVIE: &str = r"UI\Singleplayer\Hud\Camera\H_Camera.feu";
const CAMERA_LEVEL: u32 = 2;
const CAMERA_ZOOM_FRAMES: u32 = 42;
const CAMERA_ON_TARGET: u32 = 2;
const SHUTTER_SECS: f32 = 0.12;

/// The viewfinder as last sent (`ready` once its clips have answered).
#[derive(Default, PartialEq, Clone, Copy)]
struct Viewfinder {
    ready: bool,
    zoom: u32,
    cursor: u32,
    shutter: bool,
}

fn drive_camera(
    state: NonSend<Interface>,
    time: Res<Time>,
    bino: Res<crate::stealth::Binoculars>,
    scripted: Res<crate::story::ScriptedPlayer>,
    mut saw: MessageReader<crate::stealth::CameraSaw>,
    mut shown: Local<Option<Viewfinder>>,
    mut shutter: Local<f32>,
) {
    let Interface::Running(host) = &*state else {
        saw.clear();
        return;
    };
    if saw.read().count() > 0 {
        *shutter = SHUTTER_SECS;
    }
    *shutter = (*shutter - time.delta_secs()).max(0.0);
    if !scripted.camera_up {
        if shown.take().is_some() {
            host.unload(CAMERA_LEVEL);
        }
        return;
    }
    if shown.is_none() {
        host.load(CAMERA_MOVIE, CAMERA_LEVEL);
    }
    let old = shown.unwrap_or_default();
    let [least, most] = crate::stealth::CAMERA_ZOOM;
    let zoom = (bino.zoom - least) / (most - least) * (CAMERA_ZOOM_FRAMES - 1) as f32;
    let now = Viewfinder {
        ready: true,
        zoom: 1 + zoom.round().clamp(0.0, (CAMERA_ZOOM_FRAMES - 1) as f32) as u32,
        cursor: if bino.on_target { CAMERA_ON_TARGET } else { 1 },
        shutter: *shutter > 0.0,
    };
    if old == now {
        return;
    }
    let number = |n: u32| fc3_ui::ExternalValue::Number(n as f64);
    let mut ok = true;
    if !old.ready || old.zoom != now.zoom {
        ok &= host.dps_frame("CameraZoomLevel", number(now.zoom));
    }
    if !old.ready || old.cursor != now.cursor {
        ok &= host.dps_frame("CameraCursor", number(now.cursor));
    }
    if !old.ready || old.shutter != now.shutter {
        ok &= host.dps_frame("ShutterAnim", number(if now.shutter { 2 } else { 1 }));
    }
    // (Until its clips answer, the movie is still loading: all again.)
    *shown = Some(if ok { now } else { old });
}

/// The tutorial box (`H_TutorialBox`, top left, its own movie at level 3):
/// a mission's hint (`HudText`; the newest when there are more) through the
/// movie's own functions, `setTutorial(title, text, prompt)` and
/// `showTutorial()` (it fades in), and `hideTutorial()` when it goes.
const TUTORIAL_MOVIE: &str = r"UI\Singleplayer\Hud\Tutorial\H_TutorialBox.feu";
const TUTORIAL_LEVEL: u32 = 3;

fn drive_tutorial(
    state: NonSend<Interface>,
    ui: Res<crate::story::StoryUi>,
    settings: Res<crate::settings::Settings>,
    actions: Res<crate::controls::ActionState>,
    mut shown: Local<Option<Option<(String, String)>>>,
) {
    let Interface::Running(host) = &*state else { return };
    let text = |s: &str| crate::story::prompt_text(s, &actions, &settings);
    let now = ui.hints.iter().max_by_key(|(id, _)| **id).map(|(_, (title, body))| (text(title), text(body)));
    if shown.as_ref() == Some(&now) {
        return;
    }
    // (Nothing to hide yet; and until the movie is in, nothing to call.)
    if shown.is_none() && now.is_none() {
        *shown = Some(None);
        return;
    }
    if host.dps_info("TutorialBoxContainerMC").is_empty() {
        return;
    }
    let call = |f: &str, args| host.call_function(&format!("_level{TUTORIAL_LEVEL}.{f}"), args);
    match &now {
        Some((title, body)) => {
            call("setTutorial", vec![fc3_ui::text_value(title), fc3_ui::text_value(body), fc3_ui::ExternalValue::Bool(false)]);
            call("showTutorial", vec![]);
        }
        None => {
            call("hideTutorial", vec![]);
        }
    }
    *shown = Some(now);
}

/// The hit indicators (`HitIndicator_<n>`, six arrows round the screen's
/// centre): each hit plays the next (from frame 2; it fades and turns
/// itself off), turned toward where the hit came from.
const HIT_SLOTS: usize = 6;
const HIT_PLAY: u32 = 2;

fn drive_hits(state: NonSend<Interface>, players: Query<(&crate::combat::PlayerStats, &crate::player::Player)>, mut memory: Local<(f32, usize)>) {
    let Interface::Running(host) = &*state else { return };
    let Ok((stats, player)) = players.single() else { return };
    let (last, next) = &mut *memory;
    let strength = stats.hurt_dir.map_or(0.0, |(_, s)| s);
    // (A fresh hit sets the strength back up; it fades between.)
    if let Some((dir, _)) = stats.hurt_dir.filter(|(_, s)| *s > *last + 1e-3) {
        let forward = Vec3::new(-player.yaw.sin(), 0.0, -player.yaw.cos());
        let right = Vec3::new(-forward.z, 0.0, forward.x);
        let flat = dir.with_y(0.0).normalize_or_zero();
        let angle = flat.dot(right).atan2(flat.dot(forward)).to_degrees();
        let id = format!("HitIndicator_{next}");
        host.dps_set(&id, "_rotation", fc3_ui::ExternalValue::Number(angle as f64));
        host.dps_play(&id, fc3_ui::ExternalValue::Number(HIT_PLAY as f64));
        *next = (*next + 1) % HIT_SLOTS;
    }
    *last = strength;
}

/// A mission's timer (`ObjectiveTimer`, top centre, with its
/// `ObjectiveTimerText`): the first of the story's timers, as the game's own
/// HUD writes it; hidden without one.
fn drive_timer(state: NonSend<Interface>, ui: Res<crate::story::StoryUi>, mut shown: Local<Option<Option<String>>>) {
    let Interface::Running(host) = &*state else { return };
    let now = ui.timers.first().map(crate::story::timer_text);
    if shown.as_ref() == Some(&now) {
        return;
    }
    let mut ok = host.dps_visible("ObjectiveTimer", now.is_some());
    if let Some(text) = &now {
        ok &= host.dps_text("ObjectiveTimerText", text);
    }
    if ok {
        *shown = Some(now);
    }
}

/// The grenade indicators (`GrenadeIndicator_<n>`, three, round the screen's
/// centre): the nearest live grenades within `hud::GRENADE_WARN`, each
/// turned toward its grenade.
const GRENADE_SLOTS: usize = 3;

/// The syringe running (`PotionContainer`, by the radar's right): its icon
/// (`PotionIcon`, a frame per syringe, coloured by its crafting category),
/// its name (`PotionName`, shown as it fades in) and the time left
/// (`PotionTimer`, m:ss). The one with the most time left shows; the box
/// fades in from `POTION_IN` as one starts and out from `POTION_OUT` once
/// none runs.
const POTION_IN: u32 = 2;
const POTION_OUT: u32 = 18;

/// A syringe's icon frame.
fn potion_frame(s: crate::syringes::Syringe) -> u32 {
    use crate::syringes::Syringe::*;
    match s {
        AnimalRepellent => 2,
        AnimalDamage => 3,
        WildlifeVision => 4,
        SprintBurst => 5,
        NeverDrown => 6,
        PlantVision => 7,
        Steady => 9,
        Fireproof => 10,
        EnemyVision => 11,
        Invincible => 13,
        OneHitKill => 15,
        HealthBoost => 16,
    }
}

fn drive_potions(
    state: NonSend<Interface>,
    boosts: Res<crate::syringes::Boosts>,
    crafting: Option<Res<crate::crafting::CraftCatalog>>,
    mut last: Local<Option<(u32, String)>>,
    mut shown: Local<Option<crate::syringes::Syringe>>,
) {
    use fc3_ui::ExternalValue::Number;
    let Interface::Running(host) = &*state else { return };
    let running = boosts.left.iter().filter(|(_, t)| **t > 0.0).max_by(|a, b| a.1.total_cmp(b.1)).map(|(s, t)| (*s, *t));
    let Some((syringe, left)) = running else {
        if shown.take().is_some() {
            host.dps_play("PotionContainer", Number(POTION_OUT as f64));
            *last = None;
        }
        return;
    };
    let mut ok = true;
    if *shown != Some(syringe) {
        let name = crafting
            .as_ref()
            .and_then(|c| c.recipes.iter().find(|r| crate::syringes::Syringe::of(&r.id) == Some(syringe)).map(|r| r.name.clone()))
            .unwrap_or_default();
        ok &= host.dps_visible("PotionContainer", true);
        ok &= host.dps_frame("PotionIcon", Number(potion_frame(syringe) as f64));
        ok &= host.dps_text("PotionName", &name);
        ok &= host.dps_play("PotionContainer", Number(POTION_IN as f64));
        if !ok {
            return;
        }
        *shown = Some(syringe);
    }
    let secs = left.ceil() as u32;
    let now = (potion_frame(syringe), format!("{}:{:02}", secs / 60, secs % 60));
    if last.as_ref() != Some(&now) && host.dps_text("PotionTimer", &now.1) {
        *last = Some(now);
    }
}

/// The radar's markers (in the minimap movie), placed as the engine places
/// them: in stage pixels from the disc's centre, turned with the player as
/// the map under them is, the disc's radius the map's reach
/// (`map::MinimapReach`). Each shows by its frame label: `Object_<n>` the
/// enemies the game's own minimap shows (tagged, or on to the player) and
/// vehicles free to take (`vehicle`, `boat`, `glider`); `LocMarker_<n>`
/// places (an outpost held, `OutpostLocked`, or taken, `FastTravel`; its
/// alarm boxes, `Alarm`, `AlarmOn` while it rings; a tower, `TowerLocked`
/// until climbed, then `Tower`; collectibles within the game's radius,
/// `Relic`, `Letter`, `Sdcard`); `ObjMarker_<n>` the objectives (`Inside`
/// the disc, else `Outside` on its rim, `Up` or `Down` well above or below);
/// and `NorthMarkerMC`, turned about the centre to the north.
const RADAR_RADIUS: f32 = 78.5;
const RADAR_OBJECTS: usize = 21;
const RADAR_PLACES: usize = 16;
const RADAR_OBJECTIVES: usize = 11;
/// Markers show within this share of the disc's radius; objectives further
/// are held there.
const RADAR_EDGE: f32 = 0.9;
/// An objective this far (m) above or below has an arrow.
const RADAR_HEIGHT: f32 = 8.0;

/// A marker as sent: stage pixels from the centre and its frame label.
type Marker = Option<(i32, i32, &'static str)>;

/// The radar as last sent.
#[derive(PartialEq, Clone)]
struct Radar {
    north: i32,
    objects: [Marker; RADAR_OBJECTS],
    places: [Marker; RADAR_PLACES],
    objectives: [Marker; RADAR_OBJECTIVES],
}

/// Sends a set of markers' changes (`prefix_<n>`): shown or hidden, moved,
/// their label (played from, for the ones that move).
fn send_markers(host: &fc3_ui::Host, prefix: &str, old: Option<&[Marker]>, now: &[Marker]) -> bool {
    use fc3_ui::ExternalValue::{Number, String as Text};
    let mut ok = true;
    for (i, marker) in now.iter().enumerate() {
        let was = old.map(|o| o[i]);
        if was == Some(*marker) {
            continue;
        }
        let id = format!("{prefix}_{i}");
        let was = was.flatten();
        if was.is_some() != marker.is_some() || old.is_none() {
            ok &= host.dps_visible(&id, marker.is_some());
        }
        let Some((x, y, label)) = *marker else { continue };
        if was.is_none_or(|w| (w.0, w.1) != (x, y)) {
            ok &= host.dps_set(&id, "_x", Number(x as f64));
            ok &= host.dps_set(&id, "_y", Number(y as f64));
        }
        if was.is_none_or(|w| w.2 != label) {
            ok &= if matches!(label, "Outside" | "Inside" | "AlarmOn") {
                host.dps_play(&id, Text(label.into()))
            } else {
                host.dps_frame(&id, Text(label.into()))
            };
        }
    }
    ok
}

#[allow(clippy::too_many_arguments)]
fn drive_radar(
    state: NonSend<Interface>,
    players: Query<(&Transform, &crate::player::Player)>,
    npcs: Query<(&GlobalTransform, &crate::npc::Npc, &crate::combat::Health, Has<crate::stealth::Tagged>)>,
    vehicles: Query<(Entity, &GlobalTransform, &crate::vehicles::Vehicle, Option<&crate::vehicles::Wear>)>,
    (reach, driving, objectives): (Res<crate::map::MinimapReach>, Res<crate::vehicles::Driving>, Res<crate::story::ObjectiveMarkers>),
    (outposts, ways, progress): (Res<crate::outposts::Outposts>, Res<crate::traversal::Ways>, Res<crate::progression::Progress>),
    (hud, pickups): (Res<crate::map::HudParams>, Query<(&GlobalTransform, &crate::pickups::Pickup)>),
    mut last: Local<Option<Radar>>,
) {
    let Interface::Running(host) = &*state else { return };
    let Ok((at, player)) = players.single() else { return };
    // The map turns with the player (heading clockwise from north).
    let heading = -player.yaw;
    let turn = Rot2::radians(heading).inverse();
    let local = |p: Vec3| {
        let d = p - at.translation;
        turn * Vec2::new(d.x, d.z) / reach.radius.max(1.0)
    };
    let on_disc = |l: Vec2| l.length() <= RADAR_EDGE;
    let marker = |l: Vec2, label: &'static str| {
        let px = l * RADAR_RADIUS;
        Some((px.x.round() as i32, px.y.round() as i32, label))
    };
    let fill = |mut found: Vec<(f32, Marker)>, slots: &mut [Marker]| {
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (slot, (_, m)) in slots.iter_mut().zip(found) {
            *slot = m;
        }
    };
    let mut now = Radar {
        north: (-heading.to_degrees()).round() as i32,
        objects: [None; RADAR_OBJECTS],
        places: [None; RADAR_PLACES],
        objectives: [None; RADAR_OBJECTIVES],
    };
    // Enemies and free vehicles, nearest first.
    let mut objects = Vec::new();
    for (t, npc, h, tagged) in &npcs {
        let aware = npc.awareness > 0.3 || matches!(npc.state, crate::npc::NpcState::Combat | crate::npc::NpcState::Search);
        let l = local(t.translation());
        if !h.dead && npc.faction.hostile_to_player() && (aware || tagged) && on_disc(l) {
            objects.push((l.length(), marker(l, "enemy")));
        }
    }
    for (e, t, v, wear) in &vehicles {
        let l = local(t.translation());
        if driving.vehicle == Some(e) || v.occupied() || wear.is_some_and(|w| w.wrecked) || !on_disc(l) {
            continue;
        }
        let label = match v.kind() {
            crate::vehicles::Kind::Car => "vehicle",
            crate::vehicles::Kind::Boat => "boat",
            crate::vehicles::Kind::Glider => "glider",
        };
        objects.push((l.length(), marker(l, label)));
    }
    fill(objects, &mut now.objects);
    // Places.
    let mut places = Vec::new();
    for o in &outposts.list {
        let l = local(o.centre);
        if on_disc(l) {
            places.push((l.length(), marker(l, if o.liberated { "FastTravel" } else { "OutpostLocked" })));
        }
        if !o.liberated {
            for (box_at, off) in o.alarms.iter().zip(&o.disabled) {
                let l = local(*box_at);
                if !off && on_disc(l) {
                    places.push((l.length(), marker(l, if o.alarm_since.is_some() { "AlarmOn" } else { "Alarm" })));
                }
            }
        }
    }
    for (i, tower) in ways.data.towers.iter().enumerate() {
        let l = local(Vec3::from(*tower));
        if on_disc(l) {
            places.push((l.length(), marker(l, if progress.towers.contains(&(i as u32)) { "Tower" } else { "TowerLocked" })));
        }
    }
    if hud.0.collectibles > 0.0 {
        for (t, pk) in &pickups {
            let label = match pk.kind {
                crate::pickups::PickupKind::Relic => "Relic",
                crate::pickups::PickupKind::Letter => "Letter",
                crate::pickups::PickupKind::MemoryCard => "Sdcard",
                _ => continue,
            };
            let d = t.translation() - at.translation;
            let l = local(t.translation());
            if Vec2::new(d.x, d.z).length() <= hud.0.collectibles && on_disc(l) {
                places.push((l.length(), marker(l, label)));
            }
        }
    }
    fill(places, &mut now.places);
    // Objectives: held on the rim when further than the disc shows.
    let mut marks = Vec::new();
    for &goal in &objectives.0 {
        let l = local(goal);
        let rise = goal.y - at.translation.y;
        let (l, label) = if !on_disc(l) {
            (l.normalize_or_zero() * RADAR_EDGE, "Outside")
        } else if rise > RADAR_HEIGHT {
            (l, "Up")
        } else if rise < -RADAR_HEIGHT {
            (l, "Down")
        } else {
            (l, "Inside")
        };
        marks.push((l.length(), marker(l, label)));
    }
    fill(marks, &mut now.objectives);
    if last.as_ref() == Some(&now) {
        return;
    }
    let old = last.as_ref();
    let mut ok = true;
    if old.is_none_or(|o| o.north != now.north) {
        ok &= host.dps_set("NorthMarkerMC", "_rotation", fc3_ui::ExternalValue::Number(now.north as f64));
    }
    ok &= send_markers(host, "Object", old.map(|o| &o.objects[..]), &now.objects);
    ok &= send_markers(host, "LocMarker", old.map(|o| &o.places[..]), &now.places);
    ok &= send_markers(host, "ObjMarker", old.map(|o| &o.objectives[..]), &now.objectives);
    let shown = |m: &[Marker]| m.iter().flatten().map(|m| m.2).collect::<Vec<_>>();
    debug!("radar: objects {:?}, places {:?}, objectives {:?} (sent {ok})", shown(&now.objects), shown(&now.places), shown(&now.objectives));
    if ok {
        *last = Some(now);
    }
}

fn drive_grenades(
    state: NonSend<Interface>,
    players: Query<(&Transform, &crate::player::Player)>,
    grenades: Query<(&crate::weapons::Projectile, &Transform), Without<crate::player::Player>>,
    mut last: Local<Option<[Option<i32>; GRENADE_SLOTS]>>,
) {
    let Interface::Running(host) = &*state else { return };
    let Ok((at, player)) = players.single() else { return };
    let mut near: Vec<(f32, Vec3)> = grenades
        .iter()
        .filter(|(g, _)| g.fuse.is_some())
        .map(|(_, t)| (t.translation.distance(at.translation), t.translation))
        .filter(|(d, _)| *d < crate::hud::GRENADE_WARN)
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    let forward = Vec3::new(-player.yaw.sin(), 0.0, -player.yaw.cos());
    let right = Vec3::new(-forward.z, 0.0, forward.x);
    let mut now = [None; GRENADE_SLOTS];
    for (slot, (_, pos)) in near.into_iter().take(GRENADE_SLOTS).enumerate() {
        let to = (pos - at.translation).with_y(0.0);
        now[slot] = Some(to.dot(right).atan2(to.dot(forward)).to_degrees().round() as i32);
    }
    if last.as_ref() == Some(&now) {
        return;
    }
    let mut ok = true;
    for (slot, angle) in now.iter().enumerate() {
        let id = format!("GrenadeIndicator_{slot}");
        ok &= host.dps_visible(&id, angle.is_some());
        if let Some(angle) = angle {
            ok &= host.dps_set(&id, "_rotation", fc3_ui::ExternalValue::Number(*angle as f64));
        }
    }
    if ok {
        *last = Some(now);
    }
}
