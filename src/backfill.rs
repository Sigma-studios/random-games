//! `random-games backfill`: dates the past. The Journal only starts with this
//! bot, but Discord kept everything: each proposal in the thread and each
//! grade replied to a vote request has its timestamp. This reads those
//! histories once and writes them to an empty Journal, oldest first.

use std::collections::{HashMap, HashSet};

use anyhow::bail;
use chrono::{DateTime, Utc};
use serenity::all::{ChannelId, Http, UserId};

use crate::app::App;
use crate::bot::history;
use crate::journal::{Action, Event};
use crate::model::{GameKey, Phase, extract_links, parse_grade};
use crate::votes;

pub async fn run(app: &App, token: &str) -> anyhow::Result<()> {
    let snapshot = app.store.refresh().await?;
    if !snapshot.events.is_empty() {
        bail!("the Journal sheet already has rows: backfill only fills an empty one");
    }
    let s = &app.settings;
    let http = Http::new(token);
    let me = http.get_current_user().await?.id;
    let mut events = Vec::new();

    // Proposals: the first message of the thread carrying each game's link.
    let mut seen: HashSet<GameKey> = HashSet::new();
    let thread = history(&http, ChannelId::new(s.thread_id), 1).await?;
    tracing::info!("thread: {} messages", thread.len());
    for msg in &thread {
        if msg.author.bot {
            continue;
        }
        let (links, _) = extract_links(&msg.content);
        for link in links {
            let Some(key) = GameKey::from_link(&link) else {
                continue;
            };
            let Some(game) = snapshot.game(&key) else {
                continue;
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            let names: Vec<&str> = [
                msg.author.global_name.as_deref(),
                Some(msg.author.name.as_str()),
            ]
            .into_iter()
            .flatten()
            .collect();
            let mut e = Event::new(Action::Added, &key, &game.name)
                .person(&s.resolve_person(msg.author.id.get(), &names));
            e.at = at(msg.timestamp);
            events.push(e);
        }
    }

    // Grades: each reply to a vote request, in order, so a later reply shows
    // the grade it replaced.
    for (person, id) in &s.vote_users {
        let dm = match UserId::new(*id).create_dm_channel(&http).await {
            Ok(dm) => dm,
            Err(e) => {
                tracing::warn!("DMs of {person}: {e}");
                continue;
            }
        };
        let msgs = history(&http, dm.id, 1).await?;
        let mut last: HashMap<(GameKey, Phase), f64> = HashMap::new();
        let mut n = 0;
        for msg in &msgs {
            if msg.author.id.get() != *id {
                continue;
            }
            let Some(parent) = msg.referenced_message.as_deref() else {
                continue;
            };
            if parent.author.id != me {
                continue;
            }
            let Some((key, phase)) = votes::parse(&parent.content, &snapshot) else {
                continue;
            };
            let Some(game) = snapshot.game(&key) else {
                continue;
            };
            let Some(grade) =
                parse_grade(&msg.content).filter(|g| (s.vote_min..=s.vote_max).contains(g))
            else {
                continue;
            };
            let previous = last.insert((key.clone(), phase), grade);
            if previous == Some(grade) {
                continue;
            }
            let action = match phase {
                Phase::Before => Action::GradeBefore,
                Phase::After => Action::GradeAfter,
            };
            let mut e = Event::new(action, &key, &game.name)
                .person(person)
                .value(grade, previous);
            e.at = at(msg.timestamp);
            events.push(e);
            n += 1;
        }
        tracing::info!("{person}: {} DMs, {n} grades", msgs.len());
    }

    events.sort_by_key(|e| e.at);
    tracing::info!("writing {} events to the Journal", events.len());
    for chunk in events.chunks(500) {
        app.store.record(chunk.to_vec()).await?;
    }
    Ok(())
}

fn at(t: serenity::all::Timestamp) -> DateTime<Utc> {
    DateTime::from_timestamp(t.unix_timestamp(), 0).unwrap_or_else(Utc::now)
}
