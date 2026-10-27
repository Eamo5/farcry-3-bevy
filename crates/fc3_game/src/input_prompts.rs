//! The prompts a script puts up (`CGSFInputManager`): up to four buttons,
//! each a localized text (`Button_N_OasisID`) and the button it is on
//! (`ButtonType_N`), shown in the HUD's prompt while the gameplay runs.
//! BNG_010's bar (buying a drink, stopping) and CIT_050's end (the takedown).
//! Pressing a button tells the scripts it was chosen
//! (`GSFInputManager_Choice_N`, by its number).

use std::collections::HashMap;

use bevy::prelude::*;

pub struct InputPromptsPlugin;

impl Plugin for InputPromptsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputPrompts>().add_systems(Update, show.after(crate::story::StorySet).before(crate::hud::HudSet));
    }
}

/// A button put up: its number, type and text id.
type Button = (usize, i64, u32);

/// The gameplays putting prompts up, by id: their buttons, and how long
/// they have been up.
#[derive(Resource, Default)]
pub struct InputPrompts {
    active: Vec<(u64, Vec<Button>)>,
    up_for: f32,
    /// Gameplays the autopilot has chosen for.
    chosen: Vec<u64>,
}

impl InputPrompts {
    /// A script's `CGSFInputManager` started with these values.
    pub fn start(&mut self, id: u64, params: &HashMap<String, String>) {
        let buttons = (1..=4)
            .filter_map(|i| {
                let text = params.get(&format!("Button_{i}_OasisID"))?.parse::<u32>().ok().filter(|&t| t != 0 && t != u32::MAX)?;
                let kind = params.get(&format!("ButtonType_{i}")).and_then(|v| v.parse().ok()).unwrap_or(i as i64);
                Some((i, kind, text))
            })
            .collect();
        self.active.retain(|(a, _)| *a != id);
        self.active.push((id, buttons));
        self.up_for = 0.0;
    }

    pub fn end(&mut self, id: u64) {
        self.active.retain(|(a, _)| *a != id);
    }

    pub fn reset(&mut self) {
        self.active.clear();
        self.chosen.clear();
    }

    /// The button chosen now, if any: the first whose action was just
    /// pressed, or for the autopilot, a while up, the first it can press.
    fn choice(&self, pressed: impl Fn(crate::controls::Action) -> bool, autopilot: bool) -> Option<usize> {
        let (id, buttons) = self.active.last()?;
        let known = || buttons.iter().filter_map(|&(i, kind, _)| action_of(kind).map(|a| (i, a)));
        known().find(|&(_, a)| pressed(a)).map(|(i, _)| i).or_else(|| {
            (autopilot && self.up_for > AUTOPILOT_AFTER && !self.chosen.contains(id)).then(|| known().next().map(|(i, _)| i)).flatten()
        })
    }
}

/// How long (s) the autopilot reads the prompts before it chooses.
const AUTOPILOT_AFTER: f32 = 1.5;

/// The game's button types we know an action for: the face button that
/// interacts, and the takedown.
fn action_of(kind: i64) -> Option<crate::controls::Action> {
    match kind {
        1 => Some(crate::controls::Action::Interact),
        22 => Some(crate::controls::Action::Melee),
        _ => None,
    }
}

fn show(
    mut prompts: ResMut<InputPrompts>,
    host: Option<NonSendMut<crate::story::StoryHost>>,
    time: Res<Time>,
    actions: Res<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    mut hud: ResMut<crate::hud::HudState>,
) {
    let Some(mut host) = host else { return };
    let Some((id, buttons)) = prompts.active.last().cloned() else { return };
    prompts.up_for += time.delta_secs();
    let autopilot = std::env::var_os("FC3_STORY_AUTOPILOT").is_some();
    // (CIT_050's end waits to hear the takedown chosen; nothing was told,
    // and the dream never ended.)
    if let Some(i) = prompts.choice(|a| actions.just_pressed(a), autopilot) {
        info!("input prompts: button {i} chosen");
        host.story.message(&format!("GSFInputManager_Choice_{i}"));
        prompts.chosen.push(id);
    }
    let strings = &host.story.data().strings;
    let line: Vec<String> = buttons
        .iter()
        .filter_map(|&(_, kind, text)| {
            let text = crate::story::plain(strings.get(text)?);
            Some(match action_of(kind) {
                Some(a) => format!("{}  {text}", actions.label(a, &settings)),
                None => text,
            })
        })
        .collect();
    if !line.is_empty() {
        hud.prompt = Some(line.join("     "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each button with a text comes up on its own type (its number when
    /// the script gives none); a second start of the same gameplay replaces
    /// the first, and its end takes them down.
    #[test]
    fn a_scripts_buttons_come_up_with_their_types() {
        let params = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>();
        let mut prompts = InputPrompts::default();
        prompts.start(7, &params(&[("Button_1_OasisID", "150745"), ("Button_2_OasisID", "150746"), ("Button_3_OasisID", "4294967295")]));
        assert_eq!(prompts.active, vec![(7, vec![(1, 1, 150745), (2, 2, 150746)])]);
        prompts.start(7, &params(&[("Button_1_OasisID", "119667"), ("ButtonType_1", "22")]));
        assert_eq!(prompts.active, vec![(7, vec![(1, 22, 119667)])]);
        assert_eq!(action_of(22), Some(crate::controls::Action::Melee));
        prompts.end(7);
        assert!(prompts.active.is_empty());
    }

    /// A button is chosen by pressing its action; the autopilot, once they
    /// have been up a while, takes the first it can press, once.
    #[test]
    fn a_button_is_chosen_by_its_action() {
        use crate::controls::Action;
        let params = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>();
        let mut prompts = InputPrompts::default();
        prompts.start(9, &params(&[("Button_1_OasisID", "5"), ("ButtonType_1", "7"), ("Button_2_OasisID", "6"), ("ButtonType_2", "22")]));
        assert_eq!(prompts.choice(|_| false, false), None);
        assert_eq!(prompts.choice(|a| a == Action::Melee, false), Some(2));
        assert_eq!(prompts.choice(|_| false, true), None);
        prompts.up_for = 2.0;
        assert_eq!(prompts.choice(|_| false, true), Some(2));
        prompts.chosen.push(9);
        assert_eq!(prompts.choice(|_| false, true), None);
    }
}
