-- Codex Meter minimal data model.
--
-- This is the only schema initialized by the current runtime.  The historical
-- migrations/0001.sql file and databases created from it are not read or
-- written by the active seven-table pipeline.

CREATE TABLE IF NOT EXISTS source_jsonl (
    id INTEGER PRIMARY KEY,
    source_key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('session', 'turn', 'usage', 'quota')),
    observed_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER,
    session_id TEXT,
    root_session_id TEXT,
    turn_id TEXT,
    relation TEXT CHECK (relation IS NULL OR relation IN ('main', 'child', 'fork', 'unknown')),
    title TEXT,
    started_at_ms INTEGER,
    ended_at_ms INTEGER,
    model TEXT,
    service_tier TEXT,
    provider TEXT,
    plan_type TEXT,
    input_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    output_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_tokens INTEGER,
    limit_id TEXT,
    window_kind TEXT,
    used_percent REAL,
    window_minutes INTEGER,
    resets_at_ms INTEGER,
    quality TEXT
);

CREATE INDEX IF NOT EXISTS idx_source_jsonl_time
    ON source_jsonl(observed_at_ms, id);
CREATE INDEX IF NOT EXISTS idx_source_jsonl_session_time
    ON source_jsonl(root_session_id, session_id, observed_at_ms, id);
CREATE INDEX IF NOT EXISTS idx_source_jsonl_turn
    ON source_jsonl(turn_id, observed_at_ms, id);
CREATE INDEX IF NOT EXISTS idx_source_jsonl_quota
    ON source_jsonl(limit_id, resets_at_ms, observed_at_ms);

CREATE TABLE IF NOT EXISTS source_app_server (
    id INTEGER PRIMARY KEY,
    source_key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('account', 'quota', 'usage')),
    first_seen_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER NOT NULL,
    account_key TEXT,
    account_label TEXT,
    auth_kind TEXT,
    provider TEXT,
    plan_type TEXT,
    limit_id TEXT,
    window_kind TEXT,
    used_percent REAL,
    window_minutes INTEGER,
    resets_at_ms INTEGER,
    lifetime_tokens INTEGER,
    daily_tokens_json TEXT,
    freshness TEXT CHECK (
        freshness IS NULL OR freshness IN ('pending', 'stale', 'settled', 'unavailable')
    ),
    status TEXT NOT NULL CHECK (status IN ('ok', 'unavailable'))
);

CREATE INDEX IF NOT EXISTS idx_source_app_server_kind_time
    ON source_app_server(kind, last_seen_at_ms, id);
CREATE INDEX IF NOT EXISTS idx_source_app_server_account_quota
    ON source_app_server(account_key, limit_id, resets_at_ms, last_seen_at_ms);

CREATE TABLE IF NOT EXISTS source_ccusage (
    id INTEGER PRIMARY KEY,
    source_key TEXT NOT NULL UNIQUE,
    run_at_ms INTEGER NOT NULL,
    range_start_ms INTEGER NOT NULL,
    range_end_ms INTEGER NOT NULL,
    scope TEXT NOT NULL CHECK (scope IN ('daily', 'session')),
    scope_key TEXT NOT NULL,
    pricing_scheme TEXT NOT NULL CHECK (pricing_scheme IN ('subscription', 'api')),
    speed TEXT NOT NULL CHECK (speed IN ('auto', 'standard')),
    input_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    output_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_tokens INTEGER,
    amount REAL,
    model_breakdown_json TEXT,
    ccusage_version TEXT,
    pricing_version TEXT,
    status TEXT NOT NULL CHECK (status IN ('ok', 'failed', 'incomparable'))
);

CREATE INDEX IF NOT EXISTS idx_source_ccusage_scope
    ON source_ccusage(scope, scope_key, run_at_ms);
CREATE INDEX IF NOT EXISTS idx_source_ccusage_range
    ON source_ccusage(range_start_ms, range_end_ms, scope);

CREATE TABLE IF NOT EXISTS usage_daily (
    local_date TEXT PRIMARY KEY,
    account_key TEXT,
    auth_kind TEXT,
    plan_type TEXT,
    capacity_profile TEXT,
    input_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    output_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_tokens INTEGER,
    credit REAL,
    api_usd REAL,
    local_percent REAL,
    account_tokens INTEGER,
    unobserved_tokens INTEGER,
    coverage_ratio REAL,
    account_token_freshness TEXT CHECK (
        account_token_freshness IS NULL OR account_token_freshness IN (
            'pending', 'stale', 'settled', 'unavailable'
        )
    ),
    official_percent_start REAL,
    official_percent_end REAL,
    official_percent_delta REAL,
    reset_count INTEGER NOT NULL DEFAULT 0,
    quality TEXT
);

CREATE INDEX IF NOT EXISTS idx_usage_daily_account_date
    ON usage_daily(account_key, local_date);

CREATE TABLE IF NOT EXISTS usage_minute (
    id INTEGER PRIMARY KEY,
    bucket_key TEXT NOT NULL UNIQUE,
    minute_start_ms INTEGER NOT NULL,
    local_date TEXT NOT NULL,
    account_key TEXT,
    auth_kind TEXT,
    plan_type TEXT,
    provider TEXT,
    capacity_profile TEXT,
    window_id TEXT,
    window_start_ms INTEGER,
    resets_at_ms INTEGER,
    reset_marker INTEGER NOT NULL DEFAULT 0 CHECK (reset_marker IN (0, 1)),
    input_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    output_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_tokens INTEGER,
    credit REAL,
    api_usd REAL,
    official_used_percent REAL,
    official_source TEXT CHECK (
        official_source IS NULL OR official_source IN ('app_server', 'jsonl', 'none')
    ),
    quality TEXT
);

CREATE INDEX IF NOT EXISTS idx_usage_minute_date_time
    ON usage_minute(local_date, minute_start_ms, id);
CREATE INDEX IF NOT EXISTS idx_usage_minute_window_time
    ON usage_minute(window_id, minute_start_ms, id);
CREATE INDEX IF NOT EXISTS idx_usage_minute_account_time
    ON usage_minute(account_key, minute_start_ms, id);

CREATE TABLE IF NOT EXISTS usage_session (
    id INTEGER PRIMARY KEY,
    row_key TEXT NOT NULL UNIQUE,
    local_date TEXT NOT NULL,
    root_session_id TEXT,
    session_id TEXT,
    turn_id TEXT,
    title TEXT,
    relation TEXT CHECK (relation IS NULL OR relation IN ('main', 'child', 'fork', 'unknown')),
    started_at_ms INTEGER,
    ended_at_ms INTEGER,
    window_id TEXT,
    account_key TEXT,
    auth_kind TEXT,
    plan_type TEXT,
    provider TEXT,
    capacity_profile TEXT,
    primary_model TEXT,
    fast_state TEXT CHECK (
        fast_state IS NULL OR fast_state IN ('fast', 'standard', 'mixed', 'unknown')
    ),
    model_breakdown_json TEXT,
    input_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    output_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_tokens INTEGER,
    credit REAL,
    api_usd REAL,
    official_percent_start REAL,
    official_percent_end REAL,
    quality TEXT
);

CREATE INDEX IF NOT EXISTS idx_usage_session_date_root
    ON usage_session(local_date, root_session_id, started_at_ms, id);
CREATE INDEX IF NOT EXISTS idx_usage_session_window
    ON usage_session(window_id, started_at_ms, id);
CREATE INDEX IF NOT EXISTS idx_usage_session_turn
    ON usage_session(turn_id, started_at_ms, id);

CREATE TABLE IF NOT EXISTS capacities_v2 (
    id INTEGER PRIMARY KEY,
    profile_code TEXT NOT NULL CHECK (profile_code IN ('usd20', 'usd100', 'usd200')),
    account_key TEXT,
    plan_type TEXT,
    weekly_credit REAL NOT NULL CHECK (weekly_credit >= 0),
    effective_from_ms INTEGER NOT NULL,
    effective_to_ms INTEGER,
    confirmed_at_ms INTEGER NOT NULL,
    UNIQUE (profile_code, account_key, effective_from_ms)
);

CREATE INDEX IF NOT EXISTS idx_capacities_account_effective
    ON capacities_v2(account_key, effective_from_ms, effective_to_ms);
