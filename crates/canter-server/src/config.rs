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
    context_window: u64,
    api_key: String,
}

impl Config {
    pub fn load(home: &Path) -> Result<Self, ServerError> {
        let path = home.join("config.json");
        let file = fs::File::open(path)
            .map_err(|_| ServerError("无法读取 config.json；请先配置模型和 API key"))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ServerError("无法将 config.json 权限设为 0600"))?;
        let config: Self =
            serde_json::from_reader(file).map_err(|_| ServerError("config.json 格式无效"))?;
        let url =
            reqwest::Url::parse(&config.base_url).map_err(|_| ServerError("base_url 无效"))?;
        let local = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if url.scheme() == "http" && !local {
            return Err(ServerError(
                "非本机 base_url 必须使用 HTTPS，避免明文传送 API key",
            ));
        }
        if config.model.trim().is_empty() || config.api_key.trim().is_empty() {
            return Err(ServerError("config.json 的 model 和 api_key 不得为空"));
        }
        if config.context_window <= u64::from(CoreOptions::default().max_output_tokens) + 16_384 {
            return Err(ServerError("context_window 必须大于 81920"));
        }
        Ok(config)
    }

    pub fn model_client(&self) -> Result<Arc<dyn Model>, ServerError> {
        ChatCompletions::new(&self.base_url, ApiKey::new(self.api_key.clone()))
            .map(|model| Arc::new(model) as Arc<dyn Model>)
            .map_err(|_| ServerError("模型配置无效"))
    }

    pub fn options(&self) -> CoreOptions {
        CoreOptions {
            context_window: self.context_window,
            ..CoreOptions::default()
        }
    }
}
