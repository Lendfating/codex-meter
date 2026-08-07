use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{json, Value};
use sqlx::Row;
use thiserror::Error;

use super::{configured_timezone, db::DbError, pricing, Database};

#[derive(Debug, Error)]
pub enum ReportError {
    #[error("database error: {0}")]
    Database(#[from] DbError),
    #[error("database query error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("timestamp error: {0}")]
    Timestamp(String),
}

pub async fn build_report(
    database: &Database,
    selected_date: Option<&str>,
) -> Result<Value, ReportError> {
    build_rollup_report(database, selected_date).await
}

#[derive(Clone, Debug, Default)]
struct RollupMetric {
    input: i64,
    cached: i64,
    cache_write: i64,
    output: i64,
    reasoning: i64,
    total: i64,
    has_usage: bool,
    credit: f64,
    api_usd: f64,
    credit_known: bool,
    api_known: bool,
}

impl RollupMetric {
    fn add(
        &mut self,
        input: Option<i64>,
        cached: Option<i64>,
        cache_write: Option<i64>,
        output: Option<i64>,
        reasoning: Option<i64>,
        total: Option<i64>,
        credit: Option<f64>,
        api_usd: Option<f64>,
    ) {
        // A result row may contain only an official percentage/reset marker.
        // Keep that row as an observed official point, not as a synthetic
        // zero-token usage point. This preserves NULL/unknown semantics in
        // the API and prevents charts from showing fake zeros.
        let observed = input.is_some()
            || cached.is_some()
            || cache_write.is_some()
            || output.is_some()
            || reasoning.is_some()
            || total.is_some()
            || credit.is_some()
            || api_usd.is_some();
        if !observed {
            return;
        }
        if !self.has_usage {
            self.credit_known = true;
            self.api_known = true;
        }
        self.has_usage = true;
        self.input = self.input.saturating_add(input.unwrap_or_default());
        self.cached = self.cached.saturating_add(cached.unwrap_or_default());
        self.cache_write = self
            .cache_write
            .saturating_add(cache_write.unwrap_or_default());
        self.output = self.output.saturating_add(output.unwrap_or_default());
        self.reasoning = self.reasoning.saturating_add(reasoning.unwrap_or_default());
        self.total = self.total.saturating_add(total.unwrap_or_default());
        if let Some(value) = credit {
            self.credit += value;
        } else {
            self.credit_known = false;
        }
        if let Some(value) = api_usd {
            self.api_usd += value;
        } else {
            self.api_known = false;
        }
    }

    fn usage_json(&self) -> Value {
        let value = |value: i64| self.has_usage.then_some(value);
        json!({
            "input": value(self.input),
            "cached": value(self.cached),
            "cache_write": value(self.cache_write),
            "output": value(self.output),
            "reasoning": value(self.reasoning),
            "total": value(self.total)
        })
    }

    fn credit(&self) -> Option<f64> {
        (self.has_usage && self.credit_known).then_some(self.credit)
    }

    fn api_usd(&self) -> Option<f64> {
        (self.has_usage && self.api_known).then_some(self.api_usd)
    }
}

#[derive(Clone, Debug, Default)]
struct RollupDayRow {
    date: String,
    account_key: Option<String>,
    plan_type: Option<String>,
    metric: RollupMetric,
    local_percent: Option<f64>,
    account_tokens: Option<i64>,
    unobserved_tokens: Option<i64>,
    coverage_ratio: Option<f64>,
    account_token_freshness: Option<String>,
    official_start: Option<f64>,
    official_end: Option<f64>,
    official_delta: Option<f64>,
    reset_count: i64,
    quality: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct RollupMinuteRow {
    minute_start_ms: i64,
    local_date: String,
    account_key: Option<String>,
    plan_type: Option<String>,
    window_id: Option<String>,
    window_start_ms: Option<i64>,
    resets_at_ms: Option<i64>,
    reset_marker: bool,
    metric: RollupMetric,
    official_percent: Option<f64>,
    official_source: Option<String>,
    quality: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct RollupSessionRow {
    local_date: String,
    root_session_id: Option<String>,
    session_id: Option<String>,
    turn_id: Option<String>,
    title: Option<String>,
    relation: Option<String>,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    window_id: Option<String>,
    primary_model: Option<String>,
    fast_state: Option<String>,
    model_breakdown_json: Option<String>,
    metric: RollupMetric,
    official_start: Option<f64>,
    official_end: Option<f64>,
    quality: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct RollupCapacity {
    profile_code: String,
    account_key: Option<String>,
    plan_type: Option<String>,
    weekly_credit: f64,
    effective_from_ms: i64,
    effective_to_ms: Option<i64>,
}

#[derive(Clone, Debug, Default)]
struct RollupAccount {
    account_key: Option<String>,
    account_label: Option<String>,
    auth_kind: Option<String>,
    provider: Option<String>,
    plan_type: Option<String>,
    observed_at_ms: Option<i64>,
    freshness: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct RollupWindow {
    window_id: String,
    account_key: Option<String>,
    plan_type: Option<String>,
    start_at_ms: Option<i64>,
    end_at_ms: Option<i64>,
    reset_at_ms: Option<i64>,
    window_minutes: Option<i64>,
    first_official: Option<f64>,
    last_official: Option<f64>,
    reset_marker: bool,
    metric: RollupMetric,
    quality: BTreeSet<String>,
    minutes: Vec<Value>,
    sessions: Vec<Value>,
}

#[derive(Clone, Debug, Default)]
struct RollupSessionView {
    session_id: String,
    root_session_id: Option<String>,
    title: Option<String>,
    relation: Option<String>,
    first_at_ms: Option<i64>,
    last_at_ms: Option<i64>,
    metric: RollupMetric,
    models: BTreeSet<String>,
    fast_states: BTreeSet<String>,
    official_start: Option<f64>,
    official_end: Option<f64>,
    quality: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct RollupModelView {
    model: String,
    tier: Option<String>,
    requests: i64,
    metric: RollupMetric,
    tiers: BTreeSet<String>,
}

async fn build_rollup_report(
    database: &Database,
    selected_date: Option<&str>,
) -> Result<Value, ReportError> {
    let timezone = configured_timezone();
    let days = load_rollup_days(database).await?;
    let minutes = load_rollup_minutes(database).await?;
    let sessions = load_rollup_sessions(database).await?;
    let capacities = load_rollup_capacities(database).await?;
    let account = load_rollup_account(database).await?;
    let account_daily_tokens = load_rollup_account_daily_tokens(database).await?;
    let day_window_info = rollup_day_window_info(&minutes);
    let quota_windows = build_rollup_windows(&minutes, &sessions, &capacities);
    let latest_date = days.last().map(|value| value.date.clone());
    let selected_date = selected_date
        .filter(|date| days.iter().any(|value| value.date == *date))
        .map(str::to_owned)
        .or(latest_date);
    let selected_day = selected_date
        .as_deref()
        .and_then(|date| days.iter().find(|value| value.date == date));
    let selected_minutes = selected_day
        .map(|day| {
            minutes
                .iter()
                .filter(|value| value.local_date == day.date)
                .map(rollup_minute_json)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selected_session_rows = selected_day
        .map(|day| {
            sessions
                .iter()
                .filter(|value| value.local_date == day.date)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selected_models = rollup_model_views(&selected_session_rows);
    let selected_sessions = rollup_session_views(&selected_session_rows);
    let current = build_rollup_current(
        &minutes,
        &quota_windows,
        &capacities,
        &account,
        &account_daily_tokens,
        database,
    )
    .await?;
    let day_values = days
        .iter()
        .map(|day| {
            let (window_id, reset) = day_window_info
                .get(&day.date)
                .cloned()
                .unwrap_or((None, day.reset_count > 0));
            rollup_day_json(day, &capacities, window_id.as_deref(), reset)
        })
        .collect::<Vec<_>>();
    let selected_day_value = selected_day
        .map(|day| {
            json!({
                "date": day.date,
                "usage": day.metric.usage_json(),
                "credit": day.metric.credit(),
                "api_usd": day.metric.api_usd(),
                "minutes": selected_minutes,
                "models": selected_models,
                "sessions": selected_sessions
            })
        })
        .unwrap_or_else(|| {
            json!({
                "date": Value::Null,
                "usage": rollup_empty_usage(),
                "credit": Value::Null,
                "api_usd": Value::Null,
                "minutes": [],
                "models": [],
                "sessions": []
            })
        });
    let validation = load_rollup_validation(database, &days, &sessions).await?;
    let audit = load_rollup_audit(database, &timezone).await?;
    let capacity_values = capacities
        .iter()
        .map(|capacity| {
            json!({
                "plan_code": capacity.profile_code,
                "credit": capacity.weekly_credit,
                "weekly_credit": capacity.weekly_credit,
                "account_key": capacity.account_key,
                "plan_type": capacity.plan_type,
                "effective_from_ms": capacity.effective_from_ms,
                "effective_to_ms": capacity.effective_to_ms,
                "status": "confirmed"
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "current": current,
        "days": day_values,
        "selected_day": selected_day_value,
        "quota_windows": quota_windows,
        "validation": validation,
        "capacities": capacity_values,
        "methodology": {
            "calculation_version": "minimal-r1-rollup",
            "pricing_version": pricing::PRICING_VERSION,
            "formulas": [
                "total_tokens = source_jsonl incremental usage total",
                "credit = model/tier price card applied to each source_jsonl usage increment",
                "api_usd = API price card applied to each source_jsonl usage increment",
                "capacity_candidate = local_credit / (official_percent_delta / 100)"
            ],
            "sources": ["jsonl", "app_server", "ccusage"],
            "price_card": pricing::price_card()
        },
        "audit": audit
    }))
}

fn rollup_empty_usage() -> Value {
    json!({
        "input": Value::Null,
        "cached": Value::Null,
        "cache_write": Value::Null,
        "output": Value::Null,
        "reasoning": Value::Null,
        "total": Value::Null
    })
}

async fn load_rollup_days(database: &Database) -> Result<Vec<RollupDayRow>, ReportError> {
    let rows = sqlx::query(
        "SELECT local_date, account_key, auth_kind, plan_type,
                input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                reasoning_tokens, total_tokens, credit, api_usd, local_percent,
                account_tokens, unobserved_tokens, coverage_ratio,
                account_token_freshness, official_percent_start, official_percent_end,
                official_percent_delta, reset_count, quality
         FROM usage_daily ORDER BY local_date",
    )
    .fetch_all(database.pool())
    .await?;
    rows.into_iter()
        .map(|row| {
            let mut metric = RollupMetric::default();
            metric.add(
                row.try_get("input_tokens")?,
                row.try_get("cache_read_tokens")?,
                row.try_get("cache_write_tokens")?,
                row.try_get("output_tokens")?,
                row.try_get("reasoning_tokens")?,
                row.try_get("total_tokens")?,
                row.try_get("credit")?,
                row.try_get("api_usd")?,
            );
            Ok(RollupDayRow {
                date: row.try_get("local_date")?,
                account_key: row.try_get("account_key")?,
                plan_type: row.try_get("plan_type")?,
                metric,
                local_percent: row.try_get("local_percent")?,
                account_tokens: row.try_get("account_tokens")?,
                unobserved_tokens: row.try_get("unobserved_tokens")?,
                coverage_ratio: row.try_get("coverage_ratio")?,
                account_token_freshness: row.try_get("account_token_freshness")?,
                official_start: row.try_get("official_percent_start")?,
                official_end: row.try_get("official_percent_end")?,
                official_delta: row.try_get("official_percent_delta")?,
                reset_count: row.try_get::<i64, _>("reset_count")?,
                quality: quality_set(row.try_get("quality")?),
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(ReportError::from)
}

async fn load_rollup_minutes(database: &Database) -> Result<Vec<RollupMinuteRow>, ReportError> {
    let rows = sqlx::query(
        "SELECT minute_start_ms, local_date, account_key, auth_kind, plan_type,
                provider, window_id, window_start_ms, resets_at_ms, reset_marker,
                input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                reasoning_tokens, total_tokens, credit, api_usd,
                official_used_percent, official_source, quality
         FROM usage_minute ORDER BY minute_start_ms, id",
    )
    .fetch_all(database.pool())
    .await?;
    rows.into_iter()
        .map(|row| {
            let mut metric = RollupMetric::default();
            metric.add(
                row.try_get("input_tokens")?,
                row.try_get("cache_read_tokens")?,
                row.try_get("cache_write_tokens")?,
                row.try_get("output_tokens")?,
                row.try_get("reasoning_tokens")?,
                row.try_get("total_tokens")?,
                row.try_get("credit")?,
                row.try_get("api_usd")?,
            );
            Ok(RollupMinuteRow {
                minute_start_ms: row.try_get("minute_start_ms")?,
                local_date: row.try_get("local_date")?,
                account_key: row.try_get("account_key")?,
                plan_type: row.try_get("plan_type")?,
                window_id: row.try_get("window_id")?,
                window_start_ms: row.try_get("window_start_ms")?,
                resets_at_ms: row.try_get("resets_at_ms")?,
                reset_marker: row.try_get::<i64, _>("reset_marker")? != 0,
                metric,
                official_percent: row.try_get("official_used_percent")?,
                official_source: row.try_get("official_source")?,
                quality: quality_set(row.try_get("quality")?),
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(ReportError::from)
}

async fn load_rollup_sessions(database: &Database) -> Result<Vec<RollupSessionRow>, ReportError> {
    let rows = sqlx::query(
        "SELECT local_date, root_session_id, session_id, turn_id, title, relation,
                started_at_ms, ended_at_ms, window_id, account_key, plan_type,
                primary_model, fast_state, model_breakdown_json,
                input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                reasoning_tokens, total_tokens, credit, api_usd,
                official_percent_start, official_percent_end, quality
         FROM usage_session ORDER BY local_date, started_at_ms, id",
    )
    .fetch_all(database.pool())
    .await?;
    rows.into_iter()
        .map(|row| {
            let mut metric = RollupMetric::default();
            metric.add(
                row.try_get("input_tokens")?,
                row.try_get("cache_read_tokens")?,
                row.try_get("cache_write_tokens")?,
                row.try_get("output_tokens")?,
                row.try_get("reasoning_tokens")?,
                row.try_get("total_tokens")?,
                row.try_get("credit")?,
                row.try_get("api_usd")?,
            );
            Ok(RollupSessionRow {
                local_date: row.try_get("local_date")?,
                root_session_id: row.try_get("root_session_id")?,
                session_id: row.try_get("session_id")?,
                turn_id: row.try_get("turn_id")?,
                title: row.try_get("title")?,
                relation: row.try_get("relation")?,
                started_at_ms: row.try_get("started_at_ms")?,
                ended_at_ms: row.try_get("ended_at_ms")?,
                window_id: row.try_get("window_id")?,
                primary_model: row.try_get("primary_model")?,
                fast_state: row.try_get("fast_state")?,
                model_breakdown_json: row.try_get("model_breakdown_json")?,
                metric,
                official_start: row.try_get("official_percent_start")?,
                official_end: row.try_get("official_percent_end")?,
                quality: quality_set(row.try_get("quality")?),
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(ReportError::from)
}

async fn load_rollup_capacities(database: &Database) -> Result<Vec<RollupCapacity>, ReportError> {
    let rows = sqlx::query(
        "SELECT profile_code, account_key, plan_type, weekly_credit,
                effective_from_ms, effective_to_ms
         FROM capacities_v2 ORDER BY effective_from_ms DESC, profile_code",
    )
    .fetch_all(database.pool())
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(RollupCapacity {
                profile_code: row.try_get("profile_code")?,
                account_key: row.try_get("account_key")?,
                plan_type: row.try_get("plan_type")?,
                weekly_credit: row.try_get("weekly_credit")?,
                effective_from_ms: row.try_get("effective_from_ms")?,
                effective_to_ms: row.try_get("effective_to_ms")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(ReportError::from)
}

async fn load_rollup_account(database: &Database) -> Result<RollupAccount, ReportError> {
    let row = sqlx::query(
        "SELECT account_key, account_label, auth_kind, provider, plan_type,
                last_seen_at_ms, freshness
         FROM source_app_server WHERE kind = 'account'
         ORDER BY last_seen_at_ms DESC, id DESC LIMIT 1",
    )
    .fetch_optional(database.pool())
    .await?;
    Ok(row
        .map(|row| RollupAccount {
            account_key: row.try_get("account_key").ok().flatten(),
            account_label: row.try_get("account_label").ok().flatten(),
            auth_kind: row.try_get("auth_kind").ok().flatten(),
            provider: row.try_get("provider").ok().flatten(),
            plan_type: row.try_get("plan_type").ok().flatten(),
            observed_at_ms: row.try_get("last_seen_at_ms").ok(),
            freshness: row.try_get("freshness").ok().flatten(),
        })
        .unwrap_or_default())
}

async fn load_rollup_account_daily_tokens(database: &Database) -> Result<Vec<Value>, ReportError> {
    let rows = sqlx::query(
        "SELECT last_seen_at_ms, daily_tokens_json FROM source_app_server
         WHERE kind = 'usage' AND daily_tokens_json IS NOT NULL
         ORDER BY last_seen_at_ms, id",
    )
    .fetch_all(database.pool())
    .await?;
    let mut values: BTreeMap<String, Value> = BTreeMap::new();
    for row in rows {
        let observed_at_ms: i64 = row.try_get("last_seen_at_ms")?;
        let raw: String = row.try_get("daily_tokens_json")?;
        let parsed: Value = serde_json::from_str(&raw)?;
        let items = parsed.as_array().cloned().unwrap_or_else(|| vec![parsed]);
        for item in items {
            let Some(date) = item
                .get("startDate")
                .or_else(|| item.get("start_date"))
                .or_else(|| item.get("date"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let tokens = item
                .get("tokens")
                .or_else(|| item.get("totalTokens"))
                .and_then(Value::as_i64);
            let value = json!({
                "date": date,
                "tokens": tokens,
                "source": "app_server",
                "observed_at_ms": observed_at_ms
            });
            let replace = values
                .get(date)
                .and_then(|old| old.get("observed_at_ms"))
                .and_then(Value::as_i64)
                .is_none_or(|old| observed_at_ms >= old);
            if replace {
                values.insert(date.to_owned(), value);
            }
        }
    }
    Ok(values.into_values().collect())
}

fn quality_set(value: Option<String>) -> BTreeSet<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn flags_json(flags: &BTreeSet<String>) -> Value {
    Value::Array(flags.iter().cloned().map(Value::String).collect())
}

fn capacity_for<'a>(
    capacities: &'a [RollupCapacity],
    account_key: Option<&str>,
    plan_type: Option<&str>,
    at_ms: i64,
) -> Option<&'a RollupCapacity> {
    let plan = plan_type.map(str::to_ascii_lowercase);
    capacities
        .iter()
        .filter(|value| value.effective_from_ms <= at_ms)
        .filter(|value| {
            value
                .effective_to_ms
                .is_none_or(|effective_to| at_ms < effective_to)
        })
        .filter(|value| {
            value
                .account_key
                .as_deref()
                .is_none_or(|key| account_key.is_some_and(|account| account == key))
        })
        .filter(|value| {
            value.plan_type.as_deref().is_none_or(|candidate| {
                plan.as_deref()
                    .is_some_and(|actual| actual == candidate.to_ascii_lowercase())
            })
        })
        .max_by_key(|value| value.effective_from_ms)
}

fn profile_for(capacity: Option<&RollupCapacity>) -> Option<&str> {
    capacity.map(|value| value.profile_code.as_str())
}

fn rollup_day_json(
    day: &RollupDayRow,
    capacities: &[RollupCapacity],
    window_id: Option<&str>,
    reset: bool,
) -> Value {
    let at_ms = time::Date::parse(
        &day.date,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .ok()
    .and_then(|date| date.with_hms(0, 0, 0).ok())
    .and_then(|date| date.assume_utc().unix_timestamp_nanos().try_into().ok())
    .map(|value: i128| (value / 1_000_000) as i64)
    .unwrap_or_default();
    let capacity = capacity_for(
        capacities,
        day.account_key.as_deref(),
        day.plan_type.as_deref(),
        at_ms,
    );
    let local_percent = day.local_percent.or_else(|| {
        day.metric
            .credit()
            .zip(capacity.map(|value| value.weekly_credit))
            .and_then(|(credit, capacity)| (capacity > 0.0).then_some(credit / capacity * 100.0))
    });
    let mut quality = day.quality.clone();
    if capacity.is_none() && day.metric.credit().is_some() {
        quality.insert("capacity_unconfirmed".to_owned());
    }
    json!({
        "date": day.date,
        "usage": day.metric.usage_json(),
        "credit": day.metric.credit(),
        "api_usd": day.metric.api_usd(),
        "local_percent": local_percent,
        "local_daily_percent": local_percent,
        "account_tokens": day.account_tokens,
        "unobserved_tokens": day.unobserved_tokens,
        "coverage_ratio": day.coverage_ratio,
        "account_token_freshness": day.account_token_freshness,
        "official_percent_start": day.official_start,
        "official_percent_end": day.official_end,
        "official_percent_delta": day.official_delta,
        "official_daily_percent": day.official_delta,
        "official_remaining_percent": day.official_end.map(|value| (100.0 - value).max(0.0)),
        "account_percent_delta": day.official_delta,
        "capacity_credit": capacity.map(|value| value.weekly_credit),
        "weekly_credit": capacity.map(|value| value.weekly_credit),
        "plan_code": profile_for(capacity),
        "plan": day.plan_type,
        "window_id": window_id,
        "reset": reset,
        "reset_boundary": reset,
        "reset_generation": window_id,
        "reset_count": day.reset_count,
        "quality_flags": flags_json(&quality),
        "audit": {"events": Value::Null, "sessions": Value::Null}
    })
}

fn rollup_minute_json(row: &RollupMinuteRow) -> Value {
    json!({
        "minute_start_ms": row.minute_start_ms,
        "local_date": row.local_date,
        "usage": row.metric.usage_json(),
        "credit": row.metric.credit(),
        "api_usd": row.metric.api_usd(),
        "official_percent": row.official_percent,
        "official_source": row.official_source,
        "window_id": row.window_id,
        "window_start_ms": row.window_start_ms,
        "resets_at_ms": row.resets_at_ms,
        "reset": row.reset_marker,
        "reset_boundary": row.reset_marker,
        "quality_flags": flags_json(&row.quality)
    })
}

fn rollup_day_window_info(minutes: &[RollupMinuteRow]) -> HashMap<String, (Option<String>, bool)> {
    let mut values = HashMap::new();
    let mut previous_window: Option<String> = None;
    for minute in minutes {
        let entry = values
            .entry(minute.local_date.clone())
            .or_insert_with(|| (None, false));
        if minute.reset_marker
            || previous_window
                .as_deref()
                .zip(minute.window_id.as_deref())
                .is_some_and(|(previous, current)| previous != current)
        {
            entry.1 = true;
        }
        entry.0 = minute.window_id.clone();
        previous_window = minute.window_id.clone();
    }
    values
}

fn build_rollup_windows(
    minutes: &[RollupMinuteRow],
    sessions: &[RollupSessionRow],
    capacities: &[RollupCapacity],
) -> Vec<Value> {
    let mut windows: BTreeMap<String, RollupWindow> = BTreeMap::new();
    for minute in minutes {
        let Some(window_id) = minute.window_id.clone() else {
            continue;
        };
        let window = windows
            .entry(window_id.clone())
            .or_insert_with(|| RollupWindow {
                window_id: window_id.clone(),
                ..RollupWindow::default()
            });
        window.account_key = window
            .account_key
            .clone()
            .or_else(|| minute.account_key.clone());
        window.plan_type = window
            .plan_type
            .clone()
            .or_else(|| minute.plan_type.clone());
        window.start_at_ms = min_option(
            window.start_at_ms,
            minute.window_start_ms.or(Some(minute.minute_start_ms)),
        );
        window.end_at_ms = max_option(window.end_at_ms, Some(minute.minute_start_ms));
        window.reset_at_ms = max_option(window.reset_at_ms, minute.resets_at_ms);
        window.window_minutes = window
            .reset_at_ms
            .zip(window.start_at_ms)
            .and_then(|(reset, start)| (reset > start).then_some((reset - start) / 60_000));
        window.first_official = window.first_official.or(minute.official_percent);
        window.last_official = minute.official_percent.or(window.last_official);
        window.reset_marker |= minute.reset_marker;
        if minute.metric.has_usage {
            window.metric.add(
                Some(minute.metric.input),
                Some(minute.metric.cached),
                Some(minute.metric.cache_write),
                Some(minute.metric.output),
                Some(minute.metric.reasoning),
                Some(minute.metric.total),
                minute.metric.credit(),
                minute.metric.api_usd(),
            );
        }
        window.quality.extend(minute.quality.iter().cloned());
        window.minutes.push(rollup_minute_json(minute));
    }
    let mut sessions_by_window: BTreeMap<String, Vec<&RollupSessionRow>> = BTreeMap::new();
    for session in sessions {
        if let Some(window_id) = session.window_id.clone() {
            sessions_by_window
                .entry(window_id)
                .or_default()
                .push(session);
        }
    }
    for (window_id, rows) in sessions_by_window {
        if let Some(window) = windows.get_mut(&window_id) {
            window.sessions = rollup_session_views(&rows);
        }
    }
    windows
        .into_values()
        .map(|window| {
            let capacity = capacity_for(
                capacities,
                window.account_key.as_deref(),
                window.plan_type.as_deref(),
                window.start_at_ms.unwrap_or_default(),
            );
            let local_percent = window
                .metric
                .credit()
                .zip(capacity.map(|value| value.weekly_credit))
                .and_then(|(credit, capacity)| {
                    (capacity > 0.0).then_some(credit / capacity * 100.0)
                });
            json!({
                "window_id": window.window_id,
                "account_key": window.account_key,
                "limit": "codex",
                "window_kind": "primary",
                "plan": window.plan_type,
                "start_at_ms": window.start_at_ms,
                "end_at_ms": window.end_at_ms,
                "reset_at_ms": window.reset_at_ms,
                "window_minutes": window.window_minutes,
                "official_percent_start": window.first_official,
                "official_percent_end": window.last_official,
                "percent_delta": window.first_official.zip(window.last_official).map(|(start, end)| (end - start).max(0.0)),
                "official_remaining_percent": window.last_official.map(|value| (100.0 - value).max(0.0)),
                "local_tokens": window.metric.has_usage.then_some(window.metric.total),
                "local_credit": window.metric.credit(),
                "local_api_usd": window.metric.api_usd(),
                "local_percent": local_percent,
                "capacity_credit": capacity.map(|value| value.weekly_credit),
                "shared": Value::Null,
                "reset": window.reset_marker,
                "reset_boundary": window.reset_marker,
                "minutes": window.minutes,
                "sessions": window.sessions,
                "quality_flags": flags_json(&window.quality)
            })
        })
        .collect()
}

fn min_option(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn max_option(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn rollup_session_views(rows: &[&RollupSessionRow]) -> Vec<Value> {
    let mut views: BTreeMap<String, RollupSessionView> = BTreeMap::new();
    for row in rows {
        let session_id = row
            .root_session_id
            .clone()
            .or_else(|| row.session_id.clone())
            .or_else(|| row.turn_id.clone())
            .unwrap_or_else(|| "unknown-session".to_owned());
        let view = views
            .entry(session_id.clone())
            .or_insert_with(|| RollupSessionView {
                session_id: session_id.clone(),
                ..RollupSessionView::default()
            });
        view.root_session_id = view
            .root_session_id
            .clone()
            .or_else(|| row.root_session_id.clone());
        view.title = view.title.clone().or_else(|| row.title.clone());
        view.relation = match (view.relation.as_deref(), row.relation.as_deref()) {
            (Some("main"), _) | (_, Some("main")) => Some("main".to_owned()),
            (Some(value), _) => Some(value.to_owned()),
            (None, Some(value)) => Some(value.to_owned()),
            (None, None) => None,
        };
        view.first_at_ms = min_option(view.first_at_ms, row.started_at_ms);
        view.last_at_ms = max_option(view.last_at_ms, row.ended_at_ms.or(row.started_at_ms));
        view.metric.add(
            Some(row.metric.input),
            Some(row.metric.cached),
            Some(row.metric.cache_write),
            Some(row.metric.output),
            Some(row.metric.reasoning),
            Some(row.metric.total),
            row.metric.credit(),
            row.metric.api_usd(),
        );
        if let Some(model) = row.primary_model.as_deref() {
            view.models.insert(model.to_owned());
        }
        if let Some(fast_state) = row.fast_state.as_deref() {
            view.fast_states.insert(fast_state.to_owned());
        }
        view.official_start = view.official_start.or(row.official_start);
        view.official_end = row.official_end.or(view.official_end);
        view.quality.extend(row.quality.iter().cloned());
        for model in model_breakdown_names(row.model_breakdown_json.as_deref()) {
            view.models.insert(model);
        }
    }
    views
        .into_values()
        .map(|view| {
            let fast = if view.fast_states.len() == 1 {
                view.fast_states
                    .iter()
                    .next()
                    .and_then(|value| match value.as_str() {
                        "fast" => Some(true),
                        "standard" => Some(false),
                        _ => None,
                    })
            } else {
                None
            };
            json!({
                "session_id": view.session_id,
                "title": view.title,
                "root_session_id": view.root_session_id,
                "relation": view.relation.unwrap_or_else(|| "unknown".to_owned()),
                "first_at_ms": view.first_at_ms,
                "last_at_ms": view.last_at_ms,
                "models": view.models.into_iter().collect::<Vec<_>>(),
                "fast": fast,
                "usage": view.metric.usage_json(),
                "credit": view.metric.credit(),
                "api_usd": view.metric.api_usd(),
                "official_percent_start": view.official_start,
                "official_percent_end": view.official_end,
                "quality_flags": flags_json(&view.quality)
            })
        })
        .collect()
}

fn rollup_model_views(rows: &[&RollupSessionRow]) -> Vec<Value> {
    let mut models: BTreeMap<(String, String), RollupModelView> = BTreeMap::new();
    for row in rows {
        let breakdown = parse_model_breakdown(row.model_breakdown_json.as_deref());
        if breakdown.is_empty() {
            if let Some(model) = row.primary_model.clone() {
                let key = (model.clone(), "unknown".to_owned());
                let view = models.entry(key).or_insert_with(|| RollupModelView {
                    model,
                    ..RollupModelView::default()
                });
                view.requests += 1;
                view.metric.add(
                    Some(row.metric.input),
                    Some(row.metric.cached),
                    Some(row.metric.cache_write),
                    Some(row.metric.output),
                    Some(row.metric.reasoning),
                    Some(row.metric.total),
                    row.metric.credit(),
                    row.metric.api_usd(),
                );
            }
            continue;
        }
        for (model, tier, metric) in breakdown {
            let key = (model.clone(), tier.clone());
            let view = models.entry(key).or_insert_with(|| RollupModelView {
                model,
                tier: Some(tier.clone()),
                ..RollupModelView::default()
            });
            view.requests += 1;
            view.tiers.insert(tier);
            view.metric.add(
                None,
                None,
                None,
                None,
                None,
                Some(metric.total),
                metric.credit(),
                metric.api_usd(),
            );
        }
    }
    models
        .into_values()
        .map(|view| {
            json!({
                "model": view.model,
                "tier": view.tier.or_else(|| (view.tiers.len() == 1).then(|| view.tiers.into_iter().next()).flatten()),
                "requests": view.requests,
                "usage": view.metric.usage_json(),
                "credit": view.metric.credit(),
                "api_usd": view.metric.api_usd()
            })
        })
        .collect()
}

fn model_breakdown_names(value: Option<&str>) -> Vec<String> {
    parse_model_breakdown(value)
        .into_iter()
        .map(|(model, _, _)| model)
        .collect()
}

fn parse_model_breakdown(value: Option<&str>) -> Vec<(String, String, RollupMetric)> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(value) else {
        return Vec::new();
    };
    parsed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let model = item.get("model").and_then(Value::as_str)?.to_owned();
            let tier = item
                .get("tier")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let mut metric = RollupMetric::default();
            metric.add(
                None,
                None,
                None,
                None,
                None,
                item.get("tokens").and_then(Value::as_i64),
                item.get("credit").and_then(Value::as_f64),
                item.get("api_usd").and_then(Value::as_f64),
            );
            Some((model, tier, metric))
        })
        .collect()
}

async fn build_rollup_current(
    minutes: &[RollupMinuteRow],
    windows: &[Value],
    capacities: &[RollupCapacity],
    account: &RollupAccount,
    account_daily_tokens: &[Value],
    database: &Database,
) -> Result<Value, ReportError> {
    let app_quota = sqlx::query(
        "SELECT used_percent, window_minutes, resets_at_ms, last_seen_at_ms
         FROM source_app_server WHERE kind = 'quota'
         ORDER BY last_seen_at_ms DESC, id DESC LIMIT 1",
    )
    .fetch_optional(database.pool())
    .await?;
    let app_used = app_quota
        .as_ref()
        .and_then(|row| row.try_get::<Option<f64>, _>("used_percent").ok())
        .flatten();
    let app_window_minutes = app_quota
        .as_ref()
        .and_then(|row| row.try_get::<Option<i64>, _>("window_minutes").ok())
        .flatten();
    let app_reset = app_quota
        .as_ref()
        .and_then(|row| row.try_get::<Option<i64>, _>("resets_at_ms").ok())
        .flatten();
    let latest_minute = minutes.last();
    let used_percent = app_used.or_else(|| latest_minute.and_then(|value| value.official_percent));
    let reset_at_ms = app_reset.or_else(|| latest_minute.and_then(|value| value.resets_at_ms));
    let official_source = if app_used.is_some() {
        "app_server"
    } else {
        latest_minute
            .and_then(|value| value.official_source.as_deref())
            .unwrap_or("none")
    };
    let current_window = reset_at_ms
        .and_then(|reset| {
            windows.iter().min_by_key(|window| {
                value_i64(window, "reset_at_ms")
                    .map(|value| (value - reset).abs())
                    .unwrap_or(i64::MAX)
            })
        })
        .or_else(|| windows.last());
    let plan = account.plan_type.as_deref();
    let window_start = current_window.and_then(|value| value_i64(value, "start_at_ms"));
    let capacity = window_start
        .and_then(|start| capacity_for(capacities, account.account_key.as_deref(), plan, start));
    let weekly_credit = capacity.map(|value| value.weekly_credit);
    let local_window = current_window
        .map(|window| {
            json!({
                "window_id": window.get("window_id").cloned().unwrap_or(Value::Null),
                "start_at_ms": window.get("start_at_ms").cloned().unwrap_or(Value::Null),
                "reset_at_ms": window.get("reset_at_ms").cloned().unwrap_or(Value::Null),
                "window_minutes": window.get("window_minutes").cloned().unwrap_or_else(|| json!(app_window_minutes)),
                "token": window.get("local_tokens").cloned().unwrap_or(Value::Null),
                "tokens": window.get("local_tokens").cloned().unwrap_or(Value::Null),
                "credit": window.get("local_credit").cloned().unwrap_or(Value::Null),
                "api_usd": window.get("local_api_usd").cloned().unwrap_or(Value::Null),
                "percent": window.get("local_percent").cloned().unwrap_or(Value::Null),
                "weekly_credit": weekly_credit,
                "capacity_credit": weekly_credit
            })
        })
        .unwrap_or_else(|| {
            json!({
                "window_id": Value::Null,
                "start_at_ms": Value::Null,
                "reset_at_ms": reset_at_ms,
                "window_minutes": app_window_minutes,
                "token": Value::Null,
                "tokens": Value::Null,
                "credit": Value::Null,
                "api_usd": Value::Null,
                "percent": Value::Null,
                "weekly_credit": weekly_credit,
                "capacity_credit": weekly_credit
            })
        });
    let quality = if used_percent.is_none() {
        json!(["official_unavailable"])
    } else if weekly_credit.is_none() {
        json!(["capacity_unconfirmed"])
    } else {
        json!([])
    };
    Ok(json!({
        "machine": "local",
        "timezone": configured_timezone(),
        "account": {
            "display": account.account_label.clone().or_else(|| account.plan_type.clone()).or_else(|| account.provider.clone()),
            "account_key": account.account_key,
            "auth_kind": account.auth_kind,
            "plan": account.plan_type,
            "provider": account.provider,
            "weekly_credit": weekly_credit,
            "credit_capacity": weekly_credit,
            "capacity_credit": weekly_credit,
            "observed_at_ms": account.observed_at_ms,
            "freshness": account.freshness
        },
        "account_daily_tokens": account_daily_tokens,
        "official": {
            "used_percent": used_percent,
            "remaining_percent": used_percent.map(|value| (100.0 - value).max(0.0)),
            "resets_at_ms": reset_at_ms,
            "window_minutes": app_window_minutes.or_else(|| current_window.and_then(|value| value_i64(value, "window_minutes"))),
            "source": official_source,
            "observed_at_ms": app_quota.as_ref().and_then(|row| row.try_get::<i64, _>("last_seen_at_ms").ok()).or_else(|| latest_minute.map(|value| value.minute_start_ms)),
            "window_id": current_window.and_then(|value| value.get("window_id")).cloned().unwrap_or(Value::Null)
        },
        "local_window": local_window,
        "quality_flags": quality
    }))
}

fn value_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_f64().map(|value| value as i64))
    })
}

#[derive(Clone, Debug, Default)]
struct RollupCcusageRow {
    run_at_ms: i64,
    scope: String,
    scope_key: String,
    pricing: String,
    speed: String,
    total_tokens: Option<i64>,
    input_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    amount: Option<f64>,
    version: Option<String>,
    status: String,
}

async fn load_rollup_validation(
    database: &Database,
    days: &[RollupDayRow],
    sessions: &[RollupSessionRow],
) -> Result<Value, ReportError> {
    let rows = sqlx::query(
        "SELECT run_at_ms, scope, scope_key, pricing_scheme, speed,
                input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                reasoning_tokens, total_tokens, amount, ccusage_version, status
         FROM source_ccusage ORDER BY run_at_ms, id",
    )
    .fetch_all(database.pool())
    .await?
    .into_iter()
    .map(|row| {
        Ok(RollupCcusageRow {
            run_at_ms: row.try_get("run_at_ms")?,
            scope: row.try_get("scope")?,
            scope_key: row.try_get("scope_key")?,
            pricing: row.try_get("pricing_scheme")?,
            speed: row.try_get("speed")?,
            input_tokens: row.try_get("input_tokens")?,
            cache_read_tokens: row.try_get("cache_read_tokens")?,
            cache_write_tokens: row.try_get("cache_write_tokens")?,
            output_tokens: row.try_get("output_tokens")?,
            reasoning_tokens: row.try_get("reasoning_tokens")?,
            total_tokens: row.try_get("total_tokens")?,
            amount: row.try_get("amount")?,
            version: row.try_get("ccusage_version")?,
            status: row.try_get("status")?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(ReportError::from)?;
    let mut latest_runs: BTreeMap<(String, String, String), i64> = BTreeMap::new();
    for row in &rows {
        let key = (row.scope.clone(), row.pricing.clone(), row.speed.clone());
        latest_runs
            .entry(key)
            .and_modify(|value| *value = (*value).max(row.run_at_ms))
            .or_insert(row.run_at_ms);
    }
    let latest_rows = rows
        .into_iter()
        .filter(|row| {
            latest_runs
                .get(&(row.scope.clone(), row.pricing.clone(), row.speed.clone()))
                .is_some_and(|run_at| *run_at == row.run_at_ms)
        })
        .collect::<Vec<_>>();
    let mut grouped: BTreeMap<(String, String, String), Vec<RollupCcusageRow>> = BTreeMap::new();
    for row in latest_rows {
        grouped
            .entry((row.scope.clone(), row.pricing.clone(), row.speed.clone()))
            .or_default()
            .push(row);
    }
    let mut latest = Vec::new();
    let mut comparisons = Vec::new();
    for ((scope, pricing, speed), values) in grouped {
        let status = if values.iter().all(|value| value.status == "ok") {
            "ok"
        } else if values.iter().any(|value| value.status == "ok") {
            "partial"
        } else {
            "failed"
        };
        let total_tokens = values
            .iter()
            .filter_map(|value| value.total_tokens)
            .sum::<i64>();
        let amount = values.iter().filter_map(|value| value.amount).sum::<f64>();
        latest.push(json!({
            "scope": scope,
            "source": "ccusage",
            "pricing": pricing,
            "speed": speed,
            "status": status,
            "version": values.iter().find_map(|value| value.version.clone()),
            "data": {
                "rows": values.len(),
                "totals": {
                    "total_tokens": (!values.is_empty()).then_some(total_tokens),
                    "amount": values.iter().any(|value| value.amount.is_some()).then_some(amount)
                }
            }
        }));
        if status != "ok" {
            continue;
        }
        for row in values {
            if row.scope == "daily" {
                if let Some(day) = days.iter().find(|day| day.date == row.scope_key) {
                    comparisons.push(rollup_comparison(
                        &day.date,
                        None,
                        "total_tokens",
                        Some(day.metric.total as f64),
                        row.total_tokens.map(|value| value as f64),
                        &row.pricing,
                        &row.speed,
                    ));
                    comparison_if_present(
                        &mut comparisons,
                        &day.date,
                        None,
                        "input_tokens",
                        Some(day.metric.input as f64),
                        row.input_tokens.map(|value| value as f64),
                        &row,
                    );
                    comparison_if_present(
                        &mut comparisons,
                        &day.date,
                        None,
                        "cache_read_tokens",
                        Some(day.metric.cached as f64),
                        row.cache_read_tokens.map(|value| value as f64),
                        &row,
                    );
                    comparison_if_present(
                        &mut comparisons,
                        &day.date,
                        None,
                        "cache_write_tokens",
                        Some(day.metric.cache_write as f64),
                        row.cache_write_tokens.map(|value| value as f64),
                        &row,
                    );
                    comparison_if_present(
                        &mut comparisons,
                        &day.date,
                        None,
                        "output_tokens",
                        Some(day.metric.output as f64),
                        row.output_tokens.map(|value| value as f64),
                        &row,
                    );
                    comparison_if_present(
                        &mut comparisons,
                        &day.date,
                        None,
                        "reasoning_tokens",
                        Some(day.metric.reasoning as f64),
                        row.reasoning_tokens.map(|value| value as f64),
                        &row,
                    );
                    if row.amount.is_some() {
                        comparisons.push(rollup_comparison(
                            &day.date,
                            None,
                            "api_usd",
                            day.metric.api_usd(),
                            row.amount,
                            &row.pricing,
                            &row.speed,
                        ));
                    }
                }
            } else if row.scope == "session" {
                if let Some(session) = sessions.iter().find(|session| {
                    session.root_session_id.as_deref() == Some(row.scope_key.as_str())
                        || session.session_id.as_deref() == Some(row.scope_key.as_str())
                }) {
                    comparisons.push(rollup_comparison(
                        &session.local_date,
                        Some(&row.scope_key),
                        "total_tokens",
                        Some(session.metric.total as f64),
                        row.total_tokens.map(|value| value as f64),
                        &row.pricing,
                        &row.speed,
                    ));
                }
            }
        }
    }
    Ok(json!({"latest": latest, "comparisons": comparisons}))
}

fn comparison_if_present(
    comparisons: &mut Vec<Value>,
    date: &str,
    session_id: Option<&str>,
    metric: &str,
    local: Option<f64>,
    ccusage: Option<f64>,
    row: &RollupCcusageRow,
) {
    if ccusage.is_some() {
        comparisons.push(rollup_comparison(
            date,
            session_id,
            metric,
            local,
            ccusage,
            &row.pricing,
            &row.speed,
        ));
    }
}

fn rollup_comparison(
    date: &str,
    session_id: Option<&str>,
    metric: &str,
    local: Option<f64>,
    ccusage: Option<f64>,
    pricing: &str,
    speed: &str,
) -> Value {
    let diff = local.zip(ccusage).map(|(local, ccusage)| ccusage - local);
    let status = match diff {
        Some(value) if value.abs() < 0.000_001 => "match",
        Some(_) => "diff",
        None => "pending",
    };
    json!({
        "date": date,
        "session_id": session_id,
        "metric": metric,
        "local": local,
        "ccusage": ccusage,
        "diff": diff,
        "status": status,
        "pricing": pricing,
        "speed": speed
    })
}

async fn load_rollup_audit(database: &Database, timezone: &str) -> Result<Value, ReportError> {
    let source_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_jsonl")
        .fetch_one(database.pool())
        .await?;
    let usage_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM source_jsonl WHERE kind = 'usage'")
            .fetch_one(database.pool())
            .await?;
    let session_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT COALESCE(root_session_id, session_id))
         FROM source_jsonl WHERE kind = 'usage'",
    )
    .fetch_one(database.pool())
    .await?;
    let ccusage_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_ccusage")
        .fetch_one(database.pool())
        .await?;
    Ok(json!({
        "files": Value::Null,
        "lines": source_rows,
        "events": usage_events,
        "duplicates": Value::Null,
        "sessions": session_count,
        "source_rows": source_rows,
        "ccusage_rows": ccusage_rows,
        "timezone": timezone
    }))
}

pub fn capacity_candidate(local_credit: f64, percent_delta: f64) -> Option<f64> {
    (local_credit.is_finite()
        && percent_delta.is_finite()
        && local_credit > 0.0
        && percent_delta > 0.0)
        .then(|| local_credit / (percent_delta / 100.0))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::minimal::{refresh_rollups, JsonlCollector};

    fn temp_home() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("codex-meter-minimal-report-{nonce}"))
    }

    #[tokio::test]
    async fn report_has_day_minute_model_and_session_grains() {
        let root = temp_home();
        fs::create_dir_all(root.join("sessions/2026/08")).unwrap();
        fs::copy(
            "fixtures/jsonl/codex-session-plus-quota-sanitized.jsonl",
            root.join("sessions/2026/08/session.jsonl"),
        )
        .unwrap();
        fs::copy(
            "fixtures/jsonl/codex-session-pro-sanitized.jsonl",
            root.join("sessions/2026/08/pro.jsonl"),
        )
        .unwrap();
        let database = Database::connect_in_memory().await.unwrap();
        JsonlCollector::new(&root)
            .scan_once(&database)
            .await
            .unwrap();
        refresh_rollups(&database).await.unwrap();
        let report = build_report(&database, None).await.unwrap();
        assert_eq!(report["days"].as_array().map(Vec::len), Some(2));
        assert_eq!(report["days"][0]["date"], "2026-07-23");
        assert_eq!(report["days"][0]["usage"]["total"], 503074);
        assert_eq!(report["days"][0]["credit"], Value::Null);
        assert_eq!(report["days"][1]["date"], "2026-08-04");
        assert_eq!(report["days"][1]["usage"]["total"], 17708);
        assert_eq!(report["days"][1]["credit"], Value::Null);
        assert!(report["selected_day"]["minutes"]
            .as_array()
            .is_some_and(|minutes| !minutes.is_empty()));
        assert!(report["selected_day"]["models"].is_array());
        assert!(report["selected_day"]["sessions"].is_array());
        assert_eq!(
            report["selected_day"]["sessions"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(report["methodology"]["sources"][0], "jsonl");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn report_reads_materialized_rollups_after_second_pipeline() {
        let root = temp_home();
        fs::create_dir_all(root.join("sessions/2026/08")).unwrap();
        fs::copy(
            "fixtures/jsonl/codex-session-plus-quota-sanitized.jsonl",
            root.join("sessions/2026/08/session.jsonl"),
        )
        .unwrap();
        let database = Database::connect_in_memory().await.unwrap();
        JsonlCollector::new(&root)
            .scan_once(&database)
            .await
            .unwrap();
        refresh_rollups(&database).await.unwrap();
        let report = build_report(&database, None).await.unwrap();
        assert_eq!(
            report["methodology"]["calculation_version"],
            "minimal-r1-rollup"
        );
        assert!(report["days"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()));
        assert!(report["selected_day"]["minutes"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()));
        assert!(report["quota_windows"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capacity_candidate_is_transparent_and_rejects_zero_delta() {
        assert_eq!(capacity_candidate(20.0, 10.0), Some(200.0));
        assert_eq!(capacity_candidate(20.0, 0.0), None);
        assert_eq!(capacity_candidate(0.0, 10.0), None);
    }
}
