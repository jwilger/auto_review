# Toolchain

Use the Nix-pinned toolchain. Do not call system `rustup`, install a global Rust toolchain, or bypass `.dependencies/` `CARGO_HOME` and `RUSTUP_HOME`.

Focused checks are `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo nextest run --workspace --no-tests=pass`, and `cargo deny check licenses bans sources`. Use `nix flake check` for full CI parity.
