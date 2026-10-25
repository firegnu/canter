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
        Ok(config)
    }

    pub fn model_client(&self) -> Result<Arc<dyn Model>, ServerError> {
        ChatCompletions::new(&self.base_url, ApiKey::new(self.api_key.clone()))
            .map(|model| Arc::new(model) as Arc<dyn Model>)
            .map_err(|_| ServerError("invalid model config".into()))
    }

    pub fn options(&self) -> CoreOptions {
        CoreOptions {
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
