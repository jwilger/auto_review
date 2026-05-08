# ADR-0007: Temporarily x86_64-only binary releases

Status: **Accepted**
Date: 2026-05-07

Amends: [ADR-0005](./ADR-0005-single-public-executable.md),
[ADR-0006](./ADR-0006-embedded-oci-gateway-isolation.md)

## Context

[ADR-0005](./ADR-0005-single-public-executable.md) accepted publishing
downloadable Linux `auto-review` binaries with provenance, and named Linux
`x86_64` and Linux `aarch64` as the first supported binary targets. That target
set assumed release automation could build both artifacts without expanding the
trusted release boundary in an uncomfortable way.

The current Forgejo runner reality does not satisfy that assumption:

- PR CI and semantic-review request jobs should run in Docker containers, not
  directly on host runners, because PR workflows execute repository-controlled
  code paths and should keep the container isolation boundary.
- Release publishing should also stay in a Docker runner for consistency and to
  avoid treating an ad-hoc native host runner as part of the release build
  boundary.
- The project does not currently have a dedicated Linux `aarch64` runner that can
  build and sign the `aarch64` binary artifact for us.
- Cross-building or emulating `aarch64` on the current runner would make QEMU,
  binfmt, and native-runner configuration part of the trusted release boundary.
  That is too much hidden operator machinery for a first binary release path.

The container image remains the recommended production deployment artifact. The
direct binary release is an operator convenience and diagnostic/onboarding path;
it must not force us into weaker runner isolation or unclear provenance.

## Decision

Publish Linux `x86_64` binary release artifacts only until the project has a
proper Linux `aarch64` build solution.

Each release should attach the Linux `x86_64` `auto-review` archive plus the
same provenance set required by [ADR-0005](./ADR-0005-single-public-executable.md):

- SHA-256 checksums;
- signature over the checksum manifest;
- signing public key / allowed signers material;
- SBOM metadata;
- provenance metadata;
- verification instructions in the Forgejo Release notes.

Do not publish Linux `aarch64` binary archives from the current Docker runner via
ad-hoc emulation, `extra-platforms`, host `binfmt`, or native-runner path probing.

Users on Linux `aarch64` can continue to build from source with Nix or Cargo
until official `aarch64` binary artifacts are restored.

Revisit this decision when one of these is available:

- a dedicated Linux `aarch64` Forgejo runner for trusted release jobs;
- a remote builder setup whose trust boundary, provenance, and signing flow are
  documented and covered by release tooling tests;
- another reproducible build path that does not require release workflows to run
  directly on an under-specified native host runner.

## Consequences

- Binary release coverage is narrower than ADR-0005 originally intended.
- Linux `aarch64` operators must build from source for now.
- Release publish can stay containerized, matching the rest of the Forgejo runner
  direction and avoiding native host runner environment assumptions.
- The threat model and operations docs must describe Linux `aarch64` binary
  artifacts as deferred, not as currently published assets.
- Release tooling tests should assert that the publish workflow does not attempt
  `aarch64-linux` builds from the Docker release runner.

## Alternatives considered

- **Run release publish on a native NixOS runner.** Rejected for now. Forgejo
  `host` labels execute shell steps directly from the runner daemon environment
  with no container isolation, and the runner service environment does not
  automatically behave like an interactive NixOS shell.
- **Use QEMU/binfmt from the current runner.** Rejected for now. It makes host
  emulation configuration part of the trusted release build boundary without a
  dedicated runner or documented verification story.
- **Skip all direct binary releases.** Rejected. Linux `x86_64` artifacts still
  provide value and can be built inside the existing Docker-based release path.
