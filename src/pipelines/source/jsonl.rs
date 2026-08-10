//! JSONL source adapter.
//!
//! Only a white-list of session, turn, token and quota facts is kept.  Raw
//! lines, prompts and tool payloads never cross this module's boundary.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{sqlite::SqliteConnectOptions, sqlite::SqlitePoolOptions, Row};
use thiserror::Error;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

mod fast;

use crate::{
    db::{Database, DbError, SourceJsonlRecord},
    pricing::TokenCounts,
};

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
    #[error("JSONL scanner worker panicked")]
    WorkerPanic,
}

fn default_from_date() -> time::Date {
    OffsetDateTime::now_utc()
        .to_offset(time::UtcOffset::from_hms(8, 0, 0).unwrap_or(time::UtcOffset::UTC))
        .date()
        - time::Duration::days(29)
}

fn parse_date(value: &str) -> Result<time::Date, JsonlError> {
    let mut parts = value.split('-');
    let year = parts
        .next()
        .and_then(|part| part.parse::<i32>().ok());
    let month = parts
        .next()
        .and_then(|part| part.parse::<u8>().ok());
    let day = parts
        .next()
        .and_then(|part| part.parse::<u8>().ok());
    if parts.next().is_some() {
        return Err(JsonlError::Timestamp(format!(
            "invalid JSONL from date: {value}"
        )));
    }
    let (Some(year), Some(month), Some(day)) = (year, month, day) else {
        return Err(JsonlError::Timestamp(format!(
            "invalid JSONL from date: {value}"
        )));
    };
    let month = time::Month::try_from(month).map_err(|_| {
        JsonlError::Timestamp(format!("invalid JSONL from date: {value}"))
    })?;
    time::Date::from_calendar_date(year, month, day).map_err(|_| {
        JsonlError::Timestamp(format!("invalid JSONL from date: {value}"))
    })
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
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
    cursor_path: Option<PathBuf>,
    root_repaired: Arc<AtomicBool>,
    from_date: Option<time::Date>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct FileContext {
    session_id: Option<String>,
    parent_session_id: Option<String>,
    root_session_id: Option<String>,
    relation: Option<String>,
    turn_id: Option<String>,
    turn_started_at_ms: Option<i64>,
    model: Option<String>,
    provider: Option<String>,
    plan: Option<String>,
    tier: Option<String>,
    reasoning_effort: Option<String>,
    previous_cumulative: Option<TokenCounts>,
}

/// The first-batch equivalent of ccusage's replay plan.  Codex fork logs can
/// start by copying the parent's token events; those copied events are facts
/// about the parent, not new usage by the child.  We filter them while the
/// source file is parsed so raw source rows remain a faithful, non-duplicated
/// event stream for the rollup pipeline.
#[derive(Clone, Debug, Default)]
struct ReplaySpec {
    parent_prefix: Vec<TokenCounts>,
    fallback_burst_len: usize,
}

#[derive(Clone, Debug)]
enum ReplayState {
    Match {
        prefix: Vec<TokenCounts>,
        index: usize,
        fallback_burst_len: usize,
    },
    SkipBurst {
        remaining: usize,
    },
    Done,
}

impl ReplayState {
    fn new(spec: Option<&ReplaySpec>) -> Option<Self> {
        let spec = spec?;
        if spec.parent_prefix.is_empty() && spec.fallback_burst_len == 0 {
            return None;
        }
        Some(Self::Match {
            prefix: spec.parent_prefix.clone(),
            index: 0,
            fallback_burst_len: spec.fallback_burst_len,
        })
    }

    fn should_skip(&mut self, tokens: &TokenCounts) -> bool {
        loop {
            match self {
                Self::Match {
                    prefix,
                    index,
                    fallback_burst_len,
                } => {
                    if let Some(expected) = prefix.get(*index) {
                        if expected == tokens {
                            *index += 1;
                            return true;
                        }
                    }
                    if *index == 0 && *fallback_burst_len > 0 {
                        let remaining = *fallback_burst_len;
                        *self = Self::SkipBurst { remaining };
                    } else {
                        *self = Self::Done;
                    }
                }
                Self::SkipBurst { remaining } => {
                    if *remaining == 0 {
                        *self = Self::Done;
                    } else {
                        *remaining -= 1;
                        return true;
                    }
                }
                Self::Done => return false,
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
struct LocalSessionMetadata {
    title: Option<String>,
    parent_session_id: Option<String>,
    reasoning_effort: Option<String>,
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
    context: FileContext,
}

impl JsonlCollector {
    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
            cursor_path: Some(PathBuf::from(".runtime/jsonl-cursors.json")),
            root_repaired: Arc::new(AtomicBool::new(false)),
            from_date: None,
        }
    }

    pub fn with_cursor_path(mut self, path: Option<PathBuf>) -> Self {
        self.cursor_path = path;
        self
    }

    pub fn with_from_date(mut self, from_date: Option<time::Date>) -> Self {
        self.from_date = from_date;
        self
    }

    pub fn from_env() -> Result<time::Date, JsonlError> {
        match std::env::var("CODEX_METER_JSONL_FROM") {
            Ok(value) => parse_date(&value),
            Err(std::env::VarError::NotPresent) => Ok(default_from_date()),
            Err(std::env::VarError::NotUnicode(_)) => Err(JsonlError::Timestamp(
                "CODEX_METER_JSONL_FROM is not valid UTF-8".to_owned(),
            )),
        }
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
        fast::scan_once(self, database).await
    }

}

#[derive(Clone, Debug, Default)]
struct ReplayMetadata {
    session_id: Option<String>,
    parent_session_id: Option<String>,
    forked_at_ms: Option<i64>,
}

fn leading_rewritten_burst_len(events: &[(i64, TokenCounts)]) -> usize {
    if events.len() < 2 {
        return 0;
    }
    let mut length = 1;
    for pair in events.windows(2) {
        if pair[1].0.saturating_sub(pair[0].0) <= 1_000 {
            length += 1;
        } else {
            break;
        }
    }
    (length >= 2).then_some(length).unwrap_or_default()
}

fn record_tokens(record: &SourceJsonlRecord) -> TokenCounts {
    TokenCounts {
        input: record.input_tokens.unwrap_or_default(),
        cached: record.cache_read_tokens.unwrap_or_default(),
        cache_write: record.cache_write_tokens.unwrap_or_default(),
        output: record.output_tokens.unwrap_or_default(),
        reasoning: record.reasoning_tokens.unwrap_or_default(),
        total: record.total_tokens.unwrap_or_default(),
    }
    .normalized()
}

/// Existing databases may have been written by the old collector, which only
/// persisted the immediate parent as `root_session_id`.  Repair that field
/// once per collector lifetime after the complete parent graph is available.
async fn repair_existing_roots(
    database: &Database,
    parent_graph: &BTreeMap<String, String>,
) -> Result<(), JsonlError> {
    for row in database.list_source_jsonl().await? {
        let Some(session_id) = row
            .try_get::<Option<String>, _>("session_id")
            .map_err(|error| JsonlError::Database(DbError::Sqlx(error)))?
        else {
            continue;
        };
        let direct_parent = row
            .try_get::<Option<String>, _>("parent_session_id")
            .map_err(|error| JsonlError::Database(DbError::Sqlx(error)))?;
        let root = resolve_root_session_id(&session_id, direct_parent.as_deref(), parent_graph);
        let previous = row
            .try_get::<Option<String>, _>("root_session_id")
            .map_err(|error| JsonlError::Database(DbError::Sqlx(error)))?;
        if previous != root {
            let source_key = row
                .try_get::<String, _>("source_key")
                .map_err(|error| JsonlError::Database(DbError::Sqlx(error)))?;
            database
                .update_source_jsonl_root(&source_key, root.as_deref())
                .await?;
        }
    }
    Ok(())
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
    let Some(path) = path else { return Ok(()) };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
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
        let path = entry?.path();
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

/// Read only the small, non-sensitive session metadata indexes that Codex
/// already maintains locally.  These files are enrichment inputs, not a
/// fourth usage source: no message text, cwd, preview or authentication data
/// is copied into the source table.
async fn load_local_session_metadata(codex_home: &Path) -> BTreeMap<String, LocalSessionMetadata> {
    let mut metadata: BTreeMap<String, LocalSessionMetadata> = BTreeMap::new();
    let index_path = codex_home.join("session_index.jsonl");
    if let Ok(contents) = fs::read_to_string(index_path) {
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(id) = value.get("id").and_then(Value::as_str) else {
                continue;
            };
            let title = non_empty_string(value.get("thread_name"));
            let entry = metadata.entry(id.to_owned()).or_default();
            if entry.title.is_none() {
                entry.title = title;
            }
        }
    }

    // state_5.sqlite is optional and may be locked while Codex is running.
    // Failure is deliberately non-fatal; JSONL metadata remains authoritative
    // and the report can show a missing title/parent as NULL.
    let state_path = codex_home.join("state_5.sqlite");
    if !state_path.is_file() {
        return metadata;
    }
    let options = SqliteConnectOptions::new()
        .filename(state_path)
        .create_if_missing(false)
        .read_only(true);
    let Ok(pool) = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
    else {
        return metadata;
    };

    if let Ok(rows) = sqlx::query(
        "SELECT id, title, reasoning_effort FROM threads
         WHERE id IS NOT NULL",
    )
    .fetch_all(&pool)
    .await
    {
        for row in rows {
            let Ok(id) = row.try_get::<String, _>("id") else {
                continue;
            };
            let entry = metadata.entry(id).or_default();
            if entry.title.is_none() {
                entry.title = row
                    .try_get::<Option<String>, _>("title")
                    .ok()
                    .flatten()
                    .and_then(|value| non_empty_string(Some(&Value::String(value))));
            }
            if entry.reasoning_effort.is_none() {
                entry.reasoning_effort = row
                    .try_get::<Option<String>, _>("reasoning_effort")
                    .ok()
                    .flatten()
                    .and_then(|value| normalize_effort(&value));
            }
        }
    }

    if let Ok(rows) = sqlx::query(
        "SELECT parent_thread_id, child_thread_id FROM thread_spawn_edges
         WHERE parent_thread_id IS NOT NULL AND child_thread_id IS NOT NULL",
    )
    .fetch_all(&pool)
    .await
    {
        for row in rows {
            let (Ok(parent), Ok(child)) = (
                row.try_get::<String, _>("parent_thread_id"),
                row.try_get::<String, _>("child_thread_id"),
            ) else {
                continue;
            };
            metadata.entry(child).or_default().parent_session_id = Some(parent);
        }
    }
    pool.close().await;
    metadata
}

fn enrich_record_from_metadata(
    record: &mut SourceJsonlRecord,
    metadata: &BTreeMap<String, LocalSessionMetadata>,
    parent_graph: &BTreeMap<String, String>,
) {
    let Some(session_id) = record.session_id.as_deref() else {
        return;
    };
    if let Some(meta) = metadata.get(session_id) {
        if record.kind == "session" && record.title.is_none() {
            record.title = meta.title.clone();
        }
        if record.parent_session_id.is_none() {
            record.parent_session_id = meta.parent_session_id.clone();
        }
        if record.reasoning_effort.is_none() {
            record.reasoning_effort = meta.reasoning_effort.clone();
        }
    }
    if record.parent_session_id.is_none() {
        record.parent_session_id = parent_graph.get(session_id).cloned();
    }
    record.root_session_id = resolve_root_session_id(
        session_id,
        record.parent_session_id.as_deref(),
        parent_graph,
    );
    if record.root_session_id.is_none() {
        append_quality(record, "root_unresolved");
    }
    if record.parent_session_id.is_some() && record.relation.as_deref() == Some("main") {
        record.relation = Some("child".to_owned());
    }
}

fn append_quality(record: &mut SourceJsonlRecord, flag: &str) {
    record.quality = Some(match record.quality.take() {
        Some(current) if !current.is_empty() => format!("{current},{flag}"),
        _ => flag.to_owned(),
    });
}

fn resolve_root_session_id(
    session_id: &str,
    direct_parent: Option<&str>,
    parent_graph: &BTreeMap<String, String>,
) -> Option<String> {
    let mut current = session_id.to_owned();
    let mut seen = BTreeMap::new();
    loop {
        if seen.insert(current.clone(), ()).is_some() {
            return None;
        }
        let parent = if current == session_id {
            direct_parent
                .map(str::to_owned)
                .or_else(|| parent_graph.get(&current).cloned())
        } else {
            parent_graph.get(&current).cloned()
        };
        let Some(parent) = parent.filter(|value| !value.is_empty()) else {
            return Some(current);
        };
        current = parent;
    }
}

fn session_parent(payload: &Value) -> Option<String> {
    payload
        .get("forked_from_id")
        .or_else(|| payload.get("parent_thread_id"))
        .or_else(|| payload.pointer("/source/subagent/thread_spawn/parent_thread_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn non_empty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn normalize_effort(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn nested_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .and_then(normalize_effort)
}

fn reasoning_effort(value: &Value) -> Option<String> {
    nested_string(value, &["reasoning_effort", "reasoningEffort", "effort"]).or_else(|| {
        value
            .get("collaboration_mode")
            .and_then(|mode| mode.get("settings"))
            .and_then(|settings| nested_string(settings, &["reasoning_effort", "effort"]))
    })
}

fn records_for_line(
    _raw_line: &[u8],
    value: &Value,
    context: &mut FileContext,
) -> Result<Vec<SourceJsonlRecord>, JsonlError> {
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
    let mut records = Vec::new();

    if event_type == "session_meta" {
        let session_id = payload
            // `id` is the current log/session.  For subagent logs Codex also
            // emits `session_id`, which points back to the parent thread.
            // Prefer the current id so child/fork facts remain distinct.
            .get("id")
            .or_else(|| payload.get("session_id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(session_id) = session_id {
            if context.session_id.as_deref() != Some(session_id.as_str()) {
                // A JSONL file normally represents one session, but resetting
                // these counters also makes a concatenated/replayed file safe.
                context.previous_cumulative = None;
                context.turn_id = None;
                context.turn_started_at_ms = None;
            }
            context.session_id = Some(session_id.clone());
            context.provider = payload
                .get("model_provider")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| context.provider.clone());
            let parent = session_parent(payload);
            let fork_parent = payload
                .get("forked_from_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let thread_parent = payload
                .get("parent_thread_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    payload
                        .pointer("/source/subagent/thread_spawn/parent_thread_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            context.parent_session_id = parent;
            context.relation = Some(
                if fork_parent.is_some() {
                    "fork"
                } else if thread_parent.is_some() {
                    "child"
                } else {
                    "main"
                }
                .to_owned(),
            );
            // The scanner resolves the final root after it has built the
            // complete parent graph.  This value is only a temporary hint for
            // callers that parse a single line in isolation.
            context.root_session_id = context.parent_session_id.clone();
            records.push(SourceJsonlRecord {
                source_key: format!("session:{session_id}"),
                kind: "session".to_owned(),
                observed_at_ms,
                session_id: Some(session_id),
                parent_session_id: context.parent_session_id.clone(),
                root_session_id: context.root_session_id.clone(),
                relation: context.relation.clone(),
                title: payload
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                provider: context.provider.clone(),
                plan_type: context.plan.clone(),
                reasoning_effort: context.reasoning_effort.clone(),
                quality: Some("session_meta".to_owned()),
                ..Default::default()
            });
        }
    }

    if inner_type == "turn_context" {
        context.turn_id = payload
            .get("turn_id")
            .or_else(|| payload.get("turnId"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        context.model = nested_string(payload, &["model"]).or_else(|| context.model.clone());
        context.reasoning_effort =
            reasoning_effort(payload).or_else(|| context.reasoning_effort.clone());
        // An absent field means "keep the previous tier".  A present but
        // unsupported value means the tier is now unclassified; retaining a
        // stale fast/standard value would price the following usage wrong.
        if let Some(value) = payload
            .get("service_tier")
            .or_else(|| payload.get("serviceTier"))
        {
            context.tier = value.as_str().and_then(normalize_tier).map(str::to_owned);
        }
    } else if matches!(
        inner_type,
        "task_started" | "task_complete" | "task_aborted" | "turn_aborted"
    ) {
        let turn_id = payload
            .get("turn_id")
            .or_else(|| payload.get("turnId"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| context.turn_id.clone());
        if let Some(turn_id) = turn_id {
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
            records.push(SourceJsonlRecord {
                source_key: format!(
                    "turn:{}:{turn_id}",
                    context.session_id.as_deref().unwrap_or("unknown")
                ),
                kind: "turn".to_owned(),
                observed_at_ms,
                session_id: context.session_id.clone(),
                parent_session_id: context.parent_session_id.clone(),
                root_session_id: context.root_session_id.clone(),
                turn_id: Some(turn_id),
                relation: context.relation.clone(),
                started_at_ms: context.turn_started_at_ms.or(Some(observed_at_ms)),
                ended_at_ms: (matches!(
                    inner_type,
                    "task_complete" | "task_aborted" | "turn_aborted"
                ))
                .then(|| {
                    payload
                        .get("completed_at")
                        .or_else(|| payload.get("completedAt"))
                        .or_else(|| payload.get("aborted_at"))
                        .or_else(|| payload.get("abortedAt"))
                        .and_then(as_epoch_ms)
                        .unwrap_or(observed_at_ms)
                }),
                model: context.model.clone(),
                service_tier: context.tier.clone(),
                reasoning_effort: context.reasoning_effort.clone(),
                provider: context.provider.clone(),
                plan_type: context.plan.clone(),
                quality: (matches!(inner_type, "task_aborted" | "turn_aborted"))
                    .then(|| "aborted".to_owned()),
                ..Default::default()
            });
        }
    }

    if inner_type == "thread_settings_applied" {
        let settings = payload.get("thread_settings").unwrap_or(&Value::Null);
        context.model = settings
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| context.model.clone());
        // Match ccusage: no `service_tier` key carries no new information;
        // an explicit unknown value clears the inherited state.
        if let Some(value) = settings.get("service_tier") {
            context.tier = value.as_str().and_then(normalize_tier).map(str::to_owned);
        }
        context.reasoning_effort =
            reasoning_effort(settings).or_else(|| context.reasoning_effort.clone());
        context.provider = settings
            .get("model_provider_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| context.provider.clone());
    }

    if inner_type == "token_count" {
        let info = payload.get("info").unwrap_or(&Value::Null);
        let last = info.get("last_token_usage").and_then(parse_token_counts);
        let cumulative = info.get("total_token_usage").and_then(parse_token_counts);
        // ccusage uses the cumulative counter to decide whether the reported
        // `last_token_usage` is a new delta.  A repeated snapshot can carry a
        // non-zero last usage, but it represents no new spend and must fall
        // back to a zero cumulative diff instead of being counted again.
        let cumulative_advanced = cumulative
            .as_ref()
            .is_none_or(|current| context.previous_cumulative.as_ref() != Some(current));
        let mut tokens = last.filter(|_| cumulative_advanced);
        let reset = cumulative
            .as_ref()
            .zip(context.previous_cumulative.as_ref())
            .is_some_and(|(current, previous)| counter_reset(current, previous));
        let mut quality = reset.then(|| "counter_reset".to_owned());
        if tokens.is_none() {
            if let Some(current) = cumulative.as_ref() {
                tokens = Some(match context.previous_cumulative.as_ref() {
                    None => *current,
                    Some(_) if reset => *current,
                    Some(previous) => current.saturating_sub(previous),
                });
                quality = Some(if reset {
                    "derived_from_cumulative,counter_reset".to_owned()
                } else {
                    "derived_from_cumulative".to_owned()
                });
            }
        }
        if let Some(current) = cumulative {
            context.previous_cumulative = Some(current);
        }
        if let Some(tokens) = tokens
            .map(TokenCounts::normalized)
            .filter(TokenCounts::observed)
        {
            let rate_limits = value
                .get("rate_limits")
                .or_else(|| payload.get("rate_limits"));
            if let Some(rate_limits) = rate_limits {
                context.plan = rate_limits
                    .get("plan_type")
                    .or_else(|| rate_limits.get("plan_type_raw"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| context.plan.clone());
                records.extend(quota_records(rate_limits, context, observed_at_ms));
            }
            // A total-only snapshot cannot be priced or reconciled: ccusage
            // also ignores events with no input/cache/output components. Keep
            // any quota facts above, but do not turn an incomplete token line
            // into a billable usage row.
            if tokens.input == 0
                && tokens.cached == 0
                && tokens.cache_write == 0
                && tokens.output == 0
                && tokens.reasoning == 0
            {
                return Ok(records);
            }
            let digest = usage_digest(context, observed_at_ms, last.as_ref(), cumulative.as_ref());
            records.push(SourceJsonlRecord {
                source_key: format!("usage:{digest}"),
                kind: "usage".to_owned(),
                observed_at_ms,
                session_id: context.session_id.clone(),
                parent_session_id: context.parent_session_id.clone(),
                root_session_id: context.root_session_id.clone(),
                turn_id: context.turn_id.clone(),
                relation: context.relation.clone(),
                model: context.model.clone(),
                service_tier: context.tier.clone(),
                reasoning_effort: context.reasoning_effort.clone(),
                provider: context.provider.clone(),
                plan_type: context.plan.clone(),
                input_tokens: Some(tokens.input),
                cache_read_tokens: Some(tokens.cached),
                cache_write_tokens: Some(tokens.cache_write),
                output_tokens: Some(tokens.output),
                reasoning_tokens: Some(tokens.reasoning),
                total_tokens: Some(tokens.total),
                quality,
                ..Default::default()
            });
        }
    }
    Ok(records)
}

fn quota_records(
    value: &Value,
    context: &FileContext,
    observed_at_ms: i64,
) -> Vec<SourceJsonlRecord> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let limit_id = object
        .get("limit_id")
        .or_else(|| object.get("limitId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let plan = object
        .get("plan_type")
        .or_else(|| object.get("plan_type_raw"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| context.plan.clone());
    ["primary", "secondary"].into_iter().filter_map(|window_kind| {
        let window = object.get(window_kind).filter(|value| !value.is_null())?;
        let used_percent = window.get("used_percent").or_else(|| window.get("usedPercent")).and_then(Value::as_f64);
        let window_minutes = window.get("window_minutes").or_else(|| window.get("windowMinutes")).and_then(as_i64);
        // Codex JSONL currently emits the reset epoch as `resets_at` (seconds),
        // while a few older/protocol-shaped records use `resetsAt` or an
        // already-normalized `resets_at_ms`.  Accept all three spellings and
        // normalize them at the source boundary so the result pipeline can
        // identify real Reset windows instead of collapsing them into an
        // unknown window.
        let resets_at_ms = window
            .get("resets_at_ms")
            .or_else(|| window.get("resetsAt"))
            .or_else(|| window.get("resets_at"))
            .and_then(as_epoch_ms);
        if used_percent.is_none() && resets_at_ms.is_none() { return None }
        let state = json!({"limit_id": limit_id, "window_kind": window_kind, "used_percent": used_percent, "window_minutes": window_minutes, "resets_at_ms": resets_at_ms, "plan": plan});
        Some(SourceJsonlRecord {
            source_key: format!("quota:{}", hex_digest(state.to_string().as_bytes())),
            kind: "quota".to_owned(),
            observed_at_ms,
            last_seen_at_ms: Some(observed_at_ms),
            provider: context.provider.clone(),
            plan_type: plan.clone(),
            limit_id: limit_id.clone(),
            window_kind: Some(window_kind.to_owned()),
            used_percent,
            window_minutes,
            resets_at_ms,
            quality: Some("jsonl".to_owned()),
            ..Default::default()
        })
    }).collect()
}

fn counter_reset(current: &TokenCounts, previous: &TokenCounts) -> bool {
    current.input < previous.input
        || current.cached < previous.cached
        || current.cache_write < previous.cache_write
        || current.output < previous.output
        || current.reasoning < previous.reasoning
        || current.total < previous.total
}

fn usage_digest(
    context: &FileContext,
    observed_at_ms: i64,
    last: Option<&TokenCounts>,
    cumulative: Option<&TokenCounts>,
) -> String {
    // Cumulative counters make replayed/forked copies identifiable even when
    // their JSON timestamps differ.  A last-only event has no stable counter,
    // so retain its observation time to avoid merging distinct requests.
    let value = json!({
        "session_id": context.session_id,
        "turn_id": context.turn_id,
        "model": context.model,
        "tier": context.tier,
        "reasoning_effort": context.reasoning_effort,
        "plan": context.plan,
        "last": last,
        "cumulative": cumulative,
        "observed_at_ms": cumulative.is_none().then_some(observed_at_ms),
    });
    hex_digest(value.to_string().as_bytes())
}

fn parse_token_counts(value: &Value) -> Option<TokenCounts> {
    let token = TokenCounts {
        input: value
            .get("input_tokens")
            .or_else(|| value.get("inputTokens"))
            .and_then(as_i64)
            .unwrap_or_default(),
        cached: value
            .get("cached_input_tokens")
            .or_else(|| value.get("cache_read_input_tokens"))
            .or_else(|| value.get("cacheReadInputTokens"))
            .and_then(as_i64)
            .unwrap_or_default(),
        cache_write: value
            .get("cache_write_input_tokens")
            .or_else(|| value.get("cache_creation_input_tokens"))
            .or_else(|| value.get("cacheCreationInputTokens"))
            .and_then(as_i64)
            .unwrap_or_default(),
        output: value
            .get("output_tokens")
            .or_else(|| value.get("outputTokens"))
            .and_then(as_i64)
            .unwrap_or_default(),
        reasoning: value
            .get("reasoning_output_tokens")
            .or_else(|| value.get("reasoningOutputTokens"))
            .and_then(as_i64)
            .unwrap_or_default(),
        total: value
            .get("total_tokens")
            .or_else(|| value.get("totalTokens"))
            .and_then(as_i64)
            .unwrap_or_default(),
    }
    .normalized();
    token.observed().then_some(token)
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

fn as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|value| value as i64))
}

fn normalize_tier(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "standard" | "default" => Some("standard"),
        "fast" | "priority" => Some("fast"),
        _ => None,
    }
}

fn hex_digest(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    hex::encode(hasher.finalize())
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
        time::{SystemTime, UNIX_EPOCH},
    };

    use sqlx::Row;

    use super::*;

    #[test]
    fn parses_turn_and_usage_without_payload_storage() {
        let mut context = FileContext::default();
        let session = json!({"timestamp":"2026-08-04T06:41:51.881Z","type":"session_meta","payload":{"id":"s1","title":"hello"}});
        let records =
            records_for_line(session.to_string().as_bytes(), &session, &mut context).unwrap();
        assert_eq!(records[0].kind, "session");
        let usage = json!({"timestamp":"2026-08-04T06:42:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"total_tokens":13}}}});
        let records = records_for_line(usage.to_string().as_bytes(), &usage, &mut context).unwrap();
        assert_eq!(
            records
                .iter()
                .find(|record| record.kind == "usage")
                .and_then(|record| record.total_tokens),
            Some(13)
        );
    }

    #[test]
    fn parses_codex_resets_at_seconds_into_milliseconds() {
        let mut context = FileContext::default();
        let line = json!({
            "timestamp":"2026-08-04T06:42:00Z",
            "type":"event_msg",
            "payload":{"type":"token_count","info":{
                "last_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}
            },"rate_limits":{
                "plan_type":"plus",
                "limit_id":"codex",
                "primary":{"used_percent":12,"window_minutes":10080,"resets_at":1786351410}
            }}
        });
        let records = records_for_line(line.to_string().as_bytes(), &line, &mut context).unwrap();
        let quota = records
            .into_iter()
            .find(|record| record.kind == "quota")
            .expect("quota record");
        assert_eq!(quota.resets_at_ms, Some(1_786_351_410_000));
    }

    #[test]
    fn cumulative_usage_is_diffed() {
        let mut context = FileContext::default();
        let line = |total| json!({"timestamp":"2026-08-04T06:42:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":total,"output_tokens":0,"total_tokens":total}}}});
        let first = line(10);
        let second = line(15);
        let a = records_for_line(first.to_string().as_bytes(), &first, &mut context).unwrap();
        let b = records_for_line(second.to_string().as_bytes(), &second, &mut context).unwrap();
        assert_eq!(
            a.iter()
                .find(|record| record.kind == "usage")
                .and_then(|record| record.total_tokens),
            Some(10)
        );
        assert_eq!(
            b.iter()
                .find(|record| record.kind == "usage")
                .and_then(|record| record.total_tokens),
            Some(5)
        );
    }

    #[test]
    fn repeated_cumulative_snapshot_does_not_recount_last_usage() {
        let mut context = FileContext::default();
        let line = |timestamp, total, last| {
            json!({
                "timestamp": timestamp,
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "last_token_usage": {"input_tokens": last, "output_tokens": 0, "total_tokens": last},
                        "total_token_usage": {"input_tokens": total, "output_tokens": 0, "total_tokens": total}
                    }
                }
            })
        };
        let first = line("2026-08-04T06:42:00Z", 100, 100);
        let second = line("2026-08-04T06:43:00Z", 140, 40);
        let repeated = line("2026-08-04T06:44:00Z", 140, 40);
        let a = records_for_line(first.to_string().as_bytes(), &first, &mut context).unwrap();
        let b = records_for_line(second.to_string().as_bytes(), &second, &mut context).unwrap();
        let c = records_for_line(repeated.to_string().as_bytes(), &repeated, &mut context).unwrap();
        assert_eq!(
            a.iter()
                .find(|row| row.kind == "usage")
                .and_then(|row| row.total_tokens),
            Some(100)
        );
        assert_eq!(
            b.iter()
                .find(|row| row.kind == "usage")
                .and_then(|row| row.total_tokens),
            Some(40)
        );
        assert!(c.iter().all(|row| row.kind != "usage"));
    }

    #[test]
    fn unknown_settings_tier_clears_inherited_state() {
        let mut context = FileContext::default();
        let fast = json!({
            "timestamp":"2026-08-04T06:42:00Z",
            "type":"event_msg",
            "payload":{"type":"thread_settings_applied","thread_settings":{"service_tier":"fast"}}
        });
        records_for_line(fast.to_string().as_bytes(), &fast, &mut context).unwrap();
        assert_eq!(context.tier.as_deref(), Some("fast"));
        let unknown = json!({
            "timestamp":"2026-08-04T06:43:00Z",
            "type":"event_msg",
            "payload":{"type":"thread_settings_applied","thread_settings":{"service_tier":"future-tier"}}
        });
        records_for_line(unknown.to_string().as_bytes(), &unknown, &mut context).unwrap();
        assert_eq!(context.tier, None);
    }

    #[test]
    fn ignores_total_only_usage_snapshot() {
        let mut context = FileContext::default();
        let line = json!({
            "timestamp":"2026-08-04T06:42:00Z",
            "type":"event_msg",
            "payload":{"type":"token_count","info":{
                "last_token_usage":{"total_tokens":16096}
            }}
        });
        let records = records_for_line(line.to_string().as_bytes(), &line, &mut context).unwrap();
        assert!(records.iter().all(|record| record.kind != "usage"));
    }

    #[test]
    fn session_meta_prefers_current_id_over_parent_thread_id() {
        let mut context = FileContext::default();
        let line = json!({
            "timestamp":"2026-08-04T06:42:00Z",
            "type":"session_meta",
            "payload":{"id":"child","session_id":"parent","parent_thread_id":"parent"}
        });
        let record = records_for_line(line.to_string().as_bytes(), &line, &mut context)
            .unwrap()
            .into_iter()
            .find(|record| record.kind == "session")
            .unwrap();
        assert_eq!(record.session_id.as_deref(), Some("child"));
        assert_eq!(record.parent_session_id.as_deref(), Some("parent"));
        assert_eq!(record.relation.as_deref(), Some("child"));
    }

    #[test]
    fn resolves_root_session_through_the_complete_parent_chain() {
        let graph = BTreeMap::from([
            ("child".to_owned(), "parent".to_owned()),
            ("parent".to_owned(), "root".to_owned()),
        ]);
        assert_eq!(
            resolve_root_session_id("child", None, &graph).as_deref(),
            Some("root")
        );
        assert_eq!(
            resolve_root_session_id("root", None, &graph).as_deref(),
            Some("root")
        );
    }

    #[test]
    fn rejects_parent_cycles_instead_of_persisting_an_intermediate_root() {
        let graph = BTreeMap::from([
            ("a".to_owned(), "b".to_owned()),
            ("b".to_owned(), "a".to_owned()),
        ]);
        assert!(resolve_root_session_id("a", None, &graph).is_none());
    }

    #[test]
    fn cumulative_counter_reset_keeps_the_new_baseline() {
        let mut context = FileContext::default();
        let line = |timestamp, total| {
            json!({
                "timestamp": timestamp,
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {"total_token_usage": {
                        "input_tokens": total,
                        "output_tokens": 0,
                        "total_tokens": total
                    }}
                }
            })
        };
        let first = line("2026-08-04T06:42:00Z", 100);
        let second = line("2026-08-04T06:43:00Z", 140);
        let reset = line("2026-08-04T06:44:00Z", 10);
        let a = records_for_line(first.to_string().as_bytes(), &first, &mut context).unwrap();
        let b = records_for_line(second.to_string().as_bytes(), &second, &mut context).unwrap();
        let c = records_for_line(reset.to_string().as_bytes(), &reset, &mut context).unwrap();
        assert_eq!(
            a.iter()
                .find(|row| row.kind == "usage")
                .unwrap()
                .total_tokens,
            Some(100)
        );
        assert_eq!(
            b.iter()
                .find(|row| row.kind == "usage")
                .unwrap()
                .total_tokens,
            Some(40)
        );
        let reset_row = c.iter().find(|row| row.kind == "usage").unwrap();
        assert_eq!(reset_row.total_tokens, Some(10));
        assert_eq!(
            reset_row.quality.as_deref(),
            Some("derived_from_cumulative,counter_reset")
        );
    }

    #[test]
    fn turn_records_keep_parent_effort_and_abort_boundary() {
        let mut context = FileContext::default();
        let session = json!({
            "timestamp":"2026-08-04T06:41:51.881Z",
            "type":"session_meta",
            "payload":{"id":"child","parent_thread_id":"root"}
        });
        records_for_line(session.to_string().as_bytes(), &session, &mut context).unwrap();
        let settings = json!({
            "timestamp":"2026-08-04T06:42:00Z",
            "type":"event_msg",
            "payload":{"type":"thread_settings_applied","thread_settings":{
                "model":"gpt-5.6-sol","reasoning_effort":"high","service_tier":"fast"
            }}
        });
        records_for_line(settings.to_string().as_bytes(), &settings, &mut context).unwrap();
        let started = json!({
            "timestamp":"2026-08-04T06:42:01Z",
            "type":"event_msg",
            "payload":{"type":"task_started","turn_id":"turn-1"}
        });
        let row = records_for_line(started.to_string().as_bytes(), &started, &mut context)
            .unwrap()
            .into_iter()
            .find(|row| row.kind == "turn")
            .unwrap();
        assert_eq!(row.parent_session_id.as_deref(), Some("root"));
        assert_eq!(row.relation.as_deref(), Some("child"));
        assert_eq!(row.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(row.service_tier.as_deref(), Some("fast"));

        let aborted = json!({
            "timestamp":"2026-08-04T06:42:02Z",
            "type":"event_msg",
            "payload":{"type":"task_aborted","turn_id":"turn-1"}
        });
        let row = records_for_line(aborted.to_string().as_bytes(), &aborted, &mut context)
            .unwrap()
            .into_iter()
            .find(|row| row.kind == "turn")
            .unwrap();
        assert!(row.ended_at_ms.is_some());
        assert_eq!(row.quality.as_deref(), Some("aborted"));
    }

    #[test]
    fn cumulative_replay_with_a_different_timestamp_has_one_key() {
        let mut context = FileContext::default();
        let line = |timestamp| {
            json!({"timestamp":timestamp,"type":"event_msg","payload":{
                "type":"token_count","info":{
                    "last_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12},
                    "total_token_usage":{"input_tokens":20,"output_tokens":4,"total_tokens":24}
                }
            }})
        };
        let first = line("2026-08-04T06:42:00Z");
        let replay = line("2026-08-04T06:42:03Z");
        let first_key = records_for_line(first.to_string().as_bytes(), &first, &mut context)
            .unwrap()
            .into_iter()
            .find(|row| row.kind == "usage")
            .unwrap()
            .source_key;
        let mut replay_context = FileContext::default();
        let replay_key =
            records_for_line(replay.to_string().as_bytes(), &replay, &mut replay_context)
                .unwrap()
                .into_iter()
                .find(|row| row.kind == "usage")
                .unwrap()
                .source_key;
        assert_eq!(first_key, replay_key);
    }

    #[tokio::test]
    async fn scan_is_incremental_and_enriches_local_title() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codex-meter-jsonl-{nonce}"));
        let session_dir = root.join("sessions/2026/08");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            root.join("session_index.jsonl"),
            r#"{"id":"child","thread_name":"标题补充"}
"#,
        )
        .unwrap();
        let lines = [
            r#"{"timestamp":"2026-08-04T06:41:51Z","type":"session_meta","payload":{"id":"child","parent_thread_id":"root"}}"#,
            r#"{"timestamp":"2026-08-04T06:42:00Z","type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"gpt-5.6-sol","reasoning_effort":"high","service_tier":"fast"}}}"#,
            r#"{"timestamp":"2026-08-04T06:42:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-08-04T06:42:02Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}},"rate_limits":{"plan_type":"plus","limit_id":"weekly","primary":{"used_percent":12,"window_minutes":10080,"resets_at_ms":1786351410000}}}}"#,
            r#"{"timestamp":"2026-08-04T06:42:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}"#,
        ];
        fs::write(
            session_dir.join("rollout-child.jsonl"),
            format!("{}\n", lines.join("\n")),
        )
        .unwrap();

        let database = Database::connect_in_memory().await.unwrap();
        let collector = JsonlCollector::new(&root).with_cursor_path(Some(root.join("cursor.json")));
        let first = collector.scan_once(&database).await.unwrap();
        let second = collector.scan_once(&database).await.unwrap();
        assert!(first.inserted_events >= 4);
        assert_eq!(second.inserted_events, 0);
        assert_eq!(second.duplicate_events, 0);

        let session = database
            .list_source_jsonl()
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.try_get::<String, _>("kind").unwrap() == "session")
            .unwrap();
        assert_eq!(
            session
                .try_get::<Option<String>, _>("title")
                .unwrap()
                .as_deref(),
            Some("标题补充")
        );
        assert_eq!(
            session
                .try_get::<Option<String>, _>("parent_session_id")
                .unwrap()
                .as_deref(),
            Some("root")
        );
        assert_eq!(
            database
                .list_source_jsonl()
                .await
                .unwrap()
                .into_iter()
                .find(|row| row.try_get::<String, _>("kind").unwrap() == "usage")
                .unwrap()
                .try_get::<Option<String>, _>("reasoning_effort")
                .unwrap()
                .as_deref(),
            Some("high")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn scan_filters_a_child_replay_prefix_using_parent_usage() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codex-meter-replay-{nonce}"));
        let session_dir = root.join("sessions/2026/08");
        fs::create_dir_all(&session_dir).unwrap();
        let usage = |timestamp: &str, last: i64, total: i64, used_percent: f64| {
            json!({
                "timestamp": timestamp,
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "last_token_usage": {"input_tokens": last, "output_tokens": 0, "total_tokens": last},
                        "total_token_usage": {"input_tokens": total, "output_tokens": 0, "total_tokens": total},
                        "rate_limits": {
                            "limit_id": "codex",
                            "primary": {"used_percent": used_percent, "window_minutes": 10080, "resets_at": 1786839350},
                            "plan_type": "pro"
                        }
                    },
                    "rate_limits": {
                        "limit_id": "codex",
                        "primary": {"used_percent": used_percent, "window_minutes": 10080, "resets_at": 1786839350},
                        "plan_type": "pro"
                    }
                }
            })
        };
        let parent = [
            json!({"timestamp":"2026-08-04T06:41:00Z","type":"session_meta","payload":{"id":"root"}}),
            usage("2026-08-04T06:42:00Z", 100, 100, 10.0),
            usage("2026-08-04T06:44:00Z", 50, 150, 20.0),
        ];
        let child = [
            json!({"timestamp":"2026-08-04T06:43:00Z","type":"session_meta","payload":{"id":"child","parent_thread_id":"root"}}),
            usage("2026-08-04T06:43:01Z", 100, 100, 10.0),
            usage("2026-08-04T06:45:00Z", 30, 130, 30.0),
        ];
        fs::write(
            session_dir.join("rollout-root.jsonl"),
            parent
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        fs::write(
            session_dir.join("rollout-child.jsonl"),
            child
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();

        let database = Database::connect_in_memory().await.unwrap();
        let collector = JsonlCollector::new(&root).with_cursor_path(Some(root.join("cursor.json")));
        collector.scan_once(&database).await.unwrap();
        let rows = database.list_source_jsonl().await.unwrap();
        let usage_rows = rows
            .iter()
            .filter(|row| row.try_get::<String, _>("kind").unwrap() == "usage")
            .collect::<Vec<_>>();
        let total = usage_rows
            .iter()
            .map(|row| {
                row.try_get::<Option<i64>, _>("total_tokens")
                    .unwrap()
                    .unwrap()
            })
            .sum::<i64>();
        assert_eq!(usage_rows.len(), 3);
        assert_eq!(total, 180);
        assert!(usage_rows.iter().any(|row| {
            row.try_get::<Option<String>, _>("session_id")
                .unwrap()
                .as_deref()
                == Some("child")
                && row.try_get::<Option<i64>, _>("total_tokens").unwrap() == Some(30)
        }));
        let quota_rows = rows
            .iter()
            .filter(|row| row.try_get::<String, _>("kind").unwrap() == "quota")
            .collect::<Vec<_>>();
        assert_eq!(quota_rows.len(), 3);
        let replayed_quota = quota_rows
            .iter()
            .find(|row| row.try_get::<Option<f64>, _>("used_percent").unwrap() == Some(10.0))
            .unwrap();
        let expected = parse_timestamp(Some(&json!("2026-08-04T06:42:00Z"))).unwrap();
        assert_eq!(
            replayed_quota.try_get::<i64, _>("observed_at_ms").unwrap(),
            expected
        );
        assert_eq!(
            replayed_quota
                .try_get::<Option<i64>, _>("last_seen_at_ms")
                .unwrap(),
            Some(expected)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_jsonl_from_date_inclusive() {
        assert_eq!(parse_date("2026-08-01").unwrap().to_string(), "2026-08-01");
        assert!(parse_date("2026-13-01").is_err());
        assert!(parse_date("2026-08-01-extra").is_err());
    }

    #[tokio::test]
    async fn from_date_skips_old_untracked_files() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codex-meter-range-{nonce}"));
        let old_dir = root.join("sessions/2026/01");
        let recent_dir = root.join("sessions/2026/08");
        fs::create_dir_all(&old_dir).unwrap();
        fs::create_dir_all(&recent_dir).unwrap();
        let session = |id: &str, timestamp: &str| {
            json!({
                "timestamp": timestamp,
                "type": "session_meta",
                "payload": {"id": id}
            })
            .to_string()
                + "\n"
        };
        fs::write(
            old_dir.join("rollout-old.jsonl"),
            session("old", "2026-01-02T00:00:00Z"),
        )
        .unwrap();
        std::fs::File::open(old_dir.join("rollout-old.jsonl"))
            .unwrap()
            .set_modified(
                SystemTime::UNIX_EPOCH
                    + std::time::Duration::from_secs(1_704_153_600),
            )
            .unwrap();
        fs::write(
            recent_dir.join("rollout-recent.jsonl"),
            session("recent", "2026-08-02T00:00:00Z"),
        )
        .unwrap();

        let database = Database::connect_in_memory().await.unwrap();
        let collector = JsonlCollector::new(&root)
            .with_cursor_path(Some(root.join("cursor.json")))
            .with_from_date(Some(parse_date("2026-08-01").unwrap()));
        let report = collector.scan_once(&database).await.unwrap();
        assert_eq!(report.files_scanned, 1);
        let rows = database.list_source_jsonl().await.unwrap();
        assert!(rows.iter().all(|row| {
            row.try_get::<Option<String>, _>("session_id").unwrap().as_deref()
                == Some("recent")
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn from_date_drops_old_records_from_recently_modified_file() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codex-meter-record-range-{nonce}"));
        let old_dir = root.join("sessions/2026/01");
        fs::create_dir_all(&old_dir).unwrap();
        let old_path = old_dir.join("rollout-old.jsonl");
        let content = [
            json!({
                "timestamp":"2026-01-02T00:00:00Z",
                "type":"session_meta",
                "payload":{"id":"old"}
            }),
            json!({
                "timestamp":"2026-01-02T00:01:00Z",
                "type":"event_msg",
                "payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}
            }),
        ]
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("\n")
            + "\n";
        fs::write(&old_path, content).unwrap();
        std::fs::File::open(&old_path)
            .unwrap()
            .set_modified(
                SystemTime::UNIX_EPOCH
                    + std::time::Duration::from_secs(1_785_600_000),
            )
            .unwrap();

        let database = Database::connect_in_memory().await.unwrap();
        let collector = JsonlCollector::new(&root)
            .with_cursor_path(Some(root.join("cursor.json")))
            .with_from_date(Some(parse_date("2026-08-01").unwrap()));
        let report = collector.scan_once(&database).await.unwrap();

        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.recognized_events, 0);
        assert_eq!(report.inserted_events, 0);
        assert!(database.list_source_jsonl().await.unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn real_sample_smoke_when_requested() {
        let Some(sample) = std::env::var_os("CODEX_METER_REAL_SAMPLE_JSONL") else {
            return;
        };
        let sample = PathBuf::from(sample);
        assert!(
            sample.is_file(),
            "sample JSONL is missing: {}",
            sample.display()
        );
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codex-meter-real-jsonl-{nonce}"));
        let target = root.join("sessions/2026/08/sample.jsonl");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(&sample, &target).unwrap();
        let database = Database::connect_in_memory().await.unwrap();
        let collector = JsonlCollector::new(&root).with_cursor_path(Some(root.join("cursor.json")));
        let first = collector.scan_once(&database).await.unwrap();
        let second = collector.scan_once(&database).await.unwrap();
        assert!(first.files_scanned == 1);
        assert!(first.recognized_events > 0);
        assert_eq!(second.inserted_events, 0);
        assert_eq!(second.inserted_quota_samples, 0);
        let rows = database.list_source_jsonl().await.unwrap();
        let counts = ["session", "turn", "usage", "quota"]
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    rows.iter()
                        .filter(|row| row.try_get::<String, _>("kind").unwrap() == kind)
                        .count(),
                )
            })
            .collect::<Vec<_>>();
        let effort_count = rows
            .iter()
            .filter(|row| {
                row.try_get::<Option<String>, _>("reasoning_effort")
                    .unwrap()
                    .is_some()
            })
            .count();
        let parent_count = rows
            .iter()
            .filter(|row| {
                row.try_get::<Option<String>, _>("parent_session_id")
                    .unwrap()
                    .is_some()
            })
            .count();
        let title_count = rows
            .iter()
            .filter(|row| row.try_get::<Option<String>, _>("title").unwrap().is_some())
            .count();
        eprintln!(
            "real sample source counts: {counts:?}, effort={effort_count}, parent={parent_count}, titles={title_count}"
        );
        assert!(rows
            .iter()
            .any(|row| row.try_get::<String, _>("kind").unwrap() == "usage"));
        assert!(rows
            .iter()
            .any(|row| row.try_get::<String, _>("kind").unwrap() == "turn"));
        let _ = fs::remove_dir_all(root);
    }

    /// Full-source acceptance run.  This is deliberately opt-in so the normal
    /// test suite never reads a user's real Codex home.  It exercises only the
    /// first source pipeline and can persist a fresh audit database when the
    /// caller supplies `CODEX_METER_FULL_SOURCE_DB`.
    #[tokio::test]
    async fn real_full_source_scan_when_requested() {
        let Some(home) = std::env::var_os("CODEX_METER_FULL_SOURCE_HOME") else {
            return;
        };
        let Some(db_path) = std::env::var_os("CODEX_METER_FULL_SOURCE_DB") else {
            panic!("CODEX_METER_FULL_SOURCE_DB is required for a full source run");
        };
        let home = PathBuf::from(home);
        let db_path = PathBuf::from(db_path);
        assert!(home.join("sessions").is_dir(), "missing sessions directory");
        let fresh_database = !db_path.exists();

        let cursor_path = std::env::var_os("CODEX_METER_FULL_SOURCE_CURSOR")
            .map(PathBuf::from)
            .unwrap_or_else(|| db_path.with_extension("cursors.json"));
        if fresh_database {
            assert!(
                !cursor_path.exists(),
                "refusing to reuse an existing full-run cursor: {}",
                cursor_path.display()
            );
        }

        let database = Database::connect(&db_path).await.unwrap();
        let from_date = std::env::var("CODEX_METER_FULL_SOURCE_FROM")
            .ok()
            .map(|value| parse_date(&value).unwrap());
        let collector = JsonlCollector::new(home)
            .with_cursor_path(Some(cursor_path))
            .with_from_date(from_date);
        let first = collector.scan_once(&database).await.unwrap();
        let second = collector.scan_once(&database).await.unwrap();
        let rows = database.list_source_jsonl().await.unwrap();
        let counts = ["session", "turn", "usage", "quota"]
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    rows.iter()
                        .filter(|row| row.try_get::<String, _>("kind").unwrap() == kind)
                        .count(),
                )
            })
            .collect::<Vec<_>>();
        let count_with = |column: &str| {
            rows.iter()
                .filter(|row| row.try_get::<Option<String>, _>(column).unwrap().is_some())
                .count()
        };
        let turn_count = counts
            .iter()
            .find(|(kind, _)| *kind == "turn")
            .map(|(_, count)| *count)
            .unwrap_or_default();
        let ended_turns = rows
            .iter()
            .filter(|row| {
                row.try_get::<String, _>("kind").unwrap() == "turn"
                    && row
                        .try_get::<Option<i64>, _>("ended_at_ms")
                        .unwrap()
                        .is_some()
            })
            .count();
        let observed_min = rows
            .iter()
            .filter_map(|row| row.try_get::<i64, _>("observed_at_ms").ok())
            .min();
        let observed_max = rows
            .iter()
            .filter_map(|row| row.try_get::<i64, _>("observed_at_ms").ok())
            .max();
        eprintln!(
            "full source scan: db={}, first={first:?}, second={second:?}, rows={}, counts={counts:?}, ended_turns={ended_turns}/{turn_count}, parent={}, effort={}, titles={}, observed_range_ms={observed_min:?}..{observed_max:?}",
            db_path.display(),
            rows.len(),
            count_with("parent_session_id"),
            count_with("reasoning_effort"),
            count_with("title"),
        );
        assert!(first.files_scanned > 0);
        if fresh_database {
            assert!(first.recognized_events > 0);
        }
        assert!(rows.iter().any(|row| {
            row.try_get::<String, _>("kind").unwrap() == "usage"
                && row
                    .try_get::<Option<i64>, _>("total_tokens")
                    .unwrap()
                    .is_some()
        }));
        // A live Codex session may append lines between the two scans.  In
        // that case the second pass is expected to ingest only those new
        // records; it must never claim more inserts than recognized events.
        assert!(second.inserted_events <= second.recognized_events);
    }
}
