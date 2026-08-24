use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use git_internal::hash::ObjectHash;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, QueryResult, Statement};
use serde::Serialize;

use super::{
    domain::{MemoryEventV1, MemoryNoteV1},
    error::{MemoryWriterError, MemoryWriterErrorKind},
    tree::parse_oid,
};
use crate::internal::{
    ai::{keyed_digest::RepositoryKeyedDigest, linear_ref::LinearRefCompanion},
    workspace::RepoIdentity,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ProjectedCell {
    pub(super) note_id: String,
    pub(super) latest_revision_oid: ObjectHash,
    pub(super) live_revision_oid: Option<String>,
}

pub(super) async fn read_memory_ref_head(
    database: &DatabaseConnection,
) -> Result<Option<ObjectHash>, MemoryWriterError> {
    let rows = database
        .query_all_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT `commit` FROM reference
             WHERE kind = 'Branch' AND remote IS NULL AND name = ? LIMIT 2",
            ["libra/memory/repo".into()],
        ))
        .await
        .map_err(|error| projection_error("query Memory ref", error))?;
    if rows.len() > 1 {
        return Err(corrupt_projection("duplicate repository Memory refs exist"));
    }
    rows.into_iter()
        .next()
        .map(|row| {
            let value: Option<String> = row
                .try_get("", "commit")
                .map_err(|error| projection_error("decode Memory ref", error))?;
            value
                .ok_or_else(|| corrupt_projection("Memory ref has no commit"))
                .and_then(|value| parse_oid(&value))
        })
        .transpose()
}

pub(super) async fn validate_projection_watermark(
    database: &DatabaseConnection,
    head: Option<ObjectHash>,
    event_seq: u64,
) -> Result<(), MemoryWriterError> {
    let row = database
        .query_one_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT projected_ref_oid, last_event_seq, schema_version
             FROM memory_projection_state WHERE scope_key = 'repo'",
            [],
        ))
        .await
        .map_err(|error| projection_error("query Memory projection watermark", error))?;
    match (head, row) {
        (None, None) => Ok(()),
        (Some(head), Some(row)) => {
            let projected: String = row
                .try_get("", "projected_ref_oid")
                .map_err(|error| projection_error("decode Memory projection watermark", error))?;
            let projected_seq: i64 = row
                .try_get("", "last_event_seq")
                .map_err(|error| projection_error("decode Memory projection sequence", error))?;
            let schema_version: i64 = row
                .try_get("", "schema_version")
                .map_err(|error| projection_error("decode Memory projection schema", error))?;
            let expected_seq = i64::try_from(event_seq).map_err(|_| {
                corrupt_projection("Memory manifest event sequence exceeds SQLite range")
            })?;
            if projected == head.to_string() && projected_seq == expected_seq && schema_version == 1
            {
                Ok(())
            } else {
                Err(corrupt_projection(
                    "Memory projection watermark does not match the authoritative ref",
                ))
            }
        }
        _ => Err(corrupt_projection(
            "Memory ref and projection watermark are not initialized together",
        )),
    }
}

pub(super) async fn find_cell(
    database: &DatabaseConnection,
    namespace: &str,
    path: &str,
) -> Result<Option<ProjectedCell>, MemoryWriterError> {
    let rows = database
        .query_all_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT note_id, latest_revision_oid, live_revision_oid
             FROM memory_head
             WHERE scope_key = 'repo' AND namespace = ? AND path = ? LIMIT 2",
            [namespace.into(), path.into()],
        ))
        .await
        .map_err(|error| projection_error("query Memory Cell", error))?;
    decode_unique_cell(rows)
}

fn decode_unique_cell(rows: Vec<QueryResult>) -> Result<Option<ProjectedCell>, MemoryWriterError> {
    if rows.len() > 1 {
        return Err(corrupt_projection(
            "duplicate Memory heads exist for one Cell",
        ));
    }
    rows.into_iter()
        .next()
        .map(projected_cell_from_row)
        .transpose()
}

fn projected_cell_from_row(row: QueryResult) -> Result<ProjectedCell, MemoryWriterError> {
    let note_id = row
        .try_get("", "note_id")
        .map_err(|error| projection_error("decode Memory note ID", error))?;
    let latest: String = row
        .try_get("", "latest_revision_oid")
        .map_err(|error| projection_error("decode Memory revision OID", error))?;
    let live_revision_oid = row
        .try_get("", "live_revision_oid")
        .map_err(|error| projection_error("decode Memory live revision OID", error))?;
    Ok(ProjectedCell {
        note_id,
        latest_revision_oid: parse_oid(&latest)?,
        live_revision_oid,
    })
}

#[derive(Clone)]
pub(super) struct ProjectionMutation {
    pub(super) note: MemoryNoteV1,
    pub(super) event: MemoryEventV1,
    pub(super) revision_oid: ObjectHash,
    pub(super) commit_oid: ObjectHash,
    pub(super) is_create: bool,
    pub(super) rebuilt_at_ms: i64,
    pub(super) expected_head: Option<ObjectHash>,
    pub(super) expected_event_seq: u64,
    pub(super) expected_cell: Option<ProjectedCell>,
    pub(super) repository_id: String,
    pub(super) digest_provider: Arc<RepositoryKeyedDigest>,
}

#[async_trait]
impl LinearRefCompanion for ProjectionMutation {
    async fn apply(&self, txn: &DatabaseTransaction) -> Result<()> {
        revalidate_snapshot(txn, self).await?;
        if self.is_create {
            insert_note(txn, &self.note).await?;
        } else {
            update_note(txn, &self.note).await?;
        }
        insert_revision(txn, &self.note, self.revision_oid).await?;
        replace_links(txn, &self.note, self.revision_oid).await?;
        replace_episode_paths(txn, &self.note, self.revision_oid).await?;
        upsert_head(txn, &self.note, &self.event, self.revision_oid).await?;
        upsert_projection_state(
            txn,
            &self.note,
            &self.event,
            self.commit_oid,
            self.rebuilt_at_ms,
        )
        .await?;
        Ok(())
    }
}

async fn revalidate_snapshot(
    txn: &DatabaseTransaction,
    mutation: &ProjectionMutation,
) -> Result<(), MemoryWriterError> {
    let repository = RepoIdentity::resolve(txn).await.map_err(|error| {
        MemoryWriterError::new(
            MemoryWriterErrorKind::CorruptProjection,
            format!("repository identity is invalid during Memory commit: {error}"),
        )
    })?;
    if repository.as_str() != mutation.repository_id
        || mutation.digest_provider.repository_id() != mutation.repository_id
    {
        return Err(corrupt_projection(
            "repository identity changed during Memory commit",
        ));
    }
    mutation
        .digest_provider
        .validate_for_connection(txn)
        .await
        .map_err(|error| {
            MemoryWriterError::new(
                MemoryWriterErrorKind::DigestKeyUnavailable,
                error.to_string(),
            )
        })?;

    let projection = txn
        .query_one_raw(Statement::from_string(
            txn.get_database_backend(),
            "SELECT projected_ref_oid, last_event_seq FROM memory_projection_state
             WHERE scope_key = 'repo'"
                .to_string(),
        ))
        .await
        .map_err(|error| projection_error("revalidate Memory projection watermark", error))?;
    match (mutation.expected_head, projection) {
        (None, None) => {}
        (Some(expected), Some(row)) => {
            let projected: String = row.try_get("", "projected_ref_oid").map_err(|error| {
                projection_error("decode revalidated Memory projection watermark", error)
            })?;
            let event_seq: i64 = row.try_get("", "last_event_seq").map_err(|error| {
                projection_error("decode revalidated Memory projection sequence", error)
            })?;
            let expected_event_seq = i64::try_from(mutation.expected_event_seq).map_err(|_| {
                corrupt_projection("Memory snapshot event sequence exceeds SQLite range")
            })?;
            if projected != expected.to_string() || event_seq != expected_event_seq {
                return Err(corrupt_projection(
                    "Memory projection changed after the writer snapshot",
                ));
            }
        }
        _ => {
            return Err(corrupt_projection(
                "Memory projection appeared or disappeared after the writer snapshot",
            ));
        }
    }

    let current_cell =
        find_cell_in_transaction(txn, &mutation.note.namespace, &mutation.note.path).await?;
    if current_cell != mutation.expected_cell {
        return Err(corrupt_projection(
            "Memory Cell changed after the writer snapshot",
        ));
    }
    Ok(())
}

async fn find_cell_in_transaction(
    txn: &DatabaseTransaction,
    namespace: &str,
    path: &str,
) -> Result<Option<ProjectedCell>, MemoryWriterError> {
    let rows = txn
        .query_all_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT note_id, latest_revision_oid, live_revision_oid
         FROM memory_head
         WHERE scope_key = 'repo' AND namespace = ? AND path = ? LIMIT 2",
            [namespace.into(), path.into()],
        ))
        .await
        .map_err(|error| projection_error("revalidate Memory Cell", error))?;
    decode_unique_cell(rows)
}

async fn insert_note(txn: &DatabaseTransaction, note: &MemoryNoteV1) -> Result<()> {
    execute(
        txn,
        "INSERT INTO memory_note_index (
            note_id, scope_key, namespace, path, kind, lifecycle, review_state,
            confidence, trust, sensitivity, visibility, acl_policy_id, origin,
            idempotency_key, idempotency_scope, created_at
         ) VALUES (?, 'repo', ?, ?, ?, ?, 'confirmed', ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        vec![
            note.note_id.to_string().into(),
            note.namespace.clone().into(),
            note.path.clone().into(),
            enum_label(&note.kind)?.into(),
            enum_label(&note.lifecycle)?.into(),
            enum_label(&note.confidence)?.into(),
            enum_label(&note.trust)?.into(),
            enum_label(&note.sensitivity)?.into(),
            enum_label(&note.visibility)?.into(),
            note.acl_policy_id.clone().into(),
            enum_label(&note.compile_record.origin)?.into(),
            note.compile_record.idempotency_key.clone().into(),
            enum_label(&note.compile_record.idempotency_scope)?.into(),
            note.created_at.to_rfc3339().into(),
        ],
    )
    .await
}

async fn update_note(txn: &DatabaseTransaction, note: &MemoryNoteV1) -> Result<()> {
    let result = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE memory_note_index SET
                review_state = 'confirmed', confidence = ?, trust = ?, sensitivity = ?,
                visibility = ?, acl_policy_id = ?
             WHERE note_id = ? AND scope_key = 'repo' AND namespace = ? AND path = ?",
            [
                enum_label(&note.confidence)?.into(),
                enum_label(&note.trust)?.into(),
                enum_label(&note.sensitivity)?.into(),
                enum_label(&note.visibility)?.into(),
                note.acl_policy_id.clone().into(),
                note.note_id.to_string().into(),
                note.namespace.clone().into(),
                note.path.clone().into(),
            ],
        ))
        .await
        .context("update Memory note projection")?;
    if result.rows_affected() != 1 {
        anyhow::bail!("Memory note projection disappeared during writer transaction");
    }
    Ok(())
}

async fn insert_revision(
    txn: &DatabaseTransaction,
    note: &MemoryNoteV1,
    revision_oid: ObjectHash,
) -> Result<()> {
    execute(
        txn,
        "INSERT INTO memory_revision_index (
            revision_oid, note_id, scope_key, namespace, origin, producer,
            rules_version, prompt_version, model_id, policy_version,
            input_fingerprints_json, created_at
         ) VALUES (?, ?, 'repo', ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        vec![
            revision_oid.to_string().into(),
            note.note_id.to_string().into(),
            note.namespace.clone().into(),
            enum_label(&note.compile_record.origin)?.into(),
            note.compile_record.producer.clone().into(),
            i64::from(note.compile_record.rules_version).into(),
            note.compile_record.prompt_version.clone().into(),
            note.compile_record.model_id.clone().into(),
            note.compile_record.policy_version.clone().into(),
            serde_json::to_string(&note.compile_record.input_hashes)?.into(),
            note.created_at.to_rfc3339().into(),
        ],
    )
    .await
}

async fn replace_links(
    txn: &DatabaseTransaction,
    note: &MemoryNoteV1,
    revision_oid: ObjectHash,
) -> Result<()> {
    for link in &note.links {
        let target = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT path FROM memory_note_index WHERE note_id = ?",
                [link.target_note_id.to_string().into()],
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("Memory link target does not exist"))?;
        let target_path: String = target.try_get("", "path")?;
        execute(
            txn,
            "INSERT INTO memory_link_index (
                source_scope_key, source_namespace, source_note_id,
                source_revision_oid, target_note_id, target_revision_oid,
                link_kind, source_path, target_path, evidence_refs_json,
                valid_from, valid_until
             ) VALUES ('repo', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                note.namespace.clone().into(),
                note.note_id.to_string().into(),
                revision_oid.to_string().into(),
                link.target_note_id.to_string().into(),
                link.target_revision_oid.clone().into(),
                enum_label(&link.kind)?.into(),
                note.path.clone().into(),
                target_path.into(),
                serde_json::to_string(&link.evidence_refs)?.into(),
                link.valid_from.map(|value| value.to_rfc3339()).into(),
                link.valid_until.map(|value| value.to_rfc3339()).into(),
            ],
        )
        .await?;
    }
    Ok(())
}

async fn replace_episode_paths(
    txn: &DatabaseTransaction,
    note: &MemoryNoteV1,
    revision_oid: ObjectHash,
) -> Result<()> {
    if let Some(episode) = &note.episode {
        for path in &episode.code.paths {
            execute(
                txn,
                "INSERT INTO memory_episode_path(note_id, revision_oid, code_path)
                 VALUES (?, ?, ?)",
                vec![
                    note.note_id.to_string().into(),
                    revision_oid.to_string().into(),
                    path.clone().into(),
                ],
            )
            .await?;
        }
    }
    Ok(())
}

async fn upsert_head(
    txn: &DatabaseTransaction,
    note: &MemoryNoteV1,
    event: &MemoryEventV1,
    revision_oid: ObjectHash,
) -> Result<()> {
    execute(
        txn,
        "INSERT INTO memory_head (
            scope_key, namespace, path, note_id, latest_revision_oid,
            live_revision_oid, latest_action, latest_review_state, kind,
            lifecycle, confidence, trust, sensitivity, visibility, acl_policy_id,
            valid_from, valid_until, effective_from_commit, effective_until_commit,
            expires_at, rank_hint, last_event_seq, updated_at
         ) VALUES ('repo', ?, ?, ?, ?, ?, ?, 'confirmed', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?)
         ON CONFLICT(scope_key, namespace, path, note_id) DO UPDATE SET
            latest_revision_oid = excluded.latest_revision_oid,
            live_revision_oid = excluded.live_revision_oid,
            latest_action = excluded.latest_action,
            latest_review_state = excluded.latest_review_state,
            confidence = excluded.confidence,
            trust = excluded.trust,
            sensitivity = excluded.sensitivity,
            visibility = excluded.visibility,
            acl_policy_id = excluded.acl_policy_id,
            effective_from_commit = excluded.effective_from_commit,
            last_event_seq = excluded.last_event_seq,
            updated_at = excluded.updated_at",
        vec![
            note.namespace.clone().into(),
            note.path.clone().into(),
            note.note_id.to_string().into(),
            revision_oid.to_string().into(),
            revision_oid.to_string().into(),
            enum_label(&event.action)?.into(),
            enum_label(&note.kind)?.into(),
            enum_label(&note.lifecycle)?.into(),
            enum_label(&note.confidence)?.into(),
            enum_label(&note.trust)?.into(),
            enum_label(&note.sensitivity)?.into(),
            enum_label(&note.visibility)?.into(),
            note.acl_policy_id.clone().into(),
            note.valid_from.map(|value| value.to_rfc3339()).into(),
            note.valid_until.map(|value| value.to_rfc3339()).into(),
            note.effective_from_commit.clone().into(),
            note.effective_until_commit.clone().into(),
            note.expires_at.map(|value| value.to_rfc3339()).into(),
            i64::try_from(event.event_seq)?.into(),
            event.at.to_rfc3339().into(),
        ],
    )
    .await
}

async fn upsert_projection_state(
    txn: &DatabaseTransaction,
    note: &MemoryNoteV1,
    event: &MemoryEventV1,
    commit_oid: ObjectHash,
    rebuilt_at_ms: i64,
) -> Result<()> {
    execute(
        txn,
        "INSERT INTO memory_projection_state (
            scope_key, projected_ref_oid, last_event_seq, schema_version,
            policy_version, rebuilt_at
         ) VALUES ('repo', ?, ?, 1, ?, ?)
         ON CONFLICT(scope_key) DO UPDATE SET
            projected_ref_oid = excluded.projected_ref_oid,
            last_event_seq = excluded.last_event_seq,
            schema_version = excluded.schema_version,
            policy_version = excluded.policy_version,
            rebuilt_at = excluded.rebuilt_at",
        vec![
            commit_oid.to_string().into(),
            i64::try_from(event.event_seq)?.into(),
            note.compile_record.policy_version.clone().into(),
            rebuilt_at_ms.into(),
        ],
    )
    .await
}

async fn execute(txn: &DatabaseTransaction, sql: &str, values: Vec<sea_orm::Value>) -> Result<()> {
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        sql,
        values,
    ))
    .await?;
    Ok(())
}

fn enum_label<T: Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value).context("serialize Memory enum")?;
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Memory enum did not serialize as a string"))
}

fn projection_error(action: &'static str, error: impl std::fmt::Display) -> MemoryWriterError {
    MemoryWriterError::new(
        MemoryWriterErrorKind::StorageFailure,
        format!("{action} failed: {error}"),
    )
}

fn corrupt_projection(summary: &'static str) -> MemoryWriterError {
    MemoryWriterError::new(MemoryWriterErrorKind::CorruptProjection, summary)
}
