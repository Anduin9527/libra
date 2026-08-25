use git_internal::internal::object::{
    ObjectTrait,
    intent_event::{IntentEvent, IntentEventKind},
    task_event::{TaskEvent, TaskEventKind},
    types::ActorKind as HistoryActorKind,
};
use sea_orm::DatabaseConnection;
use thiserror::Error;

use super::{
    admission::{EpisodeAdmission, EpisodeAdmissionErrorKind},
    compiler::{EpisodeCompileConfig, EpisodeCompiler},
    domain::{ActorKind, ActorRefV1, EpisodeRootKind},
    error::MemoryWriterErrorKind,
    job_sql::{claim_next_job, complete_job, record_job_failure},
    job_state::{
        CompileFailureClass, CompileJobCompletionOutcome, CompileJobMutationOutcome,
        StableJobFailure,
    },
    limits::EpisodeSourceLimits,
    observer::MemoryDependencyObserver,
    policy::{AuthenticatedMemoryContext, TrustedMemoryTarget},
    source::{EpisodeSourceErrorKind, EpisodeSourceResolver},
    writer::MemoryWriter,
};
use crate::internal::ai::{history::HistoryManager, keyed_digest::RepositoryKeyedDigest};

const TERMINAL_APPEND_ANCESTRY_LIMIT: usize = 2_048;
const TERMINAL_APPEND_TREE_BYTES: u64 = 4 * 1024 * 1024;
const TERMINAL_APPEND_BLOB_BYTES: u64 = 512 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GenerationRunOutcome {
    NoWork,
    Committed {
        appended: bool,
        new_generation_pending: bool,
    },
    RetryScheduled,
    StableFailure,
    FencedOut,
}

/// Drives exactly one claimed generation through the complete Memory write
/// path. Looping, concurrency, and runtime deadlines stay outside this deep
/// module so they cannot create a second authority or bypass lease fencing.
pub(crate) struct EpisodeGenerationRunner<'a> {
    history: &'a HistoryManager,
    database: &'a DatabaseConnection,
    digest: &'a RepositoryKeyedDigest,
    writer: &'a MemoryWriter,
    scope_key: &'a str,
    source_limits: EpisodeSourceLimits,
}

impl<'a> EpisodeGenerationRunner<'a> {
    pub(crate) fn new(
        history: &'a HistoryManager,
        database: &'a DatabaseConnection,
        digest: &'a RepositoryKeyedDigest,
        writer: &'a MemoryWriter,
        scope_key: &'a str,
        source_limits: EpisodeSourceLimits,
    ) -> Result<Self, EpisodeGenerationRunnerError> {
        source_limits
            .validate()
            .map_err(|_| EpisodeGenerationRunnerError::configuration())?;
        if scope_key != "repo" {
            return Err(EpisodeGenerationRunnerError::configuration());
        }
        Ok(Self {
            history,
            database,
            digest,
            writer,
            scope_key,
            source_limits,
        })
    }

    pub(crate) async fn run_one<C: EpisodeCompiler>(
        &self,
        compiler: &C,
        config: &EpisodeCompileConfig,
        owner: &str,
        now_ms: i64,
    ) -> Result<GenerationRunOutcome, EpisodeGenerationRunnerError> {
        let Some(lease) = claim_next_job(self.database, self.scope_key, owner, now_ms)
            .await
            .map_err(|_| EpisodeGenerationRunnerError::job())?
        else {
            return Ok(GenerationRunOutcome::NoWork);
        };

        let actor = match self.terminal_actor(&lease).await {
            Ok(actor) => actor,
            Err(failure) => return self.record_failure(&lease, failure, now_ms).await,
        };
        let context = AuthenticatedMemoryContext::new(self.digest.repository_id(), actor)
            .map_err(|_| EpisodeGenerationRunnerError::configuration())?;
        let target = TrustedMemoryTarget::episode(lease.key().root().clone());
        let resolver = EpisodeSourceResolver::new(self.history, self.digest, self.source_limits)
            .map_err(|_| EpisodeGenerationRunnerError::configuration())?;
        let source = match resolver
            .resolve(&context, &target, lease.terminal_source_oid())
            .await
        {
            Ok(source) => source,
            Err(error) => {
                let failure = source_failure(error.kind())?;
                return self.record_failure(&lease, failure, now_ms).await;
            }
        };
        let admitted = match EpisodeAdmission::new(self.digest)
            .compile(compiler, config, &context, &target, source)
            .await
        {
            Ok(admitted) => admitted,
            Err(error) => {
                let failure = admission_failure(error.kind())?;
                return self.record_failure(&lease, failure, now_ms).await;
            }
        };
        let committed = match self
            .writer
            .commit_admitted(&resolver, &context, &target, &admitted, None, Some(&lease))
            .await
        {
            Ok(committed) => committed,
            Err(error) => {
                let failure = writer_failure(error.kind())?;
                return self.record_failure(&lease, failure, now_ms).await;
            }
        };
        if let Ok(observer) =
            MemoryDependencyObserver::new(self.history, self.database, self.digest, self.scope_key)
            && observer.observe_task_revisions().await.is_err()
        {
            tracing::warn!(
                "Memory dependency observer failed after a committed generation; repair will retry"
            );
        }
        match complete_job(self.database, &lease, now_ms)
            .await
            .map_err(|_| EpisodeGenerationRunnerError::job())?
        {
            CompileJobCompletionOutcome::Clean => Ok(GenerationRunOutcome::Committed {
                appended: committed.appended(),
                new_generation_pending: false,
            }),
            CompileJobCompletionOutcome::NewGenerationPending => {
                Ok(GenerationRunOutcome::Committed {
                    appended: committed.appended(),
                    new_generation_pending: true,
                })
            }
            CompileJobCompletionOutcome::FencedOut => Ok(GenerationRunOutcome::FencedOut),
        }
    }

    async fn terminal_actor(
        &self,
        lease: &super::job_state::CompileJobLease,
    ) -> Result<ActorRefV1, StableJobFailure> {
        let append = self
            .history
            .read_append_at(
                lease.terminal_source_oid(),
                TERMINAL_APPEND_ANCESTRY_LIMIT,
                TERMINAL_APPEND_TREE_BYTES,
                TERMINAL_APPEND_BLOB_BYTES,
            )
            .await
            .map_err(|_| stable_failure("LBR-MEMORY-202", "terminal source is unavailable"))?;
        let history_actor = match lease.key().root().kind() {
            EpisodeRootKind::Task if append.object_type() == "task_event" => {
                let event =
                    TaskEvent::from_bytes(append.bytes(), append.object_oid()).map_err(|_| {
                        stable_failure("LBR-MEMORY-202", "terminal Task event is invalid")
                    })?;
                if event.task_id().to_string() != lease.key().root().id()
                    || !matches!(
                        event.kind(),
                        TaskEventKind::Done | TaskEventKind::Failed | TaskEventKind::Cancelled
                    )
                {
                    return Err(stable_failure(
                        "LBR-MEMORY-202",
                        "terminal Task source does not match the claimed root",
                    ));
                }
                event.header().created_by().clone()
            }
            EpisodeRootKind::Intent if append.object_type() == "intent_event" => {
                let event =
                    IntentEvent::from_bytes(append.bytes(), append.object_oid()).map_err(|_| {
                        stable_failure("LBR-MEMORY-202", "terminal Intent event is invalid")
                    })?;
                if event.intent_id().to_string() != lease.key().root().id()
                    || !matches!(
                        event.kind(),
                        IntentEventKind::Completed | IntentEventKind::Cancelled
                    )
                {
                    return Err(stable_failure(
                        "LBR-MEMORY-202",
                        "terminal Intent source does not match the claimed root",
                    ));
                }
                event.header().created_by().clone()
            }
            _ => {
                return Err(stable_failure(
                    "LBR-MEMORY-202",
                    "terminal source type does not match the claimed root",
                ));
            }
        };
        let kind = match history_actor.kind() {
            HistoryActorKind::Human => ActorKind::Human,
            HistoryActorKind::Agent | HistoryActorKind::McpClient => ActorKind::Agent,
            HistoryActorKind::System | HistoryActorKind::Other(_) => ActorKind::System,
        };
        Ok(ActorRefV1 {
            kind,
            principal_id: history_actor.id().to_string(),
        })
    }

    async fn record_failure(
        &self,
        lease: &super::job_state::CompileJobLease,
        failure: StableJobFailure,
        now_ms: i64,
    ) -> Result<GenerationRunOutcome, EpisodeGenerationRunnerError> {
        let class = failure.class();
        match record_job_failure(self.database, lease, &failure, now_ms)
            .await
            .map_err(|_| EpisodeGenerationRunnerError::job())?
        {
            CompileJobMutationOutcome::FencedOut => Ok(GenerationRunOutcome::FencedOut),
            CompileJobMutationOutcome::Applied => Ok(match class {
                CompileFailureClass::Transient => GenerationRunOutcome::RetryScheduled,
                CompileFailureClass::Stable => GenerationRunOutcome::StableFailure,
            }),
        }
    }
}

fn source_failure(
    kind: EpisodeSourceErrorKind,
) -> Result<StableJobFailure, EpisodeGenerationRunnerError> {
    let (class, code, summary) = match kind {
        EpisodeSourceErrorKind::SourceNotReachable => (
            CompileFailureClass::Transient,
            "LBR-MEMORY-201",
            "terminal source is not currently reachable",
        ),
        EpisodeSourceErrorKind::Unauthorized
        | EpisodeSourceErrorKind::InvalidRequest
        | EpisodeSourceErrorKind::SourceCorrupt
        | EpisodeSourceErrorKind::LimitExceeded
        | EpisodeSourceErrorKind::RedactionFailed
        | EpisodeSourceErrorKind::DigestUnavailable => (
            CompileFailureClass::Stable,
            "LBR-MEMORY-202",
            "terminal source failed policy or validation",
        ),
    };
    failure(class, code, summary)
}

fn admission_failure(
    kind: EpisodeAdmissionErrorKind,
) -> Result<StableJobFailure, EpisodeGenerationRunnerError> {
    let (class, code, summary) = match kind {
        EpisodeAdmissionErrorKind::CompilerTransient => (
            CompileFailureClass::Transient,
            "LBR-MEMORY-203",
            "Episode compiler provider failed",
        ),
        EpisodeAdmissionErrorKind::CompilerStable
        | EpisodeAdmissionErrorKind::InvalidProposal
        | EpisodeAdmissionErrorKind::SourceMismatch
        | EpisodeAdmissionErrorKind::DigestUnavailable => (
            CompileFailureClass::Stable,
            "LBR-MEMORY-204",
            "Episode compiler output failed deterministic admission",
        ),
    };
    failure(class, code, summary)
}

fn writer_failure(
    kind: MemoryWriterErrorKind,
) -> Result<StableJobFailure, EpisodeGenerationRunnerError> {
    let transient = matches!(
        kind,
        MemoryWriterErrorKind::ProjectionStale
            | MemoryWriterErrorKind::StorageFailure
            | MemoryWriterErrorKind::ConflictExhausted
    );
    failure(
        if transient {
            CompileFailureClass::Transient
        } else {
            CompileFailureClass::Stable
        },
        if transient {
            "LBR-MEMORY-205"
        } else {
            "LBR-MEMORY-206"
        },
        if transient {
            "Memory writer encountered a transient local conflict"
        } else {
            "Memory writer rejected the admitted proposal"
        },
    )
}

fn stable_failure(code: &str, summary: &str) -> StableJobFailure {
    // INVARIANT: every call site uses a compile-time LBR-MEMORY-NNN code and
    // a short static diagnostic, both inside StableJobFailure's hard limits.
    StableJobFailure::new(CompileFailureClass::Stable, code, summary)
        .expect("runner stable failures use valid bounded diagnostics")
}

fn failure(
    class: CompileFailureClass,
    code: &str,
    summary: &str,
) -> Result<StableJobFailure, EpisodeGenerationRunnerError> {
    StableJobFailure::new(class, code, summary)
        .map_err(|_| EpisodeGenerationRunnerError::configuration())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EpisodeGenerationRunnerErrorKind {
    InvalidConfiguration,
    JobState,
}

#[derive(Debug, Error)]
#[error("Memory generation runner failed ({kind:?})")]
pub(crate) struct EpisodeGenerationRunnerError {
    kind: EpisodeGenerationRunnerErrorKind,
}

impl EpisodeGenerationRunnerError {
    const fn configuration() -> Self {
        Self {
            kind: EpisodeGenerationRunnerErrorKind::InvalidConfiguration,
        }
    }

    const fn job() -> Self {
        Self {
            kind: EpisodeGenerationRunnerErrorKind::JobState,
        }
    }

    pub(crate) const fn kind(&self) -> EpisodeGenerationRunnerErrorKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use git_internal::internal::object::{
        ObjectTrait,
        task::Task,
        task_event::{TaskEvent, TaskEventKind},
        types::ActorRef,
    };
    use sea_orm::{ConnectionTrait, Statement};

    use super::*;
    use crate::{
        internal::ai::{
            context_budget::MemoryAnchorConfidence,
            memory::{
                compiler::{
                    EpisodeClaimProposalV1, EpisodeCompilerError, EpisodeCompilerProposalV1,
                },
                domain::{EpisodeRoot, EpistemicStatus},
                observer::EpisodeObserver,
                policy::REPO_EPISODE_PRODUCER,
                source::RedactedEpisodeSource,
                writer::tests::fixture,
            },
        },
        utils::{object::write_git_object, storage::local::LocalStorage},
    };

    struct FakeCompiler;

    #[async_trait]
    impl EpisodeCompiler for FakeCompiler {
        async fn compile(
            &self,
            source: &RedactedEpisodeSource,
            _config: &EpisodeCompileConfig,
        ) -> Result<EpisodeCompilerProposalV1, EpisodeCompilerError> {
            let evidence_fragment_id = source
                .fragments()
                .first()
                .expect("resolved source has a root fragment")
                .fragment_id()
                .to_string();
            let observation = EpisodeClaimProposalV1 {
                epistemic_status: EpistemicStatus::Observation,
                claim: "the task reached a terminal state".to_string(),
                confidence: None,
                evidence_fragment_ids: vec![evidence_fragment_id.clone()],
            };
            let inference = EpisodeClaimProposalV1 {
                epistemic_status: EpistemicStatus::Inference,
                claim: "the terminal evidence is ready for reuse".to_string(),
                confidence: Some(MemoryAnchorConfidence::Medium),
                evidence_fragment_ids: vec![evidence_fragment_id],
            };
            Ok(EpisodeCompilerProposalV1 {
                summary: inference.clone(),
                observations: vec![observation],
                inferences: vec![inference],
                decisions: Vec::new(),
                failed_attempts: Vec::new(),
                unresolved: Vec::new(),
            })
        }
    }

    async fn append_object<T: ObjectTrait>(
        history: &HistoryManager,
        object_type: &str,
        object_id: &str,
        object: &T,
    ) {
        let bytes = object.to_data().expect("serialize AI history object");
        let oid = write_git_object(history.repository_path(), "blob", &bytes)
            .expect("write AI history blob");
        history
            .append(object_type, object_id, oid)
            .await
            .expect("append AI history object");
    }

    #[tokio::test]
    async fn generation_runner_resolves_compiles_and_commits_one_task() {
        let fixture = fixture().await;
        let storage = Arc::new(LocalStorage::new(fixture._temp.path().join("objects")));
        let history = HistoryManager::new(
            storage,
            fixture._temp.path().to_path_buf(),
            Arc::clone(&fixture.database),
        );
        let actor = ActorRef::agent("runner-test-agent").expect("test actor");
        let task =
            Task::new(actor.clone(), "compile a bounded episode", None).expect("construct task");
        let task_id = task.header().object_id();
        append_object(&history, "task", &task_id.to_string(), &task).await;
        let done = TaskEvent::new(actor, task_id, TaskEventKind::Done)
            .expect("construct terminal Task event");
        append_object(
            &history,
            "task_event",
            &done.header().object_id().to_string(),
            &done,
        )
        .await;

        EpisodeObserver::new(&history, fixture.database.as_ref(), &fixture.digest, "repo")
            .expect("construct terminal observer")
            .observe_terminal_events()
            .await
            .expect("observe terminal Task event");
        let config = EpisodeCompileConfig::new(
            REPO_EPISODE_PRODUCER,
            1,
            "m2-08-fake-v1",
            "deterministic-fake",
        )
        .expect("construct compile config");
        let runner = EpisodeGenerationRunner::new(
            &history,
            fixture.database.as_ref(),
            &fixture.digest,
            &fixture.writer,
            "repo",
            EpisodeSourceLimits::repo_v1(),
        )
        .expect("construct generation runner");
        assert_eq!(
            runner
                .run_one(&FakeCompiler, &config, "runner-a", 2_000_000_000_000)
                .await
                .expect("run one generation"),
            GenerationRunOutcome::Committed {
                appended: true,
                new_generation_pending: false,
            }
        );

        let job = fixture
            .database
            .query_one_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "SELECT state, observed_generation, processed_generation, lease_owner
                 FROM memory_compile_job
                 WHERE scope_key = 'repo' AND root_kind = 'task' AND root_id = ?",
                [task_id.to_string().into()],
            ))
            .await
            .expect("query completed job")
            .expect("completed job exists");
        assert_eq!(job.try_get::<String>("", "state").unwrap(), "idle");
        assert_eq!(job.try_get::<i64>("", "observed_generation").unwrap(), 1);
        assert_eq!(job.try_get::<i64>("", "processed_generation").unwrap(), 1);
        assert_eq!(
            job.try_get::<Option<String>>("", "lease_owner").unwrap(),
            None
        );

        let memory = fixture
            .database
            .query_one_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "SELECT latest_review_state FROM memory_head
                 WHERE scope_key = 'repo' AND note_id = ?",
                [EpisodeRoot::task(task_id.to_string())
                    .unwrap()
                    .note_id()
                    .to_string()
                    .into()],
            ))
            .await
            .expect("query generated Memory head")
            .expect("generated Memory head exists");
        assert_eq!(
            memory.try_get::<String>("", "latest_review_state").unwrap(),
            "confirmed"
        );
    }
}
