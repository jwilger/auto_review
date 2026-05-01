//! OCI sandbox launcher.
//!
//! Runs untrusted code (linters, LLM-issued shell commands) in an isolated
//! container with no network, read-only repo mount, seccomp restrictions,
//! and CPU/memory/wall-clock limits. Failure to sandbox is the
//! Kudelski-class RCE vector — non-negotiable for v1.
