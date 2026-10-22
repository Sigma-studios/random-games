//! The spreadsheet's computed tables (Recap, Stats from Simon), rebuilt from the
//! raw grades so the site can filter them. Each function names the sheet range
//! it reproduces; averages are rounded to 2 decimals like the sheet's ROUND(…, 2).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::model::{Game, Person, Phase};

/// Rounds like the sheet's ROUND(v, 2), on the decimal value: 6.355, stored
/// as 6.35499…, gives 6.36.
pub fn round2(v: f64) -> f64 {
    ((v * 100.0 * 1e6).round() / 1e6).round() / 100.0
}

fn mean(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values
        .into_iter()
        .fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f64)
}

/// Who counts in averages (the Recap sheet's player checkboxes, or the site's).
#[derive(Debug, Clone)]
pub struct Included(pub BTreeSet<String>);

impl Included {
    pub fn from_people(people: &[Person]) -> Self {
        Included(
            people
                .iter()
                .filter(|p| p.included)
                .map(|p| p.name.clone())
                .collect(),
        )
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

/// Notes!K / Notes!S: the average grade of the included people, and how many
/// of them graded (Notes!L).
pub fn game_average(game: &Game, phase: Phase, inc: &Included) -> Option<(f64, usize)> {
    let grades = match phase {
        Phase::Before => &game.before,
        Phase::After => &game.after,
    };
    let values: Vec<f64> = grades
        .iter()
        .filter(|(who, _)| inc.contains(who))
        .map(|(_, v)| *v)
        .collect();
    mean(values.iter().copied()).map(|m| (round2(m), values.len()))
}

/// Notes!Z: how much the group's opinion moved once the game was played.
pub fn game_diff(game: &Game, inc: &Included) -> Option<f64> {
    if !game.played {
        return None;
    }
    let (before, _) = game_average(game, Phase::Before, inc)?;
    let (after, _) = game_average(game, Phase::After, inc)?;
    Some(round2(after - before))
}

/// Notes!T..Y: one person's after minus before, on a played game.
pub fn person_diff(game: &Game, person: &str) -> Option<f64> {
    if !game.played {
        return None;
    }
    Some(round2(game.after.get(person)? - game.before.get(person)?))
}

#[derive(Debug, Clone, Serialize)]
pub struct GameRow {
    pub slug: String,
    pub name: String,
    pub link: Option<String>,
    pub proposer: String,
    pub played: bool,
    pub price: Option<f64>,
    pub avg_before: Option<f64>,
    pub votes_before: usize,
    pub avg_after: Option<f64>,
    pub votes_after: usize,
    pub diff: Option<f64>,
}

pub fn game_row(game: &Game, inc: &Included) -> GameRow {
    let before = game_average(game, Phase::Before, inc);
    let after = game_average(game, Phase::After, inc);
    GameRow {
        slug: game.slug.clone(),
        name: game.name.clone(),
        link: game.link.clone(),
        proposer: game.proposer.clone(),
        played: game.played,
        price: game.price,
        avg_before: before.map(|b| b.0),
        votes_before: before.map_or(0, |b| b.1),
        avg_after: after.map(|a| a.0),
        votes_after: after.map_or(0, |a| a.1),
        diff: game_diff(game, inc),
    }
}

fn by_desc(rows: &mut [GameRow], key: impl Fn(&GameRow) -> Option<f64>) {
    rows.sort_by(|a, b| {
        key(b)
            .unwrap_or(f64::MIN)
            .total_cmp(&key(a).unwrap_or(f64::MIN))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Recap!J:N, "Not played games": unplayed games with a grade, best first.
pub fn to_play(games: &[Game], inc: &Included) -> Vec<GameRow> {
    let mut rows: Vec<GameRow> = games
        .iter()
        .filter(|g| !g.played)
        .map(|g| game_row(g, inc))
        .filter(|r| r.avg_before.is_some())
        .collect();
    by_desc(&mut rows, |r| r.avg_before);
    rows
}

/// Recap!P:R, "Jeux joués": played games by their after-playing grade.
pub fn played(games: &[Game], inc: &Included) -> Vec<GameRow> {
    let mut rows: Vec<GameRow> = games
        .iter()
        .filter(|g| g.played)
        .map(|g| game_row(g, inc))
        .filter(|r| r.avg_after.is_some())
        .collect();
    by_desc(&mut rows, |r| r.avg_after);
    rows
}

/// Recap!T:V, "Biggest comeback": played games whose grade rose the most
/// (the end of the list is the flops).
pub fn comebacks(games: &[Game], inc: &Included) -> Vec<GameRow> {
    let mut rows: Vec<GameRow> = games
        .iter()
        .map(|g| game_row(g, inc))
        .filter(|r| r.diff.is_some())
        .collect();
    by_desc(&mut rows, |r| r.diff);
    rows
}

/// Recap!A1:H7, one row per person.
#[derive(Debug, Clone, Serialize)]
pub struct PersonSummary {
    pub name: String,
    pub included: bool,
    /// B: average group grade (before) of the games they proposed.
    pub proposed_avg_before: Option<f64>,
    /// C: the same, after playing.
    pub proposed_avg_after: Option<f64>,
    /// D: the mean of B and C (before rounding). (The sheet's formula weighs them by row
    /// counts, but its "No Data" cells make both counts the row count, so what
    /// it shows is this mean.)
    pub proposed_avg: Option<f64>,
    /// E: games proposed.
    pub proposed: usize,
    pub proposed_played: usize,
    /// F: average grade they give before playing.
    pub given_avg_before: Option<f64>,
    /// G: average grade they give after playing.
    pub given_avg_after: Option<f64>,
    /// H: mean of F and G.
    pub given_avg: Option<f64>,
    pub given_before: usize,
    pub given_after: usize,
}

pub fn person_summaries(people: &[Person], games: &[Game], inc: &Included) -> Vec<PersonSummary> {
    people
        .iter()
        .map(|p| {
            let theirs: Vec<&Game> = games.iter().filter(|g| g.proposer == p.name).collect();
            let befores: Vec<f64> = theirs
                .iter()
                .filter_map(|g| game_average(g, Phase::Before, inc).map(|a| a.0))
                .collect();
            let afters: Vec<f64> = theirs
                .iter()
                .filter_map(|g| game_average(g, Phase::After, inc).map(|a| a.0))
                .collect();
            let b = mean(befores.iter().copied());
            let c = mean(afters.iter().copied());
            let given_b: Vec<f64> = games
                .iter()
                .filter_map(|g| g.before.get(&p.name))
                .copied()
                .collect();
            let given_a: Vec<f64> = games
                .iter()
                .filter_map(|g| g.after.get(&p.name))
                .copied()
                .collect();
            let given_avg_before = mean(given_b.iter().copied()).map(round2);
            let given_avg_after = mean(given_a.iter().copied()).map(round2);
            PersonSummary {
                name: p.name.clone(),
                included: inc.contains(&p.name),
                proposed_avg_before: b.map(round2),
                proposed_avg_after: c.map(round2),
                proposed_avg: mean(b.into_iter().chain(c)).map(round2),
                proposed: theirs.len(),
                proposed_played: theirs.iter().filter(|g| g.played).count(),
                given_avg_before,
                given_avg_after,
                given_avg: mean(given_avg_before.into_iter().chain(given_avg_after)).map(round2),
                given_before: given_b.len(),
                given_after: given_a.len(),
            }
        })
        .collect()
}

/// The games a phase's grades are read on in Stats from Simon: unplayed games
/// for grades before playing ("jeu proposé"), played ones after ("jeu joué").
fn in_phase(game: &Game, phase: Phase) -> bool {
    match phase {
        Phase::Before => !game.played,
        Phase::After => game.played,
    }
}

/// Stats from Simon!G5:M11 and G14:M20: the average grade each giver gave each
/// proposer's games, `matrix[giver][proposer]`.
pub fn grade_matrix(
    people: &[Person],
    games: &[Game],
    phase: Phase,
) -> BTreeMap<String, BTreeMap<String, Option<f64>>> {
    people
        .iter()
        .map(|giver| {
            let row = people
                .iter()
                .map(|proposer| {
                    let avg = mean(
                        games
                            .iter()
                            .filter(|g| g.proposer == proposer.name && in_phase(g, phase))
                            .filter_map(|g| g.grade(phase, &giver.name)),
                    )
                    .map(round2);
                    (proposer.name.clone(), avg)
                })
                .collect();
            (giver.name.clone(), row)
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Extreme {
    pub value: f64,
    pub games: Vec<GameRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameRef {
    pub slug: String,
    pub name: String,
    pub proposer: String,
}

fn game_ref(g: &Game) -> GameRef {
    GameRef {
        slug: g.slug.clone(),
        name: g.name.clone(),
        proposer: g.proposer.clone(),
    }
}

/// The games that reach the max (or min) of `value`, all of them on a tie.
fn extreme<'a>(
    games: impl Iterator<Item = &'a Game>,
    value: impl Fn(&Game) -> Option<f64>,
    max: bool,
) -> Option<Extreme> {
    let scored: Vec<(&Game, f64)> = games.filter_map(|g| value(g).map(|v| (g, v))).collect();
    let best = scored
        .iter()
        .map(|(_, v)| *v)
        .reduce(|a, b| if (b > a) == max { b } else { a })?;
    Some(Extreme {
        value: best,
        games: scored
            .iter()
            .filter(|(_, v)| (*v - best).abs() < 1e-9)
            .map(|(g, _)| game_ref(g))
            .collect(),
    })
}

/// One row of Stats from Simon!A5:E10, for player X and another person Y.
#[derive(Debug, Clone, Serialize)]
pub struct Exchange {
    pub other: String,
    /// B: Y's average grade on X's unplayed proposals, before playing.
    pub received_before: Option<f64>,
    /// C: Y's average grade on X's played proposals, after playing.
    pub received_after: Option<f64>,
    /// D: X's average grade on Y's unplayed proposals, before playing.
    pub given_before: Option<f64>,
    /// E: the same, after.
    pub given_after: Option<f64>,
}

/// Stats from Simon, the per-player page.
#[derive(Debug, Clone, Serialize)]
pub struct PlayerStats {
    pub name: String,
    pub exchanges: Vec<Exchange>,
    /// O, Q: highest grade X gave an unplayed / a played game, and which games.
    pub highest_before: Option<Extreme>,
    pub highest_after: Option<Extreme>,
    /// S, U: lowest.
    pub lowest_before: Option<Extreme>,
    pub lowest_after: Option<Extreme>,
    /// W: the played game X's grade rose most on; Y: fell most on.
    pub best_comeback: Option<Extreme>,
    pub worst_flop: Option<Extreme>,
}

pub fn player_stats(name: &str, people: &[Person], games: &[Game]) -> PlayerStats {
    let avg_on = |proposer: &str, giver: &str, phase: Phase| {
        mean(
            games
                .iter()
                .filter(|g| g.proposer == proposer && in_phase(g, phase))
                .filter_map(|g| g.grade(phase, giver)),
        )
        .map(round2)
    };
    let exchanges = people
        .iter()
        .map(|y| Exchange {
            other: y.name.clone(),
            received_before: avg_on(name, &y.name, Phase::Before),
            received_after: avg_on(name, &y.name, Phase::After),
            given_before: avg_on(&y.name, name, Phase::Before),
            given_after: avg_on(&y.name, name, Phase::After),
        })
        .collect();
    let unplayed = || games.iter().filter(|g| !g.played);
    let played = || games.iter().filter(|g| g.played);
    PlayerStats {
        name: name.to_string(),
        exchanges,
        highest_before: extreme(unplayed(), |g| g.grade(Phase::Before, name), true),
        highest_after: extreme(played(), |g| g.grade(Phase::After, name), true),
        lowest_before: extreme(unplayed(), |g| g.grade(Phase::Before, name), false),
        lowest_after: extreme(played(), |g| g.grade(Phase::After, name), false),
        best_comeback: extreme(played(), |g| person_diff(g, name), true),
        worst_flop: extreme(played(), |g| person_diff(g, name), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GameKey;

    fn person(name: &str, included: bool) -> Person {
        Person {
            name: name.into(),
            col_before: 0,
            col_after: None,
            included,
        }
    }

    fn game(
        name: &str,
        proposer: &str,
        played: bool,
        before: &[(&str, f64)],
        after: &[(&str, f64)],
    ) -> Game {
        Game {
            key: GameKey::Name(name.into()),
            slug: name.into(),
            name: name.into(),
            link: None,
            proposer: proposer.into(),
            played,
            price: None,
            before: before.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            after: after.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            row: 0,
        }
    }

    fn fixture() -> (Vec<Person>, Vec<Game>) {
        let people = vec![
            person("Ana", true),
            person("Bob", true),
            person("Cyd", false),
        ];
        let games = vec![
            game(
                "Alpha",
                "Ana",
                false,
                &[("Ana", 8.0), ("Bob", 6.0), ("Cyd", 1.0)],
                &[],
            ),
            game(
                "Beta",
                "Bob",
                true,
                &[("Ana", 4.0), ("Bob", 6.0)],
                &[("Ana", 9.0), ("Bob", 7.0)],
            ),
            game(
                "Gamma",
                "Ana",
                true,
                &[("Ana", 7.0), ("Bob", 7.0)],
                &[("Ana", 3.0)],
            ),
            game("Delta", "Bob", false, &[("Cyd", 10.0)], &[]),
        ];
        (people, games)
    }

    #[test]
    fn rounding_matches_the_sheet() {
        assert_eq!(round2(6.3549999999999995), 6.36);
        assert_eq!(round2(7.365591397849462), 7.37);
        assert_eq!(round2(-1.005), -1.01);
        assert_eq!(round2(2.0), 2.0);
    }

    #[test]
    fn averages_follow_the_player_checkboxes() {
        let (people, games) = fixture();
        let inc = Included::from_people(&people);
        // Cyd is unchecked: Alpha's 1.0 from Cyd does not count.
        assert_eq!(game_average(&games[0], Phase::Before, &inc), Some((7.0, 2)));
        // Delta only has Cyd's grade, so no average at all (the sheet's "No Data").
        assert_eq!(game_average(&games[3], Phase::Before, &inc), None);
        assert_eq!(game_diff(&games[1], &inc), Some(3.0));
        assert_eq!(game_diff(&games[0], &inc), None);
        assert_eq!(person_diff(&games[2], "Ana"), Some(-4.0));
    }

    #[test]
    fn recap_lists() {
        let (people, games) = fixture();
        let inc = Included::from_people(&people);
        let names = |rows: Vec<GameRow>| rows.into_iter().map(|r| r.name).collect::<Vec<_>>();
        assert_eq!(names(to_play(&games, &inc)), ["Alpha"]);
        assert_eq!(names(played(&games, &inc)), ["Beta", "Gamma"]);
        // Beta: 5 → 8 (+3); Gamma: 7 → 3 (-4).
        assert_eq!(names(comebacks(&games, &inc)), ["Beta", "Gamma"]);
    }

    #[test]
    fn recap_people() {
        let (people, games) = fixture();
        let inc = Included::from_people(&people);
        let s = person_summaries(&people, &games, &inc);
        let ana = &s[0];
        // Ana proposed Alpha (7.0 before) and Gamma (7.0 before, 3.0 after).
        assert_eq!(ana.proposed, 2);
        assert_eq!(ana.proposed_avg_before, Some(7.0));
        assert_eq!(ana.proposed_avg_after, Some(3.0));
        assert_eq!(ana.proposed_avg, Some(5.0));
        // Ana gave 8, 4, 7 before and 9, 3 after.
        assert_eq!(ana.given_avg_before, Some(6.33));
        assert_eq!(ana.given_avg_after, Some(6.0));
        assert_eq!(ana.given_avg, Some(6.17));
        // Given averages ignore the checkboxes, like Recap!F:G.
        assert_eq!(s[2].given_avg_before, Some(5.5));
    }

    #[test]
    fn player_page() {
        let (people, games) = fixture();
        let m = grade_matrix(&people, &games, Phase::Before);
        // Ana on Bob's unplayed games: Delta has no grade from her, Beta is played.
        assert_eq!(m["Ana"]["Bob"], None);
        assert_eq!(m["Cyd"]["Bob"], Some(10.0));
        assert_eq!(
            grade_matrix(&people, &games, Phase::After)["Ana"]["Bob"],
            Some(9.0)
        );
        assert_eq!(m["Bob"]["Cyd"], None);

        let ana = player_stats("Ana", &people, &games);
        let bob = ana.exchanges.iter().find(|e| e.other == "Bob").unwrap();
        // Bob on Ana's unplayed proposals (Alpha 6; Gamma is played).
        assert_eq!(bob.received_before, Some(6.0));
        assert_eq!(bob.given_after, Some(9.0)); // Ana on Beta after
        let highest = ana.highest_before.unwrap();
        assert_eq!(
            (highest.value, highest.games[0].name.as_str()),
            (8.0, "Alpha")
        );
        assert_eq!(ana.best_comeback.unwrap().games[0].name, "Beta");
        assert_eq!(ana.worst_flop.unwrap().value, -4.0);
    }
}
