//! Steam store lookups: a game's name and price (for new rows and the daily
//! price update) and its cover image (for the site).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::SteamStore;

#[derive(Debug, Clone)]
pub struct AppInfo {
    pub name: Option<String>,
    /// In the store's currency; 0 for free games.
    pub price: Option<f64>,
    pub header_image: Option<String>,
}

pub struct Steam {
    http: reqwest::Client,
    cfg: SteamStore,
}

impl Steam {
    pub fn new(cfg: SteamStore) -> Steam {
        Steam {
            http: reqwest::Client::builder()
                .user_agent("random-games-bot/1.0")
                .timeout(Duration::from_secs_f64(
                    cfg.request_timeout_seconds.max(1.0),
                ))
                .build()
                .expect("http client"),
            cfg,
        }
    }

    pub async fn app(&self, id: u64) -> anyhow::Result<Option<AppInfo>> {
        let payload: Value = self
            .http
            .get("https://store.steampowered.com/api/appdetails")
            .query(&[
                ("appids", id.to_string()),
                ("cc", self.cfg.country_code.clone()),
                ("l", self.cfg.language.clone()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_app(&payload, id))
    }

    /// Several apps within the configured time budget, pausing between
    /// requests (Steam allows about 200 per 5 minutes). Apps past the budget,
    /// or that fail, are left out: they are tried again next time.
    pub async fn apps(&self, ids: &[u64]) -> HashMap<u64, AppInfo> {
        let deadline = Instant::now() + Duration::from_secs_f64(self.cfg.budget_seconds);
        let mut out = HashMap::new();
        for (i, id) in ids.iter().enumerate() {
            if Instant::now() >= deadline {
                tracing::warn!(
                    "Steam budget of {}s spent; {} app(s) left for next time",
                    self.cfg.budget_seconds,
                    ids.len() - i
                );
                break;
            }
            if i > 0 {
                tokio::time::sleep(Duration::from_secs_f64(self.cfg.request_delay_seconds)).await;
            }
            match self.app(*id).await {
                Ok(Some(info)) => {
                    out.insert(*id, info);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("Steam app {id}: {e}"),
            }
        }
        out
    }
}

pub fn parse_app(payload: &Value, id: u64) -> Option<AppInfo> {
    let entry = &payload[id.to_string()];
    if entry["success"] != Value::Bool(true) {
        return None;
    }
    let data = &entry["data"];
    let price = match data["price_overview"]["final"].as_i64() {
        Some(cents) => Some((cents.max(0) as f64) / 100.0),
        None if data["is_free"] == Value::Bool(true) => Some(0.0),
        None => None,
    };
    Some(AppInfo {
        name: data["name"].as_str().map(str::to_string),
        price,
        header_image: data["header_image"].as_str().map(str::to_string),
    })
}

/// The usual address of an app's cover. Newer apps have theirs under a hashed
/// path instead, which only the store API knows (AppInfo::header_image).
pub fn header_image(id: u64) -> String {
    format!("https://shared.steamstatic.com/store_item_assets/steam/apps/{id}/header.jpg")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prices() {
        let paid = json!({"7": {"success": true, "data": {"name": "Seven", "price_overview": {"final": 1999}}}});
        let info = parse_app(&paid, 7).unwrap();
        assert_eq!(
            (info.name.as_deref(), info.price),
            (Some("Seven"), Some(19.99))
        );
        let free = json!({"8": {"success": true, "data": {"name": "Eight", "is_free": true}}});
        assert_eq!(parse_app(&free, 8).unwrap().price, Some(0.0));
        let unreleased = json!({"9": {"success": true, "data": {"name": "Nine"}}});
        assert_eq!(parse_app(&unreleased, 9).unwrap().price, None);
        assert!(parse_app(&json!({"9": {"success": false}}), 9).is_none());
    }
}
