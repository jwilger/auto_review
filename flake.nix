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
    let
      autoReviewNixosModule =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          programCfg = config.programs.auto-review;
          gatewayCfg = config.services.auto-review.gateway;
          defaultPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        in
        {
          options = {
            programs.auto-review = {
              enable = lib.mkEnableOption "auto_review CLI installation";

              package = lib.mkOption {
                type = lib.types.package;
                default = defaultPackage;
                defaultText = lib.literalExpression "self.packages.${pkgs.system}.default";
                description = "auto_review package to install.";
              };
            };

            services.auto-review.gateway = {
              enable = lib.mkEnableOption "auto_review gateway service";

              environmentFile = lib.mkOption {
                type = lib.types.nullOr lib.types.path;
                default = null;
                description = "Environment file loaded by the auto_review gateway service.";
              };

              package = lib.mkOption {
                type = lib.types.package;
                default = programCfg.package;
                defaultText = lib.literalExpression "config.programs.auto-review.package";
                description = "auto_review package used by the gateway service.";
              };
            };
          };

          config = lib.mkMerge [
            (lib.mkIf programCfg.enable {
              environment.systemPackages = [ programCfg.package ];
            })

            (lib.mkIf gatewayCfg.enable {
              users.groups.auto_review = { };
              users.users.auto_review = {
                isSystemUser = true;
                group = "auto_review";
              };

              systemd.services.auto-review-gateway = {
                description = "auto_review gateway";
                wantedBy = [ "multi-user.target" ];
                after = [ "network-online.target" ];
                wants = [ "network-online.target" ];
                environment.AR_GATEWAY_BARE = "true";
                environment.AR_GATEWAY_BIND = "127.0.0.1:8080";
                serviceConfig = {
                  ExecStart = "${gatewayCfg.package}/bin/auto-review gateway";
                  StateDirectory = "auto_review";
                  Type = "exec";
                  RuntimeDirectory = "auto_review";
                  RuntimeDirectoryMode = "0700";
                  StateDirectoryMode = "0700";
                  ReadWritePaths = [ "/var/lib/auto_review" ];
                  Restart = "on-failure";
                  RestartSec = "5s";
                  KillSignal = "SIGTERM";
                  TimeoutStopSec = "30s";
                  LimitNOFILE = 4096;
                  TasksMax = 512;
                  StandardOutput = "journal";
                  StandardError = "journal";
                  SyslogIdentifier = "auto_review";
                  User = "auto_review";
                  Group = "auto_review";
                  NoNewPrivileges = true;
                  ProtectSystem = "strict";
                  ProtectHome = true;
                  ProtectKernelTunables = true;
                  ProtectKernelModules = true;
                  ProtectKernelLogs = true;
                  ProtectControlGroups = true;
                  ProtectClock = true;
                  ProtectHostname = true;
                  ProtectProc = "invisible";
                  PrivateTmp = true;
                  PrivateDevices = true;
                  PrivateUsers = true;
                  RestrictAddressFamilies = [
                    "AF_UNIX"
                    "AF_INET"
                    "AF_INET6"
                  ];
                  RestrictNamespaces = true;
                  RestrictRealtime = true;
                  RestrictSUIDSGID = true;
                  LockPersonality = true;
                  MemoryDenyWriteExecute = false;
                  SystemCallFilter = [ "@system-service" ];
                  SystemCallArchitectures = "native";
                  CapabilityBoundingSet = [ "" ];
                  AmbientCapabilities = [ "" ];
                }
                // lib.optionalAttrs (gatewayCfg.environmentFile != null) {
                  EnvironmentFile = gatewayCfg.environmentFile;
                };
              };
            })
          ];
        };
    in
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        # Single source of truth: rust-toolchain.toml. The dev
        # shell and package/check derivations resolve to
        # the same compiler + components from this file.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Crane source filter. We need cargo sources plus:
        # - workspace-root configuration files cargo-deny needs
        # - JSON schemas referenced via `include_str!` from
        #   ar-prompts (review/triage/verification)
        # - docs/*.md files (ar-cli's contract test reads docs/CLI.md
        #   to verify every subcommand is documented)
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
            || baseName == "lefthook.yml"
            || baseName == "Justfile"
            || pkgs.lib.hasInfix "/.cargo/" strPath
            || (pkgs.lib.hasInfix "/ar-prompts/schemas/" strPath && pkgs.lib.hasSuffix ".json" path)
            || (pkgs.lib.hasInfix "/docs/" strPath && pkgs.lib.hasSuffix ".md" strPath)
            || (pkgs.lib.hasInfix "/bench/fixtures/" strPath && pkgs.lib.hasSuffix ".json" path)
            # Deploy assets the gateway's contract tests cross-check
            # against the live /metrics surface and workflow action
            # contract.
            || (pkgs.lib.hasInfix "/deploy/grafana/" strPath && pkgs.lib.hasSuffix ".json" path)
            || (pkgs.lib.hasInfix "/deploy/prometheus/" strPath && pkgs.lib.hasSuffix ".yaml" path)
            || strPath == "${toString ./.}/deploy/forgejo-action/action.yml"
            # ar-review's config tests verify the example YAML
            # documents every known key.
            || baseName == ".auto_review.example.yaml"
            # Release automation contract tests and the project-local
            # release script they exercise.
            || (
              type == "directory"
              && (
                strPath == "${toString ./.}/tests"
                || strPath == "${toString ./.}/scripts"
                || strPath == "${toString ./.}/.codex"
                || strPath == "${toString ./.}/.codex/agents"
                || strPath == "${toString ./.}/.codex/hooks"
                || strPath == "${toString ./.}/.agents"
                || strPath == "${toString ./.}/.agents/skills"
                || strPath == "${toString ./.}/.forgejo"
                || strPath == "${toString ./.}/.forgejo/workflows"
                || strPath == "${toString ./.}/docs"
                || strPath == "${toString ./.}/deploy"
                || strPath == "${toString ./.}/deploy/systemd"
              )
            )
            || (pkgs.lib.hasInfix "/tests/" strPath && pkgs.lib.hasSuffix ".sh" path)
            || (pkgs.lib.hasInfix "/tests/" strPath && pkgs.lib.hasSuffix ".mjs" path)
            || (pkgs.lib.hasInfix "/tests/codex/" strPath && pkgs.lib.hasSuffix ".py" path)
            || (pkgs.lib.hasInfix "/scripts/" strPath)
            || (pkgs.lib.hasInfix "/.codex/" strPath)
            || (pkgs.lib.hasInfix "/.agents/skills/" strPath)
            || (pkgs.lib.hasInfix "/.forgejo/workflows/" strPath && pkgs.lib.hasSuffix ".yml" path)
            || strPath == "${toString ./.}/.forgejo/pull_request_template.md"
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

        # sidequest control plane (CLI + `sidequest-mcp` MCP stdio
        # server) from the jwilger/ai-plugins marketplace repo,
        # consumed by the side-quest Claude Code plugin.
        #
        # Deliberately NOT a flake input: flake inputs are fetched
        # eagerly at evaluation time, so an input would force CI's
        # egress-restricted runner to reach github.com just to
        # evaluate the flake. As a build-time `fetchgit` referenced only
        # by the interactive `default` dev shell, the source is fetched
        # and compiled solely when that shell is realized — CI's `.#ci`
        # shell never touches it.
        #
        # Tracks main by pinned rev; bump by re-running
        # `nix-prefetch-git --url <repo> --rev <main HEAD>` and updating
        # both fields below.
        sidequestSrc = pkgs.fetchgit {
          url = "https://github.com/jwilger/ai-plugins.git";
          rev = "bd4efae37fd046ef86de5dac97cf389172bbfdd4";
          hash = "sha256-QEzHuyqNOSGskxiJ4HFDVvPHSK6mnQV324/aPSTe20Q=";
        };
        sidequest = craneLib.buildPackage {
          src = sidequestSrc;
          pname = "sidequest";
          version = "0.1.0";
          strictDeps = true;
          # Build/install just the control-plane crate and its path dep.
          cargoExtraArgs = "-p sidequest";
          # The crate's cucumber/process/worktree tests need a writable
          # git+process environment the Nix sandbox lacks; the dev-shell
          # binary only needs to compile and install.
          doCheck = false;
        };
      in
      {
        # ----- Dev shells --------------------------------------------
        # `nix develop` (or `direnv allow` with .envrc) drops into
        # an environment with the pinned rust toolchain plus the
        # supply-chain and ergonomics tools the workflow expects.
        #
        # Two shells share one tool list:
        #   - `ci`      — toolchain + quality gates only. CI uses this
        #                 (see .forgejo/workflows/ci.yml) so its
        #                 sandboxed, egress-restricted runner never has
        #                 to compile the cross-repo `sidequest` crate.
        #   - `default` — the `ci` tools plus the sidequest control
        #                 plane, for interactive use.
        devShells =
          let
            baseTools = with pkgs; [
              rustToolchain
              cargo-deny
              cargo-nextest
              just
              cargo-semver-checks
              git
              lefthook
              forgejo-mcp
              tea
              python3
              nodejs_24
              jq
              openssh
              kubernetes-helm
              pkg-config
              openssl
              # Quick foreground Rust check loops.
              bacon
            ];

            mkDevShell =
              extraInputs:
              pkgs.mkShell {
                buildInputs = baseTools ++ extraInputs;

                RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

                shellHook = ''
                  # Project-local cargo + rustup directories so the
                  # nix-pinned toolchain doesn't fight a system rustup.
                  export CARGO_HOME="$PWD/.dependencies/cargo"
                  export RUSTUP_HOME="$PWD/.dependencies/rustup"
                  mkdir -p "$CARGO_HOME" "$RUSTUP_HOME"
                  export PATH="$CARGO_HOME/bin:$PATH"
                  if [ -f lefthook.yml ]; then
                    lefthook install
                  fi
                '';
              };
          in
          {
            ci = mkDevShell [ ];

            # Interactive default also provides the sidequest CLI and
            # `sidequest-mcp` (the MCP stdio server the side-quest
            # plugin's .mcp.json launches) on PATH.
            default = mkDevShell [ sidequest ];
          };

        # ----- Packages ----------------------------------------------
        # auto-review is the single binary operators run; expose it as
        # the default package so `nix build` produces a deployable
        # artefact.
        packages = rec {
          forgejo-mcp = pkgs.forgejo-mcp;
          ar-cli-unwrapped = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              pname = "ar-cli";
              cargoExtraArgs = "-p ar-cli";
            }
          );
          ar-gateway-embedded-oci-rootfs =
            pkgs.runCommand "embedded-gateway-oci-rootfs"
              {
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
                      uid = 0;
                      gid = 0;
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
                    readonly = false;
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
                        "uid=0"
                        "gid=0"
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
                      { type = "user"; }
                    ];
                    uidMappings = [
                      {
                        containerID = 0;
                        hostID = 65532;
                        size = 1;
                      }
                    ];
                    gidMappings = [
                      {
                        containerID = 0;
                        hostID = 65532;
                        size = 1;
                      }
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
              }
              ''
                mkdir -p "$out/rootfs/bin" "$out/rootfs/dev" "$out/rootfs/etc/ssl/certs" "$out/rootfs/nix/store" "$out/rootfs/var/lib/auto_review" "$out/rootfs/tmp"
                while IFS= read -r storePath; do
                  cp -a "$storePath" "$out/rootfs/nix/store/"
                done < "$rootfsClosure/store-paths"
                ln -s "${ar-cli-unwrapped}/bin/auto-review" "$out/rootfs/bin/auto-review"
                ln -s "${pkgs.git}/bin/git" "$out/rootfs/bin/git"
                ln -s "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt" "$out/rootfs/etc/ssl/certs/ca-bundle.crt"
                : > "$out/rootfs/dev/null"
                printf 'auto_review:x:65532:65532:auto_review:/var/lib/auto_review:/sbin/nologin\n' > "$out/rootfs/etc/passwd"
                printf 'auto_review:x:65532:\n' > "$out/rootfs/etc/group"
                printf 'hosts: files dns\n' > "$out/rootfs/etc/nsswitch.conf"
                : > "$out/rootfs/etc/resolv.conf"
                cp "$ociConfigPath" "$out/config.json"
              '';
          ar-cli =
            pkgs.runCommand "ar-cli"
              {
                nativeBuildInputs = [ pkgs.makeWrapper ];
              }
              ''
                mkdir -p "$out/bin"
                makeWrapper "${ar-cli-unwrapped}/bin/auto-review" "$out/bin/auto-review" \
                  --set-default AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH "${ar-gateway-embedded-oci-rootfs}" \
                  --set-default AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH "${pkgs.youki}/bin/youki"
              '';
          ar-cli-portable-release-root =
            pkgs.runCommand "auto-review-linux-x86_64-release-root"
              {
                nativeBuildInputs = [ pkgs.patchelf ];
                runtimeClosure = pkgs.closureInfo {
                  rootPaths = [
                    ar-cli-unwrapped
                    ar-gateway-embedded-oci-rootfs
                    pkgs.youki
                  ];
                };
              }
              ''
                            set -eu

                            mkdir -p "$out/bin" "$out/lib" "$out/nix/store"
                            while IFS= read -r storePath; do
                              cp -a "$storePath" "$out/nix/store/"
                            done < "$runtimeClosure/store-paths"
                            for sharedObject in "$out"/nix/store/*/lib/*.so* "$out"/nix/store/*/lib64/*.so*; do
                              if [ -e "$sharedObject" ]; then
                                cp -L "$sharedObject" "$out/lib/$(basename "$sharedObject")"
                                chmod 0644 "$out/lib/$(basename "$sharedObject")"
                              fi
                            done

                            interpreter="$(patchelf --print-interpreter "${ar-cli-unwrapped}/bin/auto-review")"
                            cp "$out${pkgs.youki}/bin/youki" "$out/bin/.youki-real"
                            chmod 0755 "$out/bin/.youki-real"
                            patchelf \
                              --set-interpreter /lib64/ld-linux-x86-64.so.2 \
                              --set-rpath '$ORIGIN/../lib' \
                              "$out/bin/.youki-real"
                            cat > "$out/auto-review" <<EOF
                #!/usr/bin/env sh
                set -eu

                root=\$(CDPATH= cd -- "\$(dirname -- "\$0")" && pwd)
                library_path=
                for lib_dir in "\$root"/nix/store/*/lib "\$root"/nix/store/*/lib64; do
                  if [ -d "\$lib_dir" ]; then
                    if [ -z "\$library_path" ]; then
                      library_path="\$lib_dir"
                    else
                      library_path="\$library_path:\$lib_dir"
                    fi
                  fi
                done

                export AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH="''${AR_GATEWAY_EMBEDDED_OCI_BUNDLE_PATH:-\$root${ar-gateway-embedded-oci-rootfs}}"
                export AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH="''${AR_GATEWAY_EMBEDDED_OCI_RUNTIME_PATH:-\$root/bin/youki}"

                exec "\$root$interpreter" --library-path "\$library_path" "\$root${ar-cli-unwrapped}/bin/auto-review" "\$@"
                EOF
                            chmod 0755 "$out/auto-review"
                            cat > "$out/bin/youki" <<EOF
                #!/usr/bin/env sh
                set -eu

                root=\$(CDPATH= cd -- "\$(dirname -- "\$0")/.." && pwd)
                export LD_LIBRARY_PATH="\$root/lib''${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
                exec "\$root/bin/.youki-real" "\$@"
                EOF
                            chmod 0755 "$out/bin/youki"
                            test -x "$out$interpreter"
                            test -x "$out${ar-cli-unwrapped}/bin/auto-review"
                            test -x "$out${pkgs.youki}/bin/youki"
                            test -x "$out/bin/youki"
                            test -d "$out${ar-gateway-embedded-oci-rootfs}"
              '';
          default = self.packages.${system}.ar-cli;
        };

        # ----- Nix boundary checks -----------------------------------
        # Keep package-shaped checks for Nix-owned packaging and module
        # contracts; routine CI checks stay in the `just` workflow.
        checks = {
          auto-review-packaged-gateway-launcher-contract =
            pkgs.runCommand "auto-review-packaged-gateway-launcher-contract"
              {
                autoReviewPackage = self.packages.${system}.default;
                nativeBuildInputs = with pkgs; [ gnugrep ];
              }
              ''
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
          auto-review-nixos-module-contract =
            let
              lib = nixpkgs.lib;
              evalAutoReviewModule =
                extraModules:
                lib.nixosSystem {
                  inherit system;
                  modules = [
                    self.nixosModules.default
                    { nixpkgs.pkgs = pkgs; }
                    { system.stateVersion = "26.05"; }
                  ]
                  ++ extraModules;
                };

              defaultSystem = evalAutoReviewModule [ ];
              programOnlySystem = evalAutoReviewModule [
                { programs.auto-review.enable = true; }
              ];
              gatewaySystem = evalAutoReviewModule [
                {
                  services.auto-review.gateway.enable = true;
                  services.auto-review.gateway.environmentFile = "/run/secrets/auto-review-gateway.env";
                }
              ];

              gatewayService = gatewaySystem.config.systemd.services.auto-review-gateway;
              gatewayServiceConfig = gatewayService.serviceConfig;
              gatewayEnvironment = gatewayService.environment;
              gatewayExecStart = lib.concatStringsSep " " (lib.toList gatewayServiceConfig.ExecStart);
              gatewayExecStartText = builtins.unsafeDiscardStringContext gatewayExecStart;
              expectedGatewayCommand = builtins.unsafeDiscardStringContext "${
                self.packages.${system}.default
              }/bin/auto-review gateway";
              gatewayEnvironmentFiles = lib.toList gatewayServiceConfig.EnvironmentFile;
              gatewayStateDirectories = lib.toList gatewayServiceConfig.StateDirectory;
              gatewayUser = gatewayServiceConfig.User or null;
              gatewayGroup = gatewayServiceConfig.Group or null;
              gatewayDeclaredUsers = gatewaySystem.config.users.users;
              gatewayDeclaredGroups = gatewaySystem.config.users.groups;
              gatewayServiceAccount = gatewayDeclaredUsers.auto_review or { };
              gatewayHasProductionHardeningBaseline =
                (gatewayServiceConfig.NoNewPrivileges or null) == true
                && (gatewayServiceConfig.ProtectSystem or null) == "strict"
                && (gatewayServiceConfig.ProtectHome or null) == true
                && (gatewayServiceConfig.ProtectKernelTunables or null) == true
                && (gatewayServiceConfig.ProtectKernelModules or null) == true
                && (gatewayServiceConfig.ProtectKernelLogs or null) == true
                && (gatewayServiceConfig.ProtectControlGroups or null) == true
                && (gatewayServiceConfig.ProtectClock or null) == true
                && (gatewayServiceConfig.ProtectHostname or null) == true
                && (gatewayServiceConfig.ProtectProc or null) == "invisible"
                && (gatewayServiceConfig.PrivateTmp or null) == true
                && (gatewayServiceConfig.PrivateDevices or null) == true
                && (gatewayServiceConfig.PrivateUsers or null) == true
                && (gatewayServiceConfig.RestrictAddressFamilies or [ ]) == [ "AF_UNIX" "AF_INET" "AF_INET6" ]
                && (gatewayServiceConfig.RestrictNamespaces or null) == true
                && (gatewayServiceConfig.RestrictRealtime or null) == true
                && (gatewayServiceConfig.RestrictSUIDSGID or null) == true
                && (gatewayServiceConfig.LockPersonality or null) == true
                && (gatewayServiceConfig.MemoryDenyWriteExecute or null) == false
                && (gatewayServiceConfig.SystemCallFilter or [ ]) == [ "@system-service" ]
                && (gatewayServiceConfig.SystemCallArchitectures or null) == "native"
                && (gatewayServiceConfig.CapabilityBoundingSet or [ ]) == [ "" ]
                && (gatewayServiceConfig.AmbientCapabilities or [ ]) == [ "" ];
              gatewayReadWritePaths = lib.toList (gatewayServiceConfig.ReadWritePaths or [ ]);
              gatewayHasProductionOperationalControls =
                (gatewayServiceConfig.Type or null) == "exec"
                && (gatewayServiceConfig.RuntimeDirectory or null) == "auto_review"
                && (gatewayServiceConfig.RuntimeDirectoryMode or null) == "0700"
                && (gatewayServiceConfig.StateDirectoryMode or null) == "0700"
                && builtins.elem "/var/lib/auto_review" gatewayReadWritePaths
                && (gatewayServiceConfig.Restart or null) == "on-failure"
                && (gatewayServiceConfig.RestartSec or null) == "5s"
                && (gatewayServiceConfig.KillSignal or null) == "SIGTERM"
                && (gatewayServiceConfig.TimeoutStopSec or null) == "30s"
                && (gatewayServiceConfig.LimitNOFILE or null) == 4096
                && (gatewayServiceConfig.TasksMax or null) == 512
                && (gatewayServiceConfig.StandardOutput or null) == "journal"
                && (gatewayServiceConfig.StandardError or null) == "journal"
                && (gatewayServiceConfig.SyslogIdentifier or null) == "auto_review";

              contract =
                assert lib.asserts.assertMsg (
                  !defaultSystem.config.services.auto-review.gateway.enable
                ) "services.auto-review.gateway.enable must default to false";
                assert lib.asserts.assertMsg (builtins.elem self.packages.${system}.default
                  programOnlySystem.config.environment.systemPackages
                ) "programs.auto-review.enable must install the auto-review package";
                assert lib.asserts.assertMsg (
                  !(programOnlySystem.config.systemd.services ? auto-review-gateway)
                ) "programs.auto-review.enable must not enable the gateway service";
                assert lib.asserts.assertMsg (lib.hasInfix expectedGatewayCommand gatewayExecStartText)
                  "gateway service ExecStart must launch auto-review gateway from the configured package";
                assert lib.asserts.assertMsg
                  (builtins.elem "/run/secrets/auto-review-gateway.env" gatewayEnvironmentFiles)
                  "gateway service must include the configured EnvironmentFile";
                assert lib.asserts.assertMsg (builtins.elem "auto_review" gatewayStateDirectories)
                  "gateway service must declare StateDirectory=auto_review";
                assert lib.asserts.assertMsg (
                  (gatewayEnvironment.AR_GATEWAY_BARE or null) == "true"
                ) "gateway service must set AR_GATEWAY_BARE=true for the bare systemd deployment path";
                assert lib.asserts.assertMsg (
                  (gatewayEnvironment.AR_GATEWAY_BIND or null) == "127.0.0.1:8080"
                ) "gateway service must default AR_GATEWAY_BIND to 127.0.0.1:8080";
                assert lib.asserts.assertMsg (
                  gatewayUser == "auto_review" && gatewayGroup == "auto_review"
                ) "gateway service must run as the dedicated non-root auto_review user/group by default";
                assert lib.asserts.assertMsg (
                  builtins.hasAttr "auto_review" gatewayDeclaredUsers
                  && builtins.hasAttr "auto_review" gatewayDeclaredGroups
                  && (gatewayServiceAccount.isSystemUser or false)
                  && (gatewayServiceAccount.group or null) == "auto_review"
                ) "gateway module must provision dedicated auto_review user/group when enabled";
                assert lib.asserts.assertMsg gatewayHasProductionHardeningBaseline
                  "gateway service must include the direct-host production systemd hardening baseline";
                assert lib.asserts.assertMsg gatewayHasProductionOperationalControls
                  "gateway service must include direct-host production operational controls for execution mode, runtime/state directories, write paths, restart/stop behavior, resource limits, and journald identity";
                true;
            in
            pkgs.runCommand "auto-review-nixos-module-contract" { inherit contract; } ''
              set -eu
              touch "$out"
            '';
          ar-gateway-embedded-oci-config-contract =
            pkgs.runCommand "ar-gateway-embedded-oci-config-contract"
              {
                rootfsBundle = self.packages.${system}.ar-gateway-embedded-oci-rootfs;
                nativeBuildInputs = with pkgs; [ jq ];
              }
              ''
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

                if ! jq -e '
                  . as $root |
                  any($root.linux.namespaces[]?; .type == "user") and
                  ([$root.linux.uidMappings[]? | select(
                    (.containerID | type == "number") and
                    (.hostID | type == "number") and
                    (.size | type == "number") and
                    .size > 0 and
                    .containerID <= $root.process.user.uid and
                    (.containerID + .size) > $root.process.user.uid
                  )] | length > 0) and
                  ([$root.linux.gidMappings[]? | select(
                    (.containerID | type == "number") and
                    (.hostID | type == "number") and
                    (.size | type == "number") and
                    .size > 0 and
                    .containerID <= $root.process.user.gid and
                    (.containerID + .size) > $root.process.user.gid
                  )] | length > 0)
                ' "$config" >/dev/null; then
                  printf 'missing OCI rootless user namespace contract: config.json must declare a user namespace with uidMappings and gidMappings covering the gateway process user\n' >&2
                  exit 1
                fi

                if ! jq -e '
                  . as $root |
                  $root.process.user.uid == 0 and
                  $root.process.user.gid == 0 and
                  ([$root.linux.uidMappings[]? | select(
                    .containerID == 0 and
                    .hostID == 65532 and
                    .size == 1 and
                    .containerID <= $root.process.user.uid and
                    (.containerID + .size) > $root.process.user.uid
                  )] | length > 0) and
                  ([$root.linux.gidMappings[]? | select(
                    .containerID == 0 and
                    .hostID == 65532 and
                    .size == 1 and
                    .containerID <= $root.process.user.gid and
                    (.containerID + .size) > $root.process.user.gid
                  )] | length > 0)
                ' "$config" >/dev/null; then
                  printf 'embedded OCI rootless config must run the process as container uid/gid 0 with single-entry uid/gid mappings for container ID 0\n' >&2
                  exit 1
                fi

                jq -e '
                  any(.linux.maskedPaths[]; . == "/proc/kcore") and
                  any(.linux.maskedPaths[]; . == "/proc/keys") and
                  any(.linux.maskedPaths[]; . == "/sys/firmware") and
                  any(.linux.readonlyPaths[]; . == "/proc/sys") and
                  any(.linux.readonlyPaths[]; . == "/sys")
                ' "$config" >/dev/null

                if ! jq -e '((.linux.maskedPaths // []) | length) > 0 and .root.readonly == false' "$config" >/dev/null; then
                  printf 'embedded OCI rootfs must stay writable when maskedPaths are configured so rootless youki can prepare mask symlinks like /proc/kcore\n' >&2
                  exit 1
                fi

                if [ ! -e "$rootfsBundle/rootfs/dev/null" ] && [ ! -L "$rootfsBundle/rootfs/dev/null" ]; then
                  printf 'embedded OCI rootfs must provide /dev/null before maskedPaths are prepared so rootless youki can safely mask /proc/kcore\n' >&2
                  exit 1
                fi

                if ! jq -e '
                  any(.mounts[]; .destination == "/tmp" and .type == "tmpfs" and (.options | index("mode=1777"))) and
                  any(.mounts[]; .destination == "/var/lib/auto_review" and .type == "tmpfs" and (.options | index("mode=0700")) and (.options | index("uid=0")) and (.options | index("gid=0")))
                ' "$config" >/dev/null; then
                  printf 'embedded OCI rootless config must mount /var/lib/auto_review tmpfs with uid=0 and gid=0 for the mapped container root user\n' >&2
                  exit 1
                fi

                touch "$out"
              '';
          ar-gateway-embedded-oci-rootfs-contents =
            pkgs.runCommand "ar-gateway-embedded-oci-rootfs-contents"
              {
                rootfsBundle = self.packages.${system}.ar-gateway-embedded-oci-rootfs;
              }
              ''
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
        };
      }
    )
    // {
      nixosModules = {
        default = autoReviewNixosModule;
        auto-review = autoReviewNixosModule;
      };
    };
}
