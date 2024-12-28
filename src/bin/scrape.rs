use chrono::{Datelike, Utc};
use reqwest::{Client, StatusCode, Url};
use std::{future::Future, pin::Pin};

struct HttpClient {
    client: Client,
}

impl usaco_standings_scraper::HttpClient for HttpClient {
    type Error = reqwest::Error;
    type Future = Pin<Box<dyn Future<Output = Result<(StatusCode, String), Self::Error>> + Send>>;

    fn get(&mut self, url: Url) -> Self::Future {
        let client = self.client.clone();

        Box::pin(async move {
            let r = client.get(url).send().await?;

            let status = r.status();
            Ok((status, r.text().await?))
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let client = HttpClient {
        client: Client::new(),
    };

    let now = Utc::now();
    let max_year = now.year() + if now.month() >= 10 { 1 } else { 0 };

    let data = usaco_standings_scraper::parse_all(
        max_year
            .try_into()
            .expect("should not be integer over/underflow"),
        client,
    )
    .await?;
    serde_json::to_writer(std::io::stdout(), &data)?;

    Ok(())
}
