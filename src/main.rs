//! Random games: the Discord bot of La Session's game votes, and the public
//! site of its statistics. The Google Sheet stays the storage.

mod app;
mod bot;
mod config;
mod google;
mod journal;
mod model;
mod schedule;
mod sheet;
mod state;
mod stats;
mod steam;
mod votes;
mod web;

use std::sync::Arc;

use anyhow::Context;

use crate::app::App;
use crate::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,serenity=warn,tracing::span=warn".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [cmd, file] = args.as_slice()
        && cmd == "preview"
    {
        return preview(file).await;
    }
    if !args.is_empty() {
        anyhow::bail!(
            "usage: random-games            (bot and site, configured by the environment)\n       random-games preview <ranges.json>   (the site only, over saved sheet ranges)"
        );
    }
    let config = Config::from_env()?;
    let settings = Arc::new(config.settings);
    let sheets = google::Sheets::new(&config.google_service_account, &settings.spreadsheet_id)?;
    tracing::info!("reading the sheet as {}", sheets.service_account_email());
    let store = sheet::Store::open(sheets, settings.clone())
        .await
        .context("cannot open the spreadsheet")?;
    let snapshot = store.current().await;
    tracing::info!(
        "{} games, {} people on the sheet",
        snapshot.games.len(),
        snapshot.people.len()
    );
    let app = Arc::new(App {
        steam: steam::Steam::new(settings.steam_store.clone()),
        state: state::StateFile::open(&config.data_dir)?,
        store,
        settings,
        avatars: Default::default(),
    });
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("cannot listen on {}", config.listen))?;
    tracing::info!("site on http://{}", config.listen);
    let site = tokio::spawn(web::serve(app.clone(), listener));
    tokio::select! {
        r = bot::run(app, &config.discord_token) => r.context("Discord"),
        r = site => r?.context("site"),
    }
}

/// The site alone, read-only, over sheet ranges saved in a file: for working
/// on the site without Discord or Google.
async fn preview(file: &str) -> anyhow::Result<()> {
    let ranges: sheet::OfflineRanges = serde_json::from_slice(&std::fs::read(file)?)
        .with_context(|| format!("{file} is not a ranges file"))?;
    let settings: config::Settings = match std::env::var("BOT_CONFIG") {
        Ok(json) => serde_json::from_str(&json)?,
        Err(_) => serde_json::from_value(serde_json::json!({
            "thread_id": "0", "spreadsheet_id": "", "vote_users": {"preview": "0"},
        }))?,
    };
    let settings = Arc::new(settings);
    let store = sheet::Store::offline(settings.clone(), &ranges)?;
    let app = Arc::new(App {
        steam: steam::Steam::new(settings.steam_store.clone()),
        state: state::StateFile::open(&std::env::temp_dir().join("random-games-preview"))?,
        store,
        settings,
        avatars: Default::default(),
    });
    let listen = std::env::var("LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!("preview on http://{listen}");
    web::serve(app, listener).await
}
