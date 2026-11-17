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
	for t in .claude/hooks/tests/*.test.sh; do bash "$t" || exit 1; done

actionlint:
	actionlint

# Test code under crates/ must clear GIT_DIR, GIT_WORK_TREE and GIT_INDEX_FILE
# before spawning git, so fixtures run inside a git hook never touch this repo.
hermetic-git:
	python3 scripts/check-hermetic-git

# Fails on unresolved merge conflict markers in tracked files.
conflict-markers:
	status=0; git grep -nE '^(<{7}|>{7}) ' || status=$?; \
	if [ "$status" -eq 0 ]; then echo "conflict markers found in tracked files" >&2; exit 1; fi; \
	[ "$status" -eq 1 ]

# Builds the packages and evaluates the Nix contract checks; slow on a cold store.
flake-check:
	nix flake check --print-build-logs

serve:
	pkg="$(nix build --no-link --print-out-paths .#ar-cli)"; AR_GATEWAY_BIND=0.0.0.0:8090 "$pkg/bin/auto-review" gateway

watch:
	AR_GATEWAY_BIND=0.0.0.0:8090 bacon --job gateway-dev

ci: fmt clippy test deny build shellcheck hooks-test actionlint hermetic-git conflict-markers
