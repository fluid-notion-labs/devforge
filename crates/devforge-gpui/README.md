# devforge-gpui (M4 — NOT YET BUILT)

Scaffold only. This crate is **excluded from the workspace** so `cargo build`
never fetches the zed/gpui dependency tree before the engine (M0/M1) is stable.

To activate:
1. In the root `Cargo.toml`, uncomment `exclude = ["crates/devforge-gpui"]`
   (i.e. remove the exclude) and add the crate to `members`.
2. Add the gpui dependency per its current docs:
   `gpui = { git = "https://github.com/zed-industries/zed" }` (workspace dep already listed).
3. Implement the client against `devforge_core::ipc::{Verb, StreamEvent}` —
   same contract as the TUI. No engine API touches.

Planned canvas (plan/spec.md "GPUI app"):
- service cards with the state machine as a diagram (ports, profile membership)
- side-by-side output panes (multi-pane log compare)
- history timeline over runs/events
- job buttons with staleness badges
