//! The game's frontend (`MultiMain.feu`, its main menu) in place of ours:
//! loaded over everything while the game is at its main menu, given what
//! it asks for, the keyboard and the mouse (it draws its own cursor).
//!
//! It loads its pages one by one (co-op and multiplayer's among them), then
//! asks `RequestMultiMainState`; the answer (`FrontEnd`
//! `RequestMultiMainStateResponse gameStart`) opens its home list, which it
//! asks for (`OnRequestMainMenu`): `MainMenuDataProvider`, the story and the
//! options (`locTitle`, `iAction`: 1 and 6; co-op, multiplayer and the map
//! editor aren't offered) and quitting (41). A choice asks
//! `AllowFrontendAction <action>` (`AllowFrontendActionResponse <action>
//! 1`), and quitting `QuitToDesktop`.
//!
//! The story: `OnRequestLoadGameFilePreview` is answered `InitStoryPage`
//! ("1" with a save), and the campaign page (`singlePlayerMenu.feu`) asks
//! which of its six slots hold one (`GetSaveGamePresence`: autosave 1, save
//! 1, autosave 2...; ours is the one, autosave 1) and, as each line is
//! chosen, what is in it (`RequestSaveGameInfo <type> <slot>`:
//! `SaveGameProvider`). `LoadSaveGame` goes on from the save
//! (`save::ContinueGame`); a new game's slot is taken
//! (`ValidateNewGameSlot`, `NewGameSlotValidated`) and `InitNewGame
//! <slot> <difficulty>` starts it. The options page takes the pause menu's
//! answers (`OnOptionLoaded`, `OnRequestOptions`, `SetOptions`).
//! `FC3_FRONTEND_KEYS=Enter,Down,...` (tests) presses keys once the home
//! list is up, 0.6 s apart.
//!
//! Before it, the first time, come the Ubisoft logo (`M_LogoUbi.feu`: it
//! says `UbiLogoEvent 1` near its end) and the title, "Press any key."
//! (`PressStartScreen.feu`: `ProcessInitialScreen PressStart begin`; a key
//! or a click is its `StartPressed`, and it says `ProcessInitialScreen
//! PressStart end` once it has gone). Its text is the consoles' "PRESS
//! START" on its timeline (`text.mc.Normal_txt` and the shadow's `mcB`;
//! the `SetText` its class listens for has no field here), so the PC's is
//! put in it while it shows; nor has it the label that says it has gone, so
//! the menu comes a moment after the key. A run with `FC3_FRONTEND_KEYS`
//! presses there by itself. A game started again from the pause menu's
//! "Quit" (`save::BackToMenu`) comes straight to the menu.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use fc3_ui::ExternalValue as V;

use super::Interface;

const FRONTEND_MOVIE: &str = r"UI\Multiplayer\Menu\MultiMain.feu";
const LOGO_MOVIE: &str = r"UI\Singleplayer\Menu\MainMenu\M_LogoUbi.feu";
const PRESS_START_MOVIE: &str = r"UI\Singleplayer\Menu\PressStartScreen\PressStartScreen.feu";
/// The title's text (PC's), and how long the logo and the title's way out
/// may take before the menu comes anyway.
const PRESS_ANY_KEY: u32 = 159971;
const LOGO_MOST: f32 = 8.0;
const PRESS_OUT_MOST: f32 = 0.8;
/// The loop it plays behind (it loads it into a clip; the engine drew it).
const FRONTEND_VIDEO: &str = r"UI\Singleplayer\Video\Swan_30_Sec_loop.bik";
const FRONTEND_LEVEL: u32 = 20;

/// The home list's entries: their names (string ids) and actions.
const HOME: [(u32, u32); 3] = [(150445, STORY), (150446, OPTIONS), (155610, QUIT)];
const STORY: u32 = 1;
const OPTIONS: u32 = 6;
const QUIT: u32 = 41;

/// The campaign page's save types (`C_SaveType_*`) and slot names.
const SAVE_NONE: f64 = -1.0;
const SAVE_AUTO: i64 = 0;
const SAVE_LATEST: i64 = 2;
const AUTOSAVE_1: u32 = 157885;
/// The difficulty a save shows (the game's middle one).
const DIFFICULTY_NORMAL: u32 = 150452;
const CHAPTER: u32 = 158019;
/// The main missions, and the chapters (pictures) the page has for them.
const MAIN_MISSIONS: u32 = 38;
const CHAPTERS: u32 = 11;

/// Where the boot has got to: the logo, the title, the menu.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
enum Boot {
    #[default]
    Logo,
    Title,
    Menu,
}

#[derive(Default)]
pub(super) struct FrontendShown {
    loaded: bool,
    boot: Boot,
    /// When the logo or title came up, and when the title was left.
    since: Option<std::time::Instant>,
    started: Option<std::time::Instant>,
    /// The home list is up (the test's keys go then), and when.
    home: Option<std::time::Instant>,
    pressed: usize,
    mouse: Vec2,
}

/// The menu's loop, under the interface.
fn backdrop(videos: &mut crate::video::Videos) {
    videos.requests.push(crate::video::VideoRequest {
        path: FRONTEND_VIDEO.into(),
        fullscreen: false,
        looped: true,
        volume: 1.0,
        screen: None,
        backdrop: true,
    });
}

/// A save's chapter (1 to 11, as the page pictures them): how far into the
/// main missions it is.
fn chapter(main_done: u32) -> u32 {
    (1 + main_done * CHAPTERS / MAIN_MISSIONS).min(CHAPTERS)
}

/// Hours played as the page shows them.
fn time_played(hours: f32) -> String {
    let minutes = (hours.max(0.0) * 60.0).round() as u64;
    format!("{}h {:02}m", minutes / 60, minutes % 60)
}

/// A file's time as a date (UTC).
fn date(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // (Days to a civil date: Howard Hinnant's.)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}/{month:02}/{day:02} {:02}:{:02}", rest / 3600, rest % 3600 / 60)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn drive_frontend(
    state: NonSend<Interface>,
    calls: Res<super::UiCalls>,
    game: Res<State<crate::menu::GameState>>,
    mut next: ResMut<NextState<crate::menu::GameState>>,
    mut original: ResMut<crate::menu::OriginalPause>,
    (slot, mut settings, displays): (Res<crate::save::SaveSlot>, ResMut<crate::settings::Settings>, Res<crate::settings::Displays>),
    (mut continue_game, mut exit): (MessageWriter<crate::save::ContinueGame>, MessageWriter<AppExit>),
    (mut keys, mut moves, mut buttons): (
        MessageReader<bevy::input::keyboard::KeyboardInput>,
        MessageReader<CursorMoved>,
        MessageReader<bevy::input::mouse::MouseButtonInput>,
    ),
    mut windows: Query<(&Window, &mut bevy::window::CursorOptions), With<PrimaryWindow>>,
    mut videos: ResMut<crate::video::Videos>,
    mut shown: Local<FrontendShown>,
) {
    use crate::menu::GameState;
    use fc3_ui::input::{MouseButton, PlayerEvent};
    let Interface::Running(host) = &*state else { return };
    let at_menu = *game.get() == GameState::MainMenu;
    if at_menu != shown.loaded {
        shown.loaded = at_menu;
        original.0 = at_menu;
        keys.clear();
        moves.clear();
        buttons.clear();
        if at_menu {
            for (key, value) in [("ShowGamePadInterface", "0"), ("IsSplitscreen", "0"), ("IsInvertActivateCancel", "0"), ("GamerIndex", "0")] {
                host.answer(key, value);
            }
            host.answer("VIEWPORT_WIDTH", &fc3_ui::STAGE.0.to_string());
            host.answer("VIEWPORT_HEIGHT", &fc3_ui::STAGE.1.to_string());
            host.level_visible(super::HUD_LEVEL, false);
            // (Back from a game, the menu comes at once.)
            if shown.boot == Boot::Logo && std::env::var_os(crate::save::BOOT_MENU).is_some() {
                shown.boot = Boot::Menu;
            }
            if shown.boot == Boot::Logo {
                host.load(LOGO_MOVIE, FRONTEND_LEVEL);
                shown.since = Some(std::time::Instant::now());
            } else {
                shown.boot = Boot::Menu;
                host.load(FRONTEND_MOVIE, FRONTEND_LEVEL);
                backdrop(&mut videos);
            }
            // (It draws its own cursor.)
            if let Ok((_, mut cursor)) = windows.single_mut() {
                cursor.visible = false;
            }
            shown.home = None;
            shown.pressed = 0;
        } else {
            host.unload(FRONTEND_LEVEL);
            videos.stop_backdrop();
            host.level_visible(super::HUD_LEVEL, true);
        }
        return;
    }
    if !at_menu {
        return;
    }
    let save = slot.existing.as_ref();
    // The boot's own steps, and their fallbacks.
    let waited = shown.since.map_or(0.0, |t| t.elapsed().as_secs_f32());
    let logo_done = calls.0.iter().any(|c| c.args.first().is_some_and(|a| a == "UbiLogoEvent") && c.args.get(1).is_some_and(|a| a == "1"));
    if shown.boot == Boot::Logo && (logo_done || waited > LOGO_MOST) {
        shown.boot = Boot::Title;
        shown.since = Some(std::time::Instant::now());
        shown.started = None;
        host.load(PRESS_START_MOVIE, FRONTEND_LEVEL);
        backdrop(&mut videos);
    }
    let title_done = calls.0.iter().any(|c| c.args.first().is_some_and(|a| a == "ProcessInitialScreen") && c.args.get(2).is_some_and(|a| a == "end"));
    if shown.boot == Boot::Title && (title_done || shown.started.is_some_and(|t| t.elapsed().as_secs_f32() > PRESS_OUT_MOST)) {
        shown.boot = Boot::Menu;
        host.load(FRONTEND_MOVIE, FRONTEND_LEVEL);
    }
    for call in calls.0.iter().filter(|c| c.command.eq_ignore_ascii_case("Send")) {
        let arg = |i: usize| call.args.get(i).map(String::as_str).unwrap_or("");
        match arg(0) {
            "UbiLogoEvent" | "ProcessInitialScreen" => {}
            "RequestMultiMainState" => {
                // (Its loop plays under the interface: the black the engine
                // drew it over goes.)
                host.set_path(&format!("_level{FRONTEND_LEVEL}.bg.black._visible"), V::Bool(false));
                host.event("FrontEnd", &["RequestMultiMainStateResponse", "gameStart", "0", "0", "0", "0", "0"]);
                host.event("FrontEnd", &["OnVersionNumber", env!("CARGO_PKG_VERSION")]);
            }
            "OnRequestMainMenu" => {
                let rows = HOME
                    .iter()
                    .map(|&(name, action)| {
                        [("locTitle", V::String(host.text(name))), ("iAction", V::Number(action as f64)), ("bShortcut", V::Bool(false))]
                            .into_iter()
                            .map(|(k, v)| (k.to_string(), v))
                            .collect()
                    })
                    .collect();
                host.set_data("MainMenuDataProvider", rows);
                shown.home.get_or_insert_with(std::time::Instant::now);
            }
            "AllowFrontendAction" => {
                let allowed = matches!(arg(1).parse::<u32>(), Ok(STORY | OPTIONS));
                host.event("FrontEnd", &["AllowFrontendActionResponse", arg(1), if allowed { "1" } else { "0" }]);
            }
            "QuitToDesktop" => {
                exit.write(AppExit::Success);
            }
            "OnRequestLoadGameFilePreview" => host.event("InitStoryPage", &[if save.is_some() { "1" } else { "0" }]),
            "GetSaveGamePresence" => host.event("GetSaveGamePresence", &[if save.is_some() { "100000" } else { "000000" }]),
            "RequestSaveGameInfo" => {
                let (kind, slot_no) = (arg(1).parse::<i64>().unwrap_or(-1), arg(2).parse::<i64>().unwrap_or(-1));
                let ours = kind == SAVE_LATEST || (kind == SAVE_AUTO && slot_no == 1);
                let row: std::collections::BTreeMap<String, V> = match save.filter(|_| ours) {
                    Some(s) => {
                        let main = crate::statistics::main_missions_done(&s.progress);
                        [
                            ("iType", V::Number(SAVE_AUTO as f64)),
                            ("iSlot", V::Number(1.0)),
                            ("sSlotName", V::String(host.text(AUTOSAVE_1))),
                            ("sCurrentChapter", V::String(format!("{} {}", host.text(CHAPTER), chapter(main)))),
                            ("sTimePlayed", V::String(time_played(s.hours))),
                            ("sSaveLocation", V::String(String::new())),
                            ("sDifficultyLevel", V::String(host.text(DIFFICULTY_NORMAL))),
                            ("sTimeSaved", V::String(slot.saved_at().map(date).unwrap_or_default())),
                        ]
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v))
                        .collect()
                    }
                    None => [("iType".to_string(), V::Number(SAVE_NONE)), ("iSlot".to_string(), V::Number(slot_no as f64))].into_iter().collect(),
                };
                host.set_data("SaveGameProvider", vec![row]);
            }
            "LoadSaveGame" if save.is_some() => {
                continue_game.write(crate::save::ContinueGame);
                next.set(GameState::Playing);
            }
            "ValidateNewGameSlot" => host.event("NewGameSlotValidated", &[]),
            "InitNewGame" => next.set(GameState::Playing),
            "OnOptionLoaded" => super::send_option_sections(host),
            "OnRequestOptions" => {
                let video = super::VideoInfo::new(&settings, &displays, super::window_size(&windows));
                super::send_options(host, &settings, &video, arg(1).parse().unwrap_or(0));
            }
            "SetOptions" => {
                let values: Vec<i32> = arg(1).split(',').map(|v| v.trim().parse().unwrap_or(0)).collect();
                let video = super::VideoInfo::new(&settings, &displays, super::window_size(&windows));
                super::set_options(&mut settings, &video, arg(2).parse().unwrap_or(u32::MAX), &values);
            }
            other => debug!("original UI: frontend {other} {:?}", call.args.get(1..)),
        }
    }
    let scale = windows.single().map_or(1.0, |(w, _)| w.scale_factor()) as f64;
    for m in moves.read() {
        shown.mouse = m.position;
        host.input(PlayerEvent::MouseMove { x: m.position.x as f64 * scale, y: m.position.y as f64 * scale });
    }
    let mut clicked = false;
    for b in buttons.read() {
        clicked |= b.state.is_pressed();
        let button = match b.button {
            bevy::input::mouse::MouseButton::Left => MouseButton::Left,
            bevy::input::mouse::MouseButton::Right => MouseButton::Right,
            bevy::input::mouse::MouseButton::Middle => MouseButton::Middle,
            _ => continue,
        };
        let (x, y) = (shown.mouse.x as f64 * scale, shown.mouse.y as f64 * scale);
        host.input(if b.state.is_pressed() { PlayerEvent::MouseDown { x, y, button, index: None } } else { PlayerEvent::MouseUp { x, y, button } });
    }
    let mut pressed_any = false;
    for k in keys.read() {
        pressed_any |= k.state.is_pressed();
        let Some(key) = super::movie_key(k.key_code) else { continue };
        host.input(if k.state.is_pressed() { PlayerEvent::KeyDown { key } } else { PlayerEvent::KeyUp { key } });
    }
    // (The PC's text, kept in its fields as its timeline plays.)
    if shown.boot == Boot::Title {
        let text = host.text(PRESS_ANY_KEY).to_uppercase();
        for clip in ["mc", "mcB"] {
            host.set_path(&format!("_level{FRONTEND_LEVEL}.text.{clip}.Normal_txt.text"), V::String(text.clone()));
        }
    }
    // The title goes with any key or click (or by itself in a test).
    if shown.boot == Boot::Title && shown.started.is_none() {
        let test = std::env::var_os("FC3_FRONTEND_KEYS").is_some() && waited > 2.0;
        if pressed_any || clicked || test {
            host.event("StartPressed", &[]);
            shown.started = Some(std::time::Instant::now());
        }
    }
    // FC3_FRONTEND_KEYS (tests): keys pressed one by one once the home list
    // has been up two seconds.
    if let (Ok(list), Some(since)) = (std::env::var("FC3_FRONTEND_KEYS"), shown.home) {
        let names: Vec<&str> = list.split(',').collect();
        if since.elapsed().as_secs_f32() > 2.0 + 0.6 * shown.pressed as f32 {
            if let Some(key) = names.get(shown.pressed).and_then(|n| super::test_key(n)).and_then(super::movie_key) {
                host.input(PlayerEvent::KeyDown { key: key.clone() });
                host.input(PlayerEvent::KeyUp { key });
            }
            shown.pressed += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_shows_its_chapter_time_and_date() {
        assert_eq!((chapter(0), chapter(19), chapter(38), chapter(60)), (1, 6, 11, 11));
        assert_eq!(time_played(10.530233), "10h 32m");
        assert_eq!(date(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_792_315_800)), "2026/10/18 09:30");
    }
}
