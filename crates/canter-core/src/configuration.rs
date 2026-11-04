use crate::{
    CoreError, CoreOptions, RuntimeContext, Session,
    session::{Shared, State},
};
use canter_model::Model;
use canter_tools::{LocalExecutor, Tools};
use futures_util::future::BoxFuture;
use std::sync::Arc;

/// A server configuration snapshot with the services currently ready to use.
/// A complete snapshot's lease keeps services alive throughout a turn's work.
pub struct Configuration {
    /// None means some services are still starting; a turn must acquire them before work.
    pub revision: Option<u64>,
    pub options: CoreOptions,
    pub model: Arc<dyn Model>,
    pub mcp_tools: Vec<canter_tools::mcp::McpTool>,
    pub lsp: Option<Arc<canter_tools::lsp::LspService>>,
    pub lease: Arc<dyn Send + Sync>,
}

pub trait ConfigurationSource: Send + Sync {
    /// Latest options and currently ready services, without waiting for a handover.
    fn snapshot(&self) -> Configuration;
    /// Wait for a complete snapshot, keeping its services leased to the caller.
    fn acquire(&self) -> BoxFuture<'_, Result<Configuration, CoreError>>;
    fn options(&self) -> CoreOptions;
}

#[derive(Clone)]
pub(crate) struct Runtime {
    pub revision: Option<u64>,
    pub options: CoreOptions,
    pub limits: crate::ModelLimits,
    pub model: Arc<dyn Model>,
    pub tools: Tools,
    pub context: RuntimeContext,
    // Background commands may still use a previous executor; all share session scratch space.
    pub retired_tools: Vec<Tools>,
}

impl Session {
    /// A new terminal opening replaces the previous opening's sandbox choice.
    /// The environment and saved approval policy stay with the session.
    pub async fn set_sandbox_disabled(
        &self,
        disabled: bool,
        configured: bool,
    ) -> Result<(), CoreError> {
        let mut state = self.shared.state.lock().await;
        let mut runtime = (*self.shared.runtime()).clone();
        runtime.context.sandbox_disabled = disabled;
        runtime.options.sandbox = configured;
        let enabled = runtime.options.sandbox && !disabled;
        if runtime.tools.sandbox_enabled() != enabled {
            let tools = sandbox_tools(&state, &runtime.context, &runtime.tools, enabled).await?;
            tools.configure_shell(
                state.approval_policy(),
                &state.journal.snapshot.settings.additional_directories,
            );
            state.refresh_tools(&tools).await?;
            if !runtime
                .tools
                .background()
                .list()
                .iter()
                .any(|command| command.running)
            {
                runtime.retired_tools.clear();
            }
            runtime.retired_tools.push(runtime.tools);
            runtime.tools = tools;
            let mut settings = state.journal.snapshot.settings.clone();
            settings.sandbox_enabled = Some(enabled);
            state
                .journal
                .append(vec![canter_protocol::EventPayload::SessionUpdated {
                    settings,
                }])
                .await?;
        }
        *self.shared.runtime.write().unwrap() = Arc::new(runtime);
        Ok(())
    }

    /// Attach immediately after creating or opening the session, using the
    /// revision that supplied its initial options, model and tools.
    pub fn set_configuration_source(
        &self,
        source: Arc<dyn ConfigurationSource>,
        revision: Option<u64>,
    ) {
        if self.shared.configuration.set(source).is_ok() {
            Arc::make_mut(&mut self.shared.runtime.write().unwrap()).revision = revision;
        }
    }

    pub async fn configuration_warning(&self, message: String) -> Result<(), CoreError> {
        let sender = self.shared.configuration_warnings.get_or_init(|| {
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
            let state = self.shared.state.clone();
            let stop = self.shared.lifetime.clone();
            tokio::spawn(async move {
                loop {
                    let message = tokio::select! { _ = stop.cancelled() => break, message = receiver.recv() => match message { Some(message) => message, None => break } };
                    let mut state = tokio::select! { _ = stop.cancelled() => break, state = state.lock() => state };
                    if state.closing || state.journal.append(vec![canter_protocol::EventPayload::Warning { message }]).await.is_err() { break; }
                }
            });
            sender
        });
        sender.send(message).map_err(|_| CoreError::internal())
    }
}

impl Shared {
    pub(crate) fn frozen(&self) -> Self {
        Self {
            agent_host: self.agent_host.clone(),
            agent_parent: self.agent_parent.clone(),
            clients: self.clients.clone(),
            input_waiters: std::sync::atomic::AtomicUsize::new(0),
            state: self.state.clone(),
            runtime: std::sync::RwLock::new(self.runtime()),
            configuration: Default::default(),
            configuration_warnings: Default::default(),
            lifetime: self.lifetime.clone(),
            hook_tasks: self.hook_tasks.clone(),
        }
    }

    pub(crate) fn next_options(&self) -> CoreOptions {
        self.configuration
            .get()
            .map_or_else(|| self.runtime().options.clone(), |source| source.options())
    }
    pub(crate) fn runtime(&self) -> Arc<Runtime> {
        self.runtime.read().unwrap().clone()
    }

    pub(crate) async fn configure(
        &self,
        state: &mut State,
    ) -> Result<Option<Arc<dyn Send + Sync>>, CoreError> {
        let Some(source) = self.configuration.get() else {
            return Ok(None);
        };
        let config = source.snapshot();
        let ready = config.revision.is_some();
        let lease = self.apply_configuration(state, config).await?;
        Ok(ready.then_some(lease))
    }

    pub(crate) async fn prepare_turn(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), CoreError> {
        let Some(source) = self.configuration.get() else {
            return Ok(());
        };
        if self
            .state
            .lock()
            .await
            .active
            .as_ref()
            .expect("active turn")
            .configuration_lease
            .is_some()
        {
            return Ok(());
        }
        // The active turn is already visible and cancellable. Never keep its
        // state lock while another turn owns the service we are waiting for.
        let config = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            config = source.acquire() => config?,
        };
        let mut state = self.state.lock().await;
        if cancel.is_cancelled() {
            return Ok(());
        }
        let lease = self.apply_configuration(&mut state, config).await?;
        state
            .active
            .as_mut()
            .expect("active turn")
            .configuration_lease = Some(lease);
        Ok(())
    }

    async fn apply_configuration(
        &self,
        state: &mut State,
        mut config: Configuration,
    ) -> Result<Arc<dyn Send + Sync>, CoreError> {
        let old = self.runtime();
        if state.journal.snapshot.parent.is_some() {
            config.options.sandbox = old.options.sandbox;
        }
        if config.revision.is_some() && old.revision == config.revision {
            return Ok(config.lease);
        }
        let mut context = old.context.clone();
        context.mcp_tools = config.mcp_tools;
        context.lsp = config.lsp;
        let mut tools = old
            .tools
            .clone()
            .with_telemetry(config.options.telemetry)
            .with_plan_enabled(config.options.plan_enabled)
            .with_mcp(context.mcp_tools.clone())
            .with_mcp_tool_search(config.options.mcp_tool_search)
            .with_web_search(config.options.web_search.clone())
            .with_web_fetch_timeout(config.options.web_fetch_timeout);
        let mut retired_tools = old.retired_tools.clone();
        if !old
            .tools
            .background()
            .list()
            .iter()
            .any(|command| command.running)
        {
            retired_tools.clear();
        }
        let sandbox = config.options.sandbox && !context.sandbox_disabled;
        if old.tools.sandbox_enabled() != sandbox {
            retired_tools.push(old.tools.clone());
            tools = sandbox_tools(state, &context, &tools, sandbox).await?;
        }
        let same_lsp = match (&old.context.lsp, &context.lsp) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        };
        if !same_lsp {
            if let Some(lsp) = old.tools.lsp() {
                lsp.close().await;
            }
            tools = tools.without_lsp();
            state.lsp_delivery = Default::default();
            let stopped = state
                .journal
                .snapshot
                .language_servers
                .iter()
                .cloned()
                .map(|mut server| {
                    server.state = canter_protocol::LanguageServerState::Stopped;
                    server.reason = Some("LSP configuration changed".into());
                    server.shared_references = 0;
                    server.rss_bytes = None;
                    server.updated_at = crate::journal::now();
                    canter_protocol::EventPayload::LanguageServerUpdated { server }
                })
                .collect();
            state.journal.append(stopped).await?;
            if let Some(service) = context.lsp.as_ref().filter(|_| !context.restricted_mode) {
                tools = tools.with_lsp(
                    service.session_with_access(
                        canter_tools::lsp::FileContext {
                            cwd: state.journal.snapshot.settings.cwd.clone().into(),
                            additional_directories: state
                                .journal
                                .snapshot
                                .settings
                                .additional_directories
                                .iter()
                                .map(Into::into)
                                .collect(),
                            environment: context.environment.clone(),
                            round: canter_tools::lsp::Round::Modify,
                        },
                        context.protected_paths.clone(),
                        context.lsp_file_access.clone(),
                    ),
                );
            }
        }
        tools.configure_shell(
            state.approval_policy(),
            &state.journal.snapshot.settings.additional_directories,
        );
        state.refresh_tools(&tools).await?;
        let mut settings = state.journal.snapshot.settings.clone();
        settings.plan_enabled = Some(config.options.plan_enabled);
        settings.sandbox_enabled = Some(sandbox);
        if settings != state.journal.snapshot.settings {
            state
                .journal
                .append(vec![canter_protocol::EventPayload::SessionUpdated {
                    settings,
                }])
                .await?;
        }
        // Keep the queue allocated for private answers already in flight.
        if config.options.telemetry && state.journal.telemetry.is_none() {
            state.journal.telemetry = Some(Default::default());
        }
        state.journal.telemetry_enabled = config.options.telemetry;
        *self.runtime.write().unwrap() = Arc::new(Runtime {
            revision: config.revision,
            model: config.model,
            limits: config
                .options
                .limits(&state.journal.snapshot.settings.model),
            options: config.options,
            tools,
            context,
            retired_tools,
        });
        Ok(config.lease)
    }
}

async fn sandbox_tools(
    state: &State,
    context: &RuntimeContext,
    tools: &Tools,
    enabled: bool,
) -> Result<Tools, CoreError> {
    let cwd = state.journal.snapshot.settings.cwd.clone();
    let environment = context.environment.clone();
    let protected = context.protected_paths.clone();
    let scratch = tools.scratch_directory().expect("local session executor");
    let memory = tools.memory();
    let executor = tokio::task::spawn_blocking(move || {
        LocalExecutor::with_scratch_directory(cwd.into(), environment, protected, scratch)
            .map(|executor| executor.with_memory(memory).with_sandbox(enabled))
    })
    .await
    .map_err(|_| CoreError::internal())?
    .map_err(|_| CoreError::internal())?;
    Ok(tools.clone().with_executor(Arc::new(executor)))
}
