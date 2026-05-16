# ADR-0018: Just-Based Checks and Binary-First Distribution

## Status

Accepted

## Date

2026-05-16

## Context

The project had been using Nix for more than dependency provisioning: the flake encoded routine CI checks, release workflow tests, production package assembly, Docker/OCI image construction, embedded OCI packaging, and NixOS service support. That coherence reduced environment drift, but it made routine CI and release changes disproportionately expensive and shifted attention away from core review behavior.

At the same time, the official Docker image had become a first-class release obligation even though the project already has a direct Linux binary direction with embedded OCI gateway isolation. Maintaining image builds, release-candidate image publication, image promotion, registry credentials, image contract tests, and related documentation created significant infrastructure overhead before there was enough operator demand to justify it.

The desired boundary is to keep Nix where it provides clear value—tool provisioning, reproducible production packaging, embedded OCI/rootfs assembly, and NixOS service installation—while using a simpler project command interface for everyday development and CI. The official out-of-the-box Linux service path should be the signed `auto-review` binary artifact with embedded OCI isolation, not a project-published Docker image.

## Decision

Adopt `just` as the canonical command interface for routine development and CI checks. Standard project commands such as formatting, clippy, tests, dependency policy checks, build checks, and aggregate CI checks should be exposed as `just` recipes that call the underlying tools directly. Recipes must not require `nix develop --command ...` internally.

Make Nix optional for developers. Developers may use any environment that provides the required tools on `PATH`; CI remains the arbiter for environment drift. The Nix dev shell remains a supported way to provision the same tool environment used by CI.

Keep Nix for the parts where it remains load-bearing: `nix develop`, the installable production package, embedded OCI/rootfs/runtime packaging for the gateway, and NixOS module/service support. Stop treating `nix flake check` as the primary CI orchestration interface.

Run PR CI as clear Forgejo jobs built around `just` recipes. CI may use Nix to provision tools, but formatting, clippy, tests, dependency policy checks, and build/package checks should appear as distinct jobs where practical so failures have focused logs and can run in parallel.

Stop publishing an official Docker/OCI image as a first-class project release artifact for now. Operators who want Docker or Podman images may build their own image. The official Linux release artifact is the signed `auto-review` binary archive with checksums, signing material, SBOM/provenance, and embedded OCI gateway isolation. The temporary Linux `x86_64`-only release target from ADR-0015 remains in force until a trusted `aarch64` build and provenance path exists.

This supersedes ADR-0013 where it recommends Docker/OCI images as the production artifact and requires release automation to publish an OCI image. It also supersedes ADR-0014 where embedded OCI isolation was framed as a direct-binary companion to a Docker-first production artifact. ADR-0013 remains in force for the single public `auto-review` executable, ADR-0014 remains in force for fail-closed embedded OCI isolation behavior, and ADR-0015 remains in force for temporary Linux `x86_64`-only direct binary releases.

## Consequences

- Routine development and CI become easier to understand because the project command surface is `just`, not flake internals.
- Nix remains valuable without becoming a participation tax for contributors who already have the required toolchain installed.
- CI can still share the Nix-provisioned tool environment while presenting focused check jobs and clearer logs.
- Release and CI workflows lose Docker image build, registry, promotion, and contract-test complexity.
- Operators lose a project-published turnkey container image and must use the binary/NixOS path or build their own image.
- Documentation, deployment examples, release automation, threat modeling, and project agent guidance must stop describing a project-published Docker image or `nix flake check` as the primary workflow.
- The embedded OCI gateway isolation path becomes more important because it is the safety basis for the official binary-first Linux distribution.

