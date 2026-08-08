//! Thin read model for the single `/api/report` contract.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use sqlx::Row;
use thiserror::Error;

use crate::{
    db::{Database, DbError},
    pricing,
};

#[derive(Debug, Error)]
pub enum ReportError {
    #[error("database error: {0}")]
    Database(#[from] DbError),
    #[error("database query error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Default)]
struct Metric {
    input: i64,
    cached: i64,
    cache_write: i64,
    output: i64,
    reasoning: i64,
    total: i64,
    credit: f64,
    api_usd: f64,
    has_usage: bool,
    credit_known: bool,
    api_known: bool,
}

impl Metric {
    fn add_row(&mut self, row: &sqlx::sqlite::SqliteRow) -> Result<(), sqlx::Error> {
        let observed = [
            "input_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "output_tokens",
            "reasoning_tokens",
            "total_tokens",
            "credit",
            "api_usd",
        ]
        .iter()
        .any(|name| {
            row.try_get::<Option<i64>, _>(*name)
                .ok()
                .flatten()
                .is_some()
                || row
                    .try_get::<Option<f64>, _>(*name)
                    .ok()
                    .flatten()
                    .is_some()
        });
        if !observed {
            return Ok(());
        }
        if !self.has_usage {
            self.credit_known = true;
            self.api_known = true;
        }
        self.has_usage = true;
        self.input += row
            .try_get::<Option<i64>, _>("input_tokens")?
            .unwrap_or_default();
        self.cached += row
            .try_get::<Option<i64>, _>("cache_read_tokens")?
            .unwrap_or_default();
        self.cache_write += row
            .try_get::<Option<i64>, _>("cache_write_tokens")?
            .unwrap_or_default();
        self.output += row
            .try_get::<Option<i64>, _>("output_tokens")?
            .unwrap_or_default();
        self.reasoning += row
            .try_get::<Option<i64>, _>("reasoning_tokens")?
            .unwrap_or_default();
        self.total += row
            .try_get::<Option<i64>, _>("total_tokens")?
            .unwrap_or_default();
        if let Some(value) = row.try_get::<Option<f64>, _>("credit")? {
            self.credit += value;
        } else {
            self.credit_known = false;
        }
        if let Some(value) = row.try_get::<Option<f64>, _>("api_usd")? {
            self.api_usd += value;
        } else {
            self.api_known = false;
        }
        Ok(())
    }
    fn usage_json(&self) -> Value {
        json!({"input": self.has_usage.then_some(self.input), "cached": self.has_usage.then_some(self.cached), "cache_write": self.has_usage.then_some(self.cache_write), "output": self.has_usage.then_some(self.output), "reasoning": self.has_usage.then_some(self.reasoning), "total": self.has_usage.then_some(self.total)})
    }
    fn credit(&self) -> Option<f64> {
        (self.has_usage && self.credit_known).then_some(self.credit)
    }
    fn api_usd(&self) -> Option<f64> {
        (self.has_usage && self.api_known).then_some(self.api_usd)
    }
}

pub async fn build_report(
    database: &Database,
    selected_date: Option<&str>,
) -> Result<Value, ReportError> {
    let daily = database.list_usage_daily().await?;
    let minutes = database.list_usage_minute().await?;
    let usage_windows = database.list_usage_window().await?;
    let sessions = database.list_usage_session().await?;
    let capacities = database.list_capacities().await?;
    let app = database.list_source_app_server().await?;
    let ccusage = database.list_source_ccusage().await?;
    let source_jsonl = database.list_source_jsonl().await?;
    let dates = daily
        .iter()
        .filter_map(|row| row.try_get::<String, _>("local_date").ok())
        .collect::<Vec<_>>();
    let selected = selected_date
        .filter(|date| dates.iter().any(|value| value == *date))
        .map(str::to_owned)
        .or_else(|| dates.last().cloned());
    let days = daily.iter().map(day_json).collect::<Result<Vec<_>, _>>()?;
    let selected_minutes = selected
        .as_deref()
        .map(|date| {
            minutes
                .iter()
                .filter(|row| row.try_get::<String, _>("local_date").ok().as_deref() == Some(date))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selected_sessions = selected
        .as_deref()
        .map(|date| {
            sessions
                .iter()
                .filter(|row| row.try_get::<String, _>("local_date").ok().as_deref() == Some(date))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selected_daily = selected.as_deref().and_then(|date| {
        daily
            .iter()
            .find(|row| row.try_get::<String, _>("local_date").ok().as_deref() == Some(date))
    });
    let mut selected_metric = Metric::default();
    if let Some(row) = selected_daily {
        selected_metric.add_row(row)?;
    }
    let models = model_json(&selected_sessions)?;
    let session_views = session_json(&selected_sessions)?;
    let validation = validation_json(&ccusage, &daily)?;
    let windows = window_json(&usage_windows, &minutes, &sessions)?;
    let current = current_json(&app, &minutes, &windows, &capacities)?;
    let capacity_values = capacities
        .iter()
        .map(|row| {
            Ok(json!({
                "plan_code": row.try_get::<String, _>("profile_code")?,
                "credit": row.try_get::<f64, _>("weekly_credit")?,
                "weekly_credit": row.try_get::<f64, _>("weekly_credit")?,
                "account_key": row.try_get::<Option<String>, _>("account_key")?,
                "plan_type": row.try_get::<Option<String>, _>("plan_type")?,
                "effective_from_ms": row.try_get::<i64, _>("effective_from_ms")?,
                "effective_to_ms": row.try_get::<Option<i64>, _>("effective_to_ms")?,
                "status": "confirmed"
            }))
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    let selected_day = json!({
        "date": selected,
        "usage": selected_metric.usage_json(),
        "plan_type": selected_daily.and_then(|row| row.try_get::<Option<String>, _>("plan_type").ok().flatten()),
        "capacity_profile": selected_daily.and_then(|row| row.try_get::<Option<String>, _>("capacity_profile").ok().flatten()),
        "credit": selected_metric.credit(),
        "api_usd": selected_metric.api_usd(),
        "minutes": selected_minutes.iter().map(|row| minute_json(row)).collect::<Result<Vec<_>, _>>()?,
        "models": models,
        "sessions": session_views
    });
    let audit = json!({
        "files": Value::Null,
        "lines": daily.len(),
        "events": source_jsonl.iter().filter(|row| {
            row.try_get::<String, _>("kind").ok().as_deref() == Some("usage")
        }).count(),
        "duplicates": Value::Null,
        "sessions": sessions.iter().filter_map(|row| row.try_get::<Option<String>, _>("root_session_id").ok().flatten()).collect::<BTreeSet<_>>().len(),
        "source_rows": source_jsonl.len(),
        "window_rows": usage_windows.len(),
        "ccusage_rows": ccusage.len(),
        "timezone": "Asia/Shanghai"
    });
    Ok(json!({
        "current": current,
        "days": days,
        "selected_day": selected_day,
        "quota_windows": windows,
        "validation": validation,
        "capacities": capacity_values,
        "methodology": {
            "calculation_version": "minimal-r3-materialize",
            "pricing_version": pricing::pricing_version(),
            "sources": ["jsonl", "app_server", "ccusage"],
            "formulas": [
                "total_tokens = non_cached_input + cache_read + output",
                "credit = model/tier subscription price at event time",
                "api_usd = model/tier API price at event time",
                "local_percent = credit / confirmed weekly credit"
            ],
            "price_card": pricing::price_card(),
            "capacity_defaults": pricing::capacity_defaults()
        },
        "audit": audit
    }))
}

fn day_json(row: &sqlx::sqlite::SqliteRow) -> Result<Value, sqlx::Error> {
    let mut metric = Metric::default();
    metric.add_row(row)?;
    Ok(json!({
        "date": row.try_get::<String, _>("local_date")?,
        "usage": metric.usage_json(),
        "credit": metric.credit(),
        "api_usd": metric.api_usd(),
        "plan_type": row.try_get::<Option<String>, _>("plan_type")?,
        "capacity_profile": row.try_get::<Option<String>, _>("capacity_profile")?,
        "local_percent": row.try_get::<Option<f64>, _>("local_percent")?,
        "account_tokens": row.try_get::<Option<i64>, _>("account_tokens")?,
        "unobserved_tokens": row.try_get::<Option<i64>, _>("unobserved_tokens")?,
        "coverage_ratio": row.try_get::<Option<f64>, _>("coverage_ratio")?,
        "account_token_freshness": row.try_get::<Option<String>, _>("account_token_freshness")?,
        "official_percent_start": row.try_get::<Option<f64>, _>("official_percent_start")?,
        "official_percent_end": row.try_get::<Option<f64>, _>("official_percent_end")?,
        "official_percent_delta": row.try_get::<Option<f64>, _>("official_percent_delta")?,
        "reset_count": row.try_get::<i64, _>("reset_count")?,
        "quality": row.try_get::<Option<String>, _>("quality")?
    }))
}

fn minute_json(row: &sqlx::sqlite::SqliteRow) -> Result<Value, sqlx::Error> {
    let mut metric = Metric::default();
    metric.add_row(row)?;
    Ok(json!({
        "minute_start_ms": row.try_get::<i64, _>("minute_start_ms")?,
        "local_date": row.try_get::<String, _>("local_date")?,
        "window_id": row.try_get::<Option<String>, _>("window_id")?,
        "window_kind": row.try_get::<Option<String>, _>("window_kind")?,
        "window_start_ms": row.try_get::<Option<i64>, _>("window_start_ms")?,
        "resets_at_ms": row.try_get::<Option<i64>, _>("resets_at_ms")?,
        "reset_marker": row.try_get::<i64, _>("reset_marker")? != 0,
        "usage": metric.usage_json(),
        "credit": metric.credit(),
        "api_usd": metric.api_usd(),
        "official_used_percent": row.try_get::<Option<f64>, _>("official_used_percent")?,
        "official_source": row.try_get::<Option<String>, _>("official_source")?,
        "quality": row.try_get::<Option<String>, _>("quality")?
    }))
}

fn model_json(rows: &[&sqlx::sqlite::SqliteRow]) -> Result<Vec<Value>, ReportError> {
    let mut models: BTreeMap<(String, String, String), Metric> = BTreeMap::new();
    for row in rows {
        let Some(text) = row.try_get::<Option<String>, _>("model_breakdown_json")? else {
            continue;
        };
        let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        for item in items {
            let Some(model) = item.get("model").and_then(Value::as_str) else {
                continue;
            };
            let tier = item
                .get("tier")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let effort = item
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let entry = models
                .entry((model.to_owned(), tier.clone(), effort.clone()))
                .or_default();
            let total = item
                .get("tokens")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            entry.has_usage = true;
            entry.total += total;
            if let Some(value) = item.get("credit").and_then(Value::as_f64) {
                entry.credit_known = true;
                entry.credit += value;
            } else {
                entry.credit_known = false;
            }
            if let Some(value) = item.get("api_usd").and_then(Value::as_f64) {
                entry.api_known = true;
                entry.api_usd += value;
            } else {
                entry.api_known = false;
            }
        }
    }
    Ok(models
        .into_iter()
        .map(|((model, tier, effort), metric)| {
            json!({"model":model,"tier":tier,"reasoning_effort":effort,"tokens":metric.has_usage.then_some(metric.total),"credit":metric.credit(),"api_usd":metric.api_usd()})
        })
        .collect())
}

fn session_json(rows: &[&sqlx::sqlite::SqliteRow]) -> Result<Vec<Value>, ReportError> {
    let mut groups: BTreeMap<String, Vec<&sqlx::sqlite::SqliteRow>> = BTreeMap::new();
    for row in rows {
        let key = row
            .try_get::<Option<String>, _>("root_session_id")?
            .or_else(|| {
                row.try_get::<Option<String>, _>("session_id")
                    .ok()
                    .flatten()
            })
            .unwrap_or_else(|| "unknown-session".to_owned());
        groups.entry(key).or_default().push(*row);
    }
    let mut output = Vec::new();
    for (root, mut values) in groups {
        values.sort_by_key(|row| {
            (
                row.try_get::<i64, _>("started_at_ms").ok(),
                row.try_get::<i64, _>("id").ok(),
            )
        });
        let first = values[0];
        let mut metric = Metric::default();
        for row in &values {
            metric.add_row(row)?;
        }
        let mut models = BTreeSet::new();
        let mut tiers = BTreeSet::new();
        let mut turns = Vec::new();
        for row in values {
            if let Some(model) = row.try_get::<Option<String>, _>("primary_model")? {
                models.insert(model);
            }
            if let Some(state) = row.try_get::<Option<String>, _>("fast_state")? {
                tiers.insert(state);
            }
            let effort = row
                .try_get::<Option<String>, _>("model_breakdown_json")?
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|value| value.as_array().and_then(|items| items.first().cloned()))
                .and_then(|item| item.get("reasoning_effort").and_then(Value::as_str).map(str::to_owned));
            turns.push(json!({"turn_id":row.try_get::<Option<String>, _>("turn_id")?,"session_id":row.try_get::<Option<String>, _>("session_id")?,"title":row.try_get::<Option<String>, _>("title")?,"started_at_ms":row.try_get::<Option<i64>, _>("started_at_ms")?,"ended_at_ms":row.try_get::<Option<i64>, _>("ended_at_ms")?,"model":row.try_get::<Option<String>, _>("primary_model")?,"fast":row.try_get::<Option<String>, _>("fast_state")?,"reasoning_effort":effort,"usage":row_metric_json(row)?,"credit":row.try_get::<Option<f64>, _>("credit")?,"api_usd":row.try_get::<Option<f64>, _>("api_usd")?}));
        }
        let primary_model = turns
            .first()
            .and_then(|turn| turn.get("model").and_then(Value::as_str).map(str::to_owned));
        let primary_effort = turns
            .first()
            .and_then(|turn| turn.get("reasoning_effort").and_then(Value::as_str).map(str::to_owned));
        output.push(json!({"root_session_id":root,"title":first.try_get::<Option<String>, _>("title")?,"relation":first.try_get::<Option<String>, _>("relation")?,"started_at_ms":values_min_start(&turns),"ended_at_ms":values_max_end(&turns),"primary_model":primary_model,"models":models,"fast":tiers,"reasoning_effort":primary_effort,"usage":metric.usage_json(),"credit":metric.credit(),"api_usd":metric.api_usd(),"turns":turns}));
    }
    Ok(output)
}

fn row_metric_json(row: &sqlx::sqlite::SqliteRow) -> Result<Value, sqlx::Error> {
    let mut metric = Metric::default();
    metric.add_row(row)?;
    Ok(metric.usage_json())
}
fn values_min_start(values: &[Value]) -> Option<i64> {
    values
        .iter()
        .filter_map(|value| value.get("started_at_ms").and_then(Value::as_i64))
        .min()
}
fn values_max_end(values: &[Value]) -> Option<i64> {
    values
        .iter()
        .filter_map(|value| value.get("ended_at_ms").and_then(Value::as_i64))
        .max()
}

fn window_json(
    windows: &[sqlx::sqlite::SqliteRow],
    minutes: &[sqlx::sqlite::SqliteRow],
    sessions: &[sqlx::sqlite::SqliteRow],
) -> Result<Vec<Value>, ReportError> {
    let mut result = Vec::new();
    for row in windows {
        let mut metric = Metric::default();
        metric.add_row(row)?;
        let window_id = row.try_get::<String, _>("window_id")?;
        let session_count = sessions
            .iter()
            .filter(|row| {
                row.try_get::<Option<String>, _>("window_id")
                    .ok()
                    .flatten()
                    .as_deref()
                    == Some(window_id.as_str())
            })
            .count();
        let window_minutes = minutes
            .iter()
            .filter(|minute| {
                minute
                    .try_get::<Option<String>, _>("window_id")
                    .ok()
                    .flatten()
                    .as_deref()
                    == Some(window_id.as_str())
            })
            .map(minute_json)
            .collect::<Result<Vec<_>, _>>()?;
        let window_session_rows = sessions
            .iter()
            .filter(|session| {
                session
                    .try_get::<Option<String>, _>("window_id")
                    .ok()
                    .flatten()
                    .as_deref()
                    == Some(window_id.as_str())
            })
            .collect::<Vec<_>>();
        let window_sessions = session_json(&window_session_rows)?;
        result.push(json!({
            "window_id": window_id,
            "account_key": row.try_get::<Option<String>, _>("account_key")?,
            "limit_id": row.try_get::<Option<String>, _>("limit_id")?,
            "window_kind": row.try_get::<String, _>("window_kind")?,
            "start_at_ms": row.try_get::<Option<i64>, _>("window_start_ms")?,
            "reset_at_ms": row.try_get::<Option<i64>, _>("resets_at_ms")?,
            "window_minutes": row.try_get::<Option<i64>, _>("window_minutes")?,
            "plan_type": row.try_get::<Option<String>, _>("plan_type")?,
            "capacity_profile": row.try_get::<Option<String>, _>("capacity_profile")?,
            "first_official_percent": row.try_get::<Option<f64>, _>("official_percent_start")?,
            "last_official_percent": row.try_get::<Option<f64>, _>("official_percent_end")?,
            "official_delta": row.try_get::<Option<f64>, _>("official_percent_delta")?,
            "percent_delta": row.try_get::<Option<f64>, _>("official_percent_delta")?,
            "local_tokens": metric.has_usage.then_some(metric.total),
            "input_tokens": metric.has_usage.then_some(metric.input),
            "cache_read_tokens": metric.has_usage.then_some(metric.cached),
            "cache_write_tokens": metric.has_usage.then_some(metric.cache_write),
            "output_tokens": metric.has_usage.then_some(metric.output),
            "reasoning_tokens": metric.has_usage.then_some(metric.reasoning),
            "local_credit": metric.credit(),
            "local_api_usd": metric.api_usd(),
            "local_percent": row.try_get::<Option<f64>, _>("local_percent")?,
            "account_tokens": row.try_get::<Option<i64>, _>("account_tokens")?,
            "unobserved_tokens": row.try_get::<Option<i64>, _>("unobserved_tokens")?,
            "coverage_ratio": row.try_get::<Option<f64>, _>("coverage_ratio")?,
            "session_count": session_count,
            "quality": row.try_get::<Option<String>, _>("quality")?,
            "minutes": window_minutes,
            "sessions": window_sessions
        }));
    }
    Ok(result)
}

fn current_json(
    app: &[sqlx::sqlite::SqliteRow],
    minutes: &[sqlx::sqlite::SqliteRow],
    windows: &[Value],
    capacities: &[sqlx::sqlite::SqliteRow],
) -> Result<Value, ReportError> {
    let account = app
        .iter()
        .filter(|row| row.try_get::<String, _>("kind").ok().as_deref() == Some("account"))
        .max_by_key(|row| row.try_get::<i64, _>("last_seen_at_ms").unwrap_or_default());
    let quota = app
        .iter()
        .filter(|row| row.try_get::<String, _>("kind").ok().as_deref() == Some("quota"))
        .filter(|row| {
            row.try_get::<Option<String>, _>("window_kind")
                .ok()
                .flatten()
                .is_none_or(|kind| kind == "primary")
        })
        .filter(|row| {
            row.try_get::<Option<String>, _>("limit_id")
                .ok()
                .flatten()
                .as_deref()
                == Some("codex")
        })
        .max_by_key(|row| row.try_get::<i64, _>("last_seen_at_ms").unwrap_or_default())
        .or_else(|| {
            app.iter()
                .filter(|row| {
                    row.try_get::<String, _>("kind").ok().as_deref() == Some("quota")
                })
                .filter(|row| {
                    row.try_get::<Option<String>, _>("window_kind")
                        .ok()
                        .flatten()
                        .is_none_or(|kind| kind == "primary")
                })
                .max_by_key(|row| row.try_get::<i64, _>("last_seen_at_ms").unwrap_or_default())
        })
        .or_else(|| {
            app.iter()
                .filter(|row| {
                    row.try_get::<String, _>("kind").ok().as_deref() == Some("quota")
                })
                .max_by_key(|row| row.try_get::<i64, _>("last_seen_at_ms").unwrap_or_default())
        });
    let account_key = account.and_then(|row| {
        row.try_get::<Option<String>, _>("account_key")
            .ok()
            .flatten()
    });
    let plan = account.and_then(|row| row.try_get::<Option<String>, _>("plan_type").ok().flatten());
    let used = quota
        .and_then(|row| row.try_get::<Option<f64>, _>("used_percent").ok().flatten())
        .or_else(|| {
            minutes.iter().rev().find_map(|row| {
                row.try_get::<Option<f64>, _>("official_used_percent")
                    .ok()
                    .flatten()
            })
        });
    let reset = quota.and_then(|row| row.try_get::<Option<i64>, _>("resets_at_ms").ok().flatten());
    let window = reset
        .and_then(|reset| {
            windows
                .iter()
                .filter(|window| {
                    window.get("window_kind").and_then(Value::as_str) == Some("primary")
                })
                .min_by_key(|window| {
                    window
                        .get("reset_at_ms")
                        .and_then(Value::as_i64)
                        .map(|value| (value - reset).abs())
                        .unwrap_or(i64::MAX)
                })
        })
        .or_else(|| {
            windows
                .iter()
                .rev()
                .find(|window| window.get("window_kind").and_then(Value::as_str) == Some("primary"))
        })
        .or_else(|| windows.last());
    let weekly_credit = window
        .and_then(|window| window.get("start_at_ms").and_then(Value::as_i64))
        .and_then(|start| {
            capacities
                .iter()
                .filter(|row| {
                    row.try_get::<i64, _>("effective_from_ms")
                        .unwrap_or(i64::MAX)
                        <= start
                })
                .filter(|row| {
                    row.try_get::<Option<String>, _>("account_key")
                        .ok()
                        .flatten()
                        .is_none_or(|key| Some(key) == account_key)
                })
                .filter(|row| {
                    row.try_get::<Option<String>, _>("plan_type")
                        .ok()
                        .flatten()
                        .is_none_or(|value| Some(value) == plan)
                })
                .max_by_key(|row| {
                    row.try_get::<i64, _>("effective_from_ms")
                        .unwrap_or_default()
                })
        })
        .and_then(|row| row.try_get::<f64, _>("weekly_credit").ok());
    let window_value = window.cloned().unwrap_or_else(|| json!({"window_id":Value::Null,"start_at_ms":Value::Null,"reset_at_ms":reset,"local_tokens":Value::Null,"local_credit":Value::Null,"local_api_usd":Value::Null,"local_percent":Value::Null}));
    let local_percent = window_value
        .get("local_credit")
        .and_then(Value::as_f64)
        .zip(weekly_credit)
        .filter(|(_, capacity)| *capacity > 0.0)
        .map(|(credit, capacity)| credit / capacity * 100.0);
    let mut local_window = window_value.clone();
    if let Some(object) = local_window.as_object_mut() {
        object.remove("minutes");
        object.remove("sessions");
        object.insert("local_percent".to_owned(), json!(local_percent));
        object.insert("weekly_credit".to_owned(), json!(weekly_credit));
    }
    let account_daily_tokens = app
        .iter()
        .filter(|row| row.try_get::<String, _>("kind").ok().as_deref() == Some("usage"))
        .max_by_key(|row| row.try_get::<i64, _>("last_seen_at_ms").unwrap_or_default())
        .map(|row| {
            let observed_at_ms = row.try_get::<i64, _>("last_seen_at_ms").unwrap_or_default();
            row.try_get::<Option<String>, _>("daily_tokens_json")
                .ok()
                .flatten()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|item| {
                    let date = item.get("startDate").and_then(Value::as_str)?;
                    let tokens = item.get("tokens").and_then(Value::as_i64)?;
                    Some(json!({
                        "date": date,
                        "tokens": tokens,
                        "source": "app_server",
                        "observed_at_ms": observed_at_ms
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(json!({
        "machine":"local",
        "timezone":"Asia/Shanghai",
        "account":{"display":account.and_then(|row| row.try_get::<Option<String>, _>("account_label").ok().flatten()).or_else(|| plan.clone()),"account_key":account_key,"auth_kind":account.and_then(|row| row.try_get::<Option<String>, _>("auth_kind").ok().flatten()),"plan":plan,"provider":account.and_then(|row| row.try_get::<Option<String>, _>("provider").ok().flatten()),"weekly_credit":weekly_credit,"capacity_credit":weekly_credit,"observed_at_ms":account.and_then(|row| row.try_get::<i64, _>("last_seen_at_ms").ok())},
        "official":{"used_percent":used,"remaining_percent":used.map(|value| (100.0-value).max(0.0)),"resets_at_ms":reset,"window_minutes":quota.and_then(|row| row.try_get::<Option<i64>, _>("window_minutes").ok().flatten()),"source":if quota.is_some(){"app_server"}else{"jsonl"},"window_id":window_value.get("window_id").cloned().unwrap_or(Value::Null)},
        "local_window":local_window,
        "account_daily_tokens":account_daily_tokens,
        "quality_flags": if used.is_none(){json!(["official_unavailable"])} else if weekly_credit.is_none(){json!(["capacity_unconfirmed"])} else {json!([])}
    }))
}

fn validation_json(
    rows: &[sqlx::sqlite::SqliteRow],
    days: &[sqlx::sqlite::SqliteRow],
) -> Result<Value, ReportError> {
    let mut latest_run: BTreeMap<(String, String, String), i64> = BTreeMap::new();
    for row in rows {
        let key = (
            row.try_get("scope")?,
            row.try_get("pricing_scheme")?,
            row.try_get("speed")?,
        );
        let at: i64 = row.try_get("run_at_ms")?;
        latest_run
            .entry(key)
            .and_modify(|value| *value = (*value).max(at))
            .or_insert(at);
    }
    let mut latest = Vec::new();
    let mut comparisons = Vec::new();
    for row in rows {
        let scope: String = row.try_get("scope")?;
        let pricing: String = row.try_get("pricing_scheme")?;
        let speed: String = row.try_get("speed")?;
        let at: i64 = row.try_get("run_at_ms")?;
        if latest_run.get(&(scope.clone(), pricing.clone(), speed.clone())) != Some(&at) {
            continue;
        }
        latest.push(json!({
            "scope": scope,
            "pricing": pricing,
            "speed": speed,
            "status": row.try_get::<String, _>("status")?,
            "scope_key": row.try_get::<String, _>("scope_key")?,
            "input_tokens": row.try_get::<Option<i64>, _>("input_tokens")?,
            "cache_read_tokens": row.try_get::<Option<i64>, _>("cache_read_tokens")?,
            "cache_write_tokens": row.try_get::<Option<i64>, _>("cache_write_tokens")?,
            "output_tokens": row.try_get::<Option<i64>, _>("output_tokens")?,
            "reasoning_tokens": row.try_get::<Option<i64>, _>("reasoning_tokens")?,
            "total_tokens": row.try_get::<Option<i64>, _>("total_tokens")?,
            "amount": row.try_get::<Option<f64>, _>("amount")?,
            "version": row.try_get::<Option<String>, _>("ccusage_version")?,
            "pricing_version": row.try_get::<Option<String>, _>("pricing_version")?,
            "run_at_ms": at
        }));
        if let Some(day) = days.iter().find(|day| {
            day.try_get::<String, _>("local_date").ok().as_deref()
                == row.try_get::<String, _>("scope_key").ok().as_deref()
        }) {
            let local = day.try_get::<Option<i64>, _>("total_tokens")?;
            let other = row.try_get::<Option<i64>, _>("total_tokens")?;
            comparisons.push(json!({"date":row.try_get::<String,_>("scope_key")?,"metric":"total_tokens","local":local,"ccusage":other,"diff":local.zip(other).map(|(left,right)| right-left),"pricing":pricing,"speed":speed,"status":if local.zip(other).is_some_and(|(left,right)| left==right){"match"}else{"diff"}}));
        }
    }
    Ok(json!({"latest":latest,"comparisons":comparisons}))
}
