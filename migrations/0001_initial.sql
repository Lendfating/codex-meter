-- Phase 1 storage foundation. All timestamps are UTC epoch milliseconds.

CREATE TABLE IF NOT EXISTS machines (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    install_id TEXT NOT NULL UNIQUE,
    timezone TEXT NOT NULL DEFAULT 'Asia/Shanghai',
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS account_identities (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('chatgpt', 'official_api', 'custom_api', 'bedrock', 'unknown')),
    email_masked TEXT,
    identity_hmac TEXT NOT NULL,
    label TEXT,
    created_at_ms INTEGER NOT NULL,
    UNIQUE (machine_id, identity_hmac)
);

CREATE TABLE IF NOT EXISTS account_context_intervals (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    account_identity_id INTEGER NOT NULL REFERENCES account_identities(id) ON DELETE RESTRICT,
    start_at_ms INTEGER NOT NULL,
    end_at_ms INTEGER,
    auth_kind TEXT NOT NULL CHECK (auth_kind IN ('chatgpt', 'official_api', 'custom_api', 'bedrock', 'unknown')),
    plan_type_raw TEXT,
    display_group TEXT NOT NULL CHECK (display_group IN ('plus', 'pro', 'other_api', 'other', 'unknown')),
    capacity_profile TEXT CHECK (capacity_profile IS NULL OR capacity_profile IN ('usd20', 'usd100', 'usd200')),
    provider_name TEXT,
    endpoint_hmac TEXT,
    classification_source TEXT NOT NULL CHECK (classification_source IN ('observed', 'inferred', 'manual', 'unknown')),
    source TEXT NOT NULL,
    quality TEXT NOT NULL,
    pricing_version TEXT,
    collector_version TEXT NOT NULL,
    CHECK (end_at_ms IS NULL OR end_at_ms > start_at_ms),
    UNIQUE (machine_id, start_at_ms)
);

CREATE INDEX IF NOT EXISTS idx_context_machine_start
    ON account_context_intervals (machine_id, start_at_ms);

CREATE TRIGGER IF NOT EXISTS account_context_intervals_no_overlap_insert
BEFORE INSERT ON account_context_intervals
WHEN EXISTS (
    SELECT 1
    FROM account_context_intervals AS existing
    WHERE existing.machine_id = NEW.machine_id
      AND existing.start_at_ms < COALESCE(NEW.end_at_ms, 9223372036854775807)
      AND COALESCE(existing.end_at_ms, 9223372036854775807) > NEW.start_at_ms
)
BEGIN
    SELECT RAISE(ABORT, 'account context interval overlaps an existing interval');
END;

CREATE TRIGGER IF NOT EXISTS account_context_intervals_no_overlap_update
BEFORE UPDATE OF machine_id, start_at_ms, end_at_ms ON account_context_intervals
WHEN EXISTS (
    SELECT 1
    FROM account_context_intervals AS existing
    WHERE existing.id <> NEW.id
      AND existing.machine_id = NEW.machine_id
      AND existing.start_at_ms < COALESCE(NEW.end_at_ms, 9223372036854775807)
      AND COALESCE(existing.end_at_ms, 9223372036854775807) > NEW.start_at_ms
)
BEGIN
    SELECT RAISE(ABORT, 'account context interval overlaps an existing interval');
END;

CREATE TABLE IF NOT EXISTS jsonl_files (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    path_key TEXT NOT NULL,
    session_id TEXT,
    inode INTEGER,
    offset_bytes INTEGER NOT NULL DEFAULT 0,
    mtime_ms INTEGER,
    digest TEXT,
    active_state TEXT NOT NULL DEFAULT 'active' CHECK (active_state IN ('active', 'archived', 'missing')),
    UNIQUE (machine_id, path_key),
    UNIQUE (machine_id, inode, session_id)
);

CREATE TABLE IF NOT EXISTS token_observations (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    context_interval_id INTEGER REFERENCES account_context_intervals(id) ON DELETE SET NULL,
    session_id TEXT NOT NULL,
    turn_id TEXT,
    observed_at_ms INTEGER NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    cached_input_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    model TEXT,
    service_tier TEXT,
    source_digest TEXT NOT NULL,
    collector_version TEXT NOT NULL,
    UNIQUE (machine_id, source_digest)
);

CREATE INDEX IF NOT EXISTS idx_token_observations_session_time
    ON token_observations (machine_id, session_id, observed_at_ms);

CREATE TABLE IF NOT EXISTS quota_snapshots (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    account_identity_id INTEGER NOT NULL REFERENCES account_identities(id) ON DELETE RESTRICT,
    observed_at_ms INTEGER NOT NULL,
    limit_id TEXT NOT NULL,
    limit_name TEXT,
    used_percent REAL,
    window_minutes INTEGER,
    resets_at_ms INTEGER,
    plan_type_raw TEXT,
    credits_has_credits INTEGER CHECK (credits_has_credits IS NULL OR credits_has_credits IN (0, 1)),
    credits_unlimited INTEGER CHECK (credits_unlimited IS NULL OR credits_unlimited IN (0, 1)),
    credits_balance TEXT,
    source TEXT NOT NULL,
    quality TEXT NOT NULL,
    pricing_version TEXT,
    collector_version TEXT NOT NULL,
    source_digest TEXT NOT NULL,
    UNIQUE (machine_id, source_digest)
);

CREATE INDEX IF NOT EXISTS idx_quota_snapshots_account_time
    ON quota_snapshots (machine_id, account_identity_id, observed_at_ms);

CREATE TABLE IF NOT EXISTS account_usage_snapshots (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    account_identity_id INTEGER NOT NULL REFERENCES account_identities(id) ON DELETE RESTRICT,
    observed_at_ms INTEGER NOT NULL,
    lifetime_tokens INTEGER,
    daily_buckets_json TEXT NOT NULL,
    source TEXT NOT NULL,
    quality TEXT NOT NULL,
    pricing_version TEXT,
    collector_version TEXT NOT NULL,
    source_digest TEXT NOT NULL,
    UNIQUE (machine_id, source_digest)
);

CREATE TABLE IF NOT EXISTS ccusage_session_snapshots (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    model_tokens_json TEXT NOT NULL,
    pricing_scheme TEXT NOT NULL CHECK (pricing_scheme IN ('subscription_credit', 'api_usd_equivalent')),
    auto_amount REAL NOT NULL,
    standard_amount REAL NOT NULL,
    pricing_version TEXT NOT NULL,
    ccusage_version TEXT NOT NULL,
    command_duration_ms INTEGER NOT NULL,
    result_hash TEXT NOT NULL,
    source_digest TEXT NOT NULL,
    source TEXT NOT NULL,
    quality TEXT NOT NULL,
    collector_version TEXT NOT NULL,
    UNIQUE (machine_id, source_digest)
);

CREATE INDEX IF NOT EXISTS idx_ccusage_snapshots_session_time
    ON ccusage_session_snapshots (machine_id, session_id, observed_at_ms);

CREATE TABLE IF NOT EXISTS usage_deltas (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    account_context_interval_id INTEGER REFERENCES account_context_intervals(id) ON DELETE SET NULL,
    session_id TEXT,
    start_at_ms INTEGER NOT NULL,
    end_at_ms INTEGER NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    cached_input_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    subscription_base_credit REAL,
    subscription_fast_surcharge REAL,
    subscription_total_credit REAL,
    api_base_usd REAL,
    api_fast_surcharge_usd REAL,
    api_total_usd REAL,
    source TEXT NOT NULL,
    quality TEXT NOT NULL,
    pricing_version TEXT,
    collector_version TEXT NOT NULL,
    dedupe_key TEXT NOT NULL UNIQUE,
    CHECK (end_at_ms > start_at_ms)
);

CREATE TABLE IF NOT EXISTS daily_rollups (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    local_date TEXT NOT NULL,
    account_identity_id INTEGER REFERENCES account_identities(id) ON DELETE SET NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    cached_input_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    subscription_credit REAL,
    api_usd REAL,
    source TEXT NOT NULL,
    quality TEXT NOT NULL,
    pricing_version TEXT,
    collector_version TEXT NOT NULL,
    UNIQUE (machine_id, local_date, account_identity_id)
);

CREATE TABLE IF NOT EXISTS pricing_versions (
    id TEXT PRIMARY KEY,
    scheme TEXT NOT NULL CHECK (scheme IN ('subscription_credit', 'api_usd_equivalent')),
    effective_at_ms INTEGER NOT NULL,
    timezone TEXT NOT NULL,
    rates_json TEXT NOT NULL,
    fast_multipliers_json TEXT NOT NULL,
    source_url TEXT,
    source_precision TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE (scheme, effective_at_ms)
);

CREATE TABLE IF NOT EXISTS plan_capacities (
    id TEXT PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    plan_code TEXT NOT NULL CHECK (plan_code IN ('usd20', 'usd100', 'usd200')),
    effective_from_ms INTEGER NOT NULL,
    effective_to_ms INTEGER,
    confirmed_credit REAL,
    status TEXT NOT NULL CHECK (status IN ('draft', 'confirmed', 'retired')),
    note TEXT,
    created_at_ms INTEGER NOT NULL,
    CHECK (effective_to_ms IS NULL OR effective_to_ms > effective_from_ms),
    CHECK (status <> 'confirmed' OR confirmed_credit IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_plan_capacities_machine_plan_time
    ON plan_capacities (machine_id, plan_code, effective_from_ms);

CREATE TABLE IF NOT EXISTS calibration_segments (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    account_identity_id INTEGER NOT NULL REFERENCES account_identities(id) ON DELETE RESTRICT,
    start_at_ms INTEGER NOT NULL,
    end_at_ms INTEGER NOT NULL,
    window_kind TEXT NOT NULL,
    used_percent_start REAL,
    used_percent_end REAL,
    local_credit REAL NOT NULL,
    candidate_capacity REAL,
    sample_count INTEGER NOT NULL,
    contamination TEXT,
    adopted INTEGER NOT NULL DEFAULT 0 CHECK (adopted IN (0, 1)),
    source TEXT NOT NULL,
    quality TEXT NOT NULL,
    pricing_version TEXT,
    collector_version TEXT NOT NULL,
    CHECK (end_at_ms > start_at_ms)
);

CREATE TABLE IF NOT EXISTS manual_annotations (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    target_type TEXT NOT NULL,
    target_id TEXT NOT NULL,
    before_json TEXT NOT NULL,
    after_json TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    source TEXT NOT NULL DEFAULT 'manual'
);

CREATE TABLE IF NOT EXISTS collector_runs (
    id INTEGER PRIMARY KEY,
    machine_id INTEGER NOT NULL REFERENCES machines(id) ON DELETE CASCADE,
    source TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    duration_ms INTEGER,
    status TEXT NOT NULL,
    stderr_summary TEXT,
    collector_version TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_collector_runs_source_time
    ON collector_runs (machine_id, source, started_at_ms);
