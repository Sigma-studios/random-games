//! The vote request DM: what it says, and reading it back when someone replies.
//! The text keeps the old bot's tagged format, so replies to reminders it sent
//! still count.

use crate::model::{GameKey, Phase};
use crate::sheet::Snapshot;

pub const TAG: &str = "[VOTE_REQUEST]";

pub fn request(display: &str, key: &GameKey, phase: Phase, min: f64, max: f64) -> String {
    let intro = match phase {
        Phase::Before => String::new(),
        Phase::After => "Tu y as joué : ta note après avoir joué ?\n".to_string(),
    };
    format!(
        "{TAG}\n{intro}Game: {display}\nId: {key}\nPhase: {}\nRéponds entre {} et {}.",
        phase.label(),
        fmt_grade(min),
        fmt_grade(max)
    )
}

/// The game and phase a request is about. Requests from the old bot carry
/// "Game: <link>" (and a row number, which sorting the sheet makes stale), so
/// the link is what identifies their game.
pub fn parse(content: &str, snapshot: &Snapshot) -> Option<(GameKey, Phase)> {
    if !content.contains(TAG) {
        return None;
    }
    let field = |name: &str| {
        content
            .lines()
            .find_map(|l| l.strip_prefix(name).map(|v| v.trim().to_string()))
    };
    let phase = match field("Phase:").as_deref() {
        Some("après" | "apres" | "after") => Phase::After,
        _ => Phase::Before,
    };
    if let Some(key) = field("Id:").as_deref().and_then(GameKey::parse) {
        return Some((key, phase));
    }
    let game = field("Game:")?;
    if let Some(key) = GameKey::from_link(&game) {
        return Some((key, phase));
    }
    // A name (prefer_link_in_reminders off, or a game without a link).
    snapshot
        .games
        .iter()
        .find(|g| g.name.eq_ignore_ascii_case(&game))
        .map(|g| (g.key.clone(), phase))
}

/// 7 → "7", 7.5 → "7.5".
pub fn fmt_grade(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn snapshot() -> Snapshot {
        Snapshot {
            people: vec![],
            games: vec![],
            events: vec![],
            fetched_at: Utc::now(),
        }
    }

    #[test]
    fn round_trip() {
        let key = GameKey::Steam(42);
        let text = request(
            "https://store.steampowered.com/app/42/X/",
            &key,
            Phase::After,
            0.0,
            10.0,
        );
        assert!(text.ends_with("Réponds entre 0 et 10."));
        assert_eq!(parse(&text, &snapshot()), Some((key, Phase::After)));
    }

    #[test]
    fn old_requests_still_count() {
        let old = "[VOTE_REQUEST]\nGame: https://store.steampowered.com/app/3989960/Frog_Sqwad/\nRow: 4\nRéponds entre 0.0 et 10.0.";
        assert_eq!(
            parse(old, &snapshot()),
            Some((GameKey::Steam(3989960), Phase::Before))
        );
        assert_eq!(parse("hello", &snapshot()), None);
    }

    #[test]
    fn grades_print_short() {
        assert_eq!(fmt_grade(7.0), "7");
        assert_eq!(fmt_grade(7.5), "7.5");
        assert_eq!(fmt_grade(6.25), "6.25");
    }
}
