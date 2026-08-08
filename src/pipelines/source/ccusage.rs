//! Black-box ccusage validator.
//!
//! It never becomes the production ledger: JSONL remains the local source of
//! truth and these rows are only used for visible reconciliation.

use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::{
    db::{Database, DbError, SourceCcusageRecord},
    pricing,
};

const SCOPES: [&str; 2] = ["daily", "session"];
const SPEEDS: [&str; 2] = ["auto", "standard"];
const SCHEMES: [&str; 2] = ["api", "subscription"];
const CCUSAGE_PACKAGE: &str = "ccusage@latest";
const NPM_REGISTRY: &str = "https://registry.npmjs.org";

#[derive(Debug, Error)]
pub enum CcusageError {
    #[error("ccusage command bridge failed: {0}")]
    Command(String),
    #[error("ccusage database error: {0}")]
    Database(#[from] DbError),
    #[error("ccusage JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
enum CcusageLauncher {
    Binary(PathBuf),
    Npx,
}

impl CcusageLauncher {
    fn from_env() -> Self {
        if let Some(binary) = find_command("ccusage") {
            return Self::Binary(binary);
        }
        if find_command("npx").is_some() {
            return Self::Npx;
        }
        // Keep the conventional command name as the final fallback so the
        // resulting error still identifies the missing CLI clearly.
        Self::Binary(PathBuf::from("ccusage"))
    }

    fn command(&self) -> Command {
        match self {
            Self::Binary(binary) => Command::new(binary),
            Self::Npx => {
                let mut command = Command::new("npx");
                command.args(["--yes", "--registry", NPM_REGISTRY, CCUSAGE_PACKAGE]);
                command
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct CcusageCollector {
    home: PathBuf,
    timezone: String,
    launcher: CcusageLauncher,
    enabled: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CcusageRunSummary {
    pub status: String,
    pub runs: usize,
    pub succeeded: usize,
}

#[derive(Clone, Debug)]
struct CommandResult {
    scope: String,
    pricing: String,
    speed: String,
    started_at_ms: i64,
    finished_at_ms: i64,
    version: Option<String>,
    status: String,
    data: Value,
}

impl CcusageCollector {
    pub fn from_env(home: impl Into<PathBuf>, timezone: impl Into<String>) -> Self {
        Self {
            home: home.into(),
            timezone: timezone.into(),
            launcher: CcusageLauncher::from_env(),
            enabled: env_flag("CODEX_METER_CCUSAGE_ON"),
        }
    }

    pub fn disabled(home: impl Into<PathBuf>, timezone: impl Into<String>) -> Self {
        let mut collector = Self::from_env(home, timezone);
        collector.enabled = false;
        collector
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub async fn run_once(&self, database: &Database) -> Result<CcusageRunSummary, CcusageError> {
        let collector = self.clone();
        let results = tokio::task::spawn_blocking(move || run_commands(&collector))
            .await
            .map_err(|error| CcusageError::Command(error.to_string()))?;
        let mut succeeded = 0;
        for result in &results {
            if result.status == "ok" {
                succeeded += 1;
            }
            let sanitized = sanitize_value(&result.data, None);
            persist_source_rows(database, result, &sanitized).await?;
        }
        Ok(CcusageRunSummary {
            status: if succeeded == results.len() {
                "ok"
            } else if succeeded == 0 {
                "failed"
            } else {
                "partial"
            }
            .to_owned(),
            runs: results.len(),
            succeeded,
        })
    }
}

async fn persist_source_rows(
    database: &Database,
    result: &CommandResult,
    data: &Value,
) -> Result<(), CcusageError> {
    let rows = match result.scope.as_str() {
        "daily" => data.get("daily").and_then(Value::as_array),
        "session" => data.get("sessions").and_then(Value::as_array),
        _ => None,
    };
    let Some(rows) = rows.filter(|rows| !rows.is_empty()) else {
        database
            .upsert_source_ccusage(&source_record(result, "__run__", None))
            .await?;
        return Ok(());
    };
    for row in rows {
        let scope_key = if result.scope == "daily" {
            row.get("date").and_then(Value::as_str).unwrap_or("unknown")
        } else {
            row.get("sessionId")
                .or_else(|| row.get("session_id"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        };
        database
            .upsert_source_ccusage(&source_record(result, scope_key, Some(row)))
            .await?;
    }
    Ok(())
}

fn source_record(
    result: &CommandResult,
    scope_key: &str,
    row: Option<&Value>,
) -> SourceCcusageRecord {
    let row = row.and_then(Value::as_object);
    let token = |keys: &[&str]| {
        row.and_then(|value| keys.iter().find_map(|key| value.get(*key)))
            .and_then(as_i64)
    };
    SourceCcusageRecord {
        source_key: format!(
            "ccusage:{}:{}:{}:{}:{}",
            result.started_at_ms, result.scope, scope_key, result.pricing, result.speed
        ),
        run_at_ms: result.finished_at_ms,
        range_start_ms: result.started_at_ms,
        range_end_ms: result.finished_at_ms,
        scope: result.scope.clone(),
        scope_key: scope_key.to_owned(),
        pricing_scheme: result.pricing.clone(),
        speed: result.speed.clone(),
        input_tokens: token(&["inputTokens", "input_tokens"]),
        cache_read_tokens: token(&["cacheReadTokens", "cache_read_tokens"]),
        cache_write_tokens: token(&[
            "cacheCreationTokens",
            "cacheWriteTokens",
            "cache_write_tokens",
        ]),
        output_tokens: token(&["outputTokens", "output_tokens"]),
        reasoning_tokens: token(&["reasoningOutputTokens", "reasoning_tokens"]),
        total_tokens: token(&["totalTokens", "total_tokens"]),
        amount: row
            .and_then(|value| value.get("costUSD").or_else(|| value.get("amount")))
            .and_then(Value::as_f64),
        model_breakdown_json: row
            .and_then(|value| value.get("models"))
            .and_then(|value| serde_json::to_string(value).ok()),
        ccusage_version: result.version.clone(),
        pricing_version: Some(pricing::pricing_version()),
        status: result.status.clone(),
    }
}

fn run_commands(collector: &CcusageCollector) -> Vec<CommandResult> {
    let version = ccusage_version(collector);
    let temp_dir = std::env::temp_dir().join(format!(
        "codex-meter-ccusage-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let _ = fs::create_dir_all(&temp_dir);
    let mut results = Vec::with_capacity(8);
    for pricing_scheme in SCHEMES {
        let config_path = write_config(&temp_dir, pricing_scheme);
        for scope in SCOPES {
            for speed in SPEEDS {
                results.push(run_one(
                    collector,
                    pricing_scheme,
                    scope,
                    speed,
                    version.clone(),
                    config_path.as_deref(),
                ));
            }
        }
    }
    let _ = fs::remove_dir_all(temp_dir);
    results
}

fn write_config(directory: &std::path::Path, scheme: &str) -> Option<PathBuf> {
    let path = directory.join(format!("{scheme}.json"));
    let config =
        json!({"defaults":{"offline":true,"pricingOverrides":pricing::ccusage_overrides(scheme)}});
    fs::write(&path, serde_json::to_vec(&config).ok()?).ok()?;
    Some(path)
}

fn run_one(
    collector: &CcusageCollector,
    pricing_scheme: &str,
    scope: &str,
    speed: &str,
    version: Option<String>,
    config_path: Option<&std::path::Path>,
) -> CommandResult {
    let started_at_ms = now_ms();
    let mut command = command_with_home(collector);
    let mut args = vec![
        "codex".to_owned(),
        scope.to_owned(),
        "--json".to_owned(),
        "--offline".to_owned(),
        "--speed".to_owned(),
        speed.to_owned(),
        "--timezone".to_owned(),
        collector.timezone.clone(),
    ];
    if let Some(path) = config_path {
        args.extend(["--config".to_owned(), path.to_string_lossy().into_owned()]);
    }
    let output = command.args(args).output();
    let (status, data) = match output {
        Ok(output) if output.status.success() => {
            match serde_json::from_slice::<Value>(&output.stdout) {
                Ok(value) => ("ok".to_owned(), value),
                Err(_) => ("failed".to_owned(), json!({"error":"invalid_json"})),
            }
        }
        Ok(output) => (
            "failed".to_owned(),
            json!({"error":"command_exit","exit_code":output.status.code()}),
        ),
        Err(error) => (
            "failed".to_owned(),
            json!({"error":"command_unavailable","kind":error.kind().to_string()}),
        ),
    };
    CommandResult {
        scope: scope.to_owned(),
        pricing: pricing_scheme.to_owned(),
        speed: speed.to_owned(),
        started_at_ms,
        finished_at_ms: now_ms(),
        version,
        status,
        data,
    }
}

fn ccusage_version(collector: &CcusageCollector) -> Option<String> {
    let output = command_with_home(collector)
        .arg("--version")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        })
        .flatten()
}

fn command_with_home(collector: &CcusageCollector) -> Command {
    let mut command = collector.launcher.command();
    command.env("CODEX_HOME", &collector.home);
    if collector
        .home
        .file_name()
        .is_some_and(|name| name == ".codex")
    {
        if let Some(parent) = collector.home.parent() {
            command.env("HOME", parent);
        }
    }
    command
}

fn sanitize_value(value: &Value, key: Option<&str>) -> Value {
    if key.is_some_and(is_private_key) {
        return Value::Null;
    }
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| !is_private_key(key))
                .map(|(key, value)| (key.clone(), sanitize_value(value, Some(key))))
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| sanitize_value(value, None))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn is_private_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "directory"
            | "sessionfile"
            | "session_file"
            | "prompt"
            | "message"
            | "content"
            | "accesstoken"
            | "access_token"
            | "refreshtoken"
            | "refresh_token"
            | "apikey"
            | "api_key"
            | "authorization"
            | "email"
    )
}

fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|value| value as i64))
}

fn find_command(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    #[test]
    fn sanitization_drops_private_fields() {
        let value = json!({"sessionId":"safe","directory":"/private","prompt":"secret","models":[{"totalTokens":10}]});
        let sanitized = sanitize_value(&value, None);
        assert!(sanitized.get("directory").is_none());
        assert!(sanitized.get("prompt").is_none());
        assert_eq!(sanitized["models"][0]["totalTokens"], 10);
    }

    #[test]
    fn source_record_normalizes_tokens() {
        let result = CommandResult {
            scope: "daily".to_owned(),
            pricing: "api".to_owned(),
            speed: "auto".to_owned(),
            started_at_ms: 10,
            finished_at_ms: 20,
            version: Some("ccusage".to_owned()),
            status: "ok".to_owned(),
            data: Value::Null,
        };
        let row = json!({"date":"2026-08-04","inputTokens":2,"cacheReadTokens":3,"cacheCreationTokens":4,"outputTokens":5,"totalTokens":10,"costUSD":1.25,"models":{"gpt-5": {"totalTokens":10}}});
        let record = source_record(&result, "2026-08-04", Some(&row));
        assert_eq!(record.total_tokens, Some(10));
        assert_eq!(record.cache_write_tokens, Some(4));
        assert_eq!(record.amount, Some(1.25));
    }

    /// Opt-in real-source run used by the first-pipeline acceptance check.  It
    /// writes only normalized ccusage rows to the caller-provided source table
    /// database; normal tests never invoke a user's Codex home.
    #[tokio::test]
    async fn real_ccusage_run_when_requested() {
        let Some(db_path) = std::env::var_os("CODEX_METER_REAL_CCUSAGE_DB") else {
            return;
        };
        let Some(home) = std::env::var_os("CODEX_METER_REAL_CCUSAGE_HOME") else {
            panic!("CODEX_METER_REAL_CCUSAGE_HOME is required for a real ccusage run");
        };
        let database = Database::connect(db_path).await.unwrap();
        let mut collector = CcusageCollector::from_env(home, "Asia/Shanghai");
        collector.enabled = false;
        let summary = collector.run_once(&database).await.unwrap();
        let rows = database.list_source_ccusage().await.unwrap();
        eprintln!(
            "real ccusage: summary={summary:?}, persisted_rows={}, ok_rows={}",
            rows.len(),
            rows.iter()
                .filter(|row| row.try_get::<String, _>("status").unwrap() == "ok")
                .count()
        );
        assert_eq!(summary.runs, 8);
        assert!(summary.succeeded > 0);
        assert!(!rows.is_empty());
    }
}
