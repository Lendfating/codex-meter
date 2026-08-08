use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use sqlx::{QueryBuilder, Row, Sqlite};

use super::{Database, DbError, SourceJsonlRecord};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceJsonlBatchReport {
    pub inserted_events: usize,
    pub duplicate_events: usize,
    pub inserted_quota_samples: usize,
}

const SOURCE_JSONL_UPSERT: &str = "INSERT INTO source_jsonl
    (source_key, kind, observed_at_ms, last_seen_at_ms, session_id,
     parent_session_id, root_session_id, turn_id, relation, title,
     started_at_ms, ended_at_ms, model, service_tier, reasoning_effort,
     provider, plan_type, input_tokens,
     cache_read_tokens, cache_write_tokens, output_tokens, reasoning_tokens,
     total_tokens, limit_id, window_kind, used_percent, window_minutes,
     resets_at_ms, quality)
 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
 ON CONFLICT(source_key) DO UPDATE SET
    last_seen_at_ms = COALESCE(excluded.last_seen_at_ms, source_jsonl.last_seen_at_ms),
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
    quality = COALESCE(excluded.quality, source_jsonl.quality)";

struct NormalizedBatch {
    records: Vec<SourceJsonlRecord>,
    input_records: usize,
    merged_duplicates: usize,
}

fn normalize_records(records: Vec<SourceJsonlRecord>) -> NormalizedBatch {
    let input_records = records.len();
    let mut indexes = HashMap::with_capacity(input_records);
    let mut normalized: Vec<SourceJsonlRecord> = Vec::with_capacity(input_records);
    let mut merged_duplicates = 0;

    for record in records {
        if let Some(index) = indexes.get(&record.source_key).copied() {
            merge_record(&mut normalized[index], record);
            merged_duplicates += 1;
        } else {
            indexes.insert(record.source_key.clone(), normalized.len());
            normalized.push(record);
        }
    }

    NormalizedBatch {
        records: normalized,
        input_records,
        merged_duplicates,
    }
}

fn replace_if_some<T>(target: &mut Option<T>, incoming: Option<T>) {
    if incoming.is_some() {
        *target = incoming;
    }
}

fn merge_record(target: &mut SourceJsonlRecord, incoming: SourceJsonlRecord) {
    let SourceJsonlRecord {
        source_key: _,
        kind: _,
        observed_at_ms: _,
        last_seen_at_ms,
        session_id: _,
        parent_session_id,
        root_session_id,
        turn_id,
        relation,
        title,
        started_at_ms,
        ended_at_ms,
        model,
        service_tier,
        reasoning_effort,
        provider,
        plan_type,
        input_tokens,
        cache_read_tokens,
        cache_write_tokens,
        output_tokens,
        reasoning_tokens,
        total_tokens,
        limit_id,
        window_kind,
        used_percent,
        window_minutes,
        resets_at_ms,
        quality,
    } = incoming;

    replace_if_some(&mut target.last_seen_at_ms, last_seen_at_ms);
    replace_if_some(&mut target.parent_session_id, parent_session_id);
    replace_if_some(&mut target.root_session_id, root_session_id);
    replace_if_some(&mut target.turn_id, turn_id);
    replace_if_some(&mut target.relation, relation);
    replace_if_some(&mut target.title, title);
    replace_if_some(&mut target.started_at_ms, started_at_ms);
    replace_if_some(&mut target.ended_at_ms, ended_at_ms);
    replace_if_some(&mut target.model, model);
    replace_if_some(&mut target.service_tier, service_tier);
    replace_if_some(&mut target.reasoning_effort, reasoning_effort);
    replace_if_some(&mut target.provider, provider);
    replace_if_some(&mut target.plan_type, plan_type);
    replace_if_some(&mut target.input_tokens, input_tokens);
    replace_if_some(&mut target.cache_read_tokens, cache_read_tokens);
    replace_if_some(&mut target.cache_write_tokens, cache_write_tokens);
    replace_if_some(&mut target.output_tokens, output_tokens);
    replace_if_some(&mut target.reasoning_tokens, reasoning_tokens);
    replace_if_some(&mut target.total_tokens, total_tokens);
    replace_if_some(&mut target.limit_id, limit_id);
    replace_if_some(&mut target.window_kind, window_kind);
    replace_if_some(&mut target.used_percent, used_percent);
    replace_if_some(&mut target.window_minutes, window_minutes);
    replace_if_some(&mut target.resets_at_ms, resets_at_ms);
    replace_if_some(&mut target.quality, quality);
}

impl Database {
    pub async fn upsert_source_jsonl_batch(
        &self,
        records: Vec<SourceJsonlRecord>,
    ) -> Result<SourceJsonlBatchReport, DbError> {
        let total_started = Instant::now();
        let normalize_started = Instant::now();
        let normalized = normalize_records(records);
        let normalize_elapsed = normalize_started.elapsed();
        if normalized.records.is_empty() {
            return Ok(SourceJsonlBatchReport::default());
        }

        let lookup_started = Instant::now();
        let existing = self.existing_source_keys(&normalized.records).await?;
        let lookup_elapsed = lookup_started.elapsed();
        let inserted_events = normalized
            .records
            .iter()
            .filter(|record| !existing.contains(&record.source_key))
            .count();
        let report = SourceJsonlBatchReport {
            inserted_events,
            duplicate_events: normalized.input_records - inserted_events,
            inserted_quota_samples: normalized
                .records
                .iter()
                .filter(|record| record.kind == "quota" && !existing.contains(&record.source_key))
                .count(),
        };

        let execute_started = Instant::now();
        let mut transaction = self.pool.begin().await?;
        for record in &normalized.records {
            sqlx::query(SOURCE_JSONL_UPSERT)
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
                .execute(&mut *transaction)
                .await?;
        }
        let execute_elapsed = execute_started.elapsed();
        let commit_started = Instant::now();
        transaction.commit().await?;
        let commit_elapsed = commit_started.elapsed();
        if std::env::var_os("CODEX_METER_PROFILE").is_some() {
            eprintln!(
                "source_jsonl_db_profile total_ms={} input_records={} unique_records={} merged_duplicates={} existing_records={} normalize_ms={} lookup_ms={} execute_ms={} commit_ms={}",
                total_started.elapsed().as_millis(),
                normalized.input_records,
                normalized.records.len(),
                normalized.merged_duplicates,
                existing.len(),
                normalize_elapsed.as_millis(),
                lookup_elapsed.as_millis(),
                execute_elapsed.as_millis(),
                commit_elapsed.as_millis(),
            );
        }
        Ok(report)
    }

    async fn existing_source_keys(
        &self,
        records: &[SourceJsonlRecord],
    ) -> Result<HashSet<String>, DbError> {
        let mut keys = HashSet::with_capacity(records.len());
        for chunk in records.chunks(400) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT source_key FROM source_jsonl WHERE source_key IN (",
            );
            {
                let mut separated = query.separated(", ");
                for record in chunk {
                    separated.push_bind(&record.source_key);
                }
            }
            query.push(")");
            for row in query.build().fetch_all(&self.pool).await? {
                keys.insert(row.try_get::<String, _>("source_key")?);
            }
        }
        Ok(keys)
    }
}
