#!/usr/bin/env python3
"""Generate a small structured-only fixture from one real Codex JSONL file."""

from __future__ import annotations

import argparse
import glob
import hashlib
import json
import re
from pathlib import Path
from typing import Any, Iterable


TOKEN_FIELDS = (
    "input_tokens",
    "cached_input_tokens",
    "cache_write_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
)
SAFE_PLANS = {"plus", "pro"}
SAFE_SERVICE_TIERS = {"default", "standard", "fast", "priority"}
NUMBER_STRING = re.compile(r"^-?\d+(?:\.\d+)?$")
SAFE_MODEL = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex-home", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--provider", choices=("pro", "openai"), default="pro")
    return parser.parse_args()


def iter_jsonl_paths(codex_home: Path) -> Iterable[Path]:
    patterns = (
        codex_home / "sessions" / "**" / "*.jsonl",
        codex_home / "archived_sessions" / "**" / "*.jsonl",
    )
    paths: list[Path] = []
    for pattern in patterns:
        paths.extend(Path(path) for path in glob.glob(str(pattern), recursive=True))
    yield from sorted(set(paths))


def stable_fixture_id(value: Any, prefix: str) -> str:
    raw = str(value or "missing").encode("utf-8")
    digest = hashlib.sha256(raw).hexdigest()[:12]
    return f"{prefix}-{digest}"


def safe_model(value: Any, fallback: str) -> str:
    if isinstance(value, str) and SAFE_MODEL.fullmatch(value):
        return value
    return fallback


def safe_number(value: Any) -> int | float | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return value
    return None


def safe_numeric_string(value: Any) -> str | int | float | None:
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return value
    if isinstance(value, str) and NUMBER_STRING.fullmatch(value):
        return value
    return None


def copy_usage(value: Any) -> dict[str, int | float] | None:
    if not isinstance(value, dict):
        return None
    result: dict[str, int | float] = {}
    for field in TOKEN_FIELDS:
        number = safe_number(value.get(field))
        if number is not None:
            result[field] = number
    return result or None


def copy_limit_window(value: Any) -> dict[str, int | float] | None:
    if not isinstance(value, dict):
        return None
    result: dict[str, int | float] = {}
    for field in ("used_percent", "window_minutes", "resets_at"):
        number = safe_number(value.get(field))
        if number is not None:
            result[field] = number
    return result or None


def copy_rate_limits(value: Any, source_limit_id: Any) -> dict[str, Any] | None:
    if value is None:
        return None
    if not isinstance(value, dict):
        return None
    plan_type = value.get("plan_type")
    result: dict[str, Any] = {
        "limit_id": stable_fixture_id(source_limit_id, "limit"),
        "limit_name": "fixture-limit",
        "plan_type": plan_type if plan_type in SAFE_PLANS else None,
        "primary": copy_limit_window(value.get("primary")),
        "secondary": copy_limit_window(value.get("secondary")),
    }
    credits = value.get("credits")
    if isinstance(credits, dict):
        result["credits"] = {
            "has_credits": credits.get("has_credits")
            if isinstance(credits.get("has_credits"), bool)
            else None,
            "unlimited": credits.get("unlimited")
            if isinstance(credits.get("unlimited"), bool)
            else None,
            "balance": safe_numeric_string(credits.get("balance")),
        }
    else:
        result["credits"] = None
    return result


def sanitize_session_meta(obj: dict[str, Any]) -> dict[str, Any]:
    payload = obj.get("payload")
    if not isinstance(payload, dict):
        raise ValueError("session_meta payload must be an object")
    session_id = stable_fixture_id(payload.get("session_id") or payload.get("id"), "session")
    provider = payload.get("model_provider")
    normalized_provider = provider if provider in {"openai", "pro"} else "unknown"
    return {
        "timestamp": obj.get("timestamp"),
        "type": "session_meta",
        "payload": {
            "cli_version": safe_model(payload.get("cli_version"), "fixture-codex"),
            "id": session_id,
            "model_provider": normalized_provider,
            "session_id": session_id,
            "thread_source": safe_model(payload.get("thread_source"), "fixture"),
            "timestamp": payload.get("timestamp"),
        },
    }


def sanitize_token_count(obj: dict[str, Any]) -> dict[str, Any]:
    payload = obj.get("payload")
    if not isinstance(payload, dict):
        raise ValueError("token_count payload must be an object")
    info = payload.get("info")
    safe_info: dict[str, Any] = {}
    if isinstance(info, dict):
        for field in ("last_token_usage", "total_token_usage"):
            usage = copy_usage(info.get(field))
            if usage is not None:
                safe_info[field] = usage
        context_window = safe_number(info.get("model_context_window"))
        if context_window is not None:
            safe_info["model_context_window"] = context_window
    return {
        "timestamp": obj.get("timestamp"),
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": safe_info,
            "rate_limits": copy_rate_limits(
                payload.get("rate_limits"),
                (payload.get("rate_limits") or {}).get("limit_id")
                if isinstance(payload.get("rate_limits"), dict)
                else None,
            ),
        },
    }


def sanitize_thread_settings(obj: dict[str, Any]) -> dict[str, Any]:
    payload = obj.get("payload")
    if not isinstance(payload, dict):
        raise ValueError("thread_settings_applied payload must be an object")
    settings = payload.get("thread_settings")
    settings = settings if isinstance(settings, dict) else {}
    tier = settings.get("service_tier")
    return {
        "timestamp": obj.get("timestamp"),
        "type": "event_msg",
        "payload": {
            "type": "thread_settings_applied",
            "thread_settings": {
                "model": safe_model(settings.get("model"), "fixture-model"),
                "model_provider_id": safe_model(
                    settings.get("model_provider_id"), "fixture-provider"
                ),
                "service_tier": tier if tier in SAFE_SERVICE_TIERS else "unknown",
            },
        },
    }


def find_source_file(codex_home: Path, requested_provider: str) -> tuple[Path, str]:
    candidates: list[tuple[int, Path, str]] = []
    for path in iter_jsonl_paths(codex_home):
        session_id: str | None = None
        provider: Any = None
        has_token_count = False
        has_primary_window = False
        has_credit_fields = False
        plan_types: set[Any] = set()
        with path.open(encoding="utf-8") as handle:
            for line in handle:
                obj = json.loads(line)
                if obj.get("type") == "session_meta" and isinstance(obj.get("payload"), dict):
                    payload = obj["payload"]
                    provider = payload.get("model_provider")
                    session_id = payload.get("session_id") or payload.get("id")
                event_payload = obj.get("payload")
                if (
                    isinstance(event_payload, dict)
                    and event_payload.get("type") == "token_count"
                ):
                    has_token_count = True
                    rate_limits = event_payload.get("rate_limits")
                    if isinstance(rate_limits, dict):
                        plan_types.add(rate_limits.get("plan_type"))
                        primary = rate_limits.get("primary")
                        if isinstance(primary, dict) and primary.get("used_percent") is not None:
                            has_primary_window = True
                        credits = rate_limits.get("credits")
                        if isinstance(credits, dict) and credits.get("balance") is not None:
                            has_credit_fields = True
        expected_plans = {None} if requested_provider == "pro" else {"plus"}
        if (
            provider == requested_provider
            and session_id
            and has_token_count
            and plan_types <= expected_plans
        ):
            score = int(has_primary_window) * 2 + int(has_credit_fields)
            candidates.append((score, path, str(session_id)))
    if candidates:
        _, path, session_id = max(candidates, key=lambda item: (item[0], item[1].name))
        return path, session_id
    raise FileNotFoundError("no real pro-provider session with token_count was found")


def generate(source: Path, source_session_id: str) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    token_count = 0
    with source.open(encoding="utf-8") as handle:
        for line in handle:
            obj = json.loads(line)
            if obj.get("type") == "session_meta":
                payload = obj.get("payload")
                if isinstance(payload, dict) and (
                    payload.get("session_id") or payload.get("id")
                ) == source_session_id:
                    events.append(sanitize_session_meta(obj))
                continue
            if obj.get("type") != "event_msg" or not isinstance(obj.get("payload"), dict):
                continue
            payload = obj["payload"]
            if payload.get("type") == "token_count":
                events.append(sanitize_token_count(obj))
                token_count += 1
            elif payload.get("type") == "thread_settings_applied":
                events.append(sanitize_thread_settings(obj))
    if not any(event.get("type") == "session_meta" for event in events):
        raise ValueError("source session_meta was not found")
    if token_count == 0:
        raise ValueError("source session has no token_count events")
    return events


def main() -> None:
    args = parse_args()
    source, source_session_id = find_source_file(args.codex_home, args.provider)
    events = generate(source, source_session_id)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8", newline="\n") as handle:
        for event in events:
            handle.write(json.dumps(event, ensure_ascii=False, separators=(",", ":")) + "\n")
    print(
        json.dumps(
            {
                "source_file_name": source.name,
                "event_count": len(events),
                "output": str(args.output),
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    main()
