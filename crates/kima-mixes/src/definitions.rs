//! Mood definitions (upstream `services/mixes/moodDefinitions.ts`): the layer between the learned heads and the
//! recipes. A definition rebuilds one mood from parts (moods, `head:<mood>` raw head scores, readings, `pace`) with
//! weights and `low:` parts like a recipe's components; every recipe component of that mood then reads the defined
//! score, standardised against the library. The shipped file defines focus and driving.

use crate::json::Json;
use crate::recipes::{DEFINITIONS_JSON, is_live_mood};
use std::sync::LazyLock;

/// The middle half of drivers' own picks (Navarro 2023). A tempo counts double or half, on a steady beat only.
pub const PACE_BAND: (f64, f64) = (94.0, 130.0);
/// A song with no tempo, or no steady beat to trust one from, has no pace to judge: it scores in the middle.
pub const NEUTRAL_PACE: f64 = 0.5;

/// A reading a definition can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionReading {
    Arousal,
    Instrumental,
    Acoustic,
}

impl DefinitionReading {
    pub fn name(self) -> &'static str {
        match self {
            DefinitionReading::Arousal => "arousal",
            DefinitionReading::Instrumental => "instrumental",
            DefinitionReading::Acoustic => "acoustic",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PartKind {
    /// A live, undefined mood's score.
    Mood(String),
    /// A live mood's raw head score (how a definition keeps its own head as a part).
    Head(String),
    Reading(DefinitionReading),
    Pace,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub kind: PartKind,
    /// Negative for a `low:` part.
    pub weight: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub mood: String,
    pub parts: Vec<Part>,
}

/// Upstream `parseDefinitions`: every key a live mood, every part known, positive weights, no part twice, never a
/// defined mood read as a part.
pub fn parse_definitions(text: &str) -> Result<Vec<Definition>, String> {
    let raw = Json::parse(text)?;
    let Some(entries) = raw.as_object() else {
        return Err("Definitions: not an object".into());
    };
    let defined: Vec<&str> = entries.iter().map(|(m, _)| m.as_str()).collect();
    let mut definitions = Vec::new();
    for (mood, entry) in entries {
        let where_ = format!("Definition {mood}");
        if !is_live_mood(mood) {
            return Err(format!("{where_}: not a mood a definition can rebuild"));
        }
        let Some(fields) = entry.as_object() else {
            return Err(format!("{where_}: not an object"));
        };
        let mut parts: Vec<Part> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for (key, value) in fields {
            let (low, name) = match key.strip_prefix("low:") {
                Some(rest) => (true, rest),
                None => (false, key.as_str()),
            };
            let weight = match value.as_f64() {
                Some(w) if w.is_finite() && w > 0.0 => w,
                _ => return Err(format!("{where_}: {key} needs a positive weight")),
            };
            let weight = if low { -weight } else { weight };
            let kind = match name {
                "pace" => PartKind::Pace,
                "arousal" => PartKind::Reading(DefinitionReading::Arousal),
                "instrumental" => PartKind::Reading(DefinitionReading::Instrumental),
                "acoustic" => PartKind::Reading(DefinitionReading::Acoustic),
                _ => {
                    if let Some(head) = name.strip_prefix("head:") {
                        if !is_live_mood(head) {
                            return Err(format!("{where_}: {name} is not a live mood's head"));
                        }
                        PartKind::Head(head.to_string())
                    } else if is_live_mood(name) {
                        if defined.contains(&name) {
                            return Err(format!(
                                "{where_}: {name} is itself defined, and a definition cannot read a defined mood; use head:{name} for its raw score"
                            ));
                        }
                        PartKind::Mood(name.to_string())
                    } else {
                        return Err(format!("{where_}: unknown part {name}"));
                    }
                }
            };
            let id = match &kind {
                PartKind::Pace => "pace".to_string(),
                PartKind::Reading(r) => r.name().to_string(),
                PartKind::Mood(m) => format!("mood:{m}"),
                PartKind::Head(m) => format!("head:{m}"),
            };
            if seen.contains(&id) {
                return Err(format!("{where_}: {name} appears twice"));
            }
            seen.push(id);
            parts.push(Part { kind, weight });
        }
        if parts.is_empty() {
            return Err(format!("{where_}: no parts"));
        }
        definitions.push(Definition {
            mood: mood.clone(),
            parts,
        });
    }
    Ok(definitions)
}

/// The shipped definitions, parsed once.
pub static DEFINITIONS: LazyLock<Vec<Definition>> = LazyLock::new(|| {
    parse_definitions(DEFINITIONS_JSON).expect("the vendored moodDefinitions.json parses")
});

/// The definition of `mood`, if the shipped file rebuilds it.
pub fn definition_of(mood: &str) -> Option<&'static Definition> {
    DEFINITIONS.iter().find(|d| d.mood == mood)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_definitions_are_focus_and_driving() {
        let focus = definition_of("focus").unwrap();
        assert_eq!(
            focus.parts,
            vec![
                Part {
                    kind: PartKind::Reading(DefinitionReading::Instrumental),
                    weight: 0.5
                },
                Part {
                    kind: PartKind::Mood("calm".into()),
                    weight: 0.3
                },
                Part {
                    kind: PartKind::Head("focus".into()),
                    weight: 0.2
                },
            ]
        );
        let driving = definition_of("driving").unwrap();
        assert_eq!(
            driving.parts[1],
            Part {
                kind: PartKind::Reading(DefinitionReading::Instrumental),
                weight: -0.25
            }
        );
        assert_eq!(driving.parts[2].kind, PartKind::Pace);
        assert_eq!(DEFINITIONS.len(), 2);
    }

    #[test]
    fn refusals() {
        for (text, message) in [
            (
                r#"{"night": {"calm": 1}}"#,
                "not a mood a definition can rebuild",
            ),
            (r#"{"focus": {"focus": 1}}"#, "is itself defined"),
            (
                r#"{"focus": {"head:night": 1}}"#,
                "is not a live mood's head",
            ),
            (r#"{"focus": {"calm": 0}}"#, "needs a positive weight"),
            (
                r#"{"focus": {"calm": 1, "low:calm": 1}}"#,
                "calm appears twice",
            ),
            (r#"{"focus": {"tempo": 1}}"#, "unknown part tempo"),
            (r#"{"focus": {}}"#, "no parts"),
        ] {
            let err = parse_definitions(text).unwrap_err();
            assert!(err.contains(message), "{text}: {err}");
        }
    }
}
