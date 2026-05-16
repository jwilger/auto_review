# Security

Do not read or commit secrets. Treat `.env*`, keys, tokens, credentials, production data, and incident material as out of scope unless the user explicitly authorizes a specific read.

Changes touching sandboxing, LLM/tool trust boundaries, webhook verification, auth, secrets, dependencies, or documented threat IDs must be checked against `docs/THREAT-MODEL.md` and the relevant red-team tests.
