# ADR-0002: Linter Sandbox

Status: **Retired for normal review runtime** (issue #46 rescope complete)
Date: 2026-04-30 (revised 2026-05-03 to note runtime linter execution removal
and issue #46 sandbox rescope; 2026-04-30 to clarify the OCI-runtime position:
podman OR docker was the historical production sandbox; youki was explicitly
future work)

> As of issues #45 and #46, normal review/orchestrator jobs no longer execute
> bundled linters or route linter findings into semantic review. Deterministic
> linters/tests/builds are owned by CI before it triggers `auto_review`.
> Historical rationale below remains useful background, but the linter sandbox
> implementation, `AR_SANDBOX_IMAGE`, and `deploy/Dockerfile.sandbox` are retired
> for the normal gateway runtime.

## Historical context (superseded)

Before issue #45, `auto_review` ran ~44 linter binaries against the working tree of an
incoming pull request. The PR's contents are attacker-controlled by
construction — anyone can open a PR, including a hostile contributor.
Several linters in the bundle (rubocop, golangci-lint, eslint via
plugins) load configuration *from the repo itself* and execute it as
trusted code. Others (semgrep, trivy) shell out to subprocess managers
that historically have had path-injection issues.

The Kudelski Security writeup against CodeRabbit (May 2024) is the
reference incident: an unjailed `rubocop` invocation, fed a malicious
`.rubocop.yml` from a PR, escaped to RCE on the reviewer host. Once
the attacker had RCE, they harvested the GitHub bot's PAT and gained
write access to roughly one million customer repos.

Without isolation, that design would have exposed
operators to the same class of attack the moment the bot is reachable
from any untrusted PR source.

## Decision

Issues #45 and #46 supersede the linter-driven decision for the normal gateway
and orchestrator runtime:

- Normal review jobs do **not** execute bundled linters.
- Normal review prompts do **not** receive linter findings.
- Gateway startup no longer selects `AR_SANDBOX_IMAGE`, and the old linter
  sandbox implementation is retired.
- Deterministic linters, tests, and builds belong in project CI before the
  CI-triggered `auto_review` request.
The old sandbox design below is retained only as historical threat-model context.
It is no longer normative for normal semantic review.

## Issue #46 workspace-path rescope

The remaining runtime paths that touch PR workspace contents are deliberately
split by capability:

| Path | Workspace access | Isolation decision |
|---|---|---|
| Gateway PR webhooks | Forgejo metadata and diff references only | No sandbox; HMAC and CI endpoint auth are the relevant controls. |
| CI-triggered review endpoint | Forgejo metadata; verifies PR head SHA before dispatch | No sandbox; it does not read or execute the checkout. |
| Orchestrator clone/context/review pipeline | Shallow clone plus read-only diff/context extraction | Git subprocesses run with hermetic config/env (no system/global config, env-injected config, templates/hooks, worktree/index/object overrides, or SSH command overrides). Subsequent workspace access uses path confinement and output caps; no process/container sandbox because no repo-controlled linter/test/build subprocess executes. |
| Agentic verifier `read_file` / `search` | Read-only LLM-issued file reads and regex search under the clone root | Pure path confinement via canonicalization, symlink escape rejection, recursion/result caps; no shell tool is exposed. |
| Chat `re-review` | Dispatches a forced review for the current Forgejo head SHA | Same as normal review; no direct workspace execution in the chat handler. |
| Chat free-form / `autofix` / `docstring` / `tests` | Fetches Forgejo diffs and asks the cheap-tier LLM for text, suggestions, or scaffolds | Forgejo-side diff only; generated text is posted for humans to apply, not executed by the bot. |
| Historical linter runners | Removed from the active workspace with the sandbox abstraction | Deterministic tool execution belongs in CI. Any future bot-side execution feature must introduce a new, feature-specific design, fail-closed config, and threat-model tests. |

Consequently, there is no global sandbox image requirement for gateway startup
and no sandbox field in `/info`.
If a future feature adds repo-controlled command execution (for example, running
tests for an `@auto_review tests` command instead of only proposing scaffolds),
that feature must be gated explicitly with its own required sandbox/runtime
configuration and must fail closed when the isolation backend is unavailable.

Git itself is the one remaining host subprocess in normal workspace
preparation. Because Git can be influenced by ambient host config, filters,
templates, hooks, credential prompts, and transport environment,
`prepare_workspace` constructs all Git invocations through a hermetic command
wrapper. The wrapper disables system and global Git config, clears env-injected
`GIT_CONFIG_*`, isolates `HOME` and `XDG_CONFIG_HOME`, disables terminal
prompts, and removes ambient Git variables that can redirect the repo, index,
object store, templates/hooks, executable path, askpass helpers, or SSH
transport. The red-team tests in `crates/ar-review/src/workspace.rs` pin this
boundary with host global aliases, env-injected aliases, explicit env-removal
assertions, and askpass/prompt assertions.

Historically, every linter spawn went through an `ar_sandbox::Sandbox` trait with
two implementations. Those crates are now removed from the active workspace;
this section records the retired design so future work does not accidentally
inherit it:

- `DirectSandbox` — a thin pass-through that spawns the binary with
  `tokio::process::Command`. **No isolation.** Suitable only for tests
  and local-dev clusters where the operator already trusts every
  contributor.
- `PodmanSandbox` — wraps the spawn in
  `<runtime> run --rm --network=none --read-only
  --tmpfs /tmp:size=64m --security-opt=no-new-privileges
  --cap-drop=ALL --memory=… --cpus=… --pids-limit=…
  --user 65534:65534 -v <repo>:/work:ro -w /work …` plus a
  tokio-side wall-clock timeout. The repo is mounted **read-only**;
  the rootfs is read-only; egress is blocked at the network namespace;
  caps are stripped; the process runs as `nobody`. Despite the
  type name, `<runtime>` is either `podman` or `docker` — both
  accept the flag set unchanged. Operators pick via
  `AR_SANDBOX_RUNTIME` (preferred) or `AR_SANDBOX_PODMAN_BIN`
  (legacy alias); when neither is set the gateway auto-detects at
  startup, preferring podman (rootless, no daemon) and falling
  back to docker.

Historically, the gateway selected an implementation at startup based on
`AR_SANDBOX_IMAGE`. That gateway wiring was removed with issue #45, and issue
#46 removed the leftover linter/sandbox crates from the active workspace. Any
future runtime execution path must be re-evaluated explicitly rather than
inheriting this linter-era decision.

## Historical trade-offs (superseded)

- **Operator overhead**: the old design required a podman daemon
  reachable from the gateway and a pre-pulled sandbox image. That was a real
  onboarding step. The issue #45 design removes this linter-driven startup
  requirement from normal review runtime.

- **No youki / reusable sandbox abstraction.** A future bot-side execution
  feature may still choose an OCI runtime, gVisor, WASM, or another isolation
  boundary, but it must do so with a fresh design and tests for that exact
  feature. Keeping a generic `Sandbox::run(...)` surface without a current
  caller encouraged speculative reuse, so issue #46 removed it.

- **No retained escape harness.** The linter-era escape tests validated a
  retired flag set for a retired runtime path. CI runner isolation is now an
  operator concern, and any future bot-side execution path must add targeted
  red-team tests with its implementation.

- **Wall-clock timeout enforced host-side**: tokio's `timeout`
  wrapper kills the parent process; podman's `--rm` cleans up the
  container. We deliberately do **not** use podman's
  `--stop-timeout` flag, which controls the SIGTERM grace period
  rather than a kill-after-N-seconds.


## Consequences

- Normal review/orchestrator jobs no longer own or receive a sandbox handle.
- `ar-review` no longer exposes linter routing or linter-only review APIs.
- The linter runner and sandbox crates are removed from the active workspace.
  Future untrusted process execution must reintroduce only the feature-specific
  code, configuration, docs, and red-team tests it needs.

## Alternatives considered

- **gVisor + runsc**: stronger isolation than vanilla namespaces,
  but adds another runtime dependency on the gateway host. Deferred with the
  rest of the linter-era sandbox implementation.
- **Cloud Run-style sidecar containers**: would require the gateway
  to be deployed on a runtime that can spin up sibling containers
  on demand. Doesn't fit the "single-tenant `docker compose up`"
  deployment story.
- **WASM sandboxing for linter logic**: most linters aren't compiled
  to WASM and wouldn't accept arbitrary repo configs anyway.
  Out of scope.
