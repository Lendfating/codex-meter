#!/usr/bin/env python3
"""Verify the recorded ccusage JSON contract against a local binary."""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CONTRACT = ROOT / "fixtures/ccusage/contract.v20.0.19.json"


def run_json(binary: Path, args: list[str]) -> dict[str, Any]:
    completed = subprocess.run(
        [str(binary), *args],
        check=True,
        capture_output=True,
        text=True,
    )
    value = json.loads(completed.stdout)
    if not isinstance(value, dict):
        raise AssertionError(f"expected JSON object for {' '.join(args)}")
    return value


def assert_row_shape(row: dict[str, Any], contract: dict[str, Any]) -> None:
    assert set(row) == set(contract["keys"]), sorted(row)
    models = row.get("models")
    assert isinstance(models, dict)
    if models:
        first_model = next(iter(models.values()))
        assert isinstance(first_model, dict)
        assert set(first_model) == set(contract["model_breakdown"]["keys"])


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--contract", type=Path, default=DEFAULT_CONTRACT)
    args = parser.parse_args()
    contract = json.loads(args.contract.read_text(encoding="utf-8"))
    version_output = subprocess.run(
        [str(args.binary), "--version"], check=True, capture_output=True, text=True
    ).stdout.strip()
    assert version_output == contract["version_output"], version_output

    for name in ("daily", "session"):
        command = contract["commands"][name]
        report = run_json(args.binary, command["argv"][1:])
        assert set(report) == set(command["top_level"]["keys"]), sorted(report)
        rows = report[command["rows"]["field"]]
        assert isinstance(rows, list) and rows
        assert_row_shape(rows[0], command["rows"])
        totals = report["totals"]
        assert isinstance(totals, dict)
        assert set(totals) == set(command["totals"]["keys"])
    print(f"ccusage contract gate: ok ({contract['version']})")


if __name__ == "__main__":
    main()
