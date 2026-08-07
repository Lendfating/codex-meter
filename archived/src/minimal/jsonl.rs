use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use super::{db::DbError, Database, EventRecord, QuotaRecord, SourceJsonlRecord, TokenCounts};

#[derive(Debug, Error)]
pub enum JsonlError {
    #[error("JSONL I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSONL cursor JSON error: {0}")]
    CursorJson(#[from] serde_json::Error),
    #[error("JSONL database error: {0}")]
    Database(#[from] DbError),
    #[error("JSONL timestamp is invalid: {0}")]
    Timestamp(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JsonlScanReport {
    pub files_scanned: usize,
    pub files_changed: usize,
    pub complete_lines: usize,
    pub recognized_events: usize,
    pub inserted_events: usize,
    pub duplicate_events: usize,
    pub inserted_quota_samples: usize,
}

#[derive(Clone, Debug)]
pub struct JsonlCollector {
    codex_home: PathBuf,
}

#[derive(Clone, Debug, Default)]
struct FileContext {
    session_id: Option<String>,
    root_session_id: Option<String>,
    turn_id: Option<String>,
    turn_started_at_ms: Option<i64>,
    model: Option<String>,
    provider: Option<String>,
    plan: Option<String>,
    tier: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct CursorStore {
    files: BTreeMap<String, CursorState>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CursorState {
    offset_bytes: u64,
    mtime_ms: Option<i64>,
    digest: Option<String>,
}

impl JsonlCollector {
    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
        }
    }

    pub fn with_timezone(self, _timezone: impl Into<String>) -> Self {
        self
    }

    pub fn home_display(&self) -> String {
        self.codex_home.to_string_lossy().into_owned()
    }

    pub fn discover_paths(&self) -> Result<Vec<(PathBuf, &'static str)>, JsonlError> {
        let mut paths = Vec::new();
        collect_jsonl(&self.codex_home.join("sessions"), "active", &mut paths)?;
        collect_jsonl(
            &self.codex_home.join("archived_sessions"),
            "archived",
            &mut paths,
        )?;
        paths.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(paths)
    }

    pub async fn scan_once(&self, database: &Database) -> Result<JsonlScanReport, JsonlError> {
        let cursor_path = database.sidecar_path("jsonl-cursors.json");
        let mut cursors = load_cursors(cursor_path.as_deref())?;
        let paths = self.discover_paths()?;
        let mut report = JsonlScanReport {
            files_scanned: paths.len(),
            ..JsonlScanReport::default()
        };
        for (path, state) in paths {
            self.scan_file(database, &path, state, &mut cursors, &mut report)
                .await?;
            save_cursors(cursor_path.as_deref(), &cursors)?;
        }
        Ok(report)
    }

    async fn scan_file(
        &self,
        database: &Database,
        path: &Path,
        _state: &'static str,
        cursors: &mut CursorStore,
        report: &mut JsonlScanReport,
    ) -> Result<(), JsonlError> {
        let metadata = fs::metadata(path)?;
        let bytes = fs::read(path)?;
        let path_key = path.to_string_lossy().into_owned();
        let mtime_ms = metadata.modified().ok().and_then(system_time_ms);
        let existing = cursors.files.get(&path_key);
        let mut start = existing
            .map(|value| value.offset_bytes as usize)
            .unwrap_or_default();
        if start > bytes.len() {
            start = 0;
        }
        let chunk = &bytes[start..];
        let complete_bytes = if chunk.ends_with(b"\n") {
            chunk.len()
        } else {
            chunk
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map(|index| index + 1)
                .unwrap_or(0)
        };
        let complete = &chunk[..complete_bytes];
        let mut context = FileContext::default();
        let mut line_count = 0_i64;
        for line in complete.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            line_count += 1;
            let Ok(value) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            if let Some(turn) = parse_turn_record(&value, &mut context)? {
                database.upsert_source_jsonl(&turn).await?;
            }
            let Some((event, rate_limits)) = parse_event(line, &value, &mut context)? else {
                continue;
            };
            report.recognized_events += 1;
            let event_observed_at_ms = event.observed_at_ms;
            let inserted = database.upsert_source_jsonl_event(&event).await?;
            if inserted {
                report.inserted_events += 1;
            } else {
                report.duplicate_events += 1;
            }
            if let Some(rate_limits) = rate_limits {
                for sample in rate_limit_records(&rate_limits, &context, event_observed_at_ms) {
                    if database
                        .upsert_source_jsonl_quota(&source_quota_key(&sample), &sample)
                        .await?
                    {
                        report.inserted_quota_samples += 1;
                    }
                }
            }
        }
        if complete_bytes > 0 || existing.is_none() {
            report.files_changed += 1;
        }
        report.complete_lines += line_count as usize;
        let next_offset = (start + complete_bytes) as i64;
        let digest = if next_offset > 0 {
            Some(hex_digest(&bytes[..next_offset as usize]))
        } else {
            None
        };
        cursors.files.insert(
            path_key,
            CursorState {
                offset_bytes: next_offset.max(0) as u64,
                mtime_ms,
                digest,
            },
        );
        Ok(())
    }
}

fn load_cursors(path: Option<&Path>) -> Result<CursorStore, JsonlError> {
    let Some(path) = path else {
        return Ok(CursorStore::default());
    };
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(CursorStore::default()),
        Err(error) => Err(JsonlError::Io(error)),
    }
}

fn save_cursors(path: Option<&Path>, cursors: &CursorStore) -> Result<(), JsonlError> {
    let Some(path) = path else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("jsonl-cursors.json.tmp-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(cursors)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn collect_jsonl(
    root: &Path,
    state: &'static str,
    paths: &mut Vec<(PathBuf, &'static str)>,
) -> Result<(), JsonlError> {
    if !root.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, state, paths)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            paths.push((path, state));
        }
    }
    Ok(())
}

fn parse_event(
    raw_line: &[u8],
    value: &Value,
    context: &mut FileContext,
) -> Result<Option<(EventRecord, Option<Value>)>, JsonlError> {
    let observed_at_ms = parse_timestamp(value.get("timestamp"))?;
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = value.get("payload").unwrap_or(&Value::Null);
    let mut kind = event_type.to_owned();
    let mut session_id = context.session_id.clone();
    let mut root_session_id = None;
    let mut title = None;
    let mut model = None;
    let mut tier = None;
    let mut provider = context.provider.clone();
    let mut plan = context.plan.clone();
    let mut last_tokens = None;
    let mut cumulative_tokens = None;
    let mut quota_json = None;

    if event_type == "session_meta" {
        session_id = payload
            .get("session_id")
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        context.session_id = session_id.clone();
        provider = payload
            .get("model_provider")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| provider.clone());
        context.provider = provider.clone();
        title = payload
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned);
        root_session_id = payload
            .get("forked_from_id")
            .or_else(|| payload.get("parent_thread_id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        context.root_session_id = root_session_id.clone();
    } else if event_type == "event_msg"
        && payload.get("type").and_then(Value::as_str) == Some("thread_settings_applied")
    {
        kind = "thread_settings".to_owned();
        let settings = payload.get("thread_settings").unwrap_or(&Value::Null);
        model = settings
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        context.model = model.clone();
        tier = settings
            .get("service_tier")
            .and_then(Value::as_str)
            .and_then(normalize_tier)
            .map(str::to_owned);
        context.tier = tier.clone();
        context.provider = settings
            .get("model_provider_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| context.provider.clone());
        provider = context.provider.clone();
    } else if event_type == "event_msg"
        && payload.get("type").and_then(Value::as_str) == Some("token_count")
    {
        kind = "token_count".to_owned();
        let info = payload.get("info").unwrap_or(&Value::Null);
        last_tokens = info.get("last_token_usage").and_then(parse_token_counts);
        cumulative_tokens = info.get("total_token_usage").and_then(parse_token_counts);
        if last_tokens.is_none() && cumulative_tokens.is_none() {
            return Ok(None);
        }
        quota_json = value
            .get("rate_limits")
            .cloned()
            .or_else(|| payload.get("rate_limits").cloned());
        if let Some(rate_limits) = quota_json.as_ref() {
            plan = rate_limits
                .get("plan_type")
                .or_else(|| rate_limits.get("plan_type_raw"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| plan.clone());
            context.plan = plan.clone();
        }
        tier = context.tier.clone();
        model = context.model.clone();
    } else {
        return Ok(None);
    }

    let digest = event_digest(&kind, session_id.as_deref(), raw_line);
    let fast = tier.as_deref().map(|value| value == "fast");
    Ok(Some((
        EventRecord {
            source_digest: digest,
            observed_at_ms,
            kind,
            session_id,
            root_session_id,
            title,
            model,
            tier,
            provider,
            account_key: None,
            plan,
            last_tokens,
            cumulative_tokens,
            fast,
            quota_json: quota_json.clone(),
            payload: value.clone(),
            quality: Vec::new(),
        },
        quota_json,
    )))
}

fn parse_turn_record(
    value: &Value,
    context: &mut FileContext,
) -> Result<Option<SourceJsonlRecord>, JsonlError> {
    let observed_at_ms = parse_timestamp(value.get("timestamp"))?;
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = value.get("payload").unwrap_or(&Value::Null);
    let inner_type = if event_type == "event_msg" {
        payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    } else {
        event_type
    };
    if inner_type == "turn_context" {
        context.turn_id = payload
            .get("turn_id")
            .or_else(|| payload.get("turnId"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        return Ok(None);
    }
    if !matches!(inner_type, "task_started" | "task_complete") {
        return Ok(None);
    }
    let turn_id = payload
        .get("turn_id")
        .or_else(|| payload.get("turnId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| context.turn_id.clone());
    let Some(turn_id) = turn_id else {
        return Ok(None);
    };
    if context.turn_id.as_deref() != Some(turn_id.as_str()) {
        context.turn_started_at_ms = None;
    }
    context.turn_id = Some(turn_id.clone());
    if inner_type == "task_started" {
        context.turn_started_at_ms = payload
            .get("started_at")
            .or_else(|| payload.get("startedAt"))
            .and_then(as_epoch_ms)
            .or(Some(observed_at_ms));
    }
    let started_at_ms = context.turn_started_at_ms.or(Some(observed_at_ms));
    let ended_at_ms = (inner_type == "task_complete").then(|| {
        payload
            .get("completed_at")
            .or_else(|| payload.get("completedAt"))
            .and_then(as_epoch_ms)
            .unwrap_or(observed_at_ms)
    });
    Ok(Some(SourceJsonlRecord {
        source_key: format!(
            "turn:{}:{}",
            context.session_id.as_deref().unwrap_or("unknown"),
            turn_id
        ),
        kind: "turn".to_owned(),
        observed_at_ms,
        session_id: context.session_id.clone(),
        root_session_id: context.root_session_id.clone(),
        turn_id: Some(turn_id),
        relation: Some(
            if context.root_session_id.is_some() {
                "fork"
            } else {
                "main"
            }
            .to_owned(),
        ),
        started_at_ms,
        ended_at_ms,
        model: context.model.clone(),
        service_tier: context.tier.clone(),
        provider: context.provider.clone(),
        plan_type: context.plan.clone(),
        ..SourceJsonlRecord::default()
    }))
}

fn rate_limit_records(
    value: &Value,
    context: &FileContext,
    observed_at_ms: i64,
) -> Vec<QuotaRecord> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let limit_id = object
        .get("limit_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let plan = object
        .get("plan_type")
        .or_else(|| object.get("plan_type_raw"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| context.plan.clone());
    let mut records = Vec::new();
    for window_kind in ["primary", "secondary"] {
        let Some(window) = object.get(window_kind).filter(|value| !value.is_null()) else {
            continue;
        };
        let used_percent = window.get("used_percent").and_then(Value::as_f64);
        let window_minutes = window.get("window_minutes").and_then(Value::as_i64);
        let resets_at_ms = window
            .get("resets_at_ms")
            .or_else(|| window.get("resets_at"))
            .and_then(as_epoch_ms);
        if used_percent.is_none() && resets_at_ms.is_none() {
            continue;
        }
        records.push(QuotaRecord {
            event_id: 0,
            source_digest: format!("{}:{window_kind}", hex_digest(value.to_string().as_bytes())),
            observed_at_ms,
            account_key: None,
            limit_id: limit_id.clone(),
            window_kind: window_kind.to_owned(),
            used_percent,
            window_minutes,
            resets_at_ms,
            plan: plan.clone(),
            source: "jsonl".to_owned(),
            raw_json: json!({"limit_id": limit_id, "window_kind": window_kind, "window": window}),
        });
    }
    records
}

fn parse_token_counts(value: &Value) -> Option<TokenCounts> {
    Some(TokenCounts {
        input: value.get("input_tokens")?.as_i64()?,
        cached: value
            .get("cached_input_tokens")
            .or_else(|| value.get("cache_read_input_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        cache_write: value
            .get("cache_write_input_tokens")
            .or_else(|| value.get("cache_creation_input_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        output: value
            .get("output_tokens")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        reasoning: value
            .get("reasoning_output_tokens")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        total: value
            .get("total_tokens")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
    })
}

fn parse_timestamp(value: Option<&Value>) -> Result<i64, JsonlError> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| JsonlError::Timestamp("missing timestamp".to_owned()))?;
    OffsetDateTime::parse(value, &Rfc3339)
        .map(|date| date.unix_timestamp_nanos() / 1_000_000)
        .map(|value| value as i64)
        .map_err(|error| JsonlError::Timestamp(error.to_string()))
}

fn as_epoch_ms(value: &Value) -> Option<i64> {
    let number = value
        .as_i64()
        .or_else(|| value.as_f64().map(|value| value as i64))?;
    Some(if number.abs() < 100_000_000_000 {
        number.saturating_mul(1_000)
    } else {
        number
    })
}

fn normalize_tier(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "standard" | "default" => Some("standard"),
        "fast" | "priority" => Some("fast"),
        _ => None,
    }
}

fn event_digest(kind: &str, session_id: Option<&str>, raw_line: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(session_id.unwrap_or_default().as_bytes());
    hasher.update([0]);
    hasher.update(raw_line);
    hex::encode(hasher.finalize())
}

fn hex_digest(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    hex::encode(hasher.finalize())
}

fn source_quota_key(sample: &QuotaRecord) -> String {
    let value = json!({
        "limit_id": sample.limit_id,
        "window_kind": sample.window_kind,
        "used_percent": sample.used_percent,
        "window_minutes": sample.window_minutes,
        "resets_at_ms": sample.resets_at_ms,
        "plan": sample.plan,
    });
    format!("quota:{}", hex_digest(value.to_string().as_bytes()))
}

fn system_time_ms(value: SystemTime) -> Option<i64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn temp_home() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("codex-meter-minimal-jsonl-{nonce}"))
    }

    #[tokio::test]
    async fn scans_fixture_and_deduplicates_second_pass() {
        let root = temp_home();
        fs::create_dir_all(root.join("sessions/2026/08")).unwrap();
        fs::copy(
            "fixtures/jsonl/codex-session-plus-quota-sanitized.jsonl",
            root.join("sessions/2026/08/session.jsonl"),
        )
        .unwrap();
        let database = Database::connect_in_memory().await.unwrap();
        let collector = JsonlCollector::new(&root);
        let first = collector.scan_once(&database).await.unwrap();
        let second = collector.scan_once(&database).await.unwrap();
        assert!(first.inserted_events > 0);
        assert!(first.inserted_quota_samples > 0);
        assert_eq!(second.inserted_events, 0);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_jsonl")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert!(count > 0);
        let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_jsonl")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert!(source_count > 0);
        let usage_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM source_jsonl WHERE kind = 'usage'")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert!(usage_count > 0);
        let quota_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM source_jsonl WHERE kind = 'quota'")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert!(quota_count > 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn turn_lifecycle_uses_one_stable_source_key() {
        let mut context = FileContext {
            session_id: Some("session-1".to_owned()),
            ..FileContext::default()
        };
        let started = json!({
            "timestamp": "2026-08-04T06:41:51.881Z",
            "type": "event_msg",
            "payload": {
                "type": "task_started",
                "turn_id": "turn-1",
                "started_at": 1785825711
            }
        });
        let first = parse_turn_record(&started, &mut context).unwrap().unwrap();
        assert_eq!(first.kind, "turn");
        assert_eq!(first.turn_id.as_deref(), Some("turn-1"));
        assert!(first.ended_at_ms.is_none());

        let completed = json!({
            "timestamp": "2026-08-04T06:42:01.881Z",
            "type": "event_msg",
            "payload": {
                "type": "task_complete",
                "turn_id": "turn-1",
                "completed_at": 1785825721
            }
        });
        let second = parse_turn_record(&completed, &mut context)
            .unwrap()
            .unwrap();
        assert_eq!(second.source_key, first.source_key);
        assert_eq!(second.started_at_ms, first.started_at_ms);
        assert!(second.ended_at_ms.is_some());
    }
}
