//! Game states with a main menu and a pause menu.
//!
//! The world streams and animates behind the main menu, but enemies only act
//! while `Playing`. Pausing freezes virtual time, which stops gameplay,
//! physics and animation together.

use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::hud::HudState;
use crate::player::InputCaptured;
use crate::Args;

#[derive(States, Default, Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum GameState {
    /// Waiting for the converted cache (first-launch conversion).
    #[default]
    Loading,
    MainMenu,
    Playing,
    Paused,
    /// The full-screen world map (gameplay paused).
    Map,
    /// Skills, crafting and stats (gameplay paused).
    Journal,
    /// Buying at a liberated outpost (gameplay paused).
    Shop,
}

pub struct MenuPlugin;

#[cfg(test)]
mod tutorial_tests {
    use super::*;
    use crate::progression::{CraftTutorial, PauseMenu};

    #[test]
    fn escape_and_gamepad_back_cannot_skip_crafting_but_can_exit_when_done() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).insert_resource(State::new(GameState::Journal)).init_resource::<NextState<GameState>>()
            .init_resource::<ButtonInput<KeyCode>>().init_resource::<crate::options::Rebinding>()
            .init_resource::<crate::options::OptionsOpen>()
            .init_resource::<OriginalPause>()
            .insert_resource(PauseMenu { tutorial: Some(CraftTutorial::Medicine(0)), ..default() })
            .add_systems(Update, pause_toggle);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::Escape);
        app.update();
        assert!(matches!(app.world().resource::<NextState<GameState>>(), NextState::Unchanged));
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        let mut pad = Gamepad::default();
        pad.digital_mut().press(bevy::input::gamepad::GamepadButton::East);
        app.world_mut().spawn(pad);
        app.update();
        assert!(matches!(app.world().resource::<NextState<GameState>>(), NextState::Unchanged));
        app.world_mut().resource_mut::<PauseMenu>().tutorial = Some(CraftTutorial::Done);
        app.update();
        assert!(matches!(app.world().resource::<NextState<GameState>>(), NextState::Pending(GameState::Playing)));
        assert!(app.world().resource::<PauseMenu>().tutorial.is_none());
    }
}

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<GameState>()
            .add_systems(Startup, (setup_menu, skip_menu_for_tests))
            .add_systems(OnEnter(GameState::Playing), enter_playing)
            .add_systems(OnEnter(GameState::Paused), enter_paused)
            .add_systems(OnEnter(GameState::Map), enter_paused)
            .add_systems(OnEnter(GameState::Journal), enter_paused)
            .add_systems(OnEnter(GameState::Shop), enter_paused)
            .add_systems(OnEnter(GameState::MainMenu), enter_menu)
            .init_resource::<StartPlaying>()
            .init_resource::<OriginalPause>()
            .add_systems(Update, (leave_loading, menu_buttons, pause_toggle, recapture_cursor, update_menu, test_pause));
    }
}

#[derive(Component)]
struct MenuRoot;

/// The original game's pause menu is up in place of ours (`original_ui`):
/// ours stays hidden and leaves the keyboard's Escape to it.
#[derive(Resource, Default)]
pub struct OriginalPause(pub bool);

#[derive(Component)]
struct MenuTitle;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum MenuButton {
    Continue,
    Play,
    /// The journal (skills, crafting, syringes), from the pause menu.
    Journal,
    Options,
    Controls,
    Quit,
}

#[derive(Component)]
struct PlayLabel;

const BUTTON_IDLE: Color = Color::srgba(0.1, 0.1, 0.1, 0.75);
const BUTTON_HOVER: Color = Color::srgba(0.75, 0.25, 0.12, 0.9);

fn setup_menu(mut commands: Commands) {
    let font = |size: f32| TextFont { font_size: bevy::text::FontSize::Px(size), ..default() };
    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::FlexStart,
                padding: UiRect::left(Val::Percent(8.0)),
                row_gap: Val::Px(14.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)),
            GlobalZIndex(10),
            MenuRoot,
        ))
        .id();
    commands.spawn((Text::new("FC3 BEVY"), font(64.0), TextColor(Color::srgb(0.95, 0.93, 0.88)), MenuTitle, ChildOf(root)));
    commands.spawn((
        Text::new("An engine reimplementation running your installed game data"),
        font(18.0),
        TextColor(Color::srgba(0.9, 0.9, 0.85, 0.75)),
        Node { margin: UiRect::bottom(Val::Px(28.0)), ..default() },
        ChildOf(root),
    ));
    let buttons = [
        (MenuButton::Continue, "CONTINUE"),
        (MenuButton::Play, "PLAY"),
        (MenuButton::Journal, "JOURNAL"),
        (MenuButton::Options, "OPTIONS"),
        (MenuButton::Controls, "CONTROLS"),
        (MenuButton::Quit, "QUIT"),
    ];
    for (kind, label) in buttons {
        let b = commands
            .spawn((
                Button,
                Node {
                    width: Val::Px(280.0),
                    height: Val::Px(52.0),
                    justify_content: JustifyContent::FlexStart,
                    align_items: AlignItems::Center,
                    padding: UiRect::left(Val::Px(18.0)),
                    ..default()
                },
                BackgroundColor(BUTTON_IDLE),
                kind,
                // CONTINUE only shows in the main menu when a save exists.
                if kind == MenuButton::Continue { Visibility::Hidden } else { Visibility::Inherited },
                ChildOf(root),
            ))
            .id();
        let mut text = commands.spawn((Text::new(label), font(24.0), TextColor(Color::srgb(0.95, 0.95, 0.92)), ChildOf(b)));
        if kind == MenuButton::Play {
            text.insert(PlayLabel);
        }
    }
}

/// Whether to go straight into play once the cache is ready (automated runs).
#[derive(Resource, Default)]
struct StartPlaying(bool);

/// Leaves the loading state as soon as the converted cache is available.
fn leave_loading(state: Res<State<GameState>>, info: Option<Res<crate::data::CacheInfo>>, start: Res<StartPlaying>, mut next: ResMut<NextState<GameState>>) {
    if *state.get() == GameState::Loading && info.is_some() {
        next.set(if start.0 { GameState::Playing } else { GameState::MainMenu });
    }
}

/// Automated runs (screenshots, scripted tests) start in play unless `--menu` is given.
fn skip_menu_for_tests(args: Res<Args>, mut start: ResMut<StartPlaying>) {
    // `--noclip` is interactive but also skips the menu.
    start.0 = (args.automated() && !args.show_menu) || args.noclip;
}

fn set_cursor(cursor: &mut Query<&mut CursorOptions, With<PrimaryWindow>>, captured: bool) {
    if let Ok(mut c) = cursor.single_mut() {
        c.grab_mode = if captured { CursorGrabMode::Locked } else { CursorGrabMode::None };
        c.visible = !captured;
    }
}

fn enter_playing(
    args: Res<Args>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut captured: ResMut<InputCaptured>,
    mut time: ResMut<Time<Virtual>>,
) {
    // Automated runs neither grab the cursor nor react to stray mouse input.
    let capture = !args.automated();
    set_cursor(&mut cursor, capture);
    captured.0 = capture;
    time.unpause();
}

fn enter_paused(mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>, mut captured: ResMut<InputCaptured>, mut time: ResMut<Time<Virtual>>) {
    set_cursor(&mut cursor, false);
    captured.0 = false;
    time.pause();
}

fn enter_menu(mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>, mut captured: ResMut<InputCaptured>, mut time: ResMut<Time<Virtual>>) {
    set_cursor(&mut cursor, false);
    captured.0 = false;
    time.unpause();
}

/// Esc / gamepad Menu toggle pause; B backs out; A picks the main action.
#[allow(clippy::too_many_arguments)]
fn pause_toggle(
    keys: Res<ButtonInput<KeyCode>>,
    gamepads: Query<&bevy::input::gamepad::Gamepad>,
    state: Res<State<GameState>>,
    rebinding: Res<crate::options::Rebinding>,
    mut options: ResMut<crate::options::OptionsOpen>,
    mut next: ResMut<NextState<GameState>>,
    mut journal: ResMut<crate::progression::PauseMenu>,
    original: Res<OriginalPause>,
    credits: Res<crate::credits::EndCredits>,
) {
    use bevy::input::gamepad::GamepadButton as B;
    // (Esc and Start skip the end credits instead.)
    if credits.on() && *state.get() == GameState::Playing {
        return;
    }
    let pad = |b: B| gamepads.iter().any(|g| g.just_pressed(b));
    let esc = keys.just_pressed(KeyCode::Escape) && !(original.0 && *state.get() == GameState::Paused);
    if options.0 {
        if (esc && !crate::options::binding_key(&rebinding)) || pad(B::East) || pad(B::Start) {
            options.0 = false;
        }
        return;
    }
    let s = *state.get();
    if s == GameState::Journal && (esc || pad(B::Start) || pad(B::East)) {
        if journal.can_exit() {
            journal.tutorial = None;
            next.set(GameState::Playing);
        }
        return;
    }
    if esc || pad(B::Start) {
        match s {
            GameState::Playing => next.set(GameState::Paused),
            GameState::Paused | GameState::Map | GameState::Journal | GameState::Shop => next.set(GameState::Playing),
            GameState::MainMenu | GameState::Loading => {}
        }
        return;
    }
    if pad(B::East) && matches!(s, GameState::Paused | GameState::Map | GameState::Journal | GameState::Shop) {
        next.set(GameState::Playing);
    }
    // A presses the focused button (see `ui_nav`).
}

/// Clicking back into the window while playing re-locks the cursor (e.g. after alt-tab).
/// --pause (tests): pause once, three seconds into play.
fn test_pause(
    args: Res<crate::Args>,
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    time: Res<Time<Real>>,
    mut since: Local<Option<f32>>,
    mut done: Local<bool>,
) {
    if !args.open_pause || *done || *state.get() != GameState::Playing {
        return;
    }
    let now = time.elapsed_secs();
    if now - *since.get_or_insert(now) > 3.0 {
        *done = true;
        next.set(GameState::Paused);
    }
}

fn recapture_cursor(
    args: Res<Args>,
    mouse: Res<ButtonInput<MouseButton>>,
    state: Res<State<GameState>>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
    mut captured: ResMut<InputCaptured>,
) {
    if *state.get() == GameState::Playing && mouse.just_pressed(MouseButton::Left) && !captured.0 && !args.automated() {
        set_cursor(&mut cursor, true);
        captured.0 = true;
    }
}

#[allow(clippy::type_complexity)]
fn menu_buttons(
    mut buttons: Query<(&Interaction, &MenuButton, &mut BackgroundColor), Changed<Interaction>>,
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut hud: ResMut<HudState>,
    mut options: ResMut<crate::options::OptionsOpen>,
    mut exit: MessageWriter<AppExit>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut continue_game: MessageWriter<crate::save::ContinueGame>,
    journal: Res<crate::progression::PauseMenu>,
) {
    for (interaction, kind, mut bg) in &mut buttons {
        if *interaction == Interaction::Pressed {
            sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.7 });
        }
        match interaction {
            Interaction::Pressed => match kind {
                MenuButton::Continue => {
                    if *state.get() == GameState::MainMenu {
                        continue_game.write(crate::save::ContinueGame);
                        next.set(GameState::Playing);
                    }
                }
                MenuButton::Play => {
                    if *state.get() != GameState::Playing && journal.can_exit() {
                        next.set(GameState::Playing);
                    }
                }
                MenuButton::Journal => {
                    if *state.get() == GameState::Paused && journal.allows_page(journal.page) {
                        next.set(GameState::Journal);
                    }
                }
                MenuButton::Options => options.0 = true,
                MenuButton::Controls => hud.show_help = !hud.show_help,
                MenuButton::Quit => {
                    exit.write(AppExit::Success);
                }
            },
            Interaction::Hovered => bg.0 = BUTTON_HOVER,
            Interaction::None => bg.0 = BUTTON_IDLE,
        }
    }
}

fn update_menu(
    state: Res<State<GameState>>,
    slot: Res<crate::save::SaveSlot>,
    options: Res<crate::options::OptionsOpen>,
    original: Res<OriginalPause>,
    mut root: Query<&mut Visibility, (With<MenuRoot>, Without<MenuButton>)>,
    mut buttons: Query<(&MenuButton, &mut Visibility, &mut Node), Without<MenuRoot>>,
    mut play: Query<&mut Text, (With<PlayLabel>, Without<MenuTitle>)>,
    mut title: Query<&mut Text, (With<MenuTitle>, Without<PlayLabel>)>,
) {
    if !state.is_changed() && !slot.is_changed() && !options.is_changed() && !original.is_changed() {
        return;
    }
    let s = *state.get();
    let can_continue = s == GameState::MainMenu && slot.existing.is_some();
    if let Ok(mut v) = root.single_mut() {
        // Hidden in play and under the options screen (so it can't take clicks).
        *v = if matches!(s, GameState::Playing | GameState::Map | GameState::Journal | GameState::Shop) || options.0 || (original.0 && s == GameState::Paused) {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
    }
    for (kind, mut v, mut node) in &mut buttons {
        let shown = match kind {
            MenuButton::Continue => can_continue,
            MenuButton::Journal => s == GameState::Paused,
            _ => continue,
        };
        *v = if shown { Visibility::Inherited } else { Visibility::Hidden };
        node.display = if shown { Display::Flex } else { Display::None };
    }
    if let Ok(mut t) = play.single_mut() {
        t.0 = match s {
            GameState::Paused => "RESUME".into(),
            _ if can_continue => "NEW GAME".into(),
            _ => "PLAY".into(),
        };
    }
    if let Ok(mut t) = title.single_mut() {
        t.0 = if s == GameState::Paused { "PAUSED".into() } else { "FC3 BEVY".into() };
    }
}
