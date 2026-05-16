set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

default: ci

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo nextest run --workspace --no-tests=pass

deny:
	cargo deny check licenses bans sources

build:
	cargo build --workspace

ci: fmt clippy test deny build
