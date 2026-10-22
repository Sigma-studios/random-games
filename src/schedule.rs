//! Timed work: the daily reminders (and Steam prices) at `daily_time`, run at
//! start-up instead when the bot was off at that time; and a sheet re-read
//! every two minutes, which also notices games marked played by hand.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serenity::all::{ChannelId, Http};

use crate::app::App;
use crate::bot::{ask_who_played, missing_for, send_request};
use crate::model::GameKey;

const REFRESH: Duration = Duration::from_secs(120);

pub fn start(app: Arc<App>, http: Arc<Http>) {
    tokio::spawn(daily(app.clone(), http.clone()));
    tokio::spawn(refresh(app, http));
}

/// The local day of the latest scheduled run at or before `now`.
fn latest_due_day(now: DateTime<Utc>, time: chrono::NaiveTime, tz: chrono_tz::Tz) -> NaiveDate {
    let local = now.with_timezone(&tz);
    let today = local.date_naive();
    match due_at(today, time, tz) {
        Some(due) if now >= due => today,
        _ => today.pred_opt().unwrap_or(today),
    }
}

fn due_at(day: NaiveDate, time: chrono::NaiveTime, tz: chrono_tz::Tz) -> Option<DateTime<Utc>> {
    // `earliest`: on the night clocks go back, the first of the two times.
    tz.from_local_datetime(&day.and_time(time))
        .earliest()
        .map(|t| t.with_timezone(&Utc))
}

async fn daily(app: Arc<App>, http: Arc<Http>) {
    let s = app.settings.clone();
    // The old bot's state has no run day: its last reminders stand in for it.
    let _ = app
        .state
        .update(|st| {
            if st.last_daily_run.is_none() {
                st.last_daily_run = st
                    .last_reminders_sent_at
                    .map(|t| t.with_timezone(&s.timezone).date_naive());
            }
        })
        .await;
    loop {
        let now = Utc::now();
        let due_day = latest_due_day(now, s.daily_time, s.timezone);
        let last = app.state.get().await.last_daily_run;
        if last.is_none_or(|l| l < due_day) {
            tracing::info!("daily run for {due_day}");
            run_daily(&app, &http).await;
            let _ = app
                .state
                .update(|st| st.last_daily_run = Some(due_day))
                .await;
            continue;
        }
        let next = due_at(
            due_day.succ_opt().unwrap_or(due_day),
            s.daily_time,
            s.timezone,
        )
        .unwrap_or(now + chrono::Duration::hours(1));
        // At most an hour at a time, so a suspended machine or a clock change
        // never pushes the run back by long.
        let wait = (next - now)
            .to_std()
            .unwrap_or_default()
            .min(Duration::from_secs(3600));
        tokio::time::sleep(wait.max(Duration::from_secs(1))).await;
    }
}

async fn run_daily(app: &App, http: &Http) {
    if app.settings.run_actions.update_prices
        && let Err(e) = update_prices(app).await
    {
        tracing::error!("price update: {e:#}");
    }
    if app.settings.run_actions.send_reminders
        && let Err(e) = reminders(app, http).await
    {
        tracing::error!("reminders: {e:#}");
    }
}

async fn update_prices(app: &App) -> anyhow::Result<()> {
    let snapshot = app.store.refresh().await?;
    let mut ids: Vec<u64> = snapshot
        .games
        .iter()
        .filter_map(|g| match g.key {
            GameKey::Steam(id) => Some(id),
            _ => None,
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    ids.sort();
    let found = app.steam.apps(&ids).await;
    let prices: HashMap<GameKey, f64> = found
        .into_iter()
        .filter_map(|(id, info)| Some((GameKey::Steam(id), info.price?)))
        .collect();
    let changed = app.store.set_prices(&prices).await?;
    tracing::info!(
        "prices: {} app(s) checked, {changed} row(s) changed",
        ids.len()
    );
    Ok(())
}

/// The old bot's reminders: for each person, a DM per missing grade, the
/// first N in sheet order (max_reminders_per_person), at most once per
/// reminder_cooldown_hours. A person whose DMs fail is skipped for the run.
async fn reminders(app: &App, http: &Http) -> anyhow::Result<()> {
    let s = &app.settings;
    let state = app.state.get().await;
    if s.reminder_cooldown_hours > 0.0
        && let Some(last) = state.last_reminders_sent_at
    {
        let next = last + chrono::Duration::seconds((s.reminder_cooldown_hours * 3600.0) as i64);
        if Utc::now() < next {
            tracing::info!("reminders skipped: cooldown until {next}");
            return Ok(());
        }
    }
    let snapshot = app.store.refresh().await?;
    let mut sent = 0;
    for person in s.vote_users.keys() {
        let missing = missing_for(&snapshot, person);
        let limit = s
            .reminder_limit(person)
            .map_or(missing.len(), |l| l as usize);
        let mut theirs = 0;
        for (key, phase) in missing.iter().take(limit) {
            match send_request(app, http, &snapshot, key, person, *phase).await {
                Ok(()) => theirs += 1,
                Err(e) => {
                    tracing::warn!("reminders to {person} stopped for this run: {e}");
                    break;
                }
            }
        }
        tracing::info!("reminders sent to {person}: {theirs}");
        sent += theirs;
    }
    if sent > 0 {
        app.state
            .update(|st| st.last_reminders_sent_at = Some(Utc::now()))
            .await?;
    }
    tracing::info!("reminders sent: {sent}");
    Ok(())
}

async fn refresh(app: Arc<App>, http: Arc<Http>) {
    let mut tick = tokio::time::interval(REFRESH);
    tick.tick().await;
    loop {
        tick.tick().await;
        if let Err(e) = refresh_once(&app, &http).await {
            tracing::warn!("sheet refresh: {e:#}");
        }
    }
}

async fn refresh_once(app: &App, http: &Http) -> anyhow::Result<()> {
    let snapshot = app.store.refresh().await?;
    let played: Vec<&GameKey> = snapshot
        .games
        .iter()
        .filter(|g| g.played && snapshot.players_of(&g.key).is_none())
        .map(|g| &g.key)
        .collect();
    let state = app.state.get().await;
    if !state.played_baseline {
        // Games played before this bot existed: nobody is asked about them.
        app.state
            .update(|st| {
                st.played_known.extend(played.iter().map(|k| k.to_string()));
                st.played_baseline = true;
            })
            .await?;
        return Ok(());
    }
    let thread = ChannelId::new(app.settings.thread_id);
    for key in played {
        if state.played_known.contains(&key.to_string()) {
            continue;
        }
        ask_who_played(http, thread, &snapshot, key).await?;
        app.state
            .update(|st| {
                st.played_known.insert(key.to_string());
            })
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;

    #[test]
    fn due_days() {
        let six = NaiveTime::from_hms_opt(18, 0, 0).unwrap();
        let tz = chrono_tz::Europe::Paris;
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let day = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        // 18:00 in Paris is 16:00 UTC in summer time.
        assert_eq!(
            latest_due_day(at("2026-10-21T15:59:00Z"), six, tz),
            day("2026-10-20")
        );
        assert_eq!(
            latest_due_day(at("2026-10-21T16:00:00Z"), six, tz),
            day("2026-10-21")
        );
        // …and 17:00 UTC in winter.
        assert_eq!(
            latest_due_day(at("2026-12-01T16:30:00Z"), six, tz),
            day("2026-11-30")
        );
        assert_eq!(
            latest_due_day(at("2026-12-01T17:00:00Z"), six, tz),
            day("2026-12-01")
        );
    }
}
