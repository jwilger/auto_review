# ADR-0005: Gateway Request Boundary Defenses

## Status

Accepted

## Date

2026-05-01

## Provenance

Reconstructed from webhook rate-limit work in `e114b4b` and documentation commit
`02b5e64`. This decision was previously embedded in the broader observability
ADR.

## Context

The gateway is the first trust boundary for Forgejo webhook traffic. Boundary
handling needs to reject unauthenticated or malformed requests cheaply while
preserving enough telemetry to distinguish routine client errors from
security-relevant drift.

## Decision

The gateway request boundary processes webhook requests in this order:

1. Apply an optional rate limit before HMAC verification when rate limiting is
   configured.
2. Verify the webhook HMAC using a constant-time comparison before trusting any
   payload content.
3. Bucket event-type metrics using bounded labels rather than raw event names.
4. Decode JSON only after authentication succeeds, and bucket JSON decode
   failures by bounded failure class.
5. Record separate counters for active probing, secret drift, and schema drift.

Active probing covers unauthenticated or suspicious boundary traffic. Secret
drift covers failures consistent with an out-of-sync webhook secret. Schema drift
covers authenticated requests whose payload shape no longer matches the expected
contract.

## Consequences

The gateway avoids using untrusted payload data before authentication, limits
metric cardinality, and gives operators distinct signals for attack traffic,
configuration drift, and upstream API changes.

Rate limiting remains optional so deployments can choose where to enforce coarse
traffic controls, but when enabled it must run before HMAC work to reduce
boundary cost under load.
