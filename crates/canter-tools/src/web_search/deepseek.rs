//! DeepSeek Search API adapter. Credentials never enter errors or Debug output.
use super::{SearchProvider, WebSearchResult};
use serde::Deserialize;
use serde_json::json;

const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Deserialize)]
struct Response {
    results: Vec<WebSearchResult>,
}

pub struct DeepSeek {
    api_key: String,
    endpoint: reqwest::Url,
}

impl DeepSeek {
    /// The server supplies the fixed official endpoint; local replay tests supply a loopback URL.
    pub fn new(api_key: String, endpoint: reqwest::Url) -> Self {
        Self { api_key, endpoint }
    }
}

#[async_trait::async_trait]
impl SearchProvider for DeepSeek {
    async fn search(&self, query: &str, count: u32) -> Result<Vec<WebSearchResult>, String> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .user_agent("canter/0.1 web_search")
            .build()
            .map_err(|_| "Could not create web_search client")?;
        let mut response = client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .json(&json!({"query":query,"count":count}))
            .send()
            .await
            .map_err(|_| "web_search request failed (connection or TLS error)")?;
        // Never expose response bodies or reqwest errors: either may contain credentials.
        match response.status().as_u16() {
            200 => {}
            401 => return Err("web_search authentication failed (401); check the API key".into()),
            402 => {
                return Err(
                    "web_search has insufficient balance (402); top up the account before retrying"
                        .into(),
                );
            }
            429 => {
                return Err(
                    "web_search rate limit exceeded (429); wait before trying again".into(),
                );
            }
            status => return Err(format!("web_search HTTP error: {status}")),
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err("web_search response exceeds 5 MiB".into());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "web_search response body failed")?
        {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err("web_search response exceeds 5 MiB".into());
            }
            body.extend_from_slice(&chunk);
        }
        let mut response: Response = serde_json::from_slice(&body)
            .map_err(|_| "web_search returned an invalid response format")?;
        // Keep even a provider echo of the credential out of model/client output and storage.
        if !self.api_key.is_empty() {
            for result in &mut response.results {
                for text in [&mut result.title, &mut result.url, &mut result.summary]
                    .into_iter()
                    .chain(result.date.iter_mut())
                {
                    *text = text.replace(&self.api_key, "[redacted]");
                }
            }
        }
        Ok(response.results)
    }
}
