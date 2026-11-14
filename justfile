set shell := ["bash", "-euo", "pipefail", "-c"]

default: check

fmt:
    cargo fmt --all

lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    python3 scripts/no_comments.py

test:
    cargo test --workspace --locked

schema:
    cargo run --quiet --locked -p kiln -- schema > schema/kiln.v1.json

drift:
    cargo run --quiet --locked -p kiln -- schema | diff - schema/kiln.v1.json

check: lint test drift
