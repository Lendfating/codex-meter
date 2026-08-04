#!/usr/bin/env python3
"""Validate Phase 0 fixture privacy and evidence assets."""

from __future__ import annotations

import json
import hashlib
import re
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = ROOT / "fixtures/jsonl"
MAPPING = ROOT / "fixtures/mappings/provider-history.json"
CCUSAGE_LOCK = ROOT / "config/ccusage.lock.json"
CCUSAGE_CONTRACT = ROOT / "fixtures/ccusage/contract.v20.0.19.json"
SCHEMA_MANIFEST = ROOT / "fixtures/app-server/codex-0.146.0-alpha.3.1/manifest.json"

TOP_KEYS = {"timestamp", "type", "payload"}
SENSITIVE_KEY_PARTS = (
    "prompt",
    "message",
    "reply",
    "response",
    "authorization",
    "api_key",
    "apikey",
    "email",
    "header",
    "query",
    "url",
    "cwd",
    "path",
)
SENSITIVE_VALUE_PATTERNS = (
    re.compile(r"(?i)bearer\s+[a-z0-9._~+/=-]+"),
    re.compile(r"(?i)authorization"),
    re.compile(r"(?i)sk-[a-z0-9_-]{8,}"),
    re.compile(r"(?i)https?://"),
    re.compile(r"(?i)ftp://"),
    re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
)


def walk_strings(value: Any, key: str = ""):
    if isinstance(value, dict):
        for child_key, child_value in value.items():
            yield from walk_strings(child_value, str(child_key))
    elif isinstance(value, list):
        for child_value in value:
            yield from walk_strings(child_value, key)
    elif isinstance(value, str):
        yield key, value


def assert_no_sensitive_content(value: Any, label: str) -> None:
    for key, string in walk_strings(value):
        lowered_key = key.lower()
        if any(part in lowered_key for part in SENSITIVE_KEY_PARTS):
            raise AssertionError(f"{label}: sensitive key {key!r}")
        for pattern in SENSITIVE_VALUE_PATTERNS:
            if pattern.search(string):
                raise AssertionError(f"{label}: sensitive value matched {pattern.pattern!r}")


def validate_fixture(path: Path) -> tuple[int, set[str], set[str]]:
    if not path.is_file():
        raise AssertionError(f"missing sanitized fixture: {path}")
    allowed_session_payload = {
        "cli_version",
        "id",
        "model_provider",
        "session_id",
        "thread_source",
        "timestamp",
    }
    allowed_event_payload = {"type", "info", "rate_limits", "thread_settings"}
    allowed_usage = {
        "input_tokens",
        "cached_input_tokens",
        "cache_write_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
        "total_tokens",
    }
    allowed_rate_limits = {
        "limit_id",
        "limit_name",
        "plan_type",
        "primary",
        "secondary",
        "credits",
    }
    allowed_window = {"used_percent", "window_minutes", "resets_at"}
    allowed_credits = {"has_credits", "unlimited", "balance"}
    allowed_thread_settings = {"model", "model_provider_id", "service_tier"}
    event_types: list[str] = []
    providers: set[str] = set()
    with path.open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, 1):
            record = json.loads(line)
            assert set(record) == TOP_KEYS, (line_number, set(record))
            assert record["type"] in {"session_meta", "event_msg"}
            payload = record["payload"]
            assert isinstance(payload, dict)
            if record["type"] == "session_meta":
                assert set(payload) <= allowed_session_payload
                providers.add(payload.get("model_provider"))
            else:
                assert set(payload) <= allowed_event_payload
                event_type = payload.get("type")
                assert event_type in {"token_count", "thread_settings_applied"}
                event_types.append(event_type)
                if event_type == "token_count":
                    info = payload.get("info")
                    assert isinstance(info, dict)
                    for usage_key in ("last_token_usage", "total_token_usage"):
                        if usage_key in info:
                            assert set(info[usage_key]) <= allowed_usage
                    rate_limits = payload.get("rate_limits")
                    if isinstance(rate_limits, dict):
                        assert set(rate_limits) <= allowed_rate_limits
                        for window_key in ("primary", "secondary"):
                            window = rate_limits.get(window_key)
                            if isinstance(window, dict):
                                assert set(window) <= allowed_window
                        credits = rate_limits.get("credits")
                        if isinstance(credits, dict):
                            assert set(credits) <= allowed_credits
                if event_type == "thread_settings_applied":
                    settings = payload.get("thread_settings")
                    assert isinstance(settings, dict)
                    assert set(settings) <= allowed_thread_settings
            assert_no_sensitive_content(record, f"{path.name} line {line_number}")
    assert "token_count" in event_types
    return len(event_types), providers, set(event_types)


def validate_mapping() -> None:
    data = json.loads(MAPPING.read_text(encoding="utf-8"))
    assert data["schema_version"] == 1
    assert len(data["mappings"]) == 1
    mapping = data["mappings"][0]
    assert mapping["match"] == {"model_provider": "pro", "plan_type_raw": None}
    assert mapping["display_group"] == "other_api"
    assert mapping["classification_source"] == "manual"
    assert mapping["reversible"] is True
    assert_no_sensitive_content(data, "provider mapping")


def validate_json_asset(path: Path, required: set[str]) -> dict[str, Any]:
    if not path.is_file():
        raise AssertionError(f"missing asset: {path}")
    data = json.loads(path.read_text(encoding="utf-8"))
    assert required <= set(data), (path, set(data))
    assert_no_sensitive_content(data, str(path))
    return data


def validate_schema_manifest() -> None:
    manifest = validate_json_asset(SCHEMA_MANIFEST, {"codex_version", "schema_files"})
    schema_dir = SCHEMA_MANIFEST.parent
    assert manifest["codex_version"] == "0.146.0-alpha.3.1"
    assert manifest["schema_files"]
    for entry in manifest["schema_files"]:
        assert set(entry) == {"file", "sha256"}
        file = schema_dir / entry["file"]
        assert file.is_file(), file
        assert hashlib.sha256(file.read_bytes()).hexdigest() == entry["sha256"], file


def main() -> None:
    fixtures = sorted(FIXTURE_DIR.glob("*.jsonl"))
    if not fixtures:
        raise AssertionError(f"no sanitized fixtures found under {FIXTURE_DIR}")
    event_count = 0
    providers: set[str] = set()
    event_types: set[str] = set()
    for fixture in fixtures:
        fixture_events, fixture_providers, fixture_event_types = validate_fixture(fixture)
        event_count += fixture_events
        providers.update(fixture_providers)
        event_types.update(fixture_event_types)
    assert {"pro", "openai"} <= providers, providers
    assert {"token_count", "thread_settings_applied"} <= event_types, event_types
    validate_mapping()
    validate_json_asset(CCUSAGE_LOCK, {"minimum_compatible_version", "json_contract_file"})
    validate_json_asset(CCUSAGE_CONTRACT, {"tool", "version", "commands"})
    validate_schema_manifest()
    print(f"phase0 fixture privacy gate: ok ({event_count} structured events)")
    print("phase0 evidence asset gate: ok")


if __name__ == "__main__":
    main()
