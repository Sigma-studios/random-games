# Random Games

The Discord bot behind La Session's game votes, and the public site of their
statistics. The Google Sheet stays the storage: the bot writes to it, people
can keep editing it, and the site reads it.

**The bot**
- A link posted in the proposals thread becomes a new row (name and price from
  Steam), with a ✅ on the message.
- Vote requests arrive by DM; **replying with a grade writes it to the sheet at
  once** (👍), an invalid one gets 👎 and an explanation, and replying again
  changes the grade.
- When a game is marked played on the sheet, the bot asks "who played?" in the
  thread (a menu of names); those players then get a request for their grade
  *after* playing.
- Every day at 18:00 (Europe/Paris), reminders for missing grades, and
  optionally a Steam price update. If the bot was off at that time, it runs on
  start-up.
- Every action is dated in a **Journal** sheet it creates, which the site's
  history and activity charts are drawn from.

**The site** shows everything the sheet's Recap and Stats from Simon tabs
compute, filterable by who counts in the averages, plus a heatmap of who grades
whom, grade distributions, price against grade, comebacks and flops, a page per
game and per player, and the history. It shows first names and Discord avatars
(served through the site, so no Discord account id is exposed), nothing else.

Games are identified by their Steam app id (other links by their URL), never by
their row, so sorting the sheet breaks nothing.

## Configuration

All through the environment:

| Variable | |
|---|---|
| `DISCORD_TOKEN` | the bot's token |
| `GOOGLE_SERVICE_ACCOUNT` | the service account key, as JSON (the sheet must be shared with its email) |
| `BOT_CONFIG` | the old bot's `config.json` without its secrets: same keys (`thread_id`, `spreadsheet_id`, `vote_users`, `columns`, `run_actions`, `max_reminders_per_person`, …), plus optional `daily_time` (`"18:00:00"`), `timezone`, `journal_sheet`, `included_range` (`"Recap!A9:B14"`) |
| `DATA_DIR` | where `state.json` is kept (default `data`); the old bot's `state.json` can be copied there as is |
| `SEED_STATE` | optional: the old bot's `state.json` contents, used when `DATA_DIR` has none yet |
| `LISTEN` | the site's address (default `0.0.0.0:8080`) |

The Discord application needs the **Message Content** intent.

## Running it on façade

```sh
facade app create random-games --git https://github.com/Sigma-studios/random-games \
  --rust-bin random-games --port 8080 --domain random-games.online-app.xyz \
  --volume data:/data --memory 256
facade env set random-games DATA_DIR=/data TZ=Europe/Paris --public
facade env set random-games DISCORD_TOKEN GOOGLE_SERVICE_ACCOUNT BOT_CONFIG   # each read from stdin
facade deploy random-games
```

Only one copy of the bot may run with a token at a time.

## Working on the site

`random-games preview <ranges.json>` serves the site alone, read-only, from sheet
ranges saved in a file (`notes_formulas`, `notes_values`, `helper`, `included`,
`journal`: rows as the Sheets API returns them). Nothing else is needed.

```sh
cargo test
LISTEN=127.0.0.1:8080 cargo run -- preview ranges.json
```

The charts use [Apache ECharts](https://echarts.apache.org) 5.6.0 (Apache-2.0),
vendored in `web/vendor`.
