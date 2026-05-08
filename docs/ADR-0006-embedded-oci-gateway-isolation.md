# ADR-0006: Embedded OCI gateway isolation for the single binary

Status: **Accepted**
Date: 2026-05-06

## Context

[ADR-0002](./ADR-0002-sandbox.md) retired the linter-era sandbox for the normal
review runtime. Normal gateway/orchestrator jobs no longer execute bundled
linters, repo-controlled tests, repo-controlled builds, or LLM-issued shell
commands. CI owns deterministic execution before it calls the semantic review
endpoint. The remaining workspace-touching paths use hermetic Git subprocesses,
read-only/path-confined workspace access, symlink escape rejection, output caps,
webhook/CI authentication, and secret redaction.

Those application-level controls remain mandatory, but issue #115 also asked for
a downloadable single binary that preserves the practical safety operators expect
from the current container-centered deployment. A plain bare process cannot
provide the same process/filesystem/namespace boundary as Docker or Podman. The
single-binary gateway therefore needs a default isolation story that is stronger
than "start bare and warn" while still keeping the direct-download artifact easy
to run.

The Rust project that fits the remembered "y" sandbox/runtime direction is
`youki`, a Rust-written OCI runtime. `youki` is not an in-process sandbox helper;
it is an OCI runtime boundary like `runc` or `crun`. To use it from a downloaded
binary, `auto-review gateway` must act as an outer launcher that starts an inner
gateway process inside an OCI bundle/rootfs.

## Decision

The first supported single-binary gateway implementation must include an
embedded OCI isolation mode based on `youki`-style runtime behavior.

`auto-review gateway` is an outer launcher by default:

1. The host `auto-review gateway` process prepares or locates the embedded OCI
   bundle/rootfs.
2. The launcher uses embedded or linked OCI runtime capability; operators should
   not need to install a separate host `youki` executable for the default path.
3. The runtime starts an inner gateway process inside the isolated environment.
4. The inner process runs the actual gateway service.

OCI isolation is the default for `auto-review gateway` on supported Linux hosts.
Bare-process gateway mode is allowed only through an explicit opt-out flag/env
var. If OCI setup fails and the operator has not explicitly opted out, startup
must fail closed.

Bare opt-out mode must emit prominent warnings in startup logs and diagnostics.
The warning must say that only application-level controls are active and must not
claim container-equivalent isolation.

The embedded OCI bundle/rootfs is part of the direct-download artifact strategy.
The default distribution should not require a separately installed rootfs or OCI
bundle. The implementation may unpack/cache the embedded payload at runtime, but
that cache behavior must be explicit, reproducible, and safe to clean up.

The embedded rootfs must be minimal. Include only what the gateway runtime needs:

- `auto-review` for the inner process;
- `git` for hermetic clone/fetch/checkout;
- CA certificates;
- resolver, passwd, and group basics;
- required shared libraries if the binary is not fully static;
- explicit writable tmp and state mounts.

Do not include shell/coreutils/debug extras in the default embedded rootfs. If a
future debug image or support bundle is needed, decide that separately.

The intended direct-download targets are Linux `x86_64` and Linux `aarch64`,
matching [ADR-0005](./ADR-0005-single-public-executable.md). Non-Linux
single-binary gateway releases are out of scope for the first implementation
because the accepted default isolation model is Linux/OCI-specific.
[ADR-0007](./ADR-0007-x86_64-only-binary-releases.md) temporarily narrows the
published release artifacts to Linux `x86_64` until a proper Linux `aarch64`
build runner or equivalent trusted build solution is available.

Runtime posture must be visible:

- startup logs state whether OCI isolation is active, failed, or explicitly
  bypassed;
- `/info` exposes non-secret posture details;
- the grouped diagnostic command, such as `auto-review ops doctor`, reports the
  same posture;
- all posture reporting must avoid leaking secrets, sensitive env values, or
  private filesystem details beyond what operators need for support.

## Security boundaries

Embedded OCI isolation is defense around the gateway process. It does not replace
existing application-level controls, and those controls remain mandatory:

- normal review runtime must not execute repo-controlled linters, tests, builds,
  or generated code;
- Git subprocesses must stay hermetic;
- LLM workspace tools must remain read-only and path-confined;
- output and prompt inputs must stay capped;
- webhook and CI review endpoints must stay authenticated;
- secrets must stay out of logs, prompts, posture reports, and review comments.

Any future feature that executes repo-controlled code must still add a
feature-specific sandbox design and red-team tests. The presence of embedded OCI
launcher machinery is not permission to reintroduce a generic `Sandbox::run(...)`
API without a concrete caller, threat-model update, and tests.

## Consequences

- The direct-download gateway path becomes substantially more complex than a
  normal Rust binary. It must manage OCI runtime behavior, rootfs/bundle content,
  writable mounts, rootless/cgroup constraints, port binding, state directories,
  cleanup, and clear error messages.
- Default startup is safer and more honest than silently running bare, but some
  Linux hosts will fail to start until their kernel/cgroup/rootless environment
  can support the embedded OCI path or the operator explicitly opts out.
- The binary artifact may be larger because it carries or can reconstruct the
  minimal rootfs and runtime capability.
- The Docker image remains recommended for production because it gives operators
  a familiar deployment boundary and operational model. The embedded OCI path
  exists to make direct binary usage safe-by-default, not to remove the image.
- Threat-model, operations, and release docs must explain the difference between
  app-level controls, embedded OCI isolation, explicit bare opt-out, and external
  deployment isolation.

## Required implementation slices

- [#117](https://git.johnwilger.com/jwilger/auto_review/issues/117) — implement
  the embedded `youki` OCI gateway launcher.
- [#118](https://git.johnwilger.com/jwilger/auto_review/issues/118) — build the
  embedded minimal OCI rootfs for `auto-review gateway`.
- [#122](https://git.johnwilger.com/jwilger/auto_review/issues/122) — report
  runtime isolation posture in gateway and CLI diagnostics.
- [#120](https://git.johnwilger.com/jwilger/auto_review/issues/120) — update
  docs, the threat model, and red-team/contract tests for the rollout.

## Required tests before shipping

- Unit tests for launcher mode selection: default OCI, OCI failure, explicit bare
  opt-out, and unsupported host behavior.
- Integration or harness tests for successful OCI launch and fail-closed startup
  where feasible under the Nix/CI environment.
- Bundle/rootfs contract tests proving only minimal runtime contents are present.
- Tests proving the rootfs is read-only except for explicit writable tmp/state
  mounts.
- Smoke tests proving the inner gateway can access `git`, CA certificates, DNS,
  configured state paths, and required network endpoints.
- `/info`, startup-log, and CLI diagnostic contract tests for posture reporting.
- Secret-redaction regression tests for launcher errors and posture reports.
- Threat-model red-team tests for any future repo-controlled process execution
  before such execution is allowed.

## Alternatives considered

- **Bare process with warning by default.** Simpler, but it makes the safe path an
  operator discipline problem and does not satisfy the desired safe "just run it"
  experience.
- **Require an externally installed `youki`.** Smaller binary and simpler build,
  but it reintroduces setup friction similar to requiring Docker/Podman before a
  user can try the gateway.
- **Publish a companion rootfs/bundle artifact.** Keeps the binary smaller, but
  makes the direct-download story multi-file and increases version-skew risk.
- **Embed a debug-friendly rootfs with shell/coreutils.** Easier support, but a
  larger and less constrained default environment. A separate debug artifact can
  be proposed later if needed.
- **Rely only on Landlock/seccomp/rlimits.** Useful future defense-in-depth, but
  not equivalent to an OCI boundary and likely to produce difficult compatibility
  failures in a networked async service that uses Git, TLS, DNS, and SQLite.
- **Container image only.** Operationally strong, but it does not address issue
  #115's direct-download goal.
