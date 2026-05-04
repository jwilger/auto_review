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
        #   ar-prompts (review/triage/verification)
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
            # Some CLI and gateway tests spawn shell/coreutils helpers;
            # provide them from Nix rather than the host.
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
            forgejo-mcp
            jq
            pkg-config
            openssl
            # Quick foreground Rust check loops. Runtime development
            # uses `nix run .#dev-gateway-container` so it exercises
            # the same Nix image shape as deploy.
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
        packages = rec {
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
          ar-gateway-image = pkgs.dockerTools.buildLayeredImage {
            name = "git.johnwilger.com/jwilger/auto_review/ar-gateway";
            tag = "dev";
            fakeRootCommands = ''
              mkdir -p tmp var/lib/auto_review
              chmod 01777 tmp
              chown 65532:65532 var/lib/auto_review
              chmod 0700 var/lib/auto_review
            '';
            contents = [
              ar-gateway
              pkgs.cacert
              # Workspace preparation shells out to git for clone/fetch/checkout.
              # Keep it in the deploy-shaped image so reviews do not fail after
              # webhook intake.
              pkgs.git
            ];
            config = {
              Cmd = [ "${ar-gateway}/bin/ar-gateway" ];
              Env = [
                "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                "PATH=/bin"
                "AR_GATEWAY_BIND=0.0.0.0:8080"
                "RUST_LOG=info,ar_gateway=debug"
              ];
              ExposedPorts = {
                "8080/tcp" = { };
              };
              WorkingDir = "/var/lib/auto_review";
              User = "65532:65532";
            };
          };
          default = self.packages.${system}.ar-gateway;
        };

        apps = {
          dev-gateway-container = {
            type = "app";
            program = "${pkgs.writeShellApplication {
              name = "auto-review-dev-gateway-container";
              runtimeInputs = with pkgs; [
                coreutils
                docker-client
                gnugrep
                nix
                podman
                watchexec
              ];
              text = ''
                set -euo pipefail

                runtime="''${AR_DEV_CONTAINER_RUNTIME:-}"
                if [ -z "$runtime" ]; then
                  if command -v podman >/dev/null 2>&1 && podman info >/dev/null 2>&1; then
                    runtime=podman
                  elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
                    runtime=docker
                  else
                    printf 'No working podman or docker runtime found. Set AR_DEV_CONTAINER_RUNTIME.\n' >&2
                    exit 1
                  fi
                fi

                name="''${AR_DEV_CONTAINER_NAME:-auto-review-dev}"
                tag="''${AR_DEV_IMAGE_TAG:-git.johnwilger.com/jwilger/auto_review/ar-gateway:dev}"
                port="''${AR_DEV_GATEWAY_PORT:-8090}"
                env_file="''${AR_DEV_ENV_FILE:-.env}"

                load_image() {
                  if [ "$runtime" = "podman" ]; then
                    policy_file="$(mktemp)"
                    printf '%s\n' '{"default":[{"type":"insecureAcceptAnything"}]}' >"$policy_file"
                    if "$runtime" load --signature-policy "$policy_file" --input ./result; then
                      rm -f "$policy_file"
                    else
                      status=$?
                      rm -f "$policy_file"
                      return "$status"
                    fi
                  else
                    "$runtime" load --input ./result
                  fi
                }

                rebuild_and_restart() {
                  nix build .#ar-gateway-image
                  load_image
                  "$runtime" rm -f "$name" >/dev/null 2>&1 || true
                  args=(run --name "$name" --rm -p "127.0.0.1:$port:8080")
                  if [ -f "$env_file" ]; then
                    args+=(--env-file "$env_file")
                  fi
                  args+=(-v "auto-review-dev-state:/var/lib/auto_review" "$tag")
                  "$runtime" "''${args[@]}"
                }

                export -f rebuild_and_restart
                export -f load_image
                export runtime name tag port env_file

                watchexec \
                  --restart \
                  --watch crates \
                  --watch Cargo.toml \
                  --watch Cargo.lock \
                  --watch flake.nix \
                  --watch flake.lock \
                  --exts rs,toml,nix,lock \
                  --shell bash \
                  'rebuild_and_restart'
              '';
            }}/bin/auto-review-dev-gateway-container";
          };
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
            # Force Cargo's formatter lookup to the exact rustfmt
            # binary exposed by the dev shell's rustToolchain. This
            # keeps local `cargo fmt` and crane's cargo-fmt check on
            # the same tool package, not merely the same version.
            RUSTFMT = "${rustToolchain}/bin/rustfmt";
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
          ar-gateway-image = self.packages.${system}.ar-gateway-image;
        };
      }
    );
}
