# Toolchain

Use `just` for routine checks. Nix remains supported for provisioning the pinned toolchain; when inside `nix develop`, do not call system `rustup`, install a global Rust toolchain, or bypass `.dependencies/` `CARGO_HOME` and `RUSTUP_HOME`.

Focused checks are `just fmt`, `just clippy`, `just test`, `just deny`, and `just build`. Use `just ci` for the aggregate routine gate.
