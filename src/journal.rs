//! The Journal sheet: one dated row per thing the bot did (a game added, a
//! grade given or changed, a game played and by whom, a price change). The
//! Notes sheet only holds the current state; this is what the site's history
//! and timelines are drawn from.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use crate::model::{GameKey, cell_number, cell_text};

pub const HEADER: [&str; 8] = [
    "Date",
    "Action",
    "Jeu",
    "Nom du jeu",
    "Personne",
    "Valeur",
    "Ancienne valeur",
    "Détails",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// A game was proposed (Personne: who proposed it).
    Added,
    /// A grade before playing (Valeur; Ancienne valeur when it replaced one).
    GradeBefore,
    /// A grade after playing.
    GradeAfter,
    /// A game was marked played; Détails lists who played, comma separated.
    Played,
    /// The Steam price changed.
    Price,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Added => "ajout",
            Action::GradeBefore => "note avant",
            Action::GradeAfter => "note après",
            Action::Played => "joué",
            Action::Price => "prix",
        }
    }

    fn parse(s: &str) -> Option<Action> {
        [
            Action::Added,
            Action::GradeBefore,
            Action::GradeAfter,
            Action::Played,
            Action::Price,
        ]
        .into_iter()
        .find(|a| a.label() == s.trim())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub at: DateTime<Utc>,
    pub action: Action,
    pub key: Option<GameKey>,
    pub game: String,
    pub person: Option<String>,
    pub value: Option<f64>,
    pub previous: Option<f64>,
    pub details: String,
}

impl Event {
    pub fn new(action: Action, key: &GameKey, game: &str) -> Event {
        Event {
            at: Utc::now(),
            action,
            key: Some(key.clone()),
            game: game.to_string(),
            person: None,
            value: None,
            previous: None,
            details: String::new(),
        }
    }

    pub fn person(mut self, person: &str) -> Event {
        self.person = Some(person.to_string());
        self
    }

    pub fn value(mut self, value: f64, previous: Option<f64>) -> Event {
        self.value = Some(value);
        self.previous = previous;
        self
    }

    pub fn details(mut self, details: impl Into<String>) -> Event {
        self.details = details.into();
        self
    }

    /// Who played, for a Played event.
    pub fn players(&self) -> Vec<String> {
        self.details
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn to_row(&self) -> Vec<Value> {
        let num = |v: Option<f64>| v.map_or(json!(""), |v| json!(v));
        vec![
            json!(self.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            json!(self.action.label()),
            json!(self.key.as_ref().map(|k| k.to_string()).unwrap_or_default()),
            json!(self.game),
            json!(self.person.clone().unwrap_or_default()),
            num(self.value),
            num(self.previous),
            json!(self.details),
        ]
    }

    /// A journal row, or None for the header and for rows a human mangled.
    pub fn from_row(row: &[Value]) -> Option<Event> {
        let cell = |i: usize| row.get(i).map(cell_text).unwrap_or_default();
        let at = DateTime::parse_from_rfc3339(&cell(0))
            .ok()?
            .with_timezone(&Utc);
        let person = cell(4);
        Some(Event {
            at,
            action: Action::parse(&cell(1))?,
            key: GameKey::parse(&cell(2)),
            game: cell(3),
            person: (!person.is_empty()).then_some(person),
            value: row.get(5).and_then(cell_number),
            previous: row.get(6).and_then(cell_number),
            details: cell(7),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip() {
        let e = Event::new(Action::Played, &GameKey::Steam(7), "Seven")
            .details("Ana, Bob")
            .value(8.5, Some(6.0));
        let row = e.to_row();
        let back = Event::from_row(&row).unwrap();
        assert_eq!(back.action, Action::Played);
        assert_eq!(back.key, Some(GameKey::Steam(7)));
        assert_eq!(back.players(), ["Ana", "Bob"]);
        assert_eq!((back.value, back.previous), (Some(8.5), Some(6.0)));
        assert_eq!(back.at.timestamp(), e.at.timestamp());
        let header: Vec<Value> = HEADER.iter().map(|h| json!(h)).collect();
        assert!(Event::from_row(&header).is_none());
    }
}
