use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::Value;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use thiserror::Error;

use super::TokenCounts;

const MINIMAL_SCHEMA: &str = include_str!("../../migrations/0002_minimal_data_model.sql");

pub const MINIMAL_TABLES: [&str; 7] = [
    "source_jsonl",
    "source_app_server",
    "source_ccusage",
    "usage_daily",
    "usage_minute",
    "usage_session",
    "capacities_v2",
];

#[derive(Debug, Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database path has no parent directory")]
    MissingParent,
}

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
    path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct EventRecord {
    pub source_digest: String,
    pub observed_at_ms: i64,
    pub kind: String,
    pub session_id: Option<String>,
    pub root_session_id: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub tier: Option<String>,
    pub provider: Option<String>,
    pub account_key: Option<String>,
    pub plan: Option<String>,
    pub last_tokens: Option<TokenCounts>,
    pub cumulative_tokens: Option<TokenCounts>,
    pub fast: Option<bool>,
    pub quota_json: Option<Value>,
    pub payload: Value,
    pub quality: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct QuotaRecord {
    pub event_id: i64,
    pub source_digest: String,
    pub observed_at_ms: i64,
    pub account_key: Option<String>,
    pub limit_id: Option<String>,
    pub window_kind: String,
    pub used_percent: Option<f64>,
    pub window_minutes: Option<i64>,
    pub resets_at_ms: Option<i64>,
    pub plan: Option<String>,
    pub source: String,
    pub raw_json: Value,
}

#[derive(Clone, Debug, Default)]
pub struct SourceJsonlRecord {
    pub source_key: String,
    pub kind: String,
    pub observed_at_ms: i64,
    pub last_seen_at_ms: Option<i64>,
    pub session_id: Option<String>,
    pub root_session_id: Option<String>,
    pub turn_id: Option<String>,
    pub relation: Option<String>,
    pub title: Option<String>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub provider: Option<String>,
    pub plan_type: Option<String>,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub limit_id: Option<String>,
    pub window_kind: Option<String>,
    pub used_percent: Option<f64>,
    pub window_minutes: Option<i64>,
    pub resets_at_ms: Option<i64>,
    pub quality: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SourceAppServerRecord {
    pub source_key: String,
    pub kind: String,
    pub first_seen_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub account_key: Option<String>,
    pub account_label: Option<String>,
    pub auth_kind: Option<String>,
    pub provider: Option<String>,
    pub plan_type: Option<String>,
    pub limit_id: Option<String>,
    pub window_kind: Option<String>,
    pub used_percent: Option<f64>,
    pub window_minutes: Option<i64>,
    pub resets_at_ms: Option<i64>,
    pub lifetime_tokens: Option<i64>,
    pub daily_tokens_json: Option<String>,
    pub freshness: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Default)]
pub struct SourceCcusageRecord {
    pub source_key: String,
    pub run_at_ms: i64,
    pub range_start_ms: i64,
    pub range_end_ms: i64,
    pub scope: String,
    pub scope_key: String,
    pub pricing_scheme: String,
    pub speed: String,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub amount: Option<f64>,
    pub model_breakdown_json: Option<String>,
    pub ccusage_version: Option<String>,
    pub pricing_version: Option<String>,
    pub status: String,
}

impl Database {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| DbError::MissingParent)?;
        }
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
        let database = Self {
            pool,
            path: Some(path.to_owned()),
        };
        database.initialize().await?;
        Ok(database)
    }

    pub async fn connect_in_memory() -> Result<Self, DbError> {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(1)
            .connect_with(options)
            .await?;
        let database = Self { pool, path: None };
        database.initialize().await?;
        Ok(database)
    }

    async fn initialize(&self) -> Result<(), DbError> {
        for statement in MINIMAL_SCHEMA.split(';') {
            let statement = statement.trim();
            if !statement.is_empty() {
                sqlx::query(statement).execute(&self.pool).await?;
            }
        }
        Ok(())
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn sidecar_path(&self, suffix: &str) -> Option<PathBuf> {
        self.path.as_ref().map(|path| {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("codex-meter.sqlite");
            path.with_file_name(format!("{name}.{suffix}"))
        })
    }

    pub async fn upsert_source_jsonl(&self, record: &SourceJsonlRecord) -> Result<bool, DbError> {
        let existed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM source_jsonl WHERE source_key = ?")
                .bind(&record.source_key)
                .fetch_one(&self.pool)
                .await?;
        sqlx::query(
            "INSERT INTO source_jsonl
                (source_key, kind, observed_at_ms, last_seen_at_ms, session_id,
                 root_session_id, turn_id, relation, title, started_at_ms, ended_at_ms,
                 model, service_tier, provider, plan_type, input_tokens,
                 cache_read_tokens, cache_write_tokens, output_tokens, reasoning_tokens,
                 total_tokens, limit_id, window_kind, used_percent, window_minutes,
                 resets_at_ms, quality)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_key) DO UPDATE SET
                last_seen_at_ms = COALESCE(excluded.last_seen_at_ms, source_jsonl.last_seen_at_ms),
                root_session_id = COALESCE(excluded.root_session_id, source_jsonl.root_session_id),
                turn_id = COALESCE(excluded.turn_id, source_jsonl.turn_id),
                relation = COALESCE(excluded.relation, source_jsonl.relation),
                title = COALESCE(excluded.title, source_jsonl.title),
                started_at_ms = COALESCE(excluded.started_at_ms, source_jsonl.started_at_ms),
                ended_at_ms = COALESCE(excluded.ended_at_ms, source_jsonl.ended_at_ms),
                model = COALESCE(excluded.model, source_jsonl.model),
                service_tier = COALESCE(excluded.service_tier, source_jsonl.service_tier),
                provider = COALESCE(excluded.provider, source_jsonl.provider),
                plan_type = COALESCE(excluded.plan_type, source_jsonl.plan_type),
                limit_id = COALESCE(excluded.limit_id, source_jsonl.limit_id),
                window_kind = COALESCE(excluded.window_kind, source_jsonl.window_kind),
                used_percent = COALESCE(excluded.used_percent, source_jsonl.used_percent),
                window_minutes = COALESCE(excluded.window_minutes, source_jsonl.window_minutes),
                resets_at_ms = COALESCE(excluded.resets_at_ms, source_jsonl.resets_at_ms),
                quality = COALESCE(excluded.quality, source_jsonl.quality)",
        )
        .bind(&record.source_key)
        .bind(&record.kind)
        .bind(record.observed_at_ms)
        .bind(record.last_seen_at_ms)
        .bind(&record.session_id)
        .bind(&record.root_session_id)
        .bind(&record.turn_id)
        .bind(&record.relation)
        .bind(&record.title)
        .bind(record.started_at_ms)
        .bind(record.ended_at_ms)
        .bind(&record.model)
        .bind(&record.service_tier)
        .bind(&record.provider)
        .bind(&record.plan_type)
        .bind(record.input_tokens)
        .bind(record.cache_read_tokens)
        .bind(record.cache_write_tokens)
        .bind(record.output_tokens)
        .bind(record.reasoning_tokens)
        .bind(record.total_tokens)
        .bind(&record.limit_id)
        .bind(&record.window_kind)
        .bind(record.used_percent)
        .bind(record.window_minutes)
        .bind(record.resets_at_ms)
        .bind(&record.quality)
        .execute(&self.pool)
        .await?;
        Ok(existed == 0)
    }

    pub async fn upsert_source_jsonl_event(&self, event: &EventRecord) -> Result<bool, DbError> {
        let (kind, source_key) = match event.kind.as_str() {
            "session_meta" => (
                "session",
                event
                    .session_id
                    .as_deref()
                    .map(|value| format!("session:{value}"))
                    .unwrap_or_else(|| format!("session:event:{}", event.source_digest)),
            ),
            "token_count" => ("usage", format!("usage:{}", event.source_digest)),
            _ => return Ok(false),
        };
        let tokens = event.last_tokens.as_ref();
        let record = SourceJsonlRecord {
            source_key,
            kind: kind.to_owned(),
            observed_at_ms: event.observed_at_ms,
            session_id: event.session_id.clone(),
            root_session_id: event.root_session_id.clone(),
            relation: Some(
                if event.root_session_id.is_some() {
                    "fork"
                } else {
                    "main"
                }
                .to_owned(),
            ),
            title: event.title.clone(),
            model: event.model.clone(),
            service_tier: event.tier.clone(),
            provider: event.provider.clone(),
            plan_type: event.plan.clone(),
            input_tokens: tokens.map(|value| value.input),
            cache_read_tokens: tokens.map(|value| value.cached),
            cache_write_tokens: tokens.map(|value| value.cache_write),
            output_tokens: tokens.map(|value| value.output),
            reasoning_tokens: tokens.map(|value| value.reasoning),
            total_tokens: tokens.map(|value| value.total),
            quality: (!event.quality.is_empty()).then(|| event.quality.join(",")),
            ..SourceJsonlRecord::default()
        };
        self.upsert_source_jsonl(&record).await
    }

    pub async fn upsert_source_jsonl_quota(
        &self,
        source_key: &str,
        sample: &QuotaRecord,
    ) -> Result<bool, DbError> {
        self.upsert_source_jsonl(&SourceJsonlRecord {
            source_key: source_key.to_owned(),
            kind: "quota".to_owned(),
            observed_at_ms: sample.observed_at_ms,
            last_seen_at_ms: Some(sample.observed_at_ms),
            limit_id: sample.limit_id.clone(),
            window_kind: Some(sample.window_kind.clone()),
            used_percent: sample.used_percent,
            window_minutes: sample.window_minutes,
            resets_at_ms: sample.resets_at_ms,
            plan_type: sample.plan.clone(),
            quality: Some(format!("source={}", sample.source)),
            ..SourceJsonlRecord::default()
        })
        .await
    }

    pub async fn upsert_source_app_server(
        &self,
        record: &SourceAppServerRecord,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO source_app_server
                (source_key, kind, first_seen_at_ms, last_seen_at_ms, account_key,
                 account_label, auth_kind, provider, plan_type, limit_id, window_kind,
                 used_percent, window_minutes, resets_at_ms, lifetime_tokens,
                 daily_tokens_json, freshness, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_key) DO UPDATE SET
                last_seen_at_ms = excluded.last_seen_at_ms,
                account_key = COALESCE(excluded.account_key, source_app_server.account_key),
                account_label = COALESCE(excluded.account_label, source_app_server.account_label),
                auth_kind = COALESCE(excluded.auth_kind, source_app_server.auth_kind),
                provider = COALESCE(excluded.provider, source_app_server.provider),
                plan_type = COALESCE(excluded.plan_type, source_app_server.plan_type),
                limit_id = COALESCE(excluded.limit_id, source_app_server.limit_id),
                window_kind = COALESCE(excluded.window_kind, source_app_server.window_kind),
                used_percent = COALESCE(excluded.used_percent, source_app_server.used_percent),
                window_minutes = COALESCE(excluded.window_minutes, source_app_server.window_minutes),
                resets_at_ms = COALESCE(excluded.resets_at_ms, source_app_server.resets_at_ms),
                lifetime_tokens = COALESCE(excluded.lifetime_tokens, source_app_server.lifetime_tokens),
                daily_tokens_json = COALESCE(excluded.daily_tokens_json, source_app_server.daily_tokens_json),
                freshness = COALESCE(excluded.freshness, source_app_server.freshness),
                status = excluded.status",
        )
        .bind(&record.source_key)
        .bind(&record.kind)
        .bind(record.first_seen_at_ms)
        .bind(record.last_seen_at_ms)
        .bind(&record.account_key)
        .bind(&record.account_label)
        .bind(&record.auth_kind)
        .bind(&record.provider)
        .bind(&record.plan_type)
        .bind(&record.limit_id)
        .bind(&record.window_kind)
        .bind(record.used_percent)
        .bind(record.window_minutes)
        .bind(record.resets_at_ms)
        .bind(record.lifetime_tokens)
        .bind(&record.daily_tokens_json)
        .bind(&record.freshness)
        .bind(&record.status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_source_ccusage(&self, record: &SourceCcusageRecord) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO source_ccusage
                (source_key, run_at_ms, range_start_ms, range_end_ms, scope, scope_key,
                 pricing_scheme, speed, input_tokens, cache_read_tokens, cache_write_tokens,
                 output_tokens, reasoning_tokens, total_tokens, amount, model_breakdown_json,
                 ccusage_version, pricing_version, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_key) DO UPDATE SET
                run_at_ms = excluded.run_at_ms,
                range_start_ms = excluded.range_start_ms,
                range_end_ms = excluded.range_end_ms,
                input_tokens = excluded.input_tokens,
                cache_read_tokens = excluded.cache_read_tokens,
                cache_write_tokens = excluded.cache_write_tokens,
                output_tokens = excluded.output_tokens,
                reasoning_tokens = excluded.reasoning_tokens,
                total_tokens = excluded.total_tokens,
                amount = excluded.amount,
                model_breakdown_json = excluded.model_breakdown_json,
                ccusage_version = excluded.ccusage_version,
                pricing_version = excluded.pricing_version,
                status = excluded.status",
        )
        .bind(&record.source_key)
        .bind(record.run_at_ms)
        .bind(record.range_start_ms)
        .bind(record.range_end_ms)
        .bind(&record.scope)
        .bind(&record.scope_key)
        .bind(&record.pricing_scheme)
        .bind(&record.speed)
        .bind(record.input_tokens)
        .bind(record.cache_read_tokens)
        .bind(record.cache_write_tokens)
        .bind(record.output_tokens)
        .bind(record.reasoning_tokens)
        .bind(record.total_tokens)
        .bind(record.amount)
        .bind(&record.model_breakdown_json)
        .bind(&record.ccusage_version)
        .bind(&record.pricing_version)
        .bind(&record.status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn close(self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn minimal_schema_has_seven_new_tables() {
        let database = Database::connect_in_memory().await.unwrap();
        for table in MINIMAL_TABLES {
            let exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_one(database.pool())
            .await
            .unwrap();
            assert_eq!(exists, 1, "missing new table {table}");
        }

        for table in [
            "files",
            "events",
            "quota_samples",
            "account_snapshots",
            "validation_runs",
            "capacities",
            "settings",
        ] {
            let legacy_exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_one(database.pool())
            .await
            .unwrap();
            assert_eq!(
                legacy_exists, 0,
                "legacy table {table} must not be initialized"
            );
        }
    }
}
