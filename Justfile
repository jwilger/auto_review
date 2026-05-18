set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

default: ci

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo nextest run --workspace --no-tests=pass

opencode-test:
	npm --prefix .opencode ci --ignore-scripts
	node --test .opencode/plugins/*.test.ts

deny:
	cargo deny check licenses bans sources

build:
	cargo build --workspace

serve:
	pkg="$(nix build --no-link --print-out-paths .#ar-cli)"; AR_GATEWAY_BIND=0.0.0.0:8090 "$pkg/bin/auto-review" gateway

watch:
	AR_GATEWAY_BIND=0.0.0.0:8090 bacon --job gateway-dev

ci: fmt clippy test opencode-test deny build
