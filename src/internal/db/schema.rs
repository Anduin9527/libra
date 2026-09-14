//! Explicit database ownership, schema manifests and configuration migrations.
//!
//! Repository compatibility wrappers select Repository, never infer ownership
//! from a filename. Configuration migration receipts have their own namespace;
//! legacy repository receipts are retained for the later confirmed repair path.

use std::{fmt, io, path::Path, sync::OnceLock, time::Duration};

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr, Statement, TransactionTrait,
};

use super::{SchemaCompatibility, SchemaUpgradeReport, migration::Migration};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DatabaseRole {
    Repository,
    GlobalConfig,
    SystemConfig,
    Derived,
}

impl fmt::Display for DatabaseRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Repository => "repository",
            Self::GlobalConfig => "global configuration",
            Self::SystemConfig => "system configuration",
            Self::Derived => "derived",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaLedger {
    Repository,
    Configuration,
}

impl SchemaLedger {
    pub fn table_name(self) -> &'static str {
        match self {
            Self::Repository => "schema_versions",
            Self::Configuration => "configuration_schema_versions",
        }
    }
}

const REPOSITORY: &[DatabaseRole] = &[DatabaseRole::Repository];
const CONFIGURATION: &[DatabaseRole] = &[DatabaseRole::GlobalConfig, DatabaseRole::SystemConfig];
const PERSISTENT: &[DatabaseRole] = &[
    DatabaseRole::Repository,
    DatabaseRole::GlobalConfig,
    DatabaseRole::SystemConfig,
];

pub(crate) const CONFIG_KV_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS `config_kv` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `key` TEXT NOT NULL,
    `value` TEXT NOT NULL,
    `encrypted` INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_config_kv_key ON config_kv(`key`);
"#;
const LEGACY_CONFIG_SQL: &str =
    include_str!("../../../sql/migrations/2026090601_legacy_config_table.sql");
const CONFIGURATION_LEDGER_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS configuration_schema_versions (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TEXT NOT NULL
);
"#;

#[derive(Clone, Debug)]
pub struct ScopedMigration {
    pub roles: &'static [DatabaseRole],
    pub migration: Migration,
}

#[derive(Clone, Copy, Debug)]
pub struct BootstrapDefinition {
    pub name: &'static str,
    pub roles: &'static [DatabaseRole],
    pub sql: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaTopUp {
    ConfigKv,
    AiProjection,
    AiRuntimeContract,
    RebaseShape,
    BisectShape,
}

#[derive(Clone, Copy, Debug)]
pub struct TopUpDefinition {
    pub roles: &'static [DatabaseRole],
    pub action: SchemaTopUp,
}

const BOOTSTRAPS: &[BootstrapDefinition] = &[
    BootstrapDefinition {
        name: "repository_core",
        roles: REPOSITORY,
        sql: super::BOOTSTRAP_SQL,
    },
    BootstrapDefinition {
        name: "configuration_legacy",
        roles: CONFIGURATION,
        sql: LEGACY_CONFIG_SQL,
    },
    BootstrapDefinition {
        name: "configuration_kv",
        roles: CONFIGURATION,
        sql: CONFIG_KV_SQL,
    },
];

const TOP_UPS: &[TopUpDefinition] = &[
    TopUpDefinition {
        roles: PERSISTENT,
        action: SchemaTopUp::ConfigKv,
    },
    TopUpDefinition {
        roles: REPOSITORY,
        action: SchemaTopUp::AiProjection,
    },
    TopUpDefinition {
        roles: REPOSITORY,
        action: SchemaTopUp::AiRuntimeContract,
    },
    TopUpDefinition {
        roles: REPOSITORY,
        action: SchemaTopUp::RebaseShape,
    },
    TopUpDefinition {
        roles: REPOSITORY,
        action: SchemaTopUp::BisectShape,
    },
];

/// The single manifest for runtime migrations, bootstraps and top-ups.
pub struct SchemaManifest {
    pub migrations: Vec<ScopedMigration>,
    pub bootstraps: &'static [BootstrapDefinition],
    pub top_ups: &'static [TopUpDefinition],
}

pub fn schema_manifest() -> &'static SchemaManifest {
    static MANIFEST: OnceLock<SchemaManifest> = OnceLock::new();
    MANIFEST.get_or_init(build_schema_manifest)
}

fn build_schema_manifest() -> SchemaManifest {
    // All historical runtime migrations belong to the repository namespace.
    // A configuration-owned migration is registered separately, even when its
    // numeric ID and SQL also occur in the repository namespace.
    let mut migrations: Vec<_> = super::migration::repository_migrations()
        .into_iter()
        .map(|migration| ScopedMigration {
            roles: REPOSITORY,
            migration,
        })
        .collect();
    migrations.push(ScopedMigration {
        roles: CONFIGURATION,
        migration: Migration {
            version: 2026090601,
            name: "configuration_base",
            up: LEGACY_CONFIG_SQL,
            down: None,
        },
    });
    SchemaManifest {
        migrations,
        bootstraps: BOOTSTRAPS,
        top_ups: TOP_UPS,
    }
}

pub fn ledger_for_role(role: DatabaseRole) -> io::Result<SchemaLedger> {
    match role {
        DatabaseRole::Repository => Ok(SchemaLedger::Repository),
        DatabaseRole::GlobalConfig | DatabaseRole::SystemConfig => Ok(SchemaLedger::Configuration),
        DatabaseRole::Derived => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "derived databases are owned by their subsystem; use its schema API instead of runtime migrations",
        )),
    }
}

pub fn migrations_for_role(role: DatabaseRole) -> Vec<Migration> {
    schema_manifest()
        .migrations
        .iter()
        .filter(|entry| entry.roles.contains(&role))
        .map(|entry| entry.migration.clone())
        .collect()
}

pub fn bootstraps_for_role(
    role: DatabaseRole,
) -> impl Iterator<Item = &'static BootstrapDefinition> {
    BOOTSTRAPS
        .iter()
        .filter(move |entry| entry.roles.contains(&role))
}

pub fn top_ups_for_role(role: DatabaseRole) -> impl Iterator<Item = SchemaTopUp> {
    TOP_UPS
        .iter()
        .filter(move |entry| entry.roles.contains(&role))
        .map(|entry| entry.action)
}

pub fn latest_schema_version_for_role(role: DatabaseRole) -> io::Result<Option<i64>> {
    // Validate once per role, not on every cache hit or configuration lookup.
    // All successful subsequent inspections perform only the two SQL queries.
    static LATEST: [OnceLock<Result<Option<i64>, String>>; 3] = [const { OnceLock::new() }; 3];
    let index = match role {
        DatabaseRole::Repository => 0,
        DatabaseRole::GlobalConfig => 1,
        DatabaseRole::SystemConfig => 2,
        DatabaseRole::Derived => return ledger_for_role(role).map(|_| None),
    };
    LATEST[index]
        .get_or_init(|| {
            let mut runner = super::migration::MigrationRunner::new();
            runner
                .extend(migrations_for_role(role))
                .map_err(|error| format!("invalid {role} migration manifest: {error}"))?;
            Ok(runner.max_registered_version())
        })
        .as_ref()
        .copied()
        .map_err(|error| io::Error::other(error.clone()))
}

/// Two indexed metadata queries, with no DDL and no configuration-value reads.
pub async fn current_schema_version_for_role<C: ConnectionTrait>(
    conn: &C,
    role: DatabaseRole,
) -> io::Result<Option<i64>> {
    let table = ledger_for_role(role)?.table_name();
    let exists = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
            [table.into()],
        ))
        .await
        .map_err(|error| io::Error::other(format!("failed to inspect {role} ledger: {error}")))?;
    if exists.is_none() {
        return Ok(None);
    }
    // `table` is selected from a closed enum, never a path or user input.
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("SELECT MAX(version) FROM {table}"),
        ))
        .await
        .map_err(|error| {
            io::Error::other(format!("failed to read {role} schema version: {error}"))
        })?;
    let row = row.ok_or_else(|| {
        io::Error::other(format!("{role} schema version query returned no result"))
    })?;
    row.try_get_by_index(0)
        .map_err(|error| io::Error::other(format!("invalid {role} schema version: {error}")))
}

pub async fn inspect_schema_for_connection(
    conn: &DatabaseConnection,
    role: DatabaseRole,
) -> io::Result<SchemaCompatibility> {
    let current = current_schema_version_for_role(conn, role).await?;
    let latest = latest_schema_version_for_role(role)?;
    Ok(match (current, latest) {
        (Some(current), latest) if latest.is_none_or(|latest| current > latest) => {
            SchemaCompatibility::UnsupportedFuture {
                current_version: current,
                latest_version: latest,
            }
        }
        (current, Some(latest)) if current != Some(latest) => {
            SchemaCompatibility::UpgradeRequired {
                current_version: current,
                latest_version: latest,
            }
        }
        (current, latest) => SchemaCompatibility::Compatible {
            current_version: current,
            latest_version: latest,
        },
    })
}

fn reject_future(role: DatabaseRole, compatibility: &SchemaCompatibility) -> io::Result<()> {
    if let SchemaCompatibility::UnsupportedFuture {
        current_version,
        latest_version,
    } = compatibility
    {
        return Err(io::Error::other(format!(
            "{role} database schema version {current_version} is newer than this Libra binary supports (latest supported: {}); install a newer Libra binary",
            super::format_schema_version(*latest_version),
        )));
    }
    Ok(())
}

pub async fn establish_connection_for_role(
    db_path: &str,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    establish_connection_with_busy_timeout_for_role(db_path, Duration::from_secs(30), role).await
}

pub async fn establish_connection_with_busy_timeout_for_role(
    db_path: &str,
    busy_timeout: Duration,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    ledger_for_role(role)?;
    let conn = super::open_connection_without_schema_management(db_path, busy_timeout).await?;
    let compatibility = inspect_schema_for_connection(&conn, role).await?;
    reject_future(role, &compatibility)?;
    if matches!(compatibility, SchemaCompatibility::UpgradeRequired { .. }) {
        upgrade_connection_for_role(&conn, role).await?;
    }
    Ok(conn)
}

pub async fn inspect_database_schema_for_role(
    db_path: &Path,
    role: DatabaseRole,
) -> io::Result<SchemaCompatibility> {
    ledger_for_role(role)?;
    let conn = super::open_database_without_migrations(db_path).await?;
    let result = inspect_schema_for_connection(&conn, role).await;
    conn.close().await.map_err(|error| {
        io::Error::other(format!("failed to close {role} schema inspection: {error}"))
    })?;
    result
}

pub async fn upgrade_database_schema_for_role(
    db_path: &Path,
    role: DatabaseRole,
) -> io::Result<SchemaUpgradeReport> {
    ledger_for_role(role)?;
    let conn = super::open_database_without_migrations(db_path).await?;
    let result = upgrade_connection_for_role(&conn, role).await;
    conn.close().await.map_err(|error| {
        io::Error::other(format!("failed to close {role} schema upgrade: {error}"))
    })?;
    result
}

pub async fn upgrade_connection_for_role(
    conn: &DatabaseConnection,
    role: DatabaseRole,
) -> io::Result<SchemaUpgradeReport> {
    let ledger = ledger_for_role(role)?;
    reject_future(role, &inspect_schema_for_connection(conn, role).await?)?;
    if ledger == SchemaLedger::Repository {
        return super::apply_database_schema_upgrades(conn).await;
    }

    let migrations = migrations_for_role(role);
    let latest = latest_schema_version_for_role(role)?;
    // Ledger creation, the write lock, the under-lock version recheck, DDL and
    // receipts share one transaction. Failure cannot leave an empty new ledger
    // or advance a receipt without its corresponding schema.
    conn.transaction::<_, _, DbErr>(|txn| {
        Box::pin(upgrade_configuration_transaction(
            txn, role, migrations, latest,
        ))
    })
    .await
    .map_err(|error| {
        io::Error::other(format!(
            "failed to upgrade {role} schema; transaction rolled back: {error}"
        ))
    })
}

async fn upgrade_configuration_transaction(
    txn: &DatabaseTransaction,
    role: DatabaseRole,
    migrations: Vec<Migration>,
    latest: Option<i64>,
) -> Result<SchemaUpgradeReport, DbErr> {
    txn.execute_unprepared(CONFIGURATION_LEDGER_SQL).await?;
    txn.execute_unprepared("UPDATE configuration_schema_versions SET version = version WHERE 0")
        .await?;
    let previous = current_schema_version_for_role(txn, role)
        .await
        .map_err(|error| DbErr::Custom(error.to_string()))?;
    if previous.is_some_and(|current| latest.is_none_or(|latest| current > latest)) {
        return Err(DbErr::Custom(format!(
            "{role} schema advanced concurrently; install a newer Libra binary"
        )));
    }
    let mut applied = Vec::new();
    if previous != latest {
        for bootstrap in bootstraps_for_role(role) {
            txn.execute_unprepared(bootstrap.sql).await?;
        }
        for top_up in top_ups_for_role(role) {
            match top_up {
                SchemaTopUp::ConfigKv => {
                    txn.execute_unprepared(CONFIG_KV_SQL).await?;
                }
                _ => {
                    return Err(DbErr::Custom(format!(
                        "repository top-up is not permitted for {role}"
                    )));
                }
            }
        }
        for migration in migrations {
            if previous.is_some_and(|version| migration.version <= version) {
                continue;
            }
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "INSERT INTO configuration_schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
                [
                    migration.version.into(),
                    migration.name.into(),
                    chrono::Utc::now().to_rfc3339().into(),
                ],
            ))
            .await?;
            txn.execute_unprepared(migration.up).await?;
            applied.push(migration.version);
        }
    }
    Ok(SchemaUpgradeReport {
        previous_version: previous,
        current_version: latest,
        latest_version: latest,
        applied_versions: applied,
    })
}

pub async fn create_database_for_role(
    db_path: &str,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    ledger_for_role(role)?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(db_path)
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot create {role} database '{db_path}': {error}"),
            )
        })?;
    let conn = super::connect_database(db_path).await?;
    if role == DatabaseRole::Repository {
        super::setup_database_sql(&conn).await.map_err(|error| {
            io::Error::other(format!("failed to bootstrap {role} database: {error}"))
        })?;
    }
    upgrade_connection_for_role(&conn, role).await?;
    Ok(conn)
}
