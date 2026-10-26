use crate::{
    MAX_CLIENT_OUTPUT_BYTES, MAX_MODEL_OUTPUT_BYTES, ToolOutcome, output::BoundedText, parse,
};
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MAX_DOWNLOAD_BYTES: usize = 5 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    url: String,
}

pub(crate) fn prepare(arguments: &Value) -> Result<Url, String> {
    let args: Args = parse(arguments)?;
    if !args.url.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    }) {
        return Err("URL must use http:// or https://".into());
    }
    let url = Url::parse(&args.url).map_err(|_| "Invalid HTTP(S) URL")?;
    validate_url(&url)?;
    Ok(url)
}

fn validate_url(url: &Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("URL must use http:// or https://".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL credentials are not supported".into());
    }
    Ok(())
}

pub(crate) fn authority(url: &Url) -> String {
    format!(
        "{}:{}",
        url.host_str().expect("validated host"),
        url.port_or_known_default().expect("HTTP(S) port")
    )
}

pub(crate) async fn run(url: &Url, cancel: &CancellationToken) -> (String, String, ToolOutcome) {
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(("web_fetch cancelled".into(), ToolOutcome::Cancelled)),
        result = tokio::time::timeout(TIMEOUT, fetch(url)) => match result {
            Err(_) => Err(("web_fetch timed out after 30 seconds".into(), ToolOutcome::TimedOut)),
            Ok(Err(error)) => Err((error, ToolOutcome::Failed)),
            Ok(Ok(text)) => Ok(text),
        },
    };
    match result {
        Ok(text) => {
            let mut model = BoundedText::new(MAX_MODEL_OUTPUT_BYTES);
            let mut client = BoundedText::new(MAX_CLIENT_OUTPUT_BYTES);
            model.push(text.as_bytes());
            client.push(text.as_bytes());
            (
                model.render(false),
                client.render(true),
                ToolOutcome::Completed,
            )
        }
        Err((error, outcome)) => (error.clone(), error, outcome),
    }
}

async fn fetch(url: &Url) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("canter/0.1 web_fetch")
        .build()
        .map_err(|_| "Could not create web_fetch client")?;
    let mut current = url.clone();
    let mut redirects = 0;
    let mut response = loop {
        let response = client
            .get(current.clone())
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|_| "web_fetch request failed (connection or TLS error)")?;
        if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or("Redirect has no valid Location")?;
            let next = current.join(location).map_err(|_| "Invalid redirect URL")?;
            validate_url(&next)?;
            if authority(&next) != authority(url) {
                return Ok(format!("跳到了 {next}"));
            }
            if redirects == MAX_REDIRECTS {
                return Err("web_fetch redirect limit exceeded (5)".into());
            }
            redirects += 1;
            current = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!("web_fetch HTTP error: {}", response.status()));
        }
        break response;
    };
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let html = matches!(content_type.as_str(), "text/html" | "application/xhtml+xml");
    let textual = html
        || content_type.starts_with("text/")
        || content_type == "application/json"
        || (content_type.starts_with("application/") && content_type.ends_with("+json"));
    let length = response.content_length();
    if !textual && let Some(length) = length {
        return Ok(placeholder(&content_type, length));
    }
    if length.is_some_and(|length| length > MAX_DOWNLOAD_BYTES as u64) {
        return Ok(download_limit());
    }
    let mut body = Vec::new();
    let mut downloaded = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "web_fetch body failed")?
    {
        downloaded += chunk.len();
        if downloaded > MAX_DOWNLOAD_BYTES {
            return Ok(download_limit());
        }
        if textual {
            body.extend_from_slice(&chunk);
        }
    }
    if !textual {
        return Ok(placeholder(&content_type, downloaded as u64));
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    if !html {
        return Ok(text);
    }
    tokio::task::spawn_blocking(move || {
        htmd::HtmlToMarkdown::builder()
            .skip_tags(vec!["script", "style", "head"])
            .build()
            .convert(&text)
            .map_err(|_| "HTML conversion failed".to_owned())
    })
    .await
    .map_err(|_| "HTML conversion failed")?
}

fn download_limit() -> String {
    format!("Download exceeds {MAX_DOWNLOAD_BYTES} bytes; content omitted.")
}
fn placeholder(content_type: &str, length: u64) -> String {
    format!("[Content omitted: {content_type}, {length} bytes]")
}
