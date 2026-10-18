//! The Trials of the Rakyat (the engine's "social challenges"): each trial's
//! row of the challenge table (`challengedata.lib`), the points its kills
//! and bonuses score (`challengescorerules.lib`), and the challenge
//! manager's template (its rewards, music, units and sounds).

use anyhow::{Context, Result};

use crate::fcb::Object;

/// The tables, in the cache (copied as they are: `story::RAW_FILES`).
pub const DATA_REL: &str = "story/challengedata.lib";
pub const RULES_REL: &str = "story/challengescorerules.lib";

/// A trial: its mission block's name (`Kill/<BLOCK>` in the table), its
/// id (what the scripts and the engine's events call it), how long it runs
/// (seconds), whether grenades never run out, its name and description
/// (string ids), the scores that earn each reward (the first money, the XP,
/// the second money), and the scores to beat (the developers', by name).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Challenge {
    pub block: String,
    pub id: u32,
    pub duration: f32,
    pub infinite_grenades: bool,
    pub name: u32,
    pub description: u32,
    pub thresholds: [u32; 3],
    pub challengers: Vec<(String, f32)>,
}

/// What a kill or a bonus scores: its bonus bit (the target's
/// `CFCXChallengeBonusComponent` kind, or a kill's way), its points (or
/// seconds, for a time bonus), its name (a string id) and its place among
/// the popups.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScoreRule {
    pub bonus: u32,
    pub score: i32,
    pub time: bool,
    pub name: u32,
    pub priority: u32,
}

/// The bonus bits the rules score by.
pub mod bonus {
    pub const TIME_BARREL: u32 = 0x2;
    pub const HEADSHOT: u32 = 0x4;
    pub const STEALTH: u32 = 0x8;
    pub const TAKEDOWN: u32 = 0x10;
    pub const HEAVY_GUNNER: u32 = 0x20;
    pub const ROCKET: u32 = 0x80;
    pub const DEFENDER: u32 = 0x200;
    pub const ASSAULTER: u32 = 0x400;
    pub const BERSERKER: u32 = 0x800;
    pub const MOLOTOV: u32 = 0x1000;
    pub const BEHEADER: u32 = 0x2000;
}

/// The challenge manager (`FCXChallengeManager`): what each reward pays
/// (money, XP, money), the music's state, the units' names (seconds,
/// points), and its sounds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Manager {
    pub rewards: [u32; 3],
    pub music: String,
    pub units: [u32; 2],
    pub sounds: Sounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Sounds {
    pub timer_ending: u32,
    pub kill: u32,
    pub kill_bonus: u32,
    pub success: u32,
    pub failed: u32,
    pub time_bonus: u32,
}

/// The `Kill/<BLOCK>` path field (its name isn't known, only its hash).
const PATH: u32 = 0x9D88_73F8;

/// A field as a number however wide it was written (a byte, a short or a
/// word).
fn int(o: &Object, name: &str) -> Option<u32> {
    let b = o.field(name)?;
    Some(match b.len() {
        1 => b[0] as u32,
        2 | 3 => u16::from_le_bytes([b[0], b[1]]) as u32,
        0 => return None,
        _ => u32::from_le_bytes(b[..4].try_into().unwrap()),
    })
}

fn string_id(o: &Object, child: &str) -> u32 {
    o.child(child).and_then(|c| int(c, "locidLocalizedId")).unwrap_or(0)
}

/// Every trial of the challenge table, in its order.
pub fn parse_challenges(bytes: &[u8]) -> Result<Vec<Challenge>> {
    let root = crate::fcb::parse(bytes).context("the challenge table")?;
    Ok(root
        .children
        .iter()
        .filter_map(|row| {
            let path = row.str_h(PATH)?;
            let block = path.strip_prefix("Kill/").unwrap_or(&path).to_string();
            let thresholds = row
                .child("RewardsThresholds")
                .map(|t| [int(t, "iThresholdMoney0").unwrap_or(0), int(t, "iThresholdXP").unwrap_or(0), int(t, "iThresholdMoney1").unwrap_or(0)])
                .unwrap_or_default();
            let challengers = row
                .child("Challengers")
                .map(|list| {
                    list.children
                        .iter()
                        .filter_map(|c| {
                            let name = c.str("sName").filter(|n| !n.is_empty() && n.chars().all(|ch| !ch.is_control()))?;
                            let score = c.child("Score").and_then(|s| s.f32("fScore")).unwrap_or(0.0);
                            Some((name, score))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Challenge {
                block,
                id: int(row, "disId").unwrap_or(0),
                duration: int(row, "iDuration").unwrap_or(90) as f32,
                infinite_grenades: row.bool("iInfiniteGrenades").unwrap_or(false),
                name: string_id(row, "Name"),
                description: string_id(row, "Description"),
                thresholds,
                challengers,
            })
        })
        .collect())
}

/// The default score rules (the table's first set).
pub fn parse_rules(bytes: &[u8]) -> Result<Vec<ScoreRule>> {
    let root = crate::fcb::parse(bytes).context("the score rules")?;
    let set = root.children.first().context("no score rules")?;
    let list = set.child("ListIncrement").context("score rules without their list")?;
    Ok(list
        .children
        .iter()
        .map(|r| ScoreRule {
            bonus: int(r, "selBonusType").unwrap_or(0),
            score: int(r, "iScore").unwrap_or(0) as i32,
            time: int(r, "selRewardType").unwrap_or(1) == 0,
            name: string_id(r, "ScoreName"),
            priority: int(r, "iDisplayPriority").unwrap_or(0),
        })
        .collect())
}

impl Manager {
    /// From the templates file (`story/templates.fcb`).
    pub fn parse(templates: &[u8]) -> Result<Self> {
        let root = crate::fcb::parse(templates).context("the templates")?;
        let t = root
            .children
            .iter()
            .find(|c| c.str("Name").as_deref() == Some("FCXChallengeManager"))
            .and_then(|c| c.child("Template"))
            .context("no challenge manager template")?;
        let rewards = t
            .child("Rewards")
            .map(|r| {
                [
                    int(r, "iRewardForThresholdMoney0").unwrap_or(0),
                    int(r, "iRewardForThresholdXP").unwrap_or(0),
                    int(r, "iRewardForThresholdMoney1").unwrap_or(0),
                ]
            })
            .unwrap_or_default();
        let units = t.child("Bonuses").map(|b| [string_id(b, "Time"), string_id(b, "Score")]).unwrap_or_default();
        let sound = |n: &str| {
            t.child("Sounds")
                .and_then(|s| s.child(n))
                .and_then(|s| s.str("sndSoundID"))
                .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                .filter(|&i| i != u32::MAX)
                .unwrap_or(0)
        };
        Ok(Manager {
            rewards,
            music: t.str_h(0x5A27_B713).unwrap_or_default(),
            units,
            sounds: Sounds {
                timer_ending: sound("TimerEnding"),
                kill: sound("KillObjectiveCompleted"),
                kill_bonus: sound("KillObjectiveBonus"),
                success: sound("ChallengeSuccess"),
                failed: sound("ChallengeFailed"),
                time_bonus: sound("TimeBonus"),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs the converted cache"]
    fn installed_challenges() {
        let root = crate::cache::default_cache_root();
        let trials = parse_challenges(&std::fs::read(root.join(DATA_REL)).unwrap()).unwrap();
        let g3 = trials.iter().find(|t| t.block == "G3_SC_05").expect("G3's trial");
        assert_eq!((g3.id, g3.duration as u32, g3.thresholds), (26, 90, [150, 225, 300]));
        assert_eq!(g3.challengers.len(), 4);
        assert!(trials.iter().filter(|t| t.block.contains("_SC_")).count() >= 12);
        let rules = parse_rules(&std::fs::read(root.join(RULES_REL)).unwrap()).unwrap();
        assert!(rules.iter().any(|r| r.bonus == bonus::TIME_BARREL && r.time && r.score == 10));
        assert!(rules.iter().any(|r| r.bonus == bonus::HEAVY_GUNNER && r.score == 50));
        let manager = Manager::parse(&std::fs::read(root.join("story/templates.fcb")).unwrap()).unwrap();
        assert_eq!(manager.rewards, [250, 400, 500]);
        assert_ne!(manager.sounds.success, 0);
    }
}
