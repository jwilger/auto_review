---
paths:
  - "crates/ar-gateway/src/**"
  - "crates/ar-llm/**"
---

Read environment variables only through `read_non_empty_env(name)` and `parse_env::<T>(name)` in `crates/ar-gateway/src/startup.rs`. Cap provider error bodies with `ar_llm::cap_for_error` before logging or returning them, so secrets and huge payloads never leak into logs. Webhook payloads are verified (HMAC or GitHub signature, constant-time) before any parsing or side effect.
