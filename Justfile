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

# Needs network: fetches the RustSec advisory database.
advisories:
	cargo deny check advisories

build:
	cargo build --workspace

shellcheck:
	shellcheck scripts/release .claude/hooks/*.sh .claude/hooks/tests/*.sh

# Exercises the factory's PreToolUse hooks against a fake gh.
hooks-test:
	for t in .claude/hooks/tests/*.test.sh; do bash "$t"; done

actionlint:
	actionlint

# Builds the packages and evaluates the Nix contract checks; slow on a cold store.
flake-check:
	nix flake check --print-build-logs

serve:
	pkg="$(nix build --no-link --print-out-paths .#ar-cli)"; AR_GATEWAY_BIND=0.0.0.0:8090 "$pkg/bin/auto-review" gateway

watch:
	AR_GATEWAY_BIND=0.0.0.0:8090 bacon --job gateway-dev

ci: fmt clippy test deny build shellcheck hooks-test actionlint
