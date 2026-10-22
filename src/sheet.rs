//! The spreadsheet as the bot's storage: a cached snapshot of the Notes,
//! Helper, Recap and Journal sheets, and the few writes the bot makes. Humans
//! edit the same sheet, so every write re-reads where things are first.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};

use crate::config::Settings;
use crate::google::{Render, Rows, Sheets, col_index, col_letters, sheet_prefix};
use crate::journal::{Action, Event, HEADER};
use crate::model::{
    Game, GameKey, Person, Phase, cell_number, cell_text, hyperlink_formula, is_no, is_yes,
    parse_game_cell, steam_slug_name,
};

/// Everything the site and the bot read, as of `fetched_at`.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub people: Vec<Person>,
    pub games: Vec<Game>,
    pub events: Vec<Event>,
    pub fetched_at: DateTime<Utc>,
}

impl Snapshot {
    pub fn game(&self, key: &GameKey) -> Option<&Game> {
        self.games.iter().find(|g| &g.key == key)
    }

    pub fn person(&self, name: &str) -> Option<&Person> {
        self.people.iter().find(|p| p.name == name)
    }

    /// The latest Played event of a game: who played it.
    pub fn players_of(&self, key: &GameKey) -> Option<Vec<String>> {
        self.events
            .iter()
            .rev()
            .find(|e| e.action == Action::Played && e.key.as_ref() == Some(key))
            .map(|e| e.players())
    }

    /// When a game was added, from the journal.
    pub fn added_at(&self, key: &GameKey) -> Option<DateTime<Utc>> {
        self.events
            .iter()
            .find(|e| e.action == Action::Added && e.key.as_ref() == Some(key))
            .map(|e| e.at)
    }
}

/// Where the data range sits: its first column and the header row.
struct Layout {
    sheet: String,
    range: String,
    start_col: usize,
    end_col: usize,
    header_row: u32,
}

impl Layout {
    fn new(s: &Settings) -> anyhow::Result<Layout> {
        let (start, end) = s
            .data_range
            .split_once(':')
            .unwrap_or((&s.data_range, &s.data_range));
        let letters = |r: &str| {
            r.chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect::<String>()
        };
        let start_row: u32 = start
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .parse()
            .context("data_range must start with a cell such as A3")?;
        if start_row != s.header_row {
            bail!("data_range must start on header_row ({})", s.header_row);
        }
        Ok(Layout {
            sheet: s.notes_sheet.clone(),
            range: format!("{}{}", sheet_prefix(&s.notes_sheet), s.data_range),
            start_col: col_index(&letters(start)).context("data_range's first column")?,
            end_col: col_index(&letters(end)).context("data_range's last column")?,
            header_row: s.header_row,
        })
    }

    /// Sheet row of the i-th data row (0-based, after the header).
    fn row(&self, i: usize) -> u32 {
        self.header_row + 1 + i as u32
    }

    /// A1 reference of a cell, `col` being an index into a data row.
    fn cell(&self, row: u32, col: usize) -> String {
        format!(
            "{}{}{row}",
            sheet_prefix(&self.sheet),
            col_letters(self.start_col + col)
        )
    }

    fn whole_row(&self, row: u32) -> String {
        format!(
            "{}{}{row}:{}{row}",
            sheet_prefix(&self.sheet),
            col_letters(self.start_col),
            col_letters(self.end_col)
        )
    }
}

/// The columns of the data range, by header name.
struct Columns {
    width: usize,
    game: usize,
    proposer: Option<usize>,
    played: Option<usize>,
    price: Option<usize>,
}

impl Columns {
    fn new(header: &[Value], s: &Settings) -> anyhow::Result<Columns> {
        let names: Vec<String> = header.iter().map(cell_text).collect();
        let find = |name: &str| names.iter().position(|n| n == name);
        Ok(Columns {
            width: names.len(),
            game: find(&s.columns.game)
                .with_context(|| format!("no column '{}' on {}", s.columns.game, s.notes_sheet))?,
            proposer: find(&s.columns.proposed_by),
            played: find(&s.columns.already_played),
            price: find(&s.columns.price),
        })
    }
}

/// The sheet ranges a preview reads, as the Sheets API returns them.
#[derive(serde::Deserialize)]
pub struct OfflineRanges {
    pub notes_formulas: Rows,
    pub notes_values: Rows,
    pub helper: Rows,
    pub included: Rows,
    #[serde(default)]
    pub journal: Rows,
}

pub struct NewGame {
    pub key: GameKey,
    pub name: String,
    pub link: Option<String>,
    pub proposer: String,
    pub price: Option<f64>,
}

pub struct GradeChange {
    pub game: String,
}

pub struct Store {
    /// None for an offline preview (read-only, from a file).
    sheets: Option<Sheets>,
    settings: Arc<Settings>,
    layout: Layout,
    snapshot: RwLock<Arc<Snapshot>>,
    /// One write at a time: each re-reads row positions, then writes.
    writing: Mutex<()>,
}

impl Store {
    pub async fn open(sheets: Sheets, settings: Arc<Settings>) -> anyhow::Result<Store> {
        let layout = Layout::new(&settings)?;
        let store = Store {
            sheets: Some(sheets),
            settings,
            layout,
            snapshot: RwLock::new(Arc::new(Snapshot {
                people: vec![],
                games: vec![],
                events: vec![],
                fetched_at: DateTime::UNIX_EPOCH,
            })),
            writing: Mutex::new(()),
        };
        store.ensure_journal().await?;
        store.refresh().await?;
        Ok(store)
    }

    /// A read-only store over sheet ranges saved in a file (`preview`).
    pub fn offline(settings: Arc<Settings>, ranges: &OfflineRanges) -> anyhow::Result<Store> {
        let layout = Layout::new(&settings)?;
        let (people, games) = parse_notes(
            &layout,
            &settings,
            &ranges.notes_formulas,
            &ranges.notes_values,
            &ranges.helper,
            &ranges.included,
        )?;
        let events = ranges
            .journal
            .iter()
            .filter_map(|r| Event::from_row(r))
            .collect();
        Ok(Store {
            sheets: None,
            settings,
            layout,
            snapshot: RwLock::new(Arc::new(Snapshot {
                people,
                games,
                events,
                fetched_at: Utc::now(),
            })),
            writing: Mutex::new(()),
        })
    }

    fn api(&self) -> anyhow::Result<&Sheets> {
        self.sheets.as_ref().context("this is a read-only preview")
    }

    pub async fn current(&self) -> Arc<Snapshot> {
        self.snapshot.read().await.clone()
    }

    /// Re-reads the sheet; the site's data and the bot's view change together.
    pub async fn refresh(&self) -> anyhow::Result<Arc<Snapshot>> {
        if self.sheets.is_none() {
            return Ok(self.current().await);
        }
        let snapshot = Arc::new(self.load().await?);
        *self.snapshot.write().await = snapshot.clone();
        Ok(snapshot)
    }

    async fn load(&self) -> anyhow::Result<Snapshot> {
        let s = &self.settings;
        let helper = format!("{}A1:D", sheet_prefix(&s.helper_sheet));
        let journal = format!("{}A2:H", sheet_prefix(&s.journal_sheet));
        let mut formula = self
            .api()?
            .get(
                &[&self.layout.range, &helper, &s.included_range],
                Render::Formula,
            )
            .await?
            .into_iter();
        let mut values = self
            .api()?
            .get(&[&self.layout.range, &journal], Render::Unformatted)
            .await?
            .into_iter();
        let (notes_f, helper, included) = (
            formula.next().unwrap_or_default(),
            formula.next().unwrap_or_default(),
            formula.next().unwrap_or_default(),
        );
        let (notes_v, journal) = (
            values.next().unwrap_or_default(),
            values.next().unwrap_or_default(),
        );
        let events = journal.iter().filter_map(|r| Event::from_row(r)).collect();
        let (people, games) = parse_notes(&self.layout, s, &notes_f, &notes_v, &helper, &included)?;
        Ok(Snapshot {
            people,
            games,
            events,
            fetched_at: Utc::now(),
        })
    }

    async fn ensure_journal(&self) -> anyhow::Result<()> {
        let meta = self.api()?.metadata().await?;
        let exists = meta["sheets"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|s| s["properties"]["title"] == self.settings.journal_sheet);
        if exists {
            return Ok(());
        }
        self.api()?
            .batch_update(vec![json!({"addSheet": {"properties": {
                "title": self.settings.journal_sheet,
                "gridProperties": {"frozenRowCount": 1},
            }}})])
            .await?;
        self.api()?
            .set(vec![(
                format!("{}A1:H1", sheet_prefix(&self.settings.journal_sheet)),
                vec![HEADER.iter().map(|h| json!(h)).collect()],
            )])
            .await?;
        tracing::info!("created the {} sheet", self.settings.journal_sheet);
        Ok(())
    }

    pub async fn record(&self, events: Vec<Event>) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let rows = events.iter().map(Event::to_row).collect();
        self.api()?
            .append(
                &format!("{}A:H", sheet_prefix(&self.settings.journal_sheet)),
                rows,
                true,
            )
            .await
    }

    /// Writes one person's grade on a game, wherever its row is now (the grade
    /// it replaced goes to the Journal).
    pub async fn set_grade(
        &self,
        key: &GameKey,
        person: &str,
        phase: Phase,
        value: f64,
    ) -> anyhow::Result<GradeChange> {
        let _writing = self.writing.lock().await;
        let snap = self.refresh().await?;
        let game = snap
            .game(key)
            .ok_or_else(|| anyhow!("{key} is not on the sheet any more"))?;
        let who = snap
            .person(person)
            .ok_or_else(|| anyhow!("{person} has no column on the sheet"))?;
        let col = match phase {
            Phase::Before => who.col_before,
            Phase::After => who
                .col_after
                .ok_or_else(|| anyhow!("{person} has no after-playing column (Helper sheet)"))?,
        };
        let previous = game.grade(phase, person);
        self.api()?
            .set(vec![(
                self.layout.cell(game.row, col),
                vec![vec![json!(value)]],
            )])
            .await?;
        let action = match phase {
            Phase::Before => Action::GradeBefore,
            Phase::After => Action::GradeAfter,
        };
        let change = GradeChange {
            game: game.name.clone(),
        };
        let event = Event::new(action, key, &game.name)
            .person(person)
            .value(value, previous);
        self.record(vec![event]).await?;
        drop(_writing);
        self.refresh().await?;
        Ok(change)
    }

    /// Marks a game played (the Oui in "Deja joue?") and dates who played it.
    pub async fn mark_played(&self, key: &GameKey, players: &[String]) -> anyhow::Result<String> {
        let _writing = self.writing.lock().await;
        let snap = self.refresh().await?;
        let game = snap
            .game(key)
            .ok_or_else(|| anyhow!("{key} is not on the sheet any more"))?;
        let header = self.header().await?;
        let cols = Columns::new(&header, &self.settings)?;
        let col = cols
            .played
            .context("the sheet has no already-played column")?;
        if !game.played {
            self.api()?
                .set(vec![(
                    self.layout.cell(game.row, col),
                    vec![vec![json!("Oui")]],
                )])
                .await?;
        }
        let name = game.name.clone();
        self.record(vec![
            Event::new(Action::Played, key, &name).details(players.join(", ")),
        ])
        .await?;
        drop(_writing);
        self.refresh().await?;
        Ok(name)
    }

    async fn header(&self) -> anyhow::Result<Vec<Value>> {
        let header = format!(
            "{}{}{h}:{}{h}",
            sheet_prefix(&self.layout.sheet),
            col_letters(self.layout.start_col),
            col_letters(self.layout.end_col),
            h = self.layout.header_row
        );
        Ok(self
            .api()?
            .get(&[&header], Render::Formula)
            .await?
            .into_iter()
            .next()
            .and_then(|rows| rows.into_iter().next())
            .unwrap_or_default())
    }

    /// Adds games as new rows: blank rows in the table first, then the table's
    /// end (or, without a table, inserted rows that copy the formulas above).
    pub async fn add_games(&self, new: Vec<NewGame>) -> anyhow::Result<()> {
        if new.is_empty() {
            return Ok(());
        }
        let _writing = self.writing.lock().await;
        let rows = self
            .api()?
            .get(&[&self.layout.range], Render::Formula)
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        let header = rows.first().cloned().unwrap_or_default();
        let cols = Columns::new(&header, &self.settings)?;
        let data = rows.get(1..).unwrap_or_default();
        let built: Vec<Vec<Value>> = new.iter().map(|g| self.build_row(&cols, g)).collect();

        let blanks: Vec<u32> = data
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.get(cols.game)
                    .map(cell_text)
                    .unwrap_or_default()
                    .is_empty()
            })
            .map(|(i, _)| self.layout.row(i))
            .collect();
        let filled = blanks.len().min(built.len());
        let writes: Vec<(String, Rows)> = blanks
            .iter()
            .zip(&built)
            .map(|(row, values)| (self.layout.whole_row(*row), vec![values.clone()]))
            .collect();
        self.api()?.set(writes).await?;

        let rest = &built[filled..];
        if !rest.is_empty() {
            let meta = self.api()?.metadata().await?;
            let sheet = meta["sheets"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s["properties"]["title"] == self.layout.sheet)
                .context("the Notes sheet is missing")?;
            let sheet_id = sheet["properties"]["sheetId"].clone();
            let table = sheet["tables"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|t| t["name"] == self.settings.data_table_name);
            if let Some(table) = table {
                self.api()?
                    .batch_update(vec![json!({"appendCells": {
                        "tableId": table["tableId"],
                        "rows": rest.iter().map(|r| append_row(r)).collect::<Vec<_>>(),
                        "fields": "userEnteredValue",
                    }})])
                    .await?;
            } else {
                self.insert_rows(&sheet_id, data.len(), rest).await?;
            }
        }
        let events = new
            .iter()
            .map(|g| Event::new(Action::Added, &g.key, &g.name).person(&g.proposer))
            .collect();
        self.record(events).await?;
        drop(_writing);
        self.refresh().await?;
        Ok(())
    }

    async fn insert_rows(
        &self,
        sheet_id: &Value,
        existing: usize,
        rows: &[Vec<Value>],
    ) -> anyhow::Result<()> {
        let first = self.layout.row(existing);
        let n = rows.len() as u32;
        let mut requests = vec![json!({"insertDimension": {
            "range": {"sheetId": sheet_id, "dimension": "ROWS",
                      "startIndex": first - 1, "endIndex": first - 1 + n},
            "inheritFromBefore": true,
        }})];
        let autofill = &self.settings.link_ingest.auto_fill_formulas;
        if autofill.enabled && existing > 0 {
            for range in &autofill.ranges {
                let (a, b) = range.split_once(':').unwrap_or((range, range));
                let (Some(a), Some(b)) = (col_index(a), col_index(b)) else {
                    tracing::warn!("auto_fill_formulas range {range} is not like K:K");
                    continue;
                };
                requests.push(json!({"copyPaste": {
                    "source": {"sheetId": sheet_id, "startRowIndex": first - 2, "endRowIndex": first - 1,
                               "startColumnIndex": a, "endColumnIndex": b + 1},
                    "destination": {"sheetId": sheet_id, "startRowIndex": first - 1, "endRowIndex": first - 1 + n,
                                    "startColumnIndex": a, "endColumnIndex": b + 1},
                    "pasteType": "PASTE_FORMULA",
                }}));
            }
        }
        self.api()?.batch_update(requests).await?;
        // Values only where the new rows have them, so copied formulas stay.
        let mut writes = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            for (col, value) in row.iter().enumerate() {
                if value != &json!("") {
                    writes.push((
                        self.layout.cell(first + i as u32, col),
                        vec![vec![value.clone()]],
                    ));
                }
            }
        }
        self.api()?.set(writes).await
    }

    fn build_row(&self, cols: &Columns, game: &NewGame) -> Vec<Value> {
        let mut row = vec![json!(""); cols.width];
        row[cols.game] = match (&game.link, self.settings.link_ingest.use_hyperlink_formula) {
            (Some(link), true) => json!(hyperlink_formula(link, &game.name)),
            (Some(link), false) if game.name.is_empty() => json!(link),
            _ => json!(game.name),
        };
        if let Some(c) = cols.proposer {
            row[c] = json!(game.proposer);
        }
        if let (Some(c), Some(v)) = (cols.played, &self.settings.defaults.already_played) {
            row[c] = json!(v);
        }
        if let (Some(c), Some(p)) = (cols.price, game.price) {
            row[c] = json!(p);
        }
        row
    }

    /// Writes changed Steam prices (by game) and dates them.
    pub async fn set_prices(&self, prices: &HashMap<GameKey, f64>) -> anyhow::Result<usize> {
        let _writing = self.writing.lock().await;
        let snap = self.refresh().await?;
        let header = self.header().await?;
        let Some(col) = Columns::new(&header, &self.settings)?.price else {
            bail!("the sheet has no price column");
        };
        let mut writes = Vec::new();
        let mut events = Vec::new();
        for game in &snap.games {
            let Some(price) = prices.get(&game.key) else {
                continue;
            };
            if game.price.is_some_and(|p| (p - price).abs() < 0.005) {
                continue;
            }
            writes.push((self.layout.cell(game.row, col), vec![vec![json!(price)]]));
            events.push(Event::new(Action::Price, &game.key, &game.name).value(*price, game.price));
        }
        let changed = writes.len();
        self.api()?.set(writes).await?;
        self.record(events).await?;
        drop(_writing);
        self.refresh().await?;
        Ok(changed)
    }
}

/// A row for the appendCells request: typed values, formulas as formulas.
fn append_row(row: &[Value]) -> Value {
    let last = row
        .iter()
        .rposition(|v| v != &json!(""))
        .map_or(0, |i| i + 1);
    let values: Vec<Value> = row[..last]
        .iter()
        .map(|v| match v {
            Value::String(s) if s.is_empty() => json!({}),
            Value::String(s) if s.starts_with('=') => {
                json!({"userEnteredValue": {"formulaValue": s}})
            }
            Value::String(s) => json!({"userEnteredValue": {"stringValue": s}}),
            Value::Number(n) => json!({"userEnteredValue": {"numberValue": n}}),
            Value::Bool(b) => json!({"userEnteredValue": {"boolValue": b}}),
            _ => json!({}),
        })
        .collect();
    json!({"values": values})
}

/// The Notes data range (formulas and values), the Helper sheet's columns and
/// the Recap checkboxes, into people and games.
fn parse_notes(
    layout: &Layout,
    s: &Settings,
    formulas: &Rows,
    values: &Rows,
    helper: &Rows,
    included: &Rows,
) -> anyhow::Result<(Vec<Person>, Vec<Game>)> {
    let header = formulas
        .first()
        .context("the Notes sheet's data range is empty")?;
    let cols = Columns::new(header, s)?;
    let header_names: Vec<String> = header.iter().map(cell_text).collect();

    let unchecked: HashSet<String> = included
        .iter()
        .filter(|r| {
            r.get(1)
                .is_some_and(|v| v == &json!(false) || is_no(&cell_text(v)))
        })
        .filter_map(|r| r.first().map(cell_text))
        .collect();
    let to_data_col =
        |letters: &str| col_index(letters).and_then(|c| c.checked_sub(layout.start_col));

    // Helper: Nom | Col Avant | Col Après; without it, a column named after the
    // person holds their grade before playing, and "<name> 2" after.
    let mut people = Vec::new();
    let helper_header: Vec<String> = helper
        .first()
        .map(|r| r.iter().map(cell_text).collect())
        .unwrap_or_default();
    let h = |names: &[&str]| {
        helper_header
            .iter()
            .position(|c| names.contains(&c.as_str()))
    };
    if let (Some(name_i), Some(before_i)) = (h(&["Nom"]), h(&["Col Avant"])) {
        let after_i = h(&["Col Après", "Col Apres"]);
        for row in helper.iter().skip(1) {
            let name = row.get(name_i).map(cell_text).unwrap_or_default();
            let Some(col_before) = row.get(before_i).and_then(|v| to_data_col(&cell_text(v)))
            else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let col_after = after_i
                .and_then(|i| row.get(i))
                .and_then(|v| to_data_col(&cell_text(v)));
            people.push(Person {
                included: !unchecked.contains(&name),
                name,
                col_before,
                col_after,
            });
        }
    }
    if people.is_empty() {
        for name in s.vote_users.keys() {
            if let Some(col_before) = header_names.iter().position(|h| h == name) {
                let col_after = header_names.iter().position(|h| *h == format!("{name} 2"));
                people.push(Person {
                    name: name.clone(),
                    col_before,
                    col_after,
                    included: !unchecked.contains(name),
                });
            }
        }
    }

    let mut games = Vec::new();
    let mut slugs: BTreeMap<String, usize> = BTreeMap::new();
    for (i, row) in formulas.iter().enumerate().skip(1) {
        let value_row = values.get(i).map(Vec::as_slice).unwrap_or_default();
        let (link, label) = parse_game_cell(&row.get(cols.game).map(cell_text).unwrap_or_default());
        let name = label
            .clone()
            .or_else(|| link.as_deref().and_then(steam_slug_name))
            .or_else(|| link.clone());
        let Some(name) = name else { continue };
        let key = link
            .as_deref()
            .and_then(GameKey::from_link)
            .unwrap_or_else(|| GameKey::Name(name.to_lowercase()));
        let text = |c: Option<usize>| {
            c.and_then(|c| value_row.get(c))
                .map(cell_text)
                .unwrap_or_default()
        };
        let grades = |col: fn(&Person) -> Option<usize>| -> BTreeMap<String, f64> {
            people
                .iter()
                .filter_map(|p| {
                    let v = value_row.get(col(p)?).and_then(cell_number)?;
                    Some((p.name.clone(), v))
                })
                .collect()
        };
        // Two rows of the same game (it happens: "Doublons présents") get
        // distinct page slugs; the bot always writes to the first.
        let mut slug = key.slug();
        let seen = slugs.entry(slug.clone()).or_default();
        *seen += 1;
        if *seen > 1 {
            slug = format!("{slug}-{seen}");
        }
        games.push(Game {
            slug,
            key,
            name,
            link,
            proposer: text(cols.proposer),
            played: is_yes(&text(cols.played)),
            price: cols
                .price
                .and_then(|c| value_row.get(c))
                .and_then(cell_number),
            before: grades(|p| Some(p.col_before)),
            after: grades(|p| p.col_after),
            row: layout.row(i - 1),
        });
    }
    Ok((people, games))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        serde_json::from_value(json!({
            "thread_id": "1", "spreadsheet_id": "x",
            "vote_users": {"Ana": "1", "Bob": "2"},
        }))
        .unwrap()
    }

    #[test]
    fn notes_parse() {
        let s = settings();
        let layout = Layout::new(&s).unwrap();
        let header = json!([
            "Nom du jeu",
            "Jeu propose par",
            "Deja joue?",
            "Prix",
            "Ana",
            "Bob",
            "Moyenne Avant",
            "Ana 2",
            "Bob 2"
        ]);
        let formulas: Rows = serde_json::from_value(json!([
            header,
            [
                r#"=HYPERLINK("https://store.steampowered.com/app/10/Ten_Game/","Ten Game")"#,
                "Ana",
                "Non",
                5,
                7,
                "",
                "=AVERAGE(E4:F4)"
            ],
            [""],
            [
                "https://store.steampowered.com/app/11/Eleven/",
                "Bob",
                "Oui",
                0,
                8,
                6,
                "",
                9
            ],
            ["Minecraft", "Bob", "Oui"],
        ]))
        .unwrap();
        let values: Rows = serde_json::from_value(json!([
            header,
            ["Ten Game", "Ana", "Non", 4.99, 7, "", 7],
            [""],
            [
                "https://store.steampowered.com/app/11/Eleven/",
                "Bob",
                "Oui",
                0,
                8,
                "6,5",
                "",
                9
            ],
            ["Minecraft", "Bob", "Oui"],
        ]))
        .unwrap();
        let helper: Rows = serde_json::from_value(json!([
            ["Nom", "Col Avant", "Col Après", "Diff"],
            ["Ana", "E", "H", "T"],
            ["Bob", "F", "I", "U"],
        ]))
        .unwrap();
        let included: Rows =
            serde_json::from_value(json!([["Ana", true], ["Bob", false]])).unwrap();
        let (people, games) =
            parse_notes(&layout, &s, &formulas, &values, &helper, &included).unwrap();

        assert_eq!(people.len(), 2);
        assert_eq!((people[0].col_before, people[0].col_after), (4, Some(7)));
        assert!(people[0].included && !people[1].included);

        assert_eq!(games.len(), 3);
        let ten = &games[0];
        assert_eq!(
            (ten.key.clone(), ten.name.as_str(), ten.row),
            (GameKey::Steam(10), "Ten Game", 4)
        );
        assert_eq!(ten.price, Some(4.99));
        assert_eq!(ten.before.get("Ana"), Some(&7.0));
        assert!(!ten.played && !ten.before.contains_key("Bob"));
        let eleven = &games[1];
        assert_eq!((eleven.name.as_str(), eleven.row), ("Eleven", 6));
        assert_eq!(eleven.before.get("Bob"), Some(&6.5));
        assert_eq!(eleven.after.get("Ana"), Some(&9.0));
        assert!(eleven.played);
        assert_eq!(games[2].key, GameKey::Name("minecraft".into()));

        assert_eq!(layout.cell(6, 7), "'Notes'!H6");
        assert_eq!(layout.whole_row(4), "'Notes'!A4:Z4");
    }

    #[test]
    fn appended_rows_are_typed() {
        let row = append_row(&[
            json!("=HYPERLINK(\"a\",\"b\")"),
            json!("Ana"),
            json!(""),
            json!(4.5),
            json!(""),
        ]);
        assert_eq!(
            row,
            json!({"values": [
                {"userEnteredValue": {"formulaValue": "=HYPERLINK(\"a\",\"b\")"}},
                {"userEnteredValue": {"stringValue": "Ana"}},
                {},
                {"userEnteredValue": {"numberValue": 4.5}},
            ]})
        );
    }
}
