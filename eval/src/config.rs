//! The eval server's `config.json`: exactly one provider and one model, copied
//! from the daily configuration. The API key stays in the eval state directory
//! and is never printed.

use std::fs;

use serde_json::{Map, Value, json};

use crate::Result;

/// Copy one provider/model from `source` into a fresh eval configuration.
pub fn write(
    destination: &std::path::Path,
    source: &std::path::Path,
    qualified: &str,
    auto_compact_tokens: Option<u64>,
    reasoning: Option<&str>,
) -> Result<()> {
    let (provider_name, model_name) = qualified
        .split_once('/')
        .ok_or_else(|| format!("model must be provider/model: {qualified}"))?;
    if provider_name.is_empty() || model_name.is_empty() {
        return Err(format!("model must be provider/model: {qualified}"));
    }
    let bytes =
        fs::read(source).map_err(|error| format!("cannot read {}: {error}", source.display()))?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{} is not valid JSON: {error}", source.display()))?;
    let provider = root
        .get("providers")
        .and_then(|providers| providers.get(provider_name))
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{} has no provider {provider_name}", source.display()))?;
    let model = provider
        .get("models")
        .and_then(|models| models.get(model_name))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            format!(
                "{} provider {provider_name} has no model {model_name}",
                source.display()
            )
        })?;

    let mut provider = provider.clone();
    let mut model = model.clone();
    if let Some(tokens) = auto_compact_tokens {
        model.insert("auto_compact_tokens".into(), json!(tokens));
    }
    if let Some(effort) = reasoning {
        set_reasoning(&mut model, effort)?;
    }
    let mut models = Map::new();
    models.insert(model_name.to_string(), Value::Object(model));
    provider.insert("models".into(), Value::Object(models));

    let mut providers = Map::new();
    providers.insert(provider_name.to_string(), Value::Object(provider));
    let config = json!({
        "model": qualified,
        "providers": Value::Object(providers),
    });
    crate::paths::write_private(
        destination,
        serde_json::to_vec_pretty(&config)
            .map_err(|error| format!("cannot encode the eval configuration: {error}"))?
            .as_slice(),
    )
}

fn set_reasoning(model: &mut Map<String, Value>, effort: &str) -> Result<()> {
    const KNOWN: [&str; 6] = ["none", "low", "medium", "high", "xhigh", "max"];
    if !KNOWN.contains(&effort) {
        return Err(format!(
            "unknown reasoning effort {effort}; use one of {}",
            KNOWN.join(", ")
        ));
    }
    let supported = model
        .entry("supported_reasoning_efforts")
        .or_insert_with(|| json!(["none", "low", "high", "max"]));
    let list = supported
        .as_array_mut()
        .ok_or("supported_reasoning_efforts in the source model is not a list")?;
    if !list.iter().any(|value| value.as_str() == Some(effort)) {
        list.push(json!(effort));
    }
    model.insert("default_reasoning_effort".into(), json!(effort));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(dir: &std::path::Path, with_efforts: bool) -> std::path::PathBuf {
        let model = if with_efforts {
            json!({"context_window": 100000, "auto_compact_tokens": 400000,
                   "supported_reasoning_efforts": ["low", "high"],
                   "default_reasoning_effort": "low"})
        } else {
            json!({"context_window": 100000, "auto_compact_tokens": 400000})
        };
        let config = json!({
            "model": "p/m",
            "providers": {"p": {"base_url": "https://example.test/v1", "api_key": "test-placeholder-key",
                                "models": {"m": model, "other": {"context_window": 200000}}},
                          "q": {"base_url": "https://q.test/v1", "api_key": "other-placeholder-key",
                                "models": {"n": {"context_window": 200000}}}}
        });
        let path = dir.join("source.json");
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        path
    }

    #[test]
    fn copies_one_provider_and_one_model_and_sets_the_run_settings() {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path(), false);
        let destination = dir.path().join("config.json");
        write(&destination, &source, "p/m", Some(60_000), Some("high")).unwrap();
        let config: Value = serde_json::from_slice(&fs::read(&destination).unwrap()).unwrap();
        assert_eq!(config["model"], "p/m");
        let providers = config["providers"].as_object().unwrap();
        assert_eq!(providers.len(), 1);
        let models = providers["p"]["models"].as_object().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models["m"]["auto_compact_tokens"], 60_000);
        assert_eq!(models["m"]["default_reasoning_effort"], "high");
        assert_eq!(providers["p"]["api_key"], "test-placeholder-key");
        let listed: Vec<_> = models["m"]["supported_reasoning_efforts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert!(listed.contains(&"high"));
    }

    #[test]
    fn keeps_an_existing_supported_list() {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path(), true);
        let destination = dir.path().join("config.json");
        write(&destination, &source, "p/m", None, Some("high")).unwrap();
        let config: Value = serde_json::from_slice(&fs::read(&destination).unwrap()).unwrap();
        let efforts = config["providers"]["p"]["models"]["m"]["supported_reasoning_efforts"]
            .as_array()
            .unwrap();
        assert_eq!(efforts, &vec![json!("low"), json!("high")]);
        assert_eq!(
            config["providers"]["p"]["models"]["m"]["auto_compact_tokens"],
            400000
        );
    }
}
