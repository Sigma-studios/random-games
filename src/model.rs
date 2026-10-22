//! What the spreadsheet holds, independent of how it is fetched: games, the
//! people who grade them, and the parsing rules for what humans type into cells.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

static STEAM_APP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https?://store\.steampowered\.com/app/(\d+)(?:[/?#]|$)").unwrap()
});
static HYPERLINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^=(?:HYPERLINK|LIEN_HYPERTEXTE)\("([^"]+)"\s*[;,]\s*"([^"]*)"\)$"#).unwrap()
});
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://\S+").unwrap());

/// What identifies a game across sheet edits. A Steam app id never changes (the
/// slug after it can), so Steam links are keyed by it; other links by their
/// normalized URL, and link-less rows by their name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GameKey {
    Steam(u64),
    Url(String),
    Name(String),
}

impl GameKey {
    pub fn from_link(link: &str) -> Option<GameKey> {
        let link = link.trim();
        if let Some(id) = steam_app_id(link) {
            return Some(GameKey::Steam(id));
        }
        link.starts_with("http")
            .then(|| GameKey::Url(normalize_url(link)))
    }

    pub fn parse(s: &str) -> Option<GameKey> {
        let (kind, rest) = s.trim().split_once(':')?;
        match kind {
            "steam" => rest.parse().ok().map(GameKey::Steam),
            "url" => Some(GameKey::Url(rest.to_string())),
            "name" => Some(GameKey::Name(rest.to_string())),
            _ => None,
        }
    }

    /// A short form safe in URLs (the site's game pages).
    pub fn slug(&self) -> String {
        match self {
            GameKey::Steam(id) => format!("steam-{id}"),
            other => {
                let text = other.to_string();
                let mut hash: u64 = 0xcbf29ce484222325;
                for b in text.bytes() {
                    hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
                }
                format!("g-{hash:016x}")
            }
        }
    }
}

impl fmt::Display for GameKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GameKey::Steam(id) => write!(f, "steam:{id}"),
            GameKey::Url(url) => write!(f, "url:{url}"),
            GameKey::Name(name) => write!(f, "name:{name}"),
        }
    }
}

impl Serialize for GameKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub fn steam_app_id(link: &str) -> Option<u64> {
    STEAM_APP
        .captures(link.trim())
        .and_then(|c| c[1].parse().ok())
}

/// Lower-cased scheme and host, no query, fragment, `www.` or trailing slash:
/// the same page pasted twice compares equal.
pub fn normalize_url(link: &str) -> String {
    let link = link.trim();
    let link = link.split(['?', '#']).next().unwrap_or(link);
    let (scheme, rest) = link.split_once("://").unwrap_or(("https", link));
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let scheme = if scheme.eq_ignore_ascii_case("http") {
        "https"
    } else {
        scheme
    };
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}/{path}")
    }
}

/// `=HYPERLINK("url","label")` → (url, label); a bare URL → (url, None); text → (None, text).
pub fn parse_game_cell(cell: &str) -> (Option<String>, Option<String>) {
    let value = cell.trim();
    if value.is_empty() {
        return (None, None);
    }
    if let Some(c) = HYPERLINK.captures(value) {
        return (Some(c[1].to_string()), Some(c[2].to_string()));
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return (Some(value.to_string()), None);
    }
    (None, Some(value.to_string()))
}

/// The cell formula for a new game row.
pub fn hyperlink_formula(link: &str, name: &str) -> String {
    format!(
        "=HYPERLINK(\"{}\",\"{}\")",
        link.replace('"', "%22"),
        name.replace('"', "'")
    )
}

/// Links in a Discord message and the text around them (the game's name when
/// the link alone does not say it).
pub fn extract_links(content: &str) -> (Vec<String>, String) {
    let mut title = content.to_string();
    let mut links = Vec::new();
    for m in URL.find_iter(content) {
        title = title.replace(m.as_str(), "");
        let link = m
            .as_str()
            .trim_matches(|c| "<>).,\"' ".contains(c))
            .to_string();
        links.push(link);
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = title.trim_matches(|c| "-–—|: ".contains(c)).to_string();
    (links, title)
}

/// The name Steam puts in its URL: `/app/123/Frog_Sqwad/` → "Frog Sqwad".
pub fn steam_slug_name(link: &str) -> Option<String> {
    let path = link.split(['?', '#']).next()?.trim_end_matches('/');
    let mut parts = path.rsplit('/');
    let slug = parts.next()?;
    if slug.is_empty() || slug.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let decoded = percent_encoding::percent_decode_str(slug)
        .decode_utf8()
        .ok()?;
    Some(decoded.replace('_', " ").trim().to_string()).filter(|s| !s.is_empty())
}

/// A grade as typed in a DM: "7", "7.5" or "7,5".
pub fn parse_grade(text: &str) -> Option<f64> {
    let cleaned = text.trim().replace(',', ".");
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A sheet cell read with UNFORMATTED_VALUE: a number, or text such as "7,5".
pub fn cell_number(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => {
            let s = s.trim().replace('€', "").replace("EUR", "");
            parse_grade(&s)
        }
        _ => None,
    }
}

pub fn cell_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// "Oui" / "Non" in the already-played column.
pub fn is_yes(text: &str) -> bool {
    matches!(
        text.trim().to_lowercase().as_str(),
        "oui" | "yes" | "y" | "o" | "true"
    )
}

pub fn is_no(text: &str) -> bool {
    matches!(
        text.trim().to_lowercase().as_str(),
        "non" | "no" | "n" | "false"
    )
}

/// Before or after playing: the two grades a person gives a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Before,
    After,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Before => "avant",
            Phase::After => "après",
        }
    }
}

/// One person who grades games, with their columns on the Notes sheet
/// (0-based indexes into a row of the data range).
#[derive(Debug, Clone, Serialize)]
pub struct Person {
    pub name: String,
    #[serde(skip)]
    pub col_before: usize,
    #[serde(skip)]
    pub col_after: Option<usize>,
    /// Counted in averages (the Recap sheet's player checkboxes).
    pub included: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Game {
    pub key: GameKey,
    pub slug: String,
    pub name: String,
    pub link: Option<String>,
    pub proposer: String,
    pub played: bool,
    pub price: Option<f64>,
    pub before: BTreeMap<String, f64>,
    pub after: BTreeMap<String, f64>,
    /// 1-based sheet row, valid for the snapshot it was read in.
    #[serde(skip)]
    pub row: u32,
}

impl Game {
    pub fn grade(&self, phase: Phase, person: &str) -> Option<f64> {
        match phase {
            Phase::Before => self.before.get(person).copied(),
            Phase::After => self.after.get(person).copied(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(
            GameKey::from_link("https://store.steampowered.com/app/3989960/Frog_Sqwad/"),
            Some(GameKey::Steam(3989960))
        );
        assert_eq!(
            GameKey::from_link(
                "https://store.steampowered.com/app/2021910/FPS_Chess/?curator_clanid=1"
            ),
            Some(GameKey::Steam(2021910))
        );
        assert_eq!(
            GameKey::from_link("http://store.steampowered.com/app/218230/PlanetSide_2/"),
            Some(GameKey::Steam(218230))
        );
        assert_eq!(
            GameKey::from_link("https://www.GeoGuessr.com/"),
            Some(GameKey::Url("https://geoguessr.com".into()))
        );
        assert_eq!(
            GameKey::from_link("http://skribbl.io/"),
            GameKey::from_link("https://skribbl.io")
        );
        let key = GameKey::Steam(42);
        assert_eq!(GameKey::parse(&key.to_string()), Some(key));
        let key = GameKey::Url("https://tetr.io".into());
        assert_eq!(GameKey::parse(&key.to_string()), Some(key));
    }

    #[test]
    fn cells() {
        assert_eq!(
            parse_game_cell(r#"=HYPERLINK("https://store.steampowered.com/app/1/X/","X Game")"#),
            (
                Some("https://store.steampowered.com/app/1/X/".into()),
                Some("X Game".into())
            )
        );
        assert_eq!(
            parse_game_cell(r#"=LIEN_HYPERTEXTE("https://a.b/"; "A")"#),
            (Some("https://a.b/".into()), Some("A".into()))
        );
        assert_eq!(
            parse_game_cell("Minecraft"),
            (None, Some("Minecraft".into()))
        );
        assert_eq!(parse_game_cell(" "), (None, None));
        assert_eq!(
            hyperlink_formula("https://a/b", r#"Say "hi""#),
            r#"=HYPERLINK("https://a/b","Say 'hi'")"#
        );
    }

    #[test]
    fn messages() {
        let (links, title) =
            extract_links("Super jeu https://store.steampowered.com/app/1/Some_Game/ !");
        assert_eq!(
            links,
            vec!["https://store.steampowered.com/app/1/Some_Game/"]
        );
        assert_eq!(title, "Super jeu !");
        let (links, _) = extract_links("<https://a.io/x>, et (https://b.io/y).");
        assert_eq!(links, vec!["https://a.io/x", "https://b.io/y"]);
        assert_eq!(
            steam_slug_name("https://store.steampowered.com/app/1/Trine_5_A_Clockwork_Conspiracy/"),
            Some("Trine 5 A Clockwork Conspiracy".into())
        );
        assert_eq!(
            steam_slug_name("https://store.steampowered.com/app/1/"),
            None
        );
    }

    #[test]
    fn grades() {
        assert_eq!(parse_grade("7"), Some(7.0));
        assert_eq!(parse_grade(" 7,5 "), Some(7.5));
        assert_eq!(parse_grade("sept"), None);
        assert_eq!(parse_grade("NaN"), None);
        assert_eq!(cell_number(&serde_json::json!("9,99 €")), Some(9.99));
        assert!(is_yes("Oui") && is_no(" non") && !is_yes(""));
    }
}
