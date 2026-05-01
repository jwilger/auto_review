# auto_review Helm chart

Kubernetes deployment of the auto_review gateway. The chart creates
a Deployment, Service, optional Ingress, and a Secret holding the
Forgejo + LLM credentials.

## Quickstart

```sh
helm install auto-review ./deploy/helm \
  --set config.forgejoBaseUrl=https://forgejo.example.com \
  --set config.llmBaseUrl=https://api.openai.com \
  --set config.llmReasoningModel=gpt-4o-mini \
  --set secrets.forgejoToken=$FORGEJO_TOKEN \
  --set secrets.webhookSecret=$WEBHOOK_SECRET \
  --set secrets.llmApiKey=$LLM_API_KEY \
  --set ingress.enabled=true \
  --set ingress.hosts[0].host=reviewer.example.com \
  --set ingress.hosts[0].paths[0].path=/ \
  --set ingress.hosts[0].paths[0].pathType=Prefix
```

For production, prefer `secrets.secretRef` to point at a
pre-existing Secret managed by your secret-injection tool of choice
(External Secrets Operator, Sealed Secrets, Vault Agent, etc.):

```sh
kubectl create secret generic auto-review-creds \
  --from-literal=FORGEJO_TOKEN=... \
  --from-literal=WEBHOOK_SECRET=... \
  --from-literal=LLM_API_KEY=...

helm install auto-review ./deploy/helm \
  --set config.forgejoBaseUrl=https://forgejo.example.com \
  --set config.llmBaseUrl=https://api.openai.com \
  --set secrets.secretRef=auto-review-creds \
  ...
```

## Required values

- `config.forgejoBaseUrl`
- `config.llmBaseUrl`
- One of `secrets.secretRef` OR (`secrets.forgejoToken` +
  `secrets.webhookSecret`)

## Optional values

- `config.llmEmbeddingModel`: enables RAG context retrieval.
- `config.llmCheapModel`: enables LLM-driven file triage and the
  verifier second-pass.
- `ingress.enabled`: expose via Ingress (otherwise reach the
  Service directly).
- `ephemeralStorage.sizeLimit`: cap on the workspace clone tempdir
  (default 5Gi).
- `resources.{requests,limits}`: CPU + memory budgets (defaults
  appropriate for OpenAI-API-only deployments; bump significantly
  for in-cluster LLM hosting).

## Pod security

The deployment runs as a non-root user (UID 10001), drops all
Linux capabilities, and uses a read-only root filesystem with two
emptyDir volumes (`/tmp` for the git clones, `~/.cargo` for any
cargo state). No Linux privileges escalation is possible from the
container.

## What's missing

- A LanceDB-backed vector store sidecar / volume — current build is
  in-memory only, so RAG state is lost on pod restart.
- A NetworkPolicy template — recommended for production but
  organization-specific so left for the operator to add.
- HorizontalPodAutoscaler — the gateway is mostly idle between
  webhook bursts, so HPA is rarely useful; skipped to keep the
  chart small.
