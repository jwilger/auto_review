set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

default: ci

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --locked --workspace --all-targets -- -D warnings

# git exports GIT_DIR and friends to its hooks, so under the pre-push hook a
# test that spawns git would act on this repository (one fixture's `git init`
# set `core.bare = true` here). Clearing them keeps every test hermetic,
# however it spawns git.
test:
	env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_COMMON_DIR \
		-u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES -u GIT_PREFIX \
		cargo nextest run --locked --workspace --no-tests=pass

deny:
	cargo deny --locked check licenses bans sources

# Needs network: fetches the RustSec advisory database.
advisories:
	cargo deny --locked check advisories

build:
	cargo build --locked --workspace

shellcheck:
	shellcheck scripts/release

actionlint:
	actionlint

# Checks the shipped Prometheus rules and runs their promtool unit tests, so
# an alert expression that can never match fails here instead of staying
# silent in production.
promtool:
	promtool check rules deploy/prometheus/auto_review.rules.yaml
	promtool test rules deploy/prometheus/auto_review.rules.test.yaml

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

ci: fmt clippy test deny build shellcheck actionlint promtool conflict-markers
