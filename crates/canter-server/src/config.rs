use std::{
    collections::BTreeMap, fs, net::IpAddr, os::unix::fs::PermissionsExt, path::Path, sync::Arc,
};

use canter_core::CoreOptions;
use canter_model::{ApiKey, ChatCompletions, Model};
use serde::Deserialize;

use crate::ServerError;

// No Debug/Serialize: errors must not echo credentials or config contents.
#[derive(Clone, Deserialize)]
pub(crate) struct Config {
    // The exact validated save, used to detect edits made during server startup.
    #[serde(skip)]
    pub contents: Vec<u8>,
    #[serde(default = "default_enabled")]
    prompt_suggestions: bool,
    #[serde(default = "default_enabled")]
    plan_enabled: bool,
    #[serde(default = "default_enabled")]
    mcp_tool_search: bool,
    #[serde(default = "default_telemetry")]
    telemetry: bool,
    #[serde(default = "default_sandbox")]
    sandbox: bool,
    #[serde(default)]
    base_url: String,
    pub model: String,
    #[serde(default = "default_models")]
    pub models: Vec<String>,
    #[serde(default = "default_image_models")]
    image_models: Vec<String>,
    #[serde(default)]
    context_window: u64,
    #[serde(default)]
    auto_compact_tokens: Option<u64>,
    #[serde(default = "default_auto_wake_limit")]
    auto_wake_limit: u32,
    #[serde(default = "default_goal_budget_usd")]
    goal_budget_usd: f64,
    #[serde(default = "default_model_max_retries")]
    model_max_retries: u32,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    headers: canter_model::RequestHeaders,
    #[serde(default)]
    prices: canter_core::usage::PriceBook,
    #[serde(default)]
    providers: BTreeMap<String, Provider>,
    #[serde(skip)]
    legacy: bool,
    #[serde(default)]
    pub mcp_servers: std::collections::BTreeMap<String, canter_core::mcp::ServerConfig>,
    #[serde(default)]
    pub lsp: canter_tools::lsp::LspConfig,
    #[serde(default)]
    hooks: canter_core::hooks::Hooks,
}

#[derive(Clone, Deserialize)]
struct Provider {
    #[serde(default = "default_api")]
    api: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    headers: canter_model::RequestHeaders,
    #[serde(default)]
    models: BTreeMap<String, ProviderModel>,
}

#[derive(Clone, Deserialize)]
struct ProviderModel {
    #[serde(default)]
    context_window: u64,
    #[serde(default)]
    auto_compact_tokens: Option<u64>,
    #[serde(default)]
    image: bool,
    #[serde(default)]
    price: Option<canter_core::usage::ModelPrice>,
}

fn default_api() -> String {
    "chat_completions".into()
}

impl Config {
    /// Only these switches are refreshed at session open; credentials and model
    /// settings keep their existing server-start lifetime.
    pub fn session_switches(home: &Path) -> Result<Option<(bool, bool)>, ServerError> {
        #[derive(Deserialize)]
        struct SessionSwitches {
            #[serde(default = "enabled")]
            plan_enabled: bool,
            #[serde(default = "enabled")]
            mcp_tool_search: bool,
        }
        fn enabled() -> bool {
            true
        }
        let file = match fs::File::open(home.join("config.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ServerError("cannot read config.json".into())),
        };
        let config: SessionSwitches = serde_json::from_reader(file)
            .map_err(|_| ServerError("config.json is not valid".into()))?;
        Ok(Some((config.plan_enabled, config.mcp_tool_search)))
    }

    pub fn load(home: &Path) -> Result<Self, ServerError> {
        let path = home.join("config.json");
        let mut file = fs::File::open(path).map_err(|_| {
            ServerError("cannot read config.json; configure a model and API key first".into())
        })?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ServerError("cannot set config.json permissions to 0600".into()))?;
        let mut contents = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut contents)
            .map_err(|_| ServerError("cannot read config.json".into()))?;
        let mut deserializer = serde_json::Deserializer::from_slice(&contents);
        let mut config: Self =
            serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
                let path = error.path().to_string();
                if path.starts_with("providers.") {
                    return ServerError(format!("invalid config.json field {path}"));
                }
                ServerError(format!(
                    "config.json is not valid at line {} column {}",
                    error.inner().line(),
                    error.inner().column()
                ))
            })?;
        deserializer
            .end()
            .map_err(|_| ServerError("config.json is not valid".into()))?;
        config.contents = contents;
        config.legacy = config.providers.is_empty()
            || !config.base_url.is_empty()
            || !config.api_key.is_empty()
            || config.context_window != 0;
        if config.legacy {
            let url = reqwest::Url::parse(&config.base_url)
                .map_err(|_| ServerError("invalid base_url".into()))?;
            let local = url.host_str().is_some_and(|host| {
                host == "localhost"
                    || host
                        .trim_matches(['[', ']'])
                        .parse::<IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
            if url.scheme() == "http" && !local {
                return Err(ServerError(
                    "a non-local base_url must use HTTPS so the API key is not sent in plain text"
                        .into(),
                ));
            }
            if config.model.trim().is_empty() || config.api_key.trim().is_empty() {
                return Err(ServerError(
                    "model and api_key in config.json must not be empty".into(),
                ));
            }
            if config.context_window <= u64::from(CoreOptions::default().max_output_tokens) + 16_384
            {
                return Err(ServerError(
                    "context_window must be greater than 81920".into(),
                ));
            }
            if config.auto_compact_tokens == Some(0) {
                return Err(ServerError(
                    "auto_compact_tokens must be greater than 0".into(),
                ));
            }
        } else {
            config.models.clear();
            config.image_models.clear();
            config.context_window = CoreOptions::default().context_window;
        }
        for (name, provider) in &config.providers {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            {
                return Err(ServerError(
                    "providers names must use ASCII letters, digits, -, _ or .".into(),
                ));
            }
            provider
                .validate()
                .map_err(|error| ServerError(format!("provider {name}: {error}")))?;
            config.models.extend(
                provider
                    .models
                    .keys()
                    .map(|model| format!("{name}/{model}")),
            );
        }
        if config.model.trim().is_empty() {
            return Err(ServerError("model in config.json must not be empty".into()));
        }
        if let Some((name, model)) = config.model.split_once('/')
            && let Some(provider) = config.providers.get(name)
            && !provider.models.contains_key(model)
        {
            return Err(ServerError(format!(
                "provider {name}: default model must be defined in models"
            )));
        }
        if !config.legacy && !config.models.contains(&config.model) {
            return Err(ServerError(
                "model must name a configured provider/model".into(),
            ));
        }
        if config.model_max_retries > 100 {
            return Err(ServerError(
                "model_max_retries must be between 0 and 100".into(),
            ));
        }
        if !config.goal_budget_usd.is_finite() || config.goal_budget_usd < 0.0 {
            return Err(ServerError(
                "goal_budget_usd must be a finite non-negative dollar amount".into(),
            ));
        }
        if !config.models.contains(&config.model) {
            config.models.insert(0, config.model.clone());
        }
        if config.prices.values().any(|price| !price.valid()) {
            return Err(ServerError(
                "prices must contain finite, non-negative dollar amounts".into(),
            ));
        }
        for server in config.mcp_servers.values() {
            server
                .validate()
                .map_err(|message| ServerError(message.into()))?;
        }
        config
            .lsp
            .resolve()
            .map_err(|message| ServerError(message.into()))?;
        Ok(config)
    }

    pub fn model_client(&self) -> Result<Arc<dyn Model>, ServerError> {
        let legacy = self.legacy.then(|| self.legacy_client()).transpose()?;
        let providers = self
            .providers
            .iter()
            .map(|(name, provider)| {
                provider
                    .client(self.model_max_retries)
                    .map(|client| (name.clone(), client))
                    .map_err(|error| ServerError(format!("provider {name}: {error}")))
            })
            .collect::<Result<_, _>>()?;
        let legacy_models = self
            .models
            .iter()
            .filter(|model| {
                !model
                    .split_once('/')
                    .is_some_and(|(name, _)| self.providers.contains_key(name))
            })
            .cloned()
            .collect();
        Ok(Arc::new(Router {
            legacy,
            legacy_models,
            providers,
        }))
    }

    fn legacy_client(&self) -> Result<Arc<dyn Model>, ServerError> {
        ChatCompletions::new(&self.base_url, ApiKey::new(self.api_key.clone()))
            .map(|model| {
                model.with_headers(self.headers.clone()).with_retry_policy(
                    canter_model::RetryPolicy {
                        max_retries: self.model_max_retries,
                        ..Default::default()
                    },
                )
            })
            .map(|model| Arc::new(model) as Arc<dyn Model>)
            .map_err(|_| ServerError("invalid model config".into()))
    }

    pub fn options(&self) -> CoreOptions {
        let mut prices = canter_core::usage::builtin_prices();
        prices.extend(self.prices.clone());
        let official = reqwest::Url::parse("https://api.deepseek.com").expect("official base URL");
        let search_key = std::iter::once((&self.base_url, &self.api_key))
            .chain(
                self.providers
                    .values()
                    .map(|provider| (&provider.base_url, &provider.api_key)),
            )
            .find_map(|(url, key)| {
                (reqwest::Url::parse(url).ok().as_ref() == Some(&official)).then_some(key)
            });
        let web_search = search_key.map(|key| {
            canter_tools::web_search::WebSearch::new(Arc::new(
                canter_tools::web_search::deepseek::DeepSeek::new(
                    key.clone(),
                    official
                        .join("/v1/tools/search")
                        .expect("official search URL"),
                ),
            ))
        });
        let mut image_models = self.image_models.clone();
        let mut model_limits = BTreeMap::new();
        for (name, provider) in &self.providers {
            for (model, config) in &provider.models {
                let id = format!("{name}/{model}");
                model_limits.insert(
                    id.clone(),
                    canter_core::ModelLimits {
                        context_window: config.context_window,
                        auto_compact_tokens: config.auto_compact_tokens,
                    },
                );
                if config.image {
                    image_models.push(id.clone());
                }
                if let Some(price) = &config.price {
                    prices.insert(id, price.clone());
                }
            }
        }
        CoreOptions {
            prompt_suggestions: self.prompt_suggestions,
            hooks: self.hooks.clone(),
            plan_enabled: self.plan_enabled,
            mcp_tool_search: self.mcp_tool_search,
            sandbox: self.sandbox,
            web_search,
            auto_wake_limit: self.auto_wake_limit,
            goal_budget_usd: self.goal_budget_usd,
            prices,
            context_window: self.context_window,
            auto_compact_tokens: self.auto_compact_tokens,
            telemetry: self.telemetry,
            image_models,
            model_limits,
            ..CoreOptions::default()
        }
    }
}

/// Selects an adapter from the same immutable configuration as the turn's capabilities.
/// Only the wire model is rewritten; the core keeps the qualified name for accounting.
struct Router {
    legacy: Option<Arc<dyn Model>>,
    legacy_models: std::collections::BTreeSet<String>,
    providers: BTreeMap<String, Arc<dyn Model>>,
}

impl Model for Router {
    fn stream(
        &self,
        mut request: canter_model::Request,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> canter_model::ModelStream<'_> {
        if let Some((provider, model)) = request.model.split_once('/')
            && let Some(client) = self.providers.get(provider)
        {
            request.model = model.to_owned();
            return client.stream(request, cancellation);
        }
        if let Some(client) = &self.legacy
            && (!request.model.contains('/') || self.legacy_models.contains(&request.model))
        {
            return client.stream(request, cancellation);
        }
        Box::pin(futures_util::stream::once(async {
            Err(canter_model::ModelError {
                kind: canter_model::ErrorKind::InvalidRequest,
                status: None,
                retry_after: None,
                retries: 0,
            })
        }))
    }
}

impl Provider {
    fn validate(&self) -> Result<(), ServerError> {
        if self.api != "chat_completions" {
            return Err(ServerError(
                "unsupported api; only chat_completions is supported".into(),
            ));
        }
        let url = reqwest::Url::parse(&self.base_url)
            .map_err(|_| ServerError("invalid base_url".into()))?;
        let local = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ServerError("invalid base_url".into()));
        }
        if url.scheme() == "http" && !local {
            return Err(ServerError("non-local base_url must use HTTPS".into()));
        }
        if self.api_key.trim().is_empty()
            || reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.api_key)).is_err()
        {
            return Err(ServerError("invalid or missing api_key".into()));
        }
        if self.models.is_empty() {
            return Err(ServerError("models must not be empty".into()));
        }
        for (name, model) in &self.models {
            if name.trim().is_empty() {
                return Err(ServerError("models contains an empty model name".into()));
            }
            let invalid = if model.context_window
                <= u64::from(CoreOptions::default().max_output_tokens) + 16_384
            {
                Some("context_window must be greater than 81920")
            } else if model.auto_compact_tokens == Some(0) {
                Some("auto_compact_tokens must be greater than 0")
            } else if model.price.as_ref().is_some_and(|price| !price.valid()) {
                Some("price must contain finite, non-negative dollar amounts")
            } else {
                None
            };
            if let Some(message) = invalid {
                return Err(ServerError(format!("model {name}: {message}")));
            }
        }
        Ok(())
    }

    fn client(&self, max_retries: u32) -> Result<Arc<dyn Model>, ServerError> {
        match self.api.as_str() {
            "chat_completions" => {
                ChatCompletions::new(&self.base_url, ApiKey::new(self.api_key.clone()))
                    .map(|client| {
                        Arc::new(client.with_headers(self.headers.clone()).with_retry_policy(
                            canter_model::RetryPolicy {
                                max_retries,
                                ..Default::default()
                            },
                        )) as Arc<dyn Model>
                    })
                    .map_err(|_| ServerError("invalid model config".into()))
            }
            _ => Err(ServerError(
                "unsupported api; only chat_completions is supported".into(),
            )),
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_telemetry() -> bool {
    true
}

fn default_sandbox() -> bool {
    true
}

fn default_models() -> Vec<String> {
    vec!["deepseek-flash".into(), "deepseek-v4-pro".into()]
}
fn default_auto_wake_limit() -> u32 {
    10
}
fn default_goal_budget_usd() -> f64 {
    5.0
}
fn default_model_max_retries() -> u32 {
    canter_model::RetryPolicy::default().max_retries
}

fn default_image_models() -> Vec<String> {
    vec!["deepseek-flash".into()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn providers_can_supply_the_entire_model_configuration() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("config.json"),
            json!({
                "model":"one/vendor/model",
                "providers": {
                    "one":{"base_url":"http://127.0.0.1:1","api_key":"test-one",
                        "models":{"vendor/model":{"context_window":200000,"image":true}}},
                    "two":{"api":"chat_completions","base_url":"http://127.0.0.1:2",
                        "api_key":"test-two","models":{"small":{"context_window":100000}}}
                }
            })
            .to_string(),
        )
        .unwrap();
        let config = Config::load(root.path()).expect("providers replace the legacy fields");
        assert_eq!(config.models, ["one/vendor/model", "two/small"]);
        assert_eq!(config.options().image_models, ["one/vendor/model"]);
        assert!(config.model_client().is_ok());
    }

    #[test]
    fn invalid_provider_fields_identify_the_provider_and_never_echo_credentials() {
        let valid = json!({"base_url":"https://example.com","api_key":"test-only-private-key",
            "headers":{"x-secret":"test-only-private-header"},"models":{"model":{"context_window":100000}}});
        for (field, value, expected) in [
            ("api", json!("responses"), "api"),
            ("base_url", json!("http://example.com"), "base_url"),
            (
                "base_url",
                json!("https://test-only-private-key@example.com"),
                "base_url",
            ),
            ("api_key", json!(""), "api_key"),
            ("api_key", json!("test-only-private-key\n"), "api_key"),
            (
                "headers",
                json!({"authorization":"test-only-private-header"}),
                "headers",
            ),
            (
                "models",
                json!({"model":{"context_window":81920}}),
                "context_window",
            ),
            (
                "models",
                json!({"model":{"context_window":100000,"auto_compact_tokens":0}}),
                "auto_compact_tokens",
            ),
            (
                "models",
                json!({"model":{"context_window":100000,"price":{"peak":{"cache_hit":-1,"cache_miss":1,"output":1}}}}),
                "price",
            ),
        ] {
            let mut provider = valid.clone();
            provider[field] = value;
            let error = load(json!({"providers":{"broken":provider}}))
                .err()
                .expect("invalid provider")
                .to_string();
            assert!(
                error.contains("broken") && error.contains(expected),
                "{error}"
            );
            assert!(
                !error.contains("test-only-private-key")
                    && !error.contains("test-only-private-header")
            );
        }
        for field in ["base_url", "api_key", "models"] {
            let mut provider = valid.clone();
            provider.as_object_mut().unwrap().remove(field);
            let error = load(json!({"providers":{"broken":provider}}))
                .err()
                .expect("missing provider field")
                .to_string();
            assert!(error.contains("broken") && error.contains(field), "{error}");
        }
    }

    #[test]
    fn named_models_have_their_own_prices_and_share_configured_search() {
        let config = load(json!({"providers":{
            "subscription":{"base_url":"https://example.com","api_key":"test-only-plan-key",
                "models":{"deepseek-flash":{"context_window":200000}}},
            "search":{"base_url":"https://api.deepseek.com/","api_key":"test-only-search-key",
                "models":{"priced":{"context_window":300000,"price":{"peak":{"cache_hit":1,"cache_miss":2,"output":3}}}}}
        }})).unwrap();
        let options = config.options();
        assert!(options.web_search.is_some());
        assert!(!options.prices.contains_key("subscription/deepseek-flash"));
        assert_eq!(options.prices["search/priced"].peak.output, 3.0);
        assert!(!format!("{options:?}").contains("test-only-search-key"));
    }

    #[test]
    fn a_named_default_requires_model_metadata_even_with_legacy_fields() {
        let error = load(json!({"model":"one/missing", "providers":{
            "one":{"base_url":"http://127.0.0.1:1","api_key":"test-only-one",
                "models":{"known":{"context_window":100000}}}
        }}))
        .err()
        .expect("a named default must have its own limits")
        .to_string();
        assert!(error.contains("one") && error.contains("models"));
    }

    #[tokio::test]
    async fn legacy_requests_keep_the_same_bytes_with_and_without_named_providers() {
        use futures_util::StreamExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let router = axum::Router::new().route("/chat/completions", axum::routing::post(
            move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let send = send.clone();
                async move {
                    send.send((headers, body)).unwrap();
                    ([("content-type", "text/event-stream")], "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
                }
            }
        ));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        for model in ["fake", "vendor/legacy"] {
            let mut extra =
                json!({"base_url":url,"model":model,"headers":{"x-test":"id {session_id}"}});
            let legacy = load(extra.clone()).unwrap();
            extra["providers"] = json!({"other":{"base_url":"http://127.0.0.1:1","api_key":"test-only-other",
                "models":{"new":{"context_window":100000}}}});
            let mixed = load(extra).unwrap();
            let request = canter_model::Request {
                session_id: Some("stable-session".into()),
                model: model.into(),
                messages: vec![canter_model::Message::User {
                    text: "unchanged".into(),
                }],
                options: canter_model::RequestOptions {
                    thinking: Some(true),
                    reasoning_effort: Some("high".into()),
                    max_output_tokens: Some(65536),
                    ..Default::default()
                },
                ..Default::default()
            };
            // The original direct adapter is the reference, including raw HTTP body bytes.
            for client in [
                legacy.legacy_client().unwrap(),
                legacy.model_client().unwrap(),
                mixed.model_client().unwrap(),
            ] {
                let mut stream =
                    client.stream(request.clone(), tokio_util::sync::CancellationToken::new());
                while let Some(event) = stream.next().await {
                    event.unwrap();
                }
            }
            let baseline = receive.recv().await.unwrap();
            for _ in 0..2 {
                let routed = receive.recv().await.unwrap();
                assert_eq!(routed, baseline);
            }
        }
        task.abort();
        let _ = task.await;
    }

    #[test]
    fn model_headers_reject_invalid_names_values_and_reserved_names_without_echoing_values() {
        for name in [
            "",
            "bad name",
            "bad:name",
            "Authorization",
            "aUtHoRiZaTiOn",
            "Content-Type",
            "CONTENT-TYPE",
            "User-Agent",
            "uSeR-aGeNt",
        ] {
            let error = load(json!({"headers":{name:"test-only-private-value"}}))
                .err()
                .expect("invalid model header must fail configuration loading");
            assert!(!format!("{error:?} {error}").contains("test-only-private-value"));
        }
        for value in ["test-only-private-value\r\n", "test-only-private-value\0"] {
            let error = load(json!({"headers":{"x-test":value}}))
                .err()
                .expect("invalid model header value must fail configuration loading");
            assert!(!format!("{error:?} {error}").contains("test-only-private-value"));
        }
        for headers in [
            json!(null),
            json!([]),
            json!("test-only-private-value"),
            json!({"x-test":42}),
            json!({"x-test":true}),
        ] {
            let error = load(json!({"headers":headers}))
                .err()
                .expect("headers must be a map of strings");
            assert!(!format!("{error:?} {error}").contains("test-only-private-value"));
        }
        assert!(
            load(json!({"headers":{"x-test":"test-only-private-value {session_id} {other}"}}))
                .is_ok()
        );
    }

    #[test]
    fn sandbox_defaults_on_and_accepts_an_explicit_false() {
        assert!(load(json!({})).unwrap().options().sandbox);
        assert!(load(json!({"sandbox":true})).unwrap().options().sandbox);
        assert!(!load(json!({"sandbox":false})).unwrap().options().sandbox);
        assert!(load(json!({"sandbox":"false"})).is_err());
    }

    #[test]
    fn enabled_lsp_requires_a_complete_server_configuration() {
        let error = load(json!({"lsp":{"enabled":true,"servers":{"fake":{"args":["--stdio"]}}}}))
            .err()
            .expect("an enabled LSP server needs a command and file/root mappings");
        assert!(error.to_string().contains("LSP"));
    }

    #[test]
    fn goal_budget_defaults_to_five_and_accepts_zero_but_not_negative_or_text() {
        assert_eq!(load(json!({})).ok().unwrap().options().goal_budget_usd, 5.0);
        for amount in [0.0, 3.5] {
            assert_eq!(
                load(json!({"goal_budget_usd":amount}))
                    .ok()
                    .unwrap()
                    .options()
                    .goal_budget_usd,
                amount
            );
        }
        for amount in [json!(-1), json!("5"), serde_json::Value::Null] {
            assert!(load(json!({"goal_budget_usd":amount})).is_err());
        }
    }

    #[test]
    fn remote_mcp_http_is_rejected_without_echoing_credentials() {
        let root = tempfile::tempdir().unwrap();
        let config = json!({
            "base_url":"http://127.0.0.1:1", "model":"fake", "api_key":"model-test-key",
            "context_window":1048576,
            "mcp_servers":{"remote":{"transport":"http", "url":"http://example.com/mcp",
                "headers":{"authorization":"test-only-mcp-credential"}}}
        });
        fs::write(root.path().join("config.json"), config.to_string()).unwrap();
        let error = Config::load(root.path())
            .err()
            .expect("non-local MCP HTTP must be rejected");
        assert!(error.to_string().contains("HTTPS"));
        assert!(!error.to_string().contains("test-only-mcp-credential"));
    }

    fn load(extra: serde_json::Value) -> Result<Config, ServerError> {
        let root = tempfile::tempdir().unwrap();
        let mut config = json!({
            "base_url":"http://127.0.0.1:1", "model":"fake", "api_key":"model-test-key",
            "context_window":1048576,
        });
        config
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        fs::write(root.path().join("config.json"), config.to_string()).unwrap();
        Config::load(root.path())
    }

    #[test]
    fn prompt_suggestions_default_on_and_require_a_boolean() {
        assert!(load(json!({})).unwrap().options().prompt_suggestions);
        assert!(
            load(json!({"prompt_suggestions":true}))
                .unwrap()
                .options()
                .prompt_suggestions
        );
        assert!(
            !load(json!({"prompt_suggestions":false}))
                .unwrap()
                .options()
                .prompt_suggestions
        );
        for value in [json!("false"), json!(0), json!(null)] {
            assert!(load(json!({"prompt_suggestions":value})).is_err());
        }
    }

    #[test]
    fn telemetry_requires_a_boolean() {
        assert!(load(json!({})).unwrap().options().telemetry);
        assert!(load(json!({"telemetry":true})).unwrap().options().telemetry);
        assert!(
            !load(json!({"telemetry":false}))
                .unwrap()
                .options()
                .telemetry
        );
        for value in [json!("false"), json!(0), json!(null)] {
            assert!(load(json!({"telemetry":value})).is_err());
        }
    }

    #[test]
    fn web_search_is_only_configured_for_the_official_deepseek_base_url() {
        for (base_url, enabled) in [
            ("https://api.deepseek.com", true),
            ("https://api.deepseek.com/", true),
            ("https://example.com", false),
            ("http://127.0.0.1:12345", false),
        ] {
            let options = load(json!({"base_url":base_url})).unwrap().options();
            assert_eq!(options.web_search.is_some(), enabled, "{base_url}");
            assert!(!format!("{options:?}").contains("model-test-key"));
        }
    }

    #[test]
    fn model_retry_limit_defaults_to_ten_and_accepts_zero_through_one_hundred() {
        assert_eq!(load(json!({})).ok().unwrap().model_max_retries, 10);
        for limit in [0, 1, 100] {
            assert_eq!(
                load(json!({"model_max_retries":limit}))
                    .ok()
                    .unwrap()
                    .model_max_retries,
                limit
            );
        }
        assert_eq!(
            load(json!({"model_max_retries":101}))
                .err()
                .unwrap()
                .to_string(),
            "model_max_retries must be between 0 and 100"
        );
        for value in [json!(-1), json!(1.5), json!("10")] {
            assert!(load(json!({"model_max_retries":value})).is_err());
        }
    }

    #[test]
    fn auto_compact_tokens_is_optional_and_must_be_a_positive_integer() {
        let options = load(json!({})).ok().unwrap().options();
        assert_eq!(options.auto_compact_tokens, None);
        let options = load(json!({"auto_compact_tokens": 400000}))
            .ok()
            .unwrap()
            .options();
        assert_eq!(options.auto_compact_tokens, Some(400_000));
        let error = load(json!({"auto_compact_tokens": 0})).err().unwrap();
        assert_eq!(
            error.to_string(),
            "auto_compact_tokens must be greater than 0"
        );
        for bad in [json!(-1), json!(1.5), json!("400000")] {
            let error = load(json!({ "auto_compact_tokens": bad })).err().unwrap();
            assert!(
                error
                    .to_string()
                    .starts_with("config.json is not valid at line 1 column "),
                "{bad}"
            );
        }
    }
}
