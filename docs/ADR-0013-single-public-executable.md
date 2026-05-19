# ADR-0013: Single Public `auto-review` Executable and Release Distribution

## Status

Partially superseded

## Date

2026-05-06

## Provenance

Reconstructed from former `docs/ADR-0005-single-public-executable.md`, created in
commit `0754c0e` on 2026-05-06, with a minor command example adjustment from
`7aac223`.

## Context

`auto_review` needs a clear operator-facing interface and a predictable release
distribution story. Earlier development exposed multiple internal binaries and
entrypoints, which made packaging, documentation, and deployment guidance harder
to keep consistent.

The project also needs to support both source-based installation from the Rust
workspace and production deployment where operators should not be required to
build from source. Forgejo is the project forge and release host, so downloadable
artifacts should be attached there with enough metadata for operators to verify
what they run.

## Decision

Expose one public executable named `auto-review`.

Group user-facing and operator-facing functionality under subcommands instead of
shipping multiple public binaries. Internal implementation crates may continue to
exist, but release artifacts and documentation should present `auto-review` as
the supported executable.

Use `auto-review gateway` as the service entrypoint for the webhook gateway and
chat handling service.

Keep the Docker/OCI image as the recommended production artifact. The image
should remain the primary deployment path for operators because it packages the
executable, runtime environment, and service defaults consistently.

Attach downloadable Linux binaries to Forgejo releases for operators who need
direct binary installation. The intended first direct binary targets are Linux
`x86_64` and Linux `aarch64`. Release attachments should include checksums,
signatures, SBOM, and provenance materials so operators can verify integrity and
trace how artifacts were built.

## Consequences

- Documentation, examples, packaging, and deployment manifests should refer to
  `auto-review` as the single public executable.
- Service deployment examples should invoke `auto-review gateway` rather than a
  separate gateway binary.
- Release automation needs to produce and publish the OCI image plus Linux binary
  attachments with checksums, signatures, SBOM, and provenance through Forgejo.
- Internal crate and binary structure can evolve, but changes must preserve the
  public `auto-review` executable contract unless a later ADR explicitly changes
  it.

## Supersession

ADR-0015 temporarily narrows the direct binary release target assumptions in this
ADR to Linux `x86_64` only.

ADR-0018 supersedes this ADR's Docker/OCI image publication requirement. The
single public executable remains the operator contract; official release
artifacts are binary-first rather than project-published images.
