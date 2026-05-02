{
  description = "auto_review — build, check, and dev environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        # Single source of truth: rust-toolchain.toml. The dev
        # shell, every `nix flake check`, and CI all resolve to
        # the same compiler + components from this file.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Crane source filter. We need cargo sources plus:
        # - workspace-root configuration files cargo-deny needs
        # - JSON schemas referenced via `include_str!` from
        #   ar-prompts (review/triage/verification/pre_merge_custom)
        # - per-crate README.md files (ar-cli's contract test reads
        #   its own README to verify every subcommand is documented)
        # - bench/fixtures/*.json (ar-cli's bench parser fixtures
        #   doubles as schema-drift contract test)
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter =
            path: type:
            let
              baseName = builtins.baseNameOf path;
              strPath = builtins.toString path;
            in
            (craneLib.filterCargoSources path type)
            || baseName == "deny.toml"
            || pkgs.lib.hasInfix "/.cargo/" strPath
            || (pkgs.lib.hasInfix "/ar-prompts/schemas/" strPath
                && pkgs.lib.hasSuffix ".json" path)
            || (pkgs.lib.hasInfix "/crates/" strPath && baseName == "README.md")
            || (pkgs.lib.hasInfix "/bench/fixtures/" strPath
                && pkgs.lib.hasSuffix ".json" path)
            # Deploy assets the gateway's contract tests cross-check
            # against the live /metrics surface.
            || (pkgs.lib.hasInfix "/deploy/grafana/" strPath
                && pkgs.lib.hasSuffix ".json" path)
            || (pkgs.lib.hasInfix "/deploy/prometheus/" strPath
                && pkgs.lib.hasSuffix ".yaml" path)
            # Dockerfile.sandbox is read by ar-tools/catalog.rs's
            # bundling contract test.
            || baseName == "Dockerfile.sandbox"
            # ar-review's config tests verify the example YAML
            # documents every known key.
            || baseName == ".auto_review.example.yaml";
        };

        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = with pkgs; [
            pkg-config
            perl
            # ar-cli's `doctor` command unconditionally probes
            # `git --version`; without git on PATH the related tests
            # fail with a clear-but-irrelevant "install git" report.
            git
            # ar-sandbox's DirectSandbox tests spawn `echo`, `sh`,
            # etc. via Command::new; coreutils + bash provide them
            # on PATH for the test runtime.
            coreutils
            bash
          ];
          buildInputs = with pkgs; [
            openssl
          ];
        };

        # Pre-built dependency layer reused by every check below.
        # Avoids re-compiling the dep tree for fmt/clippy/test/deny.
        cargoArtifacts = craneLib.buildDepsOnly (
          commonArgs
          // {
            pname = "auto_review";
          }
        );
      in
      {
        # ----- Dev shell ---------------------------------------------
        # `nix develop` (or `direnv allow` with .envrc) drops into
        # an environment with the pinned rust toolchain plus the
        # supply-chain and ergonomics tools the workflow expects.
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustToolchain
            cargo-deny
            cargo-nextest
            git
            jq
            pkg-config
            openssl
            # Local dev loop: `bacon` watches workspace sources and
            # re-runs the configured job (see bacon.toml). The default
            # `run` job builds + restarts ar-gateway on every change,
            # replacing the older zellij-tab watcher script.
            bacon
          ];

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

          shellHook = ''
            # Project-local cargo + rustup directories so the
            # nix-pinned toolchain doesn't fight a system rustup.
            export CARGO_HOME="$PWD/.dependencies/cargo"
            export RUSTUP_HOME="$PWD/.dependencies/rustup"
            mkdir -p "$CARGO_HOME" "$RUSTUP_HOME"
            export PATH="$CARGO_HOME/bin:$PATH"
          '';
        };

        # ----- Packages ----------------------------------------------
        # The gateway is the single binary operators run; expose it
        # as the default package so `nix build` produces a
        # deployable artefact.
        packages = {
          ar-gateway = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              pname = "ar-gateway";
              cargoExtraArgs = "-p ar-gateway --bin ar-gateway";
            }
          );
          ar-cli = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              pname = "ar-cli";
              cargoExtraArgs = "-p ar-cli";
            }
          );
          default = self.packages.${system}.ar-gateway;
        };

        # ----- CI checks ---------------------------------------------
        # `nix flake check` runs every entry below. CI shells out
        # to exactly that command, so local + CI exercise the same
        # derivations bit-for-bit.
        checks = {
          # Formatting drift — rejects any file rustfmt would
          # rewrite. Same surface as `cargo fmt --all -- --check`.
          cargo-fmt = craneLib.cargoFmt {
            inherit src;
            pname = "auto_review";
          };

          # Clippy with `-D warnings` so the lint set the project
          # has chosen to enforce blocks the merge.
          cargo-clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              pname = "auto_review";
              cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
            }
          );

          # Full workspace test suite. nextest produces parallel,
          # well-formatted output and refuses to run a target with
          # zero tests (caught by `--no-tests=pass`).
          cargo-nextest = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              pname = "auto_review";
              cargoNextestExtraArgs = "--workspace --no-tests=pass";
            }
          );

          # Supply-chain gate. Advisory checks need network access
          # (fetching RustSec DB) which the Nix sandbox blocks, so
          # the in-flake check covers licenses + bans + sources.
          # Operators run `cargo deny check advisories` separately
          # from the dev shell when they want the vuln scan.
          cargo-deny = craneLib.mkCargoDerivation (
            commonArgs
            // {
              inherit cargoArtifacts;
              pname = "auto_review";
              pnameSuffix = "-deny";
              nativeBuildInputs = (commonArgs.nativeBuildInputs or [ ]) ++ [ pkgs.cargo-deny ];
              buildPhaseCargoCommand = "cargo deny check licenses bans sources";
            }
          );
        };
      }
    );
}
