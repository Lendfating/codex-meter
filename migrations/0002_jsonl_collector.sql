-- Phase 2 JSONL collector cursor and allowlisted event projections.

ALTER TABLE jsonl_files ADD COLUMN cli_version TEXT;
ALTER TABLE jsonl_files ADD COLUMN model_provider TEXT;
ALTER TABLE jsonl_files ADD COLUMN thread_source TEXT;
ALTER TABLE jsonl_files ADD COLUMN session_started_at_ms INTEGER;
ALTER TABLE jsonl_files ADD COLUMN last_model TEXT;
ALTER TABLE jsonl_files ADD COLUMN last_model_provider_id TEXT;
ALTER TABLE jsonl_files ADD COLUMN last_service_tier TEXT;

ALTER TABLE token_observations ADD COLUMN model_provider TEXT;
ALTER TABLE token_observations ADD COLUMN model_context_window INTEGER;
ALTER TABLE token_observations ADD COLUMN rate_limits_json TEXT;

CREATE TABLE IF NOT EXISTS jsonl_session_metadata (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    cli_version TEXT,
    model_provider TEXT,
    thread_source TEXT,
    source_digest TEXT NOT NULL,
    collector_version TEXT NOT NULL,
    UNIQUE (machine_id, source_digest)
);

CREATE INDEX IF NOT EXISTS idx_jsonl_session_metadata_session_time
    ON jsonl_session_metadata (machine_id, session_id, observed_at_ms);

CREATE TABLE IF NOT EXISTS jsonl_thread_settings (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    model TEXT,
    model_provider_id TEXT,
    service_tier TEXT NOT NULL,
    source_digest TEXT NOT NULL,
    collector_version TEXT NOT NULL,
    UNIQUE (machine_id, source_digest)
);

CREATE INDEX IF NOT EXISTS idx_jsonl_thread_settings_session_time
    ON jsonl_thread_settings (machine_id, session_id, observed_at_ms);

CREATE TABLE IF NOT EXISTS jsonl_rate_limit_observations (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    limit_id TEXT NOT NULL,
    limit_name TEXT,
    plan_type_raw TEXT,
    primary_used_percent REAL,
    primary_window_minutes INTEGER,
    primary_resets_at_ms INTEGER,
    secondary_used_percent REAL,
    secondary_window_minutes INTEGER,
    secondary_resets_at_ms INTEGER,
    credits_has_credits INTEGER CHECK (credits_has_credits IS NULL OR credits_has_credits IN (0, 1)),
    credits_unlimited INTEGER CHECK (credits_unlimited IS NULL OR credits_unlimited IN (0, 1)),
    credits_balance TEXT,
    source_digest TEXT NOT NULL,
    collector_version TEXT NOT NULL,
    UNIQUE (machine_id, source_digest)
);

CREATE INDEX IF NOT EXISTS idx_jsonl_rate_limits_session_time
    ON jsonl_rate_limit_observations (machine_id, session_id, observed_at_ms);

CREATE TABLE IF NOT EXISTS jsonl_daily_token_rollups (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    local_date TEXT NOT NULL,
    timezone TEXT NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    cached_input_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL,
    quality TEXT NOT NULL,
    collector_version TEXT NOT NULL,
    source_digest TEXT NOT NULL,
    UNIQUE (machine_id, local_date)
);

CREATE INDEX IF NOT EXISTS idx_jsonl_daily_token_rollups_date
    ON jsonl_daily_token_rollups (machine_id, local_date);
