# ar-sandbox

Linter-execution isolation. The cloned PR working tree is
attacker-controlled by construction; running linters directly on
the host opens the Kudelski-class RCE risk (CodeRabbit's May 2024
rubocop incident). This crate gates every linter spawn through a
trait so production deploys can wrap them in a hardened container.

## Public surface

| Item | Purpose |
|------|---------|
| `Sandbox` (trait) | `run(SandboxCommand) -> SandboxOutput`. Object-safe so the orchestrator threads `Arc<dyn Sandbox>` through. |
| `SandboxCommand` | Argv + working dir + env + wall-clock timeout. |
| `SandboxOutput` | Status + stdout + stderr. |
| `DirectSandbox` | The "no isolation" path. Spawns directly via `tokio::process::Command`. **Never use this in production** — gateway startup now fails closed unless the OCI sandbox image is configured. |
| `PodmanSandbox` | The hardened production path. Wraps every command in `podman run --network=none --read-only ...` with CPU / memory / pids / wall-clock limits. |
| `PodmanSandboxConfig` | Image + memory + cpus + pids + timeout + binary path. Built from env vars in the gateway's `main.rs`. |

## Threat coverage

See [`docs/THREAT-MODEL.md`](../../docs/THREAT-MODEL.md) §T1. The
Podman backend implements every mitigation listed there:
- `--network=none` — no egress
- `--read-only` — no host filesystem writes outside the writable
  workspace volume
- `--memory`, `--cpus`, `--pids-limit` — bounded resource use
- `--timeout` — wall-clock kill
- No host env-var passthrough — `FORGEJO_TOKEN` and
  `LLM_API_KEY` aren't visible to the sandboxed process

[`docs/ADR-0002-sandbox.md`](../../docs/ADR-0002-sandbox.md)
documents the design choice.

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
