# Codex App Server schema snapshot

The snapshot is generated from the installed Codex CLI version recorded in
the snapshot manifest. It is an evidence artifact for the protocol version,
not an implementation of the App Server collector.

Regenerate with:

```bash
codex app-server generate-json-schema \
  --experimental \
  --out fixtures/app-server/codex-0.146.0-alpha.3.1
```
