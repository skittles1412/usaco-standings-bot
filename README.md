# USACO Standings Bot

A Discord bot for looking up past USACO results. [Invite the bot to your server!](https://discord.com/api/oauth2/authorize?client_id=758792251496333392&permissions=10304&scope=bot).

## Developers

The scraper and relevant structs live in `usaco-standings-scraper`. You can use the crate by adding the following to your `Cargo.toml`. Be aware that breaking changes may happen at any time, so you might want to lock it [to a specific commit](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#choice-of-commit).
```
usaco-standings-scraper = { git = "https://github.com/skittles1412/usaco-standings-bot.git" }
```
