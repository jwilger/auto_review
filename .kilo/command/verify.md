---
description: Run focused or full repository verification through the Nix-pinned toolchain.
agent: auto-review-rust-implementer
---

Verify the current work: $ARGUMENTS

Prefer focused checks first, then broader gates as needed:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-tests=pass
cargo deny check licenses bans sources
nix flake check
```

Use `nix flake check` when the change affects Rust, Nix, CI, generated checks, or release/operator behavior. State any skipped gate and why.
