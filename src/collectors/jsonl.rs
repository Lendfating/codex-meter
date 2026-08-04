use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use thiserror::Error;
use time::{
    format_description::well_known::Rfc3339, macros::format_description, OffsetDateTime, UtcOffset,
};

use crate::{
    domain::{
        shanghai_date, JsonlCredits, JsonlDailyTokenRollup, JsonlFileState,
        JsonlRateLimitObservation, JsonlRateLimitWindow, JsonlSessionMetadata, JsonlThreadSetting,
        Quality, TokenCounts, TokenObservation,
    },
    storage::{Database, StorageError},
};

pub const JSONL_DEBOUNCE: Duration = Duration::from_secs(2);
pub const JSONL_FULL_SCAN_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const UNKNOWN_SESSION_ID: &str = "unknown-session";
const MAX_CURSOR_VERIFICATION_BYTES: u64 = 1024 * 1024;
const LOCAL_DATE_FORMAT: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]");

#[derive(Debug, Error)]
pub enum JsonlCollectorError {
    #[error("JSONL I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSONL record is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("JSONL timestamp is invalid: {0}")]
    Timestamp(String),
    #[error("JSONL storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("JSONL storage query error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("JSONL watcher error: {0}")]
    Notify(#[from] notify::Error),
    #[error("JSONL watcher channel was closed")]
    WatcherClosed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JsonlSessionMetaEvent {
    pub session_id: String,
    pub observed_at_ms: i64,
    pub cli_version: Option<String>,
    pub model_provider: Option<String>,
    pub thread_source: Option<String>,
    #[serde(skip)]
    pub source_digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JsonlThreadSettingsEvent {
    pub session_id: String,
    pub observed_at_ms: i64,
    pub model: Option<String>,
    pub model_provider_id: Option<String>,
    pub service_tier: String,
    #[serde(skip)]
    pub source_digest: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct JsonlRateLimits {
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub plan_type_raw: Option<String>,
    pub primary: JsonlRateLimitWindow,
    pub secondary: JsonlRateLimitWindow,
    pub credits: JsonlCredits,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct JsonlTokenCountEvent {
    pub session_id: String,
    pub turn_id: Option<String>,
    pub observed_at_ms: i64,
    pub last_token_usage: Option<TokenCounts>,
    pub total_token_usage: Option<TokenCounts>,
    pub model: Option<String>,
    pub model_provider: Option<String>,
    pub service_tier: String,
    pub model_context_window: Option<i64>,
    pub rate_limits: Option<JsonlRateLimits>,
    #[serde(skip)]
    pub source_digest: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JsonlEvent {
    SessionMeta(JsonlSessionMetaEvent),
    ThreadSettings(JsonlThreadSettingsEvent),
    TokenCount(Box<JsonlTokenCountEvent>),
}

impl JsonlEvent {
    fn resolve_unknown_session(&mut self, session_id: &str) -> Result<(), JsonlCollectorError> {
        let event_session_id = match self {
            Self::SessionMeta(event) => &mut event.session_id,
            Self::ThreadSettings(event) => &mut event.session_id,
            Self::TokenCount(event) => &mut event.session_id,
        };
        if event_session_id == UNKNOWN_SESSION_ID {
            *event_session_id = session_id.to_owned();
            self.refresh_digest()?;
        }
        Ok(())
    }

    fn refresh_digest(&mut self) -> Result<(), JsonlCollectorError> {
        match self {
            Self::SessionMeta(event) => {
                event.source_digest = event_digest("session_meta", event)?;
            }
            Self::ThreadSettings(event) => {
                event.source_digest = event_digest("thread_settings_applied", event)?;
            }
            Self::TokenCount(event) => {
                event.source_digest = event_digest("token_count", event)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CursorContext {
    session_id: Option<String>,
    cli_version: Option<String>,
    model_provider: Option<String>,
    thread_source: Option<String>,
    session_started_at_ms: Option<i64>,
    last_model: Option<String>,
    last_model_provider_id: Option<String>,
    last_service_tier: Option<String>,
}

impl CursorContext {
    fn from_state(state: &JsonlFileState) -> Self {
        Self {
            session_id: state.session_id.clone(),
            cli_version: state.cli_version.clone(),
            model_provider: state.model_provider.clone(),
            thread_source: state.thread_source.clone(),
            session_started_at_ms: state.session_started_at_ms,
            last_model: state.last_model.clone(),
            last_model_provider_id: state.last_model_provider_id.clone(),
            last_service_tier: state.last_service_tier.clone(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ParsedFile {
    events: Vec<JsonlEvent>,
    complete_line_count: usize,
    pending_bytes: usize,
    next_offset: i64,
    last_line_digest: Option<String>,
    context: CursorContext,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JsonlScanReport {
    pub files_scanned: usize,
    pub files_changed: usize,
    pub complete_lines: usize,
    pub pending_bytes: usize,
    pub recognized_events: usize,
    pub inserted_session_metadata: usize,
    pub inserted_thread_settings: usize,
    pub inserted_token_observations: usize,
    pub inserted_rate_limits: usize,
    pub daily_rollups_rebuilt: usize,
}

#[derive(Clone, Debug)]
pub struct JsonlCollector {
    codex_home: PathBuf,
    machine_id: i64,
    collector_version: String,
    timezone: String,
}

impl JsonlCollector {
    pub fn new(
        codex_home: impl Into<PathBuf>,
        machine_id: i64,
        collector_version: impl Into<String>,
    ) -> Self {
        Self {
            codex_home: codex_home.into(),
            machine_id,
            collector_version: collector_version.into(),
            timezone: "Asia/Shanghai".to_owned(),
        }
    }

    pub fn with_timezone(mut self, timezone: impl Into<String>) -> Self {
        self.timezone = timezone.into();
        self
    }

    pub fn sessions_root(&self) -> PathBuf {
        self.codex_home.join("sessions")
    }

    pub fn archived_sessions_root(&self) -> PathBuf {
        self.codex_home.join("archived_sessions")
    }

    pub fn watch(&self) -> Result<JsonlEventWatcher, JsonlCollectorError> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(
            move |event| {
                let _ = sender.send(event);
            },
            Config::default(),
        )?;
        if self.codex_home.is_dir() {
            watcher.watch(&self.codex_home, RecursiveMode::Recursive)?;
        } else {
            for root in [self.sessions_root(), self.archived_sessions_root()] {
                if root.is_dir() {
                    watcher.watch(&root, RecursiveMode::Recursive)?;
                }
            }
        }
        Ok(JsonlEventWatcher {
            _watcher: watcher,
            receiver,
            queue: DebouncedPathQueue::new(JSONL_DEBOUNCE),
        })
    }

    pub async fn scan_once(
        &self,
        database: &Database,
    ) -> Result<JsonlScanReport, JsonlCollectorError> {
        let paths = self.discover_jsonl_paths()?;
        let mut report = JsonlScanReport {
            files_scanned: paths.len(),
            ..JsonlScanReport::default()
        };

        for (path, active_state) in paths {
            let path_key = path.to_string_lossy().into_owned();
            let metadata = fs::metadata(&path)?;
            let inode = file_inode(&metadata);
            let mut previous = database
                .jsonl_file_by_path(self.machine_id, &path_key)
                .await?;
            if previous.is_none() {
                if let Some(inode) = inode {
                    previous = database.jsonl_file_by_inode(self.machine_id, inode).await?;
                }
            }

            let scanned = scan_file(&path, inode, previous.as_ref())?;
            report.complete_lines += scanned.complete_line_count;
            report.pending_bytes += scanned.pending_bytes;
            report.recognized_events += scanned.events.len();
            let changed = previous.as_ref().is_none_or(|state| {
                state.path_key != path_key
                    || state.offset_bytes != scanned.next_offset
                    || state.inode != inode
            });
            if changed || !scanned.events.is_empty() {
                report.files_changed += 1;
            }

            for event in scanned.events {
                match event {
                    JsonlEvent::SessionMeta(event) => {
                        let inserted = database
                            .insert_jsonl_session_metadata(&JsonlSessionMetadata {
                                machine_id: self.machine_id,
                                session_id: event.session_id,
                                observed_at_ms: event.observed_at_ms,
                                cli_version: event.cli_version,
                                model_provider: event.model_provider,
                                thread_source: event.thread_source,
                                source_digest: event.source_digest,
                                collector_version: self.collector_version.clone(),
                            })
                            .await?;
                        report.inserted_session_metadata += usize::from(inserted);
                    }
                    JsonlEvent::ThreadSettings(event) => {
                        let inserted = database
                            .insert_jsonl_thread_setting(&JsonlThreadSetting {
                                machine_id: self.machine_id,
                                session_id: event.session_id,
                                observed_at_ms: event.observed_at_ms,
                                model: event.model,
                                model_provider_id: event.model_provider_id,
                                service_tier: event.service_tier,
                                source_digest: event.source_digest,
                                collector_version: self.collector_version.clone(),
                            })
                            .await?;
                        report.inserted_thread_settings += usize::from(inserted);
                    }
                    JsonlEvent::TokenCount(event) => {
                        let rate_limits_json = event
                            .rate_limits
                            .as_ref()
                            .map(serde_json::to_value)
                            .transpose()?;
                        let tokens = event
                            .last_token_usage
                            .clone()
                            .or_else(|| event.total_token_usage.clone())
                            .unwrap_or_default();
                        let inserted = database
                            .insert_token_observation(&TokenObservation {
                                machine_id: self.machine_id,
                                context_interval_id: None,
                                session_id: event.session_id.clone(),
                                turn_id: event.turn_id,
                                observed_at_ms: event.observed_at_ms,
                                tokens,
                                model: event.model,
                                model_provider: event.model_provider,
                                service_tier: Some(event.service_tier),
                                model_context_window: event.model_context_window,
                                rate_limits: rate_limits_json,
                                source_digest: event.source_digest.clone(),
                                collector_version: self.collector_version.clone(),
                            })
                            .await?;
                        report.inserted_token_observations += usize::from(inserted);

                        if let Some(rate_limits) = event.rate_limits {
                            let inserted = database
                                .insert_jsonl_rate_limit(&JsonlRateLimitObservation {
                                    machine_id: self.machine_id,
                                    session_id: event.session_id,
                                    observed_at_ms: event.observed_at_ms,
                                    limit_id: rate_limits.limit_id,
                                    limit_name: rate_limits.limit_name,
                                    plan_type_raw: rate_limits.plan_type_raw,
                                    primary: rate_limits.primary,
                                    secondary: rate_limits.secondary,
                                    credits: rate_limits.credits,
                                    source_digest: format!("{}:rate_limits", event.source_digest),
                                    collector_version: self.collector_version.clone(),
                                })
                                .await?;
                            report.inserted_rate_limits += usize::from(inserted);
                        }
                    }
                }
            }

            let context = scanned.context;
            let state = JsonlFileState {
                machine_id: self.machine_id,
                path_key,
                session_id: context.session_id,
                inode,
                offset_bytes: scanned.next_offset,
                mtime_ms: metadata_modified_ms(&metadata),
                digest: scanned.last_line_digest,
                active_state: active_state.to_owned(),
                cli_version: context.cli_version,
                model_provider: context.model_provider,
                thread_source: context.thread_source,
                session_started_at_ms: context.session_started_at_ms,
                last_model: context.last_model,
                last_model_provider_id: context.last_model_provider_id,
                last_service_tier: context.last_service_tier,
            };
            database.upsert_jsonl_file(&state).await?;
        }

        report.daily_rollups_rebuilt = self.rebuild_daily_token_rollups(database).await?;

        Ok(report)
    }

    async fn rebuild_daily_token_rollups(
        &self,
        database: &Database,
    ) -> Result<usize, JsonlCollectorError> {
        let rows = sqlx::query(
            "SELECT observed_at_ms, input_tokens, cached_input_tokens,
                    cache_write_input_tokens, output_tokens, reasoning_output_tokens,
                    total_tokens, source_digest
             FROM token_observations
             WHERE machine_id = ?
             ORDER BY observed_at_ms, id",
        )
        .bind(self.machine_id)
        .fetch_all(database.pool())
        .await?;
        let mut aggregates: BTreeMap<String, (TokenCounts, Vec<String>)> = BTreeMap::new();
        for row in rows {
            let observed_at_ms: i64 = row.try_get("observed_at_ms")?;
            let local_date = local_date_for_timezone(observed_at_ms, &self.timezone)?;
            let entry = aggregates
                .entry(local_date)
                .or_insert_with(|| (TokenCounts::default(), Vec::new()));
            add_token_counts(
                &mut entry.0,
                TokenCounts {
                    input_tokens: row.try_get("input_tokens")?,
                    cached_input_tokens: row.try_get("cached_input_tokens")?,
                    cache_write_input_tokens: row.try_get("cache_write_input_tokens")?,
                    output_tokens: row.try_get("output_tokens")?,
                    reasoning_output_tokens: row.try_get("reasoning_output_tokens")?,
                    total_tokens: row.try_get("total_tokens")?,
                },
            );
            entry.1.push(row.try_get("source_digest")?);
        }

        for (local_date, (tokens, source_digests)) in &aggregates {
            let mut digest_input = format!("jsonl-daily\0{local_date}\0").into_bytes();
            for source_digest in source_digests {
                digest_input.extend(source_digest.as_bytes());
                digest_input.push(0);
            }
            database
                .upsert_jsonl_daily_token_rollup(&JsonlDailyTokenRollup {
                    machine_id: self.machine_id,
                    local_date: local_date.clone(),
                    timezone: self.timezone.clone(),
                    tokens: tokens.clone(),
                    source: "jsonl".to_owned(),
                    quality: Quality::Exact,
                    collector_version: self.collector_version.clone(),
                    source_digest: hash_bytes(&digest_input),
                })
                .await?;
        }
        Ok(aggregates.len())
    }

    fn discover_jsonl_paths(&self) -> Result<Vec<(PathBuf, &'static str)>, JsonlCollectorError> {
        let mut paths = Vec::new();
        collect_jsonl_paths(&self.sessions_root(), "active", &mut paths)?;
        collect_jsonl_paths(&self.archived_sessions_root(), "archived", &mut paths)?;
        paths.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(paths)
    }
}

pub struct JsonlEventWatcher {
    _watcher: RecommendedWatcher,
    receiver: Receiver<Result<Event, notify::Error>>,
    queue: DebouncedPathQueue,
}

impl JsonlEventWatcher {
    pub fn next_batch(&mut self) -> Result<Vec<PathBuf>, JsonlCollectorError> {
        loop {
            if let Some(deadline) = self.queue.next_deadline() {
                let now = Instant::now();
                if deadline <= now {
                    return Ok(self.queue.take_ready(now));
                }
                match self
                    .receiver
                    .recv_timeout(deadline.saturating_duration_since(now))
                {
                    Ok(event) => self.add_event(event?)?,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        return Ok(self.queue.take_ready(Instant::now()));
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(JsonlCollectorError::WatcherClosed);
                    }
                }
            } else {
                match self.receiver.recv() {
                    Ok(event) => self.add_event(event?)?,
                    Err(mpsc::RecvError) => return Err(JsonlCollectorError::WatcherClosed),
                }
            }
        }
    }

    fn add_event(&mut self, event: Event) -> Result<(), JsonlCollectorError> {
        for path in event.paths {
            self.queue.push(path);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct DebouncedPathQueue {
    delay: Duration,
    pending: HashMap<PathBuf, Instant>,
}

impl DebouncedPathQueue {
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            pending: HashMap::new(),
        }
    }

    pub fn push(&mut self, path: PathBuf) {
        self.pending.insert(path, Instant::now() + self.delay);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().copied().min()
    }

    pub fn take_ready(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready = self
            .pending
            .iter()
            .filter_map(|(path, deadline)| (*deadline <= now).then_some(path.clone()))
            .collect::<Vec<_>>();
        for path in &ready {
            self.pending.remove(path);
        }
        ready.sort();
        ready
    }

    #[cfg(test)]
    fn push_at(&mut self, path: PathBuf, now: Instant) {
        self.pending.insert(path, now + self.delay);
    }
}

fn scan_file(
    path: &Path,
    inode: Option<i64>,
    previous: Option<&JsonlFileState>,
) -> Result<ParsedFile, JsonlCollectorError> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let file_len = metadata.len();
    let mut reset_cursor = previous
        .is_none_or(|state| state.inode != inode && (state.inode.is_some() || inode.is_some()));
    let mut start_offset = previous.map_or(0_u64, |state| state.offset_bytes.max(0) as u64);
    if start_offset > file_len {
        reset_cursor = true;
    }
    if !reset_cursor {
        if let Some(previous_digest) = previous.and_then(|state| state.digest.as_deref()) {
            let actual_digest = line_digest_at_offset(&mut file, start_offset)?;
            if actual_digest.as_deref() != Some(previous_digest) {
                reset_cursor = true;
            }
        }
    }
    if reset_cursor {
        start_offset = 0;
    }

    let context = if reset_cursor {
        CursorContext::default()
    } else {
        previous.map_or_else(CursorContext::default, CursorContext::from_state)
    };
    file.seek(SeekFrom::Start(start_offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let (complete_bytes, pending_bytes) = complete_prefix_len(&bytes);
    let mut parsed = parse_complete_lines(&bytes[..complete_bytes], context)?;
    parsed.complete_line_count = bytes[..complete_bytes]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count();
    parsed.pending_bytes = pending_bytes;
    parsed.next_offset = i64::try_from(start_offset + complete_bytes as u64).unwrap_or(i64::MAX);
    parsed.last_line_digest = if complete_bytes > 0 {
        last_complete_line(&bytes[..complete_bytes]).map(hash_bytes)
    } else if reset_cursor {
        None
    } else {
        previous.and_then(|state| state.digest.clone())
    };
    if let Some(session_id) = parsed.context.session_id.clone() {
        for event in &mut parsed.events {
            event.resolve_unknown_session(&session_id)?;
        }
    }
    Ok(parsed)
}

fn parse_complete_lines(
    bytes: &[u8],
    mut context: CursorContext,
) -> Result<ParsedFile, JsonlCollectorError> {
    let mut parsed = ParsedFile {
        context: context.clone(),
        ..ParsedFile::default()
    };
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let record: Value = serde_json::from_slice(line)?;
        let record_type = record.get("type").and_then(Value::as_str);
        match record_type {
            Some("session_meta") => {
                let payload = record.get("payload").and_then(Value::as_object);
                let Some(payload) = payload else {
                    continue;
                };
                let session_id = first_string(payload, &["session_id", "id"])
                    .unwrap_or_else(|| UNKNOWN_SESSION_ID.to_owned());
                let observed_at_ms = record_timestamp(&record)
                    .or_else(|| payload.get("timestamp").and_then(parse_timestamp))
                    .ok_or_else(|| {
                        JsonlCollectorError::Timestamp("session_meta has no timestamp".into())
                    })?;
                let event = JsonlSessionMetaEvent {
                    session_id: session_id.clone(),
                    observed_at_ms,
                    cli_version: first_string(payload, &["cli_version"]),
                    model_provider: first_string(payload, &["model_provider"]),
                    thread_source: first_string(payload, &["thread_source"]),
                    source_digest: String::new(),
                };
                context.session_id = Some(session_id);
                context.cli_version = event.cli_version.clone();
                context.model_provider = event.model_provider.clone();
                context.thread_source = event.thread_source.clone();
                context.session_started_at_ms = Some(observed_at_ms);
                let mut event = event;
                event.source_digest = event_digest("session_meta", &event)?;
                parsed.events.push(JsonlEvent::SessionMeta(event));
            }
            Some("event_msg") => {
                let Some(payload) = record.get("payload").and_then(Value::as_object) else {
                    continue;
                };
                let observed_at_ms = record_timestamp(&record).ok_or_else(|| {
                    JsonlCollectorError::Timestamp("event_msg has no timestamp".into())
                })?;
                match payload.get("type").and_then(Value::as_str) {
                    Some("thread_settings_applied") => {
                        let settings = payload
                            .get("thread_settings")
                            .and_then(Value::as_object)
                            .unwrap_or(payload);
                        let model = first_string(settings, &["model"]);
                        let model_provider_id =
                            first_string(settings, &["model_provider_id", "provider"]);
                        let service_tier = normalize_service_tier(
                            first_string(settings, &["service_tier"]).as_deref(),
                        );
                        context.last_model = model.clone();
                        context.last_model_provider_id = model_provider_id.clone();
                        context.last_service_tier = Some(service_tier.clone());
                        if context.model_provider.is_none() {
                            context.model_provider = model_provider_id.clone();
                        }
                        let mut event = JsonlThreadSettingsEvent {
                            session_id: event_session_id(payload, &context),
                            observed_at_ms,
                            model,
                            model_provider_id,
                            service_tier,
                            source_digest: String::new(),
                        };
                        event.source_digest = event_digest("thread_settings_applied", &event)?;
                        parsed.events.push(JsonlEvent::ThreadSettings(event));
                    }
                    Some("token_count") => {
                        let info = payload.get("info").and_then(Value::as_object);
                        let last_token_usage = info
                            .and_then(|info| info.get("last_token_usage"))
                            .and_then(parse_token_counts);
                        let total_token_usage = info
                            .and_then(|info| info.get("total_token_usage"))
                            .and_then(parse_token_counts);
                        if last_token_usage.is_none() && total_token_usage.is_none() {
                            continue;
                        }
                        let model = first_string(payload, &["model"]).or_else(|| {
                            info.and_then(|info| first_string_map(info, &["model"]))
                                .or_else(|| context.last_model.clone())
                        });
                        let model_provider = first_string(payload, &["model_provider"])
                            .or_else(|| context.model_provider.clone());
                        let service_tier = first_string(payload, &["service_tier"])
                            .map(|value| normalize_service_tier(Some(&value)))
                            .or_else(|| context.last_service_tier.clone())
                            .unwrap_or_else(|| "unknown".to_owned());
                        let model_context_window = info
                            .and_then(|info| info.get("model_context_window"))
                            .and_then(parse_integer);
                        let rate_limits = payload.get("rate_limits").and_then(parse_rate_limits);
                        let mut event = JsonlTokenCountEvent {
                            session_id: event_session_id(payload, &context),
                            turn_id: first_string(payload, &["turn_id", "turnId"]),
                            observed_at_ms,
                            last_token_usage,
                            total_token_usage,
                            model,
                            model_provider,
                            service_tier,
                            model_context_window,
                            rate_limits,
                            source_digest: String::new(),
                        };
                        event.source_digest = event_digest("token_count", &event)?;
                        parsed.events.push(JsonlEvent::TokenCount(Box::new(event)));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    parsed.context = context;
    Ok(parsed)
}

fn event_session_id(payload: &serde_json::Map<String, Value>, context: &CursorContext) -> String {
    first_string(payload, &["session_id", "thread_id", "threadId"])
        .or_else(|| context.session_id.clone())
        .unwrap_or_else(|| UNKNOWN_SESSION_ID.to_owned())
}

fn parse_rate_limits(value: &Value) -> Option<JsonlRateLimits> {
    let object = value.as_object()?;
    let limit_id =
        first_string(object, &["limit_id", "id"]).unwrap_or_else(|| "unknown-limit".to_owned());
    Some(JsonlRateLimits {
        limit_id,
        limit_name: first_string(object, &["limit_name", "name"]),
        plan_type_raw: first_string(object, &["plan_type"]),
        primary: parse_rate_limit_window(object.get("primary")),
        secondary: parse_rate_limit_window(object.get("secondary")),
        credits: parse_credits(object.get("credits")),
    })
}

fn parse_rate_limit_window(value: Option<&Value>) -> JsonlRateLimitWindow {
    let Some(object) = value.and_then(Value::as_object) else {
        return JsonlRateLimitWindow::default();
    };
    JsonlRateLimitWindow {
        used_percent: object.get("used_percent").and_then(parse_float),
        window_minutes: object.get("window_minutes").and_then(parse_integer),
        resets_at_ms: object
            .get("resets_at")
            .and_then(parse_integer)
            .map(normalize_epoch_seconds_or_millis),
    }
}

fn parse_credits(value: Option<&Value>) -> JsonlCredits {
    let Some(object) = value.and_then(Value::as_object) else {
        return JsonlCredits::default();
    };
    JsonlCredits {
        has_credits: object.get("has_credits").and_then(Value::as_bool),
        unlimited: object.get("unlimited").and_then(Value::as_bool),
        balance: object.get("balance").and_then(value_as_string),
    }
}

fn parse_token_counts(value: &Value) -> Option<TokenCounts> {
    let object = value.as_object()?;
    let mut counts = TokenCounts::default();
    let mut found = false;
    for (name, target) in [
        ("input_tokens", &mut counts.input_tokens),
        ("cached_input_tokens", &mut counts.cached_input_tokens),
        (
            "cache_write_input_tokens",
            &mut counts.cache_write_input_tokens,
        ),
        ("output_tokens", &mut counts.output_tokens),
        (
            "reasoning_output_tokens",
            &mut counts.reasoning_output_tokens,
        ),
        ("total_tokens", &mut counts.total_tokens),
    ] {
        if let Some(value) = object.get(name).and_then(parse_integer) {
            if value >= 0 {
                *target = value;
                found = true;
            }
        }
    }
    found.then_some(counts)
}

fn first_string(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(value_as_string))
}

fn first_string_map(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    first_string(object, names)
}

fn value_as_string(value: &Value) -> Option<String> {
    value.as_str().map(ToOwned::to_owned)
}

fn parse_integer(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    value.as_str()?.parse().ok()
}

fn parse_float(value: &Value) -> Option<f64> {
    if let Some(number) = value.as_f64() {
        return Some(number);
    }
    value.as_str()?.parse().ok()
}

fn parse_timestamp(value: &Value) -> Option<i64> {
    if let Some(value) = value.as_str() {
        return OffsetDateTime::parse(value, &Rfc3339)
            .ok()
            .and_then(|date_time| {
                i64::try_from(date_time.unix_timestamp_nanos() / 1_000_000).ok()
            });
    }
    parse_integer(value).map(normalize_epoch_seconds_or_millis)
}

fn record_timestamp(record: &Value) -> Option<i64> {
    record.get("timestamp").and_then(parse_timestamp)
}

fn normalize_epoch_seconds_or_millis(value: i64) -> i64 {
    if value.unsigned_abs() < 1_000_000_000_000 {
        value.saturating_mul(1_000)
    } else {
        value
    }
}

fn normalize_service_tier(value: Option<&str>) -> String {
    match value.map(str::to_ascii_lowercase).as_deref() {
        Some("fast") | Some("priority") => "fast".to_owned(),
        Some("standard") | Some("default") => "standard".to_owned(),
        _ => "unknown".to_owned(),
    }
}

fn add_token_counts(total: &mut TokenCounts, value: TokenCounts) {
    total.input_tokens += value.input_tokens;
    total.cached_input_tokens += value.cached_input_tokens;
    total.cache_write_input_tokens += value.cache_write_input_tokens;
    total.output_tokens += value.output_tokens;
    total.reasoning_output_tokens += value.reasoning_output_tokens;
    total.total_tokens += value.total_tokens;
}

fn local_date_for_timezone(epoch_ms: i64, timezone: &str) -> Result<String, JsonlCollectorError> {
    match timezone {
        "Asia/Shanghai" | "Asia/Chongqing" | "Asia/Harbin" | "Asia/Urumqi" | "PRC" => {
            shanghai_date(epoch_ms)
                .map_err(|error| JsonlCollectorError::Timestamp(error.to_string()))
        }
        "UTC" | "Etc/UTC" => {
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(epoch_ms) * 1_000_000)
                .map_err(|error| JsonlCollectorError::Timestamp(error.to_string()))?
                .to_offset(UtcOffset::UTC)
                .format(LOCAL_DATE_FORMAT)
                .map_err(|error| JsonlCollectorError::Timestamp(error.to_string()))
        }
        _ => Err(JsonlCollectorError::Timestamp(format!(
            "unsupported JSONL rollup timezone: {timezone}"
        ))),
    }
}

fn complete_prefix_len(bytes: &[u8]) -> (usize, usize) {
    match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => (index + 1, bytes.len() - index - 1),
        None => (0, bytes.len()),
    }
}

fn last_complete_line(bytes: &[u8]) -> Option<&[u8]> {
    let bytes = bytes.strip_suffix(b"\n")?;
    bytes.rsplit(|byte| *byte == b'\n').next()
}

fn line_digest_at_offset(
    file: &mut File,
    offset: u64,
) -> Result<Option<String>, JsonlCollectorError> {
    if offset == 0 {
        return Ok(None);
    }
    let start = offset.saturating_sub(MAX_CURSOR_VERIFICATION_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let length = usize::try_from(offset - start).unwrap_or(usize::MAX);
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    let Some(line) = last_complete_line(&bytes) else {
        return Ok(None);
    };
    if start > 0 && !bytes[..bytes.len().saturating_sub(line.len())].contains(&b'\n') {
        return Ok(None);
    }
    Ok(Some(hash_bytes(line)))
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn event_digest<T: Serialize>(kind: &str, event: &T) -> Result<String, JsonlCollectorError> {
    let mut bytes = kind.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend(serde_json::to_vec(event)?);
    Ok(hash_bytes(&bytes))
}

fn collect_jsonl_paths(
    root: &Path,
    active_state: &'static str,
    output: &mut Vec<(PathBuf, &'static str)>,
) -> Result<(), JsonlCollectorError> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_jsonl_paths(&path, active_state, output)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            output.push((path, active_state));
        }
    }
    Ok(())
}

fn metadata_modified_ms(metadata: &Metadata) -> Option<i64> {
    metadata.modified().ok().and_then(system_time_ms)
}

fn system_time_ms(value: SystemTime) -> Option<i64> {
    let duration = value.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

#[cfg(unix)]
fn file_inode(metadata: &Metadata) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;

    i64::try_from(metadata.ino()).ok()
}

#[cfg(not(unix))]
fn file_inode(_metadata: &Metadata) -> Option<i64> {
    None
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
        time::Duration,
    };

    use serde_json::json;

    use crate::{domain::Machine, storage::Database};

    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("codex-meter-jsonl-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    async fn setup(label: &str) -> (Database, PathBuf, JsonlCollector) {
        let database = Database::connect_in_memory().await.unwrap();
        let machine_id = database
            .insert_machine(&Machine {
                name: "jsonl-test".into(),
                install_id: format!("jsonl-{label}"),
                timezone: "Asia/Shanghai".into(),
                created_at_ms: 1,
            })
            .await
            .unwrap();
        let root = temp_root(label);
        fs::create_dir_all(root.join("sessions/2026/08")).unwrap();
        fs::create_dir_all(root.join("archived_sessions/2026/08")).unwrap();
        let collector = JsonlCollector::new(&root, machine_id, "phase2-test");
        (database, root, collector)
    }

    fn write_fixture(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
    }

    fn meta_line(session_id: &str) -> String {
        serde_json::to_string(&json!({
            "timestamp": "2026-08-04T06:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "cli_version": "0.146.0-alpha.3.1",
                "session_id": session_id,
                "model_provider": "openai",
                "thread_source": "cli",
                "timestamp": "2026-08-04T06:00:00.000Z"
            }
        }))
        .unwrap()
    }

    fn settings_line(timestamp: &str, tier: &str) -> String {
        serde_json::to_string(&json!({
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {
                "type": "thread_settings_applied",
                "thread_settings": {
                    "model": "fixture-model",
                    "model_provider_id": "openai",
                    "service_tier": tier
                }
            }
        }))
        .unwrap()
    }

    fn token_line(timestamp: &str, total: i64) -> String {
        serde_json::to_string(&json!({
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": total,
                        "total_tokens": total
                    },
                    "total_token_usage": {
                        "input_tokens": total,
                        "total_tokens": total
                    },
                    "model_context_window": 100000
                },
                "rate_limits": {
                    "limit_id": "weekly",
                    "limit_name": "fixture",
                    "plan_type": "plus",
                    "primary": {
                        "used_percent": 12,
                        "window_minutes": 10080,
                        "resets_at": 1786351410
                    },
                    "secondary": null,
                    "credits": null
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn parser_inherits_standard_fast_standard_and_keeps_whitelist() {
        let content = [
            meta_line("session-tier"),
            settings_line("2026-08-04T06:00:01.000Z", "default"),
            token_line("2026-08-04T06:00:02.000Z", 10),
            settings_line("2026-08-04T06:00:03.000Z", "priority"),
            token_line("2026-08-04T06:00:04.000Z", 20),
            settings_line("2026-08-04T06:00:05.000Z", "standard"),
            token_line("2026-08-04T06:00:06.000Z", 30),
        ]
        .join("\n")
            + "\n";
        let root = temp_root("parser");
        let path = root.join("session.jsonl");
        write_fixture(&path, &content);
        let state = JsonlFileState {
            machine_id: 1,
            path_key: path.to_string_lossy().into_owned(),
            session_id: None,
            inode: None,
            offset_bytes: 0,
            mtime_ms: None,
            digest: None,
            active_state: "active".into(),
            cli_version: None,
            model_provider: None,
            thread_source: None,
            session_started_at_ms: None,
            last_model: None,
            last_model_provider_id: None,
            last_service_tier: None,
        };
        let parsed = scan_file(
            &path,
            file_inode(&fs::metadata(&path).unwrap()),
            Some(&state),
        )
        .unwrap();
        let tiers = parsed
            .events
            .iter()
            .filter_map(|event| match event {
                JsonlEvent::TokenCount(event) => Some(event.service_tier.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tiers, ["standard", "fast", "standard"]);
        assert!(parsed.events.iter().all(|event| match event {
            JsonlEvent::SessionMeta(_)
            | JsonlEvent::ThreadSettings(_)
            | JsonlEvent::TokenCount(_) => {
                true
            }
        }));
        cleanup(&root);
    }

    #[tokio::test]
    async fn fixture_replay_is_idempotent() {
        let (database, root, collector) = setup("replay").await;
        let path = root.join("sessions/2026/08/replay.jsonl");
        write_fixture(
            &path,
            include_str!("../../fixtures/jsonl/codex-session-pro-sanitized.jsonl"),
        );
        let first = collector.scan_once(&database).await.unwrap();
        let second = collector.scan_once(&database).await.unwrap();
        assert!(first.inserted_token_observations > 0);
        assert_eq!(first.daily_rollups_rebuilt, 1);
        assert_eq!(second.inserted_token_observations, 0);
        assert_eq!(second.daily_rollups_rebuilt, 1);
        assert_eq!(second.inserted_session_metadata, 0);
        assert_eq!(second.inserted_thread_settings, 0);
        let token_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM token_observations")
            .fetch_one(database.pool())
            .await
            .unwrap();
        let expected_tokens = include_str!("../../fixtures/jsonl/codex-session-pro-sanitized.jsonl")
            .lines()
            .filter(|line| line.contains("\"type\":\"token_count\""))
            .count() as i64;
        assert_eq!(token_count, expected_tokens);
        let daily_rollup_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM jsonl_daily_token_rollups")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(daily_rollup_count, 1);
        cleanup(&root);
    }

    #[tokio::test]
    async fn active_and_archived_copies_are_counted_once() {
        let (database, root, collector) = setup("archive").await;
        let content = format!(
            "{}\n{}\n",
            meta_line("session-archive"),
            token_line("2026-08-04T06:00:02.000Z", 10)
        );
        let active = root.join("sessions/2026/08/archive.jsonl");
        let archived = root.join("archived_sessions/2026/08/archive.jsonl");
        write_fixture(&active, &content);
        write_fixture(&archived, &content);
        let report = collector.scan_once(&database).await.unwrap();
        assert_eq!(report.inserted_token_observations, 1);
        assert_eq!(report.inserted_session_metadata, 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM token_observations")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
        cleanup(&root);
    }

    #[tokio::test]
    async fn partial_tail_is_deferred_until_completed() {
        let (database, root, collector) = setup("partial").await;
        let path = root.join("sessions/2026/08/partial.jsonl");
        let meta = format!("{}\n", meta_line("session-partial"));
        let token = token_line("2026-08-04T06:00:02.000Z", 10);
        write_fixture(&path, &(meta.clone() + &token[..token.len() / 2]));
        let first = collector.scan_once(&database).await.unwrap();
        assert_eq!(first.inserted_token_observations, 0);
        assert!(first.pending_bytes > 0);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(format!("{}\n", &token[token.len() / 2..]).as_bytes())
            .unwrap();
        let second = collector.scan_once(&database).await.unwrap();
        let third = collector.scan_once(&database).await.unwrap();
        assert_eq!(second.inserted_token_observations, 1);
        assert_eq!(third.inserted_token_observations, 0);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM token_observations")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
        cleanup(&root);
    }

    #[tokio::test]
    async fn truncation_restarts_cursor_without_losing_deduplication() {
        let (database, root, collector) = setup("truncate").await;
        let path = root.join("sessions/2026/08/truncate.jsonl");
        let meta = format!("{}\n", meta_line("session-truncate"));
        let first_token = format!("{}\n", token_line("2026-08-04T06:00:02.000Z", 10));
        write_fixture(&path, &(meta.clone() + &first_token));
        assert_eq!(
            collector
                .scan_once(&database)
                .await
                .unwrap()
                .inserted_token_observations,
            1
        );

        write_fixture(&path, &meta);
        let after_truncation = collector.scan_once(&database).await.unwrap();
        assert_eq!(after_truncation.inserted_token_observations, 0);

        let second_token = format!("{}\n", token_line("2026-08-04T06:00:03.000Z", 20));
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(second_token.as_bytes())
            .unwrap();
        assert_eq!(
            collector
                .scan_once(&database)
                .await
                .unwrap()
                .inserted_token_observations,
            1
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM token_observations")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(count, 2);
        cleanup(&root);
    }

    #[tokio::test]
    async fn archive_move_reuses_inode_cursor() {
        let (database, root, collector) = setup("move").await;
        let active = root.join("sessions/2026/08/move.jsonl");
        let archived = root.join("archived_sessions/2026/08/move.jsonl");
        write_fixture(
            &active,
            &format!(
                "{}\n{}\n",
                meta_line("session-move"),
                token_line("2026-08-04T06:00:02.000Z", 10)
            ),
        );
        let first = collector.scan_once(&database).await.unwrap();
        assert_eq!(first.inserted_token_observations, 1);
        fs::rename(&active, &archived).unwrap();
        let second = collector.scan_once(&database).await.unwrap();
        assert_eq!(second.inserted_token_observations, 0);
        let file_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jsonl_files")
            .fetch_one(database.pool())
            .await
            .unwrap();
        assert_eq!(file_count, 1);
        let active_state: String =
            sqlx::query_scalar("SELECT active_state FROM jsonl_files WHERE path_key = ?")
                .bind(archived.to_string_lossy().as_ref())
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(active_state, "archived");
        cleanup(&root);
    }

    #[test]
    fn debounce_coalesces_same_path() {
        let now = Instant::now();
        let mut queue = DebouncedPathQueue::new(Duration::from_secs(2));
        queue.push_at(PathBuf::from("session.jsonl"), now);
        queue.push_at(PathBuf::from("session.jsonl"), now + Duration::from_secs(1));
        assert!(queue.take_ready(now + Duration::from_secs(2)).is_empty());
        assert_eq!(
            queue.take_ready(now + Duration::from_secs(3)),
            vec![PathBuf::from("session.jsonl")]
        );
    }
}
