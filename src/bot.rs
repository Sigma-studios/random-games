//! The Discord side. Links posted in the proposals thread become sheet rows,
//! replies to vote requests become grades, the moment they arrive. `/played`
//! records who played a game and asks them for their grade after playing.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, OnceLock};

use serenity::all::{
    ChannelId, CommandDataOptionValue, CommandInteraction, CommandOptionType, ComponentInteraction,
    ComponentInteractionDataKind, Context, CreateActionRow, CreateAutocompleteResponse,
    CreateCommand, CreateCommandOption, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, EditInteractionResponse, EventHandler, GatewayIntents, GetMessages,
    Http, Interaction, Message, MessageId, Ready, UserId,
};
use tokio::sync::Mutex;

use crate::app::App;
use crate::model::{GameKey, Phase, extract_links, parse_grade, steam_slug_name};
use crate::sheet::{NewGame, Snapshot};
use crate::votes;

const THUMBS_UP: char = '👍';
const THUMBS_DOWN: char = '👎';
const CHECK: char = '✅';

pub struct Bot {
    app: Arc<App>,
    me: OnceLock<UserId>,
    started: OnceLock<()>,
    /// Links being added right now, so the same link posted twice in a row is
    /// added once.
    adding: Mutex<HashSet<GameKey>>,
}

pub async fn run(app: Arc<App>, token: &str) -> anyhow::Result<()> {
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS;
    let bot = Bot {
        app,
        me: OnceLock::new(),
        started: OnceLock::new(),
        adding: Mutex::new(HashSet::new()),
    };
    let mut client = serenity::Client::builder(token, intents)
        .event_handler(bot)
        .await?;
    client.start().await?;
    Ok(())
}

#[serenity::async_trait]
impl EventHandler for Bot {
    async fn ready(&self, ctx: Context, ready: Ready) {
        let _ = self.me.set(ready.user.id);
        tracing::info!("logged in to Discord as {}", ready.user.name);
        if self.started.set(()).is_err() {
            return; // a reconnection: everything below already runs
        }
        if let Err(e) = self.register_commands(&ctx).await {
            tracing::error!("cannot register the slash commands: {e:#}");
        }
        self.load_avatars(&ctx).await;
        if let Err(e) = self.catch_up(&ctx).await {
            tracing::error!("catching up on missed messages: {e:#}");
        }
        crate::schedule::start(self.app.clone(), ctx.http.clone());
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }
        let result = if msg.guild_id.is_none() {
            self.on_dm(&ctx, &msg).await
        } else if msg.channel_id.get() == self.app.settings.thread_id {
            self.on_thread_message(&ctx, &msg).await
        } else {
            return;
        };
        if let Err(e) = result {
            tracing::error!("message {}: {e:#}", msg.id);
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let result = match interaction {
            Interaction::Autocomplete(cmd) => self.autocomplete(&ctx, &cmd).await,
            Interaction::Command(cmd) => match cmd.data.name.as_str() {
                "played" => self.cmd_played(&ctx, &cmd).await,
                "missing" => self.cmd_missing(&ctx, &cmd).await,
                _ => Ok(()),
            },
            Interaction::Component(comp) if comp.data.custom_id.starts_with("played:") => {
                self.on_players_chosen(&ctx, &comp).await
            }
            _ => Ok(()),
        };
        if let Err(e) = result {
            tracing::error!("interaction: {e:#}");
        }
    }
}

impl Bot {
    fn me(&self) -> Option<UserId> {
        self.me.get().copied()
    }

    async fn register_commands(&self, ctx: &Context) -> anyhow::Result<()> {
        let thread = ChannelId::new(self.app.settings.thread_id)
            .to_channel(ctx)
            .await?;
        let guild = thread
            .guild()
            .map(|c| c.guild_id)
            .ok_or_else(|| anyhow::anyhow!("thread_id is not a server channel"))?;
        guild
            .set_commands(
                &ctx.http,
                vec![
                    CreateCommand::new("played")
                        .description("Marquer un jeu comme joué, et dire qui y a joué")
                        .add_option(
                            CreateCommandOption::new(CommandOptionType::String, "jeu", "Le jeu")
                                .required(true)
                                .set_autocomplete(true),
                        ),
                    CreateCommand::new("missing").description(
                        "Les jeux que tu n'as pas encore notés (et te renvoyer les demandes)",
                    ),
                ],
            )
            .await?;
        Ok(())
    }

    /// Avatars for the site, fetched once at start.
    async fn load_avatars(&self, ctx: &Context) {
        let mut avatars = self.app.avatars.write().await;
        for (person, id) in &self.app.settings.vote_users {
            match UserId::new(*id).to_user(ctx).await {
                Ok(user) => {
                    avatars.insert(person.clone(), user.face());
                }
                Err(e) => tracing::warn!("avatar of {person}: {e}"),
            }
        }
    }

    // ---- proposals thread ------------------------------------------------

    async fn on_thread_message(&self, ctx: &Context, msg: &Message) -> anyhow::Result<()> {
        if self.app.settings.run_actions.read_thread && self.app.settings.link_ingest.enabled {
            self.ingest_links(ctx, msg).await?;
        }
        self.app
            .state
            .update(|s| {
                s.thread_last_message_id =
                    Some(s.thread_last_message_id.unwrap_or(0).max(msg.id.get()))
            })
            .await?;
        Ok(())
    }

    async fn ingest_links(&self, ctx: &Context, msg: &Message) -> anyhow::Result<()> {
        let settings = &self.app.settings;
        let (links, title) = extract_links(&msg.content);
        let prefixes = &settings.link_ingest.allowed_prefixes;
        let snapshot = self.app.store.current().await;
        let mut new = Vec::new();
        {
            let mut adding = self.adding.lock().await;
            for link in links {
                if !prefixes.is_empty() && !prefixes.iter().any(|p| link.starts_with(p.as_str())) {
                    continue;
                }
                let Some(key) = GameKey::from_link(&link) else {
                    continue;
                };
                if snapshot.game(&key).is_some() || !adding.insert(key.clone()) {
                    continue;
                }
                new.push((key, link));
            }
        }
        if new.is_empty() {
            return Ok(());
        }
        let names: Vec<&str> = [
            msg.member.as_ref().and_then(|m| m.nick.as_deref()),
            msg.author.global_name.as_deref(),
            Some(msg.author.name.as_str()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let proposer = settings.resolve_person(msg.author.id.get(), &names);
        let mut games = Vec::new();
        for (key, link) in &new {
            let info = match key {
                GameKey::Steam(id) => self.app.steam.app(*id).await.unwrap_or_else(|e| {
                    tracing::warn!("Steam app {id}: {e}");
                    None
                }),
                _ => None,
            };
            let name = info
                .as_ref()
                .and_then(|i| i.name.clone())
                .or_else(|| steam_slug_name(link).filter(|_| matches!(key, GameKey::Steam(_))))
                .or_else(|| (!title.is_empty()).then(|| title.clone()))
                .unwrap_or_else(|| link.clone());
            games.push(NewGame {
                key: key.clone(),
                name,
                link: Some(link.clone()),
                proposer: proposer.clone(),
                price: info.and_then(|i| i.price),
            });
        }
        let result = self.app.store.add_games(games).await;
        {
            let mut adding = self.adding.lock().await;
            for (key, _) in &new {
                adding.remove(key);
            }
        }
        result?;
        tracing::info!("{proposer} proposed {} game(s)", new.len());
        let _ = msg.react(&ctx.http, CHECK).await;
        Ok(())
    }

    // ---- grades by DM -----------------------------------------------------

    async fn on_dm(&self, ctx: &Context, msg: &Message) -> anyhow::Result<()> {
        if !self.app.settings.run_actions.read_responses {
            return Ok(());
        }
        let Some(person) = self
            .app
            .settings
            .person_for(msg.author.id.get())
            .map(str::to_string)
        else {
            return Ok(());
        };
        self.handle_reply(ctx, msg, &person).await?;
        self.app
            .state
            .update(|s| {
                let last = s
                    .dm_last_message_id
                    .entry(msg.author.id.to_string())
                    .or_default();
                *last = (*last).max(msg.id.get());
            })
            .await?;
        Ok(())
    }

    async fn handle_reply(&self, ctx: &Context, msg: &Message, person: &str) -> anyhow::Result<()> {
        let Some(parent) = msg.referenced_message.as_deref() else {
            return Ok(());
        };
        if Some(parent.author.id) != self.me() {
            return Ok(());
        }
        let snapshot = self.app.store.current().await;
        let Some((key, phase)) = votes::parse(&parent.content, &snapshot) else {
            return Ok(());
        };
        let s = &self.app.settings;
        let grade = parse_grade(&msg.content).filter(|g| (s.vote_min..=s.vote_max).contains(g));
        let Some(grade) = grade else {
            let _ = msg.react(&ctx.http, THUMBS_DOWN).await;
            msg.reply(
                &ctx.http,
                format!(
                    "Note invalide. Réponds avec un nombre entre {} et {}.",
                    votes::fmt_grade(s.vote_min),
                    votes::fmt_grade(s.vote_max)
                ),
            )
            .await?;
            return Ok(());
        };
        // Replayed on catch-up: the same grade again changes nothing.
        if snapshot.game(&key).and_then(|g| g.grade(phase, person)) == Some(grade) {
            let _ = msg.react(&ctx.http, THUMBS_UP).await;
            return Ok(());
        }
        match self.app.store.set_grade(&key, person, phase, grade).await {
            Ok(change) => {
                let _ = msg.react(&ctx.http, THUMBS_UP).await;
                if let Some(previous) = change.previous {
                    msg.reply(
                        &ctx.http,
                        format!(
                            "Note {} modifiée pour {} : {} → {}",
                            phase.label(),
                            change.game,
                            votes::fmt_grade(previous),
                            votes::fmt_grade(grade)
                        ),
                    )
                    .await?;
                }
                tracing::info!(
                    "{person} graded {} ({}) {grade}",
                    change.game,
                    phase.label()
                );
            }
            Err(e) => {
                let _ = msg.react(&ctx.http, THUMBS_DOWN).await;
                msg.reply(
                    &ctx.http,
                    "Je n'ai pas pu enregistrer ta note, réessaie un peu plus tard.",
                )
                .await?;
                return Err(e);
            }
        }
        Ok(())
    }

    /// Messages that arrived while the bot was offline: thread proposals and
    /// DM replies after the last ones it saw.
    async fn catch_up(&self, ctx: &Context) -> anyhow::Result<()> {
        let state = self.app.state.get().await;
        let thread = ChannelId::new(self.app.settings.thread_id);
        if let Some(after) = state.thread_last_message_id {
            for msg in history(&ctx.http, thread, after).await? {
                if !msg.author.bot {
                    self.on_thread_message(ctx, &msg).await?;
                }
            }
        }
        for (person, id) in &self.app.settings.vote_users {
            let Some(after) = state.dm_last_message_id.get(&id.to_string()).copied() else {
                continue;
            };
            let dm = match UserId::new(*id).create_dm_channel(ctx).await {
                Ok(dm) => dm,
                Err(e) => {
                    tracing::warn!("DM channel of {person}: {e}");
                    continue;
                }
            };
            for msg in history(&ctx.http, dm.id, after).await? {
                if msg.author.id.get() == *id {
                    self.on_dm(ctx, &msg).await?;
                }
            }
        }
        Ok(())
    }

    // ---- slash commands --------------------------------------------------

    async fn autocomplete(&self, ctx: &Context, cmd: &CommandInteraction) -> anyhow::Result<()> {
        let typed = cmd
            .data
            .autocomplete()
            .map(|o| o.value.to_lowercase())
            .unwrap_or_default();
        let snapshot = self.app.store.current().await;
        let mut response = CreateAutocompleteResponse::new();
        // Unplayed games first: those are the ones being marked.
        let mut games: Vec<_> = snapshot
            .games
            .iter()
            .filter(|g| g.name.to_lowercase().contains(&typed))
            .collect();
        games.sort_by_key(|g| (g.played, g.name.to_lowercase()));
        for g in games.into_iter().take(25) {
            let label = if g.played {
                format!("{} (déjà joué)", g.name)
            } else {
                g.name.clone()
            };
            response = response.add_string_choice(truncate(&label, 100), g.slug.clone());
        }
        cmd.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(response))
            .await?;
        Ok(())
    }

    async fn cmd_played(&self, ctx: &Context, cmd: &CommandInteraction) -> anyhow::Result<()> {
        let slug = cmd
            .data
            .options
            .iter()
            .find(|o| o.name == "jeu")
            .and_then(|o| match &o.value {
                CommandDataOptionValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let snapshot = self.app.store.current().await;
        let game = snapshot.games.iter().find(|g| g.slug == slug).or_else(|| {
            snapshot
                .games
                .iter()
                .find(|g| g.name.eq_ignore_ascii_case(&slug))
        });
        let message = match game {
            None => CreateInteractionResponseMessage::new()
                .content("Je ne trouve pas ce jeu : choisis-le dans la liste proposée.")
                .ephemeral(true),
            Some(game) => {
                let me = self.app.settings.person_for(cmd.user.id.get());
                CreateInteractionResponseMessage::new()
                    .content(format!("Qui a joué à **{}** ?", game.name))
                    .components(vec![players_menu(&game.slug, &snapshot, me)])
                    .ephemeral(true)
            }
        };
        cmd.create_response(&ctx.http, CreateInteractionResponse::Message(message))
            .await?;
        Ok(())
    }

    async fn on_players_chosen(
        &self,
        ctx: &Context,
        comp: &ComponentInteraction,
    ) -> anyhow::Result<()> {
        let ComponentInteractionDataKind::StringSelect { values } = &comp.data.kind else {
            return Ok(());
        };
        let slug = comp
            .data
            .custom_id
            .trim_start_matches("played:")
            .to_string();
        comp.create_response(&ctx.http, CreateInteractionResponse::Acknowledge)
            .await?;
        let snapshot = self.app.store.current().await;
        let Some(game) = snapshot.games.iter().find(|g| g.slug == slug) else {
            comp.edit_response(
                &ctx.http,
                EditInteractionResponse::new().content("Ce jeu n'est plus dans la feuille."),
            )
            .await?;
            return Ok(());
        };
        let key = game.key.clone();
        let players: Vec<String> = values.clone();
        let name = self.app.store.mark_played(&key, &players).await?;
        self.app
            .state
            .update(|s| {
                s.played_known.insert(key.to_string());
            })
            .await?;
        let who = players.join(", ");
        let edit = EditInteractionResponse::new()
            .content(format!(
                "✅ **{name}** marqué joué par {who}. Je leur demande leur note après."
            ))
            .components(vec![]);
        comp.edit_response(&ctx.http, edit).await?;
        // A `/played` menu is only visible to its author, so the thread gets
        // told; a "who played?" prompt in the thread already says it once edited.
        let ephemeral = comp
            .message
            .flags
            .is_some_and(|f| f.contains(serenity::all::MessageFlags::EPHEMERAL));
        if ephemeral {
            let _ = ChannelId::new(self.app.settings.thread_id)
                .send_message(
                    &ctx.http,
                    CreateMessage::new().content(format!("🎮 **{name}** joué par {who}.")),
                )
                .await;
        }
        let snapshot = self.app.store.current().await;
        if let Some(game) = snapshot.game(&key) {
            for person in &players {
                if game.grade(Phase::After, person).is_none() {
                    self.send_request(&ctx.http, &snapshot, &key, person, Phase::After)
                        .await;
                }
            }
        }
        Ok(())
    }

    async fn cmd_missing(&self, ctx: &Context, cmd: &CommandInteraction) -> anyhow::Result<()> {
        let Some(person) = self
            .app
            .settings
            .person_for(cmd.user.id.get())
            .map(str::to_string)
        else {
            cmd.create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Tu n'es pas dans la liste des votants.")
                        .ephemeral(true),
                ),
            )
            .await?;
            return Ok(());
        };
        cmd.defer_ephemeral(&ctx.http).await?;
        let snapshot = self.app.store.current().await;
        let missing = missing_for(&snapshot, &person);
        let content = if missing.is_empty() {
            "Tu as tout noté 🎉".to_string()
        } else {
            let limit = self.app.settings.reminder_limit(&person).unwrap_or(10) as usize;
            let mut text = format!("Il te manque {} note(s) :\n", missing.len());
            for (key, phase) in missing.iter().take(20) {
                let name = snapshot.game(key).map(|g| g.name.as_str()).unwrap_or("?");
                text += &format!("• {name} ({})\n", phase.label());
            }
            if missing.len() > 20 {
                text += "…\n";
            }
            let sent = missing.len().min(limit);
            for (key, phase) in missing.iter().take(sent) {
                self.send_request(&ctx.http, &snapshot, key, &person, *phase)
                    .await;
            }
            text + &format!("Je t'envoie les {sent} premières demandes en DM.")
        };
        cmd.edit_response(
            &ctx.http,
            EditInteractionResponse::new().content(truncate(&content, 1900)),
        )
        .await?;
        Ok(())
    }

    async fn send_request(
        &self,
        http: &Http,
        snapshot: &Snapshot,
        key: &GameKey,
        person: &str,
        phase: Phase,
    ) {
        if let Err(e) = send_request(&self.app, http, snapshot, key, person, phase).await {
            tracing::warn!("vote request to {person}: {e}");
        }
    }
}

/// The grades a person still owes: before playing on unplayed games, after
/// playing on the games they played. In sheet order, like the reminders.
pub fn missing_for(snapshot: &Snapshot, person: &str) -> Vec<(GameKey, Phase)> {
    let Some(p) = snapshot.person(person) else {
        return vec![];
    };
    let mut out = Vec::new();
    for g in &snapshot.games {
        if !g.played {
            if g.grade(Phase::Before, person).is_none() {
                out.push((g.key.clone(), Phase::Before));
            }
        } else if p.col_after.is_some()
            && g.grade(Phase::After, person).is_none()
            && snapshot
                .players_of(&g.key)
                .is_some_and(|ps| ps.iter().any(|x| x == person))
        {
            out.push((g.key.clone(), Phase::After));
        }
    }
    out
}

pub async fn send_request(
    app: &App,
    http: &Http,
    snapshot: &Snapshot,
    key: &GameKey,
    person: &str,
    phase: Phase,
) -> anyhow::Result<()> {
    let s = &app.settings;
    let id = *s
        .vote_users
        .get(person)
        .ok_or_else(|| anyhow::anyhow!("{person} is not in vote_users"))?;
    let game = snapshot
        .game(key)
        .ok_or_else(|| anyhow::anyhow!("{key} is gone"))?;
    let display = match (&game.link, s.prefer_link_in_reminders) {
        (Some(link), true) => link.clone(),
        _ => game.name.clone(),
    };
    let text = votes::request(&display, key, phase, s.vote_min, s.vote_max);
    UserId::new(id)
        .direct_message(http, CreateMessage::new().content(text))
        .await?;
    Ok(())
}

/// The people menu of `/played`: whoever runs it is preselected.
fn players_menu(slug: &str, snapshot: &Snapshot, me: Option<&str>) -> CreateActionRow {
    let people: BTreeSet<&str> = snapshot.people.iter().map(|p| p.name.as_str()).collect();
    let options: Vec<CreateSelectMenuOption> = people
        .iter()
        .map(|name| CreateSelectMenuOption::new(*name, *name).default_selection(Some(*name) == me))
        .collect();
    let n = options.len().max(1) as u8;
    CreateActionRow::SelectMenu(
        CreateSelectMenu::new(
            format!("played:{slug}"),
            CreateSelectMenuKind::String { options },
        )
        .placeholder("Qui a joué ?")
        .min_values(1)
        .max_values(n),
    )
}

/// Posts "who played?" in the thread for a game someone marked played on the
/// sheet by hand.
pub async fn ask_who_played(
    http: &Http,
    thread: ChannelId,
    snapshot: &Snapshot,
    key: &GameKey,
) -> anyhow::Result<()> {
    let game = snapshot
        .game(key)
        .ok_or_else(|| anyhow::anyhow!("{key} is gone"))?;
    thread
        .send_message(
            http,
            CreateMessage::new()
                .content(format!(
                    "**{}** est marqué joué dans la feuille : qui y a joué ? (pour demander les notes après)",
                    game.name
                ))
                .components(vec![players_menu(&game.slug, snapshot, None)]),
        )
        .await?;
    Ok(())
}

/// Every message of a channel after `after`, oldest first.
async fn history(http: &Http, channel: ChannelId, after: u64) -> anyhow::Result<Vec<Message>> {
    let mut out: Vec<Message> = Vec::new();
    let mut cursor = after;
    loop {
        let mut page = channel
            .messages(
                http,
                GetMessages::new().after(MessageId::new(cursor)).limit(100),
            )
            .await?;
        page.sort_by_key(|m| m.id);
        let full = page.len() == 100;
        if let Some(last) = page.last() {
            cursor = last.id.get();
        }
        out.extend(page);
        if !full {
            break;
        }
    }
    Ok(out)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max - 1).collect::<String>() + "…"
}
