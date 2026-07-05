# Deployment

This is the operator-facing deployment reference for `auto_review`. Start with
[Quickstart](./QUICKSTART.md) for the shortest path; return here for production
layouts and platform-specific details.

## Recommended production posture

Use the signed `auto-review` Linux binary archive for production. On supported
Linux hosts, the packaged binary attempts embedded OCI isolation by default. If
that is unavailable, startup fails closed unless you explicitly opt out with
`auto-review gateway --bare` or `AR_GATEWAY_BARE=true`. Bare mode is not
container-equivalent isolation.

The project does not publish an official Docker/OCI image as a first-class
artifact. Operators who want Docker, Podman, Kubernetes, or Helm deployments may
build and publish their own image from the binary package.

## Environment file

A minimal gateway env file looks like this:

```sh
FORGEJO_BASE_URL=https://forgejo.example.com
AR_FORGEJO_TOKEN=<bot PAT>
WEBHOOK_SECRET=<openssl rand -hex 32>
AR_CI_REVIEW_TOKEN=<openssl rand -hex 32>
LLM_BASE_URL=http://ollama.example.internal:11434
LLM_REASONING_MODEL=qwen2.5-coder:32b

# Optional explicit persistence paths. If unset, the gateway chooses SQLite
# paths under its state directory; use :memory: only for volatile evaluation.
AR_LEARNINGS_DB=/var/lib/auto_review/learnings.db
AR_HISTORY_DB=/var/lib/auto_review/review_history.db
AR_VECTOR_DB=/var/lib/auto_review/vector.db
AR_DEDUP_DB=/var/lib/auto_review/webhook_dedup.db

# Optional LLM cost attribution overrides
# AR_PRICE_TABLE_PATH=/etc/auto_review/prices.json
# AR_REVIEW_COST_FOOTER=false

RUST_LOG=info,ar_gateway=debug
```

Keep this file out of Git and out of the Nix store. Use your platform's secret
manager for production.

## Custom container images

No official `auto_review` gateway image is published. If your production boundary
is Docker, Podman, Kubernetes, or a platform that consumes OCI images, build and
publish an operator-owned image that runs `auto-review gateway`, includes `git`,
listens on `0.0.0.0:8080`, and stores persistent state under
`/var/lib/auto_review`. Set `AR_GATEWAY_EXTERNAL_ISOLATION=container` in the
image or deployment environment so the gateway reports the container boundary
instead of trying to enter the packaged embedded OCI launcher.

Example with an operator-owned Podman or Docker image:

```sh
podman run -d --name auto-review \
  --restart unless-stopped \
  --env-file /etc/auto_review/auto_review.env \
  -e AR_GATEWAY_EXTERNAL_ISOLATION=container \
  -p 127.0.0.1:8080:8080 \
  -v auto-review-state:/var/lib/auto_review \
  registry.example.com/auto-review/ar-gateway:latest
```

Put a TLS-terminating reverse proxy in front of `127.0.0.1:8080` and point
Forgejo webhooks at `https://reviewer.example.com/webhooks/forgejo`.

Treat operator-owned images as part of your deployment boundary: scan, sign, and
promote them with your normal platform controls.

## AWS Bedrock AgentCore runtime

The AgentCore path is for CI-invoked semantic review without a dedicated gateway
host. Build an operator-owned image that runs `auto-review agentcore serve` on
port 9000, configure `/ping` as the health path, and invoke `/invocations` only
after deterministic repository checks pass.

Deployment examples live under `deploy/agentcore/`:

- `Containerfile` shows the minimal runtime image shape.
- `runtime-config.json` records the port, health path, invocation path, and
  environment contract.
- `iam-policy.md` lists the DynamoDB state-table permissions and TTL note.
- `github-actions-oidc.yml` shows a GitHub Actions OIDC invocation.
- `forgejo-actions.yml` shows a Forgejo Actions invocation.

Set DynamoDB tables for cold-start-safe AgentCore state:

```sh
AGENTCORE_IDEMPOTENCY_DYNAMODB_TABLE=auto-review-agentcore-idempotency
AGENTCORE_HISTORY_DYNAMODB_TABLE=auto-review-agentcore-history
AGENTCORE_LEARNINGS_DYNAMODB_TABLE=auto-review-agentcore-learnings
```

Forgejo AgentCore review uses `FORGEJO_BASE_URL`, `AR_FORGEJO_TOKEN`, and
`LLM_BASE_URL`. GitHub AgentCore review uses GitHub App credentials:
`GITHUB_API_URL` (optional, defaults to `https://api.github.com`),
`GITHUB_APP_ID`, `GITHUB_APP_PRIVATE_KEY`, and `LLM_BASE_URL`. For GitHub, pass
the repository installation id in each CI invocation payload as
`installation_id`; the runtime exchanges the app JWT for a repository-scoped
installation token before fetching the PR and dispatching review.

Keep the always-on gateway deployment available for Forgejo webhook and chat
use cases. AgentCore is the no-dedicated-server path for CI-triggered semantic
review.

## Nix and NixOS

Build or install the current program:

```sh
nix build git+https://github.com/jwilger/auto_review
nix profile install git+https://github.com/jwilger/auto_review
nix shell git+https://github.com/jwilger/auto_review -c auto-review --help
```

The flake also ships a NixOS module for direct-host deployments and for installing
only the CLI:

```nix
{
  inputs.auto-review.url = "git+https://github.com/jwilger/auto_review";

  outputs = { nixpkgs, auto-review, ... }: {
    nixosConfigurations.reviewer = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        auto-review.nixosModules.default
        {
          services.auto-review.gateway = {
            enable = true;
            environmentFile = "/run/secrets/auto-review-gateway.env";
          };
        }
      ];
    };
  };
}
```

The module's gateway service intentionally sets `AR_GATEWAY_BARE=true`; it is a
systemd/direct-host deployment path with systemd hardening rather than embedded
OCI isolation.

When the gateway is enabled, the module also creates the dedicated
`auto_review` system user and group, runs the service under that account, binds
to `127.0.0.1:8080` by default for a reverse proxy, persists SQLite state under
the systemd `StateDirectory=auto_review`, and applies the same restart,
resource-limit, journald, and hardening baseline as the direct-host systemd unit
below. Keep the configured `environmentFile` out of Git and out of the Nix store;
use `/run/secrets/...` or your NixOS secret manager.

Embedded OCI isolation for NixOS deployments with durable host persistence is
tracked separately; until that lands, this module is the hardened bare/systemd
service path.

To install the CLI without enabling the service:

```nix
{
  imports = [ inputs.auto-review.nixosModules.default ];
  programs.auto-review.enable = true;
}
```

## systemd direct-host service

Use this path when you intentionally run the direct binary on a Linux host.

```bash
# Build and install the binary.
nix build .
sudo install -m 0755 result/bin/auto-review /usr/local/bin/auto-review

# Dedicated unprivileged user and state paths.
sudo useradd --system --no-create-home --shell /usr/sbin/nologin auto_review
sudo install -d -m 0700 -o auto_review -g auto_review /var/lib/auto_review
sudo install -d -m 0755 /etc/auto_review

# Secret env file.
sudo install -m 0600 -o root -g root \
  deploy/systemd/auto_review.env.example \
  /etc/auto_review/auto_review.env
sudo $EDITOR /etc/auto_review/auto_review.env

# Unit.
sudo install -m 0644 deploy/systemd/auto_review.service \
  /etc/systemd/system/auto_review.service
sudo systemctl daemon-reload
sudo systemctl enable --now auto_review.service

# Verify.
sudo systemctl status auto_review.service
journalctl -u auto_review.service --since "5m ago"
auto-review ops doctor
```

The example env file sets `AR_GATEWAY_BARE=true` and binds the service to
`127.0.0.1:8080` for a reverse proxy. The unit adds systemd hardening such as
`NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp`, `PrivateDevices`,
empty capability sets, and syscall filtering. These controls are defense in
depth around a bare process; they are not equivalent to embedded OCI isolation.

Upgrade a systemd host with the pinned Nix build:

```bash
git -C /opt/auto_review pull
nix build /opt/auto_review -o /tmp/auto-review-result
sudo install -m 0755 /tmp/auto-review-result/bin/auto-review /usr/local/bin/auto-review
sudo systemctl restart auto_review.service
curl -s http://localhost:8080/version
auto-review ops doctor
```

## Kubernetes / Helm

The Helm chart in `deploy/helm` creates a Deployment, Service, optional Ingress,
and Secret for an operator-owned image. Build and publish that image first, then
point the chart at your repository:

```sh
helm install auto-review ./deploy/helm \
  --set image.repository=registry.example.com/auto-review/ar-gateway \
  --set image.tag=latest \
  --set config.forgejoBaseUrl=https://forgejo.example.com \
  --set config.llmBaseUrl=https://api.openai.com \
  --set config.llmReasoningModel=gpt-4o-mini \
  --set secrets.forgejoToken="$AR_FORGEJO_TOKEN" \
  --set secrets.webhookSecret="$WEBHOOK_SECRET" \
  --set secrets.ciReviewToken="$AR_CI_REVIEW_TOKEN" \
  --set secrets.llmApiKey="$LLM_API_KEY" \
  --set ingress.enabled=true \
  --set ingress.hosts[0].host=reviewer.example.com \
  --set ingress.hosts[0].paths[0].path=/ \
  --set ingress.hosts[0].paths[0].pathType=Prefix
```

For production, prefer `secrets.secretRef` pointing at a Secret managed by your
secret-injection tool:

```sh
kubectl create secret generic auto-review-creds \
  --from-literal=AR_FORGEJO_TOKEN=... \
  --from-literal=WEBHOOK_SECRET=... \
  --from-literal=AR_CI_REVIEW_TOKEN=... \
  --from-literal=LLM_API_KEY=...

helm install auto-review ./deploy/helm \
  --set image.repository=registry.example.com/auto-review/ar-gateway \
  --set config.forgejoBaseUrl=https://forgejo.example.com \
  --set config.llmBaseUrl=https://api.openai.com \
  --set secrets.secretRef=auto-review-creds
```

The chart sets `AR_GATEWAY_EXTERNAL_ISOLATION=container` and wires liveness to
`/healthz` and readiness to `/readyz`. Add a PVC or hostPath for the gateway
state directory if you want SQLite learnings, review history, vector snippets,
and webhook dedup state to survive pod replacement.

## Forgejo Actions semantic-review trigger

`deploy/forgejo-action/action.yml` is a thin gateway client. It does not run the
review locally, call LLM providers, or execute linters; it only authenticates to
`POST /reviews/ci` after your prerequisite jobs pass.

Configure the gateway with `AR_CI_REVIEW_TOKEN`, store the gateway base URL as
an Actions variable such as `AUTO_REVIEW_GATEWAY_URL`, and store the same token
value as the Actions secret `AR_CI_REVIEW_TOKEN`. For this project,
configure those values on `Slipstream/auto_review` after repository ownership
changes; a stale or missing token secret reaches the gateway but receives HTTP
401 from `POST /reviews/ci`. Then add a gated job:

```yaml
semantic-review:
  runs-on: docker
  needs: [fmt, clippy, test]
  if: ${{ github.event_name == 'pull_request' }}
  steps:
    - uses: https://github.com/jwilger/auto_review/deploy/forgejo-action@main
      with:
        gateway-url: https://reviewer.example.com
        action-token: ${{ secrets.AR_CI_REVIEW_TOKEN }}
        owner: ${{ github.repository_owner }}
        repo: ${{ github.event.repository.name }}
        pr-number: ${{ github.event.pull_request.number }}
        head-sha: ${{ github.event.pull_request.head.sha }}
```

Forked PRs do not receive repository secrets. Do not use a privileged target-style
workflow that checks out or executes untrusted fork code with secrets.

## Prometheus and Grafana

Prometheus rules ship at `deploy/prometheus/auto_review.rules.yaml`. Install them
alongside a scrape job:

```yaml
rule_files:
  - /etc/prometheus/auto_review.rules.yaml

scrape_configs:
  - job_name: auto_review
    metrics_path: /metrics
    static_configs:
      - targets: ["reviewer.example.com:8080"]
```

The rules include recording rules for success rate, p95 latency, and chat command
rate, plus conservative alerts for signature failures, payload decode failures,
low success rate, poller stalls, review latency, and per-class Forgejo/LLM
failure spikes.

Import `deploy/grafana/auto_review.dashboard.json` into Grafana and select your
Prometheus data source for `DS_PROMETHEUS`. Install the Prometheus rules for the
lightest dashboard queries.

## Forgejo runner Nix cache

For this repository's own Forgejo Actions runners, a persistent `/nix` volume can
cut cold `nix flake check` runs substantially. Configure the runner host's
`act_runner.yaml`:

```yaml
container:
  valid_volumes:
    - /var/cache/forgejo-runner-nix
  options: -v /var/cache/forgejo-runner-nix:/nix
```

Then create the directory and restart the runner:

```bash
sudo install -d -m 0755 /var/cache/forgejo-runner-nix
sudo systemctl restart forgejo-runner
```

This cache is shared by every job on that runner. Use a dedicated runner if you
accept untrusted PRs from other repositories and do not want them sharing the Nix
store cache.
