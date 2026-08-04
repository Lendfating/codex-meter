# Sanitized JSONL fixtures

The `codex-session-*.jsonl` files are generated from real local Codex JSONL
sessions by `scripts/generate_sanitized_fixture.py`. The generator emits only
the allowlisted structured fields needed for later collectors and replaces
session identifiers with deterministic fixture identifiers.

The source Codex directory is never copied into the repository. Regenerate it
locally with:

```bash
python3 scripts/generate_sanitized_fixture.py \
  --codex-home /Users/Lendfating/.codex \
  --output fixtures/jsonl/codex-session-sanitized.jsonl
python3 scripts/generate_sanitized_fixture.py \
  --codex-home /Users/Lendfating/.codex \
  --provider openai \
  --output fixtures/jsonl/codex-session-plus-quota-sanitized.jsonl
```

Run the privacy gate afterwards:

```bash
python3 scripts/validate_fixture.py
```
