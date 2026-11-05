use crate::chatgpt_auth::{ChatgptAuth, RecordStatus};
use canter_core::{CoreOptions, ModelLimits, ModelReasoning};
use canter_protocol::ReasoningEffort;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Read,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
#[path = "model_catalog_tests.rs"]
mod tests;

const REFRESH_AFTER: Duration = Duration::from_secs(5 * 60);
const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogModel {
    slug: String,
    display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supported_reasoning_efforts: Option<Vec<ReasoningEffort>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input_modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    apply_patch_tool_type: Option<String>,
}

impl CatalogModel {
    fn normalized(mut self) -> Option<Self> {
        if self.slug.trim().is_empty() {
            return None;
        }
        if self.display_name.trim().is_empty() {
            self.display_name = self.slug.clone();
        }
        self.context_window = self.context_window.filter(|window| *window > 81_920);
        self.apply_patch_tool_type = self.apply_patch_tool_type.filter(|kind| kind == "freeform");
        if let Some(efforts) = &mut self.supported_reasoning_efforts {
            efforts.retain(|effort| *effort != ReasoningEffort::Unknown);
            efforts.dedup();
            if efforts.is_empty() {
                self.supported_reasoning_efforts = None;
                self.default_reasoning_effort = None;
            }
        }
        if self.default_reasoning_effort == Some(ReasoningEffort::Unknown) {
            self.default_reasoning_effort = None;
        }
        Some(self)
    }

    fn reasoning(&self) -> Option<ModelReasoning> {
        let supported = self.supported_reasoning_efforts.clone()?;
        let default = self.default_reasoning_effort?;
        supported
            .contains(&default)
            .then_some(ModelReasoning { supported, default })
    }

    fn missing(&self) -> Vec<String> {
        let mut missing = Vec::new();
        if self.context_window.is_none() {
            missing.push("contextWindow".into());
        }
        if self.reasoning().is_none() {
            missing.push("reasoningEffort".into());
        }
        if self.input_modalities.is_none() {
            missing.push("imageInput".into());
        }
        missing
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedCatalog {
    version: u8,
    fetched_at: i64,
    models: Vec<CatalogModel>,
    #[serde(default)]
    known_models: Vec<CatalogModel>,
}

impl CachedCatalog {
    fn normalized(mut self) -> Option<Self> {
        if self.version != 1 {
            return None;
        }
        self.models = normalize_models(self.models);
        let mut known = BTreeMap::new();
        for model in self.known_models.into_iter().chain(self.models.clone()) {
            if let Some(model) = model.normalized() {
                known.entry(model.slug.clone()).or_insert(model);
            }
        }
        self.known_models = known.into_values().collect();
        Some(self)
    }

    fn new(fetched_at: i64, models: Vec<CatalogModel>, previous: Option<&Self>) -> Self {
        let mut known = BTreeMap::new();
        for model in previous
            .into_iter()
            .flat_map(|cache| cache.known_models.iter())
            .chain(models.iter())
        {
            known.insert(model.slug.clone(), model.clone());
        }
        Self {
            version: 1,
            fetched_at,
            models,
            known_models: known.into_values().collect(),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct Pause {
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
impl Pause {
    async fn wait(&self) {
        if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[derive(Default)]
struct State {
    account: Option<String>,
    generation: u64,
    revision: u64,
    models: Vec<CatalogModel>,
    known: BTreeMap<String, CatalogModel>,
    caches: BTreeMap<String, CachedCatalog>,
    fetched_at: Option<i64>,
    refresh_failed: bool,
    refreshing: bool,
}

pub(crate) struct ModelCatalog {
    home: PathBuf,
    endpoint: reqwest::Url,
    client: reqwest::Client,
    state: RwLock<State>,
    activation: tokio::sync::Mutex<()>,
    #[cfg(test)]
    pause_after_activate: Pause,
}

impl ModelCatalog {
    pub fn new(home: PathBuf, endpoint: Option<reqwest::Url>) -> Arc<Self> {
        let endpoint = endpoint.unwrap_or_else(|| {
            reqwest::Url::parse("https://api.openai.com/v1/models").expect("fixed catalog URL")
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("canter/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("catalog HTTP client");
        let mut state = State::default();
        for (account, cache) in load_caches(&cache_dir(&home)) {
            for model in cache.models.iter().chain(&cache.known_models) {
                state.known.insert(model.slug.clone(), model.clone());
            }
            state.caches.insert(account, cache);
        }
        Arc::new(Self {
            home,
            endpoint,
            client,
            state: RwLock::new(state),
            activation: Default::default(),
            #[cfg(test)]
            pause_after_activate: Pause::default(),
        })
    }

    pub fn start(self: &Arc<Self>, auth: ChatgptAuth) {
        self.spawn_refresh(auth, true);
    }

    pub async fn account_changed(self: &Arc<Self>, auth: ChatgptAuth) {
        self.activate(&auth).await;
        self.spawn_refresh(auth, true);
    }

    pub async fn logout(self: &Arc<Self>, auth: &ChatgptAuth, account: &str) {
        {
            let mut state = self.state.write().unwrap();
            state.caches.remove(account);
            if state.account.as_deref() == Some(account) {
                state.generation = state.generation.wrapping_add(1);
                state.revision = state.revision.wrapping_add(1);
                state.account = None;
                state.models.clear();
                state.fetched_at = None;
                state.refresh_failed = false;
                state.refreshing = false;
            }
        }
        if valid_account(account) {
            let _ = fs::remove_file(cache_dir(&self.home).join(format!("{account}.json")));
        }
        self.activate(auth).await;
    }

    pub fn refresh_stale(self: &Arc<Self>, auth: ChatgptAuth) {
        self.spawn_refresh(auth, false);
    }

    pub fn apply(
        &self,
        provider: Option<&str>,
        models: Vec<String>,
        options: CoreOptions,
    ) -> (Vec<String>, CoreOptions) {
        let (models, options, _) = self.apply_versioned(provider, models, options);
        (models, options)
    }

    pub fn apply_versioned(
        &self,
        provider: Option<&str>,
        models: Vec<String>,
        mut options: CoreOptions,
    ) -> (Vec<String>, CoreOptions, u64) {
        let state = self.state.read().unwrap();
        let revision = state.revision;
        let Some(provider) = provider else {
            return (models, options, revision);
        };
        let prefix = format!("{provider}/");
        let (configured, mut models): (Vec<_>, Vec<_>) = models
            .into_iter()
            .partition(|model| model.starts_with(&prefix));
        let mut merged = Vec::new();
        for model in &state.models {
            merged.push(format!("{provider}/{}", model.slug));
        }
        for model in configured {
            if !merged.contains(&model) {
                merged.push(model);
            }
        }
        models.extend(merged);
        for model in state.known.values() {
            let id = format!("{provider}/{}", model.slug);
            if model.apply_patch_tool_type.as_deref() == Some("freeform") {
                options.apply_patch_models.insert(id.clone());
            }
            if !options.model_limits.contains_key(&id) {
                options.model_limits.insert(
                    id.clone(),
                    ModelLimits {
                        context_window: model.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW),
                        auto_compact_tokens: None,
                    },
                );
                options.model_reasoning.insert(
                    id.clone(),
                    model.reasoning().unwrap_or_else(|| ModelReasoning {
                        supported: vec![ReasoningEffort::Medium],
                        default: ReasoningEffort::Medium,
                    }),
                );
                if model
                    .input_modalities
                    .as_ref()
                    .is_some_and(|modalities| modalities.iter().any(|kind| kind == "image"))
                    && !options.image_models.contains(&id)
                {
                    options.image_models.push(id.clone());
                }
            }
        }
        (models, options, revision)
    }

    pub fn details(&self, id: &str) -> Option<(String, Vec<String>)> {
        let (_, slug) = id.split_once('/')?;
        let state = self.state.read().unwrap();
        let model = state.models.iter().find(|model| model.slug == slug)?;
        Some((model.display_name.clone(), model.missing()))
    }

    pub fn status(&self, provider: &str) -> Option<canter_protocol::ModelCatalogStatus> {
        let state = self.state.read().unwrap();
        (state.account.is_some() && (state.fetched_at.is_some() || state.refresh_failed)).then(
            || canter_protocol::ModelCatalogStatus {
                provider: provider.into(),
                stale: state.refresh_failed && state.fetched_at.is_some(),
                refresh_failed: state.refresh_failed,
                fetched_at: state.fetched_at,
            },
        )
    }

    fn spawn_refresh(self: &Arc<Self>, auth: ChatgptAuth, force: bool) {
        let catalog = self.clone();
        tokio::spawn(async move {
            let Some((account, activation_generation)) = catalog.activate(&auth).await else {
                return;
            };
            #[cfg(test)]
            catalog.pause_after_activate.wait().await;
            let generation = {
                let mut state = catalog.state.write().unwrap();
                if state.generation != activation_generation
                    || state.account.as_deref() != Some(&account)
                {
                    return;
                }
                if state.refreshing {
                    return;
                }
                let stale = state.fetched_at.is_none_or(|fetched_at| {
                    now_ms().saturating_sub(fetched_at) > REFRESH_AFTER.as_millis() as i64
                });
                if !force && !stale {
                    return;
                }
                state.refreshing = true;
                state.generation
            };
            let result = catalog.fetch(&auth, &account).await;
            let mut state = catalog.state.write().unwrap();
            if state.generation != generation || state.account.as_deref() != Some(&account) {
                if state.generation == generation {
                    state.refreshing = false;
                }
                return;
            }
            state.refreshing = false;
            match result {
                Ok(models) => {
                    let fetched_at = now_ms();
                    let cache = CachedCatalog::new(fetched_at, models, state.caches.get(&account));
                    if save_cache(&cache_dir(&catalog.home), &account, &cache).is_err() {
                        state.refresh_failed = true;
                        return;
                    }
                    let changed = state.models != cache.models
                        || cache
                            .models
                            .iter()
                            .chain(&cache.known_models)
                            .any(|model| state.known.get(&model.slug) != Some(model));
                    if changed {
                        state.revision = state.revision.wrapping_add(1);
                    }
                    for model in cache.models.iter().chain(&cache.known_models) {
                        state.known.insert(model.slug.clone(), model.clone());
                    }
                    state.models = cache.models.clone();
                    state.fetched_at = Some(fetched_at);
                    state.caches.insert(account, cache);
                    state.refresh_failed = false;
                }
                Err(()) => state.refresh_failed = true,
            }
        });
    }

    async fn activate(&self, auth: &ChatgptAuth) -> Option<(String, u64)> {
        let _activation = self.activation.lock().await;
        let current = auth.current_record().await;
        let account = match current {
            Some(account)
                if auth.records().await.into_iter().any(|record| {
                    record.id == account && record.status == RecordStatus::LoggedIn
                }) =>
            {
                Some(account)
            }
            _ => None,
        };
        let mut state = self.state.write().unwrap();
        if state.account == account {
            return account.map(|account| (account, state.generation));
        }
        state.generation = state.generation.wrapping_add(1);
        state.revision = state.revision.wrapping_add(1);
        state.account = account.clone();
        state.models.clear();
        state.fetched_at = None;
        state.refresh_failed = false;
        state.refreshing = false;
        if let Some(cache) = account
            .as_ref()
            .and_then(|id| state.caches.get(id))
            .cloned()
        {
            state.models = cache.models;
            state.fetched_at = Some(cache.fetched_at);
        }
        account.map(|account| (account, state.generation))
    }

    async fn fetch(&self, auth: &ChatgptAuth, account: &str) -> Result<Vec<CatalogModel>, ()> {
        let mut token = auth.access_token(account).await.map_err(|_| ())?;
        for attempt in 0..2 {
            let header = token.authorization_header().map_err(|_| ())?;
            let response = self
                .client
                .get(self.endpoint.clone())
                .header(reqwest::header::AUTHORIZATION, header)
                .send()
                .await
                .map_err(|_| ())?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                token = auth
                    .recover_unauthorized(&mut token)
                    .await
                    .map_err(|_| ())?;
                continue;
            }
            if !response.status().is_success() {
                return Err(());
            }
            let value: serde_json::Value = response.json().await.map_err(|_| ())?;
            let models = value
                .get("models")
                .and_then(serde_json::Value::as_array)
                .ok_or(())?
                .iter()
                .filter_map(parse_model)
                .collect();
            return Ok(normalize_models(models));
        }
        Err(())
    }
}

fn parse_model(value: &serde_json::Value) -> Option<CatalogModel> {
    let object = value.as_object()?;
    if object.get("visibility")?.as_str()? != "list" {
        return None;
    }
    let slug = object.get("slug")?.as_str()?.to_owned();
    let display_name = object
        .get("display_name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&slug)
        .to_owned();
    let context_window = object
        .get("context_window")
        .and_then(serde_json::Value::as_u64);
    let supported_reasoning_efforts = object
        .get("supported_reasoning_levels")
        .and_then(serde_json::Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| level.get("effort")?.as_str())
                .filter_map(|effort| serde_json::from_str(&format!("\"{effort}\"")).ok())
                .collect()
        });
    let default_reasoning_effort = object
        .get("default_reasoning_level")
        .and_then(serde_json::Value::as_str)
        .and_then(|effort| serde_json::from_str(&format!("\"{effort}\"")).ok());
    let input_modalities = object
        .get("input_modalities")
        .and_then(serde_json::Value::as_array)
        .map(|modalities| {
            modalities
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        });
    CatalogModel {
        slug,
        display_name,
        context_window,
        supported_reasoning_efforts,
        default_reasoning_effort,
        input_modalities,
        apply_patch_tool_type: object
            .get("apply_patch_tool_type")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    }
    .normalized()
}

fn normalize_models(models: Vec<CatalogModel>) -> Vec<CatalogModel> {
    let mut seen = HashSet::new();
    models
        .into_iter()
        .filter_map(CatalogModel::normalized)
        .filter(|model| seen.insert(model.slug.clone()))
        .collect()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn cache_dir(home: &Path) -> PathBuf {
    home.join("models/chatgpt")
}

fn valid_account(account: &str) -> bool {
    !account.is_empty()
        && account
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn load_caches(dir: &Path) -> Vec<(String, CachedCatalog)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            let account = name.strip_suffix(".json")?.to_owned();
            if !valid_account(&account) {
                return None;
            }
            let cache = load_cache(&entry.path())?;
            Some((account, cache))
        })
        .collect()
}

fn load_cache(path: &Path) -> Option<CachedCatalog> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    let uid = unsafe { nix::libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return None;
    }
    let mut contents = Vec::new();
    file.take(4 * 1024 * 1024).read_to_end(&mut contents).ok()?;
    serde_json::from_slice::<CachedCatalog>(&contents)
        .ok()?
        .normalized()
}

fn save_cache(dir: &Path, account: &str, cache: &CachedCatalog) -> Result<(), ()> {
    if !valid_account(account) {
        return Err(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|_| ())?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|_| ())?;
    let temporary = dir.join(format!(".{account}.{}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW);
    let result = (|| {
        let mut file = options.open(&temporary).map_err(|_| ())?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ())?;
        std::io::Write::write_all(&mut file, &serde_json::to_vec(cache).map_err(|_| ())?)
            .map_err(|_| ())?;
        file.sync_all().map_err(|_| ())?;
        drop(file);
        fs::rename(&temporary, dir.join(format!("{account}.json"))).map_err(|_| ())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
