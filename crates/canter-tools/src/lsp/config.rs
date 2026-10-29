use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

// Configs can contain credentials. Never derive Debug or include values in errors.
#[derive(Clone, Default, Deserialize)]
pub struct LspConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub servers: BTreeMap<String, serde_json::Map<String, Value>>,
}

#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub extensions: BTreeMap<String, String>,
    pub root_markers: Vec<String>,
    pub initialization_options: Value,
    pub settings: Value,
}

impl LspConfig {
    pub fn resolve(&self) -> Result<BTreeMap<String, ServerConfig>, &'static str> {
        self.resolve_with(include_str!("servers.json"))
    }

    fn resolve_with(&self, builtins: &str) -> Result<BTreeMap<String, ServerConfig>, &'static str> {
        if !self.enabled {
            return Ok(BTreeMap::new());
        }
        let mut merged: BTreeMap<String, serde_json::Map<String, Value>> =
            serde_json::from_str(builtins).map_err(|_| "Invalid built-in LSP configuration")?;
        for (id, fields) in &self.servers {
            merged.entry(id.clone()).or_default().extend(fields.clone());
        }
        merged
            .into_iter()
            .map(|(id, fields)| {
                let config: ServerConfig = serde_json::from_value(Value::Object(fields))
                    .map_err(|_| "Invalid LSP server configuration")?;
                if id.is_empty()
                    || !id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
                {
                    return Err(
                        "LSP server ids must contain only letters, digits, hyphens or underscores",
                    );
                }
                config.validate()?;
                Ok((id, config))
            })
            .collect()
    }
}

impl ServerConfig {
    fn validate(&self) -> Result<(), &'static str> {
        if self.command.trim().is_empty()
            || self.command.contains('\0')
            || self.args.iter().any(|a| a.contains('\0'))
        {
            return Err("LSP command and arguments must be valid executable arguments");
        }
        let program = Path::new(&self.command)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if matches!(program, "npx" | "pnpx" | "bunx")
            || (matches!(program, "npm" | "pnpm" | "yarn" | "bun")
                && self
                    .args
                    .iter()
                    .any(|a| matches!(a.as_str(), "exec" | "x" | "dlx")))
        {
            return Err("LSP commands must use an installed server, not a package installer");
        }
        if self.extensions.is_empty()
            || self.extensions.iter().any(|(ext, language)| {
                ext.trim_start_matches('.').is_empty()
                    || ext.contains(['/', '\\', '\0'])
                    || language.is_empty()
            })
        {
            return Err("LSP servers require extension to languageId mappings");
        }
        if self.root_markers.is_empty()
            || self.root_markers.iter().any(|marker| {
                marker.is_empty()
                    || marker.contains('\0')
                    || !Path::new(marker)
                        .components()
                        .all(|c| matches!(c, Component::Normal(_)))
            })
        {
            return Err("LSP servers require relative project root markers");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overrides_replace_whole_fields_and_preserve_omitted_fields() {
        let config: LspConfig = serde_json::from_value(json!({"enabled":true,"servers":{"fake":{
            "args":[],"settings":{"replacement":true},"initialization_options":null
        }}}))
        .unwrap();
        let servers = config.resolve_with(r#"{"fake":{"command":"fake-lsp","args":["--stdio"],"extensions":{"rs":"rust"},"root_markers":["Cargo.toml"],"settings":{"old":true},"initialization_options":{"old":true}}}"#).unwrap();
        assert_eq!(servers["fake"].command, "fake-lsp");
        assert!(servers["fake"].args.is_empty());
        assert_eq!(servers["fake"].settings, json!({"replacement":true}));
        assert_eq!(servers["fake"].initialization_options, Value::Null);
        assert_eq!(servers["fake"].extensions["rs"], "rust");
    }

    #[test]
    fn builtin_languages_apply_only_when_lsp_is_enabled() {
        assert!(LspConfig::default().resolve().unwrap().is_empty());
        let servers = LspConfig {
            enabled: true,
            ..Default::default()
        }
        .resolve()
        .unwrap();
        assert_eq!(
            servers.keys().map(String::as_str).collect::<Vec<_>>(),
            ["pyright", "rust-analyzer", "typescript"]
        );
        let rust = &servers["rust-analyzer"];
        assert_eq!(rust.extensions["rs"], "rust");
        assert_eq!(rust.root_markers, ["Cargo.lock"]);
        assert_eq!(rust.initialization_options["checkOnSave"], false);
        assert_eq!(rust.settings["rust-analyzer"]["checkOnSave"], false);
        let python = &servers["pyright"];
        assert_eq!(
            (python.command.as_str(), python.args.as_slice()),
            ("pyright-langserver", ["--stdio".to_string()].as_slice())
        );
        assert_eq!(python.extensions["py"], "python");
        assert_eq!(python.extensions["pyi"], "python");
        assert_eq!(
            python.root_markers,
            ["pyproject.toml", "pyrightconfig.json"]
        );
        let typescript = &servers["typescript"];
        assert_eq!(typescript.command, "node_modules/.bin/tsc");
        assert_eq!(typescript.args, ["--lsp", "--stdio"]);
        for (extension, language) in [
            ("ts", "typescript"),
            ("mts", "typescript"),
            ("cts", "typescript"),
            ("tsx", "typescriptreact"),
            ("js", "javascript"),
            ("mjs", "javascript"),
            ("cjs", "javascript"),
            ("jsx", "javascriptreact"),
        ] {
            assert_eq!(typescript.extensions[extension], language);
        }
        assert_eq!(
            typescript.root_markers,
            ["tsconfig.json", "jsconfig.json", "package.json"]
        );
    }
}
