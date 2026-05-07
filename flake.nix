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
              || baseName == "CHANGELOG.md"
              || baseName == "AGENTS.md"
              || baseName == "flake.nix"
             || pkgs.lib.hasInfix "/.cargo/" strPath
            || (pkgs.lib.hasInfix "/ar-prompts/schemas/" strPath
                && pkgs.lib.hasSuffix ".json" path)
            || (pkgs.lib.hasInfix "/crates/" strPath && baseName == "README.md")
            || (pkgs.lib.hasInfix "/bench/fixtures/" strPath
                && pkgs.lib.hasSuffix ".json" path)
            # Deploy assets the gateway's contract tests cross-check
            # against the live /metrics surface and workflow action
            # contract.
            || (pkgs.lib.hasInfix "/deploy/grafana/" strPath
                && pkgs.lib.hasSuffix ".json" path)
            || (pkgs.lib.hasInfix "/deploy/prometheus/" strPath
                && pkgs.lib.hasSuffix ".yaml" path)
            || strPath == "${toString ./.}/deploy/forgejo-action/action.yml"
             # ar-review's config tests verify the example YAML
             # documents every known key.
             || baseName == ".auto_review.example.yaml"
             # Release automation contract tests and the project-local
             # release script they exercise.
             || (type == "directory"
                 && (strPath == "${toString ./.}/tests"
                      || strPath == "${toString ./.}/scripts"
                      || strPath == "${toString ./.}/.kilo"
                      || strPath == "${toString ./.}/.kilo/command"
                      || strPath == "${toString ./.}/.kilo/skills"
                      || strPath == "${toString ./.}/.kilo/skills/rust-workspace-engineering"
                      || strPath == "${toString ./.}/.forgejo"
                      || strPath == "${toString ./.}/.forgejo/workflows"
                     || strPath == "${toString ./.}/docs"
                     || strPath == "${toString ./.}/deploy"
                     || strPath == "${toString ./.}/deploy/systemd"))
             || (pkgs.lib.hasInfix "/tests/" strPath
                 && pkgs.lib.hasSuffix ".sh" path)
             || (pkgs.lib.hasInfix "/scripts/" strPath)
              || (pkgs.lib.hasInfix "/.forgejo/workflows/" strPath
                  && pkgs.lib.hasSuffix ".yml" path)
              || strPath == "${toString ./.}/.forgejo/pull_request_template.md"
              || strPath == "${toString ./.}/.kilo/command/prepare-forgejo-pr.md"
              || strPath == "${toString ./.}/.kilo/skills/rust-workspace-engineering/SKILL.md"
              || strPath == "${toString ./.}/docs/OPERATIONS.md"
             || strPath == "${toString ./.}/docs/THREAT-MODEL.md"
             || strPath == "${toString ./.}/deploy/systemd/auto_review.env.example";
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
            cargo-semver-checks
            git
            forgejo-mcp
            tea
            python3
            jq
            skopeo
            kubernetes-helm
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
        # auto-review is the single binary operators run; expose it as
        # the default package so `nix build` produces a deployable
        # artefact.
        packages = rec {
          ar-cli-unwrapped = craneLib.buildPackage (
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
              ar-cli
              pkgs.cacert
              # Workspace preparation shells out to git for clone/fetch/checkout.
              # Keep it in the deploy-shaped image so reviews do not fail after
              # webhook intake.
              pkgs.git
            ];
            config = {
              Cmd = [ "/bin/auto-review" "gateway" ];
              Env = [
                "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                "PATH=/bin"
                "AR_GATEWAY_BIND=0.0.0.0:8080"
                "AR_GATEWAY_EXTERNAL_ISOLATION=container"
                "RUST_LOG=info,ar_gateway=debug"
              ];
              ExposedPorts = {
                "8080/tcp" = { };
              };
              WorkingDir = "/var/lib/auto_review";
              User = "65532:65532";
            };
          };
          ar-gateway-embedded-oci-rootfs = pkgs.runCommand "embedded-gateway-oci-rootfs" {
            rootfsClosure = pkgs.closureInfo {
              rootPaths = [
                ar-cli-unwrapped
                pkgs.cacert
                pkgs.git
              ];
            };
            ociConfig = builtins.toJSON {
              ociVersion = "1.0.2";
              process = {
                terminal = false;
                user = {
                  uid = 65532;
                  gid = 65532;
                };
                noNewPrivileges = true;
                capabilities = {
                  bounding = [ ];
                  effective = [ ];
                  inheritable = [ ];
                  permitted = [ ];
                  ambient = [ ];
                };
                args = [
                  "/bin/auto-review"
                  "gateway"
                ];
                env = [
                  "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
                  "PATH=/bin"
                  "AR_GATEWAY_BIND=0.0.0.0:8080"
                  "AR_GATEWAY_EXTERNAL_ISOLATION=container"
                  "RUST_LOG=info,ar_gateway=debug"
                ];
                cwd = "/var/lib/auto_review";
              };
              root = {
                path = "rootfs";
                readonly = true;
              };
              mounts = [
                {
                  destination = "/tmp";
                  type = "tmpfs";
                  source = "tmpfs";
                  options = [
                    "nosuid"
                    "nodev"
                    "mode=1777"
                  ];
                }
                {
                  destination = "/var/lib/auto_review";
                  type = "tmpfs";
                  source = "tmpfs";
                  options = [
                    "nosuid"
                    "nodev"
                  "mode=0700"
                  "uid=65532"
                  "gid=65532"
                ];
              }
            ];
              linux = {
                namespaces = [
                  { type = "pid"; }
                  { type = "network"; }
                  { type = "mount"; }
                  { type = "ipc"; }
                  { type = "uts"; }
                  { type = "cgroup"; }
                ];
                maskedPaths = [
                  "/proc/acpi"
                  "/proc/asound"
                  "/proc/kcore"
                  "/proc/keys"
                  "/proc/latency_stats"
                  "/proc/scsi"
                  "/proc/timer_list"
                  "/proc/timer_stats"
                  "/sys/firmware"
                ];
                readonlyPaths = [
                  "/proc/bus"
                  "/proc/fs"
                  "/proc/irq"
                  "/proc/sys"
                  "/proc/sysrq-trigger"
                  "/sys"
                ];
                resources = {
                  devices = [
                    {
                      allow = false;
                      access = "rwm";
                    }
                  ];
                };
              };
            };
            passAsFile = [ "ociConfig" ];
          } ''
            mkdir -p "$out/rootfs/bin" "$out/rootfs/etc/ssl/certs" "$out/rootfs/nix/store" "$out/rootfs/var/lib/auto_review" "$out/rootfs/tmp"
            while IFS= read -r storePath; do
              cp -a "$storePath" "$out/rootfs/nix/store/"
            done < "$rootfsClosure/store-paths"
            ln -s "${ar-cli-unwrapped}/bin/auto-review" "$out/rootfs/bin/auto-review"
            ln -s "${pkgs.git}/bin/git" "$out/rootfs/bin/git"
            ln -s "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt" "$out/rootfs/etc/ssl/certs/ca-bundle.crt"
            printf 'auto_review:x:65532:65532:auto_review:/var/lib/auto_review:/sbin/nologin\n' > "$out/rootfs/etc/passwd"
            printf 'auto_review:x:65532:\n' > "$out/rootfs/etc/group"
            printf 'hosts: files dns\n' > "$out/rootfs/etc/nsswitch.conf"
            : > "$out/rootfs/etc/resolv.conf"
            cp "$ociConfigPath" "$out/config.json"
          '';
          ar-cli = pkgs.runCommand "ar-cli" {
            nativeBuildInputs = [ pkgs.makeWrapper ];
          } ''
            mkdir -p "$out/bin"
            makeWrapper "${ar-cli-unwrapped}/bin/auto-review" "$out/bin/auto-review" \
              --set-default AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH "${ar-gateway-embedded-oci-rootfs}" \
              --set-default AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH "${pkgs.youki}/bin/youki"
          '';
          default = self.packages.${system}.ar-cli;
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
                env_passthrough="''${AR_DEV_ENV_PASSTHROUGH:-WEBHOOK_SECRET FORGEJO_BASE_URL AR_FORGEJO_TOKEN LLM_BASE_URL LLM_API_KEY LLM_REASONING_MODEL LLM_CHEAP_MODEL LLM_CHEAP_BASE_URL LLM_CHEAP_API_KEY LLM_EMBEDDING_MODEL LLM_EMBEDDING_BASE_URL LLM_EMBEDDING_API_KEY AR_CI_REVIEW_TOKEN}"

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
                  for env_name in $env_passthrough; do
                    if [ -n "''${!env_name:-}" ]; then
                      args+=(--env "$env_name")
                    fi
                  done
                  args+=(-v "auto-review-dev-state:/var/lib/auto_review" "$tag")
                  "$runtime" "''${args[@]}"
                }

                export -f rebuild_and_restart
                export -f load_image
                export runtime name tag port env_file env_passthrough

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
          auto-review-packaged-gateway-launcher-contract = pkgs.runCommand "auto-review-packaged-gateway-launcher-contract" {
            autoReviewPackage = self.packages.${system}.default;
            nativeBuildInputs = with pkgs; [ gnugrep ];
          } ''
            set -eu

            executable="$autoReviewPackage/bin/auto-review"
            missing=0

            if ! grep -aE 'AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH=.*/nix/store/' "$executable" >/dev/null; then
              printf 'missing wrapper contract: AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH must be set to an absolute packaged OCI bundle path\n' >&2
              missing=1
            fi

            if ! grep -aE 'AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH=.*/nix/store/' "$executable" >/dev/null; then
              printf 'missing wrapper contract: AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH must be set to an absolute packaged runtime path\n' >&2
              missing=1
            fi

            if ! grep -aE "/nix/store/[^[:space:]\"']*/bin/youki" "$executable" >/dev/null; then
              printf 'missing wrapper contract: packaged auto-review must reference a Nix-store youki runtime path instead of requiring host youki\n' >&2
              missing=1
            fi

            if [ "$missing" -ne 0 ]; then
              exit 1
            fi

            touch "$out"
          '';
          ar-gateway-embedded-oci-config-contract = pkgs.runCommand "ar-gateway-embedded-oci-config-contract" {
            rootfsBundle = self.packages.${system}.ar-gateway-embedded-oci-rootfs;
            nativeBuildInputs = with pkgs; [ jq ];
          } ''
            set -eu

            config="$rootfsBundle/config.json"

            jq -e '.process.noNewPrivileges == true' "$config" >/dev/null
            jq -e '
              .process.capabilities.bounding == [] and
              .process.capabilities.effective == [] and
              .process.capabilities.inheritable == [] and
              .process.capabilities.permitted == [] and
              .process.capabilities.ambient == []
            ' "$config" >/dev/null

            for namespace in pid network mount ipc uts cgroup; do
              jq -e --arg namespace "$namespace" 'any(.linux.namespaces[]; .type == $namespace)' "$config" >/dev/null
            done

            jq -e '
              any(.linux.maskedPaths[]; . == "/proc/kcore") and
              any(.linux.maskedPaths[]; . == "/proc/keys") and
              any(.linux.maskedPaths[]; . == "/sys/firmware") and
              any(.linux.readonlyPaths[]; . == "/proc/sys") and
              any(.linux.readonlyPaths[]; . == "/sys")
            ' "$config" >/dev/null

            jq -e '
              any(.mounts[]; .destination == "/tmp" and .type == "tmpfs" and (.options | index("mode=1777"))) and
              any(.mounts[]; .destination == "/var/lib/auto_review" and .type == "tmpfs" and (.options | index("mode=0700")))
            ' "$config" >/dev/null

            touch "$out"
          '';
          ar-gateway-embedded-oci-rootfs-contents = pkgs.runCommand "ar-gateway-embedded-oci-rootfs-contents" {
            rootfsBundle = self.packages.${system}.ar-gateway-embedded-oci-rootfs;
          } ''
            set -eu

            assert_resolves_inside_rootfs() {
              path="$1"
              if [ ! -e "$rootfsBundle/rootfs$path" ] && [ ! -L "$rootfsBundle/rootfs$path" ]; then
                printf 'missing rootfs path: %s\n' "$path" >&2
                exit 1
              fi

              if [ -L "$rootfsBundle/rootfs$path" ]; then
                target="$(readlink "$rootfsBundle/rootfs$path")"
                case "$target" in
                  /*)
                    resolved="$rootfsBundle/rootfs$target"
                    ;;
                  *)
                    resolved="$(dirname "$rootfsBundle/rootfs$path")/$target"
                    ;;
                esac

                if [ ! -e "$resolved" ]; then
                  printf 'rootfs path %s points outside embedded bundle: %s\n' "$path" "$target" >&2
                  printf 'expected target to resolve under %s/rootfs\n' "$rootfsBundle" >&2
                  exit 1
                fi
              fi
            }

            assert_resolves_inside_rootfs /bin/auto-review
            assert_resolves_inside_rootfs /bin/git
            assert_resolves_inside_rootfs /etc/ssl/certs/ca-bundle.crt

            touch "$out"
          '';
          ar-gateway-docker-image-unified-binary-contract = pkgs.runCommand "ar-gateway-docker-image-unified-binary-contract" {
            gatewayImage = self.packages.${system}.ar-gateway-image;
            nativeBuildInputs = with pkgs; [ jq gnugrep ];
          } ''
            set -eu

            image_dir="$PWD/image"
            mkdir -p "$image_dir"
            tar -xf "$gatewayImage" -C "$image_dir"

            config_file="$(jq -r '.[0].Config' "$image_dir/manifest.json")"
            config="$image_dir/$config_file"
            missing=0

            if ! jq -e '.config.Cmd == ["/bin/auto-review", "gateway"]' "$config" >/dev/null; then
              printf 'docker image must launch the unified CLI as /bin/auto-review gateway; observed Cmd: %s\n' "$(jq -c '.config.Cmd' "$config")" >&2
              missing=1
            fi

            if jq -e '((.config.Entrypoint // []) + (.config.Cmd // [])) | any(. == "ar-gateway" or test("(^|/)ar-gateway$"))' "$config" >/dev/null; then
              printf 'docker image still carries a stale ar-gateway entrypoint expectation\n' >&2
              missing=1
            fi

            assert_layer_contains() {
              path="$1"
              found=0
              for layer in "$image_dir"/*/layer.tar; do
                entries="$PWD/layer-entries.txt"
                tar -tf "$layer" > "$entries" 2>/dev/null
                if grep -Eq "^/?(\\./)?$path$" "$entries"; then
                  found=1
                  break
                fi
              done

              if [ "$found" -ne 1 ]; then
                printf 'docker image must contain /%s for operator exec/smoke usage\n' "$path" >&2
                missing=1
              fi
            }

            assert_layer_contains bin/auto-review
            assert_layer_contains bin/git

            if [ "$missing" -ne 0 ]; then
              exit 1
            fi

            touch "$out"
          '';
          release-tooling = pkgs.runCommand "auto-review-release-tooling" {
            inherit src;
            nativeBuildInputs = with pkgs; [
              bash
              coreutils
              git
              python3
              cargo-semver-checks
              tea
            ];
          } ''
            cp -R "$src" source
            chmod -R u+w source
            cd source
            patchShebangs scripts/release
            bash tests/release_tooling_test.sh
            touch "$out"
          '';
          ar-gateway-image = self.packages.${system}.ar-gateway-image;
        };
      }
    );
}
