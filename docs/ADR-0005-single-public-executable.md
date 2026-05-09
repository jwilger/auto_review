# ADR-0005: Single public executable and release distribution

Status: **Accepted**
Date: 2026-05-06

## Context

`auto_review` currently exposes two operator-facing executables:

- `ar-gateway` runs the webhook gateway, chat poller, review dispatcher,
  readiness endpoints, and metrics.
- `auto_review` runs operator CLI tasks such as bootstrap, webhook management,
  deploy checks, one-shot reviews, benchmark fixtures, config validation, status
  inspection, and SQLite maintenance.

The release path is container-centered: the Nix flake builds `.#ar-gateway`,
`.#ar-cli`, and `.#ar-gateway-image`, while release automation publishes the
`ar-gateway` Docker/OCI image. That is the right production deployment artifact,
but it creates friction for operators who want to download and inspect a single
program, run local diagnostics, or try the gateway before wiring a container
runtime.

Issue #115 asked whether the project should release a downloadable
`auto-review` binary while keeping the threat model honest. The answer is yes,
provided the binary distribution is paired with the isolation decision in
[ADR-0006](./ADR-0006-embedded-oci-gateway-isolation.md) and the project does
not market a bare host process as equivalent to a container boundary.

## Decision

Adopt one public executable name: `auto-review`.

This is a breaking command-line switch. The implementation does **not** need to
preserve `ar-gateway`, `auto_review`, or old top-level subcommand compatibility
wrappers/aliases. The implementation PRs may stage internal refactors, but the
supported public surface after the switch is the grouped `auto-review` command
surface only.

Organize commands by domain:

```text
auto-review gateway ...
auto-review auth ...
auto-review webhook ...
auto-review config ...
auto-review review ...
auto-review bench run ...
auto-review ops ...
auto-review history ...
auto-review learnings ...
```

The exact leaf command names are implementation details, but existing operator
capabilities should map into those groups rather than staying as a flat list of
top-level commands.

`auto-review gateway` is the service entry point. It must use the same gateway
startup/configuration path as the old `ar-gateway` binary rather than duplicating
env parsing, defaults, validation, or startup warnings.

Continue publishing the Docker/OCI image as the recommended production
deployment artifact. The image must contain and run the same `auto-review`
binary, with its default command equivalent to:

```text
auto-review gateway
```

That keeps production deployment container-first while allowing an operator to
attach to the running image and execute the same grouped `auto-review` diagnostic
or maintenance commands inside the container.

Also attach downloadable Linux binaries to each release. The intended supported
binary targets are:

- Linux `x86_64`
- Linux `aarch64`

[ADR-0007](./ADR-0007-x86_64-only-binary-releases.md) temporarily narrows the
published release artifacts to Linux `x86_64` until a proper Linux `aarch64`
build runner or equivalent trusted build solution is available.

Binary releases must include full provenance from the first supported release:

- binary archives;
- SHA-256 checksums;
- signatures over checksums and/or artifacts;
- SBOM/provenance metadata;
- verification instructions in the Forgejo Release notes or attached docs.

Use Forgejo Releases, Forgejo package registry, `tea`, and/or Forgejo API calls.
Do not introduce GitHub-only release workflows.

## Consequences

- This is a deliberate breaking CLI migration. Operator docs, shell snippets,
  systemd units, Docker image commands, release notes, tests, and crate README
  contracts must move to `auto-review` and the grouped command layout.
- The Docker image remains a first-class production artifact, but the image and
  direct binary now share the same public executable.
- Release publishing gains new assets and therefore a wider supply-chain surface.
  `docs/THREAT-MODEL.md` must account for binary archives, checksums,
  signatures, SBOM/provenance metadata, and the release-publish token blast
  radius before the first binary release ships.
- The flake/package layout needs a public `auto-review` package. The image should
  be built from that package, not from a separate gateway-only binary.
- The CLI grouping should happen once, as a breaking migration, rather than
  carrying long-lived hidden aliases that make documentation and support harder.

## Required implementation slices

- [#116](https://git.johnwilger.com/jwilger/auto_review/issues/116) — implement
  the unified `auto-review` CLI with grouped commands.
- [#119](https://git.johnwilger.com/jwilger/auto_review/issues/119) — run the
  Docker image through the unified `auto-review` binary.
- [#121](https://git.johnwilger.com/jwilger/auto_review/issues/121) — publish
  Linux `auto-review` binaries with full provenance.
- [#120](https://git.johnwilger.com/jwilger/auto_review/issues/120) — update
  docs, the threat model, and red-team/contract tests for the rollout.

## Required tests and documentation before shipping

- Clap parsing tests for the grouped command surface.
- README/help contract tests for all public grouped commands.
- Gateway startup/config tests proving `auto-review gateway` uses the shared
  gateway env parsing and validation path.
- Image config/contract tests proving the image command uses `auto-review
  gateway` and contains the same `auto-review` binary operators can exec.
- Release-tooling tests for binary asset names, checksums, signatures,
  provenance/SBOM files, and Forgejo Release notes.
- `docs/QUICKSTART.md`, `docs/DEPLOYMENT.md`, `docs/OPERATIONS.md`, E2E
  runbook, and `docs/CLI.md` updated after the commands and artifacts exist.

## Alternatives considered

- **Keep split binaries.** Lowest implementation cost, but preserves the current
  onboarding friction and forces operators to understand which binary belongs to
  which workflow.
- **Single executable with flat commands.** Lower migration cost, but the current
  command list is already broad. Adding `gateway` is the right moment to group by
  domain and make the public surface easier to grow.
- **Compatibility shims for one or more releases.** Gentler for existing users,
  but this project is still pre-1.0 and the user-facing command surface is small
  enough to make a clean breaking switch preferable.
- **Stop publishing the Docker image.** Rejected. Container deployment remains the
  recommended production path even when direct binaries are available.
