# ADR-0015: Temporarily x86_64-Only Binary Releases

## Status

Accepted

## Date

2026-05-07

## Provenance

Reconstructed from former `docs/ADR-0007-x86_64-only-binary-releases.md`, created
in commit `f257d39` on 2026-05-07.

## Context

`auto_review` publishes direct Linux binary artifacts for release consumption.
ADR-0013 and ADR-0014 include target assumptions for Linux `x86_64` and Linux
`aarch64` release coverage.

Until `auto_review` has a trusted Linux `aarch64` build and provenance path,
producing Linux `aarch64` binary artifacts would require an ad-hoc workaround
such as QEMU/binfmt emulation or an unverified native runner path. Those
workarounds would weaken the release provenance story and make the published
binaries harder to trust.

## Decision

Temporarily publish direct binary release artifacts for Linux `x86_64` only.

Do not publish Linux `aarch64` direct binary artifacts until a trusted Linux
`aarch64` build and provenance path exists. Do not use ad-hoc QEMU, binfmt, or
native-runner workarounds to produce Linux `aarch64` binary release artifacts.

Linux `aarch64` users should build `auto_review` from source while this decision
is in effect.

## Consequences

- Linux `x86_64` users continue to receive direct binary release artifacts.
- Linux `aarch64` users do not receive direct binary release artifacts until the
  project can provide a trusted build and provenance path.
- Release automation remains simpler and avoids adding infrastructure that would
  need to be unwound later.
- The project preserves provenance expectations rather than trading them for
  broader but less trustworthy binary availability.

## Supersession

This ADR partially supersedes the direct-binary release target assumptions in
ADR-0013 and ADR-0014. Those ADRs remain in force except where their release
target assumptions conflict with this temporary `x86_64`-only policy.
