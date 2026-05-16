# ADR-0014: Embedded OCI Gateway Isolation for Direct Binary

## Status

Partially superseded

## Date

2026-05-06

## Provenance

Reconstructed from former `docs/ADR-0006-embedded-oci-gateway-isolation.md`,
created in commit `0754c0e` on 2026-05-06. Implementation follow-up landed in
`9f2b82b`, and runtime posture reporting landed in `3694fc8`.

## Context

The direct binary gateway needs a default runtime posture that preserves the
isolation expectations of the service without requiring operators to assemble a
separate container runtime deployment by hand. The gateway receives Forgejo
webhooks, verifies signed payloads, and dispatches review work, so accidental
bare-process execution should not be the default for production-like use.

## Decision

The direct binary gateway defaults to embedded or linked OCI isolation using
youki-style runtime behavior.

The shipped binary acts as an outer launcher. It prepares the embedded OCI
execution environment, then starts an inner gateway process inside that isolated
environment. Operators get the direct-binary install shape while the gateway
still runs with a container-like boundary by default.

Bare-process execution is allowed only through an explicit opt-out. If isolation
cannot be prepared or entered, startup fails closed rather than silently falling
back to an unisolated process.

The embedded runtime environment uses a minimal embedded rootfs containing only
what the inner gateway needs to start and serve its role.

The intended first direct binary targets are Linux `x86_64` and Linux `aarch64`,
matching the direct binary release target assumptions in ADR-0013. Non-Linux
single-binary gateway releases are out of scope for the first implementation
because the accepted default isolation model is Linux/OCI-specific.

The runtime posture must be visible to operators. Startup logs, `/info`, and
doctor diagnostics report whether the gateway is running under embedded OCI
isolation or under the explicit bare-process opt-out.

## Consequences

- Operators can deploy the direct binary without losing the default isolation
  posture expected by the project.
- Startup failure modes are safer: missing or broken isolation support prevents
  service startup unless the operator deliberately opts into bare-process
  execution.
- Packaging and release engineering must account for the embedded runtime and
  minimal rootfs.
- Operational diagnostics become part of the contract. Logs, info, and doctor
  output must make the active runtime posture clear enough to support audits and
  incident response.

## Supersession

ADR-0015 temporarily narrows target support to Linux `x86_64` for direct binary
releases while trusted `aarch64` build provenance remains unavailable.
