//! The options screen (from the main and pause menus): look sensitivity, FOV,
//! volume, view distance, shadows, window mode, original shaders, and key
//! bindings. Changes apply immediately and are saved (see `settings`).

use bevy::prelude::*;

use crate::controls::{default_bindings, Action, Binding};
use crate::settings::{Quality, Settings};

pub struct OptionsPlugin;

impl Plugin for OptionsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OptionsOpen>()
            .init_resource::<Rebinding>()
            .add_systems(Startup, setup_options)
            .add_systems(Update, (option_buttons, capture_binding, refresh_options, show_options).chain());
    }
}

/// Whether the options screen is showing.
#[derive(Resource, Default)]
pub struct OptionsOpen(pub bool);

/// The action waiting for a new key, and the frame it started (so the click
/// that started it isn't taken as the binding).
#[derive(Resource, Default)]
pub struct Rebinding(Option<(Action, u32)>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Opt {
    Sensitivity,
    GamepadSensitivity,
    InvertY,
    Vibration,
    AimAssist,
    Fov,
    Volume,
    MusicVolume,
    ViewDistance,
    Shadows,
    Effects,
    VSync,
    Fullscreen,
    OriginalShaders,
    ShowFps,
    Subtitles,
    Story,
    Difficulty,
}

impl Opt {
    const ALL: [Opt; 18] = [
        Opt::Difficulty,
        Opt::Story,
        Opt::Sensitivity,
        Opt::GamepadSensitivity,
        Opt::InvertY,
        Opt::Vibration,
        Opt::AimAssist,
        Opt::Fov,
        Opt::Volume,
        Opt::MusicVolume,
        Opt::ViewDistance,
        Opt::Shadows,
        Opt::Effects,
        Opt::VSync,
        Opt::Fullscreen,
        Opt::OriginalShaders,
        Opt::ShowFps,
        Opt::Subtitles,
    ];

    fn label(self) -> &'static str {
        match self {
            Opt::Sensitivity => "Mouse sensitivity",
            Opt::GamepadSensitivity => "Gamepad sensitivity",
            Opt::InvertY => "Invert look",
            Opt::Vibration => "Vibration",
            Opt::AimAssist => "Aim assist (gamepad)",
            Opt::Fov => "Field of view",
            Opt::Volume => "Volume",
            Opt::MusicVolume => "Music volume",
            Opt::ViewDistance => "View distance",
            Opt::Shadows => "Shadows",
            Opt::Effects => "Effects (bloom, shafts)",
            Opt::VSync => "V-sync",
            Opt::Fullscreen => "Window mode",
            Opt::OriginalShaders => "Original shaders",
            Opt::ShowFps => "Show FPS",
            Opt::Subtitles => "Subtitles",
            Opt::Story => "New game: story",
            Opt::Difficulty => "Difficulty",
        }
    }

    fn value(self, s: &Settings) -> String {
        let on = |b: bool| if b { "On" } else { "Off" }.to_string();
        match self {
            Opt::Sensitivity => format!("{:.1}", s.sensitivity),
            Opt::GamepadSensitivity => format!("{:.1}", s.gamepad_sensitivity),
            Opt::InvertY => on(s.invert_y),
            Opt::Vibration => on(s.vibration),
            Opt::AimAssist => on(s.aim_assist),
            Opt::Fov => format!("{:.0}", s.fov),
            Opt::Volume => format!("{:.0}%", s.master_volume * 100.0),
            Opt::MusicVolume => format!("{:.0}%", s.music_volume * 100.0),
            Opt::ViewDistance => s.view_distance.name().into(),
            Opt::Shadows => s.shadows.name().into(),
            Opt::Effects => s.effects.name().into(),
            Opt::VSync => on(s.vsync),
            Opt::Fullscreen => s.window.name().into(),
            Opt::OriginalShaders => on(s.original_shaders),
            Opt::ShowFps => on(s.show_fps),
            Opt::Subtitles => on(s.subtitles),
            Opt::Story => if s.story { "Story" } else { "Free roam" }.to_string(),
            Opt::Difficulty => s.difficulty.name().into(),
        }
    }

    fn step(self, s: &mut Settings, dir: i32) {
        let d = dir as f32;
        let cycle = |q: Quality| {
            let i = Quality::ALL.iter().position(|x| *x == q).unwrap_or(0) as i32;
            Quality::ALL[(i + dir).clamp(0, Quality::ALL.len() as i32 - 1) as usize]
        };
        match self {
            Opt::Sensitivity => s.sensitivity = (s.sensitivity + d * 0.1).clamp(0.1, 5.0),
            Opt::GamepadSensitivity => s.gamepad_sensitivity = (s.gamepad_sensitivity + d * 0.1).clamp(0.1, 5.0),
            Opt::InvertY => s.invert_y = !s.invert_y,
            Opt::Vibration => s.vibration = !s.vibration,
            Opt::AimAssist => s.aim_assist = !s.aim_assist,
            Opt::Fov => s.fov = (s.fov + d * 5.0).clamp(55.0, 110.0),
            Opt::Volume => s.master_volume = (s.master_volume + d * 0.1).clamp(0.0, 1.0),
            Opt::MusicVolume => s.music_volume = (s.music_volume + d * 0.1).clamp(0.0, 1.0),
            Opt::ViewDistance => s.view_distance = cycle(s.view_distance),
            Opt::Shadows => s.shadows = cycle(s.shadows),
            Opt::Effects => s.effects = cycle(s.effects),
            Opt::VSync => s.vsync = !s.vsync,
            Opt::Fullscreen => {
                use crate::settings::WindowKind;
                let i = WindowKind::ALL.iter().position(|k| *k == s.window).unwrap_or(1) as i32;
                s.window = WindowKind::ALL[(i + dir).rem_euclid(WindowKind::ALL.len() as i32) as usize];
            }
            Opt::OriginalShaders => s.original_shaders = !s.original_shaders,
            Opt::ShowFps => s.show_fps = !s.show_fps,
            Opt::Subtitles => s.subtitles = !s.subtitles,
            Opt::Story => s.story = !s.story,
            Opt::Difficulty => {
                use crate::settings::Difficulty;
                let i = Difficulty::ALL.iter().position(|x| *x == s.difficulty).unwrap_or(1) as i32;
                s.difficulty = Difficulty::ALL[(i + dir).clamp(0, Difficulty::ALL.len() as i32 - 1) as usize];
            }
        }
    }
}

#[derive(Component)]
struct OptionsRoot;

#[derive(Component)]
struct OptValue(Opt);

#[derive(Component, Clone, Copy)]
enum OptButton {
    Step(Opt, i32),
    Bind(Action),
    Reset,
    Back,
}

#[derive(Component)]
struct BindText(Action);

const IDLE: Color = Color::srgba(0.1, 0.1, 0.1, 0.8);
const HOVER: Color = Color::srgba(0.75, 0.25, 0.12, 0.9);
const TEXT: Color = Color::srgb(0.95, 0.95, 0.92);

fn font(size: f32) -> TextFont {
    TextFont { font_size: bevy::text::FontSize::Px(size), ..default() }
}

fn button(commands: &mut Commands, parent: Entity, kind: OptButton, label: &str, width: f32) -> Entity {
    let b = commands
        .spawn((
            Button,
            Node { width: Val::Px(width), height: Val::Px(26.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() },
            BackgroundColor(IDLE),
            kind,
            ChildOf(parent),
        ))
        .id();
    commands.spawn((Text::new(label), font(16.0), TextColor(TEXT), ChildOf(b))).id()
}

fn setup_options(mut commands: Commands, args: Res<crate::Args>, mut open: ResMut<OptionsOpen>) {
    open.0 = args.open_options;
    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(16.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.82)),
            Visibility::Hidden,
            GlobalZIndex(12),
            OptionsRoot,
        ))
        .id();
    commands.spawn((Text::new("OPTIONS"), font(42.0), TextColor(TEXT), ChildOf(root)));
    let columns = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(60.0), ..default() }, ChildOf(root))).id();
    let left = commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(6.0), ..default() }, ChildOf(columns))).id();
    commands.spawn((Text::new("GENERAL"), font(20.0), TextColor(Color::srgb(0.95, 0.6, 0.3)), ChildOf(left)));
    for opt in Opt::ALL {
        let row = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(8.0),
                    height: Val::Px(30.0),
                    flex_shrink: 0.0,
                    ..default()
                },
                ChildOf(left),
            ))
            .id();
        commands.spawn((Text::new(opt.label()), font(17.0), TextColor(TEXT), Node { width: Val::Px(210.0), ..default() }, ChildOf(row)));
        button(&mut commands, row, OptButton::Step(opt, -1), "<", 30.0);
        commands.spawn((
            Text::new(""),
            font(17.0),
            TextColor(Color::srgb(1.0, 0.85, 0.5)),
            TextLayout { justify: Justify::Center, ..default() },
            Node { width: Val::Px(90.0), ..default() },
            OptValue(opt),
            ChildOf(row),
        ));
        button(&mut commands, row, OptButton::Step(opt, 1), ">", 30.0);
    }
    let right = commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(6.0), ..default() }, ChildOf(columns))).id();
    commands.spawn((Text::new("CONTROLS  (click, then press a key or mouse button)"), font(20.0), TextColor(Color::srgb(0.95, 0.6, 0.3)), ChildOf(right)));
    // Bindings in two columns so the screen fits at 720p.
    let grid = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(28.0), ..default() }, ChildOf(right))).id();
    let half = Action::ALL.len().div_ceil(2);
    for chunk in Action::ALL.chunks(half) {
        let col = commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), ..default() }, ChildOf(grid))).id();
        for &a in chunk {
            let row = commands
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(8.0),
                        height: Val::Px(26.0),
                        flex_shrink: 0.0,
                        ..default()
                    },
                    ChildOf(col),
                ))
                .id();
            commands.spawn((Text::new(a.name()), font(15.0), TextColor(TEXT), Node { width: Val::Px(150.0), ..default() }, ChildOf(row)));
            let label = button(&mut commands, row, OptButton::Bind(a), "", 110.0);
            commands.entity(label).insert(BindText(a));
        }
    }
    commands.spawn((
        Text::new("Gamepad: sticks move and look, RT fire, LT aim, A jump, B crouch, X reload / interact, Y next weapon, LB grenade,\nRB tag, L3 sprint, R3 melee, D-pad up medkit / down molotov / left rock (hold: mine) / right C4 (hold: detonate),\nView map, Menu pause.\nIn menus the D-pad or left stick moves between buttons, A selects and B goes back."),
        font(14.0),
        TextColor(Color::srgba(0.9, 0.9, 0.85, 0.7)),
        TextLayout { justify: Justify::Center, ..default() },
        ChildOf(root),
    ));
    let bar = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(20.0), ..default() }, ChildOf(root))).id();
    button(&mut commands, bar, OptButton::Reset, "RESET CONTROLS", 200.0);
    button(&mut commands, bar, OptButton::Back, "BACK", 200.0);
}

#[allow(clippy::type_complexity)]
fn option_buttons(
    mut open: ResMut<OptionsOpen>,
    mut buttons: Query<(&Interaction, &OptButton, &mut BackgroundColor), Changed<Interaction>>,
    mut settings: ResMut<Settings>,
    mut rebinding: ResMut<Rebinding>,
    frames: Res<bevy::diagnostic::FrameCount>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    if !open.0 {
        return;
    }
    for (interaction, kind, mut bg) in &mut buttons {
        match interaction {
            Interaction::Pressed => {
                sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.7 });
                match *kind {
                    OptButton::Step(opt, dir) => opt.step(&mut settings, dir),
                    OptButton::Bind(a) => rebinding.0 = Some((a, frames.0)),
                    OptButton::Reset => settings.bindings = default_bindings(),
                    OptButton::Back => open.0 = false,
                }
            }
            Interaction::Hovered => bg.0 = HOVER,
            Interaction::None => bg.0 = IDLE,
        }
    }
}

/// Takes the next key or mouse button as the new binding (Esc cancels).
fn capture_binding(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    frames: Res<bevy::diagnostic::FrameCount>,
    mut rebinding: ResMut<Rebinding>,
    mut settings: ResMut<Settings>,
) {
    let Some((action, started)) = rebinding.0 else { return };
    if frames.0 <= started + 1 {
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        rebinding.0 = None;
        return;
    }
    let key = keys.get_just_pressed().find(|k| crate::controls::KEYS.contains(k)).map(|k| Binding::Key(*k));
    let button = mouse.get_just_pressed().next().map(|m| Binding::Mouse(*m));
    if let Some(b) = key.or(button) {
        settings.rebind(action, b);
        rebinding.0 = None;
    }
}

fn refresh_options(
    settings: Res<Settings>,
    rebinding: Res<Rebinding>,
    mut values: Query<(&OptValue, &mut Text), Without<BindText>>,
    mut binds: Query<(&BindText, &mut Text), Without<OptValue>>,
) {
    if !settings.is_changed() && !rebinding.is_changed() {
        return;
    }
    for (v, mut t) in &mut values {
        t.0 = v.0.value(&settings);
    }
    for (b, mut t) in &mut binds {
        t.0 = if rebinding.0.is_some_and(|(a, _)| a == b.0) {
            "press a key...".into()
        } else {
            settings.binding(b.0).map(|x| x.label()).unwrap_or_else(|| "-".into())
        };
    }
}

/// Whether a key is being bound (Esc then cancels the binding, not the screen).
pub fn binding_key(r: &Rebinding) -> bool {
    r.0.is_some()
}

fn show_options(open: Res<OptionsOpen>, mut root: Query<&mut Visibility, With<OptionsRoot>>) {
    if let Ok(mut v) = root.single_mut() {
        *v = if open.0 { Visibility::Inherited } else { Visibility::Hidden };
    }
}
