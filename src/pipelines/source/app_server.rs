//! Minimal Codex App Server bridge.
//!
//! Only account identity, rate limits and account usage are requested.  The
//! complete protocol response is sanitized and discarded after extracting the
//! source-table fields.

use std::{
    collections::HashSet,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::db::{Database, DbError, SourceAppServerRecord};

#[derive(Debug, Error)]
pub enum AppServerError {
    #[error("App Server I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("App Server JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("App Server database error: {0}")]
    Database(#[from] DbError),
    #[error("App Server timed out")]
    Timeout,
}

#[derive(Clone, Debug)]
pub struct AppServerConfig {
    pub program: String,
    pub args: Vec<String>,
    pub poll_interval: Duration,
    pub read_timeout: Duration,
}

impl Default for AppServerConfig {
    fn default() -> Self {
        Self {
            program: "codex".to_owned(),
            args: vec!["app-server".to_owned(), "--stdio".to_owned()],
            poll_interval: Duration::from_secs(60),
            read_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct AppServerScanReport {
    pub lines_seen: usize,
    pub account_rows: usize,
    pub quota_rows: usize,
    pub usage_rows: usize,
}

#[derive(Clone, Debug, Default)]
struct IngestContext {
    account_key: Option<String>,
    account_label: Option<String>,
    auth_kind: Option<String>,
    provider: Option<String>,
    plan_type: Option<String>,
}

pub async fn spawn_supervisor(
    database: Database,
    config: AppServerConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_usage_at = std::time::Instant::now()
            .checked_sub(Duration::from_secs(6 * 60 * 60))
            .unwrap_or_else(std::time::Instant::now);
        loop {
            let usage_due = last_usage_at.elapsed() >= Duration::from_secs(6 * 60 * 60);
            match poll_once(&database, &config, usage_due).await {
                Ok(report) => eprintln!(
                    "App Server: {} lines, {} account, {} quota, {} usage",
                    report.lines_seen, report.account_rows, report.quota_rows, report.usage_rows
                ),
                Err(error) => eprintln!("App Server unavailable; JSONL remains primary: {error}"),
            }
            if usage_due {
                last_usage_at = std::time::Instant::now();
            }
            tokio::time::sleep(config.poll_interval).await;
        }
    })
}

pub async fn poll_once(
    database: &Database,
    config: &AppServerConfig,
    include_usage: bool,
) -> Result<AppServerScanReport, AppServerError> {
    let command = config.clone();
    let lines = match tokio::task::spawn_blocking(move || run_command(&command, include_usage))
        .await
        .map_err(|error| AppServerError::Io(std::io::Error::other(error.to_string())))?
    {
        Ok(lines) => lines,
        Err(error) => {
            record_unavailable(database, now_ms()).await?;
            return Err(error);
        }
    };
    let mut report = AppServerScanReport::default();
    let mut context = IngestContext::default();
    for line in lines {
        report.lines_seen += 1;
        let value: Value = serde_json::from_str(&line)?;
        ingest_value(database, &value, now_ms(), &mut context, &mut report).await?;
    }
    Ok(report)
}

pub async fn ingest_line(
    database: &Database,
    line: &str,
    fallback_observed_at_ms: i64,
) -> Result<AppServerScanReport, AppServerError> {
    let value: Value = serde_json::from_str(line)?;
    let mut report = AppServerScanReport {
        lines_seen: 1,
        ..Default::default()
    };
    ingest_value(
        database,
        &value,
        fallback_observed_at_ms,
        &mut IngestContext::default(),
        &mut report,
    )
    .await?;
    Ok(report)
}

fn run_command(
    config: &AppServerConfig,
    include_usage: bool,
) -> Result<Vec<String>, AppServerError> {
    let mut child = Command::new(&config.program)
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "App Server stdin unavailable",
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "App Server stdout unavailable",
        )
    })?;
    let requests = [
        serde_json::json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"codex-meter","title":"Codex Meter","version":"minimal-r2"},"capabilities":null}}),
        serde_json::json!({"method":"initialized"}),
        // The current Codex App Server rejects `null` for these request
        // params.  An empty object is the protocol's no-argument payload and
        // keeps account and quota snapshots available on real installations.
        serde_json::json!({"id":2,"method":"account/read","params":{}}),
        serde_json::json!({"id":3,"method":"account/rateLimits/read","params":{}}),
    ];
    for request in requests {
        writeln!(stdin, "{}", serde_json::to_string(&request)?)?;
    }
    if include_usage {
        writeln!(
            stdin,
            "{{\"id\":4,\"method\":\"account/usage/read\",\"params\":{{}}}}"
        )?;
    }
    stdin.flush()?;
    // Keep the request pipe open while reading responses.  Current Codex App
    // Server treats EOF as a disconnected client and can drop queued replies
    // before account/quota/usage results are emitted.
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().take(64).flatten() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let wanted = if include_usage { 4 } else { 3 };
    let mut seen_ids = HashSet::new();
    let mut lines = Vec::new();
    while seen_ids.len() < wanted {
        let line = receiver
            .recv_timeout(config.read_timeout)
            .map_err(|_| AppServerError::Timeout)?;
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            if let Some(id) = value.get("id").and_then(Value::as_i64) {
                seen_ids.insert(id);
            }
        }
        lines.push(line);
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(lines)
}

async fn ingest_value(
    database: &Database,
    message: &Value,
    fallback_observed_at_ms: i64,
    context: &mut IngestContext,
    report: &mut AppServerScanReport,
) -> Result<(), AppServerError> {
    if message.get("error").is_some() {
        record_unavailable(database, fallback_observed_at_ms).await?;
        return Ok(());
    }
    let method = message.get("method").and_then(Value::as_str);
    let payload = message
        .get("params")
        .or_else(|| message.get("result"))
        .cloned()
        .unwrap_or(Value::Null);
    let observed_at_ms = find_value(&payload, &["observed_at", "observedAt", "timestamp"])
        .and_then(epoch_ms)
        .unwrap_or(fallback_observed_at_ms);
    let result = message.get("result").unwrap_or(&payload);

    if method == Some("account/read") || result.get("account").is_some() {
        let account = result.get("account").unwrap_or(result);
        let email = find_value(account, &["email"]).and_then(Value::as_str);
        context.account_key = email.map(stable_key).or_else(|| {
            find_value(account, &["id", "accountId"])
                .and_then(Value::as_str)
                .map(stable_key)
        });
        context.account_label = context
            .account_key
            .as_deref()
            .map(|key| format!("account-{}", &key[key.len().saturating_sub(8)..]));
        context.plan_type = find_value(account, &["planType", "plan_type"])
            .and_then(Value::as_str)
            .map(str::to_owned);
        context.provider = find_value(account, &["provider", "providerName"])
            .and_then(Value::as_str)
            .map(str::to_owned);
        context.auth_kind = find_value(account, &["type", "authType", "auth_kind"])
            .and_then(Value::as_str)
            .map(str::to_owned);
        report.account_rows += 1;
        let state = serde_json::json!({
            "account_key": context.account_key,
            "account_label": context.account_label,
            "auth_kind": context.auth_kind,
            "provider": context.provider,
            "plan_type": context.plan_type,
        });
        database
            .upsert_source_app_server(&SourceAppServerRecord {
                source_key: state_digest("account", &state),
                kind: "account".to_owned(),
                first_seen_at_ms: observed_at_ms,
                last_seen_at_ms: observed_at_ms,
                account_key: context.account_key.clone(),
                account_label: context.account_label.clone(),
                auth_kind: context.auth_kind.clone(),
                provider: context.provider.clone(),
                plan_type: context.plan_type.clone(),
                freshness: Some("settled".to_owned()),
                status: "ok".to_owned(),
                ..Default::default()
            })
            .await?;
    }

    if method == Some("account/rateLimits/read")
        || find_value(result, &["rateLimits", "rate_limits"]).is_some()
    {
        let limits = find_value(result, &["rateLimits", "rate_limits"]).unwrap_or(result);
        // Codex currently returns both a single `rateLimits` object and a
        // `rateLimitsByLimitId` map.  Prefer the latter when present so the
        // same quota is not ingested twice; keep the older map/array shapes as
        // compatibility fallbacks for recorded protocol fixtures.
        let by_limit_id = find_value(result, &["rateLimitsByLimitId", "rate_limits_by_limit_id"]);
        if let Some(object) = by_limit_id.and_then(Value::as_object) {
            for (fallback_id, limit) in object {
                insert_limit(
                    database,
                    limit,
                    Some(fallback_id),
                    observed_at_ms,
                    context,
                    report,
                )
                .await?;
            }
        } else if let Some(items) = limits.as_array() {
            for limit in items {
                insert_limit(database, limit, None, observed_at_ms, context, report).await?;
            }
        } else if limits
            .as_object()
            .is_some_and(|object| object.contains_key("limitId") || object.contains_key("primary"))
        {
            insert_limit(database, limits, None, observed_at_ms, context, report).await?;
        } else if let Some(object) = limits.as_object() {
            for (fallback_id, limit) in object {
                insert_limit(
                    database,
                    limit,
                    Some(fallback_id),
                    observed_at_ms,
                    context,
                    report,
                )
                .await?;
            }
        }
    }

    if method == Some("account/usage/read")
        || find_value(
            result,
            &[
                "dailyUsageBuckets",
                "daily_usage_buckets",
                "lifetimeTokens",
                "lifetime_tokens",
            ],
        )
        .is_some()
    {
        let lifetime = find_value(result, &["lifetimeTokens", "lifetime_tokens"]).and_then(as_i64);
        let daily = find_value(result, &["dailyUsageBuckets", "daily_usage_buckets"])
            .cloned()
            .or_else(|| {
                find_value(result, &["summary"]).and_then(|summary| {
                    find_value(summary, &["dailyUsageBuckets", "daily_usage_buckets"]).cloned()
                })
            })
            .unwrap_or_else(|| Value::Array(Vec::new()));
        report.usage_rows += 1;
        let daily = sanitize(daily);
        let daily_tokens_json = serde_json::to_string(&daily)?;
        let state = serde_json::json!({
            "account_key": context.account_key,
            "lifetime_tokens": lifetime,
            "daily_tokens": daily,
        });
        database
            .upsert_source_app_server(&SourceAppServerRecord {
                source_key: state_digest("usage", &state),
                kind: "usage".to_owned(),
                first_seen_at_ms: observed_at_ms,
                last_seen_at_ms: observed_at_ms,
                account_key: context.account_key.clone(),
                account_label: context.account_label.clone(),
                auth_kind: context.auth_kind.clone(),
                provider: context.provider.clone(),
                plan_type: context.plan_type.clone(),
                lifetime_tokens: lifetime,
                daily_tokens_json: Some(daily_tokens_json),
                freshness: Some("settled".to_owned()),
                status: "ok".to_owned(),
                ..Default::default()
            })
            .await?;
    }
    Ok(())
}

async fn insert_limit(
    database: &Database,
    limit: &Value,
    fallback_id: Option<&str>,
    observed_at_ms: i64,
    context: &IngestContext,
    report: &mut AppServerScanReport,
) -> Result<(), AppServerError> {
    let limit_id = find_value(limit, &["limitId", "limit_id", "id"])
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| fallback_id.map(str::to_owned));
    let Some(limit_id) = limit_id else {
        return Ok(());
    };
    let plan = find_value(limit, &["planType", "plan_type"])
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| context.plan_type.clone());
    for kind in ["primary", "secondary"] {
        let Some(window) = limit.get(kind) else {
            continue;
        };
        let used = find_value(window, &["usedPercent", "used_percent"]).and_then(Value::as_f64);
        let minutes = find_value(
            window,
            &[
                "windowMinutes",
                "window_minutes",
                "windowDurationMins",
                "window_duration_mins",
            ],
        )
        .and_then(as_i64);
        let reset =
            find_value(window, &["resetsAt", "resets_at", "resets_at_ms"]).and_then(epoch_ms);
        if used.is_none() && reset.is_none() {
            continue;
        }
        report.quota_rows += 1;
        // Hash only the fields that define quota state.  The complete response
        // often contains request IDs or unrelated fields; including them would
        // create a new row on every poll even when the quota is unchanged.
        let state = serde_json::json!({
            "account_key": context.account_key,
            "limit_id": limit_id,
            "plan": plan,
            "window_kind": kind,
            "used_percent": used,
            "window_minutes": minutes,
            "resets_at_ms": reset,
        });
        database
            .upsert_source_app_server(&SourceAppServerRecord {
                source_key: state_digest("quota", &state),
                kind: "quota".to_owned(),
                first_seen_at_ms: observed_at_ms,
                last_seen_at_ms: observed_at_ms,
                account_key: context.account_key.clone(),
                account_label: context.account_label.clone(),
                auth_kind: context.auth_kind.clone(),
                provider: context.provider.clone(),
                plan_type: plan.clone(),
                limit_id: Some(limit_id.clone()),
                window_kind: Some(kind.to_owned()),
                used_percent: used,
                window_minutes: minutes,
                resets_at_ms: reset,
                freshness: Some("settled".to_owned()),
                status: "ok".to_owned(),
                ..Default::default()
            })
            .await?;
    }
    Ok(())
}

async fn record_unavailable(
    database: &Database,
    observed_at_ms: i64,
) -> Result<(), AppServerError> {
    database
        .upsert_source_app_server(&SourceAppServerRecord {
            source_key: "app_server:unavailable".to_owned(),
            kind: "account".to_owned(),
            first_seen_at_ms: observed_at_ms,
            last_seen_at_ms: observed_at_ms,
            freshness: Some("unavailable".to_owned()),
            status: "unavailable".to_owned(),
            ..Default::default()
        })
        .await?;
    Ok(())
}

fn find_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    value
        .as_object()
        .and_then(|object| keys.iter().find_map(|key| object.get(*key)))
}

fn epoch_ms(value: &Value) -> Option<i64> {
    if let Some(text) = value.as_str() {
        if let Ok(date) =
            time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        {
            return i64::try_from(date.unix_timestamp_nanos() / 1_000_000).ok();
        }
    }
    let value = value
        .as_i64()
        .or_else(|| value.as_f64().map(|value| value as i64))?;
    Some(if value.abs() < 100_000_000_000 {
        value.saturating_mul(1_000)
    } else {
        value
    })
}

fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|value| value as i64))
}

fn sanitize(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .filter_map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    if [
                        "directory",
                        "sessionfile",
                        "session_file",
                        "access_token",
                        "accesstoken",
                        "refresh_token",
                        "refreshtoken",
                        "api_key",
                        "apikey",
                        "authorization",
                        "prompt",
                        "message",
                        "content",
                        "email",
                    ]
                    .contains(&lower.as_str())
                    {
                        None
                    } else {
                        Some((key, sanitize(value)))
                    }
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(sanitize).collect()),
        other => other,
    }
}

fn stable_key(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.trim().to_ascii_lowercase().as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn state_digest(scope: &str, value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.as_bytes());
    hasher.update([0]);
    hasher.update(value.to_string().as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    #[tokio::test]
    async fn stores_sanitized_account_quota_and_usage() {
        let database = Database::connect_in_memory().await.unwrap();
        let account = r#"{"id":1,"result":{"account":{"type":"chatgpt","email":"Alice@example.com","planType":"plus","provider":"openai","access_token":"secret"}}}"#;
        let report = ingest_line(&database, account, 1_700_000_000_000)
            .await
            .unwrap();
        assert_eq!(report.account_rows, 1);
        let rows = database.list_source_app_server().await.unwrap();
        let saved = rows
            .iter()
            .find(|row| row.try_get::<String, _>("kind").unwrap() == "account")
            .map(|row| {
                format!(
                    "{}{}",
                    row.try_get::<Option<String>, _>("account_key")
                        .unwrap()
                        .unwrap_or_default(),
                    row.try_get::<Option<String>, _>("account_label")
                        .unwrap()
                        .unwrap_or_default()
                )
            })
            .unwrap_or_default();
        assert!(!saved.contains("secret"));
        let quota = r#"{"id":2,"result":{"rateLimits":{"weekly":{"limitId":"weekly","planType":"plus","primary":{"usedPercent":12,"windowMinutes":10080,"resetsAt":1786351410}}}}}"#;
        assert_eq!(
            ingest_line(&database, quota, 1_700_000_000_000)
                .await
                .unwrap()
                .quota_rows,
            1
        );
        let usage = r#"{"id":3,"result":{"lifetimeTokens":42,"dailyUsageBuckets":[{"startDate":"2026-08-04","tokens":42}]}}"#;
        assert_eq!(
            ingest_line(&database, usage, 1_700_000_000_000)
                .await
                .unwrap()
                .usage_rows,
            1
        );
        assert_eq!(database.list_source_app_server().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn quota_state_keeps_first_and_last_seen_without_poll_growth() {
        let database = Database::connect_in_memory().await.unwrap();
        let quota = |used: i64| {
            format!(
                r#"{{"id":2,"result":{{"rateLimits":{{"weekly":{{"limitId":"weekly","primary":{{"usedPercent":{used},"windowMinutes":10080,"resetsAt":1786351410}}}}}}}}}}"#
            )
        };
        ingest_line(&database, &quota(12), 1_000).await.unwrap();
        ingest_line(&database, &quota(12), 2_000).await.unwrap();
        let rows = database.list_source_app_server().await.unwrap();
        let quota_rows = rows
            .iter()
            .filter(|row| row.try_get::<String, _>("kind").unwrap() == "quota")
            .collect::<Vec<_>>();
        assert_eq!(quota_rows.len(), 1);
        assert_eq!(
            quota_rows[0].try_get::<i64, _>("first_seen_at_ms").unwrap(),
            1_000
        );
        assert_eq!(
            quota_rows[0].try_get::<i64, _>("last_seen_at_ms").unwrap(),
            2_000
        );

        ingest_line(&database, &quota(13), 3_000).await.unwrap();
        let rows = database.list_source_app_server().await.unwrap();
        assert_eq!(
            rows.iter()
                .filter(|row| row.try_get::<String, _>("kind").unwrap() == "quota")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn parses_current_rate_limits_and_rate_limits_by_id_shapes() {
        let database = Database::connect_in_memory().await.unwrap();
        let response = r#"{"id":3,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":97,"windowDurationMins":10080,"resetsAt":1786351410},"secondary":null,"planType":"plus"},"rateLimitsByLimitId":{"codex":{"limitId":"codex","primary":{"usedPercent":97,"windowDurationMins":10080,"resetsAt":1786351410},"secondary":null,"planType":"plus"}}}}"#;
        let report = ingest_line(&database, response, 1_700_000_000_000)
            .await
            .unwrap();
        assert_eq!(report.quota_rows, 1);
        let row = database
            .list_source_app_server()
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.try_get::<String, _>("kind").unwrap() == "quota")
            .unwrap();
        assert_eq!(
            row.try_get::<Option<String>, _>("limit_id")
                .unwrap()
                .as_deref(),
            Some("codex")
        );
        assert_eq!(
            row.try_get::<Option<i64>, _>("window_minutes").unwrap(),
            Some(10080)
        );
        assert_eq!(
            row.try_get::<Option<f64>, _>("used_percent").unwrap(),
            Some(97.0)
        );
        assert_eq!(
            row.try_get::<Option<i64>, _>("resets_at_ms").unwrap(),
            Some(1_786_351_410_000)
        );
    }

    #[tokio::test]
    async fn protocol_error_is_saved_as_unavailable_without_zero_values() {
        let database = Database::connect_in_memory().await.unwrap();
        ingest_line(
            &database,
            r#"{"id":4,"error":{"code":"server_unavailable","message":"offline"}}"#,
            4_000,
        )
        .await
        .unwrap();
        let row = database
            .list_source_app_server()
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(row.try_get::<String, _>("status").unwrap(), "unavailable");
        assert_eq!(
            row.try_get::<Option<String>, _>("freshness")
                .unwrap()
                .as_deref(),
            Some("unavailable")
        );
        assert!(row
            .try_get::<Option<i64>, _>("lifetime_tokens")
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    #[ignore = "requires a real authenticated Codex App Server and runtime database"]
    async fn real_app_server_poll_when_requested() {
        if std::env::var("CODEX_METER_RUN_REAL_APP_SERVER")
            .ok()
            .as_deref()
            != Some("1")
        {
            return;
        }
        let database = Database::connect(".runtime/codex-meter.sqlite")
            .await
            .unwrap();
        let config = AppServerConfig {
            program: "codex".to_owned(),
            args: vec!["app-server".to_owned(), "--stdio".to_owned()],
            poll_interval: Duration::from_secs(60),
            read_timeout: Duration::from_secs(30),
        };
        let report = poll_once(&database, &config, true).await.unwrap();
        eprintln!("real App Server poll: {report:?}");
    }
}
