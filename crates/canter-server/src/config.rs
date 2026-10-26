use std::{fs, net::IpAddr, os::unix::fs::PermissionsExt, path::Path, sync::Arc};

use canter_core::CoreOptions;
use canter_model::{ApiKey, ChatCompletions, Model};
use serde::Deserialize;

use crate::ServerError;

// No Debug/Serialize: errors must not echo credentials or config contents.
#[derive(Deserialize)]
pub(crate) struct Config {
    base_url: String,
    pub model: String,
    #[serde(default = "default_models")]
    pub models: Vec<String>,
    #[serde(default = "default_image_models")]
    image_models: Vec<String>,
    context_window: u64,
    api_key: String,
    #[serde(default)]
    prices: canter_core::usage::PriceBook,
    #[serde(default)]
    pub mcp_servers: std::collections::BTreeMap<String, canter_core::mcp::ServerConfig>,
}

impl Config {
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
        Ok(config)
    }

    pub fn model_client(&self) -> Result<Arc<dyn Model>, ServerError> {
        ChatCompletions::new(&self.base_url, ApiKey::new(self.api_key.clone()))
            .map(|model| Arc::new(model) as Arc<dyn Model>)
            .map_err(|_| ServerError("invalid model config".into()))
    }

    pub fn options(&self) -> CoreOptions {
        let mut prices = canter_core::usage::builtin_prices();
        prices.extend(self.prices.clone());
        CoreOptions {
            prices,
            context_window: self.context_window,
            image_models: self.image_models.clone(),
            ..CoreOptions::default()
        }
    }
}

fn default_models() -> Vec<String> {
    vec!["deepseek-flash".into(), "deepseek-v4-pro".into()]
}

fn default_image_models() -> Vec<String> {
    vec!["deepseek-flash".into()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
