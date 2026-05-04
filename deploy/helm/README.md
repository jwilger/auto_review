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
  --set secrets.forgejoToken=$AR_FORGEJO_TOKEN \
  --set secrets.webhookSecret=$WEBHOOK_SECRET \
  --set secrets.ciReviewToken=$AR_CI_REVIEW_TOKEN \
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
  --from-literal=AR_FORGEJO_TOKEN=... \
  --from-literal=WEBHOOK_SECRET=... \
  --from-literal=AR_CI_REVIEW_TOKEN=... \
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
  `secrets.webhookSecret` + `secrets.ciReviewToken`)

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

- A LanceDB-backed vector store sidecar / volume — the chart can persist the
  current SQLite stores when paths and volumes are configured, but it does not
  yet package a larger vector-store service.
- A NetworkPolicy template — recommended for production but
  organization-specific so left for the operator to add.
- HorizontalPodAutoscaler — the gateway is mostly idle between
  webhook bursts, so HPA is rarely useful; skipped to keep the
  chart small.
