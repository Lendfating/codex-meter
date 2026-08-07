//! Second-batch Pipeline: materialize the three page-facing result tables.
//!
//! The source tables are the only durable input.  This module is deliberately
//! rebuildable: every run replaces `usage_daily`, `usage_minute`, and
//! `usage_session` in one SQLite transaction.  Reset/weekly windows are not a
//! fourth table; they are represented by `usage_minute.window_id`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{json, Value};
use sqlx::Row;
use thiserror::Error;
use time::{OffsetDateTime, UtcOffset};

use super::{configured_timezone, pricing, Database, TokenCounts};

const RESET_DROP_PERCENT: f64 = 5.0;
const RESET_TIME_JITTER_MS: i64 = 5 * 60 * 1_000;

#[derive(Debug, Error)]
pub enum RollupError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("database wrapper error: {0}")]
    Db(#[from] super::db::DbError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("timestamp error: {0}")]
    Timestamp(String),
}

#[derive(Clone, Debug, Default)]
pub struct RollupSummary {
    pub days: usize,
    pub minutes: usize,
    pub sessions: usize,
    pub windows: usize,
}

#[derive(Clone, Debug)]
struct UsageRecord {
    source_key: String,
    observed_at_ms: i64,
    session_id: Option<String>,
    root_session_id: Option<String>,
    model: Option<String>,
    tier: Option<String>,
    provider: Option<String>,
    plan_type: Option<String>,
    tokens: TokenCounts,
}

#[derive(Clone, Debug, Default)]
struct SessionMeta {
    parent_session_id: Option<String>,
    title: Option<String>,
}

#[derive(Clone, Debug)]
struct TurnRecord {
    turn_id: String,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
}

#[derive(Clone, Debug)]
struct QuotaObservation {
    observed_at_ms: i64,
    account_key: Option<String>,
    limit_id: Option<String>,
    window_kind: String,
    used_percent: Option<f64>,
    window_minutes: Option<i64>,
    resets_at_ms: Option<i64>,
    plan_type: Option<String>,
    source: &'static str,
    priority: u8,
}

#[derive(Clone, Debug)]
struct WindowSegment {
    id: String,
    account_key: Option<String>,
    window_kind: String,
    start_at_ms: i64,
    reset_at_ms: Option<i64>,
    window_minutes: Option<i64>,
    plan_type: Option<String>,
    ordinal: usize,
}

#[derive(Clone, Debug, Default)]
struct AccountContext {
    account_key: Option<String>,
    auth_kind: Option<String>,
    plan_type: Option<String>,
    daily_tokens: HashMap<String, i64>,
}

#[derive(Clone, Debug, Default)]
struct MetricAggregate {
    tokens: TokenCounts,
    credit_micros: i64,
    api_usd_micros: i64,
    has_usage: bool,
    credit_known: bool,
    api_known: bool,
    quality: BTreeSet<String>,
}

#[derive(Clone, Debug)]
struct OfficialPoint {
    percent: f64,
    source: &'static str,
    observed_at_ms: i64,
    priority: u8,
}

#[derive(Clone, Debug, Default)]
struct MinuteBucket {
    minute_start_ms: i64,
    local_date: String,
    window_id: Option<String>,
    window_start_ms: Option<i64>,
    resets_at_ms: Option<i64>,
    reset_marker: bool,
    account_key: Option<String>,
    plan_type: Option<String>,
    provider: Option<String>,
    metrics: MetricAggregate,
    official: Option<OfficialPoint>,
}

#[derive(Clone, Debug, Default)]
struct DayAggregate {
    local_date: String,
    metrics: MetricAggregate,
    first_official: Option<f64>,
    last_official: Option<f64>,
    official_delta: f64,
    reset_count: i64,
    last_window_id: Option<String>,
    quality: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct SessionAggregate {
    local_date: String,
    root_session_id: Option<String>,
    session_id: Option<String>,
    turn_id: Option<String>,
    title: Option<String>,
    relation: Option<String>,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
    window_id: Option<String>,
    account_key: Option<String>,
    plan_type: Option<String>,
    provider: Option<String>,
    metrics: MetricAggregate,
    models: BTreeMap<(String, String), MetricAggregate>,
    tiers: BTreeSet<String>,
    official_start: Option<f64>,
    official_end: Option<f64>,
    quality: BTreeSet<String>,
}

impl MetricAggregate {
    fn add(&mut self, tokens: &TokenCounts, price: &pricing::Price) {
        if !self.has_usage {
            self.credit_known = true;
            self.api_known = true;
        }
        self.has_usage = true;
        self.tokens.add_assign(tokens);
        if let Some(value) = price.credit_micros {
            self.credit_micros = self.credit_micros.saturating_add(value);
        } else {
            self.credit_known = false;
        }
        if let Some(value) = price.api_usd_micros {
            self.api_usd_micros = self.api_usd_micros.saturating_add(value);
        } else {
            self.api_known = false;
        }
        self.quality.extend(price.quality.iter().cloned());
    }

    fn merge(&mut self, other: &MetricAggregate) {
        if !other.has_usage {
            return;
        }
        if !self.has_usage {
            self.credit_known = true;
            self.api_known = true;
        }
        self.has_usage = true;
        self.tokens.add_assign(&other.tokens);
        self.credit_micros = self.credit_micros.saturating_add(other.credit_micros);
        self.api_usd_micros = self.api_usd_micros.saturating_add(other.api_usd_micros);
        self.credit_known &= other.credit_known;
        self.api_known &= other.api_known;
        self.quality.extend(other.quality.iter().cloned());
    }

    fn credit(&self) -> Option<f64> {
        (self.has_usage && self.credit_known).then(|| self.credit_micros as f64 / 1_000_000.0)
    }

    fn api_usd(&self) -> Option<f64> {
        (self.has_usage && self.api_known).then(|| self.api_usd_micros as f64 / 1_000_000.0)
    }
}

/// Rebuild the page-facing result tables from the three source tables.
pub async fn refresh_rollups(database: &Database) -> Result<RollupSummary, RollupError> {
    let timezone = configured_timezone();
    let usages = load_usages(database).await?;
    let sessions = load_sessions(database).await?;
    let turns = load_turns(database).await?;
    let quotas = load_quotas(database).await?;
    let account = load_account_context(database).await?;
    let windows = build_windows(&quotas);

    let mut minutes: BTreeMap<(i64, String), MinuteBucket> = BTreeMap::new();
    for quota in &quotas {
        let Some(percent) = quota.used_percent else {
            continue;
        };
        let segment = find_window(&windows, quota.observed_at_ms);
        let window_id = segment.map(|value| value.id.clone());
        let key = (
            minute_start(quota.observed_at_ms),
            window_key(window_id.as_deref()),
        );
        let bucket = minutes.entry(key.clone()).or_insert_with(|| MinuteBucket {
            minute_start_ms: key.0,
            local_date: local_date(key.0, &timezone).unwrap_or_else(|_| "unknown".to_owned()),
            window_id: window_id.clone(),
            window_start_ms: segment.map(|value| value.start_at_ms),
            resets_at_ms: segment.and_then(|value| value.reset_at_ms),
            reset_marker: segment.is_some_and(|value| {
                value.ordinal > 0 && value.start_at_ms == quota.observed_at_ms
            }),
            account_key: quota
                .account_key
                .clone()
                .or_else(|| account.account_key.clone()),
            plan_type: quota
                .plan_type
                .clone()
                .or_else(|| account.plan_type.clone()),
            provider: None,
            metrics: MetricAggregate::default(),
            official: None,
        });
        if bucket.official.as_ref().is_none_or(|current| {
            quota.priority > current.priority
                || (quota.priority == current.priority
                    && quota.observed_at_ms >= current.observed_at_ms)
        }) {
            bucket.official = Some(OfficialPoint {
                percent,
                source: quota.source,
                observed_at_ms: quota.observed_at_ms,
                priority: quota.priority,
            });
        }
        if segment
            .is_some_and(|value| value.ordinal > 0 && value.start_at_ms == quota.observed_at_ms)
        {
            bucket.reset_marker = true;
        }
    }

    let mut session_rollups: BTreeMap<String, SessionAggregate> = BTreeMap::new();
    for usage in &usages {
        let minute = minute_start(usage.observed_at_ms);
        let segment = find_window(&windows, usage.observed_at_ms);
        let window_id = segment.map(|value| value.id.clone());
        let key = (minute, window_key(window_id.as_deref()));
        let bucket = minutes.entry(key.clone()).or_insert_with(|| MinuteBucket {
            minute_start_ms: minute,
            local_date: local_date(minute, &timezone).unwrap_or_else(|_| "unknown".to_owned()),
            window_id: window_id.clone(),
            window_start_ms: segment.map(|value| value.start_at_ms),
            resets_at_ms: segment.and_then(|value| value.reset_at_ms),
            reset_marker: segment
                .is_some_and(|value| value.ordinal > 0 && value.start_at_ms == minute),
            account_key: account.account_key.clone(),
            plan_type: usage
                .plan_type
                .clone()
                .or_else(|| account.plan_type.clone()),
            provider: usage.provider.clone(),
            metrics: MetricAggregate::default(),
            official: None,
        });
        bucket.account_key = bucket
            .account_key
            .clone()
            .or_else(|| account.account_key.clone());
        bucket.plan_type = bucket
            .plan_type
            .clone()
            .or_else(|| usage.plan_type.clone())
            .or_else(|| account.plan_type.clone());
        bucket.provider = bucket.provider.clone().or_else(|| usage.provider.clone());
        let price = pricing::price(
            &usage.tokens,
            usage.model.as_deref(),
            usage.tier.as_deref(),
            usage.observed_at_ms,
        );
        bucket.metrics.add(&usage.tokens, &price);

        let session_id = usage
            .session_id
            .clone()
            .unwrap_or_else(|| "unknown-session".to_owned());
        let turn = find_turn(&turns, &session_id, usage.observed_at_ms);
        let turn_id = turn.map(|value| value.turn_id.clone());
        let root_session_id = usage
            .root_session_id
            .clone()
            .unwrap_or_else(|| resolve_root(&session_id, &sessions));
        let title = sessions
            .get(&root_session_id)
            .and_then(|value| value.title.clone())
            .or_else(|| {
                sessions
                    .get(&session_id)
                    .and_then(|value| value.title.clone())
            });
        let relation = if root_session_id == session_id {
            "main"
        } else {
            "fork"
        };
        let local_date = local_date(usage.observed_at_ms, &timezone)?;
        let turn_key = turn_id.clone().unwrap_or_else(|| usage.source_key.clone());
        let session_key = format!(
            "{}|{}|{}|{}|{}",
            local_date,
            root_session_id,
            session_id,
            turn_key,
            window_key(window_id.as_deref())
        );
        let rollup = session_rollups
            .entry(session_key)
            .or_insert_with(|| SessionAggregate {
                local_date: local_date.clone(),
                root_session_id: Some(root_session_id.clone()),
                session_id: Some(session_id.clone()),
                turn_id: turn_id.clone(),
                title,
                relation: Some(relation.to_owned()),
                started_at_ms: turn.and_then(|value| value.started_at_ms),
                ended_at_ms: turn.and_then(|value| value.ended_at_ms),
                window_id: window_id.clone(),
                account_key: account.account_key.clone(),
                plan_type: usage
                    .plan_type
                    .clone()
                    .or_else(|| account.plan_type.clone()),
                provider: usage.provider.clone(),
                metrics: MetricAggregate::default(),
                models: BTreeMap::new(),
                tiers: BTreeSet::new(),
                official_start: None,
                official_end: None,
                quality: BTreeSet::new(),
            });
        rollup.metrics.add(&usage.tokens, &price);
        if turn.is_none() {
            rollup.quality.insert("missing_turn_boundary".to_owned());
        }
        if let Some(model) = usage.model.clone() {
            let tier = usage.tier.clone().unwrap_or_else(|| "unknown".to_owned());
            rollup.tiers.insert(tier.clone());
            rollup
                .models
                .entry((model, tier))
                .or_default()
                .add(&usage.tokens, &price);
        } else {
            rollup.quality.insert("missing_model".to_owned());
        }
        let official = minutes.get(&key).and_then(|value| value.official.as_ref());
        if let Some(point) = official {
            rollup.official_start = Some(rollup.official_start.unwrap_or(point.percent));
            rollup.official_end = Some(point.percent);
        }
    }

    let mut days: BTreeMap<String, DayAggregate> = BTreeMap::new();
    for bucket in minutes.values() {
        let day = days
            .entry(bucket.local_date.clone())
            .or_insert_with(|| DayAggregate {
                local_date: bucket.local_date.clone(),
                ..DayAggregate::default()
            });
        day.metrics.merge(&bucket.metrics);
        day.quality.extend(bucket.metrics.quality.iter().cloned());
        if bucket.reset_marker {
            day.reset_count += 1;
        }
        if let Some(point) = &bucket.official {
            day.first_official = Some(day.first_official.unwrap_or(point.percent));
            let previous = day.last_official;
            if day.last_window_id.as_deref() == bucket.window_id.as_deref()
                && !bucket.reset_marker
                && previous.is_some_and(|value| point.percent >= value)
            {
                day.official_delta += point.percent - previous.unwrap_or(point.percent);
            }
            day.last_window_id = bucket.window_id.clone();
            day.last_official = Some(point.percent);
        }
    }

    persist_rollups(database, &days, &minutes, &session_rollups, &account).await?;
    Ok(RollupSummary {
        days: days.len(),
        minutes: minutes.len(),
        sessions: session_rollups.len(),
        windows: windows.len(),
    })
}

async fn load_usages(database: &Database) -> Result<Vec<UsageRecord>, RollupError> {
    let rows = sqlx::query(
        "SELECT source_key, observed_at_ms, session_id, root_session_id, model,
                service_tier, provider, plan_type, input_tokens, cache_read_tokens,
                cache_write_tokens, output_tokens, reasoning_tokens, total_tokens
         FROM source_jsonl WHERE kind = 'usage' ORDER BY observed_at_ms, id",
    )
    .fetch_all(database.pool())
    .await?;
    rows.into_iter()
        .map(|row| {
            let input = optional_i64(&row, "input_tokens")?.unwrap_or_default();
            let cached = optional_i64(&row, "cache_read_tokens")?.unwrap_or_default();
            let cache_write = optional_i64(&row, "cache_write_tokens")?.unwrap_or_default();
            let output = optional_i64(&row, "output_tokens")?.unwrap_or_default();
            let reasoning = optional_i64(&row, "reasoning_tokens")?.unwrap_or_default();
            let total = optional_i64(&row, "total_tokens")?
                .unwrap_or_else(|| input.saturating_add(output).saturating_add(reasoning));
            Ok(UsageRecord {
                source_key: row.try_get("source_key")?,
                observed_at_ms: row.try_get("observed_at_ms")?,
                session_id: row.try_get("session_id")?,
                root_session_id: row.try_get("root_session_id")?,
                model: row.try_get("model")?,
                tier: row.try_get("service_tier")?,
                provider: row.try_get("provider")?,
                plan_type: row.try_get("plan_type")?,
                tokens: TokenCounts {
                    input,
                    cached,
                    cache_write,
                    output,
                    reasoning,
                    total,
                },
            })
        })
        .collect::<Result<Vec<_>, RollupError>>()
}

async fn load_sessions(database: &Database) -> Result<HashMap<String, SessionMeta>, RollupError> {
    let rows = sqlx::query(
        "SELECT session_id, root_session_id, title FROM source_jsonl
         WHERE kind = 'session' AND session_id IS NOT NULL ORDER BY observed_at_ms, id",
    )
    .fetch_all(database.pool())
    .await?;
    let mut sessions = HashMap::new();
    for row in rows {
        let session_id: String = row.try_get("session_id")?;
        sessions.insert(
            session_id,
            SessionMeta {
                parent_session_id: row.try_get("root_session_id")?,
                title: row.try_get("title")?,
            },
        );
    }
    Ok(sessions)
}

async fn load_turns(database: &Database) -> Result<HashMap<String, Vec<TurnRecord>>, RollupError> {
    let rows = sqlx::query(
        "SELECT session_id, turn_id, started_at_ms, ended_at_ms FROM source_jsonl
         WHERE kind = 'turn' AND session_id IS NOT NULL AND turn_id IS NOT NULL
         ORDER BY started_at_ms, id",
    )
    .fetch_all(database.pool())
    .await?;
    let mut turns: HashMap<String, Vec<TurnRecord>> = HashMap::new();
    for row in rows {
        let session_id: String = row.try_get("session_id")?;
        turns.entry(session_id).or_default().push(TurnRecord {
            turn_id: row.try_get("turn_id")?,
            started_at_ms: row.try_get("started_at_ms")?,
            ended_at_ms: row.try_get("ended_at_ms")?,
        });
    }
    Ok(turns)
}

async fn load_quotas(database: &Database) -> Result<Vec<QuotaObservation>, RollupError> {
    let mut quotas = Vec::new();
    let rows = sqlx::query(
        "SELECT observed_at_ms, limit_id, window_kind, used_percent,
                window_minutes, resets_at_ms, plan_type FROM source_jsonl
         WHERE kind = 'quota' ORDER BY observed_at_ms, id",
    )
    .fetch_all(database.pool())
    .await?;
    for row in rows {
        quotas.push(QuotaObservation {
            observed_at_ms: row.try_get("observed_at_ms")?,
            account_key: None,
            limit_id: row.try_get("limit_id")?,
            window_kind: row
                .try_get::<Option<String>, _>("window_kind")?
                .unwrap_or_else(|| "primary".to_owned()),
            used_percent: row.try_get("used_percent")?,
            window_minutes: row.try_get("window_minutes")?,
            resets_at_ms: row.try_get("resets_at_ms")?,
            plan_type: row.try_get("plan_type")?,
            source: "jsonl",
            priority: 1,
        });
    }
    let rows = sqlx::query(
        "SELECT first_seen_at_ms, account_key, limit_id, window_kind,
                used_percent, window_minutes, resets_at_ms, plan_type
         FROM source_app_server WHERE kind = 'quota' ORDER BY first_seen_at_ms, id",
    )
    .fetch_all(database.pool())
    .await?;
    for row in rows {
        quotas.push(QuotaObservation {
            observed_at_ms: row.try_get("first_seen_at_ms")?,
            account_key: row.try_get("account_key")?,
            limit_id: row.try_get("limit_id")?,
            window_kind: row
                .try_get::<Option<String>, _>("window_kind")?
                .unwrap_or_else(|| "primary".to_owned()),
            used_percent: row.try_get("used_percent")?,
            window_minutes: row.try_get("window_minutes")?,
            resets_at_ms: row.try_get("resets_at_ms")?,
            plan_type: row.try_get("plan_type")?,
            source: "app_server",
            priority: 2,
        });
    }
    quotas.sort_by_key(|value| value.observed_at_ms);
    Ok(quotas)
}

async fn load_account_context(database: &Database) -> Result<AccountContext, RollupError> {
    let mut context = sqlx::query(
        "SELECT account_key, auth_kind, plan_type FROM source_app_server
         WHERE kind = 'account' ORDER BY last_seen_at_ms DESC, id DESC LIMIT 1",
    )
    .fetch_optional(database.pool())
    .await?
    .map(|row| AccountContext {
        account_key: row.try_get("account_key").ok().flatten(),
        auth_kind: row.try_get("auth_kind").ok().flatten(),
        plan_type: row.try_get("plan_type").ok().flatten(),
        daily_tokens: HashMap::new(),
    })
    .unwrap_or_default();
    let rows = sqlx::query(
        "SELECT first_seen_at_ms, daily_tokens_json FROM source_app_server
         WHERE kind = 'usage' AND daily_tokens_json IS NOT NULL
         ORDER BY last_seen_at_ms, id",
    )
    .fetch_all(database.pool())
    .await?;
    for row in rows {
        let json_value: Value =
            serde_json::from_str(&row.try_get::<String, _>("daily_tokens_json")?)?;
        let Some(items) = json_value.as_array() else {
            continue;
        };
        for item in items {
            let Some(date) = item
                .get("startDate")
                .or_else(|| item.get("start_date"))
                .or_else(|| item.get("date"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some(tokens) = item
                .get("tokens")
                .or_else(|| item.get("totalTokens"))
                .and_then(Value::as_i64)
            else {
                continue;
            };
            context.daily_tokens.insert(date.to_owned(), tokens);
        }
    }
    Ok(context)
}

fn build_windows(quotas: &[QuotaObservation]) -> Vec<WindowSegment> {
    let mut groups: BTreeMap<String, Vec<&QuotaObservation>> = BTreeMap::new();
    for quota in quotas {
        let group = format!(
            "{}:{}",
            quota.limit_id.as_deref().unwrap_or("unknown"),
            quota.window_kind
        );
        groups.entry(group).or_default().push(quota);
    }
    let mut windows = Vec::new();
    for (group, mut values) in groups {
        values.sort_by_key(|value| (value.observed_at_ms, value.priority));
        let mut current: Option<WindowSegment> = None;
        let mut previous_percent = None;
        let mut previous_reset_at_ms: Option<i64> = None;
        let mut ordinal = 0usize;
        for quota in values {
            let percent_reset = previous_percent
                .zip(quota.used_percent)
                .is_some_and(|(previous, current)| current + RESET_DROP_PERCENT < previous);
            let reset_time_changed =
                previous_reset_at_ms
                    .zip(quota.resets_at_ms)
                    .is_some_and(|(previous, current)| {
                        (current - previous).abs() > RESET_TIME_JITTER_MS
                    });
            let reset = percent_reset || reset_time_changed;
            if current.is_none() || reset {
                if let Some(value) = current.take() {
                    windows.push(value);
                }
                let id = format!("window:{}:{}", group, ordinal);
                current = Some(WindowSegment {
                    id,
                    account_key: quota.account_key.clone(),
                    window_kind: quota.window_kind.clone(),
                    start_at_ms: quota.observed_at_ms,
                    reset_at_ms: quota.resets_at_ms,
                    window_minutes: quota.window_minutes,
                    plan_type: quota.plan_type.clone(),
                    ordinal,
                });
                ordinal += 1;
            } else if let Some(value) = current.as_mut() {
                value.account_key = value
                    .account_key
                    .clone()
                    .or_else(|| quota.account_key.clone());
                value.reset_at_ms = quota.resets_at_ms.or(value.reset_at_ms);
                value.window_minutes = quota.window_minutes.or(value.window_minutes);
                value.plan_type = value.plan_type.clone().or_else(|| quota.plan_type.clone());
            }
            previous_percent = quota.used_percent.or(previous_percent);
            previous_reset_at_ms = quota.resets_at_ms.or(previous_reset_at_ms);
        }
        if let Some(value) = current {
            windows.push(value);
        }
    }
    windows.sort_by_key(|value| value.start_at_ms);
    windows
}

fn find_window(windows: &[WindowSegment], observed_at_ms: i64) -> Option<&WindowSegment> {
    let primary: Vec<&WindowSegment> = windows
        .iter()
        .filter(|value| value.window_kind == "primary")
        .collect();
    let candidates = if primary.is_empty() {
        windows.iter().collect::<Vec<_>>()
    } else {
        primary
    };
    candidates
        .iter()
        .filter(|value| value.start_at_ms <= observed_at_ms)
        .max_by_key(|value| value.start_at_ms)
        .copied()
        .or_else(|| {
            candidates
                .iter()
                .min_by_key(|value| value.start_at_ms)
                .copied()
        })
}

fn resolve_root(session_id: &str, sessions: &HashMap<String, SessionMeta>) -> String {
    let mut current = session_id.to_owned();
    let mut seen = HashSet::new();
    while seen.insert(current.clone()) {
        let Some(parent) = sessions
            .get(&current)
            .and_then(|value| value.parent_session_id.clone())
        else {
            break;
        };
        current = parent;
    }
    current
}

fn find_turn<'a>(
    turns: &'a HashMap<String, Vec<TurnRecord>>,
    session_id: &str,
    observed_at_ms: i64,
) -> Option<&'a TurnRecord> {
    let values = turns.get(session_id)?;
    values
        .iter()
        .filter(|turn| {
            turn.started_at_ms
                .is_some_and(|start| start <= observed_at_ms)
        })
        .filter(|turn| turn.ended_at_ms.is_none_or(|end| observed_at_ms <= end))
        .max_by_key(|turn| turn.started_at_ms)
        .or_else(|| {
            values
                .iter()
                .filter(|turn| {
                    turn.started_at_ms
                        .is_some_and(|start| start <= observed_at_ms)
                })
                .max_by_key(|turn| turn.started_at_ms)
        })
}

async fn persist_rollups(
    database: &Database,
    days: &BTreeMap<String, DayAggregate>,
    minutes: &BTreeMap<(i64, String), MinuteBucket>,
    sessions: &BTreeMap<String, SessionAggregate>,
    account: &AccountContext,
) -> Result<(), RollupError> {
    let mut transaction = database.pool().begin().await?;
    sqlx::query("DELETE FROM usage_daily")
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM usage_minute")
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM usage_session")
        .execute(&mut *transaction)
        .await?;

    for day in days.values() {
        let account_tokens = account.daily_tokens.get(&day.local_date).copied();
        let local_total = day.metrics.has_usage.then_some(day.metrics.tokens.total);
        let unobserved = account_tokens
            .zip(local_total)
            .map(|(remote, local)| remote.saturating_sub(local));
        let coverage = account_tokens
            .zip(local_total)
            .and_then(|(remote, local)| (remote > 0).then(|| local as f64 / remote as f64));
        let mut quality = day.quality.clone();
        if day.first_official.is_none() {
            quality.insert("official_unavailable".to_owned());
        }
        if account_tokens.is_none() {
            quality.insert("account_tokens_unavailable".to_owned());
        }
        sqlx::query(
            "INSERT INTO usage_daily
             (local_date, account_key, auth_kind, plan_type, capacity_profile,
              input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
              reasoning_tokens, total_tokens, credit, api_usd, local_percent,
              account_tokens, unobserved_tokens, coverage_ratio, account_token_freshness,
              official_percent_start, official_percent_end, official_percent_delta,
              reset_count, quality)
             VALUES (?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&day.local_date)
        .bind(&account.account_key)
        .bind(&account.auth_kind)
        .bind(&account.plan_type)
        .bind(optional_metric(&day.metrics, |value| value.input))
        .bind(optional_metric(&day.metrics, |value| value.cached))
        .bind(optional_metric(&day.metrics, |value| value.cache_write))
        .bind(optional_metric(&day.metrics, |value| value.output))
        .bind(optional_metric(&day.metrics, |value| value.reasoning))
        .bind(optional_metric(&day.metrics, |value| value.total))
        .bind(day.metrics.credit())
        .bind(day.metrics.api_usd())
        .bind(account_tokens)
        .bind(unobserved)
        .bind(coverage)
        .bind(account_tokens.map(|_| "settled").or(Some("unavailable")))
        .bind(day.first_official)
        .bind(day.last_official)
        .bind((day.official_delta > 0.0).then_some(day.official_delta))
        .bind(day.reset_count)
        .bind((!quality.is_empty()).then(|| quality.into_iter().collect::<Vec<_>>().join(",")))
        .execute(&mut *transaction)
        .await?;
    }

    for bucket in minutes.values() {
        let quality = (!bucket.metrics.quality.is_empty()).then(|| {
            bucket
                .metrics
                .quality
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        });
        let official_source = bucket.official.as_ref().map(|value| value.source);
        sqlx::query(
            "INSERT INTO usage_minute
             (bucket_key, minute_start_ms, local_date, account_key, auth_kind,
              plan_type, provider, capacity_profile, window_id, window_start_ms,
              resets_at_ms, reset_marker, input_tokens, cache_read_tokens,
              cache_write_tokens, output_tokens, reasoning_tokens, total_tokens,
              credit, api_usd, official_used_percent, official_source, quality)
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(format!(
            "{}:{}",
            bucket.minute_start_ms,
            window_key(bucket.window_id.as_deref())
        ))
        .bind(bucket.minute_start_ms)
        .bind(&bucket.local_date)
        .bind(&bucket.account_key)
        .bind(&account.auth_kind)
        .bind(&bucket.plan_type)
        .bind(&bucket.provider)
        .bind(&bucket.window_id)
        .bind(bucket.window_start_ms)
        .bind(bucket.resets_at_ms)
        .bind(i64::from(bucket.reset_marker))
        .bind(optional_metric(&bucket.metrics, |value| value.input))
        .bind(optional_metric(&bucket.metrics, |value| value.cached))
        .bind(optional_metric(&bucket.metrics, |value| value.cache_write))
        .bind(optional_metric(&bucket.metrics, |value| value.output))
        .bind(optional_metric(&bucket.metrics, |value| value.reasoning))
        .bind(optional_metric(&bucket.metrics, |value| value.total))
        .bind(bucket.metrics.credit())
        .bind(bucket.metrics.api_usd())
        .bind(bucket.official.as_ref().map(|value| value.percent))
        .bind(official_source)
        .bind(quality)
        .execute(&mut *transaction)
        .await?;
    }

    for (row_key, session) in sessions {
        let model_json = if session.models.is_empty() {
            None
        } else {
            Some(serde_json::to_string(
                &session
                    .models
                    .iter()
                    .map(|((model, tier), metrics)| {
                        json!({
                            "model": model,
                            "tier": tier,
                            "tokens": metrics.tokens.total,
                            "credit": metrics.credit(),
                            "api_usd": metrics.api_usd()
                        })
                    })
                    .collect::<Vec<_>>(),
            )?)
        };
        let fast_state = match session.tiers.len() {
            0 => Some("unknown"),
            1 if session.tiers.contains("fast") => Some("fast"),
            1 => Some("standard"),
            _ => Some("mixed"),
        };
        let mut quality = session.quality.clone();
        quality.extend(session.metrics.quality.iter().cloned());
        sqlx::query(
            "INSERT INTO usage_session
             (row_key, local_date, root_session_id, session_id, turn_id, title,
              relation, started_at_ms, ended_at_ms, window_id, account_key,
              auth_kind, plan_type, provider, capacity_profile, primary_model,
              fast_state, model_breakdown_json, input_tokens, cache_read_tokens,
              cache_write_tokens, output_tokens, reasoning_tokens, total_tokens,
              credit, api_usd, official_percent_start, official_percent_end, quality)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(row_key)
        .bind(&session.local_date)
        .bind(&session.root_session_id)
        .bind(&session.session_id)
        .bind(&session.turn_id)
        .bind(&session.title)
        .bind(&session.relation)
        .bind(session.started_at_ms)
        .bind(session.ended_at_ms)
        .bind(&session.window_id)
        .bind(&session.account_key)
        .bind(&account.auth_kind)
        .bind(&session.plan_type)
        .bind(&session.provider)
        .bind(primary_model(session))
        .bind(fast_state)
        .bind(model_json)
        .bind(optional_metric(&session.metrics, |value| value.input))
        .bind(optional_metric(&session.metrics, |value| value.cached))
        .bind(optional_metric(&session.metrics, |value| value.cache_write))
        .bind(optional_metric(&session.metrics, |value| value.output))
        .bind(optional_metric(&session.metrics, |value| value.reasoning))
        .bind(optional_metric(&session.metrics, |value| value.total))
        .bind(session.metrics.credit())
        .bind(session.metrics.api_usd())
        .bind(session.official_start)
        .bind(session.official_end)
        .bind((!quality.is_empty()).then(|| quality.into_iter().collect::<Vec<_>>().join(",")))
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

fn primary_model(session: &SessionAggregate) -> Option<String> {
    (session.models.len() == 1)
        .then(|| session.models.keys().next().map(|value| value.0.clone()))
        .flatten()
}

fn optional_metric<F>(metrics: &MetricAggregate, field: F) -> Option<i64>
where
    F: FnOnce(&TokenCounts) -> i64,
{
    metrics.has_usage.then(|| field(&metrics.tokens))
}

fn optional_i64(row: &sqlx::sqlite::SqliteRow, name: &str) -> Result<Option<i64>, sqlx::Error> {
    row.try_get(name)
}

fn window_key(window_id: Option<&str>) -> String {
    window_id.unwrap_or("none").to_owned()
}

fn minute_start(value: i64) -> i64 {
    value - value.rem_euclid(60_000)
}

fn local_date(epoch_ms: i64, timezone: &str) -> Result<String, RollupError> {
    let utc = OffsetDateTime::from_unix_timestamp_nanos(i128::from(epoch_ms) * 1_000_000)
        .map_err(|error| RollupError::Timestamp(error.to_string()))?;
    let offset = if timezone.eq_ignore_ascii_case("UTC") {
        UtcOffset::UTC
    } else {
        UtcOffset::from_hms(8, 0, 0).map_err(|error| RollupError::Timestamp(error.to_string()))?
    };
    utc.to_offset(offset)
        .format(&time::macros::format_description!("[year]-[month]-[day]"))
        .map_err(|error| RollupError::Timestamp(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::minimal::JsonlCollector;

    fn temp_home() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("codex-meter-minimal-rollup-{nonce}"))
    }

    #[test]
    fn reset_windows_use_reset_time_change_but_ignore_small_jitter() {
        let quota = |observed_at_ms, used_percent, resets_at_ms| QuotaObservation {
            observed_at_ms,
            account_key: None,
            limit_id: Some("codex".to_owned()),
            window_kind: "primary".to_owned(),
            used_percent: Some(used_percent),
            window_minutes: Some(10_080),
            resets_at_ms: Some(resets_at_ms),
            plan_type: Some("plus".to_owned()),
            source: "jsonl",
            priority: 1,
        };
        let windows = build_windows(&[
            quota(1_000_000, 10.0, 2_000_000),
            quota(1_060_000, 11.0, 2_001_000),
            quota(1_120_000, 12.0, 2_600_000),
        ]);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].ordinal, 0);
        assert_eq!(windows[1].ordinal, 1);
    }

    #[tokio::test]
    async fn rebuilds_daily_minute_and_session_tables_from_fixture() {
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
        let summary = refresh_rollups(&database).await.unwrap();
        assert!(summary.days > 0);
        assert!(summary.minutes > 0);
        assert!(summary.sessions > 0);
        let daily: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_daily")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let minute: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_minute")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let session: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_session")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(daily as usize, summary.days);
        assert_eq!(minute as usize, summary.minutes);
        assert_eq!(session as usize, summary.sessions);
        let first_tokens: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(total_tokens), 0) FROM usage_minute")
                .fetch_one(database.pool())
                .await
                .unwrap();
        let second = refresh_rollups(&database).await.unwrap();
        let second_tokens: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(total_tokens), 0) FROM usage_minute")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(second.days, summary.days);
        assert_eq!(second.minutes, summary.minutes);
        assert_eq!(second.sessions, summary.sessions);
        assert_eq!(second.windows, summary.windows);
        assert_eq!(second_tokens, first_tokens);
        let _ = fs::remove_dir_all(root);
    }
}
