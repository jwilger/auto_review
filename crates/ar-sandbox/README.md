# ar-sandbox

Sandbox abstraction retained for issue #46's workspace-isolation
rescope. Normal review/orchestrator jobs no longer execute bundled
linters and no longer wire this crate through the gateway. The original
linter-execution use case is preserved as historical context in
ADR-0002.

## Public surface

| Item | Purpose |
|------|---------|
| `Sandbox` (trait) | `run(SandboxCommand) -> SandboxOutput`. Object-safe for callers that need an execution boundary. |
| `SandboxCommand` | Argv + working dir + env + wall-clock timeout. |
| `SandboxOutput` | Status + stdout + stderr. |
| `DirectSandbox` | The "no isolation" path. Spawns directly via `tokio::process::Command`. Suitable only for tests or trusted local experiments. |
| `PodmanSandbox` | Hardened container path. Wraps commands in `podman run --network=none --read-only ...` with CPU / memory / pids / wall-clock limits. |
| `PodmanSandboxConfig` | Image + memory + cpus + pids + timeout + binary path. Not built by the gateway in the normal review runtime after issue #45. |

## Threat coverage

See [`docs/THREAT-MODEL.md`](../../docs/THREAT-MODEL.md) and
[`docs/ADR-0002-sandbox.md`](../../docs/ADR-0002-sandbox.md). The Podman
backend implements the linter-era mitigations that may inform issue #46:
- `--network=none` — no egress
- `--read-only` — no host filesystem writes outside the writable
  workspace volume
- `--memory`, `--cpus`, `--pids-limit` — bounded resource use
- `--timeout` — wall-clock kill
- No host env-var passthrough — `FORGEJO_TOKEN` and
  `LLM_API_KEY` aren't visible to the sandboxed process

[`docs/ADR-0002-sandbox.md`](../../docs/ADR-0002-sandbox.md)
documents the superseded linter-sandbox decision.

## Tests

`cargo test -p ar-sandbox` covers `DirectSandbox`'s plumbing and
`PodmanSandbox`'s argv construction. The actual container-escape
verification needs a live Podman binary — operators should run
the smoke-test corpus described in the ADR before exposing the
deploy to drive-by PRs.

## Dependencies

`tokio` for async process spawning. No platform-specific deps —
the same code path works on Linux and macOS, but the Podman
backend assumes a Linux container runtime.
