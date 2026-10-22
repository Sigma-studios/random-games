//! What the bot remembers between restarts, in `$DATA_DIR/state.json`. It
//! reads the old bot's state.json as is, so the switch-over loses nothing: the
//! last thread message and DM replies it saw, and when reminders last went out.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::Mutex;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default, deserialize_with = "opt_id")]
    pub thread_last_message_id: Option<u64>,
    /// Discord user id → the last DM message seen from them.
    #[serde(default, deserialize_with = "id_map")]
    pub dm_last_message_id: BTreeMap<String, u64>,
    #[serde(default)]
    pub last_reminders_sent_at: Option<DateTime<Utc>>,
    /// The local day the daily job last ran (reminders, prices).
    #[serde(default)]
    pub last_daily_run: Option<NaiveDate>,
    /// Played games the bot has asked "who played?" about, or that were played
    /// before it started: it asks once per game.
    #[serde(default)]
    pub played_known: BTreeSet<String>,
    #[serde(default)]
    pub played_baseline: bool,
}

pub struct StateFile {
    path: PathBuf,
    state: Mutex<State>,
}

impl StateFile {
    pub fn open(dir: &Path) -> anyhow::Result<StateFile> {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let path = dir.join("state.json");
        let state = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("{} is not valid", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        Ok(StateFile {
            path,
            state: Mutex::new(state),
        })
    }

    pub async fn get(&self) -> State {
        self.state.lock().await.clone()
    }

    /// Changes the state and saves it (written beside, then renamed, so a
    /// crash mid-write never leaves half a file).
    pub async fn update<T>(&self, change: impl FnOnce(&mut State) -> T) -> anyhow::Result<T> {
        let mut state = self.state.lock().await;
        let out = change(&mut state);
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&*state)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(out)
    }
}

/// The old bot stored message ids as strings.
fn parse_id(v: serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn opt_id<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    Ok(parse_id(serde_json::Value::deserialize(d)?))
}

fn id_map<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, u64>, D::Error> {
    let map = BTreeMap::<String, serde_json::Value>::deserialize(d)?;
    Ok(map
        .into_iter()
        .filter_map(|(k, v)| Some((k, parse_id(v)?)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_old_state() {
        let s: State = serde_json::from_str(
            r#"{"dm_last_message_id": {"11": "1000", "22": null},
                "thread_last_message_id": "2000",
                "known_links": ["https://x"],
                "last_reminders_sent_at": "2026-10-21T16:02:28.123456+00:00"}"#,
        )
        .unwrap();
        assert_eq!(s.thread_last_message_id, Some(2000));
        assert_eq!(s.dm_last_message_id.get("11"), Some(&1000));
        assert!(!s.dm_last_message_id.contains_key("22"));
        assert!(s.last_reminders_sent_at.is_some());
        assert!(!s.played_baseline);
    }
}
