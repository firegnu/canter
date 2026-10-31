use std::{fs, net::IpAddr, os::unix::fs::PermissionsExt, path::Path, sync::Arc};

use canter_core::CoreOptions;
use canter_model::{ApiKey, ChatCompletions, Model};
use serde::Deserialize;

use crate::ServerError;

// No Debug/Serialize: errors must not echo credentials or config contents.
#[derive(Deserialize)]
pub(crate) struct Config {
    #[serde(default = "default_telemetry")]
    telemetry: bool,
    #[serde(default = "default_sandbox")]
    sandbox: bool,
    base_url: String,
    pub model: String,
    #[serde(default = "default_models")]
    pub models: Vec<String>,
    #[serde(default = "default_image_models")]
    image_models: Vec<String>,
    context_window: u64,
    #[serde(default)]
    auto_compact_tokens: Option<u64>,
    #[serde(default = "default_auto_wake_limit")]
    auto_wake_limit: u32,
    #[serde(default = "default_goal_budget_usd")]
    goal_budget_usd: f64,
    #[serde(default = "default_model_max_retries")]
    model_max_retries: u32,
    api_key: String,
    #[serde(default)]
    prices: canter_core::usage::PriceBook,
    #[serde(default)]
    pub mcp_servers: std::collections::BTreeMap<String, canter_core::mcp::ServerConfig>,
    #[serde(default)]
    pub lsp: canter_tools::lsp::LspConfig,
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
        let file = fs::File::open(path).map_err(|_| {
            ServerError("cannot read config.json; configure a model and API key first".into())
        })?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ServerError("cannot set config.json permissions to 0600".into()))?;
        let mut config: Self = serde_json::from_reader(file)
            .map_err(|_| ServerError("config.json is not valid".into()))?;
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
        if config.context_window <= u64::from(CoreOptions::default().max_output_tokens) + 16_384 {
            return Err(ServerError(
                "context_window must be greater than 81920".into(),
            ));
        }
        if config.auto_compact_tokens == Some(0) {
            return Err(ServerError(
                "auto_compact_tokens must be greater than 0".into(),
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
        ChatCompletions::new(&self.base_url, ApiKey::new(self.api_key.clone()))
            .map(|model| {
                model.with_retry_policy(canter_model::RetryPolicy {
                    max_retries: self.model_max_retries,
                    ..Default::default()
                })
            })
            .map(|model| Arc::new(model) as Arc<dyn Model>)
            .map_err(|_| ServerError("invalid model config".into()))
    }

    pub fn options(&self) -> CoreOptions {
        let mut prices = canter_core::usage::builtin_prices();
        prices.extend(self.prices.clone());
        let official = reqwest::Url::parse("https://api.deepseek.com").expect("official base URL");
        let web_search = (reqwest::Url::parse(&self.base_url).ok().as_ref() == Some(&official))
            .then(|| {
                canter_tools::web_search::WebSearch::new(Arc::new(
                    canter_tools::web_search::deepseek::DeepSeek::new(
                        self.api_key.clone(),
                        official
                            .join("/v1/tools/search")
                            .expect("official search URL"),
                    ),
                ))
            });
        CoreOptions {
            sandbox: self.sandbox,
            web_search,
            auto_wake_limit: self.auto_wake_limit,
            goal_budget_usd: self.goal_budget_usd,
            prices,
            context_window: self.context_window,
            auto_compact_tokens: self.auto_compact_tokens,
            telemetry: self.telemetry,
            image_models: self.image_models.clone(),
            ..CoreOptions::default()
        }
    }
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
            assert_eq!(error.to_string(), "config.json is not valid", "{bad}");
        }
    }
}
