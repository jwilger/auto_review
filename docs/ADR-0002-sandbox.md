# ADR-0002: Linter Sandbox

Status: **Superseded for normal review runtime** (issue #46 rescope complete;
`ar-sandbox` is retained only for future execution features and tests)
Date: 2026-04-30 (revised 2026-05-03 to note runtime linter execution removal
and issue #46 sandbox rescope; 2026-04-30 to clarify the OCI-runtime position:
podman OR docker was the historical production sandbox; youki was explicitly
future work)

> As of issues #45 and #46, normal review/orchestrator jobs no longer execute
> bundled linters or route linter findings into semantic review. Deterministic
> linters/tests/builds are owned by CI before it triggers `auto_review`.
> Historical rationale below remains useful background for any future sandboxed
> execution feature, but `AR_SANDBOX_IMAGE` and `deploy/Dockerfile.sandbox` are
> retired for the normal gateway runtime.

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
- Gateway startup no longer selects `AR_SANDBOX_IMAGE`, and
  `deploy/Dockerfile.sandbox` is retired.
- Deterministic linters, tests, and builds belong in project CI before the
  CI-triggered `auto_review` request.
- `/info` reports `sandbox: "not-used"` for the normal review runtime even if a
  legacy `AR_SANDBOX_IMAGE` value is present.

The old sandbox design below is retained as historical threat-model context for
future features that would execute untrusted workspace contents. It is no
longer normative for normal semantic review.

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
| Legacy `ar-tools` linter runners | Can execute deterministic tools through `ar_sandbox::Sandbox` when called directly by tests or experiments | Not wired into gateway/orchestrator runtime. Any supported future caller must introduce feature-specific fail-closed config and threat-model tests. |

Consequently, there is no global sandbox image requirement for gateway startup.
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
two implementations:

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
`AR_SANDBOX_IMAGE`. That gateway wiring was removed with issue #45 and issue
#46 confirms no remaining default path needs it. Any future runtime execution
path must be re-evaluated explicitly rather than inheriting this linter-era
decision.

## Historical trade-offs (superseded)

- **Operator overhead**: the old design required a podman daemon
  reachable from the gateway and a pre-pulled sandbox image. That was a real
  onboarding step. The issue #45 design removes this linter-driven startup
  requirement from normal review runtime.

- **No youki yet — and that's fine for v1.** youki is a Rust-
  native OCI runtime that would let us skip the shell-out
  altogether (linker against `libcontainer` instead of forking
  `podman` / `docker`). The win is purely operational: lower
  per-spawn overhead, no external binary on PATH, pure-Rust build
  artefact. The threat-model coverage is *identical* — the
  hardening flags (`--network=none`, `--read-only`,
  `--cap-drop=ALL`, etc.) get translated into the same kernel-
  level controls regardless of which OCI runtime applies them.
  youki integration is therefore explicitly **future-work, not a
  gate on shipping**. The trait surface (`Sandbox::run(...)
  -> SandboxOutput`) is shaped so a `YoukiSandbox` impl drops
  in alongside `PodmanSandbox` without touching callers. Until
  then podman/docker is the production answer: same threat
  model, dramatically less integration cost.

- **No precision/recall benchmark for sandbox escapes** — *until
  the v0.1 ship, that is.* The escape harness in
  `crates/ar-sandbox/tests/escape.rs` covers seven attack classes
  against the production flag set: network egress denial, fork-
  bomb containment, wall-clock termination, repo-mount read-only
  enforcement, unprivileged-uid execution, no-new-privileges,
  and dropped capabilities. Run with `cargo test -p ar-sandbox
  --test escape -- --ignored`. Tests skip cleanly when no OCI
  runtime is on PATH; in CI they run against whichever runtime
  the runner provides.

- **DirectSandbox ships in the workspace**: it's tempting to make
  it test-only. We chose to ship it because (a) tests need it,
  (b) local-dev needs it, and (c) the warning banner makes the
  production gap loud. Removing it would push test code into a
  dev-only crate without making operators safer.

- **Wall-clock timeout enforced host-side**: tokio's `timeout`
  wrapper kills the parent process; podman's `--rm` cleans up the
  container. We deliberately do **not** use podman's
  `--stop-timeout` flag, which controls the SIGTERM grace period
  rather than a kill-after-N-seconds.


## Consequences

- Normal review/orchestrator jobs no longer own or receive a sandbox handle.
- `ar-review` no longer exposes linter routing or linter-only review APIs.
- `ar_sandbox::PodmanSandbox` is exercised today by argv-shape unit tests and
  retained live escape tests, but not by normal gateway/orchestrator jobs. That's
  a deliberate boundary: the crate remains available for a future feature that
  explicitly needs untrusted process execution, not as a startup dependency for
  semantic review.

## Alternatives considered

- **gVisor + runsc**: stronger isolation than vanilla namespaces,
  but adds another runtime dependency on the gateway host. Podman
  configured to use runsc would slot in via `AR_SANDBOX_PODMAN_BIN`
  or by adding a `--runtime=runsc` flag in `PodmanSandboxConfig`.
  Deferred.
- **Cloud Run-style sidecar containers**: would require the gateway
  to be deployed on a runtime that can spin up sibling containers
  on demand. Doesn't fit the "single-tenant `docker compose up`"
  deployment story.
- **WASM sandboxing for linter logic**: most linters aren't compiled
  to WASM and wouldn't accept arbitrary repo configs anyway.
  Out of scope.
