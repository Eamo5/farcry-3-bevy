//! Player input as actions: rebindable keyboard and mouse bindings plus a fixed
//! gamepad layout, merged once per frame into [`ActionState`], which gameplay
//! systems read instead of raw devices.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use bevy::input::gamepad::{Gamepad, GamepadAxis, GamepadButton};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::input::InputSystems;
use bevy::prelude::*;

use crate::player::InputCaptured;
use crate::settings::Settings;

pub struct ControlsPlugin;

/// The throwable in hand for the HUD (`ThrowableIcon`): the one last thrown
/// or laid, or the one a script chose (`CAPI_Player.SelectThrowGadget`: a
/// trial's molotovs); a grenade until then.
#[derive(Resource, Default)]
pub struct ThrowChoice(pub Option<Action>);

impl Plugin for ControlsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ActionState>()
            .init_resource::<ThrowChoice>()
            .init_resource::<ScriptActionMaps>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_action_maps)
            .add_systems(PreUpdate, (update_actions.after(InputSystems), apply_action_maps.after(update_actions)));
    }
}

/// The engine's input action maps (each map's signals, from the cache), the
/// maps the scripts have pushed on the player, and the one they've locked
/// him to: the map in force lets through only its signals (a narrative
/// scene takes away moving and shooting, and leaves looking and using).
#[derive(Resource, Default)]
pub struct ScriptActionMaps {
    table: BTreeMap<String, BTreeSet<String>>,
    stack: Vec<String>,
    locked: Option<String>,
}

impl ScriptActionMaps {
    pub fn push(&mut self, map: &str) {
        self.stack.push(map.to_string());
    }

    /// The map taken off (its last push).
    pub fn remove(&mut self, map: &str) {
        if let Some(i) = self.stack.iter().rposition(|m| m.eq_ignore_ascii_case(map)) {
            self.stack.remove(i);
        }
    }

    /// The map alone in force.
    pub fn set(&mut self, map: &str) {
        self.stack = vec![map.to_string()];
    }

    /// The map in force whatever is pushed, until unlocked.
    pub fn lock(&mut self, map: &str) {
        self.locked = Some(map.to_string());
    }

    pub fn unlock(&mut self) {
        self.locked = None;
    }

    pub fn reset(&mut self) {
        self.stack.clear();
        self.locked = None;
    }

    /// The map in force, if the scripts have set one.
    pub(crate) fn in_force(&self) -> Option<&String> {
        self.locked.as_ref().or(self.stack.last())
    }

    /// The signals the map in force lets through (none set, or one the
    /// table lacks: everything).
    fn allowed(&self) -> Option<&BTreeSet<String>> {
        let name = self.in_force()?;
        self.table.get(name).or_else(|| self.table.iter().find(|(m, _)| m.eq_ignore_ascii_case(name)).map(|(_, s)| s))
    }
}

fn load_action_maps(info: Res<crate::data::CacheInfo>, mut maps: ResMut<ScriptActionMaps>, mut actions: ResMut<ActionState>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::actionmaps::ACTION_MAPS_REL)).unwrap_or_default();
    maps.table = fc3_formats::actionmaps::from_text(&text);
    actions.use_hold = fc3_formats::actionmaps::hold_from_text(&text);
}

/// The engine's signals an action sends (any of them let through lets it
/// through; using also goes by the narrative scenes' interact signals).
fn signals(a: Action) -> &'static [&'static str] {
    match a {
        Action::Forward | Action::Back | Action::Left | Action::Right => &["move"],
        Action::Jump => &["jump", "jump_use"],
        Action::Crouch | Action::SwimDown => &["crouch"],
        Action::Sprint => &["sprint"],
        Action::Fire => &["startshooting"],
        Action::Aim => &["startironsight"],
        Action::Reload => &["reload", "quickReload"],
        Action::Interact => &["use", "jump_use"],
        Action::Grenade => &["throw_grenade", "select_grenade"],
        Action::Molotov => &["select_molotov"],
        Action::Medkit => &["heal"],
        Action::Melee => &["short_range_melee_start"],
        Action::NextWeapon => &["select_next_weapon"],
        Action::PrevWeapon => &["select_previous_weapon"],
        Action::Map => &["show_map"],
        Action::Tag => &["toggle_monocular"],
        Action::ThrowRock => &["throw_rock"],
        Action::C4 => &["select_IED_explosive"],
        Action::Mine => &["select_proximity_mine"],
        Action::SyringeA => &["use_recipe_a"],
        Action::SyringeB => &["use_recipe_b"],
    }
}

/// Takes away what the map in force doesn't let through.
pub fn apply_action_maps(maps: Res<ScriptActionMaps>, mut actions: ResMut<ActionState>, mut last: Local<Option<String>>) {
    if maps.is_changed() && maps.in_force() != last.as_ref() {
        *last = maps.in_force().cloned();
        info!("input action map: {:?} (pushed {:?}, locked {:?})", *last, maps.stack, maps.locked);
    }
    let Some(allowed) = maps.allowed() else { return };
    let has = |s: &str| allowed.contains(s);
    if !has("move") {
        actions.movement = Vec2::ZERO;
    }
    if !has("look") {
        actions.look = Vec2::ZERO;
    }
    if !has("select_next_weapon") && !has("select_previous_weapon") {
        actions.cycle = 0;
    }
    let interact = allowed.iter().any(|s| s.starts_with("interact_"));
    for a in Action::ALL {
        if !(signals(a).iter().any(|s| has(s)) || (a == Action::Interact && interact)) {
            actions.suppress(a);
        }
    }
}

#[cfg(test)]
mod action_map_tests {
    use super::*;

    #[test]
    fn using_takes_the_games_hold_once_a_hold() {
        // Held at 60 frames a second: it uses once, 0.37 s in, then not
        // again while held; a tap doesn't use.
        let (mut held, mut was, mut uses) = (0.0, false, Vec::new());
        for frame in 0..60 {
            let (used, h) = use_step(Some(0.37), true, was, held, 1.0 / 60.0);
            (held, was) = (h, true);
            if used {
                uses.push(frame);
            }
        }
        assert_eq!(uses, vec![22]);
        let tap: Vec<bool> = [true, true, false]
            .iter()
            .scan((0.0, false), |(h, w), &down| {
                let (used, now) = use_step(Some(0.37), down, *w, *h, 1.0 / 60.0);
                (*h, *w) = (now, down);
                Some(used)
            })
            .collect();
        assert!(tap.iter().all(|u| !u));
        // Without the game's hold time, a press uses.
        assert_eq!(use_step(None, true, false, 0.0, 0.016).0, true);
        assert_eq!(use_step(None, true, true, 0.1, 0.016).0, false);
    }

    #[test]
    fn a_pushed_map_lets_through_only_its_signals() {
        let mut maps = ScriptActionMaps::default();
        let set = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        maps.table.insert("scene".into(), set(&["look", "interact_press_a"]));
        maps.table.insert("walk_only".into(), set(&["move", "look"]));
        let mut actions = ActionState::default();
        let frame = |maps: &ScriptActionMaps, actions: &mut ActionState| {
            actions.movement = Vec2::Y;
            actions.look = Vec2::X;
            actions.press(Action::Fire);
            actions.press(Action::Interact);
            let mut app = App::new();
            app.insert_resource(ScriptActionMaps { table: maps.table.clone(), stack: maps.stack.clone(), locked: maps.locked.clone() })
                .insert_resource(std::mem::take(actions))
                .add_systems(Update, apply_action_maps);
            app.update();
            *actions = app.world_mut().remove_resource::<ActionState>().unwrap();
        };
        // Nothing pushed: everything.
        frame(&maps, &mut actions);
        assert_eq!(actions.movement, Vec2::Y);
        assert!(actions.pressed(Action::Fire));
        // A scene's map: looking and using, not moving or firing.
        maps.push("scene");
        frame(&maps, &mut actions);
        assert_eq!((actions.movement, actions.look), (Vec2::ZERO, Vec2::X));
        assert!(!actions.pressed(Action::Fire) && actions.pressed(Action::Interact));
        // Locked to another until unlocked; popped back to everything.
        maps.lock("walk_only");
        frame(&maps, &mut actions);
        assert_eq!(actions.movement, Vec2::Y);
        assert!(!actions.pressed(Action::Interact));
        maps.unlock();
        maps.remove("scene");
        frame(&maps, &mut actions);
        assert!(actions.pressed(Action::Fire) && actions.pressed(Action::Interact));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Crouch,
    Sprint,
    SwimDown,
    Fire,
    Aim,
    Reload,
    Interact,
    Grenade,
    Molotov,
    Medkit,
    Melee,
    NextWeapon,
    PrevWeapon,
    Map,
    Tag,
    ThrowRock,
    C4,
    Mine,
    /// The syringes in the two quick slots (`use_recipe_a`, `_b`).
    SyringeA,
    SyringeB,
}

impl Action {
    /// Rebindable actions, in the order the options screen lists them.
    pub const ALL: [Action; 25] = [
        Action::Forward,
        Action::Back,
        Action::Left,
        Action::Right,
        Action::Jump,
        Action::Crouch,
        Action::Sprint,
        Action::SwimDown,
        Action::Fire,
        Action::Aim,
        Action::Reload,
        Action::Interact,
        Action::Grenade,
        Action::Molotov,
        Action::Medkit,
        Action::Melee,
        Action::NextWeapon,
        Action::PrevWeapon,
        Action::Map,
        Action::Tag,
        Action::ThrowRock,
        Action::C4,
        Action::Mine,
        Action::SyringeA,
        Action::SyringeB,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Action::Forward => "Move forward",
            Action::Back => "Move back",
            Action::Left => "Move left",
            Action::Right => "Move right",
            Action::Jump => "Jump / swim up",
            Action::Crouch => "Crouch",
            Action::Sprint => "Sprint",
            Action::SwimDown => "Swim down",
            Action::Fire => "Fire",
            Action::Aim => "Aim",
            Action::Reload => "Reload",
            Action::Interact => "Interact",
            Action::Grenade => "Throw grenade",
            Action::Molotov => "Throw molotov",
            Action::Medkit => "Use medkit",
            Action::Melee => "Melee / takedown",
            Action::NextWeapon => "Next weapon",
            Action::PrevWeapon => "Previous weapon",
            Action::Map => "Map",
            Action::Tag => "Tag enemies",
            Action::ThrowRock => "Throw rock",
            Action::C4 => "C4 (hold: detonate)",
            Action::Mine => "Lay a mine",
            Action::SyringeA => "Syringe quick slot A",
            Action::SyringeB => "Syringe quick slot B",
        }
    }

    /// Stable key for the settings file.
    pub fn id(self) -> &'static str {
        match self {
            Action::Forward => "forward",
            Action::Back => "back",
            Action::Left => "left",
            Action::Right => "right",
            Action::Jump => "jump",
            Action::Crouch => "crouch",
            Action::Sprint => "sprint",
            Action::SwimDown => "swim_down",
            Action::Fire => "fire",
            Action::Aim => "aim",
            Action::Reload => "reload",
            Action::Interact => "interact",
            Action::Grenade => "grenade",
            Action::Molotov => "molotov",
            Action::Medkit => "medkit",
            Action::Melee => "melee",
            Action::NextWeapon => "next_weapon",
            Action::PrevWeapon => "prev_weapon",
            Action::Map => "map",
            Action::Tag => "tag",
            Action::ThrowRock => "rock",
            Action::C4 => "c4",
            Action::Mine => "mine",
            Action::SyringeA => "syringe_a",
            Action::SyringeB => "syringe_b",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }

    /// Action names embedded in the game's localized tutorial text.
    pub fn from_prompt(name: &str) -> Option<Action> {
        match name.to_ascii_lowercase().as_str() {
            "moveforward" => Some(Action::Forward),
            "movebackward" => Some(Action::Back),
            "moveleft" => Some(Action::Left),
            "moveright" => Some(Action::Right),
            "throwrock" => Some(Action::ThrowRock),
            "throwgrenade" => Some(Action::Grenade),
            "throwmolotov" => Some(Action::Molotov),
            "heal" => Some(Action::Medkit),
            // (A zip line: on with the interact key, off with a jump.)
            "zipattach" => Some(Action::Interact),
            "zipdetach" => Some(Action::Jump),
            // (Looting is using.)
            "loot" => Some(Action::Interact),
            // (The wingsuit and the parachute open with a jump.)
            "parachute" | "wingsuit" => Some(Action::Jump),
            "takedown" => Some(Action::Melee),
            "nextweapon" | "gadgetwheel" | "switchweapon" => Some(Action::NextWeapon),
            "previousweapon" => Some(Action::PrevWeapon),
            "showmap" | "worldmap" => Some(Action::Map),
            "binoculars" | "camera" => Some(Action::Tag),
            other => Action::from_id(other),
        }
    }

    fn gamepad(self) -> &'static [GamepadButton] {
        use GamepadButton as B;
        match self {
            Action::Jump => &[B::South],
            Action::Crouch => &[B::East],
            Action::Sprint => &[B::LeftThumb],
            Action::SwimDown => &[B::DPadDown],
            Action::Fire => &[B::RightTrigger2],
            Action::Aim => &[B::LeftTrigger2],
            Action::Reload | Action::Interact => &[B::West],
            Action::Grenade => &[B::LeftTrigger],
            Action::Molotov => &[B::DPadDown],
            Action::Medkit => &[B::DPadUp],
            Action::Melee => &[B::RightThumb],
            Action::NextWeapon => &[B::North],
            Action::PrevWeapon => &[],
            // D-pad left: a tap throws a rock, a hold lays a mine (see `update_actions`).
            Action::ThrowRock | Action::Mine => &[B::DPadLeft],
            Action::C4 => &[B::DPadRight],
            Action::Map => &[B::Select],
            Action::Tag => &[B::RightTrigger],
            // (The game's quick slots are the D-pad's down and left, which
            // throw and lay here.)
            Action::Forward | Action::Back | Action::Left | Action::Right | Action::SyringeA | Action::SyringeB => &[],
        }
    }
}

/// A keyboard key or mouse button.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Binding {
    Key(KeyCode),
    Mouse(MouseButton),
}

impl Binding {
    pub fn label(self) -> String {
        match self {
            Binding::Mouse(MouseButton::Left) => "LMB".into(),
            Binding::Mouse(MouseButton::Right) => "RMB".into(),
            Binding::Mouse(MouseButton::Middle) => "MMB".into(),
            Binding::Mouse(MouseButton::Back) => "Mouse 4".into(),
            Binding::Mouse(MouseButton::Forward) => "Mouse 5".into(),
            Binding::Mouse(m) => format!("{m:?}"),
            Binding::Key(k) => {
                let s = format!("{k:?}");
                let s = s.strip_prefix("Key").or_else(|| s.strip_prefix("Digit")).unwrap_or(&s).to_string();
                match s.as_str() {
                    "ShiftLeft" => "Shift".into(),
                    "ControlLeft" => "Ctrl".into(),
                    "AltLeft" => "Alt".into(),
                    _ => s,
                }
            }
        }
    }

    /// Settings-file form (`key:KeyW`, `mouse:Left`).
    pub fn id(self) -> String {
        match self {
            Binding::Key(k) => format!("key:{k:?}"),
            Binding::Mouse(m) => format!("mouse:{m:?}"),
        }
    }

    pub fn from_id(s: &str) -> Option<Binding> {
        if let Some(m) = s.strip_prefix("mouse:") {
            return Some(Binding::Mouse(match m {
                "Left" => MouseButton::Left,
                "Right" => MouseButton::Right,
                "Middle" => MouseButton::Middle,
                "Back" => MouseButton::Back,
                "Forward" => MouseButton::Forward,
                _ => return None,
            }));
        }
        let k = s.strip_prefix("key:")?;
        KEYS.iter().find(|key| format!("{key:?}") == k).map(|key| Binding::Key(*key))
    }
}

/// Keys that can be bound (and parsed back from the settings file).
pub const KEYS: &[KeyCode] = &{
    use KeyCode as K;
    [
        K::KeyA,
        K::KeyB,
        K::KeyC,
        K::KeyD,
        K::KeyE,
        K::KeyF,
        K::KeyG,
        K::KeyH,
        K::KeyI,
        K::KeyJ,
        K::KeyK,
        K::KeyL,
        K::KeyM,
        K::KeyN,
        K::KeyO,
        K::KeyP,
        K::KeyQ,
        K::KeyR,
        K::KeyS,
        K::KeyT,
        K::KeyU,
        K::KeyV,
        K::KeyW,
        K::KeyX,
        K::KeyY,
        K::KeyZ,
        K::Digit0,
        K::Digit9,
        K::Space,
        K::ShiftLeft,
        K::ShiftRight,
        K::ControlLeft,
        K::ControlRight,
        K::AltLeft,
        K::Tab,
        K::CapsLock,
        K::Backquote,
        K::Enter,
        K::Backspace,
        K::ArrowUp,
        K::ArrowDown,
        K::ArrowLeft,
        K::ArrowRight,
        K::Insert,
        K::Delete,
        K::Home,
        K::End,
        K::PageUp,
        K::PageDown,
        K::Numpad0,
        K::Numpad1,
        K::Numpad2,
        K::Numpad3,
        K::Numpad4,
        K::Numpad5,
        K::Numpad6,
        K::Numpad7,
        K::Numpad8,
        K::Numpad9,
        K::F5,
        K::F6,
        K::F7,
        K::F8,
        K::F9,
        K::Comma,
        K::Period,
        K::Slash,
        K::Semicolon,
        K::Quote,
        K::BracketLeft,
        K::BracketRight,
        K::Backslash,
        K::Minus,
        K::Equal,
    ]
};

/// Default keyboard and mouse bindings.
pub fn default_bindings() -> HashMap<Action, Binding> {
    use Binding::{Key, Mouse};
    use KeyCode as K;
    HashMap::from([
        (Action::Forward, Key(K::KeyW)),
        (Action::Back, Key(K::KeyS)),
        (Action::Left, Key(K::KeyA)),
        (Action::Right, Key(K::KeyD)),
        (Action::Jump, Key(K::Space)),
        (Action::Crouch, Key(K::ControlLeft)),
        (Action::Sprint, Key(K::ShiftLeft)),
        (Action::SwimDown, Key(K::KeyC)),
        (Action::Fire, Mouse(MouseButton::Left)),
        (Action::Aim, Mouse(MouseButton::Right)),
        (Action::Reload, Key(K::KeyR)),
        (Action::Interact, Key(K::KeyE)),
        (Action::Grenade, Key(K::KeyG)),
        (Action::Molotov, Key(K::KeyQ)),
        (Action::Medkit, Key(K::KeyH)),
        (Action::Melee, Key(K::KeyF)),
        (Action::NextWeapon, Key(K::KeyX)),
        (Action::PrevWeapon, Key(K::KeyZ)),
        (Action::Map, Key(K::KeyM)),
        (Action::Tag, Key(K::KeyB)),
        (Action::ThrowRock, Mouse(MouseButton::Middle)),
        (Action::C4, Key(K::KeyJ)),
        (Action::Mine, Key(K::KeyK)),
        // (The game's: `use_recipe_a` on 7, `_b` on 8.)
        (Action::SyringeA, Key(K::Digit7)),
        (Action::SyringeB, Key(K::Digit8)),
    ])
}

/// This frame's player input.
#[derive(Resource, Default)]
pub struct ActionState {
    pressed: HashSet<Action>,
    just_pressed: HashSet<Action>,
    /// Movement: x right, y forward, length up to 1 (analogue on a gamepad).
    pub movement: Vec2,
    /// View turn this frame in radians: x yaw right, y pitch up.
    pub look: Vec2,
    /// Weapon cycling from the mouse wheel (-1, 0, 1).
    pub cycle: i32,
    /// Whether the gamepad was the last device used (for prompts).
    pub gamepad_active: bool,
    sprint_latch: bool,
    crouch_latch: bool,
    /// How long the gamepad's D-pad left has been held.
    pad_hold: f32,
    /// The game's hold to use on foot (s; its `ClickGenerator` `HoldTime`,
    /// from the cache): none keeps using a press. How long the interact key
    /// has been down, and whether it used this frame.
    pub use_hold: Option<f32>,
    use_held: f32,
    used: bool,
}

/// Holding D-pad left this long lays a mine instead of throwing a rock.
const PAD_HOLD: f32 = 0.35;

impl ActionState {
    pub fn pressed(&self, a: Action) -> bool {
        self.pressed.contains(&a)
    }

    pub fn just_pressed(&self, a: Action) -> bool {
        self.just_pressed.contains(&a)
    }

    /// The player used what's at hand on foot: as the game has it, the
    /// interact key held a moment (its gameplay map binds `use` on a hold),
    /// once a hold; a press without the cache's hold time.
    pub fn used(&self) -> bool {
        self.used
    }

    /// Presses an action this frame (scripted tests; the story's autopilot
    /// uses with a press).
    pub fn press(&mut self, a: Action) {
        self.pressed.insert(a);
        self.just_pressed.insert(a);
        if a == Action::Interact {
            self.used = true;
        }
    }

    /// Holds an action down this frame without a fresh press (scripted tests).
    pub fn hold(&mut self, a: Action) {
        self.pressed.insert(a);
    }

    /// Marks a press as handled so later systems don't act on it too.
    pub fn consume(&mut self, a: Action) {
        self.just_pressed.remove(&a);
        if a == Action::Interact {
            self.used = false;
        }
    }

    /// An exclusive interaction owns both the tap and the held control.
    pub fn suppress(&mut self, a: Action) {
        self.pressed.remove(&a);
        self.just_pressed.remove(&a);
        if a == Action::Interact {
            self.used = false;
        }
    }

    /// Drops this frame's input (a box on screen has it).
    pub fn clear(&mut self) {
        self.pressed.clear();
        self.just_pressed.clear();
        self.used = false;
        self.movement = Vec2::ZERO;
        self.look = Vec2::ZERO;
        self.cycle = 0;
    }

    /// Prompt label for an action on the device in use.
    pub fn label(&self, a: Action, settings: &Settings) -> String {
        if self.gamepad_active {
            if a == Action::Mine {
                return "hold DPadLeft".into();
            }
            return match a.gamepad().first() {
                Some(GamepadButton::South) => "A".into(),
                Some(GamepadButton::East) => "B".into(),
                Some(GamepadButton::West) => "X".into(),
                Some(GamepadButton::North) => "Y".into(),
                Some(b) => format!("{b:?}"),
                None => "-".into(),
            };
        }
        settings.binding(a).map(|b| b.label()).unwrap_or_else(|| "-".into())
    }
}

#[allow(clippy::too_many_arguments)]
pub fn update_actions(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    gamepads: Query<&Gamepad>,
    captured: Res<InputCaptured>,
    settings: Res<Settings>,
    time: Res<Time<Real>>,
    mut state: ResMut<ActionState>,
) {
    let previous = std::mem::take(&mut state.pressed);
    state.just_pressed.clear();
    state.look = Vec2::ZERO;
    state.cycle = 0;
    state.movement = Vec2::ZERO;
    let mut pressed = HashSet::new();
    let mut device_keyboard = false;
    let mut device_pad = false;

    // Keyboard and mouse, only while the game has the cursor.
    if captured.0 {
        for a in Action::ALL {
            let down = match settings.binding(a) {
                Some(Binding::Key(k)) => keys.pressed(k),
                Some(Binding::Mouse(m)) => mouse.pressed(m),
                None => false,
            };
            if down {
                pressed.insert(a);
                device_keyboard = true;
            }
        }
        let invert = if settings.invert_y { -1.0 } else { 1.0 };
        let mouse_scale = 0.0022 * settings.sensitivity;
        state.look += Vec2::new(motion.delta.x, -motion.delta.y * invert) * mouse_scale;
        if motion.delta.length_squared() > 4.0 {
            device_keyboard = true;
        }
        state.cycle = -scroll.delta.y.signum() as i32;
        let axis = |pos: Action, neg: Action| f32::from(u8::from(pressed.contains(&pos))) - f32::from(u8::from(pressed.contains(&neg)));
        state.movement = Vec2::new(axis(Action::Right, Action::Left), axis(Action::Forward, Action::Back));
    }

    // FC3_LOOK_TEST (tests): a steady turn to the right, as a mouse would.
    if std::env::var_os("FC3_LOOK_TEST").is_some() {
        state.look += Vec2::new(0.01, 0.0);
    }

    // Gamepad: works whenever play has focus (no cursor capture needed).
    if let Some(pad) = gamepads.iter().next() {
        let dead = |v: Vec2, zone: f32| {
            let l = v.length();
            if l < zone {
                Vec2::ZERO
            } else {
                v / l * ((l - zone) / (1.0 - zone)).min(1.0)
            }
        };
        let stick = |x, y| Vec2::new(pad.get(x).unwrap_or(0.0), pad.get(y).unwrap_or(0.0));
        let left = dead(stick(GamepadAxis::LeftStickX, GamepadAxis::LeftStickY), 0.18);
        let right = dead(stick(GamepadAxis::RightStickX, GamepadAxis::RightStickY), 0.15);
        if left != Vec2::ZERO || right != Vec2::ZERO {
            device_pad = true;
        }
        let invert = if settings.invert_y { -1.0 } else { 1.0 };
        // Response curve: fine aim near the centre, fast turns at full tilt.
        let curved = right * right.length();
        let rate = 3.2 * settings.gamepad_sensitivity;
        state.look += Vec2::new(curved.x, curved.y * invert) * rate * time.delta_secs().min(0.05);
        state.movement += left;
        // D-pad left: a rock on a tap (on letting go), a mine on a hold.
        if pad.pressed(GamepadButton::DPadLeft) {
            device_pad = true;
            let before = state.pad_hold;
            state.pad_hold += time.delta_secs();
            if before < PAD_HOLD && state.pad_hold >= PAD_HOLD {
                pressed.insert(Action::Mine);
            }
        } else {
            if state.pad_hold > 0.0 && state.pad_hold < PAD_HOLD {
                pressed.insert(Action::ThrowRock);
            }
            state.pad_hold = 0.0;
        }
        for a in Action::ALL {
            if matches!(a, Action::ThrowRock | Action::Mine) {
                continue;
            }
            let buttons = a.gamepad();
            if buttons.iter().any(|b| pad.pressed(*b)) {
                pressed.insert(a);
                device_pad = true;
            }
            // Click-to-sprint and press-to-crouch latch on a gamepad.
            if a == Action::Sprint && buttons.iter().any(|b| pad.just_pressed(*b)) {
                state.sprint_latch = true;
            }
            if a == Action::Crouch && buttons.iter().any(|b| pad.just_pressed(*b)) {
                state.crouch_latch = !state.crouch_latch;
            }
        }
        if left.length() < 0.3 {
            state.sprint_latch = false;
        }
        if state.sprint_latch {
            pressed.insert(Action::Sprint);
            state.crouch_latch = false;
        }
        if state.crouch_latch {
            pressed.insert(Action::Crouch);
        }
    }
    state.movement = state.movement.clamp_length_max(1.0);
    if device_pad {
        state.gamepad_active = true;
    } else if device_keyboard {
        state.gamepad_active = false;
    }
    for a in &pressed {
        if !previous.contains(a) {
            state.just_pressed.insert(*a);
        }
    }
    let (used, held) = use_step(state.use_hold, pressed.contains(&Action::Interact), previous.contains(&Action::Interact), state.use_held, time.delta_secs());
    (state.used, state.use_held) = (used, held);
    state.pressed = pressed;
}

/// Using on foot, a frame on: whether it uses now, and how long the
/// interact key has been down. Once the key has been held the game's hold
/// time, once a hold; a press without it.
fn use_step(hold: Option<f32>, down: bool, was_down: bool, held: f32, dt: f32) -> (bool, f32) {
    let now = if down { held + dt } else { 0.0 };
    let used = match hold {
        Some(need) => down && held < need && now >= need,
        None => down && !was_down,
    };
    (used, now)
}
