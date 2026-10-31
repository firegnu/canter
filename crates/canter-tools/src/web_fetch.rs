use crate::{
    MAX_CLIENT_OUTPUT_BYTES, MAX_MODEL_OUTPUT_BYTES, ToolOutcome, output::BoundedText, parse,
};
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, X_USER_DEFINED};
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAX_DOWNLOAD_BYTES: usize = 5 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
/// Default total time for one fetch; tests may pass a shorter one through `Tools`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HTML_DEPTH: usize = 256;
const MAX_HTML_TAG_BYTES: usize = 4096;
const HTML_STACK_BYTES: usize = 256 * 1024 * 1024;
const MAX_TITLE_CHARS: usize = 512;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    url: String,
}

pub(crate) fn prepare(arguments: &Value) -> Result<Url, String> {
    let args: Args = parse(arguments)?;
    validate_url_text(&args.url)?;
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

fn validate_url_text(text: &str) -> Result<(), String> {
    if text
        .chars()
        .any(|c| c == '\\' || c.is_whitespace() || c.is_control())
    {
        return Err("URL must not contain backslashes, whitespace or control characters".into());
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

pub(crate) async fn run(
    url: &Url,
    timeout: Duration,
    cancel: &CancellationToken,
) -> (String, String, ToolOutcome, Option<u64>) {
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(("web_fetch cancelled".into(), ToolOutcome::Cancelled)),
        result = tokio::time::timeout(timeout, fetch(url)) => match result {
            Err(_) => Err((
                format!("web_fetch timed out after {} seconds", timeout.as_secs()),
                ToolOutcome::TimedOut,
            )),
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
                model.original_bytes_if_truncated(),
            )
        }
        Err((error, outcome)) => (error.clone(), error, outcome, None),
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
            validate_url_text(location)?;
            let next = current.join(location).map_err(|_| "Invalid redirect URL")?;
            validate_url(&next)?;
            if authority(&next) != authority(url) {
                return Ok(format!("Redirected to {next}"));
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
    for value in response
        .headers()
        .get_all(reqwest::header::CONTENT_ENCODING)
    {
        let encoding = value.to_str().unwrap_or("invalid");
        if !encoding
            .split(',')
            .all(|part| part.trim().eq_ignore_ascii_case("identity"))
        {
            return Ok(format!(
                "[Content omitted: unsupported Content-Encoding: {encoding}]"
            ));
        }
    }
    let content_type_header = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let charset = content_type_header
        .split(';')
        .skip(1)
        .find_map(|parameter| {
            let (name, value) = parameter.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches(['\'', '"']))
        });
    let content_type = content_type_header
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
    let (mut text, unknown_charset) = decode_text(&body, charset, html);
    let mut title = String::new();
    if html {
        text = match check_html(&text) {
            Err(reason) => {
                format!("[HTML not converted: {reason}; showing raw HTML.]\n\n{text}")
            }
            Ok(()) => {
                let (sender, receiver) = tokio::sync::oneshot::channel();
                // The dedicated stack is a second guard after check_html.
                // Dropping the handle/receiver never waits for the worker.
                let _thread = std::thread::Builder::new()
                    .name("canter-web-fetch-html".into())
                    .stack_size(HTML_STACK_BYTES)
                    .spawn(move || {
                        let title = Arc::new(Mutex::new(String::new()));
                        let captured = title.clone();
                        let result = htmd::HtmlToMarkdown::builder()
                            .skip_tags(vec!["script", "style", "head"])
                            // Visit head only to capture title; discard all its output.
                            .add_handler(
                                vec!["head"],
                                |handlers: &dyn htmd::element_handler::Handlers,
                                 element: htmd::Element| {
                                    handlers.walk_children(element.node);
                                    None
                                },
                            )
                            .add_handler(
                                vec!["title"],
                                move |handlers: &dyn htmd::element_handler::Handlers,
                                      element: htmd::Element| {
                                    let mut title = captured.lock().expect("title lock");
                                    if title.is_empty() {
                                        *title = handlers
                                            .walk_children(element.node)
                                            .content
                                            .split_whitespace()
                                            .collect::<Vec<_>>()
                                            .join(" ")
                                            .chars()
                                            .take(MAX_TITLE_CHARS)
                                            .collect();
                                    }
                                    None
                                },
                            )
                            .build()
                            .convert(&text)
                            .map(|body| (body, title.lock().expect("title lock").clone()))
                            .map_err(|_| "HTML conversion failed".to_owned());
                        let _ = sender.send(result);
                    })
                    .map_err(|_| "Could not start HTML conversion thread")?;
                let (body, page_title) = receiver.await.map_err(|_| "HTML conversion failed")??;
                title = page_title;
                body
            }
        };
    }
    let text = match unknown_charset {
        Some(charset) => format!("[Unknown charset: {charset}; decoded as UTF-8.]\n\n{text}"),
        None => text,
    };
    Ok(if title.is_empty() {
        text
    } else {
        format!("Title: {title}\n\n{text}")
    })
}

fn decode_text(body: &[u8], charset: Option<&str>, html: bool) -> (String, Option<String>) {
    if let Some((encoding, bom_len)) = Encoding::for_bom(body) {
        let (text, _) = encoding.decode_without_bom_handling(&body[bom_len..]);
        return (text.into_owned(), None);
    }
    let from_meta = charset.is_none() && html;
    let label = charset
        .map(str::as_bytes)
        .or_else(|| from_meta.then(|| html_charset(body)).flatten());
    let mut unknown = None;
    let mut encoding = match label {
        Some(label) => Encoding::for_label(label).unwrap_or_else(|| {
            unknown = Some(String::from_utf8_lossy(label).into_owned());
            UTF_8
        }),
        None => UTF_8,
    };
    // HTML's meta prescan normalizes these labels; BOM and transport do not.
    if from_meta {
        if encoding == UTF_16LE || encoding == UTF_16BE {
            encoding = UTF_8;
        } else if encoding == X_USER_DEFINED {
            encoding = WINDOWS_1252;
        }
    }
    let (text, _) = encoding.decode_without_bom_handling(body);
    (text.into_owned(), unknown)
}

fn html_space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

// Byte-level HTML encoding prescan, limited to meta declarations in 1024 bytes.
// This deliberately does not build a DOM or attempt encoding detection.
fn html_charset(body: &[u8]) -> Option<&[u8]> {
    let bytes = &body[..body.len().min(1024)];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"<!--") {
            i += 2;
            i += bytes[i..].windows(3).position(|part| part == b"-->")? + 3;
        } else if bytes[i] == b'<' && bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
            i += 1;
            let start = i;
            while i < bytes.len() && !html_space(bytes[i]) && !b"/>".contains(&bytes[i]) {
                i += 1;
            }
            let meta = bytes[start..i].eq_ignore_ascii_case(b"meta")
                && bytes
                    .get(i)
                    .is_some_and(|&byte| html_space(byte) || byte == b'/');
            let mut charset = None;
            let mut content = None;
            let mut pragma = None;
            while let Some((name, value)) = prescan_attribute(bytes, &mut i) {
                if name.eq_ignore_ascii_case(b"charset") && charset.is_none() {
                    charset = Some(value);
                } else if name.eq_ignore_ascii_case(b"content") && content.is_none() {
                    content = Some(value);
                } else if name.eq_ignore_ascii_case(b"http-equiv") && pragma.is_none() {
                    pragma = Some(value);
                }
            }
            // A declaration cut off by the prescan limit is not complete.
            if i == bytes.len() {
                return None;
            }
            if meta {
                let label = charset.or_else(|| {
                    pragma
                        .filter(|value| value.eq_ignore_ascii_case(b"content-type"))
                        .and(content)
                        .and_then(meta_content_charset)
                });
                if label.is_some() {
                    return label;
                }
            }
            i += 1;
        } else if bytes[i] == b'<' && bytes.get(i + 1).is_some_and(|b| b"!/?".contains(b)) {
            i += bytes[i..].iter().position(|&byte| byte == b'>')? + 1;
        } else {
            i += 1;
        }
    }
    None
}

fn prescan_attribute<'a>(bytes: &'a [u8], i: &mut usize) -> Option<(&'a [u8], &'a [u8])> {
    while *i < bytes.len() && (html_space(bytes[*i]) || bytes[*i] == b'/') {
        *i += 1;
    }
    if *i == bytes.len() || bytes[*i] == b'>' {
        return None;
    }
    let start = *i;
    // A leading '=' belongs to the name in the prescan algorithm.
    *i += 1;
    while *i < bytes.len() && !html_space(bytes[*i]) && !b"=/>".contains(&bytes[*i]) {
        *i += 1;
    }
    let name = &bytes[start..*i];
    while *i < bytes.len() && html_space(bytes[*i]) {
        *i += 1;
    }
    if bytes.get(*i) != Some(&b'=') {
        return Some((name, b""));
    }
    *i += 1;
    while *i < bytes.len() && html_space(bytes[*i]) {
        *i += 1;
    }
    let quote = bytes.get(*i).copied().filter(|b| b"\"'".contains(b));
    if quote.is_some() {
        *i += 1;
    }
    let start = *i;
    while *i < bytes.len()
        && match quote {
            Some(quote) => bytes[*i] != quote,
            None => !html_space(bytes[*i]) && bytes[*i] != b'>',
        }
    {
        *i += 1;
    }
    let value = &bytes[start..*i];
    if quote.is_some() && *i < bytes.len() {
        *i += 1;
    }
    Some((name, value))
}

fn meta_content_charset(content: &[u8]) -> Option<&[u8]> {
    let mut rest = content;
    while let Some(start) = rest
        .windows(7)
        .position(|part| part.eq_ignore_ascii_case(b"charset"))
    {
        rest = &rest[start + 7..];
        rest = rest.trim_ascii_start();
        if !rest.starts_with(b"=") {
            continue;
        }
        rest = rest[1..].trim_ascii_start();
        if matches!(rest.first(), Some(b'\'' | b'"')) {
            let quote = rest[0];
            rest = &rest[1..];
            return rest
                .iter()
                .position(|&byte| byte == quote)
                .map(|end| &rest[..end]);
        }
        let end = rest
            .iter()
            .position(|&byte| html_space(byte) || byte == b';')
            .unwrap_or(rest.len());
        return Some(&rest[..end]);
    }
    None
}

// A conservative, linear preflight, not an HTML parser. In particular, only a
// matching top closing tag reduces the estimate. HTML ignores a self-closing
// slash on non-void elements, so it must not reduce our estimate either.
fn check_html(text: &str) -> Result<(), String> {
    let bytes = text.as_bytes();
    // Each frame saves the enclosing context, so no scan of the stack is needed.
    let mut stack: Vec<(&[u8], bool, bool)> = Vec::new();
    let mut foreign = false;
    let mut raw_text = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"<!--") {
            // Raw-text elements do not use the ordinary HTML comment grammar.
            // Skip conversion rather than skipping a real closing tag and hiding nodes.
            if raw_text {
                return Err("HTML nesting limit cannot be verified for raw-text comments".into());
            }
            i += 4;
            // HTML also permits abrupt empty comments and the --!> ending.
            if bytes[i..].starts_with(b">") {
                i += 1;
            } else if bytes[i..].starts_with(b"->") {
                i += 2;
            } else {
                while i < bytes.len() {
                    let end = if bytes[i..].starts_with(b"-->") {
                        3
                    } else if bytes[i..].starts_with(b"--!>") {
                        4
                    } else {
                        0
                    };
                    i += if end == 0 { 1 } else { end };
                    if end != 0 {
                        break;
                    }
                }
            }
            continue;
        }
        let start = i;
        if bytes[i..].starts_with(b"<![CDATA[") {
            return Err("HTML nesting limit cannot be verified for CDATA".into());
        }
        i += 1;
        let declaration = matches!(bytes.get(i), Some(b'!' | b'?'));
        let closing = bytes.get(i) == Some(&b'/');
        if closing {
            i += 1;
        }
        if !declaration && !bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
            continue;
        }
        let name_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !b"/>".contains(&bytes[i]) {
            if bytes[i] == b'<' {
                return Err("HTML tag contains nested markup".into());
            }
            i += 1;
            if i - start > MAX_HTML_TAG_BYTES {
                return Err("HTML tag length limit exceeded (4096 bytes)".into());
            }
        }
        let name = &bytes[name_start..i];
        let mut quote = None;
        while i < bytes.len() {
            let byte = bytes[i];
            i += 1;
            if i - start > MAX_HTML_TAG_BYTES {
                return Err("HTML tag length limit exceeded (4096 bytes)".into());
            }
            // Quotes in malformed/unquoted attributes need not open a quoted
            // value in HTML. Do not let this rough scan hide subsequent tags.
            if byte == b'<' {
                return Err("HTML tag contains nested markup".into());
            }
            if Some(byte) == quote {
                quote = None;
            } else if quote.is_none() {
                if matches!(byte, b'\'' | b'"') {
                    quote = Some(byte);
                } else if byte == b'>' {
                    break;
                }
            }
        }
        if declaration {
            continue;
        }
        if closing {
            if stack
                .last()
                .is_some_and(|(open, _, _)| open.eq_ignore_ascii_case(name))
            {
                let (_, previous_foreign, previous_raw) = stack.pop().expect("matched opening tag");
                foreign = previous_foreign;
                raw_text = previous_raw;
            }
        } else {
            let void = [
                "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
                "param", "source", "track", "wbr",
            ]
            .iter()
            .any(|tag| name.eq_ignore_ascii_case(tag.as_bytes()));
            if !void || foreign {
                stack.push((name, foreign, raw_text));
                if stack.len() > MAX_HTML_DEPTH {
                    return Err("HTML nesting limit exceeded (256)".into());
                }
                foreign |= name.eq_ignore_ascii_case(b"svg") || name.eq_ignore_ascii_case(b"math");
                raw_text |= [
                    "script",
                    "style",
                    "title",
                    "textarea",
                    "xmp",
                    "iframe",
                    "noembed",
                    "noframes",
                    "noscript",
                    "plaintext",
                ]
                .iter()
                .any(|tag| name.eq_ignore_ascii_case(tag.as_bytes()));
            }
        }
    }
    Ok(())
}

fn download_limit() -> String {
    format!("Download exceeds {MAX_DOWNLOAD_BYTES} bytes; content omitted.")
}
fn placeholder(content_type: &str, length: u64) -> String {
    format!("[Content omitted: {content_type}, {length} bytes]")
}
