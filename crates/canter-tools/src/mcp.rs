use std::{collections::BTreeMap, net::IpAddr};

use serde::Deserialize;

mod http;
pub mod oauth;

// Deliberately neither Debug nor Serialize: these values can contain credentials.
#[derive(Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        startup_timeout_sec: Option<u64>,
        #[serde(default)]
        tool_timeout_sec: Option<u64>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        startup_timeout_sec: Option<u64>,
        #[serde(default)]
        tool_timeout_sec: Option<u64>,
    },
}

impl ServerConfig {
    fn timeouts(&self) -> (Option<u64>, Option<u64>) {
        match self {
            Self::Stdio {
                startup_timeout_sec,
                tool_timeout_sec,
                ..
            }
            | Self::Http {
                startup_timeout_sec,
                tool_timeout_sec,
                ..
            } => (*startup_timeout_sec, *tool_timeout_sec),
        }
    }
    /// Connecting, initializing and listing tools, including later re-listing.
    pub fn startup_timeout(&self) -> Duration {
        Duration::from_secs(self.timeouts().0.unwrap_or(DEFAULT_STARTUP_TIMEOUT_SEC))
    }
    /// One tool call.
    pub fn tool_timeout(&self) -> Duration {
        Duration::from_secs(self.timeouts().1.unwrap_or(DEFAULT_TOOL_TIMEOUT_SEC))
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        let (startup, tool) = self.timeouts();
        if [startup, tool]
            .into_iter()
            .flatten()
            .any(|seconds| !(1..=MAX_TIMEOUT_SEC).contains(&seconds))
        {
            return Err("MCP timeouts must be between 1 and 86400 seconds");
        }
        match self {
            Self::Stdio {
                command, args, env, ..
            } => {
                if command.is_empty()
                    || command.contains('\0')
                    || args.iter().any(|arg| arg.contains('\0'))
                    || env.iter().any(|(key, value)| {
                        key.is_empty() || key.contains(['=', '\0']) || value.contains('\0')
                    })
                {
                    return Err("invalid MCP command or environment");
                }
            }
            Self::Http { url, headers, .. } => {
                let url = reqwest::Url::parse(url).map_err(|_| "invalid MCP URL")?;
                let local = url.host_str().is_some_and(|host| {
                    host == "localhost"
                        || host
                            .trim_matches(['[', ']'])
                            .parse::<IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                });
                if url.scheme() != "https" && !(url.scheme() == "http" && local) {
                    return Err("a non-local MCP URL must use HTTPS");
                }
                if !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                {
                    return Err("invalid MCP URL; put credentials in headers");
                }
                for (name, value) in headers {
                    reqwest::header::HeaderName::from_bytes(name.as_bytes())
                        .map_err(|_| "invalid MCP header")?;
                    reqwest::header::HeaderValue::from_str(value)
                        .map_err(|_| "invalid MCP header")?;
                }
            }
        }
        Ok(())
    }
}

use canter_protocol::{McpServerStatus, McpToolInfo};
use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResponse},
    service::{NotificationContext, Peer},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::Value;
use std::{
    collections::HashSet,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const DEFAULT_STARTUP_TIMEOUT_SEC: u64 = 10;
const DEFAULT_TOOL_TIMEOUT_SEC: u64 = 120;
// Keeps deadline arithmetic far from overflow.
const MAX_TIMEOUT_SEC: u64 = 86_400;
const STDIO_BASE_ENV: [&str; 8] = [
    "HOME", "PATH", "USER", "LOGNAME", "SHELL", "TMPDIR", "LANG", "LC_ALL",
];

#[derive(Default)]
pub struct Registry {
    servers: Vec<Arc<Server>>,
    stop: CancellationToken,
    tasks: tokio::sync::Mutex<Vec<JoinHandle<()>>>,
}
struct Server {
    name: String,
    state: Mutex<ServerState>,
    secrets: Arc<Mutex<Vec<String>>>,
    needs_login: Arc<AtomicBool>,
    call_timeout: Duration,
}
#[derive(Default)]
struct ServerState {
    peer: Option<Peer<RoleClient>>,
    tools: Vec<rmcp::model::Tool>,
    error: Option<&'static str>,
    warnings: Vec<String>,
}
#[derive(Clone)]
pub struct McpTool {
    pub name: String,
    pub tool: String,
    pub description: String,
    pub parameters: Value,
    server: Arc<Server>,
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl Registry {
    pub async fn connect(
        config: BTreeMap<String, ServerConfig>,
        home: std::path::PathBuf,
        stop: CancellationToken,
    ) -> Self {
        let mut registry = Self {
            servers: Vec::new(),
            stop,
            tasks: Default::default(),
        };
        let mut connecting = tokio::task::JoinSet::new();
        for (name, config) in config {
            let (values, skip_base_env) = match &config {
                ServerConfig::Stdio { env, .. } => (env, true),
                ServerConfig::Http { headers, .. } => (headers, false),
            };
            let mut secrets: Vec<_> = values
                .iter()
                .filter(|(name, _)| !skip_base_env || !STDIO_BASE_ENV.contains(&name.as_str()))
                .flat_map(|(_, value)| {
                    std::iter::once(value.as_str()).chain(value.split_whitespace())
                })
                .filter(|value| value.len() >= 8)
                .map(str::to_owned)
                .collect();
            secrets.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
            secrets.dedup();
            let server = Arc::new(Server {
                name,
                secrets: Arc::new(Mutex::new(secrets)),
                needs_login: Default::default(),
                call_timeout: config.tool_timeout(),
                state: Mutex::new(ServerState::default()),
            });
            registry.servers.push(server.clone());
            let stop = registry.stop.clone();
            let home = home.to_owned();
            connecting.spawn(async move { connect(server, config, &home, stop).await });
        }
        while let Some(result) = connecting.join_next().await {
            if let Ok(Some(task)) = result {
                registry.tasks.get_mut().push(task);
            }
        }
        registry
    }

    /// Cloned once when opening a session. Connection state remains live, definitions do not.
    pub fn snapshot(&self) -> Vec<McpTool> {
        let mut names = HashSet::new();
        let mut tools = Vec::new();
        for server in &self.servers {
            let state = server.state.lock().unwrap();
            if server.needs_login.load(Ordering::Relaxed)
                || state
                    .peer
                    .as_ref()
                    .is_none_or(|peer| peer.is_transport_closed())
            {
                continue;
            }
            for tool in &state.tools {
                let name = format!("mcp__{}__{}", server.name, tool.name);
                if valid_name(&name) && names.insert(name.clone()) {
                    tools.push(McpTool {
                        name,
                        tool: tool.name.to_string(),
                        description: server.public(tool.description.as_deref().unwrap_or_default()),
                        parameters: server
                            .public_value(Value::Object((*tool.input_schema).clone())),
                        server: server.clone(),
                    });
                } else {
                    eprintln!(
                        "MCP {}: skipped duplicate model tool name",
                        server.public(&server.name).replace(['\n', '\r'], " ")
                    );
                }
            }
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools
    }

    pub fn status(&self) -> Vec<McpServerStatus> {
        let mut names = HashSet::new();
        self.servers
            .iter()
            .map(|server| {
                let state = server.state.lock().unwrap();
                let connected = !server.needs_login.load(Ordering::Relaxed)
                    && state
                        .peer
                        .as_ref()
                        .is_some_and(|p| !p.is_transport_closed());
                let mut warnings = state.warnings.clone();
                let mut tools = Vec::new();
                for tool in &state.tools {
                    let name = format!("mcp__{}__{}", server.name, tool.name);
                    if valid_name(&name) && (!connected || names.insert(name.clone())) {
                        tools.push(McpToolInfo {
                            name: server.public(&name),
                            tool: server.public(&tool.name),
                        });
                    } else {
                        warnings.push("Skipped an invalid or duplicate tool name".into());
                    }
                }
                tools.sort_by(|a, b| a.name.cmp(&b.name));
                McpServerStatus {
                    name: server.public(&server.name),
                    connected,
                    error: (!connected).then(|| {
                        if server.needs_login.load(Ordering::Relaxed) {
                            format!(
                                "需要登录：运行 `canter mcp login {}`",
                                server.public(&server.name)
                            )
                        } else {
                            state.error.unwrap_or("connection closed").into()
                        }
                    }),
                    tools,
                    warnings,
                }
            })
            .collect()
    }

    pub async fn shutdown(&self) {
        self.stop.cancel();
        for task in self.tasks.lock().await.drain(..) {
            let _ = task.await;
        }
    }
}
fn valid_name(name: &str) -> bool {
    name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
impl Server {
    fn public_value(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.public(&text)),
            Value::Array(values) => {
                Value::Array(values.into_iter().map(|v| self.public_value(v)).collect())
            }
            Value::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(k, v)| (self.public(&k), self.public_value(v)))
                    .collect(),
            ),
            value => value,
        }
    }
    fn public(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for secret in self.secrets.lock().unwrap().iter() {
            text = text.replace(secret, "[redacted]");
            let escaped = serde_json::to_string(secret).unwrap();
            text = text.replace(&escaped[1..escaped.len() - 1], "[redacted]");
        }
        text
    }
    fn disconnected(&self, reason: &'static str) {
        let mut state = self.state.lock().unwrap();
        state.peer = None;
        state.error = Some(reason);
        eprintln!(
            "MCP {}: {reason}",
            self.public(&self.name).replace(['\n', '\r'], " ")
        );
    }
    fn update(&self, mut tools: Vec<rmcp::model::Tool>) {
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        let mut names = HashSet::new();
        let mut warnings = Vec::new();
        tools.retain(|tool| {
            let valid = !self.name.is_empty()
                && !tool.name.is_empty()
                && self.public(&self.name) == self.name
                && self.public(&tool.name) == tool.name
                && valid_name(&format!("mcp__{}__{}", self.name, tool.name))
                && names.insert(tool.name.to_string());
            if !valid {
                warnings.push("Skipped an invalid or duplicate tool name".into());
                eprintln!(
                    "MCP {}: skipped an invalid or duplicate tool name",
                    self.public(&self.name).replace(['\n', '\r'], " ")
                );
            }
            valid
        });
        let mut state = self.state.lock().unwrap();
        state.tools = tools;
        state.warnings = warnings;
    }
}
struct Handler(Arc<Notify>);
impl ClientHandler for Handler {
    async fn on_tool_list_changed(&self, _: NotificationContext<RoleClient>) {
        self.0.notify_one();
    }
}
async fn connect(
    server: Arc<Server>,
    config: ServerConfig,
    home: &std::path::Path,
    stop: CancellationToken,
) -> Option<JoinHandle<()>> {
    let changed = Arc::new(Notify::new());
    let startup_timeout = config.startup_timeout();
    let deadline = tokio::time::Instant::now() + startup_timeout;
    let mut process_group = None;
    let initialize = async {
        match config {
            ServerConfig::Stdio {
                command, args, env, ..
            } => {
                let mut command = tokio::process::Command::new(command);
                command
                    .args(args)
                    .env_clear()
                    .envs(
                        STDIO_BASE_ENV
                            .into_iter()
                            .filter_map(|name| std::env::var_os(name).map(|value| (name, value))),
                    )
                    .envs(env)
                    .kill_on_drop(true)
                    .process_group(0);
                let (transport, _) = TokioChildProcess::builder(command)
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|_| "could not start MCP process")?;
                process_group = transport.id().map(ProcessGroup);
                Handler(changed.clone())
                    .serve(transport)
                    .await
                    .map_err(|_| "MCP initialization failed")
            }
            ServerConfig::Http { url, headers, .. } => {
                let use_oauth = !headers
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case("authorization"));
                let headers = headers
                    .into_iter()
                    .map(|(name, value)| {
                        Ok((
                            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                                .map_err(|_| "invalid MCP header")?,
                            reqwest::header::HeaderValue::from_str(&value)
                                .map_err(|_| "invalid MCP header")?,
                        ))
                    })
                    .collect::<Result<_, &str>>()?;
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(startup_timeout)
                    .build()
                    .map_err(|_| "could not create MCP HTTP client")?;
                let store = oauth::Store::new(home, &server.name, &url, server.secrets.clone());
                let client = match oauth::authorized_client(store, client, use_oauth).await {
                    Ok(inner) => http::Client {
                        inner,
                        needs_login: server.needs_login.clone(),
                        use_oauth,
                    },
                    Err(_) => {
                        server.needs_login.store(true, Ordering::Relaxed);
                        return Err("MCP OAuth initialization failed");
                    }
                };
                let config = StreamableHttpClientTransportConfig::with_uri(url)
                    .custom_headers(headers)
                    .reinit_on_expired_session(false);
                let transport = StreamableHttpClientTransport::with_client(client, config);
                Handler(changed.clone())
                    .serve(transport)
                    .await
                    .map_err(|_| "MCP connection or initialization failed")
            }
        }
    };
    let mut service = match tokio::select! {
        _ = stop.cancelled() => { return None; }
        result = tokio::time::timeout_at(deadline, initialize) => result,
    } {
        Ok(Ok(service)) => service,
        Ok(Err(reason)) => {
            server.disconnected(reason);
            return None;
        }
        Err(_) => {
            server.disconnected("MCP connection timed out");
            return None;
        }
    };
    let peer = service.peer().clone();
    match tokio::select! {
        _ = stop.cancelled() => { let _ = service.close().await; return None; }
        result = tokio::time::timeout_at(deadline, peer.list_all_tools()) => result,
    } {
        Ok(Ok(tools)) => server.update(tools),
        _ => {
            server.disconnected("MCP tool listing failed or timed out");
            return Some(tokio::spawn(async move {
                let _process_group = process_group;
                let _ = service.close().await;
            }));
        }
    }
    server.state.lock().unwrap().peer = Some(peer.clone());
    Some(tokio::spawn(async move {
        let _process_group = process_group;
        let service_stop = service.cancellation_token();
        let waiting = service.waiting();
        tokio::pin!(waiting);
        loop {
            tokio::select! {
                _ = stop.cancelled() => { service_stop.cancel(); let _ = waiting.await; break; }
                _ = &mut waiting => { server.disconnected("MCP connection closed"); break; }
                _ = changed.notified() => {
                    match tokio::time::timeout(startup_timeout, peer.list_all_tools()).await {
                        Ok(Ok(tools)) => server.update(tools),
                        _ => { service_stop.cancel(); let _ = waiting.await; server.disconnected("MCP tool listing failed or timed out"); break; }
                    }
                }
            }
        }
    }))
}

impl McpTool {
    pub fn server(&self) -> &str {
        &self.server.name
    }
    pub(crate) async fn call(
        &self,
        args: &Value,
        cancel: &CancellationToken,
    ) -> (String, String, crate::ToolOutcome, Vec<crate::ToolImage>) {
        use crate::{
            MAX_CLIENT_OUTPUT_BYTES, MAX_MODEL_OUTPUT_BYTES, ToolOutcome, output::BoundedText,
        };
        let peer = if self.server.needs_login.load(Ordering::Relaxed) {
            None
        } else {
            self.server.state.lock().unwrap().peer.clone()
        };
        let Some(peer) = peer.filter(|peer| !peer.is_transport_closed()) else {
            return (
                "MCP service disconnected".into(),
                "MCP service disconnected".into(),
                ToolOutcome::Failed,
                Vec::new(),
            );
        };
        let params = CallToolRequestParams::new(self.tool.clone())
            .with_arguments(args.as_object().cloned().unwrap_or_default());
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(("MCP call cancelled",ToolOutcome::Cancelled)),
            result = tokio::time::timeout(self.server.call_timeout,peer.call_tool_once(params)) => match result {
                Err(_) => Err(("MCP call timed out",ToolOutcome::TimedOut)),
                Ok(Err(_)) => Err(("MCP tool call failed",ToolOutcome::Failed)),
                Ok(Ok(CallToolResponse::Complete(result))) => Ok(result),
                Ok(Ok(_)) => Err(("MCP returned an unsupported result",ToolOutcome::Failed)),
            }
        };
        match result {
            Err((error, outcome)) => (error.into(), error.into(), outcome, Vec::new()),
            Ok(result) => {
                let mut model = BoundedText::new(MAX_MODEL_OUTPUT_BYTES);
                let mut client = BoundedText::new(MAX_CLIENT_OUTPUT_BYTES);
                let mut images = Vec::new();
                let mut image_number = 0;
                let mut has_text = false;
                if result.is_error == Some(true) {
                    model.push(b"MCP tool error: ");
                    client.push(b"MCP tool error: ");
                }
                for (index, content) in result.content.iter().enumerate() {
                    if index > 0 {
                        model.push(b"\n");
                        client.push(b"\n");
                    }
                    let text = match content {
                        rmcp::model::ContentBlock::Text(text) => {
                            has_text |= !text.text.trim().is_empty();
                            self.server.public(&text.text)
                        }
                        rmcp::model::ContentBlock::Resource(resource) => match &resource.resource {
                            rmcp::model::ResourceContents::TextResourceContents {
                                text, ..
                            } => {
                                has_text |= !text.trim().is_empty();
                                self.server.public(text)
                            }
                            _ => "[MCP non-text content omitted]".into(),
                        },
                        rmcp::model::ContentBlock::Image(image) => {
                            image_number += 1;
                            match self.image(&image.data, image_number) {
                                Ok(image) => {
                                    let text = image.explanation();
                                    images.push(image);
                                    text
                                }
                                Err(error) => format!("[MCP image omitted: {error}]"),
                            }
                        }
                        _ => "[MCP non-text content omitted]".into(),
                    };
                    model.push(text.as_bytes());
                    client.push(text.as_bytes());
                }
                if !has_text && let Some(structured) = &result.structured_content {
                    if !result.content.is_empty() {
                        model.push(b"\n");
                        client.push(b"\n");
                    }
                    let text = self.server.public(&structured.to_string());
                    model.push(text.as_bytes());
                    client.push(text.as_bytes());
                }
                let outcome = if result.is_error == Some(true) {
                    ToolOutcome::Failed
                } else {
                    ToolOutcome::Completed
                };
                let mut model = model.render(false);
                // Head/tail truncation must not hide how to find an attached image.
                for image in &images {
                    let explanation = image.explanation();
                    if !model.contains(&explanation) {
                        model.push('\n');
                        model.push_str(&explanation);
                    }
                }
                (model, client.render(true), outcome, images)
            }
        }
    }

    fn image(&self, data: &str, number: usize) -> Result<crate::ToolImage, &'static str> {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        // Reject before allocating the decoded image; check exact bytes afterwards.
        if data.len() as u64 > canter_protocol::MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err("image exceeds the 8 MiB limit");
        }
        let bytes = STANDARD.decode(data).map_err(|_| "invalid image base64")?;
        let mut image = crate::ToolImage::new(bytes, String::new())?;
        image.name = format!(
            "{}-{}-image-{number}.{}",
            self.server.name,
            self.tool,
            image.mime_type.strip_prefix("image/").unwrap()
        );
        Ok(image)
    }
}

// Only the group created for this transport; never search by process name or path.
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0 as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: serde_json::Value) -> ServerConfig {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn timeouts_default_to_ten_and_one_hundred_twenty_seconds() {
        for config in [
            parse(serde_json::json!({"transport":"stdio","command":"server"})),
            parse(serde_json::json!({"transport":"http","url":"http://127.0.0.1:1/mcp"})),
        ] {
            assert_eq!(config.startup_timeout(), Duration::from_secs(10));
            assert_eq!(config.tool_timeout(), Duration::from_secs(120));
        }
    }

    #[test]
    fn configured_timeouts_are_used_and_out_of_range_values_are_rejected() {
        let config = parse(serde_json::json!({
            "transport":"stdio","command":"server","startup_timeout_sec":30,"tool_timeout_sec":600
        }));
        assert_eq!(config.startup_timeout(), Duration::from_secs(30));
        assert_eq!(config.tool_timeout(), Duration::from_secs(600));
        assert!(config.validate().is_ok());
        for field in ["startup_timeout_sec", "tool_timeout_sec"] {
            let mut value = serde_json::json!({"transport":"http","url":"http://127.0.0.1:1/mcp"});
            for seconds in [0, 86_401, u64::MAX] {
                value[field] = seconds.into();
                assert!(
                    parse(value.clone()).validate().is_err(),
                    "{field} = {seconds}"
                );
            }
            value[field] = 86_400.into();
            assert!(parse(value).validate().is_ok());
        }
    }
}
