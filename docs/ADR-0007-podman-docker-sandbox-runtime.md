# ADR-0007: Podman or Docker as Production Sandbox Runtime, Youki Deferred

## Status

Superseded

## Date

2026-05-01

## Provenance

Reconstructed from implementation commit `f79fc5c` and documentation mutation
`7e87f01`, both dated 2026-05-01.

## Context

The original linter sandbox decision selected an OCI-based runtime for
production sandboxing. Subsequent implementation and documentation changes
clarified that the security decision was the use of a containerized sandbox
boundary, not a hard dependency on one specific OCI runtime implementation.

Youki was evaluated as a future operational improvement because it could reduce
runtime overhead or simplify deployment characteristics in some environments. It
was not required for the security model and was not a release gate.

## Decision

Accept Podman or Docker as the production OCI sandbox runtime.

Prefer `AR_SANDBOX_RUNTIME` for explicit operator configuration. Retain the
legacy Podman binary alias for existing deployments.

Defer Youki as a future operational improvement rather than treating it as a
security requirement or release blocker.

## Consequences

- Operators can deploy with the OCI runtime already supported by their
  environment, reducing operational friction without weakening the sandbox
  decision.
- Documentation and configuration should describe the runtime contract in terms
  of the selected OCI runtime command, not as a Podman-only requirement.
- Future Youki adoption can be evaluated independently from release readiness and
  security sign-off.

## Superseded by

ADR-0010 later retires bundled linter execution from the normal review runtime,
removing the linter-era runtime path this ADR clarified.
