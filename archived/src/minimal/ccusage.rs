//! Small black-box bridge for the locally installed `ccusage` executable.
//!
//! JSONL remains the production fact source.  This module only runs the
//! daily/session reports, removes path-like fields, and stores compact rows in
//! `source_ccusage`.

use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use super::{Database, SourceCcusageRecord};

const SCOPES: [&str; 2] = ["daily", "session"];
const SPEEDS: [&str; 2] = ["auto", "standard"];
const SCHEMES: [&str; 2] = ["api", "subscription"];

#[derive(Debug, Error)]
pub enum CcusageError {
    #[error("ccusage command bridge failed: {0}")]
    Command(String),
    #[error("ccusage database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("ccusage database wrapper error: {0}")]
    Db(#[from] super::db::DbError),
    #[error("ccusage JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
pub struct CcusageCollector {
    home: PathBuf,
    timezone: String,
    binary: PathBuf,
    run_on_boot: bool,
    run_on_refresh: bool,
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
            binary: std::env::var_os("CODEX_METER_CCUSAGE_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("ccusage")),
            run_on_boot: env_flag("CODEX_METER_CCUSAGE_ON_BOOT"),
            run_on_refresh: env_flag("CODEX_METER_CCUSAGE_ON_REFRESH"),
        }
    }

    pub fn disabled(home: impl Into<PathBuf>, timezone: impl Into<String>) -> Self {
        let mut collector = Self::from_env(home, timezone);
        collector.run_on_boot = false;
        collector.run_on_refresh = false;
        collector
    }

    pub fn run_on_boot(&self) -> bool {
        self.run_on_boot
    }

    pub fn run_on_refresh(&self) -> bool {
        self.run_on_refresh
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
                "ok".to_owned()
            } else if succeeded == 0 {
                "failed".to_owned()
            } else {
                "partial".to_owned()
            },
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
    let Some(rows) = rows else {
        database
            .upsert_source_ccusage(&source_record(result, "__run__", None))
            .await?;
        return Ok(());
    };
    if rows.is_empty() {
        database
            .upsert_source_ccusage(&source_record(result, "__run__", None))
            .await?;
        return Ok(());
    }
    for row in rows {
        let scope_key = match result.scope.as_str() {
            "daily" => row.get("date").and_then(Value::as_str).unwrap_or("unknown"),
            "session" => row
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            _ => "unknown",
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
    let amount = row
        .and_then(|value| value.get("costUSD"))
        .and_then(Value::as_f64);
    let model_breakdown_json = row
        .and_then(|value| value.get("models"))
        .and_then(|value| serde_json::to_string(value).ok());
    SourceCcusageRecord {
        source_key: format!(
            "ccusage:{}:{}:{}:{}:{}",
            result.run_at_key(),
            result.scope,
            scope_key,
            result.pricing,
            result.speed
        ),
        run_at_ms: result.finished_at_ms,
        range_start_ms: result.started_at_ms,
        range_end_ms: result.finished_at_ms,
        scope: result.scope.clone(),
        scope_key: scope_key.to_owned(),
        pricing_scheme: result.pricing.clone(),
        speed: result.speed.clone(),
        input_tokens: token(&["inputTokens"]),
        cache_read_tokens: token(&["cacheReadTokens"]),
        cache_write_tokens: token(&["cacheCreationTokens"]),
        output_tokens: token(&["outputTokens"]),
        reasoning_tokens: token(&["reasoningOutputTokens"]),
        total_tokens: token(&["totalTokens"]),
        amount,
        model_breakdown_json,
        ccusage_version: result.version.clone(),
        pricing_version: Some(format!("codex-meter:{}", result.pricing)),
        status: result.status.clone(),
    }
}

impl CommandResult {
    fn run_at_key(&self) -> i64 {
        self.started_at_ms
    }
}

fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|value| value as i64))
}

fn run_commands(collector: &CcusageCollector) -> Vec<CommandResult> {
    let version = ccusage_version(collector);
    let mut results = Vec::with_capacity(SCOPES.len() * SPEEDS.len() * SCHEMES.len());
    let temp_dir = std::env::temp_dir().join(format!(
        "codex-meter-ccusage-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let _ = fs::create_dir_all(&temp_dir);
    for pricing in SCHEMES {
        let config_path = write_config(&temp_dir, pricing);
        for scope in SCOPES {
            for speed in SPEEDS {
                results.push(run_one(
                    collector,
                    pricing,
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

fn write_config(directory: &PathBuf, scheme: &str) -> Option<PathBuf> {
    let path = directory.join(format!("{scheme}.json"));
    let config = json!({
        "defaults": {
            "offline": true,
            "pricingOverrides": super::pricing::ccusage_overrides(scheme)
        }
    });
    fs::write(&path, serde_json::to_vec(&config).ok()?).ok()?;
    Some(path)
}

fn run_one(
    collector: &CcusageCollector,
    pricing: &str,
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
    if let Some(config_path) = config_path {
        args.push("--config".to_owned());
        args.push(config_path.to_string_lossy().into_owned());
    }
    let output = command.args(args).output();
    let (status, data) = match output {
        Ok(output) if output.status.success() => {
            match serde_json::from_slice::<Value>(&output.stdout) {
                Ok(value) => ("ok".to_owned(), value),
                Err(_) => ("failed".to_owned(), json!({"error": "invalid_json"})),
            }
        }
        Ok(output) => (
            "failed".to_owned(),
            json!({
                "error": "command_exit",
                "exit_code": output.status.code()
            }),
        ),
        Err(error) => (
            "failed".to_owned(),
            json!({"error": "command_unavailable", "kind": error.kind().to_string()}),
        ),
    };
    CommandResult {
        scope: scope.to_owned(),
        pricing: pricing.to_owned(),
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
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn command_with_home(collector: &CcusageCollector) -> Command {
    let mut command = Command::new(&collector.binary);
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
    let key = key.to_ascii_lowercase();
    [
        "directory",
        "sessionfile",
        "prompt",
        "message",
        "content",
        "access_token",
        "refresh_token",
        "api_key",
        "authorization",
        "email",
    ]
    .iter()
    .any(|private| key == *private)
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

    #[test]
    fn sanitization_removes_paths_and_prompt_like_fields() {
        let value = json!({
            "sessionId": "safe",
            "directory": "/private/path",
            "sessionFile": "/private/file",
            "prompt": "do not store",
            "models": [{"model": "gpt-5", "totalTokens": 10}]
        });
        let sanitized = sanitize_value(&value, None);
        assert_eq!(sanitized["sessionId"], "safe");
        assert!(sanitized.get("directory").is_none());
        assert!(sanitized.get("sessionFile").is_none());
        assert!(sanitized.get("prompt").is_none());
        assert_eq!(sanitized["models"][0]["totalTokens"], 10);
    }

    #[test]
    fn source_record_keeps_only_normalized_ccusage_fields() {
        let result = CommandResult {
            scope: "daily".to_owned(),
            pricing: "api".to_owned(),
            speed: "auto".to_owned(),
            started_at_ms: 10,
            finished_at_ms: 20,
            version: Some("ccusage 20.0.19".to_owned()),
            status: "ok".to_owned(),
            data: Value::Null,
        };
        let row = json!({
            "date": "2026-08-04",
            "inputTokens": 2,
            "cacheReadTokens": 3,
            "cacheCreationTokens": 4,
            "outputTokens": 5,
            "reasoningOutputTokens": 1,
            "totalTokens": 10,
            "costUSD": 1.25,
            "models": {"gpt-5": {"totalTokens": 10}}
        });
        let record = source_record(&result, "2026-08-04", Some(&row));
        assert_eq!(record.total_tokens, Some(10));
        assert_eq!(record.cache_write_tokens, Some(4));
        assert_eq!(record.amount, Some(1.25));
        assert!(record.model_breakdown_json.is_some());
        assert_eq!(record.status, "ok");
    }

    #[tokio::test]
    async fn persists_normalized_daily_source_rows() {
        let database = Database::connect_in_memory().await.unwrap();
        let result = CommandResult {
            scope: "daily".to_owned(),
            pricing: "api".to_owned(),
            speed: "standard".to_owned(),
            started_at_ms: 10,
            finished_at_ms: 20,
            version: Some("ccusage 20.0.19".to_owned()),
            status: "ok".to_owned(),
            data: Value::Null,
        };
        persist_source_rows(
            &database,
            &result,
            &json!({"daily": [{"date": "2026-08-04", "totalTokens": 10}]}),
        )
        .await
        .unwrap();
        let row: (String, i64) =
            sqlx::query_as("SELECT scope_key, total_tokens FROM source_ccusage LIMIT 1")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(row, ("2026-08-04".to_owned(), 10));
    }
}
