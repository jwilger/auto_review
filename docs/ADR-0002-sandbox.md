# ADR-0002: Linter Sandbox

Status: **Accepted**
Date: 2026-04-30 (revised 2026-04-30 to clarify the OCI-runtime
position: podman OR docker is the production sandbox; youki is
explicitly future work)

## Context

`auto_review` runs ~44 linter binaries against the working tree of an
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

Without isolation, `auto_review` shipping in this state would expose
operators to the same class of attack the moment the bot is reachable
from any untrusted PR source.

## Decision

Every linter spawn goes through an `ar_sandbox::Sandbox` trait with
two production-bound implementations:

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

The gateway picks an implementation at startup based on
`AR_SANDBOX_IMAGE`. When set, every linter goes through the OCI
runtime. When unset, the gateway logs a `WARN: sandbox: direct
(NO ISOLATION)` banner so the production-deploy gap is loud and
discoverable in logs.

**This is the production sandbox.** No further isolation layer
is required to ship; the threat model the Kudelski incident
defines is fully covered by the flag set above. youki is
documented below as future-work for performance ergonomics, not
as a security gate that has to land before v1.

## Trade-offs

- **Operator overhead**: production deploys need a podman daemon
  reachable from the gateway and a pre-pulled sandbox image
  (`deploy/Dockerfile.sandbox`). This is a real onboarding step.
  We accept it: the alternative (bundling everything in one image
  and hoping for the best) is what got CodeRabbit owned.

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

- The `ar-tools` crate gained a dependency on `ar-sandbox`. Every
  runner takes `&dyn Sandbox` instead of spawning `Command` directly.
- The orchestrator (`SpawningDispatcher`) owns one
  `Arc<dyn Sandbox>`, defaults to `DirectSandbox::new()`, and exposes
  `with_sandbox(...)` so the gateway can inject a `PodmanSandbox`.
- The `lint_workspace_via(sandbox, …)` API is the canonical entry
  point. `lint_workspace` and `lint_workspace_with` are kept as
  thin wrappers that build a fresh `DirectSandbox` per call;
  appropriate for tests and for the CLI's `review-once` debug path.
- `ar_sandbox::PodmanSandbox` is exercised today only by argv-shape
  unit tests — no live podman integration test runs in the workspace
  CI. That's a deliberate boundary; running real podman in CI is
  brittle and the argv shape is the surface that determines whether
  isolation is correct.

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
