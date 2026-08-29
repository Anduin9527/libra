//! Code UI bridge for the UI-neutral [`AgentRuntimeHandle`].
//!
//! The adapter deliberately owns no terminal-UI state. Its session is the event
//! projection cache used by the HTTP/SSE surface; commands are admitted,
//! responded to, and cancelled by the serialized runtime worker.
//!
//! For default Web non-Codex launches, optional [`WebCodeUiAdmission`] supplies
//! persist-before-gate transcript semantics and plan-vs-explicit routing while
//! this adapter remains the mounted [`CodeUiCommandAdapter`] write-path owner.

use std::sync::{Arc, Weak};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{
    code_ui::{
        CodeUiApiError, CodeUiCapabilities, CodeUiCommandAdapter, CodeUiInteractionResponse,
        CodeUiReadModel, CodeUiSession,
    },
    web_admission::WebCodeUiAdmission,
};
use crate::internal::ai::{
    agent::runtime::{RuntimeUsageService, RuntimeUsageTotals},
    observed_agents::{IndexedSkillEvent, SkillEventProjection},
    permission::revoke_session_approval_memos,
    runtime::{
        AgentEventKind, AgentRuntimeHandle, CodeSkillActivation, CodeSkillSearch, EventCursor,
        ExecutionControlService, InteractionResponse, InteractionState, RuntimeCommandDurability,
        RuntimeWorkerError, TurnRequest, runtime_worker_adapter_message,
    },
    sandbox::ApprovalStore,
    usage::UsageQueryFilter,
};

#[derive(Clone, Debug)]
struct ActiveTurnSlot {
    turn_id: String,
    text: String,
}

/// Process-level shutdown hook for web-owned adapter mounts.
pub trait CodeUiLifecycleShutdown: Send + Sync {
    fn shutdown(&self) -> BoxFuture<'_, Result<()>>;

    /// Durable workflow fan-out for SSE wire v2 when this host owns a session
    /// JSONL store (W3-06). Default is unavailable.
    fn workflow_hub(&self) -> Option<std::sync::Arc<super::sse_wire::CodeUiWorkflowHub>> {
        None
    }
}

/// Production Code UI command bridge backed by the serialized Agent runtime.
///
/// `runtime_session_id` is intentionally separate from the browser-visible
/// session id: it is the worker/durability namespace used for turn admission.
#[derive(Clone)]
pub struct AgentRuntimeCodeUiAdapter {
    session: Arc<CodeUiSession>,
    capabilities: CodeUiCapabilities,
    runtime: AgentRuntimeHandle,
    runtime_session_id: String,
    execution_control: Arc<ExecutionControlService>,
    usage: Option<RuntimeUsageService>,
    durability: Option<RuntimeCommandDurability>,
    active_turn: Arc<Mutex<Option<ActiveTurnSlot>>>,
    /// When set, browser submit/cancel/respond use persist-before-gate web
    /// admission (W3-03) instead of the lightweight managed-session path.
    web_admission: Option<Arc<WebCodeUiAdmission>>,
    /// Optional lifecycle shutdown for web-only mounts (worker join, fence).
    /// Held as [`Weak`] so the adapter does not form a retain cycle with the
    /// headless lifecycle host (`Headless` → adapter → host).
    lifecycle_shutdown: Arc<Mutex<Option<Weak<dyn CodeUiLifecycleShutdown>>>>,
    /// In-memory session/TTL approval cache to drop on lease takeover (W4-13).
    approval_store: Arc<Mutex<Option<Arc<Mutex<ApprovalStore>>>>>,
    /// DF-07: A0-07 skill activations pending consumption by the next plain
    /// turn (session-scoped, ids only — never skill contents/credentials).
    pending_skills: Arc<Mutex<PendingSkillContext>>,
}

/// DF-07: session-scoped pending skill activations plus the most recent
/// composed turn, kept so a durable `commandId` retry recomposes the exact
/// same payload instead of tripping payload-conflict guards.
#[derive(Default)]
struct PendingSkillContext {
    /// `(provider, name)` in activation order, deduplicated.
    active: Vec<(String, String)>,
    last_composed: Option<ComposedSkillTurn>,
}

struct ComposedSkillTurn {
    command_id: String,
    raw_text: String,
    input: String,
}

impl PendingSkillContext {
    /// Resolve the runtime input for a submitted message. Plain non-empty,
    /// non-slash messages consume the active set (appended after the user
    /// text, so slash routing and intent-revision parsing never see it);
    /// control/slash/empty messages pass through untouched and keep the
    /// activations pending. A repeated `command_id` + raw text reuses the
    /// previously composed input verbatim (idempotent retries and durable
    /// replay comparisons stay stable).
    fn resolve_input(&mut self, text: &str, command_id: Option<&str>) -> String {
        if let (Some(command_id), Some(last)) = (command_id, self.last_composed.as_ref())
            && last.command_id == command_id
            && last.raw_text == text
        {
            return last.input.clone();
        }
        let trimmed = text.trim();
        if self.active.is_empty() || trimmed.is_empty() || trimmed.starts_with('/') {
            return text.to_string();
        }
        let mut lines = vec![
            text.to_string(),
            String::new(),
            "[skill activation] The operator activated these provider skills for this \
             session; consume them on this turn when relevant. Tool permissions are \
             unchanged by activation:"
                .to_string(),
        ];
        for (provider, name) in &self.active {
            lines.push(format!("- '{name}' ({provider})"));
        }
        let input = lines.join("\n");
        self.active.clear();
        if let Some(command_id) = command_id {
            self.last_composed = Some(ComposedSkillTurn {
                command_id: command_id.to_string(),
                raw_text: text.to_string(),
                input: input.clone(),
            });
        }
        input
    }

    /// Record one validated activation; duplicates keep their original slot.
    /// Returns the pending count.
    fn record(&mut self, provider: &str, name: &str) -> usize {
        if !self.active.iter().any(|(p, n)| p == provider && n == name) {
            self.active.push((provider.to_string(), name.to_string()));
        }
        self.active.len()
    }
}

impl AgentRuntimeCodeUiAdapter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: Arc<CodeUiSession>,
        capabilities: CodeUiCapabilities,
        runtime: AgentRuntimeHandle,
        runtime_session_id: impl Into<String>,
        execution_control: Arc<ExecutionControlService>,
        usage: Option<RuntimeUsageService>,
        durability: Option<RuntimeCommandDurability>,
    ) -> Arc<Self> {
        Self::new_with_web_admission(
            session,
            capabilities,
            runtime,
            runtime_session_id,
            execution_control,
            usage,
            durability,
            None,
        )
    }

    /// Construct the production adapter with optional web admit semantics.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_web_admission(
        session: Arc<CodeUiSession>,
        capabilities: CodeUiCapabilities,
        runtime: AgentRuntimeHandle,
        runtime_session_id: impl Into<String>,
        execution_control: Arc<ExecutionControlService>,
        usage: Option<RuntimeUsageService>,
        durability: Option<RuntimeCommandDurability>,
        web_admission: Option<Arc<WebCodeUiAdmission>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            session,
            capabilities,
            runtime,
            runtime_session_id: runtime_session_id.into(),
            execution_control,
            usage,
            durability,
            active_turn: Arc::new(Mutex::new(None)),
            web_admission,
            lifecycle_shutdown: Arc::new(Mutex::new(None)),
            approval_store: Arc::new(Mutex::new(None)),
            pending_skills: Arc::new(Mutex::new(PendingSkillContext::default())),
        })
    }

    /// Bind the runtime ApprovalStore so a controller lease takeover can drop
    /// session/TTL memos (W4-13). Always rows stay in `approved_permission`.
    pub async fn set_approval_store(&self, store: Arc<Mutex<ApprovalStore>>) {
        *self.approval_store.lock().await = Some(store);
    }

    /// Attach process shutdown for a web-only mount after the lifecycle host
    /// `Arc` exists. Uses [`Weak`] so dropping the externally retained host
    /// (or [`super::code_ui::CodeUiRuntimeHandle`]) can tear down the worker
    /// without an adapter↔host retain cycle.
    pub async fn attach_lifecycle_shutdown(&self, shutdown: Arc<dyn CodeUiLifecycleShutdown>) {
        *self.lifecycle_shutdown.lock().await = Some(Arc::downgrade(&shutdown));
    }

    fn turn_id(&self, command_id: Option<String>) -> Result<String> {
        match command_id {
            Some(_command_id) if self.durability.is_none() && self.web_admission.is_none() => {
                Err(anyhow!(
                    "commandId requires durable AgentRuntime command storage; omit commandId or resume this Code session"
                ))
            }
            Some(command_id) if command_id.trim().is_empty() => Err(anyhow!(
                "commandId must be a non-empty string when provided"
            )),
            Some(command_id) => Ok(command_id),
            None => Ok(format!("code-ui-{}", Uuid::new_v4())),
        }
    }

    fn map_runtime_error(error: RuntimeWorkerError) -> anyhow::Error {
        anyhow!(
            "AgentRuntime rejected the Code UI command: {}",
            runtime_worker_adapter_message(error)
        )
    }

    fn spawn_release_watcher(
        &self,
        mut stream: crate::internal::ai::runtime::AgentEventStream,
        turn_id: String,
    ) {
        let active_turn = self.active_turn.clone();
        let session_id = self.runtime_session_id.clone();
        tokio::spawn(async move {
            loop {
                match stream.recv().await {
                    Ok(event)
                        if event.session_id == session_id
                            && event.turn_id.as_deref() == Some(turn_id.as_str()) =>
                    {
                        match event.kind {
                            AgentEventKind::TurnCompleted { .. }
                            | AgentEventKind::TurnCancelled
                            | AgentEventKind::TurnFailed { .. }
                            | AgentEventKind::TurnIndeterminateSideEffect { .. } => {
                                let mut slot = active_turn.lock().await;
                                if slot
                                    .as_ref()
                                    .is_some_and(|active| active.turn_id == turn_id)
                                {
                                    *slot = None;
                                }
                                return;
                            }
                            _ => {}
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
    }

    async fn rollback_active_turn(&self, turn_id: &str) {
        let mut slot = self.active_turn.lock().await;
        if slot
            .as_ref()
            .is_some_and(|active| active.turn_id == turn_id)
        {
            *slot = None;
        }
    }

    /// Query usage only when the runtime was constructed with a durable usage
    /// recorder. Returning an error (rather than zero totals) preserves the
    /// unknown/partial distinction on the HTTP surface.
    pub async fn usage_cumulative(&self, filter: UsageQueryFilter) -> Result<RuntimeUsageTotals> {
        let usage = self
            .usage
            .as_ref()
            .ok_or_else(|| anyhow!("usage is unavailable for this Code runtime"))?;
        usage.cumulative(filter).await.map_err(Into::into)
    }

    /// A0-07 search is read-only and remains projection-backed.
    pub fn skill_search<'a>(
        &self,
        projection: &'a SkillEventProjection,
        search: &CodeSkillSearch,
    ) -> Vec<&'a IndexedSkillEvent> {
        self.execution_control.skill_search(projection, search)
    }

    /// Validate an A0-07 activation before a provider consumes it.
    pub fn skill_activate(&self, activation: &CodeSkillActivation) -> Result<()> {
        self.execution_control.skill_activate(activation)
    }
}

#[async_trait]
impl CodeUiReadModel for AgentRuntimeCodeUiAdapter {
    fn session(&self) -> Arc<CodeUiSession> {
        self.session.clone()
    }
}

#[async_trait]
impl CodeUiCommandAdapter for AgentRuntimeCodeUiAdapter {
    fn capabilities(&self) -> CodeUiCapabilities {
        self.capabilities.clone()
    }

    /// DF-07: validate against the A0-07 curated registry (unknown provider
    /// or undiscoverable name fail closed there) and queue the activation
    /// for the next plain turn. No new registry, no widened tools.
    async fn activate_skill(
        &self,
        provider: &str,
        name: &str,
    ) -> Result<super::code_ui::CodeUiSkillActivationAck> {
        self.execution_control
            .skill_activate(&CodeSkillActivation {
                provider: provider.to_string(),
                name: name.to_string(),
            })?;
        let pending = self.pending_skills.lock().await.record(provider, name);
        Ok(super::code_ui::CodeUiSkillActivationAck {
            provider: provider.to_string(),
            name: name.to_string(),
            pending,
        })
    }

    async fn submit_message(&self, text: String) -> Result<()> {
        self.submit_message_with_command_id(text, None).await
    }

    async fn submit_message_with_command_id(
        &self,
        text: String,
        command_id: Option<String>,
    ) -> Result<()> {
        // DF-07: resolve pending skill activations into the turn input first
        // so both admission branches see one consistent payload identity.
        let text = self
            .pending_skills
            .lock()
            .await
            .resolve_input(&text, command_id.as_deref());
        if let Some(admission) = self.web_admission.as_ref() {
            return admission
                .submit_message_with_command_id(&self.runtime, &self.session, text, command_id)
                .await;
        }
        if text.trim().is_empty() {
            return Err(anyhow!("Empty messages are not accepted by libra code"));
        }
        let turn_id = self.turn_id(command_id)?;
        {
            let mut active_turn = self.active_turn.lock().await;
            if let Some(existing) = active_turn.as_ref() {
                if existing.turn_id == turn_id {
                    if existing.text == text {
                        // Idempotent retry with the same payload.
                        return Ok(());
                    }
                    return Err(RuntimeWorkerError::CommandPayloadConflict {
                        session_id: self.runtime_session_id.clone(),
                        turn_id,
                    }
                    .into());
                }
                return Err(anyhow!(
                    "A Code UI turn is already active; cancel it or wait for it to finish"
                ));
            }
            // Reserve before awaiting observe/submit so a concurrent caller
            // cannot also admit a second tool-capable turn into the worker.
            *active_turn = Some(ActiveTurnSlot {
                turn_id: turn_id.clone(),
                text: text.clone(),
            });
        }
        // Subscribe before admission so a fast terminal broadcast cannot be
        // missed between submit and the release watcher.
        let stream = match self
            .runtime
            .observe(EventCursor::new(self.runtime_session_id.clone(), 0))
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                self.rollback_active_turn(&turn_id).await;
                return Err(Self::map_runtime_error(error));
            }
        };
        if let Err(error) = self
            .runtime
            .submit(TurnRequest::new(
                self.runtime_session_id.clone(),
                turn_id.clone(),
                text,
                true,
            ))
            .await
        {
            self.rollback_active_turn(&turn_id).await;
            return Err(Self::map_runtime_error(error));
        }
        self.spawn_release_watcher(stream, turn_id);
        Ok(())
    }

    async fn respond_interaction(
        &self,
        interaction_id: &str,
        response: CodeUiInteractionResponse,
    ) -> Result<()> {
        if let Some(admission) = self.web_admission.as_ref() {
            return admission
                .respond_interaction(&self.runtime, &self.session, interaction_id, response)
                .await;
        }
        let turn_id = self
            .active_turn
            .lock()
            .await
            .as_ref()
            .map(|slot| slot.turn_id.clone())
            .ok_or_else(|| {
                anyhow!(CodeUiApiError::conflict(
                    "INTERACTION_NOT_ACTIVE",
                    format!(
                        "interaction '{interaction_id}' has no active AgentRuntime turn to receive a response"
                    )
                ))
            })?;
        let response = serde_json::to_string(&response)
            .map_err(|error| anyhow!("failed to encode interaction response: {error}"))?;
        self.runtime
            .respond(
                self.runtime_session_id.clone(),
                turn_id,
                InteractionResponse::new(interaction_id, response),
            )
            .await
            .map_err(Self::map_runtime_error)
    }

    async fn cancel_turn(&self) -> Result<()> {
        if let Some(admission) = self.web_admission.as_ref() {
            return admission
                .cancel_turn(&self.runtime, &self.session, |state| match state {
                    InteractionState::AwaitingIntentReview { interaction_id }
                    | InteractionState::AwaitingPlanReview { interaction_id }
                    | InteractionState::AwaitingPlanRepair { interaction_id }
                    | InteractionState::AwaitingNetworkPolicy { interaction_id }
                    | InteractionState::AwaitingUserInput { interaction_id }
                    | InteractionState::AwaitingToolApproval { interaction_id, .. } => {
                        Some(interaction_id.as_str())
                    }
                    InteractionState::Idle
                    | InteractionState::Queued
                    | InteractionState::Running
                    | InteractionState::Cancelling
                    | InteractionState::Completed
                    | InteractionState::Failed { .. }
                    | InteractionState::Cancelled
                    | InteractionState::IndeterminateSideEffect { .. } => None,
                })
                .await;
        }
        let turn_id = self
            .active_turn
            .lock()
            .await
            .as_ref()
            .map(|slot| slot.turn_id.clone());
        let Some(turn_id) = turn_id else {
            return Ok(());
        };
        self.runtime
            .cancel(self.runtime_session_id.clone(), turn_id)
            .await
            .map_err(Self::map_runtime_error)
        // Do not clear here: CancelRequested is not terminal. The observe task
        // releases the slot on TurnCancelled / IndeterminateSideEffect / Failed.
    }

    async fn task_dispatch(&self, agent: String, prompt: String) -> Result<String> {
        if let Some(admission) = self.web_admission.as_ref() {
            admission.ensure_not_shutting_down()?;
            admission
                .ensure_session_is_recoverable(&self.session)
                .await?;
        }
        self.execution_control.task_dispatch(agent, prompt).await
    }

    async fn goal_start(&self, objective: String) -> Result<String> {
        if let Some(admission) = self.web_admission.as_ref() {
            admission.ensure_not_shutting_down()?;
        }
        self.execution_control
            .goal_start(objective)
            .await
            .map_err(Into::into)
    }

    async fn goal_status(&self) -> Result<String> {
        self.execution_control
            .goal_status()
            .await
            .map_err(Into::into)
    }

    async fn goal_cancel(&self, reason: String) -> Result<String> {
        if let Some(admission) = self.web_admission.as_ref() {
            admission.ensure_not_shutting_down()?;
        }
        self.execution_control
            .goal_cancel(reason)
            .await
            .map_err(Into::into)
    }

    async fn shutdown(&self) -> Result<()> {
        let hook = self
            .lifecycle_shutdown
            .lock()
            .await
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(hook) = hook {
            return hook.shutdown().await;
        }
        Ok(())
    }

    async fn on_controller_lease_takeover(&self) -> Result<()> {
        if let Some(store) = self.approval_store.lock().await.clone() {
            revoke_session_approval_memos(&store).await;
        }
        self.runtime
            .drop_pending_after_lease_takeover(&self.runtime_session_id)
            .await
            .map_err(Self::map_runtime_error)?;
        if let Some(admission) = self.web_admission.as_ref() {
            admission
                .clear_pending_tool_interactions(&self.session)
                .await?;
        } else {
            self.session.clear_pending_tool_interactions().await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PendingSkillContext;

    /// DF-07: plain turns consume the active set (appended after the user
    /// text), slash/empty turns pass through and keep it pending, and
    /// duplicates hold one slot.
    #[test]
    fn pending_skill_context_consumes_only_plain_turns() {
        let mut ctx = PendingSkillContext::default();
        assert_eq!(ctx.record("claude-code", "/review"), 1);
        assert_eq!(ctx.record("claude-code", "/review"), 1, "dedup");
        assert_eq!(ctx.record("codex", "/plan"), 2);

        // Slash and empty submissions never consume.
        assert_eq!(ctx.resolve_input("/help", None), "/help");
        assert_eq!(ctx.resolve_input("   ", None), "   ");
        assert_eq!(ctx.active.len(), 2, "still pending after control turns");

        let composed = ctx.resolve_input("review my diff", None);
        assert!(composed.starts_with("review my diff"), "{composed}");
        assert!(
            composed.contains("[skill activation]")
                && composed.contains("- '/review' (claude-code)")
                && composed.contains("- '/plan' (codex)"),
            "activation context must ride the plain turn: {composed}"
        );
        assert!(ctx.active.is_empty(), "consumed on the plain turn");
        assert_eq!(
            ctx.resolve_input("next message", None),
            "next message",
            "a later turn must not re-inject"
        );
    }

    /// DF-07: a durable `commandId` retry with the same raw text must get
    /// the identical composed input even though the active set was already
    /// consumed — otherwise payload-conflict guards would reject the retry.
    #[test]
    fn pending_skill_context_recomposes_for_command_id_retries() {
        let mut ctx = PendingSkillContext::default();
        ctx.record("claude-code", "/review");
        let first = ctx.resolve_input("do it", Some("cmd-1"));
        assert!(first.contains("[skill activation]"));
        let retry = ctx.resolve_input("do it", Some("cmd-1"));
        assert_eq!(first, retry, "retry must reuse the composed payload");
        // A different command with no pending activations passes through.
        assert_eq!(ctx.resolve_input("other", Some("cmd-2")), "other");
    }
}
