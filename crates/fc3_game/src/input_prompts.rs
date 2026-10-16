//! The prompts a script puts up (`CGSFInputManager`): up to four buttons,
//! each a localized text (`Button_N_OasisID`) and the button it is on
//! (`ButtonType_N`), shown in the HUD's prompt while the gameplay runs.
//! BNG_010's bar (buying a drink, stopping) and CIT_050's end (the takedown).

use std::collections::HashMap;

use bevy::prelude::*;

pub struct InputPromptsPlugin;

impl Plugin for InputPromptsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputPrompts>().add_systems(Update, show.after(crate::story::StorySet).before(crate::hud::HudSet));
    }
}

/// The gameplays putting prompts up, by id: each button's type and text id.
#[derive(Resource, Default)]
pub struct InputPrompts {
    active: Vec<(u64, Vec<(i64, u32)>)>,
}

impl InputPrompts {
    /// A script's `CGSFInputManager` started with these values.
    pub fn start(&mut self, id: u64, params: &HashMap<String, String>) {
        let buttons = (1..=4)
            .filter_map(|i| {
                let text = params.get(&format!("Button_{i}_OasisID"))?.parse::<u32>().ok().filter(|&t| t != 0 && t != u32::MAX)?;
                let kind = params.get(&format!("ButtonType_{i}")).and_then(|v| v.parse().ok()).unwrap_or(i);
                Some((kind, text))
            })
            .collect();
        self.active.retain(|(a, _)| *a != id);
        self.active.push((id, buttons));
    }

    pub fn end(&mut self, id: u64) {
        self.active.retain(|(a, _)| *a != id);
    }

    pub fn reset(&mut self) {
        self.active.clear();
    }
}

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
    prompts: Res<InputPrompts>,
    host: Option<NonSend<crate::story::StoryHost>>,
    actions: Res<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    mut hud: ResMut<crate::hud::HudState>,
) {
    let (Some((_, buttons)), Some(host)) = (prompts.active.last(), host) else { return };
    let strings = &host.story.data().strings;
    let line: Vec<String> = buttons
        .iter()
        .filter_map(|&(kind, text)| {
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
        assert_eq!(prompts.active, vec![(7, vec![(1, 150745), (2, 150746)])]);
        prompts.start(7, &params(&[("Button_1_OasisID", "119667"), ("ButtonType_1", "22")]));
        assert_eq!(prompts.active, vec![(7, vec![(22, 119667)])]);
        assert_eq!(action_of(22), Some(crate::controls::Action::Melee));
        prompts.end(7);
        assert!(prompts.active.is_empty());
    }
}
