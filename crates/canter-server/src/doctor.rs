use super::*;
use canter_protocol::{DoctorCategory, DoctorCheck, DoctorResponse, DoctorStatus};
use futures_util::StreamExt;
use std::path::Path;

fn check(
    id: &str,
    name: &str,
    category: DoctorCategory,
    status: DoctorStatus,
    reason: Option<String>,
    fix: Option<String>,
) -> DoctorCheck {
    DoctorCheck {
        id: id.into(),
        name: name.into(),
        category,
        status,
        reason,
        fix,
    }
}

fn ok(id: &str, name: &str, category: DoctorCategory) -> DoctorCheck {
    check(id, name, category, DoctorStatus::Ok, None, None)
}

fn fail(
    id: &str,
    name: &str,
    category: DoctorCategory,
    reason: String,
    fix: String,
) -> DoctorCheck {
    check(
        id,
        name,
        category,
        DoctorStatus::Fail,
        Some(reason),
        Some(fix),
    )
}

fn skip(id: &str, name: &str, category: DoctorCategory, reason: String) -> DoctorCheck {
    check(id, name, category, DoctorStatus::Skip, Some(reason), None)
}

fn state(home: &Path) -> DoctorCheck {
    use std::os::unix::ffi::OsStrExt;
    let directory = std::fs::metadata(home).is_ok_and(|metadata| metadata.is_dir());
    let writable = std::ffi::CString::new(home.as_os_str().as_bytes())
        .is_ok_and(|path| unsafe { nix::libc::access(path.as_ptr(), nix::libc::W_OK) } == 0);
    if directory && writable {
        ok("state", "CANTER_HOME", DoctorCategory::Configuration)
    } else {
        fail(
            "state",
            "CANTER_HOME",
            DoctorCategory::Configuration,
            "The state directory is not a writable directory.".into(),
            "Check CANTER_HOME and its directory permissions.".into(),
        )
    }
}

fn optional_json(home: &Path, file: &str, id: &str) -> DoctorCheck {
    let path = home.join(file);
    let valid = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .map(|_| ()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(()),
        Err(_) => None,
    };
    match valid {
        Some(()) => ok(id, file, DoctorCategory::Configuration),
        None => fail(
            id,
            file,
            DoctorCategory::Configuration,
            format!("{file} cannot be read or is not valid JSON."),
            format!("Fix or remove {file} in CANTER_HOME."),
        ),
    }
}

fn rules_reason(error: std::io::Error) -> String {
    let message = error.to_string();
    if let Some(field) = message
        .strip_prefix("fileRules[")
        .and_then(|message| message.split_once(']').map(|(index, _)| index))
    {
        return format!("rules.json is invalid: fileRules[{field}]: invalid pattern.");
    }
    if message.starts_with("rules[") && message.ends_with("empty or invalid command prefix") {
        return format!("rules.json is invalid: {message}.");
    }
    if let Some(location) = message.rfind(" at line ") {
        return format!("rules.json is invalid{}.", &message[location..]);
    }
    "rules.json cannot be read or is invalid.".into()
}

fn rules(home: &Path) -> DoctorCheck {
    match canter_tools::rules::list(&home.join("rules.json")) {
        Ok(_) => ok("rules", "rules.json", DoctorCategory::Configuration),
        Err(error) => fail(
            "rules",
            "rules.json",
            DoctorCategory::Configuration,
            rules_reason(error),
            "Fix rules.json in CANTER_HOME; doctor will not change it.".into(),
        ),
    }
}

fn config(home: &Path) -> DoctorCheck {
    match crate::config::Config::load_read_only(home) {
        Ok(_) => ok("config", "config.json", DoctorCategory::Configuration),
        Err(error) => fail(
            "config",
            "config.json",
            DoctorCategory::Configuration,
            error.to_string(),
            "Fix config.json in CANTER_HOME; doctor will not change it.".into(),
        ),
    }
}

async fn agents(app: &App) -> DoctorCheck {
    let mut options = app.metadata().2;
    options.agent_roles_directory = Some(app.home.join("agents"));
    let mcp_tools = app.reload.as_ref().map_or_else(
        || app.mcp.snapshot(),
        |reload| {
            use canter_core::ConfigurationSource;
            reload.snapshot().mcp_tools
        },
    );
    match canter_core::roles::inspect(&options, &mcp_tools).await {
        Ok(roles) => {
            let errors = roles
                .into_iter()
                .filter_map(|role| role.error.map(|error| format!("{}: {error}", role.name)))
                .collect::<Vec<_>>();
            if errors.is_empty() {
                ok("agents", "agents/*.md", DoctorCategory::Configuration)
            } else {
                fail(
                    "agents",
                    "agents/*.md",
                    DoctorCategory::Configuration,
                    errors.join("; "),
                    "Fix or remove the listed files in CANTER_HOME/agents.".into(),
                )
            }
        }
        Err(_) => skip(
            "agents",
            "agents/*.md",
            DoctorCategory::Configuration,
            "Could not validate role files.".into(),
        ),
    }
}

async fn authentication(app: &App, configured: bool) -> DoctorCheck {
    if !configured {
        return skip(
            "auth.chatgpt",
            "ChatGPT sign-in",
            DoctorCategory::Authentication,
            "ChatGPT is not configured.".into(),
        );
    }
    let Some(source) = app.reload.as_ref().map(|reload| reload.auth.clone()) else {
        return skip(
            "auth.chatgpt",
            "ChatGPT sign-in",
            DoctorCategory::Authentication,
            "The ChatGPT account service is unavailable.".into(),
        );
    };
    let Some(auth) = source.existing().await else {
        return skip(
            "auth.chatgpt",
            "ChatGPT sign-in",
            DoctorCategory::Authentication,
            "The ChatGPT account store is not initialized.".into(),
        );
    };
    let current = auth.current_record().await;
    let record = auth
        .records()
        .await
        .into_iter()
        .find(|record| Some(&record.id) == current.as_ref());
    match record.map(|record| record.status) {
        Some(crate::chatgpt_auth::RecordStatus::LoggedIn) => ok(
            "auth.chatgpt",
            "ChatGPT sign-in",
            DoctorCategory::Authentication,
        ),
        Some(_) => fail(
            "auth.chatgpt",
            "ChatGPT sign-in",
            DoctorCategory::Authentication,
            "ChatGPT requires sign-in or plan authorization.".into(),
            "Run canter auth login chatgpt.".into(),
        ),
        None => fail(
            "auth.chatgpt",
            "ChatGPT sign-in",
            DoctorCategory::Authentication,
            "No current ChatGPT account is logged in.".into(),
            "Run canter auth login chatgpt.".into(),
        ),
    }
}

async fn chatgpt_provider(app: &App, name: String) -> DoctorCheck {
    let id = format!("provider.{name}");
    let display = format!("Provider {name}");
    let Some(source) = app.reload.as_ref().map(|reload| reload.auth.clone()) else {
        return skip(
            &id,
            &display,
            DoctorCategory::Provider,
            "The ChatGPT account service is unavailable.".into(),
        );
    };
    let Some(auth) = source.existing().await else {
        return skip(
            &id,
            &display,
            DoctorCategory::Provider,
            "The ChatGPT account store is not initialized.".into(),
        );
    };
    let Some(account) = auth.current_record().await else {
        return fail(
            &id,
            &display,
            DoctorCategory::Provider,
            format!("Provider {name} has no current ChatGPT account."),
            "Run canter auth login chatgpt.".into(),
        );
    };
    use crate::model_catalog::CatalogCheckError;
    match source.catalog().check(&auth, &account).await {
        Ok(()) => ok(&id, &display, DoctorCategory::Provider),
        Err(CatalogCheckError::Status(status @ (401 | 403))) => fail(
            &id,
            &display,
            DoctorCategory::Provider,
            format!("Provider {name} rejected the current ChatGPT authorization (HTTP {status})."),
            "Run canter auth login chatgpt.".into(),
        ),
        Err(CatalogCheckError::Auth(error))
            if matches!(
                error.kind,
                crate::chatgpt_auth::ErrorKind::NotLoggedIn
                    | crate::chatgpt_auth::ErrorKind::NeedsLogin
                    | crate::chatgpt_auth::ErrorKind::PlanNotAuthorized
                    | crate::chatgpt_auth::ErrorKind::Unauthorized
            ) =>
        {
            fail(
                &id,
                &display,
                DoctorCategory::Provider,
                format!("Provider {name} requires ChatGPT sign-in or authorization."),
                "Run canter auth login chatgpt.".into(),
            )
        }
        Err(CatalogCheckError::Timeout) => fail(
            &id,
            &display,
            DoctorCategory::Provider,
            format!("Provider {name} timed out while reading its model list."),
            "Check network connectivity to the ChatGPT model catalog.".into(),
        ),
        Err(CatalogCheckError::Network) => fail(
            &id,
            &display,
            DoctorCategory::Provider,
            format!("Could not connect to provider {name}."),
            "Check network connectivity to the ChatGPT model catalog.".into(),
        ),
        Err(CatalogCheckError::Auth(_)) => skip(
            &id,
            &display,
            DoctorCategory::Provider,
            "Could not read or refresh ChatGPT authorization.".into(),
        ),
        Err(CatalogCheckError::Status(status)) => skip(
            &id,
            &display,
            DoctorCategory::Provider,
            format!("Provider {name} model list returned HTTP {status}."),
        ),
        Err(CatalogCheckError::InvalidResponse) => skip(
            &id,
            &display,
            DoctorCategory::Provider,
            format!("Provider {name} returned an unrecognized model list."),
        ),
    }
}

async fn provider(app: &App, provider: crate::config::DoctorProvider) -> DoctorCheck {
    let (name, base_url, api_key) = match provider {
        crate::config::DoctorProvider::ChatCompletions {
            name,
            base_url,
            api_key,
        } => (name, base_url, api_key),
        crate::config::DoctorProvider::Chatgpt { name } => {
            return chatgpt_provider(app, name).await;
        }
    };
    let id = format!("provider.{name}");
    let display = format!("Provider {name}");
    if api_key.trim().is_empty() {
        return fail(
            &id,
            &display,
            DoctorCategory::Provider,
            format!("Provider {name} has no API key."),
            format!("Set providers.{name}.api_key in config.json."),
        );
    }
    let endpoint = match reqwest::Url::parse(&format!("{}/models", base_url.trim_end_matches('/')))
    {
        Ok(endpoint) => endpoint,
        Err(_) => {
            return fail(
                &id,
                &display,
                DoctorCategory::Provider,
                format!("Provider {name} has an invalid base_url."),
                format!("Fix providers.{name}.base_url in config.json."),
            );
        }
    };
    let client = match reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .user_agent(concat!("canter/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            return skip(
                &id,
                &display,
                DoctorCategory::Provider,
                "Could not create the provider HTTP client.".into(),
            );
        }
    };
    let mut authorization =
        match reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}")) {
            Ok(value) => value,
            Err(_) => {
                return fail(
                    &id,
                    &display,
                    DoctorCategory::Provider,
                    format!("Provider {name} has an invalid API key."),
                    format!("Replace providers.{name}.api_key in config.json."),
                );
            }
        };
    authorization.set_sensitive(true);
    let result = client
        .get(endpoint)
        .header(reqwest::header::AUTHORIZATION, authorization)
        .send()
        .await;
    let response = match result {
        Ok(response) => response,
        Err(error) if error.is_timeout() => {
            return fail(
                &id,
                &display,
                DoctorCategory::Provider,
                format!("Provider {name} timed out while reading its model list."),
                format!("Check providers.{name}.base_url and network connectivity."),
            );
        }
        Err(_) => {
            return fail(
                &id,
                &display,
                DoctorCategory::Provider,
                format!("Could not connect to provider {name}."),
                format!("Check providers.{name}.base_url and network connectivity."),
            );
        }
    };
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return fail(
            &id,
            &display,
            DoctorCategory::Provider,
            format!(
                "Provider {name} rejected its API key (HTTP {}).",
                status.as_u16()
            ),
            format!("Replace providers.{name}.api_key in config.json."),
        );
    }
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::METHOD_NOT_ALLOWED
    {
        return skip(
            &id,
            &display,
            DoctorCategory::Provider,
            format!("Provider {name} has no recognized model list endpoint."),
        );
    }
    if !status.is_success() {
        return skip(
            &id,
            &display,
            DoctorCategory::Provider,
            format!(
                "Provider {name} model list returned HTTP {}.",
                status.as_u16()
            ),
        );
    }
    match response.json::<serde_json::Value>().await {
        Ok(value) if value.get("data").is_some_and(serde_json::Value::is_array) => {
            ok(&id, &display, DoctorCategory::Provider)
        }
        _ => skip(
            &id,
            &display,
            DoctorCategory::Provider,
            format!("Provider {name} returned an unrecognized model list."),
        ),
    }
}

#[cfg(test)]
static SANDBOX_FAILURES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
    std::sync::LazyLock::new(Default::default);

#[cfg(all(test, target_os = "macos"))]
pub(super) fn fail_sandbox_for_test(home: PathBuf) {
    SANDBOX_FAILURES.lock().unwrap().insert(home);
}

async fn sandbox(config: &crate::config::Config, home: &Path) -> DoctorCheck {
    if !config.options().sandbox {
        return skip(
            "sandbox",
            "Sandbox",
            DoctorCategory::Sandbox,
            "Sandbox is disabled in config.json.".into(),
        );
    }
    if !cfg!(target_os = "macos") {
        return fail(
            "sandbox",
            "Sandbox",
            DoctorCategory::Sandbox,
            "Seatbelt sandboxing is unavailable on this operating system.".into(),
            "Run on macOS or set sandbox to false in config.json.".into(),
        );
    }
    let mut command = tokio::process::Command::new("/usr/bin/sandbox-exec");
    command
        .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(2), command.output()).await;
    #[cfg(test)]
    let forced = SANDBOX_FAILURES.lock().unwrap().contains(home);
    #[cfg(not(test))]
    let forced = {
        let _ = home;
        false
    };
    if !forced && result.is_ok_and(|result| result.is_ok_and(|output| output.status.success())) {
        ok("sandbox", "Sandbox", DoctorCategory::Sandbox)
    } else {
        fail(
            "sandbox",
            "Sandbox",
            DoctorCategory::Sandbox,
            "The macOS Seatbelt sandbox probe failed or timed out.".into(),
            "Check /usr/bin/sandbox-exec and macOS Seatbelt availability.".into(),
        )
    }
}

fn mcp(app: &App) -> Vec<DoctorCheck> {
    let status = app
        .reload
        .as_ref()
        .map_or_else(|| app.mcp.status(), |reload| reload.mcp_status());
    status
        .into_iter()
        .map(|server| {
            let id = format!("mcp.{}", server.name);
            let name = format!("MCP {}", server.name);
            if server.connected {
                ok(&id, &name, DoctorCategory::Mcp)
            } else if server
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Login required"))
            {
                fail(
                    &id,
                    &name,
                    DoctorCategory::Mcp,
                    format!("MCP service {} needs authorization.", server.name),
                    format!("Run canter mcp login {}.", server.name),
                )
            } else {
                fail(
                    &id,
                    &name,
                    DoctorCategory::Mcp,
                    format!("MCP service {} is not connected.", server.name),
                    format!(
                        "Check {} in config.json; doctor does not reconnect it.",
                        server.name
                    ),
                )
            }
        })
        .collect()
}

async fn active_configuration(app: &App) -> DoctorCheck {
    let Some(reload) = &app.reload else {
        return skip(
            "config.active",
            "Active configuration",
            DoctorCategory::Configuration,
            "The running configuration state is unavailable.".into(),
        );
    };
    match reload.warning().await {
        Some(warning) => fail(
            "config.active",
            "Active configuration",
            DoctorCategory::Configuration,
            warning,
            "Fix config.json; the previous valid configuration remains active.".into(),
        ),
        None => ok(
            "config.active",
            "Active configuration",
            DoctorCategory::Configuration,
        ),
    }
}

pub(super) async fn handle(
    State(app): State<Arc<App>>,
    params: Result<Query<DoctorParams>, QueryRejection>,
) -> Result<Json<DoctorResponse>, ApiError> {
    let Query(params) = params.map_err(|_| ApiError::invalid())?;
    Ok(Json(run(app, params).await))
}

type CheckFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = (String, Vec<DoctorCheck>)> + Send>>;

fn add_check(
    tasks: &futures_util::stream::FuturesUnordered<CheckFuture>,
    expected: &mut Vec<DoctorCheck>,
    id: &str,
    name: &str,
    category: DoctorCategory,
    timeout: Duration,
    future: impl std::future::Future<Output = Vec<DoctorCheck>> + Send + 'static,
) {
    let id = id.to_owned();
    let name = name.to_owned();
    expected.push(fail(
        &id,
        &name,
        category,
        "Check exceeded doctor's 8-second total time limit.".into(),
        "Check local filesystem and network responsiveness; rerun canter doctor.".into(),
    ));
    let task_id = id.clone();
    tasks.push(Box::pin(async move {
        let checks = match tokio::time::timeout(timeout, future).await {
            Ok(checks) => checks,
            Err(_) => vec![fail(
                &id,
                &name,
                category,
                "Check timed out.".into(),
                "Check local filesystem and network responsiveness; rerun canter doctor.".into(),
            )],
        };
        (task_id, checks)
    }));
}

fn local_check(
    id: &str,
    name: &str,
    category: DoctorCategory,
    work: impl FnOnce() -> DoctorCheck + Send + 'static,
) -> impl std::future::Future<Output = Vec<DoctorCheck>> + Send + 'static {
    let id = id.to_owned();
    let name = name.to_owned();
    async move {
        vec![match tokio::task::spawn_blocking(work).await {
            Ok(check) => check,
            Err(_) => fail(
                &id,
                &name,
                category,
                "Check could not be completed.".into(),
                "Rerun canter doctor; no changes were made.".into(),
            ),
        }]
    }
}

pub(super) async fn run(app: Arc<App>, params: DoctorParams) -> DoctorResponse {
    let version = if params.client_version == crate::VERSION
        && params.client_protocol_version == PROTOCOL_VERSION
    {
        ok("version", "Client/server versions", DoctorCategory::Version)
    } else {
        fail(
            "version",
            "Client/server versions",
            DoctorCategory::Version,
            format!(
                "Server is {} (protocol {}); client is {} (protocol {}).",
                crate::VERSION,
                PROTOCOL_VERSION,
                params.client_version,
                params.client_protocol_version
            ),
            "Install matching versions and run canter serve --replace.".into(),
        )
    };
    let active = app.reload.as_ref().map_or_else(
        || crate::config::Config::load_read_only(&app.home).ok(),
        |reload| Some(reload.doctor_config()),
    );
    let mut tasks = futures_util::stream::FuturesUnordered::new();
    let mut expected = Vec::new();
    for (id, name, work) in [
        ("config", "config.json", config as fn(&Path) -> DoctorCheck),
        ("rules", "rules.json", rules as fn(&Path) -> DoctorCheck),
    ] {
        let home = app.home.clone();
        add_check(
            &tasks,
            &mut expected,
            id,
            name,
            DoctorCategory::Configuration,
            Duration::from_secs(2),
            local_check(id, name, DoctorCategory::Configuration, move || work(&home)),
        );
    }
    let home = app.home.clone();
    add_check(
        &tasks,
        &mut expected,
        "tui",
        "tui.json",
        DoctorCategory::Configuration,
        Duration::from_secs(2),
        local_check(
            "tui",
            "tui.json",
            DoctorCategory::Configuration,
            move || optional_json(&home, "tui.json", "tui"),
        ),
    );
    let home = app.home.clone();
    add_check(
        &tasks,
        &mut expected,
        "state",
        "CANTER_HOME",
        DoctorCategory::Configuration,
        Duration::from_secs(2),
        local_check(
            "state",
            "CANTER_HOME",
            DoctorCategory::Configuration,
            move || state(&home),
        ),
    );
    let task_app = app.clone();
    add_check(
        &tasks,
        &mut expected,
        "config.active",
        "Active configuration",
        DoctorCategory::Configuration,
        Duration::from_secs(2),
        async move { vec![active_configuration(&task_app).await] },
    );
    let task_app = app.clone();
    add_check(
        &tasks,
        &mut expected,
        "agents",
        "agents/*.md",
        DoctorCategory::Configuration,
        Duration::from_secs(2),
        async move { vec![agents(&task_app).await] },
    );
    let task_app = app.clone();
    let configured = active
        .as_ref()
        .is_some_and(|config| config.catalog_provider().is_some());
    add_check(
        &tasks,
        &mut expected,
        "auth.chatgpt",
        "ChatGPT sign-in",
        DoctorCategory::Authentication,
        Duration::from_secs(5),
        async move { vec![authentication(&task_app, configured).await] },
    );
    for target in active
        .as_ref()
        .map(|config| config.doctor_providers())
        .unwrap_or_default()
    {
        let name = match &target {
            crate::config::DoctorProvider::ChatCompletions { name, .. }
            | crate::config::DoctorProvider::Chatgpt { name } => name.clone(),
        };
        let task_app = app.clone();
        add_check(
            &tasks,
            &mut expected,
            &format!("provider.{name}"),
            &format!("Provider {name}"),
            DoctorCategory::Provider,
            Duration::from_secs(5),
            async move { vec![provider(&task_app, target).await] },
        );
    }
    let task_app = app.clone();
    add_check(
        &tasks,
        &mut expected,
        "mcp",
        "MCP",
        DoctorCategory::Mcp,
        Duration::from_secs(2),
        async move {
            match tokio::task::spawn_blocking(move || mcp(&task_app)).await {
                Ok(checks) => checks,
                Err(_) => vec![fail(
                    "mcp",
                    "MCP",
                    DoctorCategory::Mcp,
                    "Could not read MCP connection state.".into(),
                    "Rerun canter doctor; doctor did not reconnect any service.".into(),
                )],
            }
        },
    );
    add_check(
        &tasks,
        &mut expected,
        "sandbox",
        "Sandbox",
        DoctorCategory::Sandbox,
        Duration::from_secs(3),
        async move {
            vec![match active.as_ref() {
                Some(config) => sandbox(config, &app.home).await,
                None => skip(
                    "sandbox",
                    "Sandbox",
                    DoctorCategory::Sandbox,
                    "Could not determine whether the sandbox is enabled.".into(),
                ),
            }]
        },
    );
    let mut data = vec![version];
    let deadline = tokio::time::sleep(Duration::from_secs(8));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            result = tasks.next() => {
                let Some((id, checks)) = result else { break; };
                expected.retain(|check| check.id != id);
                data.extend(checks);
            }
            _ = &mut deadline => {
                data.extend(expected);
                break;
            }
        }
    }
    DoctorResponse {
        data,
        server_version: crate::VERSION.into(),
        server_protocol_version: PROTOCOL_VERSION,
    }
}

fn offline_version(
    registration: Option<&crate::Registration>,
    client_version: &str,
) -> DoctorCheck {
    match registration {
        Some(registration)
            if registration.version == client_version
                && registration.protocol_version == PROTOCOL_VERSION =>
        {
            ok("version", "Client/server versions", DoctorCategory::Version)
        }
        Some(registration) => fail(
            "version",
            "Client/server versions",
            DoctorCategory::Version,
            format!(
                "Registered server is {} (protocol {}); client is {} (protocol {}).",
                registration.version,
                registration.protocol_version,
                client_version,
                PROTOCOL_VERSION
            ),
            "Install matching versions and run canter serve --replace.".into(),
        ),
        None => skip(
            "version",
            "Client/server versions",
            DoctorCategory::Version,
            "server.json is unavailable or invalid.".into(),
        ),
    }
}

fn sensitive_values(config: &serde_json::Value, values: &mut Vec<String>) {
    match config {
        serde_json::Value::Object(object) => {
            for (name, value) in object {
                let sensitive = ["key", "token", "secret", "authorization"]
                    .iter()
                    .any(|part| name.to_ascii_lowercase().contains(part))
                    || matches!(name.as_str(), "headers" | "env");
                if sensitive {
                    if let Some(value) = value.as_str()
                        && value.len() >= 4
                    {
                        values.push(value.to_owned());
                    }
                    if let serde_json::Value::Object(values_object) = value {
                        for value in values_object.values().filter_map(serde_json::Value::as_str) {
                            if value.len() >= 4 {
                                values.push(value.to_owned());
                            }
                        }
                    }
                }
                sensitive_values(value, values);
            }
        }
        serde_json::Value::Array(values_array) => {
            for value in values_array {
                sensitive_values(value, values);
            }
        }
        _ => {}
    }
}

fn log_errors(home: &Path) -> DoctorCheck {
    let path = home.join("server.log");
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => {
            return skip(
                "log",
                "server.log",
                DoctorCategory::Configuration,
                "Could not read server.log.".into(),
            );
        }
    };
    let mut secrets = Vec::new();
    if let Ok(config) = std::fs::read(home.join("config.json"))
        && let Ok(config) = serde_json::from_slice::<serde_json::Value>(&config)
    {
        sensitive_values(&config, &mut secrets);
    }
    let errors = text
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            ["error", "failed", "fatal", "panic"]
                .iter()
                .any(|marker| lower.contains(marker))
        })
        .filter(|line| !secrets.iter().any(|secret| line.contains(secret)))
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let errors = errors.into_iter().rev().take(3).collect::<Vec<_>>();
    if errors.is_empty() {
        ok("log", "server.log", DoctorCategory::Configuration)
    } else {
        skip(
            "log",
            "server.log recent errors",
            DoctorCategory::Configuration,
            errors.into_iter().rev().collect::<Vec<_>>().join(" | "),
        )
    }
}

pub async fn offline(
    home: PathBuf,
    client_version: String,
    connection_reason: Option<String>,
) -> DoctorResponse {
    let registration = crate::lifecycle::read_registration(&home).ok().flatten();
    let loaded = crate::config::Config::load_read_only(&home).ok();
    let mut data = vec![
        fail(
            "server",
            "Server connection",
            DoctorCategory::Version,
            connection_reason.unwrap_or_else(|| {
                "Could not connect to a running server; showing local checks only.".into()
            }),
            "Start canter normally, or run canter serve --replace to upgrade it.".into(),
        ),
        config(&home),
        skip(
            "config.active",
            "Active configuration",
            DoctorCategory::Configuration,
            "The running server's active configuration is unavailable.".into(),
        ),
        rules(&home),
        optional_json(&home, "tui.json", "tui"),
    ];
    if let Some(config) = &loaded {
        let mut options = config.options();
        options.agent_roles_directory = Some(home.join("agents"));
        match canter_core::roles::inspect(&options, &[]).await {
            Ok(roles) => {
                let errors = roles
                    .into_iter()
                    .filter_map(|role| role.error.map(|error| format!("{}: {error}", role.name)))
                    .collect::<Vec<_>>();
                data.push(if errors.is_empty() {
                    ok("agents", "agents/*.md", DoctorCategory::Configuration)
                } else {
                    fail(
                        "agents",
                        "agents/*.md",
                        DoctorCategory::Configuration,
                        errors.join("; "),
                        "Fix or remove the listed files in CANTER_HOME/agents.".into(),
                    )
                });
            }
            Err(_) => data.push(skip(
                "agents",
                "agents/*.md",
                DoctorCategory::Configuration,
                "Could not validate role files.".into(),
            )),
        }
    } else {
        data.push(skip(
            "agents",
            "agents/*.md",
            DoctorCategory::Configuration,
            "Role validation needs a readable config.json.".into(),
        ));
    }
    data.push(state(&home));
    data.push(offline_version(registration.as_ref(), &client_version));
    data.push(log_errors(&home));
    data.push(skip(
        "auth.chatgpt",
        "ChatGPT sign-in",
        DoctorCategory::Authentication,
        "Sign-in state requires a running server.".into(),
    ));
    for provider in loaded
        .as_ref()
        .map(|config| config.doctor_providers())
        .unwrap_or_default()
    {
        let name = match provider {
            crate::config::DoctorProvider::ChatCompletions { name, .. }
            | crate::config::DoctorProvider::Chatgpt { name } => name,
        };
        data.push(skip(
            &format!("provider.{name}"),
            &format!("Provider {name}"),
            DoctorCategory::Provider,
            "Model list checks require a running server.".into(),
        ));
    }
    if let Some(config) = &loaded {
        for name in config.mcp_servers.keys() {
            data.push(skip(
                &format!("mcp.{name}"),
                &format!("MCP {name}"),
                DoctorCategory::Mcp,
                "MCP connection state requires a running server.".into(),
            ));
        }
    }
    data.push(skip(
        "sandbox",
        "Sandbox",
        DoctorCategory::Sandbox,
        "Sandbox state requires a running server.".into(),
    ));
    DoctorResponse {
        data,
        server_version: registration
            .as_ref()
            .map(|registration| registration.version.clone())
            .unwrap_or_else(|| "unknown".into()),
        server_protocol_version: registration
            .as_ref()
            .map(|registration| registration.protocol_version)
            .unwrap_or(0),
    }
}
