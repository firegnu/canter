use crate::{Action, PreparedCall, Risk, Tool};
use canter_protocol::{ApprovalDecision, ApprovalRequest, PermissionMode};
use std::collections::HashSet;

#[derive(Debug, PartialEq)]
pub enum PermissionCheck<'a> {
    Execute(ExecutionPermit<'a>),
    Ask(ApprovalRequest),
    Forbidden {
        reason: String,
    },
    Deny {
        cancel_turn: bool,
        feedback: Option<String>,
    },
}

/// An unforgeable, single-use permission for this exact prepared call.
pub struct ExecutionPermit<'a> {
    call: &'a PreparedCall,
}

impl<'a> ExecutionPermit<'a> {
    pub(crate) fn into_call(self) -> &'a PreparedCall {
        self.call
    }
}
impl std::fmt::Debug for ExecutionPermit<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionPermit").finish_non_exhaustive()
    }
}
impl PartialEq for ExecutionPermit<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.call, other.call)
    }
}

/// Own one gate per session. Grants are in-memory only and must not be restored from disk.
#[derive(Default)]
pub struct PermissionGate {
    grants: HashSet<String>,
    shell_grants: Vec<Vec<String>>,
    rules_file: Option<std::path::PathBuf>,
    rules_warning: std::sync::Mutex<(bool, Option<String>)>,
    additional_directories: Vec<std::path::PathBuf>,
}

impl PermissionGate {
    /// Call only after validating the answer against the pending proposal.
    pub fn save_rules(&self, prefixes: &[Vec<String>]) -> std::io::Result<std::path::PathBuf> {
        let path = self
            .rules_file
            .as_ref()
            .ok_or_else(|| std::io::Error::other("command rules are not configured"))?;
        crate::rules::append(path, prefixes)?;
        Ok(path.clone())
    }
    pub fn take_rules_warning(&self) -> Option<String> {
        self.rules_warning.lock().unwrap().1.take()
    }

    fn read_rules(&self) -> Result<crate::rules::Rules, ()> {
        let result = self
            .rules_file
            .as_deref()
            .map(crate::rules::Rules::read)
            .transpose();
        let mut warning = self.rules_warning.lock().unwrap();
        match result {
            Ok(rules) => {
                warning.0 = false;
                Ok(rules.unwrap_or_default())
            }
            Err(_) => {
                if !warning.0 {
                    warning.1 = Some(format!(
                        "Could not read command rules at {}; model shell commands require one-time approval until the file is fixed.",
                        self.rules_file.as_ref().unwrap().display()
                    ));
                    warning.0 = true;
                }
                Err(())
            }
        }
    }

    fn ask<'a>(&self, call: &'a PreparedCall, session: bool) -> PermissionCheck<'a> {
        let mut decisions = vec![ApprovalDecision::Accept];
        if session {
            decisions.push(ApprovalDecision::AcceptForSession);
        }
        decisions.extend([
            ApprovalDecision::Decline,
            ApprovalDecision::DeclineWithFeedback,
            ApprovalDecision::Cancel,
        ]);
        PermissionCheck::Ask(ApprovalRequest {
            proposed_execpolicy_amendment: None,
            session_prefixes: None,
            item_id: call.id.clone(),
            tool: match &call.action {
                Action::Mcp(tool) => tool.name.clone(),
                _ => call.tool.name().into(),
            },
            arguments: call.arguments.clone(),
            available_decisions: decisions,
        })
    }
    /// The server's global rules file; never inferred from a model's arguments.
    pub fn with_rules_file(mut self, path: std::path::PathBuf) -> Self {
        self.rules_file = Some(path);
        self
    }
    /// Canonical roots from the session settings; changing roots keeps existing grants.
    pub fn set_additional_directories(&mut self, directories: &[String]) {
        self.additional_directories = directories.iter().map(Into::into).collect();
    }

    fn outside_directories(&self, call: &PreparedCall) -> bool {
        if !matches!(call.tool, Tool::Write | Tool::Edit) {
            return false;
        }
        let Some(path) = &call.resolved_path else {
            return true;
        };
        !call
            .executor
            .cwd()
            .canonicalize()
            .is_ok_and(|cwd| path.starts_with(cwd))
            && !self
                .additional_directories
                .iter()
                .any(|root| path.starts_with(root))
    }
    /// Planning restrictions precede grants and ordinary permission decisions.
    pub fn evaluate_plan<'a>(
        &self,
        call: &'a PreparedCall,
        mode: PermissionMode,
    ) -> PermissionCheck<'a> {
        match call.tool {
            Tool::Read
            | Tool::AskUser
            | Tool::SubmitPlan
            | Tool::ShellOutput
            | Tool::ShellStop
            | Tool::Monitor => PermissionCheck::Execute(ExecutionPermit { call }),
            Tool::WebFetch => self.evaluate(call, mode),
            Tool::Shell if matches!(&call.action, Action::Shell(args) if call.executor.is_readonly_shell(&args.command)) => {
                PermissionCheck::Execute(ExecutionPermit { call })
            }
            _ => PermissionCheck::Deny {
                cancel_turn: false,
                feedback: None,
            },
        }
    }
    /// Review restrictions precede both permission mode and session grants.
    pub fn evaluate_review<'a>(&self, call: &'a PreparedCall) -> PermissionCheck<'a> {
        match call.tool {
            Tool::Read | Tool::UpdatePlan | Tool::ShellOutput | Tool::ShellStop | Tool::Monitor => {
                PermissionCheck::Execute(ExecutionPermit { call })
            }
            Tool::Shell if matches!(&call.action, Action::Shell(args) if call.executor.is_readonly_shell(&args.command)) => {
                PermissionCheck::Execute(ExecutionPermit { call })
            }
            Tool::Write
            | Tool::Edit
            | Tool::Shell
            | Tool::AskUser
            | Tool::Mcp
            | Tool::WebFetch
            | Tool::SubmitPlan => PermissionCheck::Deny {
                cancel_turn: false,
                feedback: None,
            },
        }
    }
    /// The caller must first correlate the answer with this session's pending request
    /// and validate its available decisions and feedback.
    pub fn resolve<'a>(
        &mut self,
        call: &'a PreparedCall,
        decision: ApprovalDecision,
        feedback: Option<String>,
    ) -> Result<PermissionCheck<'a>, &'static str> {
        self.resolve_with_prefixes(call, decision, feedback, None)
    }

    /// Session prefixes are those shown in the pending request, not newly inferred scopes.
    pub fn resolve_with_prefixes<'a>(
        &mut self,
        call: &'a PreparedCall,
        decision: ApprovalDecision,
        feedback: Option<String>,
        approved_prefixes: Option<&[Vec<String>]>,
    ) -> Result<PermissionCheck<'a>, &'static str> {
        if call.tool == Tool::Shell
            && matches!(
                decision,
                ApprovalDecision::Accept | ApprovalDecision::AcceptWithExecpolicyAmendment
            )
            && let forbidden @ PermissionCheck::Forbidden { .. } =
                self.evaluate(call, PermissionMode::AlwaysAsk)
        {
            return Ok(forbidden);
        }
        match decision {
            ApprovalDecision::Accept | ApprovalDecision::AcceptWithExecpolicyAmendment => {
                Ok(PermissionCheck::Execute(ExecutionPermit { call }))
            }
            ApprovalDecision::AcceptForSession => {
                if call.tool == Tool::Shell {
                    let request = match self.evaluate(call, PermissionMode::AlwaysAsk) {
                        PermissionCheck::Ask(request) => request,
                        PermissionCheck::Execute(permit) => {
                            if let Some(prefixes) = approved_prefixes {
                                self.shell_grants.extend_from_slice(prefixes);
                            }
                            return Ok(PermissionCheck::Execute(permit));
                        }
                        forbidden @ PermissionCheck::Forbidden { .. } => return Ok(forbidden),
                        _ => {
                            return Err(
                                "invalid_decision: command no longer permits a session grant",
                            );
                        }
                    };
                    let proposed = request
                        .session_prefixes
                        .ok_or("invalid_decision: command prefix is not known")?;
                    let prefixes = approved_prefixes.unwrap_or(&proposed);
                    if prefixes.iter().any(|prefix| !proposed.contains(prefix)) {
                        return Err("invalid_decision: command rule proposal changed");
                    }
                    self.shell_grants.extend_from_slice(prefixes);
                } else {
                    self.grants.insert(call.permission_key());
                }
                Ok(PermissionCheck::Execute(ExecutionPermit { call }))
            }
            ApprovalDecision::Decline => Ok(PermissionCheck::Deny {
                cancel_turn: false,
                feedback: None,
            }),
            ApprovalDecision::DeclineWithFeedback => Ok(PermissionCheck::Deny {
                cancel_turn: false,
                feedback,
            }),
            ApprovalDecision::Cancel => Ok(PermissionCheck::Deny {
                cancel_turn: true,
                feedback: None,
            }),
            ApprovalDecision::Unknown => Err("invalid_decision: unknown approval answer"),
        }
    }

    pub fn evaluate<'a>(
        &self,
        call: &'a PreparedCall,
        mode: PermissionMode,
    ) -> PermissionCheck<'a> {
        if let Action::Shell(args) = &call.action {
            let Ok(rules) = self.read_rules() else {
                return self.ask(call, false);
            };
            let parsed = crate::readonly::scan_commands(&args.command, None, true);
            let commands = match &parsed {
                Ok(commands) | Err(commands) => commands,
            };
            if commands.iter().any(|command| {
                rules.decision(&command.words) == Some(crate::rules::Decision::Forbidden)
            }) {
                return PermissionCheck::Forbidden {
                    reason: "Command forbidden by rules.json".into(),
                };
            }
            if mode == PermissionMode::AllowAll {
                return PermissionCheck::Execute(ExecutionPermit { call });
            }
            if commands.iter().any(|command| {
                rules.decision(&command.words) == Some(crate::rules::Decision::Prompt)
            }) {
                return self.ask(call, false);
            }
            let allowed = |words: &[String]| {
                rules.decision(words) == Some(crate::rules::Decision::Allow)
                    || self
                        .shell_grants
                        .iter()
                        .any(|prefix| words.starts_with(prefix))
            };
            if call.executor.is_readonly_shell(&args.command)
                || parsed.is_ok() && call.executor.is_allowed_shell(&args.command, &allowed)
            {
                return PermissionCheck::Execute(ExecutionPermit { call });
            }
            let mut request = match self.ask(call, false) {
                PermissionCheck::Ask(request) => request,
                _ => unreachable!(),
            };
            if parsed.is_ok() {
                let needed = std::cell::RefCell::new(Vec::new());
                let suggestible = call.executor.is_allowed_shell(&args.command, &|words| {
                    if !allowed(words) {
                        let Some(prefix) = crate::rules::suggested_prefix(words) else {
                            return false;
                        };
                        let mut needed = needed.borrow_mut();
                        if !needed.contains(&prefix) {
                            needed.push(prefix);
                        }
                    }
                    true
                });
                let prefixes = needed.into_inner();
                if suggestible && !prefixes.is_empty() {
                    request
                        .available_decisions
                        .insert(1, ApprovalDecision::AcceptForSession);
                    request.session_prefixes = Some(prefixes.clone());
                    if self.rules_file.is_some()
                        && prefixes
                            .iter()
                            .all(|prefix| crate::rules::can_persist_prefix(prefix))
                    {
                        request
                            .available_decisions
                            .insert(2, ApprovalDecision::AcceptWithExecpolicyAmendment);
                        request.proposed_execpolicy_amendment = Some(prefixes);
                    }
                }
            }
            return PermissionCheck::Ask(request);
        }
        if call.tool == Tool::UpdatePlan || self.grants.contains(&call.permission_key()) {
            return PermissionCheck::Execute(ExecutionPermit { call });
        }
        let ask = match mode {
            PermissionMode::AlwaysAsk | PermissionMode::Unknown => call.tool.risk() != Risk::Read,
            PermissionMode::AskForExecution => {
                call.tool.risk() == Risk::Execute || self.outside_directories(call)
            }
            PermissionMode::AllowAll => false,
        };
        if ask {
            self.ask(call, true)
        } else {
            PermissionCheck::Execute(ExecutionPermit { call })
        }
    }
}
