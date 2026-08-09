use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    thread,
    time::Instant,
};

use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{Date, OffsetDateTime, UtcOffset};

use super::*;

struct ParsedFile {
    path: PathBuf,
    session_id: Option<String>,
    records: Vec<SourceJsonlRecord>,
    usage_events: Vec<(i64, TokenCounts)>,
    cursor: CursorState,
    complete_lines: usize,
    changed: bool,
    profile: FileProfile,
}

#[derive(Default)]
struct FileProfile {
    bytes_scanned: u64,
    relevant_lines: usize,
    parsed_records: usize,
    json_parse_nanos: u128,
    record_parse_nanos: u128,
}

pub(super) async fn scan_once(
    collector: &JsonlCollector,
    database: &Database,
) -> Result<JsonlScanReport, JsonlError> {
    let profile = profile_enabled();
    let total_started = Instant::now();
    let metadata_started = Instant::now();
    let mut cursors = load_cursors(collector.cursor_path.as_deref())?;
    let local_metadata = load_local_session_metadata(&collector.codex_home).await;
    let metadata_elapsed = metadata_started.elapsed();
    let discover_started = Instant::now();
    let all_paths = collector.discover_paths()?;
    let discover_elapsed = discover_started.elapsed();
    let headers_started = Instant::now();
    let headers = read_headers(&all_paths)?;
    let headers_elapsed = headers_started.elapsed();
    let parent_graph = parent_graph(&headers, &local_metadata);
    let select_started = Instant::now();
    let candidates = select_candidates(collector, &all_paths, &headers, &cursors)?;
    let select_elapsed = select_started.elapsed();
    let initial_root_repair = !collector.root_repaired.load(Ordering::Acquire);
    let need_replay = initial_root_repair
        || candidates
            .iter()
            .any(|(path, _)| !cursors.files.contains_key(path.to_string_lossy().as_ref()));
    let parse_started = Instant::now();
    let parsed = parse_candidates(&candidates, profile)?;
    let parse_elapsed = parse_started.elapsed();
    let parsed_profile = parsed
        .iter()
        .fold(FileProfile::default(), |mut total, file| {
            total.bytes_scanned += file.profile.bytes_scanned;
            total.relevant_lines += file.profile.relevant_lines;
            total.parsed_records += file.profile.parsed_records;
            total.json_parse_nanos += file.profile.json_parse_nanos;
            total.record_parse_nanos += file.profile.record_parse_nanos;
            total
        });

    let mut report = JsonlScanReport {
        files_scanned: candidates.len(),
        ..Default::default()
    };
    for file in &parsed {
        report.complete_lines += file.complete_lines;
        report.files_changed += usize::from(file.changed);
        cursors.files.insert(
            file.path.to_string_lossy().into_owned(),
            file.cursor.clone(),
        );
    }

    let replay_started = Instant::now();
    let replay_specs = if need_replay {
        build_replay_specs(&parsed, &headers, &all_paths)?
    } else {
        BTreeMap::new()
    };
    let replay_elapsed = replay_started.elapsed();
    let mut records = Vec::new();
    let from_ms = collector.from_date.map(date_start_ms);
    for file in parsed {
        let mut replay_state = ReplayState::new(replay_specs.get(&file.path));
        for mut record in file.records {
            if record.kind == "usage"
                && replay_state
                    .as_mut()
                    .is_some_and(|state| state.should_skip(&record_tokens(&record)))
            {
                continue;
            }
            enrich_record_from_metadata(&mut record, &local_metadata, &parent_graph);
            if from_ms.is_some_and(|from| record.observed_at_ms < from) {
                continue;
            }
            report.recognized_events += 1;
            records.push(record);
        }
    }

    let database_started = Instant::now();
    let batch = database.upsert_source_jsonl_batch(records).await?;
    let database_elapsed = database_started.elapsed();
    report.inserted_events = batch.inserted_events;
    report.duplicate_events = batch.duplicate_events;
    report.inserted_quota_samples = batch.inserted_quota_samples;
    let cursor_started = Instant::now();
    save_cursors(collector.cursor_path.as_deref(), &cursors)?;
    let cursor_elapsed = cursor_started.elapsed();

    let repair_started = Instant::now();
    if initial_root_repair {
        repair_existing_roots(database, &parent_graph).await?;
        collector.root_repaired.store(true, Ordering::Release);
    }
    let repair_elapsed = repair_started.elapsed();
    if profile {
        let candidate_bytes = candidates
            .iter()
            .filter_map(|(path, _)| fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .sum::<u64>();
        eprintln!(
            "jsonl_profile total_ms={} metadata_ms={} discover_ms={} headers_ms={} select_ms={} parse_wall_ms={} replay_ms={} database_ms={} cursor_ms={} repair_ms={} all_files={} candidates={} candidate_bytes={} scanned_bytes={} complete_lines={} relevant_lines={} parsed_records={} json_parse_ms={} record_parse_ms={} records={} inserted={} duplicates={}",
            total_started.elapsed().as_millis(),
            metadata_elapsed.as_millis(),
            discover_elapsed.as_millis(),
            headers_elapsed.as_millis(),
            select_elapsed.as_millis(),
            parse_elapsed.as_millis(),
            replay_elapsed.as_millis(),
            database_elapsed.as_millis(),
            cursor_elapsed.as_millis(),
            repair_elapsed.as_millis(),
            all_paths.len(),
            candidates.len(),
            candidate_bytes,
            parsed_profile.bytes_scanned,
            report.complete_lines,
            parsed_profile.relevant_lines,
            parsed_profile.parsed_records,
            parsed_profile.json_parse_nanos / 1_000_000,
            parsed_profile.record_parse_nanos / 1_000_000,
            report.recognized_events,
            report.inserted_events,
            report.duplicate_events,
        );
    }
    Ok(report)
}

fn select_candidates(
    collector: &JsonlCollector,
    paths: &[(PathBuf, &'static str)],
    headers: &BTreeMap<PathBuf, ReplayMetadata>,
    cursors: &CursorStore,
) -> Result<Vec<(PathBuf, Option<CursorState>)>, JsonlError> {
    paths
        .iter()
        .filter_map(|(path, _)| {
            let key = path.to_string_lossy();
            let existing = cursors.files.get(key.as_ref()).cloned();
            let included = existing.is_some()
                || collector.from_date.is_none()
                || headers
                    .get(path)
                    .is_none_or(|header| file_is_in_range(path, header, collector.from_date));
            included.then_some((path.clone(), existing))
        })
        .collect::<Vec<_>>()
        .pipe(Ok)
}

fn file_is_in_range(path: &Path, header: &ReplayMetadata, from: Option<Date>) -> bool {
    let Some(from) = from else {
        return true;
    };
    let session_date = header
        .forked_at_ms
        .and_then(local_date_from_ms)
        .or_else(|| path_date(path));
    if session_date.is_some_and(|date| date >= from) {
        return true;
    }
    let Some(mtime) = fs::metadata(path)
        .ok()
        .and_then(|value| value.modified().ok())
        .and_then(system_time_ms)
    else {
        return session_date.is_none();
    };
    mtime >= date_start_ms(from)
}

fn path_date(path: &Path) -> Option<Date> {
    let text = path.to_string_lossy();
    for index in 0..text.len().saturating_sub(9) {
        let candidate = &text[index..index + 10];
        if candidate.as_bytes().get(4) == Some(&b'-')
            && candidate.as_bytes().get(7) == Some(&b'-')
            && candidate.as_bytes()[..4].iter().all(u8::is_ascii_digit)
            && candidate.as_bytes()[5..7].iter().all(u8::is_ascii_digit)
            && candidate.as_bytes()[8..10].iter().all(u8::is_ascii_digit)
        {
            if let Ok(date) = parse_date(candidate) {
                return Some(date);
            }
        }
    }
    None
}

fn date_start_ms(date: Date) -> i64 {
    date.with_hms(0, 0, 0)
        .ok()
        .map(|value| {
            value
                .assume_offset(UtcOffset::from_hms(8, 0, 0).unwrap_or(UtcOffset::UTC))
                .unix_timestamp_nanos()
        })
        .and_then(|value| i64::try_from(value / 1_000_000).ok())
        .unwrap_or(i64::MIN)
}

fn local_date_from_ms(value: i64) -> Option<Date> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value) * 1_000_000)
        .ok()
        .map(|value| {
            value
                .to_offset(UtcOffset::from_hms(8, 0, 0).unwrap_or(UtcOffset::UTC))
                .date()
        })
}

fn read_headers(
    paths: &[(PathBuf, &'static str)],
) -> Result<BTreeMap<PathBuf, ReplayMetadata>, JsonlError> {
    paths
        .iter()
        .map(|(path, _)| Ok((path.clone(), read_header(path)?)))
        .collect()
}

fn read_header(path: &Path) -> Result<ReplayMetadata, JsonlError> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(ReplayMetadata::default());
        }
        trim_line_end(&mut line);
        if line.is_empty() || !has_json_type(&line, b"session_meta") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let payload = value.get("payload").unwrap_or(&Value::Null);
        return Ok(ReplayMetadata {
            session_id: payload
                .get("id")
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            parent_session_id: session_parent(payload),
            forked_at_ms: parse_timestamp(value.get("timestamp")).ok(),
        });
    }
}

fn parent_graph(
    headers: &BTreeMap<PathBuf, ReplayMetadata>,
    local_metadata: &BTreeMap<String, LocalSessionMetadata>,
) -> BTreeMap<String, String> {
    let mut graph = local_metadata
        .iter()
        .filter_map(|(session_id, value)| {
            value
                .parent_session_id
                .as_ref()
                .map(|parent| (session_id.clone(), parent.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    for header in headers.values() {
        if let (Some(session_id), Some(parent)) = (
            header.session_id.as_ref(),
            header.parent_session_id.as_ref(),
        ) {
            graph.insert(session_id.clone(), parent.clone());
        }
    }
    graph
}

fn parse_candidates(
    candidates: &[(PathBuf, Option<CursorState>)],
    profile: bool,
) -> Result<Vec<ParsedFile>, JsonlError> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let workers = thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .min(candidates.len());
    let mut jobs = candidates.to_vec();
    jobs.sort_by_key(|(path, _)| {
        std::cmp::Reverse(fs::metadata(path).map(|value| value.len()).unwrap_or(0))
    });
    let mut worker_jobs = (0..workers)
        .map(|_| Vec::new())
        .collect::<Vec<Vec<(PathBuf, Option<CursorState>)>>>();
    for (index, job) in jobs.into_iter().enumerate() {
        worker_jobs[index % workers].push(job);
    }
    thread::scope(|scope| {
        let handles = worker_jobs.into_iter().map(|jobs| {
            scope.spawn(move || {
                jobs.iter()
                    .map(|job| parse_file(job, profile))
                    .collect::<Result<Vec<_>, _>>()
            })
        });
        let mut parsed = Vec::with_capacity(candidates.len());
        for handle in handles {
            let result = handle.join().map_err(|_| JsonlError::WorkerPanic)??;
            parsed.extend(result);
        }
        Ok(parsed)
    })
}

fn parse_file(
    job: &(PathBuf, Option<CursorState>),
    profile: bool,
) -> Result<ParsedFile, JsonlError> {
    let (path, existing) = job;
    let file_metadata = fs::metadata(path)?;
    let mtime_ms = file_metadata.modified().ok().and_then(system_time_ms);
    let file_len = file_metadata.len();
    if let Some(cursor) = existing {
        if cursor.offset_bytes == file_len && cursor.mtime_ms == mtime_ms {
            return Ok(ParsedFile {
                path: path.clone(),
                session_id: cursor.context.session_id.clone(),
                records: Vec::new(),
                usage_events: Vec::new(),
                cursor: cursor.clone(),
                complete_lines: 0,
                changed: false,
                profile: FileProfile::default(),
            });
        }
    }

    let mut start = existing
        .as_ref()
        .map(|value| value.offset_bytes)
        .unwrap_or(0);
    let mut context = existing
        .as_ref()
        .filter(|_| start > 0)
        .map(|value| value.context.clone())
        .unwrap_or_default();
    if start > file_len
        || existing.as_ref().is_some_and(|value| {
            value.digest.as_deref() != cursor_digest(path, value.offset_bytes).ok().as_deref()
        })
    {
        start = 0;
        context = FileContext::default();
    }

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::with_capacity(128 * 1024, file);
    let mut line = Vec::with_capacity(1024);
    let mut complete_lines = 0;
    let mut complete_bytes = 0_u64;
    let mut records = Vec::new();
    let mut usage_events = Vec::new();
    let mut session_id = context.session_id.clone();
    let mut file_profile = FileProfile::default();

    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            break;
        }
        complete_lines += 1;
        complete_bytes = complete_bytes.saturating_add(read as u64);
        trim_line_end(&mut line);
        if line.is_empty() || !likely_relevant(&line) {
            continue;
        }
        file_profile.relevant_lines += 1;
        let json_started = profile.then(Instant::now);
        let value = serde_json::from_slice::<Value>(&line);
        if let Some(started) = json_started {
            file_profile.json_parse_nanos += started.elapsed().as_nanos();
        }
        let Ok(value) = value else {
            continue;
        };
        let record_started = profile.then(Instant::now);
        let parsed = match records_for_line(&line, &value, &mut context) {
            Ok(records) => records,
            Err(JsonlError::Timestamp(_)) => continue,
            Err(error) => return Err(error),
        };
        if let Some(started) = record_started {
            file_profile.record_parse_nanos += started.elapsed().as_nanos();
        }
        session_id = context.session_id.clone();
        file_profile.parsed_records += parsed.len();
        for record in parsed {
            if record.kind == "usage" {
                let tokens = record_tokens(&record);
                if tokens.observed() {
                    usage_events.push((record.observed_at_ms, tokens));
                }
            }
            records.push(record);
        }
    }

    let next_offset = start.saturating_add(complete_bytes);
    let digest = (next_offset > 0)
        .then(|| cursor_digest(path, next_offset).ok())
        .flatten();
    Ok(ParsedFile {
        path: path.clone(),
        session_id,
        records,
        usage_events,
        cursor: CursorState {
            offset_bytes: next_offset,
            mtime_ms,
            digest,
            context,
        },
        complete_lines,
        changed: complete_bytes > 0 || existing.is_none(),
        profile: FileProfile {
            bytes_scanned: complete_bytes,
            ..file_profile
        },
    })
}

fn profile_enabled() -> bool {
    std::env::var_os("CODEX_METER_PROFILE").is_some()
}

fn cursor_digest(path: &Path, length: u64) -> io::Result<String> {
    const SAMPLE_BYTES: u64 = 64 * 1024;
    let mut digest = Sha256::new();
    let mut file = File::open(path)?;
    let first_len = length.min(SAMPLE_BYTES);
    let mut buffer = vec![0_u8; first_len as usize];
    file.read_exact(&mut buffer)?;
    digest.update(&buffer);
    if length > first_len {
        file.seek(SeekFrom::Start(length.saturating_sub(SAMPLE_BYTES)))?;
        let tail_len = length.min(SAMPLE_BYTES) as usize;
        buffer.resize(tail_len, 0);
        file.read_exact(&mut buffer)?;
        digest.update(&buffer);
    }
    Ok(hex_digest(&digest.finalize()))
}

fn build_replay_specs(
    parsed: &[ParsedFile],
    headers: &BTreeMap<PathBuf, ReplayMetadata>,
    all_paths: &[(PathBuf, &'static str)],
) -> Result<BTreeMap<PathBuf, ReplaySpec>, JsonlError> {
    let mut usage_by_session = BTreeMap::<String, Vec<(i64, TokenCounts)>>::new();
    let parsed_paths = parsed
        .iter()
        .map(|file| file.path.clone())
        .collect::<std::collections::HashSet<_>>();
    for file in parsed {
        if let Some(session_id) = file.session_id.clone().or_else(|| {
            headers
                .get(&file.path)
                .and_then(|value| value.session_id.clone())
        }) {
            usage_by_session.insert(session_id, file.usage_events.clone());
        }
    }

    let mut files_by_session = BTreeMap::<String, PathBuf>::new();
    for (path, _) in all_paths {
        if let Some(session_id) = headers.get(path).and_then(|value| value.session_id.clone()) {
            files_by_session
                .entry(session_id)
                .or_insert_with(|| path.clone());
        }
    }

    let mut parent_paths = BTreeMap::<PathBuf, String>::new();
    for file in parsed {
        let Some(header) = headers.get(&file.path) else {
            continue;
        };
        let Some(parent_id) = header.parent_session_id.clone() else {
            continue;
        };
        if let Some(parent_path) = files_by_session
            .get(&parent_id)
            .filter(|path| **path != file.path)
        {
            parent_paths.insert(parent_path.clone(), parent_id);
        }
    }

    for (path, session_id) in parent_paths {
        if parsed_paths.contains(&path) || usage_by_session.contains_key(&session_id) {
            continue;
        }
        usage_by_session.insert(session_id, read_usage_events_stream(&path)?);
    }

    let mut specs = BTreeMap::new();
    for file in parsed {
        let Some(header) = headers.get(&file.path) else {
            continue;
        };
        let Some(parent_id) = header.parent_session_id.as_ref() else {
            continue;
        };
        let child_events = &file.usage_events;
        let fallback_burst_len = leading_rewritten_burst_len(child_events);
        let parent_prefix = usage_by_session
            .get(parent_id)
            .into_iter()
            .flat_map(|events| events.iter())
            .filter(|(timestamp, _)| {
                header
                    .forked_at_ms
                    .is_none_or(|forked_at| *timestamp <= forked_at)
            })
            .map(|(_, tokens)| *tokens)
            .collect::<Vec<_>>();
        specs.insert(
            file.path.clone(),
            ReplaySpec {
                parent_prefix,
                fallback_burst_len,
            },
        );
    }
    Ok(specs)
}

fn read_usage_events_stream(path: &Path) -> Result<Vec<(i64, TokenCounts)>, JsonlError> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(128 * 1024, file);
    let mut line = Vec::with_capacity(1024);
    let mut context = FileContext::default();
    let mut events = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        trim_line_end(&mut line);
        if line.is_empty() || !likely_relevant(&line) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let Ok(records) = records_for_line(&line, &value, &mut context) else {
            continue;
        };
        for record in records.into_iter().filter(|record| record.kind == "usage") {
            let tokens = record_tokens(&record);
            if tokens.observed() {
                events.push((record.observed_at_ms, tokens));
            }
        }
    }
    Ok(events)
}

fn likely_relevant(line: &[u8]) -> bool {
    let prefix = b"\"type\":\"";
    let mut event_msg = false;
    let mut nested_event = false;
    let mut index = 0;
    while index + prefix.len() < line.len() {
        if line[index] != b'"' || !line[index..].starts_with(prefix) {
            index += 1;
            continue;
        }
        let value_start = index + prefix.len();
        let Some(value_length) = line[value_start..].iter().position(|byte| *byte == b'"') else {
            break;
        };
        let value = &line[value_start..value_start + value_length];
        match value {
            b"session_meta" | b"turn_context" => return true,
            b"event_msg" => event_msg = true,
            b"token_count"
            | b"task_started"
            | b"task_complete"
            | b"task_aborted"
            | b"turn_aborted"
            | b"thread_settings_applied" => nested_event = true,
            _ => {}
        }
        index = value_start + value_length + 1;
    }
    event_msg && nested_event
}

fn has_json_type(line: &[u8], value: &[u8]) -> bool {
    let prefix = b"\"type\":\"";
    line.windows(prefix.len() + value.len() + 1).any(|window| {
        window.starts_with(prefix)
            && window[prefix.len()..prefix.len() + value.len()] == *value
            && window.last() == Some(&b'"')
    })
}

fn trim_line_end(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
}

trait Pipe: Sized {
    fn pipe<T>(self, function: impl FnOnce(Self) -> T) -> T {
        function(self)
    }
}

impl<T> Pipe for T {}
