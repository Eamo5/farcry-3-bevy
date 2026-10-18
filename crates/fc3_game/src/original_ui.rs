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

mod frontend;
mod shop;

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
            .init_resource::<MapPage>()
            .init_resource::<shop::ShopPage>()
            .add_systems(Startup, make_canvas)
            .add_systems(
                Update,
                (
                    drive_hud,
                    drive_story,
                    drive_tutorial,
                    drive_timer,
                    drive_xp,
                    (drive_loot, drive_saving),
                    (drive_stealth, drive_indicators, drive_wheel),
                    drive_hits,
                    drive_grenades,
                    drive_radar,
                    drive_potions,
                    drive_camera,
                    drive_credits,
                    (drive_loading, frontend::drive_frontend),
                    drive_gallery,
                    drive_poker,
                    drive_knives,
                    (drive_races, drive_trials).chain(),
                    (drive_pause, drive_map_page, shop::drive_shop_page).chain(),
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
/// The HUD's level (`MainHud.feu`, and the pieces it loads).
const HUD_LEVEL: u32 = 1;

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
    (ui, mut progress, crafting, time, mut settings): (
        Res<crate::story::StoryUi>,
        ResMut<crate::progression::Progress>,
        Option<Res<crate::crafting::CraftCatalog>>,
        Res<Time>,
        ResMut<crate::settings::Settings>,
    ),
    (loot, perks, skills, handbook): (
        Option<Res<crate::progression::LootCatalog>>,
        Res<crate::progression::Perks>,
        Option<Res<crate::progression::Skills>>,
        Option<Res<crate::progression::HandbookData>>,
    ),
    story: Option<NonSendMut<crate::story::StoryHost>>,
    (mut keys, mut moves, mut buttons): (
        MessageReader<bevy::input::keyboard::KeyboardInput>,
        MessageReader<CursorMoved>,
        MessageReader<bevy::input::mouse::MouseButtonInput>,
    ),
    mut windows: Query<(&Window, &mut bevy::window::CursorOptions), With<PrimaryWindow>>,
    (mut exit, mut back_to_menu, mut save_request): (MessageWriter<AppExit>, ResMut<crate::save::BackToMenu>, MessageWriter<crate::save::SaveRequest>),
    (mut open, mut mouse, mut paged, mut pressed): (Local<bool>, Local<Vec2>, Local<bool>, Local<(Option<std::time::Instant>, usize)>),
    (mut players, mut crafted, mut page, mut boosts): (
        Query<&mut crate::combat::PlayerStats, With<crate::player::Player>>,
        MessageWriter<crate::crafting::ItemCrafted>,
        Local<CraftPage>,
        Option<ResMut<crate::syringes::Boosts>>,
    ),
    (mut map_page, outposts, mut trials, mut shop_page): (
        ResMut<MapPage>,
        Res<crate::outposts::Outposts>,
        ResMut<crate::trials::Trials>,
        ResMut<shop::ShopPage>,
    ),
) {
    use crate::menu::GameState;
    use fc3_ui::input::{MouseButton, PlayerEvent};
    let Interface::Running(host) = &mut *state else { return };
    // (The map key opens it at its world map page, as the game did, and a
    // shop at the shop page.)
    let paused = matches!(game.get(), GameState::Paused | GameState::Map | GameState::Shop);
    if paused != *open {
        *open = paused;
        original.0 = paused;
        // The HUD goes under the menu (its subtitles showed through).
        host.level_visible(HUD_LEVEL, !paused);
        keys.clear();
        moves.clear();
        buttons.clear();
        calls.0.clear();
        map_page.open = false;
        map_page.calls.clear();
        shop_page.calls.clear();
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
            // (A trial's board, result or pause page when a trial asked.)
            "RequestPageToLoad" => {
                // (A trial's page takes FC3_PAUSE_KEYS too, for tests.)
                *paged |= trials.page_wanted.is_some();
                let page = trials.page_wanted.take().unwrap_or(match game.get() {
                    GameState::Map => "2",
                    GameState::Shop => "0",
                    _ => "-1",
                });
                open_page(host, page);
            }
            "ChallengeBoardPageLoaded" | "ChallengeResultPageLoaded" => {
                if let Some(page) = &trials.page {
                    send_trial_page(host, page);
                }
            }
            "ChallengePausePageLoaded" => {
                let legend = trials.pause_legend();
                host.event("SetChallengeSetupLegendInfo", &[&legend[0], &legend[1], &legend[2], &legend[3]]);
            }
            // The trial's pages' choices: start it or leave the board, play
            // it again (0) or leave (2) from the result, give it up (1) or
            // go on (0) from its pause.
            "ChallengeBoardStart" | "ChallengeBoardQuit" | "ChallengeResultChoice" | "ChallengePauseAbort" => {
                use crate::trials::Choice;
                trials.choices.push(match (arg(0), arg(1)) {
                    ("ChallengeBoardStart", _) => Choice::Start,
                    ("ChallengeResultChoice", "0") => Choice::Again,
                    ("ChallengePauseAbort", "1") => Choice::Abort,
                    ("ChallengePauseAbort", _) => Choice::Resume,
                    _ => Choice::Leave,
                });
                trials.page = None;
                next.set(GameState::Playing);
            }
            "ChallengeShowGamerCard" => {}
            // The world map page's (`drive_map_page`).
            "MapLoaded" | "IconsLoaded" | "SetupMarker" | "onFastTravel" => map_page.calls.push(call.clone()),
            // The shop page's (`shop::drive_shop_page`).
            "ShopManagerLoaded"
            | "onRequestedMenu"
            | "OnBuy"
            | "SellLoot"
            | "OnSellAll"
            | "SortSell"
            | "OnWeaponEquip"
            | "OnEquipRequest"
            | "EquipAttachment"
            | "OnMeleeEquip"
            | "onSeen"
            | "onRequestWeapon3D"
            | "onRequestItem3D"
            | "onRequestAttachment3D"
            | "onRequestCategory3D" => shop_page.calls.push(call.clone()),
            "OnSwitchMode"
            | "LoadSoundTutorial"
            | "OnEnterTransitionEnd"
            | "OnCenterMap"
            | "OnUpdatingLevelOfZoomEnded"
            | "OnIconFocused"
            | "OnIconUnfocused"
            | "SinglePlayerMenuCheckConnectionStatus" => {}
            "SinglePlayerMenuLoaded" => {
                map_page.open = false;
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
                // FC3_PAUSE_PAGE=13 (tests): on to a page, as if chosen
                // (once: each page says the menu's loaded too); `10,5`
                // opens it where its launch arguments say (the handbook's
                // guide tab). Without it, FC3_PAUSE_KEYS go to this list.
                if !*paged {
                    *paged = true;
                    if let Ok(page) = std::env::var("FC3_PAUSE_PAGE") {
                        let mut args: Vec<&str> = page.split(',').collect();
                        args.resize(4, "-1");
                        host.event("SetPageToLoad", &args);
                    }
                }
            }
            "Resume" | "QuitMenu" => next.set(GameState::Playing),
            // "Quit" is to the main menu (the game starts again there), the
            // other to the desktop.
            "Quit" | "QuitToDesktop" => {
                back_to_menu.0 = arg(0) == "Quit";
                exit.write(AppExit::Success);
            }
            // The world map (2), skills (3), crafting (8), the handbook
            // (10), the options (12) and the rucksack (13) are the
            // original's, and back (-1) is the pause page.
            "RequestPageChange" if matches!(arg(1), "2" | "3" | "8" | "10" | "12" | "13" | "-1") => open_page(host, arg(1)),
            "OnOptionLoaded" => send_option_sections(host),
            "OnRequestOptions" => send_options(host, &settings, arg(1).parse().unwrap_or(0)),
            "SetOptions" => {
                let values: Vec<i32> = arg(1).split(',').map(|v| v.trim().parse().unwrap_or(0)).collect();
                set_options(&mut settings, arg(2).parse().unwrap_or(u32::MAX), &values);
            }
            "OnEncyclopediaRequest" => {
                if let Some(book) = handbook.as_deref().and_then(|b| b.0.as_ref()) {
                    let extra = crate::statistics::Extra {
                        recipes: crafting.as_ref().map_or(0, |c| c.recipes.iter().filter(|r| c.available(&progress, r)).count()),
                        outposts: outposts.liberated_ids().len(),
                    };
                    send_handbook(host, book, &progress, extra, arg(1).parse().unwrap_or(0), arg(2));
                }
            }
            // Crafting: its tabs, a tab's groups, a group's recipes, a
            // recipe's ingredients, and making the one last selected.
            "CraftingPageLoaded"
            | "CraftingRecipeTypeChanged"
            | "CraftingRecipeSubTypeChanged"
            | "CraftingRecipeSelected"
            | "CraftingRecipeCrafted"
            | "CraftingRecipeUsed" => {
                let (Some(catalog), Some(loot)) = (crafting.as_deref(), loot.as_deref()) else { continue };
                let mut stats = players.single_mut().ok();
                match call.args[0].as_str() {
                    "CraftingPageLoaded" => {
                        *page = CraftPage::default();
                        // The item's 3D views are the engine's drawing; their
                        // stand-ins are plain white and blue sheets.
                        for id in ["3D_Crafting_MC", "3D_CraftMirror_MC", "3D_Crafting_MC_B", "3D_Crafting_MC_C", "3D_Crafting_MC_D"] {
                            host.dps_visible(id, false);
                        }
                        let n = perks.arrow_cap.to_string();
                        host.event("BagSizes", &[&perks.medkit_cap.to_string(), &n, &n]);
                        send_craft_types(host, catalog);
                    }
                    "CraftingRecipeTypeChanged" => {
                        page.category = arg(1).parse().unwrap_or(0);
                        send_craft_groups(host, catalog, loot, &progress, stats.as_deref(), perks.medkit_cap, page.category);
                    }
                    "CraftingRecipeSubTypeChanged" => {
                        page.subtype = arg(2).parse().unwrap_or(0);
                        send_craft_recipes(host, catalog, loot, &progress, stats.as_deref(), perks.medkit_cap, &page);
                    }
                    "CraftingRecipeSelected" => {
                        page.recipe = arg(1).parse().ok().filter(|&i: &usize| i < catalog.recipes.len());
                        send_craft_ingredients(host, catalog, loot, &progress, &page);
                    }
                    // The selected syringe injected (`CraftingUseResult` 1, or 0).
                    "CraftingRecipeUsed" => {
                        let recipe = page.recipe.and_then(|i| catalog.recipes.get(i));
                        let used = match (recipe, boosts.as_deref_mut()) {
                            (Some(r), Some(b)) => crate::syringes::inject(&mut progress, b, r),
                            _ => false,
                        };
                        host.event("CraftingUseResult", &[if used { "1" } else { "0" }]);
                        send_craft_groups(host, catalog, loot, &progress, stats.as_deref(), perks.medkit_cap, page.category);
                        send_craft_recipes(host, catalog, loot, &progress, stats.as_deref(), perks.medkit_cap, &page);
                    }
                    _ => {
                        let Some(recipe) = page.recipe.and_then(|i| catalog.recipes.get(i)) else { continue };
                        let Some(stats) = stats.as_deref_mut() else { continue };
                        if let Some(event) = catalog.craft(&mut progress, stats, perks.medkit_cap, loot, &recipe.id) {
                            crafted.write(event);
                        }
                        send_craft_groups(host, catalog, loot, &progress, Some(stats), perks.medkit_cap, page.category);
                        send_craft_recipes(host, catalog, loot, &progress, Some(stats), perks.medkit_cap, &page);
                        send_craft_ingredients(host, catalog, loot, &progress, &page);
                    }
                }
            }
            "PlayerSkillsLoaded" => {
                if let Some(skills) = skills.as_deref() {
                    send_skills(host, &progress, skills, true);
                }
            }
            // A skill bought (tree, index in it): learned if it can be, and
            // the page told (`DisplaySkillTutorial`, 1 or 0) and refreshed.
            "BuyAbility" => {
                if let Some(skills) = skills.as_deref() {
                    let learned = buy_skill(&mut progress, skills, arg(1).parse().unwrap_or(usize::MAX), arg(2).parse().unwrap_or(usize::MAX));
                    host.event("DisplaySkillTutorial", &[if learned { "1" } else { "0" }, "", "", ""]);
                    send_skills(host, &progress, skills, false);
                }
            }
            "PlayerInventoryLoaded" => send_rucksack(host, &progress, crafting.as_deref(), loot.as_deref(), &perks),
            // Dropped from the rucksack: one of it goes.
            "RemoveLoot" => {
                if let (Ok(id), Some(loot)) = (arg(1).parse::<u32>(), loot.as_deref()) {
                    drop_loot(&mut progress, loot, id);
                    send_rucksack(host, &progress, crafting.as_deref(), Some(loot), &perks);
                }
            }
            // 0 last checkpoint, 4 save (done on pausing), 6 crafting, 7 skills.
            "MissionOptionSelected" => match arg(1) {
                "0" => {
                    if let Some(mut story) = story {
                        story.reload_checkpoint(time.elapsed_secs());
                    }
                    next.set(GameState::Playing);
                    return;
                }
                // (Saved as the menu opened too; this one is shown.)
                "4" => {
                    save_request.write(crate::save::SaveRequest);
                }
                "6" => open_page(host, "8"),
                "7" => open_page(host, "3"),
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
    // FC3_PAUSE_KEYS=Enter,Down,Enter (tests): keys pressed one by one,
    // 0.6 s apart, once FC3_PAUSE_PAGE's page has had two seconds (on the
    // wall clock: the game's is stopped under the menu); `Click@x:y` clicks
    // there (the window's pixels, as its screenshots have them).
    if let Some(list) = std::env::var("FC3_PAUSE_KEYS").ok().filter(|_| *paged) {
        let since = pressed.0.get_or_insert_with(std::time::Instant::now).elapsed().as_secs_f32();
        let names: Vec<&str> = list.split(',').collect();
        if since > 2.0 + 0.6 * pressed.1 as f32 {
            let click = names.get(pressed.1).and_then(|n| n.strip_prefix("Click@")).and_then(|at| at.split_once(':'));
            if let Some((x, y)) = click.and_then(|(x, y)| Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?))) {
                host.input(PlayerEvent::MouseMove { x, y });
                host.input(PlayerEvent::MouseDown { x, y, button: MouseButton::Left, index: None });
                host.input(PlayerEvent::MouseUp { x, y, button: MouseButton::Left });
            } else if let Some(key) = names.get(pressed.1).and_then(|n| test_key(n)).and_then(movie_key) {
                host.input(PlayerEvent::KeyDown { key: key.clone() });
                host.input(PlayerEvent::KeyUp { key });
            }
            pressed.1 += 1;
        }
    }
}

/// The world map page (`WorldMap.feu`, quick launch 2): the calls it made
/// this frame (`drive_pause` hands them on), whether it's up, its zoom, and
/// where each of its fast travel icons goes.
#[derive(Resource, Default)]
struct MapPage {
    calls: Vec<fc3_ui::EngineCall>,
    open: bool,
    /// The whole map's width on the stage (pixels).
    zoom: f32,
    /// Fast travel icons by their turn among them (the movie's
    /// `iTypeIndex`): an outpost (its place in `Outposts`) or a
    /// safehouse (its marker).
    travel: Vec<Travel>,
    /// Each icon's clip (`Icon_<index>`) and place (a fraction of the map
    /// each way), and the view they were last put in.
    icons: Vec<(String, Vec2)>,
    placed: Option<[f64; 4]>,
    /// The mouse (window pixels) and how far it has gone with the left
    /// button down: a drag pans the map.
    mouse: Option<Vec2>,
    dragged: f32,
    /// A test's drag, made once the map is in place (`FC3_MAP_PAN`).
    pan_test: Option<Vec2>,
}

/// How far (window pixels) the mouse goes with its button down before it
/// pans the map rather than clicks.
const MAP_DRAG: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Travel {
    Outpost(usize),
    Safehouse(u64),
}

/// The map's zoom, the whole map's width on the stage: from the whole of it
/// in view to 2 stage pixels a metre. It opens where the movie's own start
/// has it (0.7 of a tile's widest at the third level), and a wheel step
/// takes it a quarter in or out.
const MAP_ZOOM: (f32, f32) = (1024.0, 20480.0);
const MAP_ZOOM_START: f32 = 7168.0;
const MAP_ZOOM_STEP: f32 = 1.25;
/// The tiles a side at each level of zoom (`WorldMap.CMap.SetMapSize`), and
/// the widest a tile is shown before the next level's are loaded (stage
/// pixels: twice its own).
const MAP_LEVELS: [f32; 4] = [2.0, 5.0, 10.0, 20.0];
const MAP_TILE_WIDEST: f32 = 1024.0;

/// The level of zoom whose tiles show the map at a width.
fn map_level(zoom: f32) -> usize {
    MAP_LEVELS.iter().position(|n| zoom / n <= MAP_TILE_WIDEST).unwrap_or(MAP_LEVELS.len() - 1)
}

/// The legend's rows (`DataMap`): an icon type and its name (the game's
/// map legend strings). Each type the map shows needs its row: the movie
/// hides the icons of types without one.
const MAP_LEGEND: [(f64, u32); 31] = [
    (map_icon::PLAYER, 93073),
    (map_icon::MARKER, 93074),
    (map_icon::MISSION, 93075),
    (map_icon::MISSION_GIVER, 93076),
    (map_icon::TOWER_LOCKED, 93078),
    (map_icon::TOWER, 93078),
    (map_icon::OUTPOST_LOCKED, 93079),
    (map_icon::SAFEHOUSE, 93093),
    (map_icon::SAFEHOUSE_LOCKED, 93093),
    (map_icon::SHOP, 93094),
    (map_icon::KNIFE, 93099),
    (map_icon::POKER, 93100),
    (map_icon::RACING, 93101),
    (map_icon::SHOOTING, 93102),
    (map_icon::CHALLENGES, 93360),
    (map_icon::SUPPLY_DROP, 93361),
    (map_icon::WANTED, 93362),
    (map_icon::HUNTING, 93363),
    (map_icon::QUEST_GIVER, QUESTS),
    (map_icon::SIDE_QUEST, QUESTS),
    (map_icon::ULC_QUEST, QUESTS),
    (map_icon::DOG, 93101),
    // What the treasure maps show.
    (map_icon::RELIC, 93106),
    (map_icon::LETTER, 93108),
    (map_icon::SD_CARD, 93107),
    (map_icon::CHEST, 93105),
    (map_icon::PLANT_GREEN, 155384),
    (map_icon::PLANT_RED, 155385),
    (map_icon::PLANT_YELLOW, 155386),
    (map_icon::PLANT_WHITE, 155387),
    (map_icon::PLANT_BLUE, 93104),
];
/// The handbook's "quests" (the map's legend has no row of its own for
/// them).
const QUESTS: u32 = 159341;
/// The fog's cells a side (`WorldMap.CFog`): each is lifted as a whole.
const MAP_FOG_CELLS: usize = 10;

/// The icons' types (`WorldMap.CIconManager.E_*`).
mod map_icon {
    pub const PLAYER: f64 = 0.0;
    pub const MISSION: f64 = 1.0;
    pub const MISSION_GIVER: f64 = 3.0;
    pub const QUEST_GIVER: f64 = 4.0;
    pub const SHOP: f64 = 5.0;
    pub const SAFEHOUSE_LOCKED: f64 = 6.0;
    pub const SAFEHOUSE: f64 = 7.0;
    pub const TOWER_LOCKED: f64 = 8.0;
    pub const TOWER: f64 = 9.0;
    pub const DOG: f64 = 10.0;
    pub const KNIFE: f64 = 11.0;
    pub const MARKER: f64 = 20.0;
    pub const CHEST: f64 = 21.0;
    pub const LETTER: f64 = 32.0;
    pub const SD_CARD: f64 = 33.0;
    pub const RELIC: f64 = 34.0;
    pub const PLANT_GREEN: f64 = 57.0;
    pub const PLANT_BLUE: f64 = 58.0;
    pub const PLANT_RED: f64 = 59.0;
    pub const PLANT_YELLOW: f64 = 60.0;
    pub const PLANT_WHITE: f64 = 61.0;
    pub const POKER: f64 = 31.0;
    pub const SHOOTING: f64 = 36.0;
    pub const RACING: f64 = 37.0;
    pub const ULC_QUEST: f64 = 55.0;
    pub const SIDE_QUEST: f64 = 62.0;
    pub const HUNTING: f64 = 63.0;
    pub const SUPPLY_DROP: f64 = 64.0;
    pub const WANTED: f64 = 65.0;
    pub const CHALLENGES: f64 = 66.0;
    pub const OUTPOST_LOCKED: f64 = 70.0;
}

/// The story's map markers' types (`mapobjectivetype`) the map shows as they
/// stand, enabled in a layer that's in: activities, shops and the social
/// challenges' boards. The rest (quest and mission givers, objectives)
/// show when a script shows them.
const MARKER_ACTIVITY: u64 = 0x058d_6bbd;
const MARKER_SHOP: u64 = 0x1350_8ab1;
const MARKER_CHALLENGES: u64 = 0x0375_88d5;
/// A mission's brief: its giver.
const MARKER_BRIEF: u64 = 0x82ca_abbe;
const MARKER_QUEST_GIVER: u64 = 0x7d8e_4092;

/// A marker's icon on the map, by its sub-type (the activity, the kind of
/// quest) and then its type; none for a plain objective.
fn marker_icon(kind: u64, sub: u64) -> Option<f64> {
    let by_sub = match sub {
        7_657_466_763_891 => Some(map_icon::DOG),
        7_657_467_198_775 => Some(map_icon::POKER),
        7_658_667_062_452 => Some(map_icon::KNIFE),
        7_659_735_889_859 => Some(map_icon::SHOP),
        7_705_350_686_419 => Some(map_icon::SHOOTING),
        7_705_350_709_557 => Some(map_icon::RACING),
        7_711_661_993_104 => Some(map_icon::SIDE_QUEST),
        7_711_663_829_951 => Some(map_icon::HUNTING),
        7_711_663_837_270 => Some(map_icon::WANTED),
        7_711_663_843_368 => Some(map_icon::SUPPLY_DROP),
        7_711_664_117_448 => Some(map_icon::ULC_QUEST),
        _ => None,
    };
    by_sub.or(match kind {
        MARKER_SHOP => Some(map_icon::SHOP),
        MARKER_CHALLENGES => Some(map_icon::CHALLENGES),
        MARKER_BRIEF => Some(map_icon::MISSION_GIVER),
        MARKER_QUEST_GIVER => Some(map_icon::QUEST_GIVER),
        _ => None,
    })
}

/// The markers scripts show, as what they are (a quest's giver, a
/// mission's, else an objective: `map_icon::MISSION`), where they are now.
fn scripted_markers(host: &crate::story::StoryHost) -> Vec<(f64, Vec3)> {
    let data = host.story.data();
    host.story
        .objective_markers()
        .into_iter()
        .map(|(id, at)| {
            let kind = data.markers.get(&id).and_then(|m| marker_icon(m.kind, m.sub_kind));
            (kind.unwrap_or(map_icon::MISSION), crate::data::to_bevy(at))
        })
        .collect()
}

/// The markers standing in the layers that are in, enabled, that no script
/// shows: activities, shops, the challenge boards.
/// Each plant's radar icon (`plant_<colour>`), as its pickups name it
/// (`CPickupCollectible`'s `sMinimapIconName`: a star rhododendron's is
/// `plant_red`).
fn plant_icons(host: &crate::story::StoryHost) -> std::collections::HashMap<crate::syringes::Plant, &'static str> {
    let mut out = std::collections::HashMap::new();
    for e in host.story.data().entities.values() {
        let Some(plant) = e.name.strip_prefix("Pickups/Plants/").and_then(crate::syringes::Plant::from_name) else { continue };
        if out.contains_key(&plant) {
            continue;
        }
        let icon = e.component("CPickupCollectible").and_then(|c| c.str("sMinimapIconName"));
        let label = match icon.as_deref() {
            Some("plant_green") => "plant_green",
            Some("plant_blue") => "plant_blue",
            Some("plant_red") => "plant_red",
            Some("plant_yellow") => "plant_yellow",
            Some("plant_white") => "plant_white",
            _ => continue,
        };
        out.insert(plant, label);
    }
    out
}

fn standing_markers(host: &crate::story::StoryHost) -> Vec<(f64, Vec3)> {
    let data = host.story.data();
    let scripted: std::collections::HashSet<u64> = host.story.objective_markers().into_iter().map(|m| m.0).collect();
    data.markers
        .values()
        .filter(|m| m.enabled && [MARKER_ACTIVITY, MARKER_SHOP, MARKER_CHALLENGES].contains(&m.kind))
        .filter(|m| !scripted.contains(&m.id) && host.story.is_loaded(m.id))
        .filter_map(|m| marker_icon(m.kind, m.sub_kind).map(|kind| (kind, crate::data::to_bevy(m.pos))))
        .collect()
}

/// A map icon's marker on the radar (`LocMarker_<n>`'s frame label).
fn radar_label(icon: f64) -> Option<&'static str> {
    Some(match icon {
        map_icon::MISSION_GIVER => "MissionGiver",
        map_icon::QUEST_GIVER => "Quest",
        map_icon::SIDE_QUEST => "SideQuest",
        map_icon::HUNTING => "HuntingQuest",
        map_icon::SUPPLY_DROP => "SpeedQuest",
        map_icon::WANTED => "WantedQuest",
        map_icon::ULC_QUEST => "ULCLostExpeditions",
        map_icon::CHALLENGES => "ChallengeBoard",
        map_icon::SHOP => "Shop",
        map_icon::SAFEHOUSE => "FastTravel",
        map_icon::SAFEHOUSE_LOCKED => "SafehouseLocked",
        map_icon::POKER => "Poker",
        map_icon::KNIFE => "KnifeThrowing",
        map_icon::SHOOTING => "ShootingChallenge",
        map_icon::RACING => "RacingChallenge",
        _ => return None,
    })
}

/// The world map page, answered as the engine answered it: on `MapLoaded`
/// the legend (`DataMap`), the fog lifted where the player has been
/// (`SetFog 0 <row> <column>`), and the icons (`IconMap`: the player, the
/// objectives, radio towers, outposts, safehouses and liberated outposts to
/// travel to, and the waypoint, each placed as a fraction of the map); once
/// they are in (`IconsLoaded`) the map centred on the player (`CenterMap
/// <level> <zoom>`). The wheel zooms (`OnLevelOfZoomUpdated`, the movie
/// loading the level's tiles itself), a marker placed or taken away
/// (`SetupMarker`) is the waypoint, and a fast travel icon chosen
/// (`onFastTravel <its turn>`) travels there. The icons are the engine's to
/// place: as the map moves each goes where its fraction of the map is shown
/// (`(fraction + MapVisual's place / its size) * MapScale's size`, as the
/// movie places the waypoint it makes itself). Panning is the engine's too:
/// a drag with the left button moves the map (`MapVisual`, and the fog's
/// `FogVisualMC` with it, within the map's edges; the movie loads the tiles
/// coming into view), and the right button is the engine's
/// `FireMouseRightButton` (Flash has none), which sets or clears the
/// waypoint under the cursor.
#[allow(clippy::too_many_arguments)]
fn drive_map_page(
    mut state: NonSendMut<Interface>,
    mut page: ResMut<MapPage>,
    mut next: ResMut<NextState<crate::menu::GameState>>,
    mut players: Query<(&mut Transform, &mut crate::player::Player)>,
    (outposts, ways, progress, objectives): (
        Res<crate::outposts::Outposts>,
        Res<crate::traversal::Ways>,
        Res<crate::progression::Progress>,
        Res<crate::story::ObjectiveMarkers>,
    ),
    (mut waypoint, explored): (ResMut<crate::map::Waypoint>, Res<crate::map::Explored>),
    (island, treasure, world): (Res<crate::treasure_maps::Island>, Res<crate::treasure_maps::TreasureMaps>, Res<crate::pickups::WorldState>),
    mut story: Option<NonSendMut<crate::story::StoryHost>>,
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    (mut cursor, buttons, windows): (MessageReader<CursorMoved>, Res<ButtonInput<bevy::input::mouse::MouseButton>>, Query<&Window, With<PrimaryWindow>>),
) {
    use crate::map::map_uv;
    use fc3_ui::ExternalValue as V;
    let Interface::Running(host) = &mut *state else { return };
    for call in std::mem::take(&mut page.calls) {
        let arg = |i: usize| call.args.get(i).map(String::as_str).unwrap_or("");
        match arg(0) {
            "MapLoaded" => {
                page.open = true;
                page.zoom = MAP_ZOOM_START;
                page.travel.clear();
                page.icons.clear();
                page.placed = None;
                let legend = MAP_LEGEND
                    .iter()
                    .map(|&(kind, name)| {
                        [("iIconID", V::Number(kind)), ("bDisplay", V::Bool(true)), ("sName", V::String(host.text(name)))]
                            .into_iter()
                            .map(|(k, v)| (k.to_string(), v))
                            .collect()
                    })
                    .collect();
                host.set_data("DataMap", legend);
                // (Rows run north to south; the explored sectors south to
                // north.)
                let n = fc3_formats::terrain::WORLD_SECTORS as usize;
                let mut lifted = std::collections::BTreeSet::new();
                for (i, _) in explored.cells.iter().enumerate().filter(|(_, c)| **c != 0) {
                    let (sx, sy) = (i % n, i / n);
                    lifted.insert(((n - 1 - sy) * MAP_FOG_CELLS / n, sx * MAP_FOG_CELLS / n));
                }
                // FC3_MAP_REVEAL (tests): all of it.
                if std::env::var_os("FC3_MAP_REVEAL").is_some() {
                    lifted.extend((0..MAP_FOG_CELLS).flat_map(|r| (0..MAP_FOG_CELLS).map(move |c| (r, c))));
                }
                for (row, col) in lifted {
                    host.event("SetFog", &["0", &row.to_string(), &col.to_string()]);
                }
                let mut items = Vec::new();
                let mut places = Vec::new();
                let mut icon = |kind: f64, at: Vec3, name: &str, angle: f32, marked: bool| {
                    let uv = map_uv(at);
                    places.push((format!("Icon_{}", items.len()), uv));
                    let fields: std::collections::BTreeMap<String, V> = [
                        ("iType", V::Number(kind)),
                        ("iXPos", V::Number(uv.x as f64)),
                        ("iYPos", V::Number(uv.y as f64)),
                        ("sName", V::String(name.to_string())),
                        ("iIndex", V::Number(items.len() as f64)),
                        ("iAngle", V::Number(angle as f64)),
                        ("bIsMarker", V::Number(if marked { 1.0 } else { 0.0 })),
                    ]
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect();
                    items.push(fields);
                };
                if let Ok((t, p)) = players.single() {
                    // (Its turn clockwise from north, as the movie turns it.)
                    icon(map_icon::PLAYER, t.translation, "", -p.yaw.to_degrees(), false);
                }
                // The markers scripts show, as what they are (a quest's giver,
                // a mission's, else an objective), and those standing in the
                // layers that are in (activities, shops, the challenge boards).
                match story.as_ref() {
                    Some(h) => {
                        for (kind, at) in scripted_markers(h).into_iter().chain(standing_markers(h)) {
                            icon(kind, at, "", 0.0, false);
                        }
                    }
                    None => {
                        for &goal in &objectives.0 {
                            icon(map_icon::MISSION, goal, "", 0.0, false);
                        }
                    }
                }
                for (i, at) in ways.data.towers.iter().enumerate() {
                    let taken = progress.towers.contains(&(i as u32));
                    icon(if taken { map_icon::TOWER } else { map_icon::TOWER_LOCKED }, Vec3::from(*at), "", 0.0, false);
                }
                let destinations = story.as_ref().map(|h| h.story.travel_destinations()).unwrap_or_default();
                for (i, o) in outposts.list.iter().enumerate() {
                    // (A liberated outpost's own safehouse stands for it.)
                    if o.liberated && destinations.iter().any(|d| o.contains(crate::data::to_bevy(d.position))) {
                        continue;
                    }
                    // (Liberated, it's a place to travel to, as the radar
                    // has it.)
                    icon(if o.liberated { map_icon::SAFEHOUSE } else { map_icon::OUTPOST_LOCKED }, o.centre, "", 0.0, false);
                    if o.liberated {
                        page.travel.push(Travel::Outpost(i));
                    }
                }
                for d in &destinations {
                    let name = crate::story::plain(&d.name);
                    icon(if d.unlocked { map_icon::SAFEHOUSE } else { map_icon::SAFEHOUSE_LOCKED }, crate::data::to_bevy(d.position), &name, 0.0, false);
                    if d.unlocked {
                        page.travel.push(Travel::Safehouse(d.marker));
                    }
                }
                // What the treasure maps bought show in their cells, until
                // it's been picked up (FC3_MAP_FINDS=C3,D4..., tests:
                // everything in those cells).
                let mut shown = treasure.shown(&progress.maps);
                for cell in std::env::var("FC3_MAP_FINDS").unwrap_or_default().split(',').filter(|c| !c.is_empty()) {
                    shown.insert(cell.trim().to_string(), u16::MAX);
                }
                for t in island.things.iter().filter(|t| shown.get(&t.cell).is_some_and(|b| b & t.find.reveal() != 0) && !world.removed.contains(&t.id)) {
                    use crate::treasure_maps::{Find, Leaf};
                    let kind = match t.find {
                        Find::Relic(_) => map_icon::RELIC,
                        Find::Letter => map_icon::LETTER,
                        Find::MemoryCard => map_icon::SD_CARD,
                        Find::Chest(_) => map_icon::CHEST,
                        Find::Plant(Leaf::Green) => map_icon::PLANT_GREEN,
                        Find::Plant(Leaf::Crimson) => map_icon::PLANT_RED,
                        Find::Plant(Leaf::Amber) => map_icon::PLANT_YELLOW,
                        Find::Plant(Leaf::White) => map_icon::PLANT_WHITE,
                        Find::Plant(Leaf::Blue) => map_icon::PLANT_BLUE,
                    };
                    icon(kind, t.at, "", 0.0, false);
                }
                if let Some(at) = waypoint.0 {
                    icon(map_icon::MARKER, at, "", 0.0, true);
                }
                page.icons = places;
                let mut kinds = std::collections::BTreeMap::<i64, usize>::new();
                for item in &items {
                    if let Some(V::Number(k)) = item.get("iType") {
                        *kinds.entry(*k as i64).or_default() += 1;
                    }
                }
                debug!("original UI: world map: {} icons (by type {kinds:?}), {} to travel to", items.len(), page.travel.len());
                host.set_data("IconMap", items);
            }
            "IconsLoaded" => {
                let zoom = page.zoom;
                host.event("CenterMap", &[&map_level(zoom).to_string(), &zoom.to_string()]);
                // FC3_MAP_ZOOM_STEPS=<n> (tests): n wheel steps in (out
                // when negative) once it's centred.
                if let Some(steps) = std::env::var("FC3_MAP_ZOOM_STEPS").ok().and_then(|s| s.parse::<i32>().ok()) {
                    page.zoom = (zoom * MAP_ZOOM_STEP.powi(steps)).clamp(MAP_ZOOM.0, MAP_ZOOM.1);
                    host.event("OnLevelOfZoomUpdated", &[&map_level(page.zoom).to_string(), &page.zoom.to_string()]);
                }
                // FC3_MAP_PAN=<x>,<y> (tests): dragged that far (stage
                // pixels) once it's centred.
                if let Some((x, y)) = std::env::var("FC3_MAP_PAN").ok().and_then(|s| {
                    let (x, y) = s.split_once(',')?;
                    Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
                }) {
                    page.pan_test = Some(Vec2::new(x, y));
                }
            }
            // `1 <x> <y> <type> <icon>` placed (a fraction of the map each
            // way), `0` taken away.
            "SetupMarker" => {
                waypoint.0 = (arg(1) == "1").then(|| crate::map::world_at(Vec2::new(arg(2).parse().unwrap_or(0.5), arg(3).parse().unwrap_or(0.5))));
            }
            "onFastTravel" => {
                let Some(&to) = arg(1).parse::<usize>().ok().and_then(|i| page.travel.get(i)) else { continue };
                if !story.as_ref().is_none_or(|h| h.story.fast_travel_enabled()) {
                    continue;
                }
                let Ok((mut t, mut p)) = players.single_mut() else { continue };
                let travelled = match to {
                    Travel::Outpost(i) => {
                        let done = crate::outposts::fast_travel(&outposts, i, &mut t, &mut p);
                        if let (true, Some(host)) = (done, story.as_mut()) {
                            host.story.report_outpost_travel(fc3_story::Pose {
                                position: crate::data::to_dunia(t.translation),
                                angles: [0.0, 0.0, -p.yaw.to_degrees()],
                            });
                        }
                        done
                    }
                    Travel::Safehouse(marker) => match story.as_mut().and_then(|h| h.story.fast_travel(marker)) {
                        Some(pose) => {
                            crate::story::teleport(&mut t, &mut p, pose.position, pose.angles[2]);
                            true
                        }
                        None => false,
                    },
                };
                info!("original UI: world map: fast travel to {to:?}: {travelled}");
                if travelled {
                    next.set(crate::menu::GameState::Playing);
                }
            }
            _ => {}
        }
    }
    if !page.open {
        wheel.clear();
        cursor.clear();
        return;
    }
    let num = |v: V| match v {
        V::Number(n) => Some(n),
        _ => None,
    };
    // A drag pans: the window's pixels to the stage's (shown whole), to the
    // map's own (its whole width a tile size a tile at its zoom).
    use bevy::input::mouse::MouseButton as Button;
    let mut moved = Vec2::ZERO;
    for c in cursor.read() {
        if let Some(last) = page.mouse {
            moved += c.position - last;
        }
        page.mouse = Some(c.position);
    }
    if buttons.pressed(Button::Left) {
        page.dragged += moved.length();
    } else {
        page.dragged = 0.0;
    }
    let stage = windows.single().map_or(1.0, |w| (w.width() / fc3_ui::STAGE.0 as f32).min(w.height() / fc3_ui::STAGE.1 as f32)).max(0.01);
    if page.dragged > MAP_DRAG && moved != Vec2::ZERO {
        pan_map(host, page.zoom, moved / stage);
    }
    // (A test's drag waits for the movie's centring to be done.)
    if page.placed.is_some() {
        if let Some(by) = page.pan_test.take() {
            pan_map(host, page.zoom, by);
        }
    }
    if buttons.just_pressed(Button::Right) {
        host.event("FireMouseRightButton", &["0", "0", "0", "true"]);
    }
    if buttons.just_released(Button::Right) {
        host.event("FireMouseRightButton", &["0", "0", "0", "false"]);
    }
    let view = (|| {
        Some([
            num(host.dps_get("MapVisual", "_x"))? / num(host.dps_get("MapVisual", "_width"))?.max(1.0),
            num(host.dps_get("MapVisual", "_y"))? / num(host.dps_get("MapVisual", "_height"))?.max(1.0),
            num(host.dps_get("MapScale", "_width"))?,
            num(host.dps_get("MapScale", "_height"))?,
        ])
    })();
    if let Some(v) = view.filter(|v| page.placed.as_ref() != Some(v) && v.iter().all(|n| n.is_finite())) {
        debug!("world map: view {v:?}; MapVisual {}; MapScale {}", host.dps_info("MapVisual"), host.dps_info("MapScale"));
        host.dps_set_parent("MapScale", "mf_XPos", V::Number(v[0]));
        host.dps_set_parent("MapScale", "mf_YPos", V::Number(v[1]));
        for (id, uv) in &page.icons {
            host.dps_set(id, "_x", V::Number((uv.x as f64 + v[0]) * v[2]));
            host.dps_set(id, "_y", V::Number((uv.y as f64 + v[1]) * v[3]));
        }
        page.placed = Some(v);
        // (A finds test: how many icons are in view.)
        if std::env::var_os("FC3_MAP_FINDS").is_some() {
            let on_stage: Vec<String> = page
                .icons
                .iter()
                .filter_map(|(clip, _)| {
                    let [x0, y0, ..] = host.dps_bounds(clip)?;
                    ((0.0..1280.0).contains(&x0) && (0.0..720.0).contains(&y0)).then(|| clip.clone())
                })
                .collect();
            info!("original UI: world map: {} icons in view", on_stage.len());
        }
    }
    for w in wheel.read() {
        let zoom = if w.y > 0.0 {
            page.zoom * MAP_ZOOM_STEP
        } else if w.y < 0.0 {
            page.zoom / MAP_ZOOM_STEP
        } else {
            continue;
        };
        let zoom = zoom.clamp(MAP_ZOOM.0, MAP_ZOOM.1);
        if zoom != page.zoom {
            page.zoom = zoom;
            host.event("OnLevelOfZoomUpdated", &[&map_level(zoom).to_string(), &zoom.to_string()]);
        }
    }
}

/// Drags the world map page's map by a distance on the stage: its picture
/// (`MapVisual`, in the map's own pixels, the whole map a tile size a tile
/// shown `zoom` wide) and the fog's with it, kept within the map's edges.
fn pan_map(host: &fc3_ui::Host, zoom: f32, by: Vec2) {
    use fc3_ui::ExternalValue as V;
    let size = (MAP_LEVELS[map_level(zoom)] * 512.0) as f64;
    let step = by.as_dvec2() * size / zoom as f64;
    let at = |p: &str| match host.dps_get("MapVisual", p) {
        V::Number(n) => n,
        _ => 0.0,
    };
    let (x, y) = ((at("_x") + step.x).clamp(-size, 0.0), (at("_y") + step.y).clamp(-size, 0.0));
    for clip in ["MapVisual", "FogVisualMC"] {
        host.dps_set(clip, "_x", V::Number(x));
        host.dps_set(clip, "_y", V::Number(y));
    }
}

/// A key by its name, for tests (`FC3_PAUSE_KEYS`).
fn test_key(name: &str) -> Option<KeyCode> {
    Some(match name {
        "Enter" => KeyCode::Enter,
        "Escape" => KeyCode::Escape,
        "Up" => KeyCode::ArrowUp,
        "Down" => KeyCode::ArrowDown,
        "Left" => KeyCode::ArrowLeft,
        "Right" => KeyCode::ArrowRight,
        "Space" => KeyCode::Space,
        "Tab" => KeyCode::Tab,
        _ => return None,
    })
}

/// The loot rucksack page's list (`DataLootInventory`): a tile for each
/// thing carried, skins, plants and loot, with what its item says (name,
/// description, sale price, rare, icon), and the money (`SetMoney`) and sort
/// line (`SortText`).
fn send_rucksack(
    host: &fc3_ui::Host,
    progress: &crate::progression::Progress,
    crafting: Option<&crate::crafting::CraftCatalog>,
    loot: Option<&crate::progression::LootCatalog>,
    perks: &crate::progression::Perks,
) {
    host.set_data("DataLootInventory", rucksack_tiles(progress, crafting, loot, perks));
    host.event("SetMoney", &[&progress.money.to_string()]);
    host.event("SortText", &["0"]);
}

/// The rucksack's tiles: one for each thing carried (the shop's sell page
/// shows them too).
fn rucksack_tiles(
    progress: &crate::progression::Progress,
    crafting: Option<&crate::crafting::CraftCatalog>,
    loot: Option<&crate::progression::LootCatalog>,
    perks: &crate::progression::Perks,
) -> Vec<std::collections::BTreeMap<String, fc3_ui::ExternalValue>> {
    use crate::progression::{Animal, LootKind};
    use fc3_ui::ExternalValue as V;
    let mut held: Vec<(LootKind, u32, &'static str)> = Vec::new();
    for a in Animal::ALL {
        held.push((LootKind::Skin(a), progress.skins.get(&a).copied().unwrap_or(0), "animal"));
    }
    for plant in crate::syringes::Plant::ALL {
        held.push((LootKind::Plant(plant), progress.plants[plant.index()], "plant"));
    }
    for (&id, &n) in &progress.loot {
        held.push((LootKind::Item(id), n, "loot"));
    }
    let mut items = Vec::new();
    for (kind, n, sub) in held.into_iter().filter(|h| h.1 > 0) {
        let Some(item) = loot.and_then(|l| l.item(kind)) else { continue };
        let price = crafting.map_or(item.cost, |c| c.sale_price(item.id, perks));
        let fields: std::collections::BTreeMap<String, V> = [
            ("sName", V::String(item.name.clone())),
            ("sInfo", V::String(item.description.clone())),
            ("sSubCategory", V::String(sub.to_string())),
            ("iPrice", V::Number(price as f64)),
            ("iStateID", V::Number(RUCKSACK_AVAILABLE)),
            ("bRare", V::Bool(item.rare)),
            ("iObjectID", V::Number(item.id as f64)),
            ("sImage", V::String(rucksack_icon(kind, item))),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        items.extend(std::iter::repeat_n(fields, n as usize));
    }
    items
}

/// A tile's state the rucksack shows as one to sell or drop
/// (`C_Button.C_STATE_AVAILABLE`).
const RUCKSACK_AVAILABLE: f64 = 1.0;

/// The rucksack movie's icon for a thing: the item's own when the movie has
/// it (`TX_...`), else, for its shared ones (`AnimalLoot`), the hide of its
/// animal (`TX_AnimalHideBoar`, the rare one's) or a part, a leaf, or a
/// stranger's personal things.
fn rucksack_icon(kind: crate::progression::LootKind, item: &fc3_formats::progression::Item) -> String {
    use crate::progression::{Animal, LootKind};
    if item.icon.starts_with("TX_") {
        return item.icon.clone();
    }
    match kind {
        LootKind::Skin(a) => {
            let hide = match a {
                Animal::Boar => "Boar",
                Animal::Deer => "Deer",
                Animal::Goat => "Goat",
                Animal::Dog => "Dog",
                Animal::Tapir => "AsianTapir",
                Animal::Leopard => "Leopard",
                Animal::Bear => "Bear",
                Animal::Tiger => "Tiger",
                Animal::Komodo => "Komodo",
                Animal::Cassowary => "Cassowary",
                Animal::Buffalo => "Buffalo",
                Animal::Shark => "Shark",
                Animal::Thylacine => "Thylacine",
                Animal::WildPig | Animal::Pig => "Pig",
                Animal::SeaTurtle => return "TX_AnimalTurtleShell".into(),
                Animal::Tortoise | Animal::Crab => return "TX_AnimalShell".into(),
                Animal::Crow => return "TX_AnimalFeather".into(),
                Animal::Crocodile => return "TX_AnimalLeather".into(),
                _ => return "TX_AnimalSkin".into(),
            };
            format!("TX_AnimalHide{}{hide}", if item.rare { "Rare" } else { "" })
        }
        LootKind::Plant(_) => "TX_PlantsLeaf".into(),
        LootKind::Item(_) => "TX_HumanPersonalItems".into(),
    }
}

/// The skills page (`M_Skills.feu`, quick launch 3): the three trees
/// (`InitChapter`: name, description and a blank each), the tattoo's
/// collectibles (`InitCollectibles`), the points to spend, `SkillsDP` (a
/// skill each, in its tree's order: the first is its root, and the icon map
/// draws a skill's icon by that index), then `SetLevel`, which builds the
/// tree tabs and draws the first. A skill's state is learned
/// (`C_STATE_BOUGHT` 3), shut by the story (`C_STATE_LOCK` 0) or open
/// (`C_STATE_BUY` 2); the page itself greys one whose parent isn't learned
/// or whose statistic isn't met (`sfulfilledPreReq`). `first` false sends
/// the points and the list again only (after a purchase).
fn send_skills(host: &fc3_ui::Host, progress: &crate::progression::Progress, skills: &crate::progression::Skills, first: bool) {
    use crate::progression::{skill_state, SkillLock, SkillState};
    use fc3_ui::ExternalValue as V;
    let tree = &skills.0;
    let branches: Vec<_> = tree.branches.iter().take(3).collect();
    if first {
        let chapters: Vec<&str> = branches.iter().flat_map(|b| [b.name.as_str(), b.description.as_str(), ""]).collect();
        host.event("InitChapter", &chapters);
        let relics = progress.stat(crate::progression::stat::RELICS).to_string();
        host.event("InitCollectibles", &[&relics, "1", "0", "0"]);
    }
    host.event("SetSkillPoint", &[&progress.skill_points.to_string()]);
    let mut items = Vec::new();
    for (t, b) in branches.iter().enumerate() {
        let list: Vec<_> = tree.abilities.iter().filter(|a| a.branch == b.number).collect();
        for (i, a) in list.iter().enumerate() {
            let state = match skill_state(progress, a) {
                SkillState::Learned => 3.0,
                SkillState::Locked(SkillLock::Story | SkillLock::Unavailable) => 0.0,
                _ => 2.0,
            };
            let stat_met = a.stat == 0 || progress.stat(a.stat) >= a.stat_target;
            let parent = a.requires.iter().find_map(|r| list.iter().position(|x| &x.id == r)).map_or(-1, |p| p as i32);
            let fields: std::collections::BTreeMap<String, V> = [
                ("iSkillTreeId", V::Number(t as f64)),
                ("iSkillIndex", V::Number(i as f64)),
                ("iAbilityState", V::Number(state)),
                ("iCurrentSkillPoint", V::Number(if state == 3.0 { a.cost as f64 } else { 0.0 })),
                ("iMaxSkillPoint", V::Number(a.cost as f64)),
                ("iMatrixRowIndex", V::Number(a.row as f64)),
                ("iMatrixColumnIndex", V::Number(a.column as f64)),
                ("sSkillnName", V::String(a.name.clone())),
                ("sSkillDescription", V::String(crate::progression::describe(a))),
                ("sSkillTreeIconName", V::String(String::new())),
                // (The movie knows tattoo parts by their names' CRC-32.)
                (
                    "itattooPartName",
                    V::String(if a.tattoo_part.is_empty() { String::new() } else { fc3_formats::hash::crc32(a.tattoo_part.as_bytes()).to_string() }),
                ),
                ("sDependancy", V::String(parent.to_string())),
                ("sfulfilledPreReq", V::String(if stat_met { "1" } else { "0" }.into())),
                ("sPreReqString", V::String(a.stat_text.clone())),
                ("sHasTutorial", V::String("0".into())),
                ("sTutorialHeader", V::String(String::new())),
                ("sTutorialBodyString", V::String(String::new())),
                ("sTutorialVideoString", V::String(String::new())),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
            items.push(fields);
        }
    }
    host.set_data("SkillsDP", items);
    if first {
        // XP so far against the next level's total (level -1 blanks it, at
        // the top).
        let next = crate::progression::xp_needed(progress.level + 1, skills);
        let level = if next == u32::MAX { "-1".to_string() } else { progress.level.to_string() };
        let total = if next == u32::MAX { progress.xp.max(1) } else { next };
        host.event("SetLevel", &[&level, &progress.xp.to_string(), &total.to_string()]);
    }
}

/// Opens a pause menu page (quick launch: -1 the pause page, 3 skills, 8
/// crafting, 13 the rucksack). The page takes the event's further arguments
/// as where to open it (the crafting page's tab, a group, an entry), -1 for
/// none; left out, a page read a blank as its tab (NaN).
fn open_page(host: &fc3_ui::Host, page: &str) {
    host.event("SetPageToLoad", &[page, "-1", "-1", "-1"]);
}

/// The handbook page (`M_Handbook.feu`, quick launch 10) asks for one list
/// at a time (`OnEncyclopediaRequest <id> <list>`): `TabCategory` its tabs
/// (the handbook's categories: collectibles 1, statistics 2, tutorials 4,
/// the survival guide 5, the manual 6), `SubCategory <tab>` a tab's groups,
/// `WikiCategory <group>` a group's pages (`bUnlocked` as the game opens
/// them: `progression::handbook_open`), `StatsCategory <category>` a
/// statistics category's rows (`statistics::rows`) and `Emanual` the
/// manual's (not answered: its own movie).
fn send_handbook(
    host: &fc3_ui::Host,
    book: &fc3_formats::progression::Handbook,
    progress: &crate::progression::Progress,
    extra: crate::statistics::Extra,
    id: u32,
    list: &str,
) {
    use fc3_ui::ExternalValue as V;
    let item = |fields: Vec<(&str, V)>| fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<std::collections::BTreeMap<String, V>>();
    let items = match list {
        "TabCategory" => {
            let mut tabs: Vec<_> = book.categories.iter().collect();
            tabs.sort_by_key(|c| c.sort);
            tabs.iter().map(|c| item(vec![("iObjectID", V::Number(c.id as f64)), ("sName", V::String(c.name.clone())), ("bNew", V::Bool(false))])).collect()
        }
        "SubCategory" => {
            let ids = book.categories.iter().find(|c| c.id == id).map(|c| c.subcategories.clone()).unwrap_or_default();
            let mut groups: Vec<_> = book.subcategories.iter().filter(|s| ids.contains(&s.id)).collect();
            groups.sort_by_key(|s| s.sort);
            groups
                .iter()
                .map(|s| {
                    item(vec![
                        ("iObjectID", V::Number(s.id as f64)),
                        ("sName", V::String(s.name.clone())),
                        ("bEnabled", V::Bool(true)),
                        ("bNew", V::Bool(false)),
                    ])
                })
                .collect()
        }
        "WikiCategory" => {
            let mut pages: Vec<_> = book.entries.iter().filter(|e| e.subcategory == id).collect();
            pages.sort_by_key(|e| e.sort);
            pages
                .iter()
                .map(|e| {
                    item(vec![
                        ("iObjectID", V::Number(e.id as f64)),
                        ("sName", V::String(e.name.clone())),
                        ("sDescription", V::String(e.description.clone())),
                        ("bUnlocked", V::Bool(crate::progression::handbook_open(e, progress))),
                        ("bNew", V::Bool(false)),
                        ("sImage", V::String(String::new())),
                    ])
                })
                .collect()
        }
        // Each row by name (the game's text), its figure (a number, or a
        // text the page shows as it is: its type the same text), the
        // progress page's total and milestones, the animals' skinned column
        // (a dash where none).
        "StatsCategory" => crate::statistics::rows(id, progress, extra)
            .into_iter()
            .map(|r| {
                use crate::statistics::Figure;
                let text = |f: Figure| match f {
                    Figure::Count(n) => V::Number(n as f64),
                    Figure::Text(t) => V::String(host.text(t)),
                    Figure::Dash => V::String("-".into()),
                };
                let value = text(r.value);
                let kind = match &value {
                    V::String(s) => s.clone(),
                    _ => r.kind.to_string(),
                };
                let mut fields = vec![
                    ("sName", V::String(host.text(r.name))),
                    ("iValue", value),
                    ("sMilestones", V::String(r.milestones.into())),
                    ("sType", V::String(kind)),
                ];
                if let Some(total) = r.total {
                    fields.push(("iTotal", text(total)));
                }
                item(fields)
            })
            .collect(),
        _ => return,
    };
    host.set_data(list, items);
}

/// A trial's board or result page (`M_ChallengeBoard.feu`,
/// `M_ChallengeResult.feu`): its name with its description (the board)
/// or the result and the best and this run's scores, then the scores to
/// beat (`DataChallengeBoard`: the developers', type 1, and the player's,
/// type 0) and the rewards (`DataChallengeRewards`, each ticked once
/// earned).
fn send_trial_page(host: &fc3_ui::Host, page: &crate::trials::Page) {
    use crate::trials::PageKind;
    use fc3_ui::ExternalValue as V;
    let name = host.text(page.name);
    match page.kind {
        PageKind::Result { success } => {
            host.event("SetResult", &[if success { "1" } else { "0" }, ""]);
            host.event("SetChallengeGlobalInfo", &[&name, &page.best.to_string(), &page.score.to_string()]);
        }
        _ => host.event("SetChallengeGlobalInfo", &[&name, &host.text(page.description), &page.best.to_string()]),
    }
    host.event("SetChallengePercent", &[""]);
    let item = |fields: Vec<(&str, V)>| fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<std::collections::BTreeMap<String, V>>();
    let board = page
        .board
        .iter()
        .map(|(who, score, player)| {
            item(vec![
                ("sName", V::String(who.clone())),
                ("sScore", V::String(score.to_string())),
                ("iType", V::Number(if *player { 0.0 } else { 1.0 })),
                ("iClientId", V::Number(0.0)),
            ])
        })
        .collect();
    host.set_data("DataChallengeBoard", board);
    let rewards =
        page.rewards.iter().map(|(line, got)| item(vec![("sName", V::String(line.clone())), ("iRewarded", V::Number(if *got { 1.0 } else { 0.0 }))])).collect();
    host.set_data("DataChallengeRewards", rewards);
}

/// A trial's HUD (`H_Challenges.feu`, the main HUD's `Challenges_MC`, up
/// with `ShowUIElement Challenges`): the score (pulsing as it moves), the
/// next reward's score, the time left (`mm:ss`), and each score's popup
/// (`ChallengeUpdateScore_<n>`, two to a kind in turn: kills, headshots,
/// stealth, takedowns, time) with its rule's name and what it added.
#[derive(Default)]
struct TrialsShown {
    on: bool,
    last: Option<crate::trials::Hud>,
}

fn drive_trials(state: NonSend<Interface>, mut trials: ResMut<crate::trials::Trials>, mut shown: Local<TrialsShown>) {
    use fc3_ui::ExternalValue as V;
    let Interface::Running(host) = &*state else { return };
    let Some(hud) = trials.hud else {
        if shown.on {
            host.event("HideUIElement", &["Challenges"]);
            host.dps_visible("ChallengeWidget", false);
            *shown = TrialsShown::default();
        }
        return;
    };
    if !shown.on {
        host.event("ShowUIElement", &["Challenges"]);
        shown.on = host.dps_visible("ChallengeWidget", true);
    }
    if shown.last != Some(hud) {
        let left = hud.left.ceil() as u32;
        host.dps_text("ChallengeTextActualScore", &hud.score.to_string());
        host.dps_text("ChallengeTextChallengerScore", &format!("/{}", hud.target));
        host.dps_text("ChallengeTextRemainingScore", &format!("{:02}:{:02}", left / 60, left % 60));
        shown.last = Some(hud);
    }
    if std::mem::take(&mut trials.pulse) {
        host.dps_play("ChallengeTextActualScoreMC", V::String("Anim".into()));
    }
    for p in std::mem::take(&mut trials.popups) {
        let slot = crate::trials::popup_slot(&mut trials, p.kind);
        host.dps_text(&format!("ChallengeTextUpdateScore_{slot}"), &host.text(p.name));
        host.dps_text(&format!("ChallengeTextScore_{slot}"), &p.value);
        host.dps_play(&format!("ChallengeUpdateScore_{slot}"), V::String("FadeIn".into()));
    }
}

/// The options page's sections (`OptionSections`), at the places the PC
/// page's code takes them by (`OptionsPage_PC`): video, calibration, video
/// quality, controls, gameplay, audio (their names are the game's strings).
const OPTION_SECTIONS: [u32; 6] = [150004, 172617, 172551, 150002, 150001, 150003];

const OPTIONS_VIDEO: u32 = 0;
const OPTIONS_CALIBRATION: u32 = 1;
const OPTIONS_QUALITY: u32 = 2;
const OPTIONS_CONTROLS: u32 = 3;
const OPTIONS_GAMEPLAY: u32 = 4;
const OPTIONS_AUDIO: u32 = 5;

/// ON and OFF, and the quality levels LOW, MEDIUM, HIGH and ULTRA, as the
/// game's strings.
const OPTION_ON_OFF: [u32; 2] = [150702, 150703];
const OPTION_QUALITY: [u32; 4] = [167389, 167390, 167391, 167393];

/// A row of the options page: its name and description (the game's
/// strings), how it is set (`iWidgetType`: 0 a choice among labels, 1 a
/// slider 0-100, 7 nothing: the page opens something of its own), and its
/// value (a label's place, or the slider's).
struct OptionRow {
    name: u32,
    desc: u32,
    widget: u32,
    labels: Vec<u32>,
    current: i32,
}

/// A section's rows from the settings. Some places are the page's own:
/// brightness first in calibration (its own popup), the control type third
/// and customizing the controls fourth among controls.
fn option_rows(section: u32, s: &crate::settings::Settings) -> Vec<OptionRow> {
    use crate::settings::{Difficulty, Quality};
    let choice = |name, desc, labels: &[u32], current: usize| OptionRow { name, desc, widget: 0, labels: labels.to_vec(), current: current as i32 };
    let on_off = |name, desc, on: bool| choice(name, desc, &OPTION_ON_OFF, if on { 0 } else { 1 });
    let bar = |name, desc, v: f32, lo: f32, hi: f32| OptionRow {
        name,
        desc,
        widget: 1,
        labels: Vec::new(),
        current: ((v - lo) / (hi - lo) * 100.0).round().clamp(0.0, 100.0) as i32,
    };
    let quality = |name, desc, q: Quality| choice(name, desc, &OPTION_QUALITY, Quality::ALL.iter().position(|x| *x == q).unwrap_or(2));
    match section {
        OPTIONS_VIDEO => vec![
            // Window mode: fullscreen, windowed.
            choice(172054, 172055, &[167839, 167837], if s.fullscreen { 0 } else { 1 }),
            on_off(150291, 150292, s.vsync),
            bar(172598, 150270, s.fov, 55.0, 110.0),
        ],
        // (Brightness: the page's own popup; not one of ours.)
        OPTIONS_CALIBRATION => vec![OptionRow { name: 91542, desc: 0, widget: 1, labels: Vec::new(), current: 50 }],
        OPTIONS_QUALITY => vec![quality(150397, 150398, s.shadows), quality(150393, 150394, s.effects), quality(150391, 150392, s.view_distance)],
        OPTIONS_CONTROLS => vec![
            on_off(91540, 0, s.invert_y),
            bar(150281, 150282, s.sensitivity, 0.1, 5.0),
            // Control type: keyboard and mouse, gamepad.
            choice(150276, 150277, &[167702, 167703], 0),
            OptionRow { name: 150279, desc: 150280, widget: 7, labels: Vec::new(), current: 0 },
            bar(91554, 0, s.gamepad_sensitivity, 0.1, 5.0),
        ],
        OPTIONS_GAMEPLAY => vec![
            // Difficulty: casual, adventurer, warrior, infamous.
            choice(160731, 0, &[160421, 150451, 150453, 31459], Difficulty::ALL.iter().position(|d| *d == s.difficulty).unwrap_or(1)),
            on_off(91553, 0, s.subtitles),
            on_off(91557, 0, s.aim_assist),
            on_off(91556, 0, s.vibration),
        ],
        OPTIONS_AUDIO => vec![bar(152306, 152307, s.master_volume, 0.0, 1.0), on_off(160662, 160663, s.music_volume > 0.0)],
        _ => Vec::new(),
    }
}

fn send_option_sections(host: &fc3_ui::Host) {
    use fc3_ui::ExternalValue as V;
    let items = OPTION_SECTIONS
        .iter()
        .enumerate()
        .map(|(i, &name)| [("iIndex", V::Number(i as f64)), ("sName", V::String(host.text(name)))].into_iter().map(|(k, v)| (k.to_string(), v)).collect())
        .collect();
    host.set_data("OptionSections", items);
}

/// A section's rows (`OptionValues`): each its name, description, widget,
/// labels (`sLocCurrentValues`, a list of `sLocCurrentValue`s: the row's
/// binder wraps it in a list of its own), the number of them (100 for a
/// slider) and its value.
fn send_options(host: &fc3_ui::Host, settings: &crate::settings::Settings, section: u32) {
    use fc3_ui::ExternalValue as V;
    let items = option_rows(section, settings)
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let labels: Vec<V> =
                row.labels.iter().map(|&l| V::Object([("sLocCurrentValue".to_string(), V::String(host.text(l)))].into_iter().collect())).collect();
            let count = if row.widget == 1 { 100 } else { row.labels.len() };
            [
                ("iIndex", V::Number(i as f64)),
                ("sName", V::String(host.text(row.name))),
                ("sDescription", V::String(if row.desc == 0 { String::new() } else { host.text(row.desc) })),
                ("iWidgetType", V::Number(row.widget as f64)),
                ("iNbValues", V::Number(count as f64)),
                ("iCurrentValue", V::Number(row.current as f64)),
                ("sLocCurrentValues", V::List(labels)),
                ("bEnable", V::Bool(true)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
        })
        .collect();
    host.set_data("OptionValues", items);
}

/// The values the options page sends for a section (`SetOptions`, its rows'
/// values in order) into the settings (saved as they change).
fn set_options(s: &mut crate::settings::Settings, section: u32, values: &[i32]) {
    use crate::settings::{Difficulty, Quality};
    let v = |i: usize| values.get(i).copied();
    let on = |i: usize, was: bool| v(i).map_or(was, |x| x == 0);
    let bar = |i: usize, was: f32, lo: f32, hi: f32| v(i).map_or(was, |x| lo + (hi - lo) * x.clamp(0, 100) as f32 / 100.0);
    let quality = |i: usize, was: Quality| v(i).and_then(|x| Quality::ALL.get(x as usize).copied()).unwrap_or(was);
    match section {
        OPTIONS_VIDEO => {
            s.fullscreen = on(0, s.fullscreen);
            s.vsync = on(1, s.vsync);
            s.fov = bar(2, s.fov, 55.0, 110.0);
        }
        OPTIONS_QUALITY => {
            s.shadows = quality(0, s.shadows);
            s.effects = quality(1, s.effects);
            s.view_distance = quality(2, s.view_distance);
        }
        OPTIONS_CONTROLS => {
            s.invert_y = on(0, s.invert_y);
            s.sensitivity = bar(1, s.sensitivity, 0.1, 5.0);
            s.gamepad_sensitivity = bar(4, s.gamepad_sensitivity, 0.1, 5.0);
        }
        OPTIONS_GAMEPLAY => {
            s.difficulty = v(0).and_then(|x| Difficulty::ALL.get(x as usize).copied()).unwrap_or(s.difficulty);
            s.subtitles = on(1, s.subtitles);
            s.aim_assist = on(2, s.aim_assist);
            s.vibration = on(3, s.vibration);
        }
        OPTIONS_AUDIO => {
            s.master_volume = bar(0, s.master_volume, 0.0, 1.0);
            let music = on(1, s.music_volume > 0.0);
            if music != (s.music_volume > 0.0) {
                s.music_volume = if music { crate::settings::Settings::default().music_volume } else { 0.0 };
            }
        }
        _ => {}
    }
}

/// What the crafting page is on: its tab, group and the recipe last
/// selected (by its place in the catalog; `CraftingRecipeCrafted` names
/// none).
#[derive(Default)]
struct CraftPage {
    category: u32,
    subtype: u32,
    recipe: Option<usize>,
}

/// How many of what a recipe makes the player carries (medicine, syringes).
fn craft_carried(progress: &crate::progression::Progress, stats: Option<&crate::combat::PlayerStats>, recipe: &fc3_formats::progression::Recipe) -> u32 {
    if recipe.id == crate::crafting::MEDICINE {
        stats.map_or(0, |s| s.medkits)
    } else {
        progress.potions.get(&recipe.id).copied().unwrap_or(0)
    }
}

/// Gear made already (it is made once).
fn craft_done(progress: &crate::progression::Progress, recipe: &fc3_formats::progression::Recipe) -> bool {
    recipe.kind == fc3_formats::progression::KIND_GEAR && progress.recipes.contains(&recipe.id)
}

/// The crafting page's tabs (`DataRecipesTypes`), in menu order: syringes,
/// equipment, ammo, arrows (the page's own `C_CAT_*` are their places).
fn send_craft_types(host: &fc3_ui::Host, catalog: &crate::crafting::CraftCatalog) {
    use fc3_ui::ExternalValue as V;
    let mut types = catalog.types.clone();
    types.sort_by_key(|t| t.sort);
    let items = types
        .iter()
        .map(|t| {
            [("iID", V::Number(t.id as f64)), ("sName", V::String(t.name.clone())), ("sDesc", V::String(String::new())), ("bNew", V::Bool(false))]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect()
        })
        .collect();
    host.set_data("DataRecipesTypes", items);
}

/// A tab's groups (`DataRecipeSubTypes`): those its recipes fall in, each
/// craftable if one of its recipes is, with what the player carries of
/// them, and done when all its gear is made.
fn send_craft_groups(
    host: &fc3_ui::Host,
    catalog: &crate::crafting::CraftCatalog,
    loot: &crate::progression::LootCatalog,
    progress: &crate::progression::Progress,
    stats: Option<&crate::combat::PlayerStats>,
    medkit_cap: u32,
    tab: u32,
) {
    use fc3_ui::ExternalValue as V;
    let mut groups: Vec<&fc3_formats::progression::Category> =
        catalog.subtypes.iter().filter(|s| catalog.recipes.iter().any(|r| r.category == tab && r.subcategory == s.id)).collect();
    groups.sort_by_key(|s| s.sort);
    let items = groups
        .iter()
        .map(|s| {
            let recipes: Vec<_> = catalog.recipes.iter().filter(|r| r.category == tab && r.subcategory == s.id).collect();
            let craftable = stats.is_some_and(|st| recipes.iter().any(|r| catalog.can_craft(progress, st, medkit_cap, loot, &r.id)));
            let carried: u32 = recipes.iter().map(|r| craft_carried(progress, stats, r)).sum();
            let done = recipes.iter().all(|r| craft_done(progress, r));
            [
                ("iID", V::Number(s.id as f64)),
                ("sName", V::String(s.name.clone())),
                ("sDesc", V::String(String::new())),
                ("bCraftable", V::Bool(craftable)),
                ("iQuantity", V::Number(carried as f64)),
                ("iFinishedCrafting", V::Number(if done { 1.0 } else { 0.0 })),
                ("bNew", V::Bool(false)),
                ("iSortID", V::Number(s.sort as f64)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
        })
        .collect();
    host.set_data("DataRecipeSubTypes", items);
}

/// A group's recipes (`DataRecipes`), by their place in the catalog: open
/// (the story and the tier before allow it), craftable now, made (gear) and
/// carried (syringes).
fn send_craft_recipes(
    host: &fc3_ui::Host,
    catalog: &crate::crafting::CraftCatalog,
    loot: &crate::progression::LootCatalog,
    progress: &crate::progression::Progress,
    stats: Option<&crate::combat::PlayerStats>,
    medkit_cap: u32,
    page: &CraftPage,
) {
    use fc3_ui::ExternalValue as V;
    let mut recipes: Vec<(usize, &fc3_formats::progression::Recipe)> =
        catalog.recipes.iter().enumerate().filter(|(_, r)| r.category == page.category && r.subcategory == page.subtype).collect();
    recipes.sort_by_key(|(_, r)| r.sort);
    let items = recipes
        .iter()
        .map(|&(i, r)| {
            let craftable = stats.is_some_and(|st| catalog.can_craft(progress, st, medkit_cap, loot, &r.id));
            let duration = if r.duration > 0.0 { format!("{:.0}s", r.duration) } else { String::new() };
            [
                ("iObjectID", V::Number(i as f64)),
                ("sName", V::String(r.name.clone())),
                ("sDescription", V::String(r.description.clone())),
                ("iUnlocked", V::Number(if catalog.available(progress, r) || craft_done(progress, r) { 1.0 } else { 0.0 })),
                ("bCraftable", V::Bool(craftable)),
                ("iFinishedCrafting", V::Number(if craft_done(progress, r) { 1.0 } else { 0.0 })),
                ("iQuantity", V::Number(craft_carried(progress, stats, r) as f64)),
                ("bNew", V::Bool(false)),
                ("iSortID", V::Number(r.sort as f64)),
                ("iLevelProgressCurrent", V::Number(0.0)),
                ("iLevelProgressMax", V::Number(0.0)),
                ("sDurationDesc", V::String(String::new())),
                ("sDurationValue", V::String(duration)),
                ("sStrengthDesc", V::String(String::new())),
                ("sStrengthValue", V::String(String::new())),
                ("sItemSize", V::String(String::new())),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
        })
        .collect();
    host.set_data("DataRecipes", items);
}

/// The selected recipe's ingredients (`DataRequirements`): each item's
/// name, how many it takes, how many are carried, and its icon.
fn send_craft_ingredients(
    host: &fc3_ui::Host,
    catalog: &crate::crafting::CraftCatalog,
    loot: &crate::progression::LootCatalog,
    progress: &crate::progression::Progress,
    page: &CraftPage,
) {
    use fc3_ui::ExternalValue as V;
    let Some(recipe) = page.recipe.and_then(|i| catalog.recipes.get(i)) else { return };
    let items = recipe
        .ingredients
        .iter()
        .map(|&(id, n)| {
            let item = catalog.item(id);
            [
                ("sName", V::String(item.map_or_else(String::new, |i| i.name.clone()))),
                ("iNeeded", V::Number(n as f64)),
                ("iPossessed", V::Number(loot.held(progress, id) as f64)),
                ("sImage", V::String(item.map_or_else(String::new, |i| i.icon.clone()))),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
        })
        .collect();
    host.set_data("DataRequirements", items);
}

/// Learns the skill at `index` of tree `tree` (as `send_skills` lists them)
/// if it can be learned now.
fn buy_skill(progress: &mut crate::progression::Progress, skills: &crate::progression::Skills, tree: usize, index: usize) -> bool {
    use crate::progression::{skill_state, SkillState};
    let Some(branch) = skills.0.branches.iter().take(3).nth(tree) else { return false };
    let Some(a) = skills.0.abilities.iter().filter(|a| a.branch == branch.number).nth(index) else { return false };
    if skill_state(progress, a) != SkillState::Ready {
        return false;
    }
    progress.skill_points -= a.cost;
    progress.skills.insert(a.id.clone());
    true
}

/// One of a thing (by its item) out of the rucksack.
fn drop_loot(progress: &mut crate::progression::Progress, loot: &crate::progression::LootCatalog, id: u32) {
    use crate::progression::{Animal, LootKind};
    let one_less = |n: &mut u32| *n = n.saturating_sub(1);
    if let Some(n) = progress.loot.get_mut(&id).filter(|n| **n > 0) {
        one_less(n);
        return;
    }
    if let Some(a) =
        Animal::ALL.into_iter().find(|a| loot.item(LootKind::Skin(*a)).is_some_and(|i| i.id == id) && progress.skins.get(a).is_some_and(|n| *n > 0))
    {
        progress.skins.entry(a).and_modify(one_less);
        return;
    }
    if let Some(plant) =
        crate::syringes::Plant::ALL.into_iter().find(|p| loot.item(LootKind::Plant(*p)).is_some_and(|i| i.id == id) && progress.plants[p.index()] > 0)
    {
        one_less(&mut progress.plants[plant.index()]);
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
    host.load(r"UI\Singleplayer\Hud\MainHud\MainHud.feu", HUD_LEVEL);
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
    (scripted, cinema, poker, minigame): (
        Res<crate::story::ScriptedPlayer>,
        Res<crate::cinema::Cinema>,
        Option<Res<crate::poker::PokerTable>>,
        Option<Res<crate::hud::Minigame>>,
    ),
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
    // (Not at a poker table: its own movie is the HUD there.)
    hud.shown = *game_state.get() == crate::menu::GameState::Playing
        && !scripted.hud_hidden
        && (cinema.view.is_none() || cinema.free_aim.is_some())
        && !health.dead
        && !poker.is_some_and(|p| p.holds_player())
        && !minigame.is_some_and(|m| m.holds_player());
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
/// The mission cards' status lines: the game's "MISSION FAILED", and its
/// "Complete" (the handbook's) in capitals, as the table has no mission's.
const MISSION_FAILED: u32 = 155587;
const MISSION_COMPLETE: u32 = 158104;

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
    gallery: Option<Res<crate::shooting_gallery::Gallery>>,
    (poker, minigame, races, trials): (
        Option<Res<crate::poker::PokerTable>>,
        Option<Res<crate::hud::Minigame>>,
        Option<Res<crate::races::Races>>,
        Option<Res<crate::trials::Trials>>,
    ),
) {
    let Interface::Running(host) = &*state else { return };
    for card in std::mem::take(&mut ui.cards) {
        match card {
            crate::story::MissionCard::Start { title } => host.event("SetMissionStart", &[&title]),
            crate::story::MissionCard::Complete { title } => host.event("SetMissionTitle", &[&title, &host.text(MISSION_COMPLETE).to_uppercase(), "1"]),
            crate::story::MissionCard::Failed { text } => host.event("SetMissionTitle", &[&text, &host.text(MISSION_FAILED), "0"]),
            crate::story::MissionCard::Event { text, count } => {
                let status = count.map(|(id, n)| host.text(id).replace("%d", &n.to_string())).unwrap_or_default();
                host.event("SetMissionTitle", &[&host.text(text), &status, "1"]);
            }
        }
    }
    let subtitle = ui.subtitle.as_ref().filter(|_| settings.subtitles).map(|s| s.0.clone());
    // (Not while a shooting gallery runs: its count is where the box is; nor
    // while a race or a trial does.)
    let objective = ui.objective.clone().filter(|_| {
        !gallery.is_some_and(|g| g.holds_player())
            && !poker.is_some_and(|p| p.holds_player())
            && !minigame.is_some_and(|m| m.holds_player())
            && !races.is_some_and(|r| r.holds_player())
            && !trials.is_some_and(|t| t.holds_player())
    });
    let now = (objective, ui.mission.clone(), subtitle, hud.shown_prompt.clone());
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

/// The level-up line (`XPLevelUp`): in from frame 2, out from 10, held this
/// long; its text the game's "Skill Point(s) Available".
const LEVEL_UP_IN: u32 = 2;
const LEVEL_UP_OUT: u32 = 10;
const LEVEL_UP_HOLD: f32 = 4.0;
const SKILL_POINT_AVAILABLE: u32 = 149414;
const SKILL_POINTS_AVAILABLE: u32 = 149413;

/// XP gains on the HUD: the kicker plays the tier the amount earns, with
/// the amount and what it was for, holds, then goes. A level gained shows
/// the level-up line a while.
#[allow(clippy::type_complexity)]
fn drive_xp(
    state: NonSend<Interface>,
    time: Res<Time>,
    progress: Res<crate::progression::Progress>,
    mut gains: MessageReader<crate::progression::XpGain>,
    mut showing: Local<Option<(f32, u32)>>,
    mut level: Local<(Option<u32>, Option<f32>)>,
) {
    let Interface::Running(host) = &*state else {
        gains.clear();
        return;
    };
    let (seen, level_until) = &mut *level;
    if seen.is_some_and(|l| progress.level > l) {
        let id = if progress.skill_points > 1 { SKILL_POINTS_AVAILABLE } else { SKILL_POINT_AVAILABLE };
        host.dps_text("XPLevelUpText", &host.text(id));
        host.dps_text("XPLevelUpTextInteractionText", "");
        host.dps_play("XPLevelUp", fc3_ui::ExternalValue::Number(LEVEL_UP_IN as f64));
        *level_until = Some(time.elapsed_secs() + LEVEL_UP_HOLD);
    }
    *seen = Some(progress.level);
    if level_until.is_some_and(|t| time.elapsed_secs() >= t) {
        host.dps_play("XPLevelUp", fc3_ui::ExternalValue::Number(LEVEL_UP_OUT as f64));
        *level_until = None;
    }
    let mut total = 0;
    let mut reason = "";
    for g in gains.read() {
        total += g.amount;
        reason = g.reason;
    }
    if total > 0 {
        let (start, hold, _) = XP_TIERS.iter().rev().find(|t| total >= t.2).copied().unwrap_or(XP_TIERS[0]);
        host.dps_text("XPKickerText", &format!("{total} XP"));
        // (Its line: the game's for a takedown or a headshot, else none.)
        let line = crate::progression::kicker_line(reason).map(|id| host.text(id)).unwrap_or_default();
        host.dps_text("XPBonusKickerText", &line);
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

/// The saving spinner (`SaveGame.feu`, which the HUD and the pause menu
/// each load): `ShowSaving 1 <text>` as a save is written, the game's
/// "Checkpoint reached" at a story checkpoint, else its "Saving...", and
/// `ShowSaving 0` a moment on (on the real clock: the pause menu's runs
/// with the game stopped).
const SAVING: u32 = 161640;
const CHECKPOINT_REACHED: u32 = 159959;
const SAVING_HOLD: f32 = 2.0;

fn drive_saving(state: NonSend<Interface>, time: Res<Time<Real>>, mut saved: MessageReader<crate::save::GameSaved>, mut until: Local<Option<f32>>) {
    let Interface::Running(host) = &*state else {
        saved.clear();
        return;
    };
    if let Some(s) = saved.read().last() {
        host.event("ShowSaving", &["1", &host.text(if s.checkpoint { CHECKPOINT_REACHED } else { SAVING })]);
        *until = Some(time.elapsed_secs() + SAVING_HOLD);
    }
    if until.is_some_and(|t| time.elapsed_secs() >= t) {
        host.event("ShowSaving", &["0", ""]);
        *until = None;
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

/// What was looted, for its line: its shop item's name (the rucksack's),
/// else ours.
fn loot_name(kind: crate::progression::LootKind, catalog: Option<&crate::progression::LootCatalog>) -> String {
    if let Some(item) = catalog.and_then(|c| c.item(kind)) {
        return item.name.clone();
    }
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
    catalog: Option<Res<crate::progression::LootCatalog>>,
    mut hud: ResMut<crate::hud::HudState>,
    mut looted: MessageReader<crate::progression::ItemLooted>,
    mut memory: Local<(Option<u32>, usize, usize, Option<f32>)>,
) {
    let Interface::Running(host) = &*state else {
        looted.clear();
        return;
    };
    let num = |n: u32| fc3_ui::ExternalValue::Number(n as f64);
    let (last_money, loot_line, cash_line, cash_until) = &mut *memory;
    let lines: Vec<String> = looted.read().map(|l| loot_name(l.0, catalog.as_deref())).chain(hud.loot_notes.drain(..).map(|id| host.text(id))).collect();
    for line in lines {
        let n = *loot_line % 2;
        *loot_line += 1;
        host.dps_text(&format!("LootItemText_{n}"), &line);
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

/// The objectives in the world on screen (`UI\Common\Hud\ObjectiveIndicator.feu`,
/// which the HUD loads): up to ten `ObjectiveIndicator_<n>` each where its
/// objective is on the stage ("ObjectiveMarker", frame 2), or, off screen,
/// on a frame round the middle toward it, turned that way
/// ("ObjectiveArrow", frame 1); the waypoint is `PlayerMarkerIndicator`
/// ("Inside" 1, "Outside" 2). No distances: the movie has none.
const INDICATORS: usize = 10;
const INDICATOR_ARROW: u32 = 1;
const INDICATOR_MARKER: u32 = 2;
const WAYPOINT_INSIDE: u32 = 1;
const WAYPOINT_OUTSIDE: u32 = 2;
/// The frame (stage pixels in from each edge) the off-screen arrows sit
/// on, and the way the arrow's art points (degrees, clockwise from right).
const INDICATOR_INSET: f32 = 70.0;
const ARROW_ART: f32 = -90.0;

#[allow(clippy::type_complexity)]
fn drive_indicators(
    state: NonSend<Interface>,
    markers: Res<crate::story::ObjectiveMarkers>,
    waypoint: Res<crate::map::Waypoint>,
    cameras: Query<(&Camera, &GlobalTransform), With<crate::player::PlayerCamera>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    (minigame, races, trials): (Option<Res<crate::hud::Minigame>>, Option<Res<crate::races::Races>>, Option<Res<crate::trials::Trials>>),
    mut last: Local<Vec<Option<(i32, i32, i32, u32)>>>,
) {
    let Interface::Running(host) = &*state else { return };
    let (Ok((cam, cam_t)), Ok(window)) = (cameras.single(), windows.single()) else { return };
    // (A race's next gate with them; while a race or a trial holds the
    // player, the gate alone; while a minigame does, none.)
    let racing = races.as_ref().is_some_and(|r| r.holds_player()) || trials.is_some_and(|t| t.holds_player());
    let mut goals: Vec<Vec3> = markers.0.iter().copied().filter(|_| !racing).chain(races.and_then(|r| r.marker)).collect();
    if minigame.is_some_and(|m| m.holds_player()) {
        goals.clear();
    }
    let (sw, sh) = (fc3_ui::STAGE.0 as f32, fc3_ui::STAGE.1 as f32);
    let scale = (window.width() / sw).min(window.height() / sh).max(0.01);
    let offset = Vec2::new(window.width() - sw * scale, window.height() - sh * scale) / 2.0;
    let (lo, hi) = (Vec2::splat(INDICATOR_INSET), Vec2::new(sw, sh) - INDICATOR_INSET);
    let mid = (lo + hi) * 0.5;
    let view = cam_t.affine().inverse();
    // Where on the stage, and whether there (else held on the frame, turned).
    let place = |at: Vec3| -> (Vec2, Option<f32>) {
        let local = view.transform_point3(at);
        let seen = cam.world_to_viewport(cam_t, at).ok().map(|p| (p - offset) / scale).filter(|p| local.z < 0.0 && p.cmpge(lo).all() && p.cmple(hi).all());
        if let Some(p) = seen {
            return (p, None);
        }
        let mut d = Vec2::new(local.x, -local.y);
        if local.z > 0.0 {
            d.y = d.y.max(0.0);
        }
        if d.length_squared() < 1e-6 {
            d = Vec2::Y;
        }
        let half = (hi - lo) * 0.5;
        let k = (half.x / d.x.abs().max(1e-6)).min(half.y / d.y.abs().max(1e-6));
        (mid + d * k, Some(d.y.atan2(d.x).to_degrees() - ARROW_ART))
    };
    let mut now: Vec<Option<(i32, i32, i32, u32)>> = Vec::with_capacity(INDICATORS + 1);
    for i in 0..INDICATORS {
        now.push(goals.get(i).map(|g| {
            let (p, turn) = place(*g);
            (p.x.round() as i32, p.y.round() as i32, turn.map_or(0, |t| t.round() as i32), if turn.is_some() { INDICATOR_ARROW } else { INDICATOR_MARKER })
        }));
    }
    now.push(waypoint.0.map(|w| {
        let (p, turn) = place(w);
        (p.x.round() as i32, p.y.round() as i32, turn.map_or(0, |t| t.round() as i32), if turn.is_some() { WAYPOINT_OUTSIDE } else { WAYPOINT_INSIDE })
    }));
    if *last == now {
        return;
    }
    let num = |n: f64| fc3_ui::ExternalValue::Number(n);
    for (i, slot) in now.iter().enumerate() {
        if last.get(i) == Some(slot) {
            continue;
        }
        let id = if i < INDICATORS { format!("ObjectiveIndicator_{i}") } else { "PlayerMarkerIndicator".to_string() };
        host.dps_visible(&id, slot.is_some());
        if let Some((x, y, turn, frame)) = *slot {
            host.dps_frame(&id, num(frame as f64));
            host.dps_set(&id, "_x", num(x as f64));
            host.dps_set(&id, "_y", num(y as f64));
            host.dps_set(&id, "_rotation", num(turn as f64));
        }
    }
    *last = now;
}

/// The weapon wheel (`H_Inventory.feu`'s `Wheel`, which the HUD loads)
/// while ours is open (`weapon_wheel`: it picks): in from frame 2 and out
/// from 12, its four places' weapons (`ItemIcon<n>`, top, right, bottom,
/// left, at the weapon's label in `WeaponAndGadgetLib`'s icons: its
/// archetype's name, `weapons:FC3/<name>`; one without is left empty), the
/// throwables carried (`WheelGadgetTextSlot<n>`: grenades, C4, mines,
/// molotovs, the movie's own order), each place's number key
/// (`KeyTextField<n>`), the weapon under the cursor (else in
/// hand) named (`ItemName`), and the arrows on the inner ring
/// (`WheelCurWeaponArrowMC_<n>`, turned about the wheel's centre: at 0 they
/// point right) to the place under the cursor (0, lit) and the weapon in
/// hand (2).
const WHEEL_IN: u32 = 2;
const WHEEL_OUT: u32 = 12;

/// A weapon's icon label in the game's weapon icons.
fn weapon_icon(inv: &crate::weapons::Inventory, def: usize, catalog: &crate::progression::ShopCatalog) -> String {
    if let Some(w) = catalog.weapon(inv, def) {
        if let Some((_, name)) = w.archetype.rsplit_once('/') {
            return name.to_string();
        }
    }
    match inv.defs.get(def).map(|d| d.class) {
        Some(crate::weapons::WeaponClass::Melee) => "Machete".into(),
        _ => String::new(),
    }
}

#[allow(clippy::type_complexity)]
fn drive_wheel(
    state: NonSend<Interface>,
    wheel: Res<crate::weapon_wheel::WeaponWheel>,
    players: Query<&crate::weapons::Inventory, With<crate::player::Player>>,
    catalog: Res<crate::progression::ShopCatalog>,
    mut shown: Local<Option<Option<usize>>>,
) {
    let Interface::Running(host) = &*state else { return };
    let Ok(inv) = players.single() else { return };
    let num = |n: f64| fc3_ui::ExternalValue::Number(n);
    if !wheel.open {
        if shown.take().is_some() {
            host.dps_play("Wheel", num(WHEEL_OUT as f64));
        }
        return;
    }
    let places = crate::weapon_wheel::wheel_slots(inv);
    let turn = |slot: usize| places.iter().position(|&s| s == slot).map(|p| (p as f64 - 1.0) * 90.0);
    if shown.is_none() {
        for p in 0..crate::weapon_wheel::PLACES {
            let id = format!("ItemIcon{p}");
            let label = places.get(p).map(|&s| weapon_icon(inv, inv.slots[s].def, &catalog)).unwrap_or_default();
            host.dps_visible(&id, !label.is_empty());
            if !label.is_empty() {
                host.dps_frame(&id, fc3_ui::ExternalValue::String(label));
            }
        }
        for (i, n) in [inv.grenades, inv.c4, inv.mines, inv.molotovs].into_iter().enumerate() {
            host.dps_text(&format!("WheelGadgetTextSlot{i}"), &n.to_string());
        }
        // (By each place, its number key: `weapon_wheel::SLOT_KEYS`.)
        for p in 0..crate::weapon_wheel::PLACES {
            host.dps_text(&format!("KeyTextField{p}"), &if p < places.len() { (p + 1).to_string() } else { String::new() });
        }
        host.dps_play("Wheel", num(WHEEL_IN as f64));
        host.dps_visible("WheelCurWeaponArrowMC_1", false);
        let current = turn(inv.current);
        host.dps_visible("WheelCurWeaponArrowMC_2", current.is_some());
        host.dps_set("WheelCurWeaponArrowMC_2", "_rotation", num(current.unwrap_or(0.0)));
    }
    if *shown != Some(wheel.selected) {
        let at = wheel.selected.unwrap_or(inv.current);
        let name = inv.slots.get(at).map(|s| {
            let def = s.def;
            catalog.weapon(inv, def).map(|w| host.text(w.name_id)).filter(|t| !t.is_empty()).unwrap_or_else(|| inv.defs[def].name.to_string())
        });
        host.dps_text("ItemName", &name.unwrap_or_default());
        let lit = wheel.selected.and_then(turn);
        host.dps_visible("WheelCurWeaponArrowMC_0", lit.is_some());
        host.dps_set("WheelCurWeaponArrowMC_0", "_rotation", num(lit.unwrap_or(0.0)));
        *shown = Some(wheel.selected);
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

/// A poker table's movie (`ACTI_Poker.feu`, level 4, while the player is
/// at a table: `poker`). Loaded, it asks for its elements
/// (`PokerMovieLoaded`: `LoadPokerElements` with the platform, whether
/// pad icons show, the accept and back keys' text and the help pages'),
/// then the game shows its boxes as the table goes (the difficulty box, the
/// table's own panel, a hand's results, leaving, busted out, the table
/// cleaned) and fills them by DPS id: the four seats' names, chips and
/// state in the table's diagram (P1 the player, then on round the table),
/// the button, the pot, the last bet, the player's chips, the info box's
/// line, the player's action panel (check, call or raise and the bet,
/// the arrows dimmed at its ends), and the player's cards and the board
/// (`UpdateCurrentHandInfo`). A hand's results come in two steps: the box
/// sized for its lines (`ResetEndOfHandResults`), then, once it says it's
/// ready, the hands shown and what each won. The mouse over a box's buttons
/// or a difficulty's row comes as `On...RollOver`.
const POKER_MOVIE: &str = r"UI\Singleplayer\Activities\ACTI_Poker.feu";
const POKER_LEVEL: u32 = 4;
/// The difficulty box's rows' wagers.
const POKER_ROWS: [&str; 4] = [
    "DifficultyBoxWagerAmountFreeText_MC",
    "DifficultyBoxWagerAmountEasyText_MC",
    "DifficultyBoxWagerAmountMediumText_MC",
    "DifficultyBoxWagerAmountHardText_MC",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PokerBox {
    Difficulty,
    Table,
    Results,
    Quit,
    Busted,
    Cleaned,
    CleanedStory,
    Help,
}

/// The help screen's hand probabilities' rows, best first, by their kind of
/// hand (`holdem::Category`'s order).
const POKER_ODDS: [(&str, usize); 10] = [
    ("RoyalFlush", 9),
    ("StraightFlush", 8),
    ("FourOfAKind", 7),
    ("FullHouse", 6),
    ("Flush", 5),
    ("Straight", 4),
    ("ThreeOfAKind", 3),
    ("TwoPair", 2),
    ("Pair", 1),
    ("HighCard", 0),
];

/// A chance as the help screen's rows show it.
fn odds_text(p: f32) -> String {
    match p * 100.0 {
        x if x <= 0.0 => "0%".to_string(),
        x if x < 1.0 => "<1%".to_string(),
        x => format!("{:.0}%", x.min(100.0)),
    }
}

#[derive(Default)]
struct PokerShown {
    loaded: bool,
    elements: bool,
    shown: Option<PokerBox>,
    /// The results box ready for its lines, and them sent.
    results_ready: bool,
    results_sent: bool,
    /// The help screen's page shown.
    help_page: u8,
    sent: Option<crate::poker::PokerHud>,
}

fn drive_poker(state: NonSend<Interface>, calls: Res<UiCalls>, mut poker: ResMut<crate::poker::PokerTable>, mut shown: Local<PokerShown>) {
    use crate::poker::{Line, Name, Panel};
    use fc3_ui::ExternalValue as V;
    let Interface::Running(host) = &*state else { return };
    let Some(hud) = poker.hud.clone() else {
        if shown.loaded {
            host.unload(POKER_LEVEL);
            *shown = PokerShown::default();
            poker.hover = (false, false, None);
        }
        return;
    };
    if !shown.loaded {
        host.load(POKER_MOVIE, POKER_LEVEL);
        shown.loaded = true;
    }
    for call in calls.0.iter().filter(|c| c.command == "Send") {
        let arg = |i: usize| call.args.get(i).map(String::as_str).unwrap_or("");
        match arg(0) {
            // (No key before the accept buttons: there's no room for it.)
            "PokerMovieLoaded" => host.event("LoadPokerElements", &["PC", "0", "", "[Esc] ", "", ""]),
            "PokerElementsLoaded" => {
                shown.elements = true;
                host.event("ShowPokerElements", &[]);
            }
            "OnConfirmRollOver" => poker.hover.0 = true,
            "OnConfirmRollOut" => poker.hover.0 = false,
            "OnCancelRollOver" => poker.hover.1 = true,
            "OnCancelRollOut" => poker.hover.1 = false,
            "OnDifficultyRollOver" => poker.hover.2 = arg(1).parse().ok(),
            "OnDifficultyRollOut" => poker.hover.2 = None,
            "OnPreviousHelpRollOver" => poker.help_hover.0 = true,
            "OnPreviousHelpRollOut" => poker.help_hover.0 = false,
            "OnNextHelpRollOver" => poker.help_hover.1 = true,
            "OnNextHelpRollOut" => poker.help_hover.1 = false,
            "ResetEndOfHandResultsComplete" => shown.results_ready = true,
            _ => {}
        }
    }
    if !shown.elements {
        return;
    }
    let name = |n: &Name| match n {
        Name::Text(id) => host.text(*id),
        Name::Seat(s) => format!("Player {s}"),
    };
    let want = match &hud.panel {
        Panel::Difficulty { .. } => PokerBox::Difficulty,
        Panel::Table => PokerBox::Table,
        Panel::Results(_) => PokerBox::Results,
        Panel::Quit { .. } => PokerBox::Quit,
        Panel::Busted => PokerBox::Busted,
        Panel::Cleaned { .. } => PokerBox::Cleaned,
        Panel::CleanedStory => PokerBox::CleanedStory,
        Panel::Help { .. } => PokerBox::Help,
    };
    if shown.shown != Some(want) {
        debug!("poker ui: {:?} to {want:?}", shown.shown);
        match shown.shown {
            Some(PokerBox::Difficulty) => host.event("HideDifficultyBox", &[]),
            Some(PokerBox::Results) => host.event("HideEndOfHandResultsBox", &[]),
            Some(PokerBox::Quit) => host.event("HideQuitBox", &[]),
            Some(PokerBox::Busted) => host.event("HideRestartBox", &[]),
            Some(PokerBox::Cleaned) => host.event("HideCleanedTableBox", &[]),
            Some(PokerBox::CleanedStory) => host.event("HideCleanedTableSQC100Box", &[]),
            Some(PokerBox::Help) => {
                host.event("HideHelpScreenBox", &[]);
                poker.help_hover = (false, false);
            }
            Some(PokerBox::Table) | None => {}
        }
        match &hud.panel {
            Panel::Difficulty { .. } => {
                host.event("HideMainHUDElements", &[]);
                host.event("ShowDifficultyBox", &[]);
            }
            Panel::Table => host.event("ShowMainHUDElements", &[]),
            Panel::Results(r) => {
                let n = r.lines.len().max(1).to_string();
                host.event("ShowMainHUDElements", &[]);
                host.event("ResetEndOfHandResults", &["1", "0", &n, &n, "0"]);
                shown.results_ready = false;
                shown.results_sent = false;
            }
            Panel::Quit { buy_in, chips } => host.event("ShowQuitBox", &[&buy_in.to_string(), &chips.to_string(), "1"]),
            Panel::Busted => host.event("ShowRestartBox", &[]),
            Panel::Cleaned { won } => host.event("ShowCleanedTableBox", &[&format!("${won}")]),
            Panel::CleanedStory => host.event("ShowCleanedTableSQC100Box", &[]),
            // (It opens on the hand probabilities.)
            Panel::Help { cards, .. } => {
                host.event("ShowHelpScreenBox", &[cards]);
                shown.help_page = 0;
            }
        }
        shown.shown = Some(want);
        shown.sent = None;
    }
    if let Panel::Help { page, .. } = &hud.panel {
        if *page != shown.help_page {
            host.event("ShowSpecificHelpScreen", &[&page.to_string()]);
            shown.help_page = *page;
            shown.sent = None;
        }
    }
    // A hand's results, once the box is sized for them.
    if let (Panel::Results(r), true, false) = (&hud.panel, shown.results_ready, shown.results_sent) {
        let folded = r.lines.iter().all(|l| l.1.is_none());
        let mut revealed: Vec<String> = r
            .lines
            .iter()
            .map(|(n, hand, _)| match hand {
                Some((cards, id)) => format!("{}[{cards}][{id}]", name(n)),
                None => name(n),
            })
            .collect();
        revealed.resize(4, String::new());
        let count = if folded { "0".to_string() } else { r.lines.len().to_string() };
        host.event("UpdateHandsRevealedInfo", &[&count, &revealed[0], &revealed[1], &revealed[2], &revealed[3]]);
        let won = r.lines.iter().filter(|l| matches!(l.2, Line::Won(_))).count();
        let busted = r.lines.iter().filter(|l| l.2 == Line::BustedOut).count();
        let flags: String = r.lines.iter().map(|l| if matches!(l.2, Line::Won(_)) { 'W' } else { 'L' }).collect();
        let mut amounts: Vec<String> = r
            .lines
            .iter()
            .map(|l| match l.2 {
                Line::Won(w) => w.to_string(),
                Line::BustedOut => "BustedOut".to_string(),
                Line::Lost => String::new(),
            })
            .collect();
        amounts.resize(4, String::new());
        host.event("UpdatePlayerWinningsInfo", &[&format!("{won}-{busted}-{flags}"), &amounts[0], &amounts[1], &amounts[2], &amounts[3]]);
        shown.results_sent = true;
    }
    if shown.sent.as_ref() == Some(&hud) {
        return;
    }
    let mut ok = true;
    match &hud.panel {
        Panel::Difficulty { row, wagers, enabled, short, subtitle } => {
            ok &= host.dps_text("DifficultyBoxSubTitle_TX", &host.text(*subtitle));
            for (id, w) in POKER_ROWS.iter().zip(wagers) {
                ok &= host.dps_text(id, &format!("${w}"));
            }
            ok &= host.dps_visible("DifficultyBoxInsufficientFundsText_MC", *short && enabled[*row]);
            ok &= host.dps_visible("DifficultyBoxDifficultyUnavailableText_MC", !enabled[*row]);
            ok &= host.dps_frame("DifficultyBoxSelectionArrows_MC", V::Number([1.0, 10.0, 20.0, 30.0][(*row).min(3)]));
            ok &= host.dps_frame("DifficultyBoxAcceptButton_MC", V::Number(if *short || !enabled[*row] { 10.0 } else { 1.0 }));
        }
        // The hand probabilities: each kind's chance, and the hand held
        // (the box's own lines while no cards are dealt; its note on the
        // hardest difficulty, with no chances).
        Panel::Help { page: 0, hand, odds, .. } => {
            for (kind, i) in POKER_ODDS {
                ok &= host.dps_text(&format!("OddsBox{kind}Text_MC"), &odds.map(|o| odds_text(o[i])).unwrap_or_default());
            }
            if let Some(h) = hand {
                ok &= host.dps_text("OddsCurrentHandInfoText_MC", &host.text(*h));
            }
            if odds.is_some() {
                ok &= host.dps_text("OddsInfoText_MC", "");
            }
        }
        Panel::Help { .. } => {}
        _ => {
            for (i, (n, chips, frame)) in hud.seats.iter().enumerate() {
                ok &= host.dps_text(&format!("P{}Name_MC", i + 1), &name(n));
                ok &= host.dps_text(&format!("P{}Money_MC", i + 1), &if *frame == 20 { String::new() } else { format!("${chips}") });
                ok &= host.dps_frame(&format!("P{}Stats_MC", i + 1), V::Number(*frame as f64));
            }
            ok &= host.dps_visible("TopLeftBox_MC", hud.totals);
            ok &= host.dps_frame("DealerContainer_MC", V::Number([1.0, 20.0, 30.0, 40.0][hud.dealer.min(3)]));
            ok &= host.dps_text("PotCounter_MC", &format!("${}", hud.pot));
            ok &= host.dps_text("LastBetCounter_MC", &format!("${}", hud.last_bet));
            ok &= host.dps_text("PlayerChipCount_MC", &format!("${}", hud.chips));
            let info = hud
                .info
                .as_ref()
                .map(|i| crate::poker::fill(&host.text(i.text), &i.name.as_ref().map(|n| name(n)).unwrap_or_default(), i.chips))
                .unwrap_or_default();
            ok &= host.dps_text("InfoBoxText_MC", &info);
            ok &= host.dps_visible("TopRightBoxB_MC", hud.actions.is_some() && !hud.bet_life);
            ok &= host.dps_visible("TopRightBoxBJAS220_MC", hud.bet_life);
            if let Some((label, bet, lower, higher)) = hud.actions {
                ok &= host.dps_text("Check_MC", &host.text(label).to_uppercase());
                ok &= host.dps_text("BetAmount_MC", &format!("${bet}"));
                let arrows = match (lower, higher) {
                    (true, true) => 1.0,
                    (false, true) => 10.0,
                    (true, false) => 20.0,
                    (false, false) => 30.0,
                };
                ok &= host.dps_frame("BetUpDownArrows_MC", V::Number(arrows));
            }
            if shown.sent.as_ref().map(|s| &s.cards) != Some(&hud.cards) {
                match &hud.cards {
                    Some((cards, folded, kind, board)) => {
                        let flag = |b: bool| if b { "True" } else { "False" };
                        host.event("UpdateCurrentHandInfo", &[cards, flag(*folded), &kind.to_string(), "True", flag(*board)]);
                        ok &= host.dps_visible("CurrentHandBox_MC", true);
                    }
                    None => ok &= host.dps_visible("CurrentHandBox_MC", false),
                }
            }
        }
    }
    // (Until its clips answer, the box is still coming: all again.)
    if ok {
        shown.sent = Some(hud);
    }
}

/// A sharpshooter challenge's HUD (`Acti_Shooting.feu`, level 4, while a
/// gallery runs: `shooting_gallery`). Loaded, it asks for its elements
/// (`ShootingGalleryMovieLoaded`: `LoadShootingGalleryElements` with the
/// platform and the accept and back keys' text), then the game shows its
/// boxes as the gallery goes (`Show...`/`Hide...`: the difficulty box, get
/// ready, the countdown, the bird count and time left, the result) and fills
/// them by DPS id: the wagers, the target, the selection bar's place (its
/// `_y` by row), the count, the seconds left, the result's frame (1 done, 30
/// short of the target) and the money won. The mouse over a row or the back
/// button comes as `OnDifficultyRollOver <row>` and `OnCancelRollOver`.
const GALLERY_MOVIE: &str = r"UI\Singleplayer\Activities\Acti_Shooting.feu";
const GALLERY_LEVEL: u32 = 4;
/// The difficulty box's selection bar's `_y` at each row (practice, novice,
/// skilled, expert): over each row's text.
const GALLERY_ROWS_Y: [f64; 4] = [-23.0, 3.0, 33.0, 61.0];
/// The result's frames: done, and short of the target.
const GALLERY_DONE_FRAME: f64 = 1.0;
const GALLERY_SHORT_FRAME: f64 = 30.0;
/// Seconds left below which the clock goes red.
const GALLERY_HURRY: f32 = 5.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GalleryBox {
    Difficulty,
    Ready,
    Countdown,
    Playing,
    Result,
}

#[derive(Default)]
struct GalleryShown {
    loaded: bool,
    elements: bool,
    shown: Option<GalleryBox>,
    screen: Option<crate::shooting_gallery::Screen>,
    red: bool,
}

fn drive_gallery(state: NonSend<Interface>, calls: Res<UiCalls>, mut gallery: ResMut<crate::shooting_gallery::Gallery>, mut shown: Local<GalleryShown>) {
    use crate::shooting_gallery::Screen;
    use fc3_ui::ExternalValue as V;
    let Interface::Running(host) = &*state else { return };
    let Some(screen) = gallery.screen.clone() else {
        if shown.loaded {
            host.unload(GALLERY_LEVEL);
            *shown = GalleryShown::default();
        }
        return;
    };
    if !shown.loaded {
        host.load(GALLERY_MOVIE, GALLERY_LEVEL);
        shown.loaded = true;
    }
    for call in calls.0.iter().filter(|c| c.command == "Send") {
        let arg = |i: usize| call.args.get(i).map(String::as_str).unwrap_or("");
        match arg(0) {
            "ShootingGalleryMovieLoaded" => host.event("LoadShootingGalleryElements", &["PC", "0", "[Enter] ", "[Esc] "]),
            "ShootingGalleryElementsLoaded" => shown.elements = true,
            "OnDifficultyRollOver" => gallery.hovered = arg(1).parse().ok(),
            "OnDifficultyRollOut" => gallery.hovered = None,
            "OnCancelRollOver" => gallery.cancel_hovered = true,
            "OnCancelRollOut" => gallery.cancel_hovered = false,
            _ => {}
        }
    }
    if !shown.elements {
        return;
    }
    let want = match screen {
        Screen::Choosing { .. } => GalleryBox::Difficulty,
        Screen::Ready => GalleryBox::Ready,
        Screen::Countdown(_) => GalleryBox::Countdown,
        Screen::Playing { .. } => GalleryBox::Playing,
        Screen::Result { .. } => GalleryBox::Result,
    };
    if shown.shown != Some(want) {
        match shown.shown {
            Some(GalleryBox::Difficulty) => host.event("HideDifficultyBox", &[]),
            Some(GalleryBox::Ready) => host.event("HideReadyBox", &[]),
            Some(GalleryBox::Countdown) => host.event("HideCountdownBox", &[]),
            Some(GalleryBox::Playing) => host.event("HideShootingGalleryElements", &[]),
            Some(GalleryBox::Result) => host.event("HideResultBox", &[]),
            None => {}
        }
        match want {
            GalleryBox::Difficulty => host.event("ShowDifficultyBox", &[]),
            GalleryBox::Ready => host.event("ShowReadyBox", &[]),
            GalleryBox::Countdown => host.event("ShowCountdownBox", &[]),
            GalleryBox::Playing => {
                host.event("ShowShootingGalleryElements", &[]);
                host.event("ShowTimeRemainingBox", &[]);
                shown.red = false;
            }
            GalleryBox::Result => host.event("ShowResultBox", &[]),
        }
        shown.shown = Some(want);
        shown.screen = None;
    }
    if shown.screen.as_ref() == Some(&screen) {
        return;
    }
    let money = |n: u32| format!("${n}");
    let mut ok = true;
    match &screen {
        &Screen::Choosing { row, wagers, target, short, subtitle } => {
            ok &= host.dps_text("DifficultyBoxSubTitle_TX", &host.text(subtitle));
            for (id, w) in ["WagerValuePracticeText_MC", "WagerValueEasyText_MC", "WagerValueMedText_MC", "WagerValueHardText_MC"].iter().zip(wagers) {
                ok &= host.dps_text(id, &money(w));
            }
            ok &= host.dps_text("KillCountText_MC", &target.to_string());
            ok &= host.dps_visible("InsufficientFundsText_MC", short);
            ok &= host.dps_set("SelectionArrowsContainer_MC", "_y", V::Number(GALLERY_ROWS_Y[row.min(3)]));
        }
        Screen::Ready => {}
        &Screen::Countdown(n) => ok &= host.dps_text("CountdownBoxText_MC", &n.to_string()),
        &Screen::Playing { kills, target, left, .. } => {
            ok &= host.dps_text("BirdCountBoxText_MC", &kills.to_string());
            ok &= host.dps_text("BirdTargetBoxText_MC", &target.to_string());
            let s = left.ceil() as u32;
            ok &= host.dps_text("TimeRemainingText_MC", &format!("{}:{:02}", s / 60, s % 60));
            if left < GALLERY_HURRY && !shown.red {
                host.event("SetTimeRemainingIconToRed", &[]);
                shown.red = true;
            }
        }
        &Screen::Result { success, kills, target, won } => {
            ok &= host.dps_visible("ResultBox_Clip", true);
            ok &= host.dps_frame("MedalResults_MC", V::Number(if success { GALLERY_DONE_FRAME } else { GALLERY_SHORT_FRAME }));
            ok &= host.dps_text("BirdsKilledText_MC", &kills.to_string());
            ok &= host.dps_text("VictoryWinAmountText_MC", &won.to_string());
            ok &= host.dps_text("FailureBirdCountTextBox_MC", &kills.to_string());
            ok &= host.dps_text("FailTargetBirdCount_MC", &target.to_string());
        }
    }
    // (Until its clips answer, the box is still coming: all again.)
    if ok {
        shown.screen = Some(screen);
    }
}

/// The knife thrower's movie (`ACTI_knifeThrowing.feu`, level 4, while the
/// player is at the game: `knives`). Loaded, it asks for its elements
/// (`KnivesMovieLoaded`: `LoadKnivesElements` with the knives a side, the
/// platform, whether pad icons show and the accept and back keys' text),
/// then the game plays its boxes' loaders in and out by DPS id (`In`,
/// `Out`) and tells each box it's shown (`Show...Box`, which only starts
/// reporting the mouse over its buttons: `OnBetRollOver <row>`,
/// `OnConfirmRollOver`, `OnCancelRollOver`): the wager box, the leave box,
/// won, lost and a draw. It fills them the same way: the wagers, the row's
/// cursor (played to the row's label), not enough money, the amount won or
/// lost. While the game is on, the scores, the knives a side (thrown ones
/// faded, the next blinking), the power bar (a frame a percent), the
/// throw's prompt, the skip prompt over the challenger's throws, and each
/// throw's points popping up as it lands.
const KNIVES_MOVIE: &str = r"UI\Singleplayer\Activities\ACTI_knifeThrowing.feu";
const KNIVES_LEVEL: u32 = 4;
/// The wager box's rows' wagers, and the cursor's labels.
const KNIFE_ROWS: [&str; 4] = ["BetValuePractice_DT", "BetValueNovice_DT", "BetValueSkilled_DT", "BetValueExpert_DT"];
const KNIFE_CURSOR: [&str; 4] = ["Practice", "Novice", "Skilled", "Expert"];
/// The knives a side stand in a row leftward from the power bar's mark for
/// them (`KnifeInitialPosition_MC`, at the bar's left end), this far apart;
/// a throw's points pop up this far over the crosshair (the movie's units).
const KNIFE_PITCH: f64 = 16.0;
const KNIFE_POPUP_RISE: f64 = 70.0;
/// The game's strings: throw knife, skip.
const KNIFE_THROW_TEXT: u32 = 155_735;
const KNIFE_SKIP_TEXT: u32 = 155_730;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KnifeBox {
    Betting,
    Playing,
    Leaving,
    Won,
    Lost,
    Draw,
}

impl KnifeBox {
    /// The box's loader and its name in the movie's events.
    fn loader(self) -> Option<(&'static str, &'static str)> {
        match self {
            KnifeBox::Betting => Some(("BettingLoader_MC", "BettingBox")),
            KnifeBox::Leaving => Some(("CancelMsgBoxLoader_MC", "CancelBox")),
            KnifeBox::Won => Some(("WinMsgBoxLoader_MC", "WinBox")),
            KnifeBox::Lost => Some(("LosingMsgBoxLoader_MC", "LoseBox")),
            KnifeBox::Draw => Some(("DrawMsgBoxLoader_MC", "DrawBox")),
            KnifeBox::Playing => None,
        }
    }
}

#[derive(Default)]
struct KnivesShown {
    loaded: bool,
    elements: bool,
    shown: Option<KnifeBox>,
    /// The score, the knives, the power bar and the crosshair played in.
    score: bool,
    knives: bool,
    bar: bool,
    crosshair: bool,
    /// Throws whose points have popped up.
    landed: u32,
    /// The throw's prompt and the skip's shown.
    prompts: Option<(bool, bool)>,
    sent: Option<crate::knives::KnifeHud>,
}

fn drive_knives(state: NonSend<Interface>, calls: Res<UiCalls>, mut knives: ResMut<crate::knives::KnifeGame>, mut shown: Local<KnivesShown>) {
    use crate::knives::KnifePanel;
    use fc3_ui::ExternalValue as V;
    let Interface::Running(host) = &*state else { return };
    let shown = &mut *shown;
    let Some(hud) = knives.hud.clone() else {
        if shown.loaded {
            host.unload(KNIVES_LEVEL);
            *shown = KnivesShown::default();
            knives.hover = (false, false, None);
        }
        return;
    };
    if !shown.loaded {
        host.load(KNIVES_MOVIE, KNIVES_LEVEL);
        shown.loaded = true;
    }
    for call in calls.0.iter().filter(|c| c.command == "Send") {
        let arg = |i: usize| call.args.get(i).map(String::as_str).unwrap_or("");
        match arg(0) {
            "KnivesMovieLoaded" => host.event("LoadKnivesElements", &[&hud.knives.to_string(), "PC", "0", "[Enter] ", "[Esc] "]),
            "KnivesElementsLoaded" => {
                shown.elements = true;
                for id in ["CrosshairLoader_MC", "KnifeInitialPosition_MC", "BarAnim_MC", "BarAnimLoader_MC", "KnifeContainer_MC0", "PointsPopUp_MC"] {
                    debug!("knives ui: {id}: {}", host.dps_info(id));
                }
            }
            "OnConfirmRollOver" => knives.hover.0 = true,
            "OnConfirmRollOut" => knives.hover.0 = false,
            "OnCancelRollOver" => knives.hover.1 = true,
            "OnCancelRollOut" => knives.hover.1 = false,
            "OnBetRollOver" => knives.hover.2 = arg(1).parse().ok(),
            "OnBetRollOut" => knives.hover.2 = None,
            _ => {}
        }
    }
    if !shown.elements {
        return;
    }
    let label = |s: &str| V::String(s.to_string());
    let want = match hud.panel {
        KnifePanel::Betting { .. } => KnifeBox::Betting,
        KnifePanel::Playing => KnifeBox::Playing,
        KnifePanel::Leaving => KnifeBox::Leaving,
        KnifePanel::Won(_) => KnifeBox::Won,
        KnifePanel::Lost(_) => KnifeBox::Lost,
        KnifePanel::Draw => KnifeBox::Draw,
    };
    if shown.shown != Some(want) {
        debug!("knives ui: {:?} to {want:?}", shown.shown);
        if let Some((clip, name)) = shown.shown.and_then(KnifeBox::loader) {
            host.dps_play(clip, label("Out"));
            host.event(&format!("Hide{name}"), &[]);
        }
        if let Some((clip, name)) = want.loader() {
            host.dps_play(clip, label("In"));
            host.event(&format!("Show{name}"), &[]);
        }
        shown.shown = Some(want);
        shown.sent = None;
    }
    // The game's elements: the score from the first throw on, the knives
    // and the power bar while it's played.
    let playing = matches!(want, KnifeBox::Playing | KnifeBox::Leaving);
    let mut ok = true;
    for (clip, on, was) in [
        ("ScoreLoader_MC", want != KnifeBox::Betting, &mut shown.score),
        ("BarAnimLoader_MC", playing && hud.player_turn, &mut shown.bar),
        ("CrosshairLoader_MC", want == KnifeBox::Playing && hud.player_turn, &mut shown.crosshair),
    ] {
        if on != *was {
            ok &= host.dps_play(clip, label(if on { "In" } else { "Out" }));
            *was = on;
        }
    }
    // (The movie adds the knives and the popup at its top left: the game
    // placed them, the knives from the bar's mark, the popup over the
    // crosshair.)
    let number = |id: &str, property: &str, or: f64| match host.dps_get(id, property) {
        V::Number(n) if n.is_finite() => n,
        _ => or,
    };
    if playing && !shown.knives {
        let place = |id: &str| (number(id, "_x", 0.0), number(id, "_y", 0.0), number(id, "_xscale", 100.0) / 100.0, number(id, "_yscale", 100.0) / 100.0);
        let ((lx, ly, lsx, lsy), (bx, by, bsx, bsy), (mx, my, _, _)) = (place("BarAnimLoader_MC"), place("BarAnim_MC"), place("KnifeInitialPosition_MC"));
        let (x, y) = (lx + lsx * (bx + bsx * mx), ly + lsy * (by + bsy * my));
        for i in 0..hud.knives {
            ok &= host.dps_set(&format!("KnifeContainer_MC{i}"), "_x", V::Number(x - KNIFE_PITCH * i as f64));
            ok &= host.dps_set(&format!("KnifeContainer_MC{i}"), "_y", V::Number(y));
        }
        // (Centred: its text stands right of its origin.)
        let middle = host.dps_bounds("PointsPopUp_MC").map_or(0.0, |b| (b[0] + b[2]) / 2.0 - number("PointsPopUp_MC", "_x", 0.0));
        ok &= host.dps_set("PointsPopUp_MC", "_x", V::Number(number("CrosshairLoader_MC", "_x", 640.0) - middle));
        ok &= host.dps_set("PointsPopUp_MC", "_y", V::Number(number("CrosshairLoader_MC", "_y", 360.0) - KNIFE_POPUP_RISE));
    }
    if playing != shown.knives {
        for i in 0..hud.knives {
            ok &= host.dps_play(&format!("KnifeContainer_MC{i}"), label(if playing { "In" } else { "Out" }));
        }
        shown.knives = playing;
        shown.sent = None;
    }
    // Each throw's points as it lands.
    if hud.landed < shown.landed {
        shown.landed = hud.landed;
    }
    if hud.landed > shown.landed {
        // (The movie has the plus.)
        ok &= host.dps_text("PointsPopUpText_MC", &hud.points.to_string());
        ok &= host.dps_play("PointsPopUp_MC", V::Number(2.0));
        shown.landed = hud.landed;
    }
    let old = shown.sent.clone();
    let changed = |f: &dyn Fn(&crate::knives::KnifeHud) -> bool| old.as_ref().is_none_or(|o| f(o));
    match &hud.panel {
        KnifePanel::Betting { row, wagers, short, subtitle } => {
            if changed(&|o| o.panel != hud.panel) {
                ok &= host.dps_text("DifficultyBoxSubTitle_TX", &host.text(*subtitle));
                for (id, w) in KNIFE_ROWS.iter().zip(wagers) {
                    ok &= host.dps_text(id, &format!("${w}"));
                }
                ok &= host.dps_visible("NotEnoughMoney_MC", *short);
                ok &= host.dps_frame("PromptAcceptContainer_MC", V::Number(if *short { 10.0 } else { 1.0 }));
            }
            let was = old.as_ref().and_then(|o| match o.panel {
                KnifePanel::Betting { row, .. } => Some(row),
                _ => None,
            });
            if was != Some(*row) {
                ok &= host.dps_play("WagerCursor_MC", label(KNIFE_CURSOR[(*row).min(3)]));
            }
        }
        KnifePanel::Won(w) => ok &= host.dps_text("WinAmount_DT", &format!("+ {w} $")),
        KnifePanel::Lost(w) => ok &= host.dps_text("LostAmount_DT", &format!("- {w} $")),
        _ => {}
    }
    if changed(&|o| o.scores != hud.scores) {
        ok &= host.dps_text("PointsPlayer_DT", &hud.scores[0].to_string());
        ok &= host.dps_text("PointsNpc_DT", &hud.scores[1].to_string());
    }
    // The knives a side: thrown ones faded, the player's next blinking.
    if playing && changed(&|o| o.thrown != hud.thrown || o.player_turn != hud.player_turn) {
        for i in 0..hud.knives {
            let clip = format!("Knife_MC{i}");
            ok &= match i.cmp(&hud.thrown) {
                std::cmp::Ordering::Less => host.dps_frame(&clip, label("Faded")),
                std::cmp::Ordering::Equal if hud.player_turn => host.dps_play(&clip, label("Blink")),
                _ => host.dps_frame(&clip, label("Normal")),
            };
        }
    }
    if changed(&|o| o.power != hud.power) {
        ok &= host.dps_frame("BarAnim_MC", V::Number(1.0 + (hud.power.unwrap_or(0.0).clamp(0.0, 1.0) * 99.0).round() as f64));
    }
    // The throw's prompt between throws, the skip's over the challenger's.
    let prompts = (want == KnifeBox::Playing && hud.player_turn && hud.power.is_none(), hud.skip);
    if old.is_none() || shown.prompts != Some(prompts) {
        ok &= host.dps_visible("ThrowKnifePrompt_MC", prompts.0);
        ok &= host.dps_text("ThrowKnifeButtonText_MC", &format!("[{}] {}", hud.throw_key, host.text(KNIFE_THROW_TEXT).to_uppercase()));
        ok &= host.dps_visible("SkipCinematicBox_MC", prompts.1);
        ok &= host.dps_text("SkipButtonText_MC", &format!("[Space] {}", host.text(KNIFE_SKIP_TEXT).to_uppercase()));
        shown.prompts = Some(prompts);
    }
    // (Until its clips answer, the box is still coming: all again.)
    shown.sent = if ok { Some(hud) } else { None };
}

/// A race's HUD (`ACTI_Driving.feu`, level 4, while a race runs: `races`).
/// Loaded, it asks for its elements (`DrivingMovieLoaded`:
/// `LoadDrivingElements` with the platform, whether pad icons show and the
/// accept and back keys' text), then the game shows its boxes by event as
/// the race goes (`Show...`/`Hide...`: the wager box, the ready box and the
/// count in the general text box, the race's own (the clock and the gates,
/// `ShowCheckpointDrivingElements`), the quit box, the result) and fills
/// them by DPS id: the wagers, the row's target time, not enough money, the
/// track's name, the cursor (played from the row's frame), the clock (red
/// in its last ten seconds), the gates passed, the count and "go", and the
/// result's medal frame, status, times and winnings. The mouse over a row
/// or a box's buttons comes as `OnDifficultyRollOver <row>`,
/// `OnConfirmRollOver`, `OnCancelRollOver`.
const RACE_MOVIE: &str = r"UI\Singleplayer\Activities\ACTI_Driving.feu";
const RACE_LEVEL: u32 = 4;
const RACE_ROWS: [&str; 4] = ["WagerValuePracticeText_MC", "WagerValueEasyText_MC", "WagerValueMedText_MC", "WagerValueHardText_MC"];
/// The clock's colour in its last seconds, the result's medal frames (won,
/// lost) and the game's strings: go, the challenge complete, failed.
const RACE_HURRY_COLOUR: f64 = 0xDA_4024 as f64;
const RACE_WON_FRAME: f64 = 20.0;
const RACE_LOST_FRAME: f64 = 30.0;
const RACE_GO: u32 = 154_848;
const RACE_WON: u32 = 154_847;
const RACE_LOST: u32 = 163_266;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RaceBox {
    Choosing,
    Countdown,
    Racing,
    Quit,
    Result,
}

#[derive(Default)]
struct RacesShown {
    loaded: bool,
    elements: bool,
    shown: Option<RaceBox>,
    /// The ready box and the general text box up.
    ready: bool,
    general: bool,
    /// The clock's own colour (put back after the red).
    colour: Option<f64>,
    sent: Option<crate::races::RaceHud>,
}

fn drive_races(state: NonSend<Interface>, calls: Res<UiCalls>, mut races: ResMut<crate::races::Races>, mut shown: Local<RacesShown>) {
    use crate::races::{race_time, RaceHud};
    use fc3_ui::ExternalValue as V;
    let Interface::Running(host) = &*state else { return };
    let shown = &mut *shown;
    let Some(hud) = races.hud.clone() else {
        if shown.loaded {
            host.unload(RACE_LEVEL);
            *shown = RacesShown::default();
            races.hover = (None, false, false);
        }
        return;
    };
    if !shown.loaded {
        host.load(RACE_MOVIE, RACE_LEVEL);
        shown.loaded = true;
    }
    for call in calls.0.iter().filter(|c| c.command == "Send") {
        let arg = |i: usize| call.args.get(i).map(String::as_str).unwrap_or("");
        match arg(0) {
            "DrivingMovieLoaded" => host.event("LoadDrivingElements", &["PC", "0", "", ""]),
            "DrivingElementsLoaded" => shown.elements = true,
            "OnDifficultyRollOver" => races.hover.0 = arg(1).parse().ok(),
            "OnDifficultyRollOut" => races.hover.0 = None,
            "OnConfirmRollOver" => races.hover.1 = true,
            "OnConfirmRollOut" => races.hover.1 = false,
            "OnCancelRollOver" => races.hover.2 = true,
            "OnCancelRollOut" => races.hover.2 = false,
            _ => {}
        }
    }
    if !shown.elements {
        return;
    }
    let want = match hud {
        RaceHud::Choosing { .. } => RaceBox::Choosing,
        RaceHud::Ready { .. } | RaceHud::Count { .. } => RaceBox::Countdown,
        RaceHud::Racing { quitting: true, .. } => RaceBox::Quit,
        RaceHud::Racing { .. } => RaceBox::Racing,
        RaceHud::Result { .. } => RaceBox::Result,
    };
    if shown.shown != Some(want) {
        debug!("race ui: {:?} to {want:?}", shown.shown);
        match shown.shown {
            Some(RaceBox::Choosing) => host.event("HideDifficultyBox", &[]),
            Some(RaceBox::Quit) => host.event("HideQuitBox", &[]),
            Some(RaceBox::Result) => host.event("HideResultBox", &[]),
            _ => {}
        }
        match want {
            RaceBox::Choosing => {
                host.event("HideDrivingElements", &[]);
                host.event("ShowDifficultyBox", &[]);
            }
            RaceBox::Countdown | RaceBox::Racing => host.event("ShowCheckpointDrivingElements", &[]),
            RaceBox::Quit => host.event("ShowQuitBox", &[]),
            RaceBox::Result => {
                host.event("HideDrivingElements", &[]);
                host.event("ShowResultBox", &[]);
            }
        }
        shown.shown = Some(want);
        shown.sent = None;
        races.hover = (None, false, false);
    }
    // The ready box, then the count and "go" in the general text box.
    let ready = matches!(hud, RaceHud::Ready { .. });
    if ready != shown.ready {
        host.event(if ready { "ShowReadyBox" } else { "HideReadyBox" }, &[]);
        shown.ready = ready;
    }
    let general = match hud {
        RaceHud::Count { n, .. } => Some(n.to_string()),
        RaceHud::Racing { go: true, quitting: false, .. } => Some(host.text(RACE_GO)),
        _ => None,
    };
    if general.is_some() != shown.general {
        host.event(if general.is_some() { "ShowGeneralTextBox" } else { "HideGeneralTextBox" }, &[]);
        shown.general = general.is_some();
        shown.sent = None;
    }
    if shown.sent.as_ref() == Some(&hud) {
        return;
    }
    let old = shown.sent.clone();
    let mut ok = true;
    if let Some(text) = &general {
        ok &= host.dps_text("MessageBoxText_MC", text);
    }
    match &hud {
        RaceHud::Choosing { row, wagers, targets, short, subtitle } => {
            ok &= host.dps_text("DifficultyBoxSubTitle_TX", &host.text(*subtitle));
            for (id, w) in RACE_ROWS.iter().zip(wagers) {
                ok &= host.dps_text(id, &format!("${w}"));
            }
            ok &= host.dps_visible("InsufficientFundsText_MC", *short);
            ok &= host.dps_text("WagerSelectionTargetTime_MC", &race_time(targets[(*row).min(3)]));
            let was = old.as_ref().and_then(|o| match o {
                RaceHud::Choosing { row, .. } => Some(*row),
                _ => None,
            });
            if was != Some(*row) {
                ok &= host.dps_play("SelectionArrowsContainer_MC", V::Number([1.0, 10.0, 20.0, 30.0][(*row).min(3)]));
            }
        }
        RaceHud::Racing { left, hurry, passed, gates, .. } => {
            ok &= host.dps_text("TimeBoxText_MC", &race_time(*left));
            ok &= host.dps_text("CheckpointBoxText_MC", &format!("{passed}/{gates}"));
            let was = old.as_ref().is_some_and(|o| matches!(o, RaceHud::Racing { hurry: true, .. }));
            if shown.colour.is_none() {
                if let V::Number(c) = host.dps_get("TimeBoxText_MC", "textColor") {
                    shown.colour = Some(c);
                }
            }
            if *hurry != was || old.is_none() {
                let colour = if *hurry { RACE_HURRY_COLOUR } else { shown.colour.unwrap_or(16_777_215.0) };
                ok &= host.dps_set("TimeBoxText_MC", "textColor", V::Number(colour));
            }
        }
        RaceHud::Result { won, target, left, money } => {
            ok &= host.dps_frame("MedalResults_MC", V::Number(if *won { RACE_WON_FRAME } else { RACE_LOST_FRAME }));
            ok &= host.dps_text("RaceStatus_MC", &host.text(if *won { RACE_WON } else { RACE_LOST }));
            ok &= host.dps_text("MyTime_MC", &race_time(*target));
            ok &= host.dps_text("FailureMyTime_MC", &race_time(*target));
            ok &= host.dps_text("FinalTime_MC", &race_time(*left));
            ok &= host.dps_text("WinAmount_MC", &format!("${money}"));
        }
        // (The clock and the gates as they'll start.)
        RaceHud::Ready { target, gates } | RaceHud::Count { target, gates, .. } => {
            ok &= host.dps_text("TimeBoxText_MC", &race_time(*target));
            ok &= host.dps_text("CheckpointBoxText_MC", &format!("0/{gates}"));
        }
    }
    // (Until its clips answer, the box is still coming: all again.)
    if ok {
        shown.sent = Some(hud);
    }
}

/// The end credits (`EndCredits`, over all but the pause menu): loaded as
/// the story asks for them (`crate::credits`), the HUD hidden under it, and
/// once it says it's loaded (`OnCreditsLoaded`) sent the credits' entries,
/// a few hundred a frame (`AddCreditItem`: an id, the kind's number and the
/// text; it rolls from the first). It says when they've scrolled by
/// (`OnEndCreditsDone`), as it does when a skip cuts them short (its own
/// `quitBeforeEnd`: they fade out).
const CREDITS_MOVIE: &str = r"UI\Singleplayer\Menu\Credits\EndCredits.feu";
const CREDITS_LEVEL: u32 = 9;
const CREDITS_PER_FRAME: usize = 250;

/// The credits being rolled: their entries, how many were sent (once the
/// movie has asked), and whether it was told to cut them short.
struct Roll {
    entries: Vec<fc3_formats::credits::Entry>,
    sent: Option<usize>,
    skipped: bool,
}

fn drive_credits(
    state: NonSend<Interface>,
    mut credits: ResMut<crate::credits::EndCredits>,
    calls: Res<UiCalls>,
    info: Res<crate::data::CacheInfo>,
    mut roll: Local<Option<Roll>>,
) {
    let Interface::Running(host) = &*state else { return };
    if !credits.on() {
        if roll.take().is_some() {
            host.unload(CREDITS_LEVEL);
            host.level_visible(HUD_LEVEL, true);
        }
        return;
    }
    if roll.is_none() {
        let entries = std::fs::read_to_string(info.root.join(fc3_formats::credits::CACHE_REL)).map(|t| fc3_formats::credits::from_text(&t)).unwrap_or_default();
        // (With none, nothing would end them: the story goes on without.)
        if entries.is_empty() {
            warn!("original UI: no credits in the cache ({})", fc3_formats::credits::CACHE_REL);
        } else {
            host.load(CREDITS_MOVIE, CREDITS_LEVEL);
            host.level_visible(HUD_LEVEL, false);
            credits.rolling = true;
        }
        *roll = Some(Roll { entries, sent: None, skipped: false });
    }
    let Some(roll) = roll.as_mut() else { return };
    for call in calls.0.iter().filter(|c| c.command == "Send") {
        match call.args.first().map(String::as_str) {
            Some("OnCreditsLoaded") if roll.sent.is_none() => roll.sent = Some(0),
            Some("OnEndCreditsDone") => credits.over = true,
            _ => {}
        }
    }
    if let Some(sent) = roll.sent.as_mut() {
        for (i, e) in roll.entries.iter().enumerate().skip(*sent).take(CREDITS_PER_FRAME) {
            host.event("AddCreditItem", &[&i.to_string(), &e.kind.code().to_string(), &e.text]);
        }
        *sent = (*sent + CREDITS_PER_FRAME).min(roll.entries.len());
    }
    if credits.skipped.is_some() && !roll.skipped {
        roll.skipped = true;
        host.call_function(&format!("_level{CREDITS_LEVEL}.ec.m_credits.quitBeforeEnd"), Vec::new());
    }
}

/// The loading screen (`Loading\GameModes\Singleplayer.feu`): the game's
/// own, over everything, while a jump keeps the player waiting for the
/// world (the start of play, a checkpoint reloaded, fast travel, a story's
/// teleport into ground not yet in: `Player::spawning` for more than
/// `LOADING_AFTER`), until the world around him has settled (the ground
/// under him, and no terrain, objects or vegetation still to come in) and
/// it has been up `LOADING_LEAST`. The movie plays itself (its words, the
/// game's tips, the spinner it loads, `Loading.feu`); it asks only
/// `IsBootTitle` ("1" is the boot title's guns instead). The HUD hides
/// under it. `FC3_LOADING_SHOT=<file>` (tests) captures it two seconds in.
const LOADING_MOVIE: &str = r"UI\Singleplayer\Menu\Loading\GameModes\Singleplayer.feu";
const LOADING_LEVEL: u32 = 60;
const LOADING_AFTER: f32 = 0.5;
const LOADING_LEAST: f32 = 2.0;
/// The longest it stays, should something never settle.
const LOADING_MOST: f32 = 60.0;
/// How long the world stays settled before it goes; longer for the one that
/// starts play (the story places the player at its checkpoint a few seconds
/// in, which would bring it up again).
const LOADING_CALM: f32 = 0.5;
const LOADING_CALM_START: f32 = 4.0;
const LOADING_START: f32 = 10.0;

#[derive(Default)]
struct LoadingShown {
    waiting: f32,
    since: Option<f32>,
    shot: bool,
    calm: f32,
    /// When play last began (from the menus or loading), and whether the
    /// screen up came with it.
    playing: Option<f32>,
    starts_play: bool,
}

#[allow(clippy::too_many_arguments)]
fn drive_loading(
    state: NonSend<Interface>,
    time: Res<Time<Real>>,
    game: Res<State<crate::menu::GameState>>,
    players: Query<&crate::player::Player>,
    (terrain, objects, vegetation): (Res<crate::terrain::TerrainStreamer>, Res<crate::objects::ObjectStreamer>, Res<crate::vegetation::VegStreamer>),
    mut shown: Local<LoadingShown>,
    mut commands: Commands,
) {
    let Interface::Running(host) = &*state else { return };
    let now = time.elapsed_secs();
    let spawning = players.single().is_ok_and(|p| p.spawning);
    match game.get() {
        crate::menu::GameState::MainMenu | crate::menu::GameState::Loading => shown.playing = None,
        crate::menu::GameState::Playing if shown.playing.is_none() => shown.playing = Some(now),
        _ => {}
    }
    let Some(since) = shown.since else {
        if spawning && *game.get() == crate::menu::GameState::Playing {
            shown.waiting += time.delta_secs();
        } else {
            shown.waiting = 0.0;
        }
        if shown.waiting > LOADING_AFTER {
            host.answer("IsBootTitle", "0");
            host.load(LOADING_MOVIE, LOADING_LEVEL);
            host.level_visible(HUD_LEVEL, false);
            shown.since = Some(now);
            shown.shot = false;
            shown.calm = 0.0;
            shown.starts_play = shown.playing.is_some_and(|t| now - t < LOADING_START);
            debug!("original UI: loading screen up");
        }
        return;
    };
    if !shown.shot && now - since > 2.0 {
        shown.shot = true;
        if let Ok(path) = std::env::var("FC3_LOADING_SHOT") {
            crate::camera::capture(&mut commands, path);
        }
    }
    let settled = !spawning && terrain.pending_count() == 0 && objects.pending_count() == 0 && vegetation.pending_count() == 0;
    shown.calm = if settled { shown.calm + time.delta_secs() } else { 0.0 };
    let calm = if shown.starts_play { LOADING_CALM_START } else { LOADING_CALM };
    if (shown.calm > calm && now - since > LOADING_LEAST) || now - since > LOADING_MOST {
        host.unload(LOADING_LEVEL);
        host.level_visible(HUD_LEVEL, *game.get() == crate::menu::GameState::Playing);
        shown.since = None;
        shown.waiting = 0.0;
        debug!("original UI: loading screen down after {:.1} s", now - since);
    }
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
    (hud, pickups, races, trials): (
        Res<crate::map::HudParams>,
        Query<(&GlobalTransform, &crate::pickups::Pickup)>,
        Option<Res<crate::races::Races>>,
        Option<Res<crate::trials::Trials>>,
    ),
    mut last: Local<Option<Radar>>,
    (story, time, mut standing): (Option<NonSend<crate::story::StoryHost>>, Res<Time<Real>>, Local<(f32, Vec<(f64, Vec3)>)>),
    mut plants: Local<Option<std::collections::HashMap<crate::syringes::Plant, &'static str>>>,
) {
    let Interface::Running(host) = &*state else { return };
    let Ok((at, player)) = players.single() else { return };
    // The story's markers: the standing ones looked over once a second.
    let now_secs = time.elapsed_secs();
    // (With the safehouses the story opens, to travel to or not yet.)
    if let Some(h) = story.as_ref().filter(|_| now_secs >= standing.0) {
        let mut markers = standing_markers(h);
        let safehouses = h.story.travel_destinations();
        markers
            .extend(safehouses.iter().map(|d| (if d.unlocked { map_icon::SAFEHOUSE } else { map_icon::SAFEHOUSE_LOCKED }, crate::data::to_bevy(d.position))));
        *standing = (now_secs + 1.0, markers);
    }
    let scripted = story.as_ref().map(|h| scripted_markers(h));
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
        // (A liberated outpost's own safehouse stands for it.)
        let own = o.liberated && standing.1.iter().any(|(k, p)| *k == map_icon::SAFEHOUSE && o.contains(*p));
        if on_disc(l) && !own {
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
    // Activities, shops, the challenge boards, and the quest and mission
    // givers a script shows.
    for (kind, place) in standing.1.iter().chain(scripted.iter().flatten()) {
        let l = local(*place);
        if let Some(label) = radar_label(*kind).filter(|_| on_disc(l)) {
            places.push((l.length(), marker(l, label)));
        }
    }
    if plants.is_none() {
        *plants = story.as_ref().map(|h| plant_icons(h));
    }
    let mut shown: Vec<(Vec3, &str)> = Vec::new();
    if hud.0.collectibles > 0.0 {
        for (t, pk) in &pickups {
            let label = match pk.kind {
                crate::pickups::PickupKind::Relic => "Relic",
                crate::pickups::PickupKind::Letter => "Letter",
                crate::pickups::PickupKind::MemoryCard => "Sdcard",
                // (Plants by their colour, as their pickups name it.)
                crate::pickups::PickupKind::Plant(p) => match plants.as_ref().and_then(|m| m.get(&p)) {
                    Some(label) => label,
                    None => continue,
                },
                _ => continue,
            };
            let d = t.translation() - at.translation;
            let l = local(t.translation());
            // (A plant is two pickups, its own and its look: one marker.)
            let twin = shown.iter().any(|(p, lb): &(Vec3, &str)| *lb == label && p.distance(t.translation()) < 1.0);
            if Vec2::new(d.x, d.z).length() <= hud.0.collectibles && on_disc(l) && !twin {
                shown.push((t.translation(), label));
                places.push((l.length(), marker(l, label)));
            }
        }
    }
    fill(places, &mut now.places);
    // Objectives: held on the rim when further than the disc shows.
    let mut marks = Vec::new();
    // (The plain objectives: givers are places.)
    let mut goals: Vec<Vec3> = match &scripted {
        Some(s) => s.iter().filter(|(k, _)| *k == map_icon::MISSION).map(|(_, p)| *p).collect(),
        None => objectives.0.clone(),
    };
    // (A race's next gate; while a race holds the player, the gate alone;
    // while a trial does, nothing.)
    if races.as_ref().is_some_and(|r| r.holds_player()) || trials.as_ref().is_some_and(|t| t.holds_player()) {
        goals.clear();
    }
    goals.extend(races.as_ref().and_then(|r| r.marker));
    for &goal in &goals {
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

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::progression::{Ability, Branch, SkillTree};

    /// The skills page buys by tree and index in it, in the tree's order,
    /// and only what can be learned now.
    #[test]
    fn a_skill_is_bought_by_its_tree_and_index() {
        let ability = |id: &str, branch: u32, requires: &[&str]| Ability {
            id: id.into(),
            branch,
            cost: 1,
            requires: requires.iter().map(|r| r.to_string()).collect(),
            purchasable: true,
            available: true,
            ..Default::default()
        };
        let branch = |number: u32| Branch { number, ..Default::default() };
        let skills = crate::progression::Skills(SkillTree {
            branches: vec![branch(1), branch(2)],
            abilities: vec![ability("a_root", 1, &[]), ability("b_root", 2, &[]), ability("a_next", 1, &["a_root"])],
            ..Default::default()
        });
        let mut progress = crate::progression::Progress { skill_points: 1, ..Default::default() };
        // The first tree's second is `a_next`, which waits for its root.
        assert!(!buy_skill(&mut progress, &skills, 0, 1));
        assert!(buy_skill(&mut progress, &skills, 0, 0));
        assert!(progress.skills.contains("a_root") && progress.skill_points == 0);
        // No points left; no such tree.
        assert!(!buy_skill(&mut progress, &skills, 1, 0));
        progress.skill_points = 1;
        assert!(!buy_skill(&mut progress, &skills, 5, 0));
        assert!(buy_skill(&mut progress, &skills, 0, 1));
    }

    /// The movie knows tattoo parts by the CRC-32 of their names.
    #[test]
    fn tattoo_parts_are_named_as_the_skills_movie_knows_them() {
        assert_eq!(fc3_formats::hash::crc32(b"FirstSkillHeron"), 3509486617);
        assert_eq!(fc3_formats::hash::crc32(b"Spider3"), 1221601108);
    }
}
