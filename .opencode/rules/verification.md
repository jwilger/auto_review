# Verification

Run the narrow test that proves the current RGR cycle before broader gates. Use `cargo nextest run -p <crate> <substring>` or an exact `cargo test` target for focused Rust tests.

Before handoff, run the strongest relevant gate feasible for the change: `just fmt`, `just clippy`, `just test`, `just deny`, `just build`, or `just ci`. If a gate is skipped, state why.
