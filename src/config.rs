//! Settings, all from the environment: `BOT_CONFIG` holds the old bot's
//! config.json minus its secrets (same keys, so it can be pasted as is), and the
//! secrets come in their own variables.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use chrono::NaiveTime;
use serde::{Deserialize, Deserializer};

pub struct Config {
    pub discord_token: String,
    /// The Google service account key, as JSON.
    pub google_service_account: String,
    pub settings: Settings,
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Config> {
        let var = |name: &str| std::env::var(name).with_context(|| format!("{name} is not set"));
        let settings: Settings = serde_json::from_str(&var("BOT_CONFIG")?)
            .context("BOT_CONFIG is not valid (it takes the old config.json's JSON)")?;
        settings.validate()?;
        Ok(Config {
            discord_token: var("DISCORD_TOKEN")?.trim().to_string(),
            google_service_account: var("GOOGLE_SERVICE_ACCOUNT")?,
            settings,
            data_dir: std::env::var("DATA_DIR")
                .unwrap_or_else(|_| "data".into())
                .into(),
            listen: std::env::var("LISTEN")
                .unwrap_or_else(|_| "0.0.0.0:8080".into())
                .parse()
                .context("LISTEN must be an address such as 0.0.0.0:8080")?,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    #[serde(deserialize_with = "id")]
    pub thread_id: u64,
    pub spreadsheet_id: String,
    #[serde(default = "notes")]
    pub notes_sheet: String,
    #[serde(default = "data_range")]
    pub data_range: String,
    #[serde(default = "header_row")]
    pub header_row: u32,
    #[serde(default)]
    pub columns: Columns,
    /// Person name (as on the sheet) → Discord user id.
    #[serde(deserialize_with = "ids")]
    pub vote_users: BTreeMap<String, u64>,
    #[serde(default)]
    pub run_actions: RunActions,
    #[serde(default)]
    pub vote_min: f64,
    #[serde(default = "ten")]
    pub vote_max: f64,
    #[serde(default)]
    pub reminder_cooldown_hours: f64,
    #[serde(default)]
    pub max_reminders_per_person: ReminderLimits,
    #[serde(default = "yes")]
    pub prefer_link_in_reminders: bool,
    #[serde(default = "helper")]
    pub helper_sheet: String,
    /// Discord display name → person name, for authors not in vote_users.
    #[serde(default)]
    pub discord_name_map: BTreeMap<String, String>,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub steam_store: SteamStore,
    #[serde(default)]
    pub link_ingest: LinkIngest,
    #[serde(default = "table")]
    pub data_table_name: String,
    /// Where the Recap sheet keeps who counts in averages (name, checkbox).
    #[serde(default = "included_range")]
    pub included_range: String,
    /// The sheet the bot dates its actions in (created if missing).
    #[serde(default = "journal")]
    pub journal_sheet: String,
    /// When reminders (and the price update) go out, in `timezone`.
    #[serde(default = "daily_time")]
    pub daily_time: NaiveTime,
    #[serde(default = "timezone")]
    pub timezone: chrono_tz::Tz,
}

impl Settings {
    fn validate(&self) -> anyhow::Result<()> {
        if self.vote_users.is_empty() {
            bail!("vote_users is empty");
        }
        if self.vote_min.partial_cmp(&self.vote_max) != Some(std::cmp::Ordering::Less) {
            bail!("vote_min must be below vote_max");
        }
        Ok(())
    }

    /// The person a Discord user is, from vote_users.
    pub fn person_for(&self, user_id: u64) -> Option<&str> {
        self.vote_users
            .iter()
            .find(|(_, id)| **id == user_id)
            .map(|(name, _)| name.as_str())
    }

    /// The person behind a Discord author: their id first (names change), then
    /// discord_name_map over the names Discord shows, then the name itself.
    pub fn resolve_person(&self, user_id: u64, names: &[&str]) -> String {
        if let Some(name) = self.person_for(user_id) {
            return name.to_string();
        }
        for name in names.iter().filter(|n| !n.is_empty()) {
            if let Some(mapped) = self.discord_name_map.get(*name).or_else(|| {
                self.discord_name_map
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v)
            }) {
                return mapped.clone();
            }
        }
        names
            .iter()
            .find(|n| !n.is_empty())
            .unwrap_or(&"")
            .to_string()
    }

    pub fn reminder_limit(&self, person: &str) -> Option<u32> {
        let limit = match &self.max_reminders_per_person {
            ReminderLimits::All(n) => Some(*n),
            ReminderLimits::Each(map) => map.get(person).or_else(|| map.get("default")).copied(),
        };
        limit.filter(|n| *n > 0)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Columns {
    #[serde(default = "col_game")]
    pub game: String,
    #[serde(default = "col_proposed")]
    pub proposed_by: String,
    #[serde(default = "col_played")]
    pub already_played: String,
    #[serde(default = "col_price")]
    pub price: String,
}

impl Default for Columns {
    fn default() -> Self {
        Columns {
            game: col_game(),
            proposed_by: col_proposed(),
            already_played: col_played(),
            price: col_price(),
        }
    }
}

/// Which jobs run. The old bot did them once a day; here thread links and DM
/// replies are handled as they arrive.
#[derive(Debug, Clone, Deserialize)]
pub struct RunActions {
    #[serde(default = "yes")]
    pub read_thread: bool,
    #[serde(default = "yes")]
    pub read_responses: bool,
    #[serde(default = "yes")]
    pub send_reminders: bool,
    #[serde(default)]
    pub update_prices: bool,
}

impl Default for RunActions {
    fn default() -> Self {
        RunActions {
            read_thread: true,
            read_responses: true,
            send_reminders: true,
            update_prices: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ReminderLimits {
    All(u32),
    Each(BTreeMap<String, u32>),
}

impl Default for ReminderLimits {
    fn default() -> Self {
        ReminderLimits::All(0)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Defaults {
    pub already_played: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SteamStore {
    #[serde(default = "fr")]
    pub country_code: String,
    #[serde(default = "french")]
    pub language: String,
    #[serde(default = "steam_timeout")]
    pub request_timeout_seconds: f64,
    #[serde(default = "steam_budget")]
    pub budget_seconds: f64,
    #[serde(default = "steam_delay")]
    pub request_delay_seconds: f64,
}

impl Default for SteamStore {
    fn default() -> Self {
        serde_json::from_str("{}").unwrap()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LinkIngest {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "yes")]
    pub use_hyperlink_formula: bool,
    #[serde(default)]
    pub allowed_prefixes: Vec<String>,
    #[serde(default)]
    pub auto_fill_formulas: AutoFill,
}

impl Default for LinkIngest {
    fn default() -> Self {
        serde_json::from_str("{}").unwrap()
    }
}

/// Columns whose formulas a new row copies from the row above (when it is
/// written by row insertion rather than appended to the sheet's table).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AutoFill {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub ranges: Vec<String>,
}

/// Ids are strings in the old config.json; numbers are accepted too.
fn id<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        N(u64),
        S(String),
    }
    match Id::deserialize(d)? {
        Id::N(n) => Ok(n),
        Id::S(s) => s.trim().parse().map_err(serde::de::Error::custom),
    }
}

fn ids<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, u64>, D::Error> {
    #[derive(Deserialize)]
    struct W(#[serde(deserialize_with = "id")] u64);
    let map = BTreeMap::<String, W>::deserialize(d)?;
    Ok(map.into_iter().map(|(k, W(v))| (k, v)).collect())
}

fn notes() -> String {
    "Notes".into()
}
fn data_range() -> String {
    "A3:Z".into()
}
fn header_row() -> u32 {
    3
}
fn ten() -> f64 {
    10.0
}
fn yes() -> bool {
    true
}
fn helper() -> String {
    "Helper".into()
}
fn table() -> String {
    "Data".into()
}
fn included_range() -> String {
    "Recap!A9:B14".into()
}
fn journal() -> String {
    "Journal".into()
}
fn daily_time() -> NaiveTime {
    NaiveTime::from_hms_opt(18, 0, 0).unwrap()
}
fn timezone() -> chrono_tz::Tz {
    chrono_tz::Europe::Paris
}
fn col_game() -> String {
    "Nom du jeu".into()
}
fn col_proposed() -> String {
    "Jeu propose par".into()
}
fn col_played() -> String {
    "Deja joue?".into()
}
fn col_price() -> String {
    "Prix".into()
}
fn fr() -> String {
    "fr".into()
}
fn french() -> String {
    "french".into()
}
fn steam_timeout() -> f64 {
    10.0
}
fn steam_budget() -> f64 {
    240.0
}
fn steam_delay() -> f64 {
    0.2
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of the old bot's config.example.json, secrets and all: the
    /// keys this program does not use are ignored.
    #[test]
    fn reads_the_old_config() {
        let s: Settings = serde_json::from_str(
            r#"{
              "discord_token": "unused here", "google_credentials_file": "service_account.json",
              "thread_id": "123", "spreadsheet_id": "sheet", "notes_sheet": "Notes",
              "data_range": "A3:Z", "header_row": 3,
              "columns": {"game": "Nom du jeu", "proposed_by": "Jeu propose par",
                          "already_played": "Deja joue?", "price": "Prix"},
              "vote_users": {"Alice": "11", "Bob": 22},
              "run_actions": {"read_thread": true, "update_prices": true},
              "vote_min": 0, "vote_max": 10, "reminder_cooldown_hours": 12,
              "max_reminders_per_person": {"default": 5, "Alice": 3},
              "discord_name_map": {"alice_example": "Alice"},
              "defaults": {"already_played": "Non"},
              "steam_store": {"country_code": "fr"},
              "event_log": {"enabled": true, "path": "bot_events.db"},
              "link_ingest": {"enabled": true, "allowed_prefixes": ["https://store.steampowered.com/app"],
                              "auto_fill_formulas": {"enabled": true, "ranges": ["K:K", "S:Z"]}}
            }"#,
        )
        .unwrap();
        s.validate().unwrap();
        assert_eq!(s.thread_id, 123);
        assert_eq!(s.vote_users["Bob"], 22);
        assert!(s.run_actions.update_prices && s.run_actions.send_reminders);
        assert_eq!(s.reminder_limit("Alice"), Some(3));
        assert_eq!(s.reminder_limit("Bob"), Some(5));
        assert_eq!(s.resolve_person(11, &["whoever"]), "Alice");
        assert_eq!(s.resolve_person(99, &["", "ALICE_example"]), "Alice");
        assert_eq!(s.resolve_person(99, &["Zed"]), "Zed");
        assert_eq!(s.daily_time, NaiveTime::from_hms_opt(18, 0, 0).unwrap());
        assert_eq!(s.steam_store.budget_seconds, 240.0);
    }
}
