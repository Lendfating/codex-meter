-- Minimal Codex Meter fact cache.
-- Daily/minute/model/session/window values are rebuilt in memory from these
-- seven tables. This schema intentionally does not mirror the App Server.

CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    inode INTEGER,
    offset_bytes INTEGER NOT NULL DEFAULT 0,
    mtime_ms INTEGER,
    state TEXT NOT NULL DEFAULT 'active',
    digest TEXT,
    line_count INTEGER NOT NULL DEFAULT 0,
    duplicate_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY,
    file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,
    source_digest TEXT NOT NULL UNIQUE,
    observed_at_ms INTEGER NOT NULL,
    kind TEXT NOT NULL,
    session_id TEXT,
    root_session_id TEXT,
    title TEXT,
    model TEXT,
    tier TEXT,
    provider TEXT,
    account_key TEXT,
    plan TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    cached_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    last_tokens_json TEXT,
    cumulative_tokens_json TEXT,
    fast INTEGER,
    quota_json TEXT,
    payload_json TEXT NOT NULL,
    quality_json TEXT NOT NULL DEFAULT '[]'
);

CREATE INDEX IF NOT EXISTS idx_events_time ON events(observed_at_ms, id);
CREATE INDEX IF NOT EXISTS idx_events_session_time ON events(session_id, observed_at_ms, id);

CREATE TABLE IF NOT EXISTS quota_samples (
    id INTEGER PRIMARY KEY,
    event_id INTEGER REFERENCES events(id) ON DELETE CASCADE,
    source_digest TEXT NOT NULL UNIQUE,
    observed_at_ms INTEGER NOT NULL,
    account_key TEXT,
    limit_id TEXT,
    window_kind TEXT NOT NULL,
    used_percent REAL,
    window_minutes INTEGER,
    resets_at_ms INTEGER,
    plan TEXT,
    source TEXT NOT NULL,
    raw_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_quota_samples_time ON quota_samples(observed_at_ms, id);
CREATE INDEX IF NOT EXISTS idx_quota_samples_window ON quota_samples(limit_id, resets_at_ms, observed_at_ms);

CREATE TABLE IF NOT EXISTS account_snapshots (
    id INTEGER PRIMARY KEY,
    observed_at_ms INTEGER NOT NULL,
    account_key TEXT,
    plan TEXT,
    provider TEXT,
    lifetime_tokens INTEGER,
    daily_json TEXT,
    source TEXT NOT NULL,
    raw_json TEXT NOT NULL,
    source_digest TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS validation_runs (
    id INTEGER PRIMARY KEY,
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    scope TEXT NOT NULL,
    pricing TEXT,
    speed TEXT,
    version TEXT,
    status TEXT NOT NULL,
    sanitized_json TEXT NOT NULL,
    comparison_json TEXT
);

CREATE TABLE IF NOT EXISTS capacities (
    id INTEGER PRIMARY KEY,
    plan_code TEXT NOT NULL CHECK (plan_code IN ('usd20', 'usd100', 'usd200')),
    credit REAL,
    effective_from_ms INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('draft', 'confirmed', 'retired')),
    note TEXT
);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
