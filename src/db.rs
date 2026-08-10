use std::{path::Path, time::Duration};

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow},
    SqlitePool,
};
use thiserror::Error;

use crate::config::CapacityDefaults;

const SCHEMA: &str = include_str!("../config/schema.sql");

mod source_jsonl;
pub use source_jsonl::SourceJsonlBatchReport;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

#[derive(Clone, Debug, Default)]
pub struct SourceJsonlRecord {
    pub source_key: String,
    pub kind: String,
    pub observed_at_ms: i64,
    pub last_seen_at_ms: Option<i64>,
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub root_session_id: Option<String>,
    pub turn_id: Option<String>,
    pub relation: Option<String>,
    pub title: Option<String>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub reasoning_effort: Option<String>,
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

#[derive(Clone, Debug, Default)]
pub struct UsageDailyRecord {
    pub local_date: String,
    pub account_key: Option<String>,
    pub auth_kind: Option<String>,
    pub plan_type: Option<String>,
    pub capacity_profile: Option<String>,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub credit: Option<f64>,
    pub api_usd: Option<f64>,
    pub local_percent: Option<f64>,
    pub account_tokens: Option<i64>,
    pub unobserved_tokens: Option<i64>,
    pub coverage_ratio: Option<f64>,
    pub account_token_freshness: Option<String>,
    pub official_percent_start: Option<f64>,
    pub official_percent_end: Option<f64>,
    pub official_percent_delta: Option<f64>,
    pub reset_count: i64,
    pub quality: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct UsageMinuteRecord {
    pub bucket_key: String,
    pub minute_start_ms: i64,
    pub local_date: String,
    pub account_key: Option<String>,
    pub auth_kind: Option<String>,
    pub plan_type: Option<String>,
    pub provider: Option<String>,
    pub capacity_profile: Option<String>,
    pub window_id: Option<String>,
    pub window_kind: Option<String>,
    pub window_start_ms: Option<i64>,
    pub resets_at_ms: Option<i64>,
    pub reset_marker: bool,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub credit: Option<f64>,
    pub api_usd: Option<f64>,
    pub official_used_percent: Option<f64>,
    pub official_source: Option<String>,
    pub quality: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct UsageWindowRecord {
    pub window_id: String,
    pub account_key: Option<String>,
    pub limit_id: Option<String>,
    pub window_kind: String,
    pub window_start_ms: Option<i64>,
    pub resets_at_ms: Option<i64>,
    pub window_minutes: Option<i64>,
    pub auth_kind: Option<String>,
    pub plan_type: Option<String>,
    pub provider: Option<String>,
    pub capacity_profile: Option<String>,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub credit: Option<f64>,
    pub api_usd: Option<f64>,
    pub local_percent: Option<f64>,
    pub account_tokens: Option<i64>,
    pub unobserved_tokens: Option<i64>,
    pub coverage_ratio: Option<f64>,
    pub official_percent_start: Option<f64>,
    pub official_percent_end: Option<f64>,
    pub official_percent_delta: Option<f64>,
    pub quality: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct UsageSessionRecord {
    pub row_key: String,
    pub local_date: String,
    pub root_session_id: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub title: Option<String>,
    pub relation: Option<String>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub window_id: Option<String>,
    pub account_key: Option<String>,
    pub auth_kind: Option<String>,
    pub plan_type: Option<String>,
    pub provider: Option<String>,
    pub capacity_profile: Option<String>,
    pub primary_model: Option<String>,
    pub fast_state: Option<String>,
    pub model_breakdown_json: Option<String>,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub credit: Option<f64>,
    pub api_usd: Option<f64>,
    pub official_percent_start: Option<f64>,
    pub official_percent_end: Option<f64>,
    pub quality: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CapacityRecord {
    pub profile_code: String,
    pub account_key: Option<String>,
    pub plan_type: Option<String>,
    pub weekly_credit: f64,
    pub effective_from_ms: i64,
    pub effective_to_ms: Option<i64>,
    pub confirmed_at_ms: i64,
}

impl Database {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
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
        let database = Self { pool };
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
        let database = Self { pool };
        database.initialize().await?;
        Ok(database)
    }

    async fn initialize(&self) -> Result<(), DbError> {
        let mut indexes = Vec::new();
        for statement in SCHEMA.split(';') {
            let statement = statement.trim();
            if !statement.is_empty() {
                if statement
                    .trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE INDEX")
                {
                    indexes.push(statement);
                } else {
                    sqlx::query(statement).execute(&self.pool).await?;
                }
            }
        }
        // The runtime database is rebuildable but may already contain the
        // baseline created before the source metadata fields were
        // added.  Keep startup idempotent by adding only these two nullable
        // source columns in place; no historical rows are rewritten.
        self.ensure_column("source_jsonl", "parent_session_id", "TEXT")
            .await?;
        self.ensure_column("source_jsonl", "reasoning_effort", "TEXT")
            .await?;
        self.ensure_column("usage_minute", "window_kind", "TEXT")
            .await?;
        for statement in indexes {
            sqlx::query(statement).execute(&self.pool).await?;
        }
        Ok(())
    }

    /// Install the configured baseline capacities once. Databases written by
    /// older builds stored manual values as dated account rows, so the newest
    /// such value is promoted only while the canonical row is still an
    /// untouched seed (`confirmed_at_ms = 0`).
    pub async fn ensure_default_capacities(
        &self,
        defaults: &CapacityDefaults,
    ) -> Result<(), DbError> {
        for (profile_code, weekly_credit) in [
            ("usd20", defaults.usd20),
            ("usd100", defaults.usd100),
            ("usd200", defaults.usd200),
        ] {
            let current: Option<(f64, i64)> = sqlx::query_as(
                "SELECT weekly_credit, confirmed_at_ms FROM capacities
                 WHERE profile_code = ?
                   AND account_key IS NULL
                   AND plan_type IS NULL
                   AND effective_from_ms = 0
                 LIMIT 1",
            )
            .bind(profile_code)
            .fetch_optional(&self.pool)
            .await?;

            let legacy: Option<(f64, i64)> = sqlx::query_as(
                "SELECT weekly_credit, confirmed_at_ms FROM capacities
                 WHERE profile_code = ?
                   AND NOT (
                       account_key IS NULL
                       AND plan_type IS NULL
                       AND effective_from_ms = 0
                   )
                 ORDER BY confirmed_at_ms DESC, effective_from_ms DESC, id DESC
                 LIMIT 1",
            )
            .bind(profile_code)
            .fetch_optional(&self.pool)
            .await?;

            if let Some((_, confirmed_at_ms)) = current {
                if confirmed_at_ms == 0 {
                    if let Some((legacy_credit, legacy_confirmed_at_ms)) = legacy {
                        self.set_current_capacity(
                            profile_code,
                            legacy_credit,
                            legacy_confirmed_at_ms,
                        )
                        .await?;
                    }
                }
                continue;
            }
            let (weekly_credit, confirmed_at_ms) = legacy.unwrap_or((weekly_credit, 0));
            sqlx::query(
                "INSERT INTO capacities
                    (profile_code, account_key, plan_type, weekly_credit,
                     effective_from_ms, effective_to_ms, confirmed_at_ms)
                 VALUES (?, NULL, NULL, ?, 0, NULL, ?)",
            )
            .bind(profile_code)
            .bind(weekly_credit)
            .bind(confirmed_at_ms)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    async fn ensure_column(
        &self,
        table: &str,
        column: &str,
        definition: &str,
    ) -> Result<(), DbError> {
        let exists: Option<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?) WHERE name = ? LIMIT 1")
                .bind(table)
                .bind(column)
                .fetch_optional(&self.pool)
                .await?;
        if exists.is_none() {
            // table/column are fixed call-site literals, not user input.
            let statement = format!("ALTER TABLE {table} ADD COLUMN {column} {definition}");
            sqlx::query(&statement).execute(&self.pool).await?;
        }
        Ok(())
    }

    pub async fn close(self) {
        self.pool.close().await;
    }

    pub async fn table_count(&self) -> Result<i64, DbError> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name IN
             ('source_jsonl','source_app_server','source_ccusage',
             'usage_daily','usage_minute','usage_window','usage_session','capacities')",
        )
        .fetch_one(&self.pool)
        .await?)
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
                 parent_session_id, root_session_id, turn_id, relation, title,
                 started_at_ms, ended_at_ms, model, service_tier, reasoning_effort,
                 provider, plan_type, input_tokens,
                 cache_read_tokens, cache_write_tokens, output_tokens, reasoning_tokens,
                 total_tokens, limit_id, window_kind, used_percent, window_minutes,
                 resets_at_ms, quality)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(source_key) DO UPDATE SET
                observed_at_ms = CASE
                    WHEN source_jsonl.kind = 'quota'
                    THEN MIN(source_jsonl.observed_at_ms, excluded.observed_at_ms)
                    ELSE source_jsonl.observed_at_ms
                END,
                last_seen_at_ms = CASE
                    WHEN source_jsonl.kind = 'quota'
                    THEN MAX(
                        COALESCE(source_jsonl.last_seen_at_ms, source_jsonl.observed_at_ms),
                        COALESCE(excluded.last_seen_at_ms, excluded.observed_at_ms)
                    )
                    ELSE COALESCE(excluded.last_seen_at_ms, source_jsonl.last_seen_at_ms)
                END,
                parent_session_id = COALESCE(excluded.parent_session_id, source_jsonl.parent_session_id),
                root_session_id = COALESCE(excluded.root_session_id, source_jsonl.root_session_id),
                turn_id = COALESCE(excluded.turn_id, source_jsonl.turn_id),
                relation = COALESCE(excluded.relation, source_jsonl.relation),
                title = COALESCE(excluded.title, source_jsonl.title),
                started_at_ms = COALESCE(excluded.started_at_ms, source_jsonl.started_at_ms),
                ended_at_ms = COALESCE(excluded.ended_at_ms, source_jsonl.ended_at_ms),
                model = COALESCE(excluded.model, source_jsonl.model),
                service_tier = COALESCE(excluded.service_tier, source_jsonl.service_tier),
                reasoning_effort = COALESCE(excluded.reasoning_effort, source_jsonl.reasoning_effort),
                provider = COALESCE(excluded.provider, source_jsonl.provider),
                plan_type = COALESCE(excluded.plan_type, source_jsonl.plan_type),
                input_tokens = COALESCE(excluded.input_tokens, source_jsonl.input_tokens),
                cache_read_tokens = COALESCE(excluded.cache_read_tokens, source_jsonl.cache_read_tokens),
                cache_write_tokens = COALESCE(excluded.cache_write_tokens, source_jsonl.cache_write_tokens),
                output_tokens = COALESCE(excluded.output_tokens, source_jsonl.output_tokens),
                reasoning_tokens = COALESCE(excluded.reasoning_tokens, source_jsonl.reasoning_tokens),
                total_tokens = COALESCE(excluded.total_tokens, source_jsonl.total_tokens),
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
        .bind(&record.parent_session_id)
        .bind(&record.root_session_id)
        .bind(&record.turn_id)
        .bind(&record.relation)
        .bind(&record.title)
        .bind(record.started_at_ms)
        .bind(record.ended_at_ms)
        .bind(&record.model)
        .bind(&record.service_tier)
        .bind(&record.reasoning_effort)
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

    pub async fn replace_rollups(
        &self,
        daily: &[UsageDailyRecord],
        minute: &[UsageMinuteRecord],
        windows: &[UsageWindowRecord],
        sessions: &[UsageSessionRecord],
    ) -> Result<(), DbError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM usage_daily")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM usage_minute")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM usage_window")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM usage_session")
            .execute(&mut *transaction)
            .await?;

        for row in daily {
            sqlx::query(
                "INSERT INTO usage_daily
                 (local_date, account_key, auth_kind, plan_type, capacity_profile,
                  input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                  reasoning_tokens, total_tokens, credit, api_usd, local_percent,
                  account_tokens, unobserved_tokens, coverage_ratio,
                  account_token_freshness, official_percent_start, official_percent_end,
                  official_percent_delta, reset_count, quality)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.local_date)
            .bind(&row.account_key)
            .bind(&row.auth_kind)
            .bind(&row.plan_type)
            .bind(&row.capacity_profile)
            .bind(row.input_tokens)
            .bind(row.cache_read_tokens)
            .bind(row.cache_write_tokens)
            .bind(row.output_tokens)
            .bind(row.reasoning_tokens)
            .bind(row.total_tokens)
            .bind(row.credit)
            .bind(row.api_usd)
            .bind(row.local_percent)
            .bind(row.account_tokens)
            .bind(row.unobserved_tokens)
            .bind(row.coverage_ratio)
            .bind(&row.account_token_freshness)
            .bind(row.official_percent_start)
            .bind(row.official_percent_end)
            .bind(row.official_percent_delta)
            .bind(row.reset_count)
            .bind(&row.quality)
            .execute(&mut *transaction)
            .await?;
        }

        for row in minute {
            sqlx::query(
                "INSERT INTO usage_minute
                 (bucket_key, minute_start_ms, local_date, account_key, auth_kind,
                 plan_type, provider, capacity_profile, window_id, window_start_ms,
                  window_kind, resets_at_ms, reset_marker, input_tokens, cache_read_tokens,
                  cache_write_tokens, output_tokens, reasoning_tokens, total_tokens,
                  credit, api_usd, official_used_percent, official_source, quality)
                 VALUES (
                    ?, ?, ?, ?, ?, ?, ?, ?,
                    ?, ?, ?, ?, ?, ?, ?, ?,
                    ?, ?, ?, ?, ?, ?, ?, ?
                 )",
            )
            .bind(&row.bucket_key)
            .bind(row.minute_start_ms)
            .bind(&row.local_date)
            .bind(&row.account_key)
            .bind(&row.auth_kind)
            .bind(&row.plan_type)
            .bind(&row.provider)
            .bind(&row.capacity_profile)
            .bind(&row.window_id)
            .bind(row.window_start_ms)
            .bind(&row.window_kind)
            .bind(row.resets_at_ms)
            .bind(i64::from(row.reset_marker))
            .bind(row.input_tokens)
            .bind(row.cache_read_tokens)
            .bind(row.cache_write_tokens)
            .bind(row.output_tokens)
            .bind(row.reasoning_tokens)
            .bind(row.total_tokens)
            .bind(row.credit)
            .bind(row.api_usd)
            .bind(row.official_used_percent)
            .bind(&row.official_source)
            .bind(&row.quality)
            .execute(&mut *transaction)
            .await?;
        }

        for row in windows {
            sqlx::query(
                "INSERT INTO usage_window
                 (window_id, account_key, limit_id, window_kind, window_start_ms,
                  resets_at_ms, window_minutes, auth_kind, plan_type, provider,
                  capacity_profile, input_tokens, cache_read_tokens, cache_write_tokens,
                  output_tokens, reasoning_tokens, total_tokens, credit, api_usd,
                  local_percent, account_tokens, unobserved_tokens, coverage_ratio,
                  official_percent_start, official_percent_end, official_percent_delta,
                  quality)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.window_id)
            .bind(&row.account_key)
            .bind(&row.limit_id)
            .bind(&row.window_kind)
            .bind(row.window_start_ms)
            .bind(row.resets_at_ms)
            .bind(row.window_minutes)
            .bind(&row.auth_kind)
            .bind(&row.plan_type)
            .bind(&row.provider)
            .bind(&row.capacity_profile)
            .bind(row.input_tokens)
            .bind(row.cache_read_tokens)
            .bind(row.cache_write_tokens)
            .bind(row.output_tokens)
            .bind(row.reasoning_tokens)
            .bind(row.total_tokens)
            .bind(row.credit)
            .bind(row.api_usd)
            .bind(row.local_percent)
            .bind(row.account_tokens)
            .bind(row.unobserved_tokens)
            .bind(row.coverage_ratio)
            .bind(row.official_percent_start)
            .bind(row.official_percent_end)
            .bind(row.official_percent_delta)
            .bind(&row.quality)
            .execute(&mut *transaction)
            .await?;
        }

        for row in sessions {
            sqlx::query(
                "INSERT INTO usage_session
                 (row_key, local_date, root_session_id, session_id, turn_id, title,
                  relation, started_at_ms, ended_at_ms, window_id, account_key,
                  auth_kind, plan_type, provider, capacity_profile, primary_model,
                  fast_state, model_breakdown_json, input_tokens, cache_read_tokens,
                  cache_write_tokens, output_tokens, reasoning_tokens, total_tokens,
                  credit, api_usd, official_percent_start, official_percent_end, quality)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.row_key)
            .bind(&row.local_date)
            .bind(&row.root_session_id)
            .bind(&row.session_id)
            .bind(&row.turn_id)
            .bind(&row.title)
            .bind(&row.relation)
            .bind(row.started_at_ms)
            .bind(row.ended_at_ms)
            .bind(&row.window_id)
            .bind(&row.account_key)
            .bind(&row.auth_kind)
            .bind(&row.plan_type)
            .bind(&row.provider)
            .bind(&row.capacity_profile)
            .bind(&row.primary_model)
            .bind(&row.fast_state)
            .bind(&row.model_breakdown_json)
            .bind(row.input_tokens)
            .bind(row.cache_read_tokens)
            .bind(row.cache_write_tokens)
            .bind(row.output_tokens)
            .bind(row.reasoning_tokens)
            .bind(row.total_tokens)
            .bind(row.credit)
            .bind(row.api_usd)
            .bind(row.official_percent_start)
            .bind(row.official_percent_end)
            .bind(&row.quality)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(())
    }

    pub async fn upsert_capacity(&self, record: &CapacityRecord) -> Result<i64, DbError> {
        let result = sqlx::query(
            "INSERT INTO capacities
                (profile_code, account_key, plan_type, weekly_credit,
                 effective_from_ms, effective_to_ms, confirmed_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(profile_code, account_key, effective_from_ms) DO UPDATE SET
                plan_type = excluded.plan_type,
                weekly_credit = excluded.weekly_credit,
                effective_to_ms = excluded.effective_to_ms,
                confirmed_at_ms = excluded.confirmed_at_ms",
        )
        .bind(&record.profile_code)
        .bind(&record.account_key)
        .bind(&record.plan_type)
        .bind(record.weekly_credit)
        .bind(record.effective_from_ms)
        .bind(record.effective_to_ms)
        .bind(record.confirmed_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn set_current_capacity(
        &self,
        profile_code: &str,
        weekly_credit: f64,
        confirmed_at_ms: i64,
    ) -> Result<(), DbError> {
        let result = sqlx::query(
            "UPDATE capacities
             SET weekly_credit = ?, confirmed_at_ms = ?, effective_to_ms = NULL
             WHERE profile_code = ?
               AND account_key IS NULL
               AND plan_type IS NULL
               AND effective_from_ms = 0",
        )
        .bind(weekly_credit)
        .bind(confirmed_at_ms)
        .bind(profile_code)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            sqlx::query(
                "INSERT INTO capacities
                    (profile_code, account_key, plan_type, weekly_credit,
                     effective_from_ms, effective_to_ms, confirmed_at_ms)
                 VALUES (?, NULL, NULL, ?, 0, NULL, ?)",
            )
            .bind(profile_code)
            .bind(weekly_credit)
            .bind(confirmed_at_ms)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn list_source_jsonl(&self) -> Result<Vec<SqliteRow>, DbError> {
        Ok(
            sqlx::query("SELECT * FROM source_jsonl ORDER BY observed_at_ms, id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn update_source_jsonl_root(
        &self,
        source_key: &str,
        root_session_id: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE source_jsonl
             SET root_session_id = ?
             WHERE source_key = ?",
        )
        .bind(root_session_id)
        .bind(source_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_source_app_server(&self) -> Result<Vec<SqliteRow>, DbError> {
        Ok(
            sqlx::query("SELECT * FROM source_app_server ORDER BY last_seen_at_ms, id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn list_source_ccusage(&self) -> Result<Vec<SqliteRow>, DbError> {
        Ok(
            sqlx::query("SELECT * FROM source_ccusage ORDER BY run_at_ms, id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn list_usage_daily(&self) -> Result<Vec<SqliteRow>, DbError> {
        Ok(sqlx::query("SELECT * FROM usage_daily ORDER BY local_date")
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn list_usage_minute(&self) -> Result<Vec<SqliteRow>, DbError> {
        Ok(
            sqlx::query("SELECT * FROM usage_minute ORDER BY minute_start_ms, id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn list_usage_window(&self) -> Result<Vec<SqliteRow>, DbError> {
        Ok(
            sqlx::query("SELECT * FROM usage_window ORDER BY window_start_ms, window_id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn list_usage_session(&self) -> Result<Vec<SqliteRow>, DbError> {
        Ok(
            sqlx::query("SELECT * FROM usage_session ORDER BY local_date, started_at_ms, id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn list_capacities(&self) -> Result<Vec<SqliteRow>, DbError> {
        Ok(
            sqlx::query("SELECT * FROM capacities ORDER BY effective_from_ms DESC, profile_code")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn list_current_capacities(&self) -> Result<Vec<SqliteRow>, DbError> {
        Ok(sqlx::query(
            "SELECT * FROM capacities
             WHERE account_key IS NULL
               AND plan_type IS NULL
               AND effective_from_ms = 0
             ORDER BY profile_code",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn list_all_tables(&self) -> Result<Vec<SqliteRow>, DbError> {
        Ok(sqlx::query(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use sqlx::Row;

    use super::*;

    #[tokio::test]
    async fn schema_contains_only_the_eight_target_tables() {
        let database = Database::connect_in_memory().await.unwrap();
        let names = database
            .list_all_tables()
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String, _>("name").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "capacities",
                "source_app_server",
                "source_ccusage",
                "source_jsonl",
                "usage_daily",
                "usage_minute",
                "usage_session",
                "usage_window",
            ]
        );
        assert_eq!(database.table_count().await.unwrap(), 8);
    }

    #[tokio::test]
    async fn seeds_default_capacities_idempotently() {
        let database = Database::connect_in_memory().await.unwrap();
        let defaults = CapacityDefaults::default();

        database
            .ensure_default_capacities(&defaults)
            .await
            .unwrap();
        database
            .ensure_default_capacities(&defaults)
            .await
            .unwrap();

        let rows = database.list_capacities().await.unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter()
                .find(|row| row.try_get::<String, _>("profile_code").unwrap() == "usd20")
                .unwrap()
                .try_get::<f64, _>("weekly_credit")
                .unwrap(),
            defaults.usd20
        );
    }

    #[tokio::test]
    async fn current_capacity_overwrites_the_seed_without_using_history_rows() {
        let database = Database::connect_in_memory().await.unwrap();
        database
            .ensure_default_capacities(&CapacityDefaults::default())
            .await
            .unwrap();
        database
            .upsert_capacity(&CapacityRecord {
                profile_code: "usd100".to_owned(),
                account_key: Some("old-account".to_owned()),
                plan_type: Some("pro".to_owned()),
                weekly_credit: 13_799.83,
                effective_from_ms: 99,
                confirmed_at_ms: 99,
                ..Default::default()
            })
            .await
            .unwrap();

        database
            .set_current_capacity("usd100", 13_800.0, 100)
            .await
            .unwrap();

        let rows = database.list_current_capacities().await.unwrap();
        assert_eq!(rows.len(), 3);
        let usd100 = rows
            .iter()
            .find(|row| row.try_get::<String, _>("profile_code").unwrap() == "usd100")
            .unwrap();
        assert_eq!(usd100.try_get::<f64, _>("weekly_credit").unwrap(), 13_800.0);
        assert_eq!(usd100.try_get::<i64, _>("effective_from_ms").unwrap(), 0);
        assert_eq!(database.list_capacities().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn legacy_manual_capacity_is_promoted_once() {
        let database = Database::connect_in_memory().await.unwrap();
        database
            .upsert_capacity(&CapacityRecord {
                profile_code: "usd200".to_owned(),
                account_key: Some("old-account".to_owned()),
                plan_type: Some("pro".to_owned()),
                weekly_credit: 55_000.0,
                effective_from_ms: 99,
                confirmed_at_ms: 99,
                ..Default::default()
            })
            .await
            .unwrap();

        database
            .ensure_default_capacities(&CapacityDefaults::default())
            .await
            .unwrap();
        let current = database.list_current_capacities().await.unwrap();
        let usd200 = current
            .iter()
            .find(|row| row.try_get::<String, _>("profile_code").unwrap() == "usd200")
            .unwrap();
        assert_eq!(usd200.try_get::<f64, _>("weekly_credit").unwrap(), 55_000.0);

        database
            .set_current_capacity("usd200", 60_000.0, 100)
            .await
            .unwrap();
        database
            .ensure_default_capacities(&CapacityDefaults::default())
            .await
            .unwrap();
        let current = database.list_current_capacities().await.unwrap();
        let usd200 = current
            .iter()
            .find(|row| row.try_get::<String, _>("profile_code").unwrap() == "usd200")
            .unwrap();
        assert_eq!(usd200.try_get::<f64, _>("weekly_credit").unwrap(), 60_000.0);
    }

    #[tokio::test]
    async fn source_and_derived_tables_round_trip() {
        let database = Database::connect_in_memory().await.unwrap();

        let mut jsonl = SourceJsonlRecord {
            source_key: "usage:1".to_owned(),
            kind: "usage".to_owned(),
            observed_at_ms: 1,
            ..Default::default()
        };
        jsonl.total_tokens = Some(10);
        assert!(database.upsert_source_jsonl(&jsonl).await.unwrap());
        assert!(!database.upsert_source_jsonl(&jsonl).await.unwrap());

        let app_server = SourceAppServerRecord {
            source_key: "account:1".to_owned(),
            kind: "account".to_owned(),
            first_seen_at_ms: 1,
            last_seen_at_ms: 1,
            status: "ok".to_owned(),
            ..Default::default()
        };
        database
            .upsert_source_app_server(&app_server)
            .await
            .unwrap();

        let ccusage = SourceCcusageRecord {
            source_key: "daily:1".to_owned(),
            run_at_ms: 1,
            range_start_ms: 1,
            range_end_ms: 2,
            scope: "daily".to_owned(),
            scope_key: "2026-08-07".to_owned(),
            pricing_scheme: "subscription".to_owned(),
            speed: "auto".to_owned(),
            status: "ok".to_owned(),
            ..Default::default()
        };
        database.upsert_source_ccusage(&ccusage).await.unwrap();

        let daily = UsageDailyRecord {
            local_date: "2026-08-07".to_owned(),
            total_tokens: Some(10),
            ..Default::default()
        };
        let minute = UsageMinuteRecord {
            bucket_key: "minute:1".to_owned(),
            minute_start_ms: 1,
            local_date: "2026-08-07".to_owned(),
            ..Default::default()
        };
        let session = UsageSessionRecord {
            row_key: "turn:1".to_owned(),
            local_date: "2026-08-07".to_owned(),
            ..Default::default()
        };
        database
            .replace_rollups(&[daily], &[minute], &[], &[session])
            .await
            .unwrap();

        let capacity = CapacityRecord {
            profile_code: "usd20".to_owned(),
            weekly_credit: 100.0,
            effective_from_ms: 1,
            confirmed_at_ms: 1,
            ..Default::default()
        };
        database.upsert_capacity(&capacity).await.unwrap();

        assert_eq!(database.list_source_jsonl().await.unwrap().len(), 1);
        assert_eq!(database.list_source_app_server().await.unwrap().len(), 1);
        assert_eq!(database.list_source_ccusage().await.unwrap().len(), 1);
        assert_eq!(database.list_usage_daily().await.unwrap().len(), 1);
        assert_eq!(database.list_usage_minute().await.unwrap().len(), 1);
        assert!(database.list_usage_window().await.unwrap().is_empty());
        assert_eq!(database.list_usage_session().await.unwrap().len(), 1);
        assert_eq!(database.list_capacities().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn jsonl_quota_upsert_preserves_first_seen_and_updates_last_seen() {
        let database = Database::connect_in_memory().await.unwrap();
        let quota = SourceJsonlRecord {
            source_key: "quota:stable".to_owned(),
            kind: "quota".to_owned(),
            observed_at_ms: 1_000,
            last_seen_at_ms: Some(1_000),
            limit_id: Some("weekly".to_owned()),
            window_kind: Some("primary".to_owned()),
            used_percent: Some(12.0),
            ..Default::default()
        };
        assert!(database.upsert_source_jsonl(&quota).await.unwrap());
        let mut second = quota.clone();
        second.observed_at_ms = 2_000;
        second.last_seen_at_ms = Some(2_000);
        assert!(!database.upsert_source_jsonl(&second).await.unwrap());
        let row = database.list_source_jsonl().await.unwrap().pop().unwrap();
        assert_eq!(row.try_get::<i64, _>("observed_at_ms").unwrap(), 1_000);
        assert_eq!(
            row.try_get::<Option<i64>, _>("last_seen_at_ms").unwrap(),
            Some(2_000)
        );
    }

    #[tokio::test]
    async fn existing_source_baseline_gets_new_nullable_columns_before_indexes() {
        let path =
            std::env::temp_dir().join(format!("codex-meter-schema-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE source_jsonl (
                id INTEGER PRIMARY KEY, source_key TEXT UNIQUE NOT NULL,
                kind TEXT NOT NULL, observed_at_ms INTEGER NOT NULL,
                last_seen_at_ms INTEGER, session_id TEXT, root_session_id TEXT,
                turn_id TEXT, relation TEXT, title TEXT, started_at_ms INTEGER,
                ended_at_ms INTEGER, model TEXT, service_tier TEXT, provider TEXT,
                plan_type TEXT, input_tokens INTEGER, cache_read_tokens INTEGER,
                cache_write_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER,
                total_tokens INTEGER, limit_id TEXT, window_kind TEXT,
                used_percent REAL, window_minutes INTEGER, resets_at_ms INTEGER, quality TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let database = Database::connect(&path).await.unwrap();
        let columns = sqlx::query("SELECT name FROM pragma_table_info('source_jsonl')")
            .fetch_all(&database.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String, _>("name").unwrap())
            .collect::<Vec<_>>();
        assert!(columns.iter().any(|name| name == "parent_session_id"));
        assert!(columns.iter().any(|name| name == "reasoning_effort"));
        database.close().await;
        let _ = std::fs::remove_file(path);
    }
}
