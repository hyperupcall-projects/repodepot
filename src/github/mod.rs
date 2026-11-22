//! GitHub API access (GraphQL).

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;

const GRAPHQL_URL: &str = "https://api.github.com/graphql";

pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("depository/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .context("couldn't create the HTTP client")
}

#[derive(Debug, Clone, Deserialize)]
pub struct Viewer {
    pub login: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimit {
    pub limit: u64,
    pub remaining: u64,
    pub reset_at: String,
}

#[derive(Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
}

pub async fn graphql<T: DeserializeOwned>(
    http: &reqwest::Client,
    token: &str,
    query: &str,
    variables: serde_json::Value,
) -> Result<T> {
    let response = http
        .post(GRAPHQL_URL)
        .bearer_auth(token)
        .json(&serde_json::json!({ "query": query, "variables": variables }))
        .send()
        .await
        .context("couldn't reach GitHub")?;

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("GitHub rejected the token (401 Unauthorized). It may be expired or revoked.");
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("GitHub returned {status}: {}", first_chars(&body, 300));
    }

    let body: GraphqlResponse<T> = response
        .json()
        .await
        .context("GitHub sent a response this app doesn't understand")?;
    let messages = body
        .errors
        .iter()
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    match body.data {
        Some(data) => {
            if !messages.is_empty() {
                tracing::warn!("GitHub returned partial data with errors: {messages}");
            }
            Ok(data)
        }
        None if !messages.is_empty() => Err(anyhow!("GitHub returned an error: {messages}")),
        None => Err(anyhow!("GitHub returned no data")),
    }
}

/// Who the token belongs to, plus the current API budget.
pub async fn whoami(http: &reqwest::Client, token: &str) -> Result<(Viewer, RateLimit)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Data {
        viewer: Viewer,
        rate_limit: RateLimit,
    }
    let data: Data = graphql(
        http,
        token,
        "query { viewer { login name } rateLimit { limit remaining resetAt } }",
        serde_json::json!({}),
    )
    .await?;
    Ok((data.viewer, data.rate_limit))
}

fn first_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}
