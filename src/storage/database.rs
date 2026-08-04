use std::{path::Path, time::Duration};

use sqlx::{
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use thiserror::Error;

pub(crate) static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("database migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("account context interval overlaps an existing interval")]
    ContextIntervalOverlap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PragmaStatus {
    pub journal_mode: String,
    pub foreign_keys: i64,
    pub busy_timeout_ms: i64,
}

#[derive(Clone)]
pub struct Database {
    pub(crate) pool: SqlitePool,
}

impl Database {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5))
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(4)
            .connect_with(options)
            .await?;
        let database = Self { pool };
        database.migrate().await?;
        Ok(database)
    }

    pub async fn connect_in_memory() -> Result<Self, StorageError> {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(1)
            .connect_with(options)
            .await?;
        let database = Self { pool };
        database.migrate().await?;
        Ok(database)
    }

    pub async fn migrate(&self) -> Result<(), StorageError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    pub async fn pragma_status(&self) -> Result<PragmaStatus, StorageError> {
        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&self.pool)
            .await?;
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&self.pool)
            .await?;
        let busy_timeout_ms: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&self.pool)
            .await?;
        Ok(PragmaStatus {
            journal_mode,
            foreign_keys,
            busy_timeout_ms,
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn close(self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn temp_database_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "codex-meter-{label}-{}-{nonce}.sqlite",
            std::process::id()
        ))
    }

    fn cleanup(path: &PathBuf) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = fs::remove_file(path.with_extension("sqlite-shm"));
    }

    #[tokio::test]
    async fn migration_is_repeatable_and_pragmas_are_enabled() {
        let path = temp_database_path("migration");
        let database = Database::connect(&path).await.unwrap();
        let first_status = database.pragma_status().await.unwrap();
        assert_eq!(first_status.journal_mode.to_lowercase(), "wal");
        assert_eq!(first_status.foreign_keys, 1);
        assert_eq!(first_status.busy_timeout_ms, 5_000);
        database.close().await;

        let database = Database::connect(&path).await.unwrap();
        database.migrate().await.unwrap();
        let migration_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(migration_count, 2);
        assert_eq!(database.pragma_status().await.unwrap().foreign_keys, 1);
        database.close().await;
        cleanup(&path);
    }
}
