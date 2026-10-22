//! The public, read-only site: one page drawn in the browser from /api/data,
//! which holds every table of the sheet computed for the chosen people.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::app::App;
use crate::journal::{Action, Event};
use crate::model::{GameKey, Phase};
use crate::sheet::Snapshot;
use crate::stats::{self, GameRow, Included, PersonSummary, PlayerStats};
use crate::steam::header_image;

/// Fetched at, bytes, content type.
type CachedImage = (Instant, Vec<u8>, String);

struct Site {
    app: Arc<App>,
    avatars: Mutex<HashMap<String, CachedImage>>,
    covers: Mutex<HashMap<u64, String>>,
    /// Two store lookups at a time: a page of covers must not burst past
    /// Steam's rate limit.
    cover_lookups: tokio::sync::Semaphore,
    http: reqwest::Client,
}

pub async fn serve(app: Arc<App>, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    let site = Arc::new(Site {
        app,
        avatars: Mutex::new(HashMap::new()),
        covers: Mutex::new(HashMap::new()),
        cover_lookups: tokio::sync::Semaphore::new(2),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?,
    });
    axum::serve(listener, router(site)).await?;
    Ok(())
}

fn router(site: Arc<Site>) -> Router {
    Router::new()
        .route("/", get(|| async { asset("index.html") }))
        .route("/app.js", get(|| async { asset("app.js") }))
        .route("/style.css", get(|| async { asset("style.css") }))
        .route(
            "/vendor/echarts.min.js",
            get(|| async { asset("echarts.min.js") }),
        )
        .route("/logo.png", get(|| async { asset("logo.png") }))
        .route("/favicon.png", get(|| async { asset("favicon.png") }))
        .route("/api/data", get(data))
        .route("/avatars/{name}", get(avatar))
        .route("/covers/{id}", get(cover))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(site)
}

fn asset(name: &str) -> Response {
    let (body, kind): (&'static [u8], &str) = match name {
        "index.html" => (
            include_bytes!("../web/index.html"),
            "text/html; charset=utf-8",
        ),
        "app.js" => (
            include_bytes!("../web/app.js"),
            "text/javascript; charset=utf-8",
        ),
        "style.css" => (
            include_bytes!("../web/style.css"),
            "text/css; charset=utf-8",
        ),
        "echarts.min.js" => (
            include_bytes!("../web/vendor/echarts.min.js"),
            "text/javascript; charset=utf-8",
        ),
        "logo.png" => (include_bytes!("../web/logo.png"), "image/png"),
        "favicon.png" => (include_bytes!("../web/favicon.png"), "image/png"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let cache = if name == "echarts.min.js" {
        "public, max-age=604800"
    } else {
        "no-cache"
    };
    (
        [(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, cache)],
        body,
    )
        .into_response()
}

#[derive(Deserialize)]
struct DataQuery {
    /// Comma-separated people counted in averages; the sheet's checkboxes
    /// when absent.
    include: Option<String>,
}

#[derive(Serialize)]
struct Data {
    updated_at: DateTime<Utc>,
    vote_min: f64,
    vote_max: f64,
    people: Vec<PersonView>,
    included: BTreeSet<String>,
    summaries: Vec<PersonSummary>,
    games: Vec<GameView>,
    to_play: Vec<String>,
    played: Vec<String>,
    comebacks: Vec<String>,
    matrix_before: BTreeMap<String, BTreeMap<String, Option<f64>>>,
    matrix_after: BTreeMap<String, BTreeMap<String, Option<f64>>>,
    players: Vec<PlayerStats>,
    events: Vec<EventView>,
}

#[derive(Serialize)]
struct PersonView {
    name: String,
    /// Counted by default (the sheet's checkbox).
    sheet_included: bool,
    avatar: String,
}

#[derive(Serialize)]
struct GameView {
    #[serde(flatten)]
    row: GameRow,
    cover: Option<String>,
    before: BTreeMap<String, f64>,
    after: BTreeMap<String, f64>,
    players: Option<Vec<String>>,
    added_at: Option<DateTime<Utc>>,
    played_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct EventView {
    at: DateTime<Utc>,
    action: Action,
    slug: Option<String>,
    game: String,
    person: Option<String>,
    value: Option<f64>,
    previous: Option<f64>,
    players: Vec<String>,
}

async fn data(State(site): State<Arc<Site>>, Query(q): Query<DataQuery>) -> Response {
    let snapshot = site.app.store.current().await;
    let included = match q.include {
        Some(list) => Included(
            list.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| snapshot.person(s).is_some())
                .collect(),
        ),
        None => Included::from_people(&snapshot.people),
    };
    let body = build(&snapshot, &included, &site.app.settings);
    ([(header::CACHE_CONTROL, "no-cache")], axum::Json(body)).into_response()
}

/// The snapshot as if only the included people existed: their grades, their
/// journal entries, their place in who-played lists. Games stay, whoever
/// proposed them.
fn only_included(snap: &Snapshot, inc: &Included) -> Snapshot {
    let keep = |grades: &BTreeMap<String, f64>| -> BTreeMap<String, f64> {
        grades
            .iter()
            .filter(|(who, _)| inc.contains(who))
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    };
    Snapshot {
        people: snap
            .people
            .iter()
            .filter(|p| inc.contains(&p.name))
            .cloned()
            .collect(),
        games: snap
            .games
            .iter()
            .map(|g| crate::model::Game {
                before: keep(&g.before),
                after: keep(&g.after),
                ..g.clone()
            })
            .collect(),
        events: snap
            .events
            .iter()
            .filter(|e| e.person.as_ref().is_none_or(|p| inc.contains(p)))
            .map(|e| {
                let mut e = e.clone();
                if e.action == Action::Played {
                    e.details = e
                        .players()
                        .into_iter()
                        .filter(|p| inc.contains(p))
                        .collect::<Vec<_>>()
                        .join(", ");
                }
                e
            })
            .collect(),
        fetched_at: snap.fetched_at,
    }
}

fn build(full: &Snapshot, inc: &Included, settings: &crate::config::Settings) -> Data {
    let filtered = only_included(full, inc);
    let snap = &filtered;
    let slugs: HashMap<&GameKey, &str> = snap
        .games
        .iter()
        .map(|g| (&g.key, g.slug.as_str()))
        .collect();
    let first_event = |key: &GameKey, action: Action| -> Option<DateTime<Utc>> {
        snap.events
            .iter()
            .filter(|e| e.action == action && e.key.as_ref() == Some(key))
            .map(|e| e.at)
            .min()
    };
    let games = snap
        .games
        .iter()
        .map(|g| GameView {
            row: stats::game_row(g, inc),
            cover: match g.key {
                GameKey::Steam(id) => Some(format!("/covers/{id}")),
                _ => None,
            },
            before: g.before.clone(),
            after: g.after.clone(),
            players: snap.players_of(&g.key),
            added_at: snap.added_at(&g.key),
            played_at: first_event(&g.key, Action::Played),
        })
        .collect();
    let slugs_of = |rows: Vec<GameRow>| rows.into_iter().map(|r| r.slug).collect();
    let event_view = |e: &Event| EventView {
        at: e.at,
        action: e.action,
        slug: e
            .key
            .as_ref()
            .and_then(|k| slugs.get(k))
            .map(|s| s.to_string()),
        game: e.game.clone(),
        person: e.person.clone(),
        value: e.value,
        previous: e.previous,
        players: if e.action == Action::Played {
            e.players()
        } else {
            vec![]
        },
    };
    Data {
        updated_at: snap.fetched_at,
        vote_min: settings.vote_min,
        vote_max: settings.vote_max,
        people: full
            .people
            .iter()
            .map(|p| PersonView {
                name: p.name.clone(),
                sheet_included: p.included,
                avatar: format!("/avatars/{}", urlencode(&p.name)),
            })
            .collect(),
        included: inc.0.clone(),
        summaries: stats::person_summaries(&snap.people, &snap.games, inc),
        to_play: slugs_of(stats::to_play(&snap.games, inc)),
        played: slugs_of(stats::played(&snap.games, inc)),
        comebacks: slugs_of(stats::comebacks(&snap.games, inc)),
        matrix_before: stats::grade_matrix(&snap.people, &snap.games, Phase::Before),
        matrix_after: stats::grade_matrix(&snap.people, &snap.games, Phase::After),
        players: snap
            .people
            .iter()
            .map(|p| stats::player_stats(&p.name, &snap.people, &snap.games))
            .collect(),
        events: snap.events.iter().map(event_view).collect(),
        games,
    }
}

fn urlencode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// Redirects to a Steam app's cover, asking the store where it is once.
async fn cover(State(site): State<Arc<Site>>, Path(id): Path<u64>) -> Response {
    let cached = site.covers.lock().await.get(&id).cloned();
    let url = match cached {
        Some(url) => url,
        None => {
            let _permit = site.cover_lookups.acquire().await;
            let found = match site.app.steam.app(id).await {
                Ok(info) => info.and_then(|i| i.header_image),
                Err(e) => {
                    tracing::debug!("cover of {id}: {e}");
                    None
                }
            };
            let url = found.clone().unwrap_or_else(|| header_image(id));
            if found.is_some() {
                site.covers.lock().await.insert(id, url.clone());
            }
            url
        }
    };
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, url),
            (header::CACHE_CONTROL, "public, max-age=86400".to_string()),
        ],
    )
        .into_response()
}

const AVATAR_TTL: Duration = Duration::from_secs(6 * 3600);

/// A person's Discord avatar, fetched by the server so the page never sees
/// the Discord URL (it contains the account id). Initials when there is none.
async fn avatar(State(site): State<Arc<Site>>, Path(name): Path<String>) -> Response {
    if let Some((at, bytes, kind)) = site.avatars.lock().await.get(&name)
        && at.elapsed() < AVATAR_TTL
    {
        return image(bytes.clone(), kind);
    }
    let url = site.app.avatars.read().await.get(&name).cloned();
    if let Some(url) = url {
        match site
            .http
            .get(sized(&url))
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(response) => {
                let kind = response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .filter(|k| k.starts_with("image/"))
                    .unwrap_or("image/png")
                    .to_string();
                if let Ok(bytes) = response.bytes().await {
                    let bytes = bytes.to_vec();
                    site.avatars
                        .lock()
                        .await
                        .insert(name, (Instant::now(), bytes.clone(), kind.clone()));
                    return image(bytes, &kind);
                }
            }
            Err(e) => tracing::warn!("avatar of {name}: {e}"),
        }
    }
    if site.app.store.current().await.person(&name).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    image(initials_svg(&name).into_bytes(), "image/svg+xml")
}

/// Asks Discord's CDN for a 128px image, whatever size the URL had.
fn sized(url: &str) -> String {
    let base = url.split('?').next().unwrap_or(url);
    format!("{base}?size=128")
}

fn image(bytes: Vec<u8>, kind: &str) -> Response {
    let kind = HeaderValue::from_str(kind).unwrap_or(HeaderValue::from_static("image/png"));
    (
        [
            (header::CONTENT_TYPE, kind),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            ),
        ],
        bytes,
    )
        .into_response()
}

fn initials_svg(name: &str) -> String {
    let initials: String = name
        .split(['-', ' '])
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    let hue = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
        % 360;
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="32" fill="hsl({hue} 45% 45%)"/><text x="32" y="41" font-family="system-ui,sans-serif" font-size="24" font-weight="600" fill="#fff" text-anchor="middle">{initials}</text></svg>"##
    )
}
