set shell := ["zsh", "-cu"]

format:
    cargo fmt --all

lint:
    cargo clippy --all-targets --all-features -- -D warnings

test:
    cargo test --all-targets

test-integration:
    cargo test --all-targets

web-test:
    cd web && npm test

e2e:
    @echo "Phase 0 has no business pages; browser E2E starts in a later phase."

check:
    cargo fmt --all -- --check
    cargo test --all-targets
    cd web && npm test
    python3 scripts/validate_fixture.py
