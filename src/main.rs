mod database;

use anyhow::Context as _;
use chrono::{Datelike, Utc};
use database::{AppStats, FileStore, NameQueryResult, UsacoDb};
use poise::{
    builtins::HelpConfiguration, serenity_prelude as serenity, serenity_prelude::CreateAttachment,
    CreateReply, FrameworkError,
};
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
use usaco_standings_scraper::{Division, Graduation, IntlMedal, Month};

/// Format a [`NameQueryResult`] as a string to display to users. If
/// `hide_name`, all names will be hidden.
///
/// This function guarantees that the number of lines in the resulting string
/// will be equal regardless of `hide_name`.
fn format_name_query_result(
    result: &NameQueryResult,
    search_name: &str,
    hide_name: bool,
) -> String {
    fn fmt_month(month: Month) -> &'static str {
        match month {
            Month::November => "nov",
            Month::December => "dec",
            Month::January => "jan",
            Month::February => "feb",
            Month::March => "mar",
            Month::Open => "open",
        }
    }

    fn fmt_division(division: Division) -> &'static str {
        match division {
            Division::Bronze => "bronze",
            Division::Silver => "silver",
            Division::Gold => "gold",
            Division::Platinum => "platinum",
        }
    }

    let mut out = String::new();

    macro_rules! outln {
        ($($tt:tt)*) => {{
            use std::fmt::Write;

            writeln!(out, $($tt)*).expect("writing to a string should not fail");
        }}
    }

    outln!(
        "A total of {} USACO record(s) with name {} found.",
        result.participants.len(),
        if hide_name {
            "[name hidden]"
        } else {
            search_name
        }
    );
    outln!();

    for p in &result.participants {
        outln!(
            "{name} from {country} {grade}. Results:",
            name = if hide_name {
                "[name hidden]"
            } else {
                &p.id.name
            },
            country = &p.id.country,
            grade = match p.id.graduation {
                Graduation::HighSchool { year } => format!("with graduation year {year}"),
                Graduation::Observer => "as an observer".to_string(),
            }
        );

        for c in &p.contests {
            let season = c.contest_time.year
                + if matches!(c.contest_time.month, Month::November | Month::December) {
                    1
                } else {
                    0
                };
            let grade = match p.id.graduation {
                Graduation::HighSchool { year } => Some(12 - (year as i32 - season as i32)),
                Graduation::Observer => None,
            };

            outln!(
                "Scored {score} on {month} {year} {division} {grade}",
                score = c.score,
                month = fmt_month(c.contest_time.month),
                year = c.contest_time.year,
                division = fmt_division(c.division),
                grade = match grade {
                    Some(grade) => format!("in grade {grade}"),
                    None => "as an observer".to_string(),
                }
            );
        }

        for c in &p.camps {
            let graduation = match p.id.graduation {
                Graduation::HighSchool { year } => year,
                Graduation::Observer => {
                    warn!("camp record from an observer {:?}", p.id);
                    9999
                }
            };
            let grade = 12 - (graduation as i32 - c.camp_year as i32);

            outln!("Camped in {} in grade {grade}", c.camp_year);
        }
        outln!();
    }

    for (comp, records) in [("IOI", &result.ioi), ("EGOI", &result.egoi)] {
        if records.is_empty() {
            continue;
        }

        outln!("Found {comp} records:");

        for r in records {
            match r.result {
                IntlMedal::VisaIssue => outln!(
                    "qualified for {comp} {} but did not attend due to visa issues",
                    r.year
                ),
                IntlMedal::NoMedal => outln!("competed at {comp} {}", r.year),
                IntlMedal::Bronze => outln!("bronze medal at {comp} {}", r.year),
                IntlMedal::Silver => outln!("silver medal at {comp} {}", r.year),
                IntlMedal::Gold => outln!("gold medal at {comp} {}", r.year),
            }
        }

        outln!();
    }

    out.trim().to_string()
}

struct AppData {
    db: &'static Mutex<UsacoDb>,
    stats: &'static Mutex<AppStats>,
    /// Start of this bot process, used to calculate uptime
    start: Instant,
    application_info: CurrentApplicationInfo,
}

type Context<'a> = poise::Context<'a, AppData, anyhow::Error>;

/// Shows this help menu
#[poise::command(prefix_command, slash_command)]
async fn help(
    ctx: Context<'_>,
    #[description = "Specific command to show help about"]
    #[autocomplete = "poise::builtins::autocomplete_command"]
    command: Option<String>,
) -> anyhow::Result<()> {
    poise::builtins::help(
        ctx,
        command.as_deref(),
        HelpConfiguration {
            extra_text_at_bottom: "Use /help <command> for more info on a specific command",
            ..Default::default()
        },
    )
    .await?;

    Ok(())
}

/// Invite the bot to your server!
#[poise::command(prefix_command, slash_command, ephemeral)]
async fn invite(ctx: Context<'_>) -> anyhow::Result<()> {
    ctx.say("https://discord.com/api/oauth2/authorize?client_id=758792251496333392&permissions=10304&scope=bot").await?;

    Ok(())
}

/// Check the bot's latency
#[poise::command(prefix_command, slash_command)]
async fn ping(ctx: Context<'_>) -> anyhow::Result<()> {
    let now = Instant::now();

    let msg = ctx.say(":ping_pong:").await?;
    msg.edit(
        ctx,
        CreateReply::default().content(format!(
            ":ping_pong:\nroundtrip: {}ms\ngateway: {}ms",
            now.elapsed().as_millis(),
            ctx.ping().await.as_millis()
        )),
    )
    .await?;

    Ok(())
}

/// Lookup USACO records for a given name
///
/// Use slash commands if you want names in result to be hidden, or for the \
/// result to be only visible to you.
///
/// Note that recent bronze and silver promotions may not be reported since \
/// USACO stopped releasing them.
///
/// The bot will update its response if you edit your command, and the bot \
/// will delete its response if you delete your message.
#[poise::command(prefix_command, slash_command, track_edits)]
async fn search(
    ctx: Context<'_>,
    #[flag]
    #[description = "Hide name in response"]
    mut hide_name: bool,
    #[description = "Should result only be shown to you? (slash command only)"] private: Option<
        bool,
    >,
    #[rest]
    #[description = "Full name to look up (case-insensitive)"]
    mut name: String,
) -> anyhow::Result<()> {
    {
        let new_query = match ctx {
            // avoid double counting caused by edit tracking
            Context::Prefix(pref) => pref.msg.edited_timestamp.is_none(),
            _ => true,
        };

        if new_query {
            let mut stats = ctx.data().stats.lock().await;

            stats.query_count += 1;
            *stats.users_queried.entry(ctx.author().id).or_default() += 1;
        }
    }

    let private = private.unwrap_or_default();

    // poise won't parse something like `s;search john doe +hide`, but we can just
    // deal with this manually.
    if name.contains("+hide") {
        hide_name = true;
        name = name.replace("+hide", "");
    }

    // we should be safe against any response hijacking, since we shouldn't be able
    // to ping anyone in our embeds, but let's still do this just to be safe.
    name = name.replace('`', "");

    let res = ctx.data().db.lock().await.query_name(&name);
    let res = format_name_query_result(&res, &name, hide_name);

    // max length of embed description is 4096
    if res.len() <= 4000 {
        let mut embed = CreateEmbed::new()
            .title("USACO Standings Search Result")
            .color(Color::BLUE)
            .description(format!("```{res}```",));

        if name.to_lowercase().starts_with("name") {
            embed = embed.footer(CreateEmbedFooter::new(
                r#"hint: this command was recently refactored. perhaps you wanted to do s;search <name>, for example "s;search benjamin qi". alternatively, use /search"#,
            ));
        }

        ctx.send(CreateReply::default().embed(embed).ephemeral(private))
            .await?;
    } else {
        ctx.send(
            CreateReply::default()
                .attachment(CreateAttachment::bytes(res, "result.txt"))
                .ephemeral(private),
        )
        .await?;
    }

    // TODO: implement name hiding with prefix commands properly
    // if hide_name {
    //     if let Context::Prefix(pref) = ctx {
    //         pref.msg.delete(ctx.http()).await.ok();
    //     }
    // }

    Ok(())
}

/// Lists bot statistics
#[poise::command(prefix_command, slash_command)]
async fn botinfo(ctx: Context<'_>) -> anyhow::Result<()> {
    let (bot_name, bot_face) = {
        let bot = ctx.cache().current_user();
        (bot.name.clone(), bot.face().clone())
    };

    let data = ctx.data();
    let db = data.db.lock().await;
    let stats = data.stats.lock().await;

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
        .field("Queries Made", stats.query_count.to_string(), true)
        .field("Users Queried", stats.users_queried.len().to_string(), true)
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
                ("USACO Records", db.people_count()),
                ("USACO Contest Records", db.contest_count()),
                ("USACO Camp Records", db.camp_count()),
                ("IOI Records", db.ioi_people_count()),
                ("IOI Contest Records", db.ioi_records_count()),
                ("EGOI Records", db.egoi_people_count()),
                ("EGOI Contest Records", db.egoi_records_count()),
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

    drop(db);
    drop(stats);
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

    *ctx.data().db.lock().await = data.into();

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
        commands: vec![help(), invite(), ping(), search(), botinfo(), update()],
        prefix_options: poise::PrefixFrameworkOptions {
            prefix: Some("s;".into()),
            edit_tracker: Some(Arc::new(poise::EditTracker::for_timespan(
                Duration::from_secs(60 * 60),
            ))),
            mention_as_prefix: true,
            execute_self_messages: false,
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
        ..Default::default()
    };

    let framework = poise::Framework::builder()
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                info!("Logged in as {}", ready.user.name);

                poise::builtins::register_globally(ctx, &framework.options().commands).await?;

                ctx.set_activity(Some(ActivityData::custom("s;help for usage!")));

                let data = AppData {
                    db: Box::leak(Box::new(Mutex::new(store_data.db))),
                    stats: Box::leak(Box::new(Mutex::new(store_data.stats))),
                    start: Instant::now(),
                    application_info: ctx.http.get_current_application_info().await?,
                };
                let db = data.db;
                let stats = data.stats;

                // save data every 5 minutes. for now, it's ok to lose the last 5 minutes of
                // data in the case of a shutdown.
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(5 * 60));

                    loop {
                        interval.tick().await;

                        // a bit unfortunate that the guards for `data` are held while waiting
                        // for the filesystem, but it probably doesn't really matter
                        if let Err(e) = filestore.save_db(&*db.lock().await).await {
                            warn!("failed to save db to database: {e:?}");
                        }
                        if let Err(e) = filestore.save_stats(&*stats.lock().await).await {
                            warn!("failed to save stats to database: {e:?}");
                        }
                    }
                });

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
