mod database;

use anyhow::Context as _;
use chrono::{Datelike, Utc};
use database::{AppStats, FileStore, UsacoDb};
use poise::{serenity_prelude as serenity, CreateReply, FrameworkError};
use reqwest::{Client, StatusCode, Url};
use serenity::{
    ActivityData, Color, CreateAllowedMentions, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter,
    CurrentApplicationInfo, GatewayIntents,
};
use std::{
    env,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, Mutex};
use tracing::{error, info, warn};

struct AppData {
    db: UsacoDb,
    stats: AppStats,
    /// Start of this bot process, used to calculate uptime
    start: Instant,
    application_info: CurrentApplicationInfo,
}

/// Bot data passed to all commands.
type Data = Arc<Mutex<AppData>>;

type Context<'a> = poise::Context<'a, Data, anyhow::Error>;

/// Shows this help menu
#[poise::command(prefix_command, slash_command)]
async fn help(
    ctx: Context<'_>,
    #[description = "Specific command to show help about"]
    #[autocomplete = "poise::builtins::autocomplete_command"]
    command: Option<String>,
) -> anyhow::Result<()> {
    poise::builtins::help(ctx, command.as_deref(), Default::default()).await?;

    Ok(())
}

/// Invite the bot to your server!
#[poise::command(prefix_command, slash_command)]
async fn invite(ctx: Context<'_>) -> anyhow::Result<()> {
    ctx.say("https://discord.com/api/oauth2/authorize?client_id=758792251496333392&permissions=10304&scope=bot").await?;

    Ok(())
}

/// Check the bot's latency
#[poise::command(prefix_command, slash_command)]
async fn ping(ctx: Context<'_>) -> anyhow::Result<()> {
    let now = Instant::now();

    let msg = ctx.say(":ping_pong:!").await?;
    msg.edit(
        ctx,
        CreateReply::default().content(format!(
            ":ping_pong:!\nroundtrip: {}ms\ngateway: {}ms",
            now.elapsed().as_millis(),
            ctx.ping().await.as_millis()
        )),
    )
    .await?;

    Ok(())
}

/// Lists bot statistics
#[poise::command(prefix_command, slash_command)]
async fn botinfo(ctx: Context<'_>) -> anyhow::Result<()> {
    let (bot_name, bot_face) = {
        let bot = ctx.cache().current_user();
        (bot.name.clone(), bot.face().clone())
    };

    let data = ctx.data().lock().await;

    let embed = CreateEmbed::new()
        .description(&data.application_info.description)
        .color(Color::BLUE)
        .author(CreateEmbedAuthor::new(bot_name).icon_url(bot_face.clone()))
        .thumbnail(bot_face)
        .field(
            "Uptime",
            readable::up::UptimeFull::from(data.start.elapsed()).to_string(),
            true,
        )
        .field("Queries Made", data.stats.query_count.to_string(), true)
        .field(
            "Users Queried",
            data.stats.users_queried.len().to_string(),
            true,
        )
        .field("Server Count", ctx.cache().guild_count().to_string(), true)
        .field(
            "User Count",
            ctx.cache()
                .guilds()
                .iter()
                .map(|id| {
                    ctx.cache()
                        .guild(id)
                        .map(|g| g.member_count)
                        .unwrap_or_default()
                })
                .sum::<u64>()
                .to_string(),
            true,
        )
        .fields(
            [
                ("USACO Records", data.db.people_count()),
                ("USACO Contest Records", data.db.contest_count()),
                ("USACO Camp Records", data.db.camp_count()),
                ("IOI Records", data.db.ioi_people_count()),
                ("IOI Contest Records", data.db.ioi_records_count()),
                ("EGOI Records", data.db.egoi_people_count()),
                ("EGOI Contest Records", data.db.egoi_records_count()),
            ]
            .into_iter()
            .map(|(k, v)| (k, v.to_string(), true)),
        )
        .footer(
            CreateEmbedFooter::new(format!(
                "Made by {}",
                data.application_info
                    .owner
                    .as_ref()
                    .and_then(|u| u.global_name.clone())
                    .unwrap_or_else(|| "???".to_string())
            ))
            .icon_url(
                data.application_info
                    .owner
                    .as_ref()
                    .and_then(|u| u.avatar_url())
                    .unwrap_or_default(),
            ),
        );

    ctx.send(CreateReply::default().embed(embed)).await?;

    Ok(())
}

/// Update the USACO standings database
#[poise::command(prefix_command, owners_only, hide_in_help)]
async fn update(ctx: Context<'_>) -> anyhow::Result<()> {
    /// Current progress of the parsing
    struct Progress {
        max_year: u16,
        parsed: u32,
        total: u32,
    }

    impl Progress {
        fn get_message(&self, ctx: Context<'_>, finished: bool) -> CreateReply {
            CreateReply::default().embed(
                CreateEmbed::new()
                    .description(format!("Parsing for years up to {}", self.max_year))
                    .color(Color::BLUE)
                    .author({
                        let user = ctx.cache().current_user();
                        CreateEmbedAuthor::new(user.name.clone()).icon_url(user.face())
                    })
                    .field(
                        "Status",
                        if finished { "Finished" } else { "Parsing" },
                        true,
                    )
                    .field(
                        "Parsed",
                        format!(
                            "{}/{} ({:.0}%)",
                            self.parsed,
                            self.total,
                            self.parsed as f64 / self.total as f64 * 100.
                        ),
                        true,
                    ),
            )
        }
    }

    struct HttpClient {
        client: Client,
        progress: Arc<Mutex<Progress>>,
    }

    impl usaco_standings_scraper::HttpClient for HttpClient {
        type Error = reqwest::Error;
        type Future =
            Pin<Box<dyn Future<Output = Result<(StatusCode, String), Self::Error>> + Send>>;

        fn get(&mut self, url: Url) -> Self::Future {
            let client = self.client.clone();
            let progress = self.progress.clone();

            Box::pin(async move {
                progress.lock().await.total += 1;

                let r = client.get(url).send().await?;

                let status = r.status();
                let text = r.text().await?;

                progress.lock().await.parsed += 1;

                Ok((status, text))
            })
        }
    }

    let now = Utc::now();
    let max_year = now.year() + if now.month() >= 10 { 1 } else { 0 };
    let max_year = max_year.try_into().expect("year shouldn't over/underflow");

    let progress = Arc::new(Mutex::new(Progress {
        max_year,
        parsed: 0,
        total: 0,
    }));
    let client = HttpClient {
        client: Client::new(),
        progress: progress.clone(),
    };

    let msg = ctx
        .send(progress.lock().await.get_message(ctx, false))
        .await?;

    let (tx, mut rx) = oneshot::channel();
    tokio::spawn(async move {
        tx.send(usaco_standings_scraper::parse_all(max_year, client).await)
            .expect("channel should always receive");
    });

    let mut interval = tokio::time::interval(Duration::from_secs(1));

    let data = loop {
        if let Ok(res) = rx.try_recv() {
            break res?;
        }
        interval.tick().await;
        msg.edit(ctx, progress.lock().await.get_message(ctx, false))
            .await?;
    };

    msg.edit(ctx, progress.lock().await.get_message(ctx, true))
        .await?;

    ctx.data().lock().await.db = data.into();

    ctx.say(format!(
        "Successfully finished parsing in {:.2} seconds!",
        (Utc::now() - now).num_milliseconds() as f64 / 1000.
    ))
    .await?;

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let store_path = env::var("FILE_STORE_PATH").context("looking for filestore path")?;
    let mut filestore = FileStore::new_path(store_path.parse()?);
    let store_data = filestore.load().await;

    let options = poise::FrameworkOptions {
        commands: vec![help(), invite(), ping(), botinfo(), update()],
        prefix_options: poise::PrefixFrameworkOptions {
            prefix: Some("s;".into()),
            edit_tracker: Some(Arc::new(poise::EditTracker::for_timespan(
                Duration::from_secs(60 * 60),
            ))),
            ..Default::default()
        },
        allowed_mentions: Some(CreateAllowedMentions::new().empty_users().empty_roles()),
        on_error: |err: FrameworkError<'_, _, anyhow::Error>| {
            Box::pin(async {
                if let Err(e) = match err {
                    FrameworkError::UnknownCommand { ctx, msg, .. } => {
                        msg.channel_id.say(
                            &ctx.http,
                            r#"Unrecognized command. Type "s;help" to view all valid commands on how to use this bot."#,
                        ).await.map(|_| ())
                    }
                    e => poise::builtins::on_error(e).await,
                } {
                    error!("{e:?}");
                }
            })
        },
        command_check: Some(|ctx| {
            Box::pin(async move { Ok(ctx.author().id != ctx.cache().current_user().id) })
        }),
        ..Default::default()
    };

    let framework = poise::Framework::builder()
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                info!("Logged in as {}", ready.user.name);

                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                // dev server
                poise::builtins::register_in_guild::<(), ()>(
                    ctx.http.clone(),
                    &[],
                    777017381167038474.into(),
                )
                .await?;

                ctx.set_activity(Some(ActivityData::custom("s;help for usage!")));

                let data = AppData {
                    db: store_data.db,
                    stats: store_data.stats,
                    start: Instant::now(),
                    application_info: ctx.http.get_current_application_info().await?,
                };
                let data = Arc::new(Mutex::new(data));

                // save data every 5 minutes. for now, it's ok to lose the last 5 minutes of
                // data in the case of a shutdown.
                {
                    let data = data.clone();
                    tokio::spawn(async move {
                        let mut interval = tokio::time::interval(Duration::from_secs(5));

                        loop {
                            interval.tick().await;

                            let data = data.lock().await;

                            if let Err(e) = filestore.save_db(&data.db).await {
                                warn!("failed to save db to database: {e:?}");
                            }
                            if let Err(e) = filestore.save_stats(&data.stats).await {
                                warn!("failed to save stats to database: {e:?}");
                            }
                        }
                    });
                }

                Ok(data)
            })
        })
        .options(options)
        .build();

    let token = env::var("DISCORD_TOKEN").context("looking for discord token")?;
    let intents = GatewayIntents::MESSAGE_CONTENT | GatewayIntents::non_privileged();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await?;

    client.start().await?;

    Ok(())
}
