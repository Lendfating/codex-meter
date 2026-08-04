#!/usr/bin/env python3
"""Write a deterministic manifest for a generated App Server schema bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--schema-dir", type=Path, required=True)
    parser.add_argument("--codex-version", required=True)
    args = parser.parse_args()

    files = []
    for path in sorted(args.schema_dir.rglob("*.json")):
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        files.append(
            {
                "file": path.relative_to(args.schema_dir).as_posix(),
                "sha256": digest,
            }
        )
    manifest = {
        "codex_version": args.codex_version,
        "generator": "codex app-server generate-json-schema --experimental",
        "schema_files": files,
    }
    output = args.schema_dir / "manifest.json"
    output.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps({"output": str(output), "file_count": len(files)}))


if __name__ == "__main__":
    main()
