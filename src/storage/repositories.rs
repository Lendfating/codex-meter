use crate::domain::{
    AccountContextInterval, AccountIdentity, AccountUsageSnapshot, CalibrationSegment,
    CcusageSessionSnapshot, CollectorRun, DailyRollup, Machine, ManualAnnotation, PlanCapacity,
    PricingVersion, QuotaSnapshot, TokenObservation, UsageDelta,
};

use super::{Database, StorageError};

fn bool_to_i64(value: Option<bool>) -> Option<i64> {
    value.map(|value| if value { 1 } else { 0 })
}

impl Database {
    pub async fn insert_machine(&self, machine: &Machine) -> Result<i64, StorageError> {
        let result = sqlx::query(
            "INSERT INTO machines (name, install_id, timezone, created_at_ms) VALUES (?, ?, ?, ?)",
        )
        .bind(&machine.name)
        .bind(&machine.install_id)
        .bind(&machine.timezone)
        .bind(machine.created_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn insert_account_identity(
        &self,
        identity: &AccountIdentity,
    ) -> Result<i64, StorageError> {
        let result = sqlx::query(
            "INSERT INTO account_identities
             (machine_id, kind, email_masked, identity_hmac, label, created_at_ms)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(identity.machine_id)
        .bind(identity.kind.as_str())
        .bind(identity.email_masked.as_deref())
        .bind(&identity.identity_hmac)
        .bind(identity.label.as_deref())
        .bind(identity.created_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn insert_account_context_interval(
        &self,
        interval: &AccountContextInterval,
    ) -> Result<i64, StorageError> {
        let result = sqlx::query(
            "INSERT INTO account_context_intervals
             (machine_id, account_identity_id, start_at_ms, end_at_ms, auth_kind,
              plan_type_raw, display_group, capacity_profile, provider_name, endpoint_hmac,
              classification_source, source, quality, pricing_version, collector_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(interval.machine_id)
        .bind(interval.account_identity_id)
        .bind(interval.start_at_ms)
        .bind(interval.end_at_ms)
        .bind(interval.auth_kind.as_str())
        .bind(interval.plan_type_raw.as_deref())
        .bind(interval.display_group.as_str())
        .bind(interval.capacity_profile.map(|profile| profile.as_str()))
        .bind(interval.provider_name.as_deref())
        .bind(interval.endpoint_hmac.as_deref())
        .bind(interval.classification_source.as_str())
        .bind(&interval.metadata.source)
        .bind(interval.metadata.quality.as_str())
        .bind(interval.metadata.pricing_version.as_deref())
        .bind(&interval.metadata.collector_version)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn insert_token_observation(
        &self,
        observation: &TokenObservation,
    ) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "INSERT INTO token_observations
             (machine_id, context_interval_id, session_id, turn_id, observed_at_ms,
              input_tokens, cached_input_tokens, cache_write_input_tokens, output_tokens,
              reasoning_output_tokens, total_tokens, model, service_tier, source_digest,
              collector_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (machine_id, source_digest) DO NOTHING",
        )
        .bind(observation.machine_id)
        .bind(observation.context_interval_id)
        .bind(&observation.session_id)
        .bind(observation.turn_id.as_deref())
        .bind(observation.observed_at_ms)
        .bind(observation.tokens.input_tokens)
        .bind(observation.tokens.cached_input_tokens)
        .bind(observation.tokens.cache_write_input_tokens)
        .bind(observation.tokens.output_tokens)
        .bind(observation.tokens.reasoning_output_tokens)
        .bind(observation.tokens.total_tokens)
        .bind(observation.model.as_deref())
        .bind(observation.service_tier.as_deref())
        .bind(&observation.source_digest)
        .bind(&observation.collector_version)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn count_token_observations(&self) -> Result<i64, StorageError> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM token_observations")
                .fetch_one(&self.pool)
                .await?,
        )
    }

    pub async fn insert_quota_snapshot(
        &self,
        snapshot: &QuotaSnapshot,
    ) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "INSERT INTO quota_snapshots
             (machine_id, account_identity_id, observed_at_ms, limit_id, limit_name,
              used_percent, window_minutes, resets_at_ms, plan_type_raw, credits_has_credits,
              credits_unlimited, credits_balance, source, quality, pricing_version,
              collector_version, source_digest)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (machine_id, source_digest) DO NOTHING",
        )
        .bind(snapshot.machine_id)
        .bind(snapshot.account_identity_id)
        .bind(snapshot.observed_at_ms)
        .bind(&snapshot.limit_id)
        .bind(snapshot.limit_name.as_deref())
        .bind(snapshot.used_percent)
        .bind(snapshot.window_minutes)
        .bind(snapshot.resets_at_ms)
        .bind(snapshot.plan_type_raw.as_deref())
        .bind(bool_to_i64(snapshot.credits_has_credits))
        .bind(bool_to_i64(snapshot.credits_unlimited))
        .bind(snapshot.credits_balance.as_deref())
        .bind(&snapshot.metadata.source)
        .bind(snapshot.metadata.quality.as_str())
        .bind(snapshot.metadata.pricing_version.as_deref())
        .bind(&snapshot.metadata.collector_version)
        .bind(&snapshot.source_digest)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn insert_account_usage_snapshot(
        &self,
        snapshot: &AccountUsageSnapshot,
    ) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "INSERT INTO account_usage_snapshots
             (machine_id, account_identity_id, observed_at_ms, lifetime_tokens,
              daily_buckets_json, source, quality, pricing_version, collector_version,
              source_digest)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (machine_id, source_digest) DO NOTHING",
        )
        .bind(snapshot.machine_id)
        .bind(snapshot.account_identity_id)
        .bind(snapshot.observed_at_ms)
        .bind(snapshot.lifetime_tokens)
        .bind(serde_json::to_string(&snapshot.daily_buckets)?)
        .bind(&snapshot.metadata.source)
        .bind(snapshot.metadata.quality.as_str())
        .bind(snapshot.metadata.pricing_version.as_deref())
        .bind(&snapshot.metadata.collector_version)
        .bind(&snapshot.source_digest)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn insert_ccusage_session_snapshot(
        &self,
        snapshot: &CcusageSessionSnapshot,
    ) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "INSERT INTO ccusage_session_snapshots
             (machine_id, session_id, observed_at_ms, model_tokens_json, pricing_scheme,
              auto_amount, standard_amount, pricing_version, ccusage_version,
              command_duration_ms, result_hash, source_digest, source, quality,
              collector_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (machine_id, source_digest) DO NOTHING",
        )
        .bind(snapshot.machine_id)
        .bind(&snapshot.session_id)
        .bind(snapshot.observed_at_ms)
        .bind(serde_json::to_string(&snapshot.model_tokens)?)
        .bind(snapshot.pricing_scheme.as_str())
        .bind(snapshot.auto_amount)
        .bind(snapshot.standard_amount)
        .bind(&snapshot.pricing_version)
        .bind(&snapshot.ccusage_version)
        .bind(snapshot.command_duration_ms)
        .bind(&snapshot.result_hash)
        .bind(&snapshot.source_digest)
        .bind(&snapshot.source)
        .bind(snapshot.quality.as_str())
        .bind(&snapshot.collector_version)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn insert_usage_delta(&self, delta: &UsageDelta) -> Result<bool, StorageError> {
        let result = sqlx::query(
            "INSERT INTO usage_deltas
             (machine_id, account_context_interval_id, session_id, start_at_ms, end_at_ms,
              input_tokens, cached_input_tokens, cache_write_input_tokens, output_tokens,
              reasoning_output_tokens, total_tokens, subscription_base_credit,
              subscription_fast_surcharge, subscription_total_credit, api_base_usd,
              api_fast_surcharge_usd, api_total_usd, source, quality, pricing_version,
              collector_version, dedupe_key)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (dedupe_key) DO NOTHING",
        )
        .bind(delta.machine_id)
        .bind(delta.account_context_interval_id)
        .bind(delta.session_id.as_deref())
        .bind(delta.start_at_ms)
        .bind(delta.end_at_ms)
        .bind(delta.tokens.input_tokens)
        .bind(delta.tokens.cached_input_tokens)
        .bind(delta.tokens.cache_write_input_tokens)
        .bind(delta.tokens.output_tokens)
        .bind(delta.tokens.reasoning_output_tokens)
        .bind(delta.tokens.total_tokens)
        .bind(delta.subscription_base_credit)
        .bind(delta.subscription_fast_surcharge)
        .bind(delta.subscription_total_credit)
        .bind(delta.api_base_usd)
        .bind(delta.api_fast_surcharge_usd)
        .bind(delta.api_total_usd)
        .bind(&delta.metadata.source)
        .bind(delta.metadata.quality.as_str())
        .bind(delta.metadata.pricing_version.as_deref())
        .bind(&delta.metadata.collector_version)
        .bind(&delta.dedupe_key)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn insert_daily_rollup(&self, rollup: &DailyRollup) -> Result<i64, StorageError> {
        let result = sqlx::query(
            "INSERT INTO daily_rollups
             (machine_id, local_date, account_identity_id, input_tokens, cached_input_tokens,
              cache_write_input_tokens, output_tokens, reasoning_output_tokens, total_tokens,
              subscription_credit, api_usd, source, quality, pricing_version, collector_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (machine_id, local_date, account_identity_id) DO UPDATE SET
              input_tokens = excluded.input_tokens,
              cached_input_tokens = excluded.cached_input_tokens,
              cache_write_input_tokens = excluded.cache_write_input_tokens,
              output_tokens = excluded.output_tokens,
              reasoning_output_tokens = excluded.reasoning_output_tokens,
              total_tokens = excluded.total_tokens,
              subscription_credit = excluded.subscription_credit,
              api_usd = excluded.api_usd,
              source = excluded.source,
              quality = excluded.quality,
              pricing_version = excluded.pricing_version,
              collector_version = excluded.collector_version",
        )
        .bind(rollup.machine_id)
        .bind(&rollup.local_date)
        .bind(rollup.account_identity_id)
        .bind(rollup.tokens.input_tokens)
        .bind(rollup.tokens.cached_input_tokens)
        .bind(rollup.tokens.cache_write_input_tokens)
        .bind(rollup.tokens.output_tokens)
        .bind(rollup.tokens.reasoning_output_tokens)
        .bind(rollup.tokens.total_tokens)
        .bind(rollup.subscription_credit)
        .bind(rollup.api_usd)
        .bind(&rollup.metadata.source)
        .bind(rollup.metadata.quality.as_str())
        .bind(rollup.metadata.pricing_version.as_deref())
        .bind(&rollup.metadata.collector_version)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn insert_pricing_version(
        &self,
        version: &PricingVersion,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO pricing_versions
             (id, scheme, effective_at_ms, timezone, rates_json, fast_multipliers_json,
              source_url, source_precision, created_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&version.id)
        .bind(version.scheme.as_str())
        .bind(version.effective_at_ms)
        .bind(&version.timezone)
        .bind(serde_json::to_string(&version.rates)?)
        .bind(serde_json::to_string(&version.fast_multipliers)?)
        .bind(version.source_url.as_deref())
        .bind(&version.source_precision)
        .bind(version.created_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_plan_capacity(&self, capacity: &PlanCapacity) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO plan_capacities
             (id, machine_id, plan_code, effective_from_ms, effective_to_ms,
              confirmed_credit, status, note, created_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&capacity.id)
        .bind(capacity.machine_id)
        .bind(capacity.plan_code.as_str())
        .bind(capacity.effective_from_ms)
        .bind(capacity.effective_to_ms)
        .bind(capacity.confirmed_credit)
        .bind(capacity.status.as_str())
        .bind(capacity.note.as_deref())
        .bind(capacity.created_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_calibration_segment(
        &self,
        segment: &CalibrationSegment,
    ) -> Result<i64, StorageError> {
        let result = sqlx::query(
            "INSERT INTO calibration_segments
             (machine_id, account_identity_id, start_at_ms, end_at_ms, window_kind,
              used_percent_start, used_percent_end, local_credit, candidate_capacity,
              sample_count, contamination, adopted, source, quality, pricing_version,
              collector_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(segment.machine_id)
        .bind(segment.account_identity_id)
        .bind(segment.start_at_ms)
        .bind(segment.end_at_ms)
        .bind(&segment.window_kind)
        .bind(segment.used_percent_start)
        .bind(segment.used_percent_end)
        .bind(segment.local_credit)
        .bind(segment.candidate_capacity)
        .bind(segment.sample_count)
        .bind(segment.contamination.as_deref())
        .bind(i64::from(segment.adopted))
        .bind(&segment.metadata.source)
        .bind(segment.metadata.quality.as_str())
        .bind(segment.metadata.pricing_version.as_deref())
        .bind(&segment.metadata.collector_version)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn insert_manual_annotation(
        &self,
        annotation: &ManualAnnotation,
    ) -> Result<i64, StorageError> {
        let result = sqlx::query(
            "INSERT INTO manual_annotations
             (machine_id, target_type, target_id, before_json, after_json, reason,
              created_at_ms, source)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'manual')",
        )
        .bind(annotation.machine_id)
        .bind(&annotation.target_type)
        .bind(&annotation.target_id)
        .bind(serde_json::to_string(&annotation.before_json)?)
        .bind(serde_json::to_string(&annotation.after_json)?)
        .bind(&annotation.reason)
        .bind(annotation.created_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn insert_collector_run(&self, run: &CollectorRun) -> Result<i64, StorageError> {
        let result = sqlx::query(
            "INSERT INTO collector_runs
             (machine_id, source, started_at_ms, duration_ms, status, stderr_summary,
              collector_version)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run.machine_id)
        .bind(&run.source)
        .bind(run.started_at_ms)
        .bind(run.duration_ms)
        .bind(&run.status)
        .bind(run.stderr_summary.as_deref())
        .bind(&run.collector_version)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::domain::{
        AccountContextInterval, AccountIdentity, AuthKind, ClassificationSource, DisplayGroup,
        Machine, Quality, SourceMetadata, TokenCounts, TokenObservation,
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

    async fn setup(label: &str) -> (Database, PathBuf, i64, i64) {
        let path = temp_database_path(label);
        let database = Database::connect(&path).await.unwrap();
        let machine_id = database
            .insert_machine(&Machine {
                name: "test-machine".into(),
                install_id: format!("install-{label}"),
                timezone: "Asia/Shanghai".into(),
                created_at_ms: 1,
            })
            .await
            .unwrap();
        let account_id = database
            .insert_account_identity(&AccountIdentity {
                machine_id,
                kind: AuthKind::Chatgpt,
                email_masked: Some("a***@e.com***".into()),
                identity_hmac: "hmac-test".into(),
                label: Some("test account".into()),
                created_at_ms: 1,
            })
            .await
            .unwrap();
        (database, path, machine_id, account_id)
    }

    fn metadata() -> SourceMetadata {
        SourceMetadata::new("phase1-test", Quality::Exact, None, "0.1.0")
    }

    #[tokio::test]
    async fn duplicate_raw_token_event_is_ignored() {
        let (database, path, machine_id, _) = setup("dedupe").await;
        let observation = TokenObservation {
            machine_id,
            context_interval_id: None,
            session_id: "session-1".into(),
            turn_id: Some("turn-1".into()),
            observed_at_ms: 100,
            tokens: TokenCounts {
                input_tokens: 10,
                total_tokens: 12,
                ..TokenCounts::default()
            },
            model: Some("fixture-model".into()),
            service_tier: Some("default".into()),
            source_digest: "same-event-digest".into(),
            collector_version: "0.1.0".into(),
        };
        assert!(database
            .insert_token_observation(&observation)
            .await
            .unwrap());
        assert!(!database
            .insert_token_observation(&observation)
            .await
            .unwrap());
        assert_eq!(database.count_token_observations().await.unwrap(), 1);
        database.close().await;
        cleanup(&path);
    }

    #[tokio::test]
    async fn context_intervals_allow_adjacency_and_reject_overlap() {
        let (database, path, machine_id, account_id) = setup("intervals").await;
        let interval = |start_at_ms, end_at_ms| AccountContextInterval {
            machine_id,
            account_identity_id: account_id,
            start_at_ms,
            end_at_ms,
            auth_kind: AuthKind::Chatgpt,
            plan_type_raw: Some("plus".into()),
            display_group: DisplayGroup::Plus,
            capacity_profile: None,
            provider_name: Some("openai".into()),
            endpoint_hmac: None,
            classification_source: ClassificationSource::Observed,
            metadata: metadata(),
        };

        database
            .insert_account_context_interval(&interval(0, Some(100)))
            .await
            .unwrap();
        database
            .insert_account_context_interval(&interval(100, Some(200)))
            .await
            .unwrap();
        let overlap = database
            .insert_account_context_interval(&interval(50, Some(150)))
            .await;
        assert!(overlap.is_err());
        let interval_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM account_context_intervals WHERE machine_id = ?",
        )
        .bind(machine_id)
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert_eq!(interval_count, 2);

        database.close().await;
        cleanup(&path);
    }
}
