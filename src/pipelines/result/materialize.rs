//! Second-batch Pipeline.
//!
//! This module is intentionally rebuildable. It reads the three source tables,
//! applies the date/window/session rules once, and atomically replaces the
//! four page-facing tables.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{json, Value};
use sqlx::Row;
use thiserror::Error;

use crate::{
    db::{
        Database, DbError, UsageDailyRecord, UsageMinuteRecord, UsageSessionRecord,
        UsageWindowRecord,
    },
    pricing::{self, TokenCounts},
};

const RESET_DROP_PERCENT: f64 = 5.0;
const RESET_TIME_JITTER_MS: i64 = 5 * 60 * 1_000;

#[derive(Debug, Error)]
pub enum MaterializeError {
    #[error("database error: {0}")]
    Database(#[from] DbError),
    #[error("database query error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("timestamp error: {0}")]
    Timestamp(String),
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct MaterializeSummary {
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
    turn_id: Option<String>,
    model: Option<String>,
    tier: Option<String>,
    reasoning_effort: Option<String>,
    relation: Option<String>,
    provider: Option<String>,
    plan_type: Option<String>,
    tokens: TokenCounts,
}

#[derive(Clone, Debug, Default)]
struct SessionMeta {
    title: Option<String>,
    relation: Option<String>,
}

#[derive(Clone, Debug)]
struct TurnRecord {
    id: String,
    started: Option<i64>,
    ended: Option<i64>,
}

#[derive(Clone, Debug)]
struct QuotaObservation {
    first_seen_at_ms: i64,
    last_seen_at_ms: i64,
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

#[derive(Clone, Debug, Default)]
struct WindowSegment {
    id: String,
    account_key: Option<String>,
    limit_id: Option<String>,
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
    provider: Option<String>,
    plan_type: Option<String>,
    daily_tokens: HashMap<String, i64>,
}

#[derive(Clone, Debug, Default)]
struct Metric {
    tokens: TokenCounts,
    credit_micros: i64,
    api_usd_micros: i64,
    has_usage: bool,
    credit_known: bool,
    api_known: bool,
    credit_partial: bool,
    api_partial: bool,
    quality: BTreeSet<String>,
}

impl Metric {
    fn add(&mut self, tokens: &TokenCounts, price: &pricing::Price) {
        self.has_usage = true;
        self.tokens.add_assign(tokens);
        if let Some(value) = price.credit_micros {
            self.credit_known = true;
            self.credit_micros = self.credit_micros.saturating_add(value);
        } else {
            self.credit_partial = true;
        }
        if let Some(value) = price.api_usd_micros {
            self.api_known = true;
            self.api_usd_micros = self.api_usd_micros.saturating_add(value);
        } else {
            self.api_partial = true;
        }
        self.quality.extend(price.quality.iter().cloned());
    }
    fn credit(&self) -> Option<f64> {
        (self.has_usage && self.credit_known).then(|| self.credit_micros as f64 / 1_000_000.0)
    }
    fn api_usd(&self) -> Option<f64> {
        (self.has_usage && self.api_known).then(|| self.api_usd_micros as f64 / 1_000_000.0)
    }

    fn quality_flags(&self) -> Option<&'static str> {
        (self.credit_partial || self.api_partial).then_some("pricing_partial")
    }
}

#[derive(Clone, Debug, Default)]
struct OfficialPoint {
    percent: f64,
    source: &'static str,
    priority: u8,
}

#[derive(Clone, Debug, Default)]
struct MinuteBucket {
    minute_start_ms: i64,
    local_date: String,
    window_id: Option<String>,
    window_kind: Option<String>,
    window_start_ms: Option<i64>,
    resets_at_ms: Option<i64>,
    reset_marker: bool,
    account_key: Option<String>,
    plan_type: Option<String>,
    provider: Option<String>,
    metric: Metric,
    official: Option<OfficialPoint>,
    quality: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct DayAggregate {
    local_date: String,
    metric: Metric,
    official_start: Option<f64>,
    official_end: Option<f64>,
    official_delta: f64,
    reset_count: i64,
    plans: BTreeMap<String, PlanAggregate>,
    quality: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct PlanAggregate {
    plan_type: Option<String>,
    capacity_profile: Option<String>,
    metric: Metric,
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
    metric: Metric,
    models: BTreeMap<(String, String, String), Metric>,
    tiers: BTreeSet<String>,
    observed_start_ms: Option<i64>,
    observed_end_ms: Option<i64>,
    quality: BTreeSet<String>,
}

#[derive(Clone, Debug, Default)]
struct WindowAggregate {
    segment: WindowSegment,
    metric: Metric,
    plans: BTreeMap<String, PlanAggregate>,
    official_start: Option<f64>,
    official_end: Option<f64>,
    official_delta: f64,
    quality: BTreeSet<String>,
}

#[derive(Clone, Debug)]
struct Capacity {
    profile: String,
    account_key: Option<String>,
    plan_type: Option<String>,
    credit: f64,
    from: i64,
    to: Option<i64>,
}

pub async fn refresh_rollups(database: &Database) -> Result<MaterializeSummary, MaterializeError> {
    let (raw_usages, session_meta, turns) = load_jsonl(database).await?;
    let usages = dedupe_usages(raw_usages);
    let (mut quotas, account) = load_account_and_quotas(database).await?;
    let capacities = load_capacities(database).await?;
    quotas.sort_by_key(|quota| {
        (
            quota.first_seen_at_ms,
            quota.last_seen_at_ms,
            quota.priority,
        )
    });
    let windows = build_windows(&quotas);
    let mut minutes: BTreeMap<(i64, String), MinuteBucket> = BTreeMap::new();
    let mut days: BTreeMap<String, DayAggregate> = BTreeMap::new();
    let mut window_aggregates = windows
        .iter()
        .cloned()
        .map(|segment| {
            let id = segment.id.clone();
            (
                id,
                WindowAggregate {
                    segment,
                    ..WindowAggregate::default()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    for quota in &quotas {
        let segment = find_quota_window(&windows, quota);
        let window_id = segment.map(|value| value.id.clone());
        let minute = minute_start(quota.first_seen_at_ms);
        let key = (
            minute,
            window_id.clone().unwrap_or_else(|| "none".to_owned()),
        );
        let bucket = minutes
            .entry(key.clone())
            .or_insert_with(|| empty_minute(minute, window_id.clone(), segment, &account));
        if quota.used_percent.is_some()
            && bucket
                .official
                .as_ref()
                .is_none_or(|current| quota.priority >= current.priority)
        {
            bucket.official = quota.used_percent.map(|percent| OfficialPoint {
                percent,
                source: quota.source,
                priority: quota.priority,
            });
        }
        if let Some(segment) = segment {
            bucket.account_key = bucket
                .account_key
                .clone()
                .or_else(|| segment.account_key.clone())
                .or_else(|| quota.account_key.clone());
            bucket.plan_type = bucket
                .plan_type
                .clone()
                .or_else(|| segment.plan_type.clone())
                .or_else(|| quota.plan_type.clone());
            bucket.window_kind = Some(segment.window_kind.clone());
            bucket.window_start_ms = Some(segment.start_at_ms);
            bucket.resets_at_ms = segment.reset_at_ms;
            bucket.reset_marker |=
                segment.ordinal > 0 && minute_start(segment.start_at_ms) == minute;
        }
    }

    // Quotas from JSONL and App Server can describe the same minute.  The
    // minute bucket has already selected the higher-priority/latest point, so
    // derive day/window official percentages from those canonical buckets.
    let mut last_official: HashMap<(String, String), f64> = HashMap::new();
    let mut counted_resets = HashSet::new();
    for bucket in minutes.values() {
        let Some(official) = bucket.official.as_ref() else {
            continue;
        };
        let Some(window_id) = bucket.window_id.as_ref() else {
            continue;
        };
        if let Some(window) = window_aggregates.get_mut(window_id) {
            window.official_start.get_or_insert(official.percent);
            if let Some(previous) = window.official_end {
                if official.percent >= previous {
                    window.official_delta += official.percent - previous;
                }
            }
            window.official_end = Some(official.percent);
        }
        if bucket.window_kind.as_deref() != Some("primary") {
            continue;
        }
        let date = bucket.local_date.clone();
        let day = days.entry(date.clone()).or_insert_with(|| DayAggregate {
            local_date: date.clone(),
            ..Default::default()
        });
        day.official_start.get_or_insert(official.percent);
        day.official_end = Some(official.percent);
        let key = (date.clone(), window_id.clone());
        if let Some(previous_percent) = last_official.insert(key.clone(), official.percent) {
            if official.percent >= previous_percent {
                day.official_delta += official.percent - previous_percent;
            }
        }
        if bucket.reset_marker && counted_resets.insert(format!("{}:{}", date, window_id)) {
            day.reset_count += 1;
        }
    }

    let mut sessions: BTreeMap<String, SessionAggregate> = BTreeMap::new();
    for usage in usages {
        let minute = minute_start(usage.observed_at_ms);
        let segment = find_usage_window(&windows, usage.observed_at_ms);
        let window_id = segment.map(|value| value.id.clone());
        let key = (
            minute,
            window_id.clone().unwrap_or_else(|| "none".to_owned()),
        );
        let bucket = minutes
            .entry(key)
            .or_insert_with(|| empty_minute(minute, window_id.clone(), segment, &account));
        bucket.account_key = bucket
            .account_key
            .clone()
            .or_else(|| account.account_key.clone());
        bucket.plan_type = bucket
            .plan_type
            .clone()
            .or_else(|| usage.plan_type.clone())
            .or_else(|| account.plan_type.clone());
        if bucket.plan_type.as_deref().is_some_and(|plan| {
            usage
                .plan_type
                .as_deref()
                .is_some_and(|value| value != plan)
        }) {
            bucket.quality.insert("mixed_plan".to_owned());
        }
        bucket.provider = bucket
            .provider
            .clone()
            .or_else(|| usage.provider.clone())
            .or_else(|| account.provider.clone());
        let price = pricing::price(
            &usage.tokens,
            usage.model.as_deref(),
            usage.tier.as_deref(),
            usage.observed_at_ms,
        );
        bucket.metric.add(&usage.tokens, &price);
        let date = bucket.local_date.clone();
        let day = days.entry(date.clone()).or_insert_with(|| DayAggregate {
            local_date: date.clone(),
            ..Default::default()
        });
        day.metric.add(&usage.tokens, &price);
        day.quality.extend(price.quality.iter().cloned());
        add_plan_metric(&mut day.plans, &usage, &price, &account, &capacities);

        if let Some(window_id) = window_id.as_ref() {
            if let Some(window) = window_aggregates.get_mut(window_id) {
                window.metric.add(&usage.tokens, &price);
                add_plan_metric(&mut window.plans, &usage, &price, &account, &capacities);
                window.quality.extend(price.quality.iter().cloned());
            }
        }

        let session_id = usage
            .session_id
            .clone()
            .unwrap_or_else(|| "unknown-session".to_owned());
        let root_session_id = usage.root_session_id.clone();
        let grouping_root = root_session_id
            .clone()
            .unwrap_or_else(|| session_id.clone());
        let turn = usage
            .turn_id
            .as_deref()
            .and_then(|turn_id| {
                turns
                    .get(&session_id)
                    .and_then(|items| items.iter().find(|item| item.id == turn_id))
            })
            .or_else(|| find_turn(turns.get(&session_id), usage.observed_at_ms));
        let turn_id = usage
            .turn_id
            .clone()
            .or_else(|| turn.map(|turn| turn.id.clone()));
        let boundary_start =
            day_start_ms(&date).max(segment.map(|window| window.start_at_ms).unwrap_or(i64::MIN));
        let boundary_end = day_end_ms(&date).min(
            segment
                .and_then(|window| window.reset_at_ms)
                .unwrap_or(i64::MAX),
        );
        let row_started_at_ms = turn
            .and_then(|value| value.started)
            .or(Some(usage.observed_at_ms))
            .map(|value| value.max(boundary_start));
        let row_ended_at_ms = turn
            .and_then(|value| value.ended)
            .map(|value| value.min(boundary_end));
        let crosses_boundary = turn.is_some_and(|value| {
            value.started.is_some_and(|start| start < boundary_start)
                || value.ended.is_some_and(|end| end > boundary_end)
        });
        let row_key = format!(
            "{}:{}:{}:{}",
            grouping_root,
            turn_id.as_deref().unwrap_or("unknown"),
            date,
            window_id.as_deref().unwrap_or("none")
        );
        let session = sessions.entry(row_key).or_insert_with(|| SessionAggregate {
            local_date: date.clone(),
            root_session_id: root_session_id.clone(),
            session_id: Some(session_id.clone()),
            turn_id: turn_id.clone(),
            title: session_meta
                .get(&grouping_root)
                .and_then(|meta| meta.title.clone())
                .or_else(|| {
                    session_meta
                        .get(&session_id)
                        .and_then(|meta| meta.title.clone())
                }),
            relation: usage
                .relation
                .clone()
                .or_else(|| {
                    session_meta
                        .get(&session_id)
                        .and_then(|meta| meta.relation.clone())
                })
                .or_else(|| {
                    Some(
                        if session_id != grouping_root {
                            "child"
                        } else {
                            "main"
                        }
                        .to_owned(),
                    )
                }),
            started_at_ms: row_started_at_ms,
            ended_at_ms: row_ended_at_ms,
            window_id: window_id.clone(),
            account_key: bucket.account_key.clone(),
            plan_type: usage
                .plan_type
                .clone()
                .or_else(|| bucket.plan_type.clone())
                .or_else(|| account.plan_type.clone()),
            provider: bucket.provider.clone(),
            observed_start_ms: Some(usage.observed_at_ms),
            observed_end_ms: Some(usage.observed_at_ms),
            ..Default::default()
        });
        if usage.root_session_id.is_none() {
            session.quality.insert("root_unresolved".to_owned());
        }
        if crosses_boundary {
            session.quality.insert("split_boundary".to_owned());
        }
        session.metric.add(&usage.tokens, &price);
        session.quality.extend(price.quality.iter().cloned());
        if let Some(model) = usage.model.clone() {
            let tier = usage.tier.clone().unwrap_or_else(|| "unknown".to_owned());
            let effort = usage
                .reasoning_effort
                .clone()
                .unwrap_or_else(|| "unknown".to_owned());
            session.tiers.insert(tier.clone());
            session
                .models
                .entry((model, tier, effort))
                .or_default()
                .add(&usage.tokens, &price);
        } else {
            session.quality.insert("missing_model".to_owned());
        }
        session.started_at_ms = min_option(session.started_at_ms, row_started_at_ms);
        session.ended_at_ms = max_option(session.ended_at_ms, row_ended_at_ms);
        session.observed_start_ms =
            min_option(session.observed_start_ms, Some(usage.observed_at_ms));
        session.observed_end_ms = max_option(session.observed_end_ms, Some(usage.observed_at_ms));
    }

    for day in days.values_mut() {
        if day.plans.len() > 1 {
            day.quality.insert("mixed_plan".to_owned());
        }
        if let Some(flag) = day.metric.quality_flags() {
            day.quality.insert(flag.to_owned());
        }
        if day.metric.credit().is_none() && day.metric.has_usage {
            day.quality.insert("missing_pricing".to_owned());
        }
    }
    let daily_rows = days
        .values()
        .map(|day| to_daily_row(day, &account, &capacities))
        .collect::<Vec<_>>();
    let minute_rows = minutes
        .values()
        .map(|bucket| to_minute_row(bucket, &account, &capacities))
        .collect::<Vec<_>>();
    let window_rows = window_aggregates
        .values()
        .map(|window| to_window_row(window, &account, &capacities))
        .collect::<Vec<_>>();
    let session_rows = sessions
        .iter()
        .map(|(key, session)| to_session_row(key, session, &account, &capacities, &minutes))
        .collect::<Vec<_>>();
    database
        .replace_rollups(&daily_rows, &minute_rows, &window_rows, &session_rows)
        .await?;
    Ok(MaterializeSummary {
        days: daily_rows.len(),
        minutes: minute_rows.len(),
        sessions: session_rows.len(),
        windows: window_rows.len(),
    })
}

fn empty_minute(
    minute: i64,
    window_id: Option<String>,
    segment: Option<&WindowSegment>,
    account: &AccountContext,
) -> MinuteBucket {
    MinuteBucket {
        minute_start_ms: minute,
        local_date: local_date(minute).unwrap_or_else(|_| "unknown".to_owned()),
        window_id,
        window_kind: segment.map(|value| value.window_kind.clone()),
        window_start_ms: segment.map(|value| value.start_at_ms),
        resets_at_ms: segment.and_then(|value| value.reset_at_ms),
        reset_marker: segment.is_some_and(|value| value.ordinal > 0 && value.start_at_ms == minute),
        account_key: segment
            .and_then(|value| value.account_key.clone())
            .or_else(|| account.account_key.clone()),
        plan_type: segment
            .and_then(|value| value.plan_type.clone())
            .or_else(|| account.plan_type.clone()),
        provider: account.provider.clone(),
        ..Default::default()
    }
}

async fn load_jsonl(
    database: &Database,
) -> Result<
    (
        Vec<UsageRecord>,
        HashMap<String, SessionMeta>,
        HashMap<String, Vec<TurnRecord>>,
    ),
    MaterializeError,
> {
    let mut usages = Vec::new();
    let mut sessions = HashMap::new();
    let mut turns: HashMap<String, Vec<TurnRecord>> = HashMap::new();
    for row in database.list_source_jsonl().await? {
        let kind: String = row.try_get("kind")?;
        match kind.as_str() {
            "usage" => {
                let tokens = TokenCounts {
                    input: row
                        .try_get::<Option<i64>, _>("input_tokens")?
                        .unwrap_or_default(),
                    cached: row
                        .try_get::<Option<i64>, _>("cache_read_tokens")?
                        .unwrap_or_default(),
                    cache_write: row
                        .try_get::<Option<i64>, _>("cache_write_tokens")?
                        .unwrap_or_default(),
                    output: row
                        .try_get::<Option<i64>, _>("output_tokens")?
                        .unwrap_or_default(),
                    reasoning: row
                        .try_get::<Option<i64>, _>("reasoning_tokens")?
                        .unwrap_or_default(),
                    total: row
                        .try_get::<Option<i64>, _>("total_tokens")?
                        .unwrap_or_default(),
                }
                .normalized();
                if tokens.observed() {
                    usages.push(UsageRecord {
                        source_key: row.try_get("source_key")?,
                        observed_at_ms: row.try_get("observed_at_ms")?,
                        session_id: row.try_get("session_id")?,
                        root_session_id: row.try_get("root_session_id")?,
                        turn_id: row.try_get("turn_id")?,
                        model: row.try_get("model")?,
                        tier: row.try_get("service_tier")?,
                        reasoning_effort: row.try_get("reasoning_effort")?,
                        relation: row.try_get("relation")?,
                        provider: row.try_get("provider")?,
                        plan_type: row.try_get("plan_type")?,
                        tokens,
                    });
                }
            }
            "session" => {
                if let Some(session_id) = row.try_get::<Option<String>, _>("session_id")? {
                    sessions.insert(
                        session_id,
                        SessionMeta {
                            title: row.try_get("title")?,
                            relation: row.try_get("relation")?,
                        },
                    );
                }
            }
            "turn" => {
                if let (Some(session_id), Some(turn_id)) = (
                    row.try_get::<Option<String>, _>("session_id")?,
                    row.try_get::<Option<String>, _>("turn_id")?,
                ) {
                    turns.entry(session_id).or_default().push(TurnRecord {
                        id: turn_id,
                        started: row.try_get("started_at_ms")?,
                        ended: row.try_get("ended_at_ms")?,
                    });
                }
            }
            _ => {}
        }
    }
    for values in turns.values_mut() {
        values.sort_by_key(|turn| turn.started);
    }
    usages.sort_by_key(|usage| usage.observed_at_ms);
    Ok((usages, sessions, turns))
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct UsageDedupKey {
    /// Forked/replayed children share their resolved root, while independent
    /// sessions do not.  This keeps daily/minute de-duplication aligned with
    /// ccusage without collapsing two unrelated identical requests.
    owner_session_id: Option<String>,
    observed_at_ms: i64,
    model: Option<String>,
    input: i64,
    cached: i64,
    cache_write: i64,
    output: i64,
    reasoning: i64,
    total: i64,
}

/// Match ccusage's Codex event de-duplication: the same observed event is
/// counted once even when a forked child log replays it. Raw source rows stay
/// intact for audit; only the result pipeline chooses one canonical owner.
fn dedupe_usages(usages: Vec<UsageRecord>) -> Vec<UsageRecord> {
    let mut groups: HashMap<UsageDedupKey, Vec<UsageRecord>> = HashMap::new();
    for usage in usages {
        let key = UsageDedupKey {
            owner_session_id: usage
                .root_session_id
                .clone()
                .or_else(|| usage.session_id.clone()),
            observed_at_ms: usage.observed_at_ms,
            model: usage
                .model
                .as_ref()
                .map(|model| normalize_dedupe_model(model)),
            input: usage.tokens.input,
            cached: usage.tokens.cached,
            cache_write: usage.tokens.cache_write,
            output: usage.tokens.output,
            reasoning: usage.tokens.reasoning,
            total: usage.tokens.total,
        };
        groups.entry(key).or_default().push(usage);
    }
    let mut deduped = groups
        .into_values()
        .map(|mut values| {
            values.sort_by_key(|value| {
                (
                    matches!(value.relation.as_deref(), Some("child" | "fork")),
                    value.observed_at_ms,
                    value.source_key.clone(),
                )
            });
            let mut canonical = values.remove(0);
            for duplicate in values {
                canonical.tier = merge_service_tier(canonical.tier.take(), duplicate.tier.clone());
                canonical.model = canonical.model.or(duplicate.model);
                canonical.root_session_id = canonical.root_session_id.or(duplicate.root_session_id);
                canonical.turn_id = canonical.turn_id.or(duplicate.turn_id);
                canonical.reasoning_effort =
                    canonical.reasoning_effort.or(duplicate.reasoning_effort);
                canonical.provider = canonical.provider.or(duplicate.provider);
                canonical.plan_type = canonical.plan_type.or(duplicate.plan_type);
            }
            canonical
        })
        .collect::<Vec<_>>();
    deduped.sort_by_key(|usage| (usage.observed_at_ms, usage.source_key.clone()));
    deduped
}

fn normalize_dedupe_model(model: &str) -> String {
    match model.to_ascii_lowercase().as_str() {
        // ccusage resolves this synthetic model to the fallback card before
        // it builds the event key.  Keep the source display value unchanged.
        "codex-auto-review" => "gpt-5.5".to_owned(),
        value => value.to_owned(),
    }
}

fn merge_service_tier(current: Option<String>, incoming: Option<String>) -> Option<String> {
    match (current.as_deref(), incoming.as_deref()) {
        (Some("standard"), _) | (_, Some("standard")) => Some("standard".to_owned()),
        (Some("fast"), _) | (_, Some("fast")) => Some("fast".to_owned()),
        _ => current.or(incoming),
    }
}

async fn load_account_and_quotas(
    database: &Database,
) -> Result<(Vec<QuotaObservation>, AccountContext), MaterializeError> {
    let mut account = AccountContext::default();
    let app_rows = database.list_source_app_server().await?;
    if let Some(row) = app_rows
        .iter()
        .filter(|row| row.try_get::<String, _>("kind").ok().as_deref() == Some("account"))
        .max_by_key(|row| row.try_get::<i64, _>("last_seen_at_ms").unwrap_or_default())
    {
        account.account_key = row.try_get("account_key")?;
        account.auth_kind = row.try_get("auth_kind")?;
        account.provider = row.try_get("provider")?;
        account.plan_type = row.try_get("plan_type")?;
    }
    for row in app_rows
        .iter()
        .filter(|row| row.try_get::<String, _>("kind").ok().as_deref() == Some("usage"))
    {
        if let Some(text) = row.try_get::<Option<String>, _>("daily_tokens_json")? {
            if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&text) {
                for item in items {
                    if let (Some(date), Some(tokens)) = (
                        item.get("startDate")
                            .or_else(|| item.get("start_date"))
                            .or_else(|| item.get("date"))
                            .and_then(Value::as_str),
                        item.get("tokens")
                            .or_else(|| item.get("totalTokens"))
                            .and_then(Value::as_i64),
                    ) {
                        account.daily_tokens.insert(date.to_owned(), tokens);
                    }
                }
            }
        }
    }
    let mut quotas = Vec::new();
    for row in database
        .list_source_jsonl()
        .await?
        .into_iter()
        .filter(|row| row.try_get::<String, _>("kind").ok().as_deref() == Some("quota"))
    {
        let first_seen_at_ms: i64 = row.try_get("observed_at_ms")?;
        quotas.push(QuotaObservation {
            first_seen_at_ms,
            last_seen_at_ms: row
                .try_get::<Option<i64>, _>("last_seen_at_ms")?
                .unwrap_or(first_seen_at_ms),
            account_key: account.account_key.clone(),
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
    for row in app_rows
        .into_iter()
        .filter(|row| row.try_get::<String, _>("kind").ok().as_deref() == Some("quota"))
    {
        let first_seen_at_ms: i64 = row.try_get("first_seen_at_ms")?;
        quotas.push(QuotaObservation {
            first_seen_at_ms,
            last_seen_at_ms: row.try_get("last_seen_at_ms")?,
            account_key: row
                .try_get::<Option<String>, _>("account_key")?
                .or_else(|| account.account_key.clone()),
            limit_id: row.try_get("limit_id")?,
            window_kind: row
                .try_get::<Option<String>, _>("window_kind")?
                .unwrap_or_else(|| "primary".to_owned()),
            used_percent: row.try_get("used_percent")?,
            window_minutes: row.try_get("window_minutes")?,
            resets_at_ms: row.try_get("resets_at_ms")?,
            plan_type: row
                .try_get::<Option<String>, _>("plan_type")?
                .or_else(|| account.plan_type.clone()),
            source: "app_server",
            priority: 2,
        });
    }
    Ok((quotas, account))
}

async fn load_capacities(database: &Database) -> Result<Vec<Capacity>, MaterializeError> {
    Ok(database
        .list_capacities()
        .await?
        .into_iter()
        .map(|row| {
            Ok(Capacity {
                profile: row.try_get("profile_code")?,
                account_key: row.try_get("account_key")?,
                plan_type: row.try_get("plan_type")?,
                credit: row.try_get("weekly_credit")?,
                from: row.try_get("effective_from_ms")?,
                to: row.try_get("effective_to_ms")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()?)
}

fn build_windows(quotas: &[QuotaObservation]) -> Vec<WindowSegment> {
    let mut groups: BTreeMap<String, Vec<&QuotaObservation>> = BTreeMap::new();
    for quota in quotas {
        groups
            .entry(format!(
                "{}:{}:{}",
                quota.account_key.as_deref().unwrap_or("unknown"),
                quota.limit_id.as_deref().unwrap_or("unknown"),
                quota.window_kind
            ))
            .or_default()
            .push(quota);
    }
    let mut windows = Vec::new();
    for (group, mut values) in groups {
        values.sort_by_key(|value| {
            (
                value.first_seen_at_ms,
                value.last_seen_at_ms,
                value.priority,
            )
        });
        let mut current = None;
        let mut previous_percent = None;
        let mut previous_reset: Option<i64> = None;
        let mut ordinal = 0;
        for quota in values {
            let percent_reset = previous_percent
                .zip(quota.used_percent)
                .is_some_and(|(old, new)| new + RESET_DROP_PERCENT < old);
            let explicit_reset = previous_reset
                .zip(quota.resets_at_ms)
                .is_some_and(|(old, new)| (new - old).abs() > RESET_TIME_JITTER_MS);
            if current.is_none() || percent_reset || explicit_reset {
                if let Some(value) = current.take() {
                    windows.push(value);
                }
                current = Some(WindowSegment {
                    id: format!("window:{group}:{ordinal}"),
                    account_key: quota.account_key.clone(),
                    limit_id: quota.limit_id.clone(),
                    window_kind: quota.window_kind.clone(),
                    start_at_ms: quota.first_seen_at_ms,
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
            previous_reset = quota.resets_at_ms.or(previous_reset);
        }
        if let Some(value) = current {
            windows.push(value);
        }
    }
    windows.sort_by_key(|window| window.start_at_ms);
    windows
}

fn find_quota_window<'a>(
    windows: &'a [WindowSegment],
    quota: &QuotaObservation,
) -> Option<&'a WindowSegment> {
    windows
        .iter()
        .filter(|window| window.window_kind == quota.window_kind)
        .filter(|window| window.limit_id == quota.limit_id)
        .filter(|window| window.account_key == quota.account_key)
        .filter(|window| window.start_at_ms <= quota.first_seen_at_ms)
        .max_by_key(|window| window.start_at_ms)
        .or_else(|| {
            windows
                .iter()
                .filter(|window| window.window_kind == quota.window_kind)
                .filter(|window| window.limit_id == quota.limit_id)
                .filter(|window| window.account_key == quota.account_key)
                .min_by_key(|window| window.start_at_ms)
        })
}

fn find_usage_window(windows: &[WindowSegment], observed_at_ms: i64) -> Option<&WindowSegment> {
    let primary = windows
        .iter()
        .filter(|window| window.window_kind == "primary")
        .collect::<Vec<_>>();
    let candidates = if primary.is_empty() {
        windows.iter().collect::<Vec<_>>()
    } else {
        primary
    };
    candidates
        .iter()
        .filter(|window| window.start_at_ms <= observed_at_ms)
        .max_by_key(|window| window.start_at_ms)
        .copied()
        .or_else(|| {
            candidates
                .iter()
                .min_by_key(|window| window.start_at_ms)
                .copied()
        })
}

fn find_turn(values: Option<&Vec<TurnRecord>>, observed_at_ms: i64) -> Option<&TurnRecord> {
    let values = values?;
    values
        .iter()
        .filter(|turn| turn.started.is_some_and(|start| start <= observed_at_ms))
        .filter(|turn| turn.ended.is_none_or(|end| observed_at_ms <= end))
        .max_by_key(|turn| turn.started)
        .or_else(|| {
            values
                .iter()
                .filter(|turn| turn.started.is_some_and(|start| start <= observed_at_ms))
                .max_by_key(|turn| turn.started)
        })
}

fn capacity_for<'a>(
    capacities: &'a [Capacity],
    account_key: Option<&str>,
    plan: Option<&str>,
    at_ms: i64,
) -> Option<&'a Capacity> {
    capacities
        .iter()
        .filter(|capacity| capacity.from <= at_ms && capacity.to.is_none_or(|to| at_ms < to))
        .filter(|capacity| {
            capacity
                .account_key
                .as_deref()
                .is_none_or(|key| Some(key) == account_key)
        })
        .filter(|capacity| {
            capacity
                .plan_type
                .as_deref()
                .is_none_or(|value| Some(value) == plan)
        })
        .max_by_key(|capacity| capacity.from)
}

fn plan_key(plan_type: Option<&str>, capacity_profile: Option<&str>) -> String {
    format!(
        "{}:{}",
        plan_type.unwrap_or("unknown"),
        capacity_profile.unwrap_or("unknown")
    )
}

fn add_plan_metric(
    plans: &mut BTreeMap<String, PlanAggregate>,
    usage: &UsageRecord,
    price: &pricing::Price,
    account: &AccountContext,
    capacities: &[Capacity],
) {
    let plan_type = usage
        .plan_type
        .clone()
        .or_else(|| account.plan_type.clone());
    let capacity = capacity_for(
        capacities,
        account.account_key.as_deref(),
        plan_type.as_deref(),
        usage.observed_at_ms,
    );
    let capacity_profile = capacity.map(|value| value.profile.clone());
    let key = plan_key(plan_type.as_deref(), capacity_profile.as_deref());
    let entry = plans.entry(key).or_default();
    entry.plan_type = entry.plan_type.clone().or(plan_type);
    entry.capacity_profile = entry.capacity_profile.clone().or(capacity_profile);
    entry.metric.add(&usage.tokens, price);
}

fn dominant_plan(plans: &BTreeMap<String, PlanAggregate>) -> Option<&PlanAggregate> {
    plans.values().max_by(|left, right| {
        let left_value = left
            .metric
            .credit()
            .unwrap_or(left.metric.tokens.total as f64);
        let right_value = right
            .metric
            .credit()
            .unwrap_or(right.metric.tokens.total as f64);
        left_value
            .partial_cmp(&right_value)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn capacity_for_plan<'a>(
    capacities: &'a [Capacity],
    account_key: Option<&str>,
    plan_type: Option<&str>,
    profile: Option<&str>,
    at_ms: i64,
) -> Option<&'a Capacity> {
    profile
        .and_then(|profile| {
            capacities
                .iter()
                .filter(|capacity| capacity.profile == profile)
                .filter(|capacity| {
                    capacity.from <= at_ms && capacity.to.is_none_or(|to| at_ms < to)
                })
                .filter(|capacity| {
                    capacity
                        .account_key
                        .as_deref()
                        .is_none_or(|key| Some(key) == account_key)
                })
                .max_by_key(|capacity| capacity.from)
        })
        .or_else(|| capacity_for(capacities, account_key, plan_type, at_ms))
}

fn join_quality(mut quality: BTreeSet<String>, metric: &Metric) -> Option<String> {
    quality.extend(metric.quality.iter().cloned());
    if let Some(flag) = metric.quality_flags() {
        quality.insert(flag.to_owned());
    }
    (!quality.is_empty()).then(|| quality.into_iter().collect::<Vec<_>>().join(","))
}

fn account_tokens_between(
    account: &AccountContext,
    start_at_ms: Option<i64>,
    end_at_ms: Option<i64>,
) -> Option<i64> {
    let start = start_at_ms?;
    let start_date = local_date(start).ok()?;
    let end_date = end_at_ms
        .and_then(|value| local_date(value).ok())
        .unwrap_or_else(|| "9999-12-31".to_owned());
    let mut total = 0_i64;
    let mut found = false;
    for (date, value) in &account.daily_tokens {
        if date >= &start_date && date <= &end_date {
            total = total.saturating_add(*value);
            found = true;
        }
    }
    found.then_some(total)
}

fn to_daily_row(
    day: &DayAggregate,
    account: &AccountContext,
    capacities: &[Capacity],
) -> UsageDailyRecord {
    let dominant = dominant_plan(&day.plans);
    let plan_type = dominant
        .and_then(|plan| plan.plan_type.clone())
        .or_else(|| account.plan_type.clone());
    let capacity_profile = dominant.and_then(|plan| plan.capacity_profile.clone());
    let capacity = capacity_for_plan(
        capacities,
        account.account_key.as_deref(),
        plan_type.as_deref(),
        capacity_profile.as_deref(),
        day_start_ms(&day.local_date),
    );
    let account_tokens = account.daily_tokens.get(&day.local_date).copied();
    let local_total = day.metric.has_usage.then_some(day.metric.tokens.total);
    let unobserved = account_tokens
        .zip(local_total)
        .map(|(remote, local)| remote.saturating_sub(local));
    let coverage = account_tokens
        .zip(local_total)
        .and_then(|(remote, local)| (remote > 0).then(|| local as f64 / remote as f64));
    let local_percent = day
        .metric
        .credit()
        .zip(capacity)
        .filter(|(_, capacity)| capacity.credit > 0.0)
        .map(|(credit, capacity)| credit / capacity.credit * 100.0);
    let mut quality = day.quality.clone();
    if day.official_start.is_none() {
        quality.insert("official_unavailable".to_owned());
    }
    if account_tokens.is_none() {
        quality.insert("account_tokens_unavailable".to_owned());
    }
    if day.plans.len() > 1 {
        quality.insert("mixed_plan".to_owned());
    }
    UsageDailyRecord {
        local_date: day.local_date.clone(),
        account_key: account.account_key.clone(),
        auth_kind: account.auth_kind.clone(),
        plan_type,
        capacity_profile: capacity
            .map(|value| value.profile.clone())
            .or(capacity_profile),
        input_tokens: day.metric.has_usage.then_some(day.metric.tokens.input),
        cache_read_tokens: day.metric.has_usage.then_some(day.metric.tokens.cached),
        cache_write_tokens: day
            .metric
            .has_usage
            .then_some(day.metric.tokens.cache_write),
        output_tokens: day.metric.has_usage.then_some(day.metric.tokens.output),
        reasoning_tokens: day.metric.has_usage.then_some(day.metric.tokens.reasoning),
        total_tokens: day.metric.has_usage.then_some(day.metric.tokens.total),
        credit: day.metric.credit(),
        api_usd: day.metric.api_usd(),
        local_percent,
        account_tokens,
        unobserved_tokens: unobserved,
        coverage_ratio: coverage,
        account_token_freshness: Some(
            if account_tokens.is_some() {
                "settled"
            } else {
                "unavailable"
            }
            .to_owned(),
        ),
        official_percent_start: day.official_start,
        official_percent_end: day.official_end,
        official_percent_delta: (day.official_delta > 0.0).then_some(day.official_delta),
        reset_count: day.reset_count,
        quality: (!quality.is_empty())
            .then(|| quality.iter().cloned().collect::<Vec<_>>().join(",")),
    }
}

fn to_minute_row(
    bucket: &MinuteBucket,
    account: &AccountContext,
    capacities: &[Capacity],
) -> UsageMinuteRecord {
    let capacity = capacity_for(
        capacities,
        bucket
            .account_key
            .as_deref()
            .or(account.account_key.as_deref()),
        bucket.plan_type.as_deref().or(account.plan_type.as_deref()),
        bucket.minute_start_ms,
    );
    UsageMinuteRecord {
        bucket_key: format!(
            "{}:{}",
            bucket.minute_start_ms,
            bucket.window_id.as_deref().unwrap_or("none")
        ),
        minute_start_ms: bucket.minute_start_ms,
        local_date: bucket.local_date.clone(),
        account_key: bucket
            .account_key
            .clone()
            .or_else(|| account.account_key.clone()),
        auth_kind: account.auth_kind.clone(),
        plan_type: bucket
            .plan_type
            .clone()
            .or_else(|| account.plan_type.clone()),
        provider: bucket.provider.clone().or_else(|| account.provider.clone()),
        capacity_profile: capacity.map(|value| value.profile.clone()),
        window_id: bucket.window_id.clone(),
        window_kind: bucket.window_kind.clone(),
        window_start_ms: bucket.window_start_ms,
        resets_at_ms: bucket.resets_at_ms,
        reset_marker: bucket.reset_marker,
        input_tokens: bucket
            .metric
            .has_usage
            .then_some(bucket.metric.tokens.input),
        cache_read_tokens: bucket
            .metric
            .has_usage
            .then_some(bucket.metric.tokens.cached),
        cache_write_tokens: bucket
            .metric
            .has_usage
            .then_some(bucket.metric.tokens.cache_write),
        output_tokens: bucket
            .metric
            .has_usage
            .then_some(bucket.metric.tokens.output),
        reasoning_tokens: bucket
            .metric
            .has_usage
            .then_some(bucket.metric.tokens.reasoning),
        total_tokens: bucket
            .metric
            .has_usage
            .then_some(bucket.metric.tokens.total),
        credit: bucket.metric.credit(),
        api_usd: bucket.metric.api_usd(),
        official_used_percent: bucket.official.as_ref().map(|point| point.percent),
        official_source: bucket
            .official
            .as_ref()
            .map(|point| point.source.to_owned()),
        quality: join_quality(bucket.quality.clone(), &bucket.metric),
    }
}

fn to_window_row(
    window: &WindowAggregate,
    account: &AccountContext,
    capacities: &[Capacity],
) -> UsageWindowRecord {
    let dominant = dominant_plan(&window.plans);
    let plan_type = dominant
        .and_then(|plan| plan.plan_type.clone())
        .or_else(|| window.segment.plan_type.clone())
        .or_else(|| account.plan_type.clone());
    let capacity_profile = dominant
        .and_then(|plan| plan.capacity_profile.clone())
        .or_else(|| {
            capacity_for(
                capacities,
                window
                    .segment
                    .account_key
                    .as_deref()
                    .or(account.account_key.as_deref()),
                plan_type.as_deref(),
                window.segment.start_at_ms,
            )
            .map(|value| value.profile.clone())
        });
    let capacity = capacity_for_plan(
        capacities,
        window
            .segment
            .account_key
            .as_deref()
            .or(account.account_key.as_deref()),
        plan_type.as_deref(),
        capacity_profile.as_deref(),
        window.segment.start_at_ms,
    );
    let account_tokens = account_tokens_between(
        account,
        Some(window.segment.start_at_ms),
        window.segment.reset_at_ms,
    );
    let local_total = window
        .metric
        .has_usage
        .then_some(window.metric.tokens.total);
    let unobserved = account_tokens
        .zip(local_total)
        .map(|(remote, local)| remote.saturating_sub(local));
    let coverage = account_tokens
        .zip(local_total)
        .and_then(|(remote, local)| (remote > 0).then(|| local as f64 / remote as f64));
    let local_percent = window
        .metric
        .credit()
        .zip(capacity)
        .filter(|(_, capacity)| capacity.credit > 0.0)
        .map(|(credit, capacity)| credit / capacity.credit * 100.0);
    let mut quality = window.quality.clone();
    if window.plans.len() > 1 {
        quality.insert("mixed_plan".to_owned());
    }
    if account_tokens.is_none() {
        quality.insert("account_tokens_unavailable".to_owned());
    }
    UsageWindowRecord {
        window_id: window.segment.id.clone(),
        account_key: window
            .segment
            .account_key
            .clone()
            .or_else(|| account.account_key.clone()),
        limit_id: window.segment.limit_id.clone(),
        window_kind: window.segment.window_kind.clone(),
        window_start_ms: Some(window.segment.start_at_ms),
        resets_at_ms: window.segment.reset_at_ms,
        window_minutes: window.segment.window_minutes,
        auth_kind: account.auth_kind.clone(),
        plan_type,
        provider: account.provider.clone(),
        capacity_profile: capacity
            .map(|value| value.profile.clone())
            .or(capacity_profile),
        input_tokens: window
            .metric
            .has_usage
            .then_some(window.metric.tokens.input),
        cache_read_tokens: window
            .metric
            .has_usage
            .then_some(window.metric.tokens.cached),
        cache_write_tokens: window
            .metric
            .has_usage
            .then_some(window.metric.tokens.cache_write),
        output_tokens: window
            .metric
            .has_usage
            .then_some(window.metric.tokens.output),
        reasoning_tokens: window
            .metric
            .has_usage
            .then_some(window.metric.tokens.reasoning),
        total_tokens: local_total,
        credit: window.metric.credit(),
        api_usd: window.metric.api_usd(),
        local_percent,
        account_tokens,
        unobserved_tokens: unobserved,
        coverage_ratio: coverage,
        official_percent_start: window.official_start,
        official_percent_end: window.official_end,
        official_percent_delta: (window.official_delta > 0.0).then_some(window.official_delta),
        quality: join_quality(quality, &window.metric),
    }
}

fn official_for_session(
    session: &SessionAggregate,
    minutes: &BTreeMap<(i64, String), MinuteBucket>,
) -> (Option<f64>, Option<f64>) {
    let start = session.observed_start_ms.unwrap_or_default();
    let end = session.observed_end_ms.unwrap_or(start);
    let points = minutes
        .values()
        .filter(|bucket| bucket.window_id == session.window_id)
        .filter(|bucket| bucket.minute_start_ms >= minute_start(start))
        .filter(|bucket| bucket.minute_start_ms <= minute_start(end))
        .filter_map(|bucket| bucket.official.as_ref().map(|point| point.percent))
        .collect::<Vec<_>>();
    (points.first().copied(), points.last().copied())
}

fn to_session_row(
    key: &str,
    session: &SessionAggregate,
    account: &AccountContext,
    capacities: &[Capacity],
    minutes: &BTreeMap<(i64, String), MinuteBucket>,
) -> UsageSessionRecord {
    let capacity = capacity_for(
        capacities,
        session
            .account_key
            .as_deref()
            .or(account.account_key.as_deref()),
        session
            .plan_type
            .as_deref()
            .or(account.plan_type.as_deref()),
        session.started_at_ms.unwrap_or_default(),
    );
    let model_breakdown_json = (!session.models.is_empty()).then(|| {
        serde_json::to_string(
            &session
                .models
                .iter()
                .map(|((model, tier, effort), metric)| {
                    json!({
                        "model": model,
                        "tier": tier,
                        "reasoning_effort": effort,
                        "tokens": metric.tokens.total,
                        "credit": metric.credit(),
                        "api_usd": metric.api_usd()
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default()
    });
    let (official_percent_start, official_percent_end) = official_for_session(session, minutes);
    let mut quality = session.quality.clone();
    if official_percent_start.is_none() {
        quality.insert("official_unavailable".to_owned());
    }
    let fast_state = match session.tiers.len() {
        0 => Some("unknown"),
        1 if session.tiers.contains("fast") => Some("fast"),
        1 => Some("standard"),
        _ => Some("mixed"),
    };
    UsageSessionRecord {
        row_key: key.to_owned(),
        local_date: session.local_date.clone(),
        root_session_id: session.root_session_id.clone(),
        session_id: session.session_id.clone(),
        turn_id: session.turn_id.clone(),
        title: session.title.clone(),
        relation: session.relation.clone(),
        started_at_ms: session.started_at_ms,
        ended_at_ms: session.ended_at_ms,
        window_id: session.window_id.clone(),
        account_key: session
            .account_key
            .clone()
            .or_else(|| account.account_key.clone()),
        auth_kind: account.auth_kind.clone(),
        plan_type: session
            .plan_type
            .clone()
            .or_else(|| account.plan_type.clone()),
        provider: session
            .provider
            .clone()
            .or_else(|| account.provider.clone()),
        capacity_profile: capacity.map(|value| value.profile.clone()),
        primary_model: (session.models.len() == 1)
            .then(|| session.models.keys().next().map(|value| value.0.clone()))
            .flatten(),
        fast_state: fast_state.map(str::to_owned),
        model_breakdown_json,
        input_tokens: session
            .metric
            .has_usage
            .then_some(session.metric.tokens.input),
        cache_read_tokens: session
            .metric
            .has_usage
            .then_some(session.metric.tokens.cached),
        cache_write_tokens: session
            .metric
            .has_usage
            .then_some(session.metric.tokens.cache_write),
        output_tokens: session
            .metric
            .has_usage
            .then_some(session.metric.tokens.output),
        reasoning_tokens: session
            .metric
            .has_usage
            .then_some(session.metric.tokens.reasoning),
        total_tokens: session
            .metric
            .has_usage
            .then_some(session.metric.tokens.total),
        credit: session.metric.credit(),
        api_usd: session.metric.api_usd(),
        official_percent_start,
        official_percent_end,
        quality: join_quality(quality, &session.metric),
    }
}

fn min_option(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        _ => None,
    }
}
fn max_option(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        _ => None,
    }
}
fn minute_start(value: i64) -> i64 {
    value - value.rem_euclid(60_000)
}

fn day_start_ms(date: &str) -> i64 {
    time::Date::parse(
        date,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .ok()
    .and_then(|date| date.with_hms(0, 0, 0).ok())
    .map(|date| {
        date.assume_offset(time::UtcOffset::from_hms(8, 0, 0).unwrap())
            .unix_timestamp_nanos() as i64
            / 1_000_000
    })
    .unwrap_or_default()
}

fn day_end_ms(date: &str) -> i64 {
    time::Date::parse(
        date,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .ok()
    .and_then(|date| date.next_day())
    .and_then(|date| date.with_hms(0, 0, 0).ok())
    .map(|date| {
        date.assume_offset(time::UtcOffset::from_hms(8, 0, 0).unwrap())
            .unix_timestamp_nanos() as i64
            / 1_000_000
    })
    .unwrap_or(i64::MAX)
}

fn local_date(epoch_ms: i64) -> Result<String, MaterializeError> {
    let utc = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(epoch_ms) * 1_000_000)
        .map_err(|error| MaterializeError::Timestamp(error.to_string()))?;
    let offset = time::UtcOffset::from_hms(8, 0, 0)
        .map_err(|error| MaterializeError::Timestamp(error.to_string()))?;
    utc.to_offset(offset)
        .format(&time::macros::format_description!("[year]-[month]-[day]"))
        .map_err(|error| MaterializeError::Timestamp(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{SourceJsonlRecord, UsageDailyRecord};

    fn usage_record(source_key: &str, observed_at_ms: i64, relation: Option<&str>) -> UsageRecord {
        UsageRecord {
            source_key: source_key.to_owned(),
            observed_at_ms,
            session_id: Some("session".to_owned()),
            root_session_id: Some("session".to_owned()),
            turn_id: Some("turn".to_owned()),
            model: Some("gpt-5.6-sol".to_owned()),
            tier: Some("standard".to_owned()),
            reasoning_effort: Some("high".to_owned()),
            relation: relation.map(str::to_owned),
            provider: None,
            plan_type: None,
            tokens: TokenCounts {
                input: 10,
                total: 10,
                ..Default::default()
            },
        }
    }

    #[test]
    fn dedupe_prefers_the_parent_owner_of_a_replayed_event() {
        let deduped = dedupe_usages(vec![
            usage_record("child:1", 100, Some("child")),
            usage_record("parent:1", 100, Some("main")),
        ]);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].source_key, "parent:1");
    }

    #[test]
    fn dedupe_keeps_identical_events_from_independent_roots() {
        let mut first = usage_record("first", 100, Some("main"));
        let mut second = usage_record("second", 100, Some("main"));
        first.root_session_id = Some("root-a".to_owned());
        second.root_session_id = Some("root-b".to_owned());
        assert_eq!(dedupe_usages(vec![first, second]).len(), 2);
    }

    #[tokio::test]
    async fn rebuilds_four_result_tables_from_source_rows() {
        let database = Database::connect_in_memory().await.unwrap();
        database
            .upsert_source_jsonl(&SourceJsonlRecord {
                source_key: "usage:1".to_owned(),
                kind: "usage".to_owned(),
                observed_at_ms: 1785825720000,
                model: Some("gpt-5.6-sol".to_owned()),
                service_tier: Some("standard".to_owned()),
                input_tokens: Some(10),
                total_tokens: Some(10),
                ..Default::default()
            })
            .await
            .unwrap();
        let result = refresh_rollups(&database).await.unwrap();
        assert_eq!(result.days, 1);
        assert_eq!(database.list_usage_daily().await.unwrap().len(), 1);
        assert!(!database.list_usage_minute().await.unwrap().is_empty());
        assert!(!database.list_usage_session().await.unwrap().is_empty());
        assert!(database.list_usage_window().await.unwrap().is_empty());
        let _ = UsageDailyRecord::default();
    }

    #[tokio::test]
    async fn unclassified_service_tier_keeps_rollup_amounts_and_marks_quality() {
        let database = Database::connect_in_memory().await.unwrap();
        database
            .upsert_source_jsonl(&SourceJsonlRecord {
                source_key: "usage:unclassified".to_owned(),
                kind: "usage".to_owned(),
                observed_at_ms: 1785825720000,
                model: Some("gpt-5.6-sol".to_owned()),
                service_tier: None,
                input_tokens: Some(10),
                total_tokens: Some(10),
                ..Default::default()
            })
            .await
            .unwrap();
        refresh_rollups(&database).await.unwrap();
        let row = database.list_usage_daily().await.unwrap().pop().unwrap();
        assert!(row.try_get::<Option<f64>, _>("credit").unwrap().is_some());
        assert!(row.try_get::<Option<f64>, _>("api_usd").unwrap().is_some());
        assert!(row
            .try_get::<Option<String>, _>("quality")
            .unwrap()
            .is_some_and(|quality| quality.contains("service_tier_unclassified")));
    }

    #[test]
    fn partial_pricing_keeps_known_amounts_and_marks_the_rollup() {
        let mut metric = Metric::default();
        metric.add(
            &TokenCounts {
                input: 10,
                total: 10,
                ..Default::default()
            },
            &pricing::Price {
                credit_micros: Some(1_000_000),
                api_usd_micros: Some(100_000),
                quality: Vec::new(),
            },
        );
        metric.add(
            &TokenCounts {
                input: 5,
                total: 5,
                ..Default::default()
            },
            &pricing::Price {
                credit_micros: None,
                api_usd_micros: None,
                quality: vec!["missing_pricing".to_owned()],
            },
        );
        assert_eq!(metric.credit(), Some(1.0));
        assert_eq!(metric.api_usd(), Some(0.1));
        assert_eq!(metric.quality_flags(), Some("pricing_partial"));
    }

    #[tokio::test]
    async fn splits_turns_at_local_date_and_keeps_reasoning_effort() {
        let database = Database::connect_in_memory().await.unwrap();
        let first = day_start_ms("2026-08-01") + 86_399_000;
        let second = day_start_ms("2026-08-02") + 1_000;
        database
            .upsert_source_jsonl(&SourceJsonlRecord {
                source_key: "session:root".to_owned(),
                kind: "session".to_owned(),
                observed_at_ms: first,
                session_id: Some("session".to_owned()),
                root_session_id: Some("session".to_owned()),
                title: Some("跨日任务".to_owned()),
                relation: Some("main".to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
        database
            .upsert_source_jsonl(&SourceJsonlRecord {
                source_key: "turn:1".to_owned(),
                kind: "turn".to_owned(),
                observed_at_ms: first,
                session_id: Some("session".to_owned()),
                root_session_id: Some("session".to_owned()),
                turn_id: Some("turn".to_owned()),
                started_at_ms: Some(first - 30_000),
                ended_at_ms: Some(second + 30_000),
                ..Default::default()
            })
            .await
            .unwrap();
        for (key, observed_at_ms) in [("usage:1", first), ("usage:2", second)] {
            database
                .upsert_source_jsonl(&SourceJsonlRecord {
                    source_key: key.to_owned(),
                    kind: "usage".to_owned(),
                    observed_at_ms,
                    session_id: Some("session".to_owned()),
                    root_session_id: Some("session".to_owned()),
                    turn_id: Some("turn".to_owned()),
                    model: Some("gpt-5.6-sol".to_owned()),
                    service_tier: Some("standard".to_owned()),
                    reasoning_effort: Some("high".to_owned()),
                    input_tokens: Some(10),
                    total_tokens: Some(10),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        let summary = refresh_rollups(&database).await.unwrap();
        assert_eq!(summary.sessions, 2);
        let rows = database.list_usage_session().await.unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert!(row
                .try_get::<Option<String>, _>("quality")
                .unwrap()
                .is_some_and(|value| value.contains("split_boundary")));
            assert!(row
                .try_get::<Option<String>, _>("model_breakdown_json")
                .unwrap()
                .is_some_and(|value| value.contains("reasoning_effort")));
        }
    }

    #[tokio::test]
    async fn materializes_primary_reset_windows_and_official_percentages() {
        let database = Database::connect_in_memory().await.unwrap();
        let first = day_start_ms("2026-08-03") + 10 * 60_000;
        let second = first + 120 * 60_000;
        for (key, observed_at_ms, used_percent, resets_at_ms) in [
            ("quota:1", first, 80.0, first + 7 * 24 * 60 * 60_000),
            ("quota:2", second, 1.0, second + 7 * 24 * 60 * 60_000),
        ] {
            database
                .upsert_source_jsonl(&SourceJsonlRecord {
                    source_key: key.to_owned(),
                    kind: "quota".to_owned(),
                    observed_at_ms,
                    limit_id: Some("weekly".to_owned()),
                    window_kind: Some("primary".to_owned()),
                    used_percent: Some(used_percent),
                    window_minutes: Some(10_080),
                    resets_at_ms: Some(resets_at_ms),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        for (key, observed_at_ms) in [
            ("usage:before", first + 60_000),
            ("usage:after", second + 60_000),
        ] {
            database
                .upsert_source_jsonl(&SourceJsonlRecord {
                    source_key: key.to_owned(),
                    kind: "usage".to_owned(),
                    observed_at_ms,
                    session_id: Some("session".to_owned()),
                    root_session_id: Some("session".to_owned()),
                    turn_id: Some(key.to_owned()),
                    model: Some("gpt-5.6-sol".to_owned()),
                    service_tier: Some("standard".to_owned()),
                    input_tokens: Some(10),
                    total_tokens: Some(10),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        let summary = refresh_rollups(&database).await.unwrap();
        assert_eq!(summary.windows, 2);
        let windows = database.list_usage_window().await.unwrap();
        assert_eq!(windows.len(), 2);
        assert!(windows
            .iter()
            .all(|row| { row.try_get::<String, _>("window_kind").unwrap() == "primary" }));
        let minutes = database.list_usage_minute().await.unwrap();
        assert!(minutes
            .iter()
            .any(|row| { row.try_get::<i64, _>("reset_marker").unwrap() != 0 }));
        assert!(windows.iter().any(|row| {
            row.try_get::<Option<f64>, _>("official_percent_start")
                .unwrap()
                == Some(80.0)
        }));
    }

    #[test]
    fn reset_windows_prefer_explicit_reset_change() {
        let values = vec![
            QuotaObservation {
                first_seen_at_ms: 1_000_000,
                last_seen_at_ms: 1_000_000,
                account_key: None,
                limit_id: Some("weekly".to_owned()),
                window_kind: "primary".to_owned(),
                used_percent: Some(10.0),
                window_minutes: Some(10_080),
                resets_at_ms: Some(2_000_000),
                plan_type: None,
                source: "jsonl",
                priority: 1,
            },
            QuotaObservation {
                first_seen_at_ms: 1_060_000,
                last_seen_at_ms: 1_060_000,
                account_key: None,
                limit_id: Some("weekly".to_owned()),
                window_kind: "primary".to_owned(),
                used_percent: Some(11.0),
                window_minutes: Some(10_080),
                resets_at_ms: Some(2_001_000),
                plan_type: None,
                source: "jsonl",
                priority: 1,
            },
            QuotaObservation {
                first_seen_at_ms: 1_120_000,
                last_seen_at_ms: 1_120_000,
                account_key: None,
                limit_id: Some("weekly".to_owned()),
                window_kind: "primary".to_owned(),
                used_percent: Some(2.0),
                window_minutes: Some(10_080),
                resets_at_ms: Some(2_600_000),
                plan_type: None,
                source: "jsonl",
                priority: 1,
            },
        ];
        assert_eq!(build_windows(&values).len(), 2);
    }

    #[tokio::test]
    #[ignore = "requires the rebuilt local runtime database"]
    async fn real_runtime_rollup_refresh_when_requested() {
        if std::env::var("CODEX_METER_RUN_REAL_ROLLUP").ok().as_deref() != Some("1") {
            return;
        }
        let database = Database::connect(".runtime/codex-meter.sqlite")
            .await
            .unwrap();
        let summary = refresh_rollups(&database).await.unwrap();
        eprintln!("real rollup refresh: {summary:?}");
    }
}
