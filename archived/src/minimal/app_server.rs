use std::{
    collections::HashSet,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use super::{rollup::refresh_rollups, Database, DbError, SourceAppServerRecord};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppServerError {
    #[error("App Server I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("App Server JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("App Server database error: {0}")]
    Database(#[from] DbError),
    #[error("App Server SQL error: {0}")]
    Sqlx(#[from] sqlx::Error),
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
            program: std::env::var("CODEX_METER_APP_SERVER_BIN")
                .unwrap_or_else(|_| "codex".to_owned()),
            args: vec!["app-server".to_owned(), "--stdio".to_owned()],
            poll_interval: Duration::from_secs(60),
            read_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AppServerScanReport {
    pub lines_seen: usize,
    pub account_rows: usize,
    pub quota_rows: usize,
    pub usage_rows: usize,
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
                Ok(report) => {
                    eprintln!(
                        "App Server: {} lines, {} account, {} quota, {} usage",
                        report.lines_seen,
                        report.account_rows,
                        report.quota_rows,
                        report.usage_rows
                    );
                    if let Err(error) = refresh_rollups(&database).await {
                        eprintln!("Rollup refresh after App Server failed: {error}");
                    }
                }
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
    let lines = tokio::task::spawn_blocking(move || run_command(&command, include_usage))
        .await
        .map_err(|error| AppServerError::Io(std::io::Error::other(error.to_string())))??;
    let mut report = AppServerScanReport::default();
    for line in lines {
        report.lines_seen += 1;
        let value: Value = serde_json::from_str(&line)?;
        ingest_value(database, &value, now_ms(), &mut report).await?;
    }
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
        serde_json::json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"codex-meter","title":"Codex Meter","version":"minimal-r1"},"capabilities":null}}),
        serde_json::json!({"method":"initialized"}),
        serde_json::json!({"id":2,"method":"account/read","params":null}),
        serde_json::json!({"id":3,"method":"account/rateLimits/read","params":null}),
    ];
    for request in requests {
        writeln!(stdin, "{}", serde_json::to_string(&request)?)?;
    }
    if include_usage {
        writeln!(
            stdin,
            "{{\"id\":4,\"method\":\"account/usage/read\",\"params\":null}}"
        )?;
    }
    stdin.flush()?;
    drop(stdin);
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().take(64) {
            if let Ok(line) = line {
                if sender.send(line).is_err() {
                    break;
                }
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
    ingest_value(database, &value, fallback_observed_at_ms, &mut report).await?;
    Ok(report)
}

async fn ingest_value(
    database: &Database,
    message: &Value,
    fallback_observed_at_ms: i64,
    report: &mut AppServerScanReport,
) -> Result<(), AppServerError> {
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
        let plan = find_value(account, &["planType", "plan_type"])
            .and_then(Value::as_str)
            .map(str::to_owned);
        let provider = find_value(account, &["provider", "providerName"])
            .and_then(Value::as_str)
            .map(str::to_owned);
        let email = find_value(account, &["email"]).and_then(Value::as_str);
        let account_key = email
            .map(stable_key)
            .or_else(|| plan.clone().map(|value| stable_key(&value)));
        let sanitized = sanitize(message.clone());
        report.account_rows += 1;
        database
            .upsert_source_app_server(&SourceAppServerRecord {
                source_key: state_digest("account", &sanitized),
                kind: "account".to_owned(),
                first_seen_at_ms: observed_at_ms,
                last_seen_at_ms: observed_at_ms,
                account_key: account_key.clone(),
                auth_kind: find_value(account, &["type", "authType", "auth_kind"])
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                provider: provider.clone(),
                plan_type: plan.clone(),
                freshness: Some("settled".to_owned()),
                status: "ok".to_owned(),
                ..SourceAppServerRecord::default()
            })
            .await?;
    }
    if method == Some("account/rateLimits/read")
        || find_value(result, &["rateLimits", "rate_limits"]).is_some()
    {
        let limits = find_value(result, &["rateLimits", "rate_limits"]).unwrap_or(result);
        let object = limits.as_object().cloned().unwrap_or_default();
        for (fallback_id, limit) in object {
            insert_limit(database, &limit, Some(&fallback_id), observed_at_ms, report).await?;
        }
        if let Some(items) = limits.as_array() {
            for limit in items {
                insert_limit(database, limit, None, observed_at_ms, report).await?;
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
        let sanitized = sanitize(message.clone());
        report.usage_rows += 1;
        database
            .upsert_source_app_server(&SourceAppServerRecord {
                source_key: state_digest("usage", &sanitized),
                kind: "usage".to_owned(),
                first_seen_at_ms: observed_at_ms,
                last_seen_at_ms: observed_at_ms,
                lifetime_tokens: lifetime,
                daily_tokens_json: Some(serde_json::to_string(&daily)?),
                freshness: Some("settled".to_owned()),
                status: "ok".to_owned(),
                ..SourceAppServerRecord::default()
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
        .map(str::to_owned);
    for kind in ["primary", "secondary"] {
        let Some(window) = limit.get(kind) else {
            continue;
        };
        let used = find_value(window, &["usedPercent", "used_percent"]).and_then(Value::as_f64);
        let minutes = find_value(window, &["windowMinutes", "window_minutes"]).and_then(as_i64);
        let reset = find_value(window, &["resetsAt", "resets_at"]).and_then(epoch_ms);
        if used.is_none() && reset.is_none() {
            continue;
        }
        let raw = sanitize(
            serde_json::json!({"limit_id": limit_id, "plan": plan, "window_kind": kind, "window": window}),
        );
        report.quota_rows += 1;
        database
            .upsert_source_app_server(&SourceAppServerRecord {
                source_key: state_digest(&format!("quota:{kind}"), &raw),
                kind: "quota".to_owned(),
                first_seen_at_ms: observed_at_ms,
                last_seen_at_ms: observed_at_ms,
                limit_id: Some(limit_id.clone()),
                window_kind: Some(kind.to_owned()),
                used_percent: used,
                window_minutes: minutes,
                resets_at_ms: reset,
                plan_type: plan.clone(),
                freshness: Some("settled".to_owned()),
                status: "ok".to_owned(),
                ..SourceAppServerRecord::default()
            })
            .await?;
    }
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
                        "access_token",
                        "accessToken",
                        "refresh_token",
                        "refreshToken",
                        "api_key",
                        "apiKey",
                        "authorization",
                        "prompt",
                    ]
                    .iter()
                    .any(|blocked| lower == blocked.to_ascii_lowercase())
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
    use crate::minimal::build_report;

    #[tokio::test]
    async fn stores_account_quota_and_usage_without_secrets() {
        let database = Database::connect_in_memory().await.unwrap();
        let account = r#"{"id":1,"result":{"account":{"type":"chatgpt","email":"Alice@example.com","planType":"plus","provider":"openai","access_token":"secret"}}}"#;
        let report = ingest_line(&database, account, 1_700_000_000_000)
            .await
            .unwrap();
        assert_eq!(report.account_rows, 1);
        let saved: String = sqlx::query_scalar(
            "SELECT COALESCE(account_key, '') || COALESCE(account_label, '')
             FROM source_app_server WHERE kind = 'account' LIMIT 1",
        )
        .fetch_one(database.pool())
        .await
        .unwrap();
        assert!(!saved.contains("secret"));
        let quota = r#"{"id":2,"result":{"rateLimits":{"weekly":{"limitId":"weekly","planType":"plus","primary":{"usedPercent":12,"windowMinutes":10080,"resetsAt":1786351410}}}}}"#;
        let report = ingest_line(&database, quota, 1_700_000_000_000)
            .await
            .unwrap();
        assert_eq!(report.quota_rows, 1);
        let usage = r#"{"id":3,"result":{"lifetimeTokens":42,"dailyUsageBuckets":[{"startDate":"2026-08-04","tokens":42}]}}"#;
        let report = ingest_line(&database, usage, 1_700_000_000_000)
            .await
            .unwrap();
        assert_eq!(report.usage_rows, 1);
        let report = build_report(&database, None).await.unwrap();
        assert_eq!(report["current"]["account"]["plan"], "plus");
        assert_eq!(report["current"]["official"]["source"], "app_server");
        assert_eq!(report["current"]["account_daily_tokens"][0]["tokens"], 42);
        let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_app_server")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(source_count, 3);
    }
}
