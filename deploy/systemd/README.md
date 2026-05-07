# systemd unit for `auto_review`

For self-hosters who run the gateway directly on a Linux host
(no k8s, no docker). Pairs with the [Helm chart](../helm/) and
[`docker-compose.yml`](../docker-compose.yml) as the third
deploy option. This path exists for operators who want to build
`auto_review` into their own custom VM images or container images
instead of running the project-published Docker/OCI image unchanged.

The Docker/OCI image is still the recommended production deployment because it
provides an external container boundary. This unit is for custom direct-binary
hosts: it runs `auto-review gateway` under systemd hardening and sets
`AR_GATEWAY_BARE=true` in the example env file to explicitly opt out of the
embedded OCI launcher. Bare systemd mode has application-level controls plus
systemd hardening only; it is not container-equivalent isolation.

## Install

```bash
# 1. Build the unified binary with the pinned Nix toolchain.
nix build .#ar-cli
sudo install -m 0755 result/bin/auto-review /usr/local/bin/auto-review

# 2. Create a dedicated unprivileged user.
sudo useradd --system --no-create-home --shell /usr/sbin/nologin auto_review

# 3. Set up directories the unit expects.
sudo mkdir -p /etc/auto_review /var/lib/auto_review
sudo chown auto_review:auto_review /var/lib/auto_review
sudo chmod 0700 /var/lib/auto_review

# 4. Copy and edit the environment file. It contains
#    credentials, so the 0600 mode + root ownership is
#    important.
sudo install -m 0600 -o root -g root \
    deploy/systemd/auto_review.env.example \
    /etc/auto_review/auto_review.env
sudo $EDITOR /etc/auto_review/auto_review.env

# 5. Install the unit and start the service.
sudo install -m 0644 deploy/systemd/auto_review.service \
    /etc/systemd/system/auto_review.service
sudo systemctl daemon-reload
sudo systemctl enable --now auto_review.service

# 6. Verify.
sudo systemctl status auto_review.service
journalctl -u auto_review.service --since "5m ago"
auto-review ops doctor    # validates config end-to-end
```

After the service is up, register the webhook on each repo per
the [QUICKSTART](../../QUICKSTART.md). Front the gateway with a
TLS-terminating reverse proxy (caddy / nginx / traefik) — the
unit binds to `127.0.0.1:8080` by default; the proxy is what
Forgejo's webhook talks to.

## Hardening

The unit ships with the conservative-defaults sandbox profile
appropriate for an internet-facing service:

- `User`/`Group` — runs as the dedicated `auto_review` account
- `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`,
  `ProtectKernel*`, `ProtectControlGroups`, `ProtectProc=invisible`
- `PrivateTmp`, `PrivateDevices`, `PrivateUsers`
- `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6` — no raw
  sockets, no AF_NETLINK
- `RestrictNamespaces`, `RestrictRealtime`, `RestrictSUIDSGID`
- `CapabilityBoundingSet=` (empty), `AmbientCapabilities=` (empty)
- `SystemCallFilter=@system-service`

Deterministic linters/tests/builds now run in CI before the semantic review
trigger; the gateway unit only needs to protect clone/context/LLM review work
(see [docs/THREAT-MODEL.md](../../docs/THREAT-MODEL.md) §T1).

Because this direct-host unit opts out of the embedded OCI default, the hardening
below is defense-in-depth around a bare process rather than a replacement for the
recommended Docker production boundary.

## Per-host customisation

Use a systemd drop-in rather than editing the shipped unit so
upgrades don't clobber your changes:

```bash
sudo systemctl edit auto_review.service
```

Common drop-ins:

```ini
# Co-locate with Forgejo on the same host.
[Unit]
Wants=forgejo.service
After=forgejo.service
```

```ini
# Larger TasksMax for a high-traffic instance.
[Service]
TasksMax=2048
LimitNOFILE=16384
```

## Upgrade

```bash
git -C /opt/auto_review pull
nix build .#ar-cli
auto-review config validate /etc/auto_review/    # if applicable
sudo install -m 0755 result/bin/auto-review /usr/local/bin/auto-review
sudo systemctl restart auto_review.service
sudo systemctl status auto_review.service
```

If the new version fails to start, restore the previous binary
from your release artefact and `systemctl restart`. See
[OPERATIONS.md §9](../../docs/OPERATIONS.md#9-upgrade) for the
rollback playbook.

## Uninstall

```bash
sudo systemctl disable --now auto_review.service
sudo rm /etc/systemd/system/auto_review.service
sudo systemctl daemon-reload
# Optional: purge state
sudo rm -rf /etc/auto_review /var/lib/auto_review
sudo userdel auto_review
sudo rm /usr/local/bin/auto-review
```
