# herdr-goal-manager

Build a project-independent Herdr plugin and CLI for durable goals, dependency-aware
work, acceptance evidence, and recovery. Runtime state stays outside repositories.

- Rust edition 2024, MSRV 1.89. Keep public APIs documented; prefer crate-private APIs.
- English code comments and documentation. Use anyhow with actionable context.
- Reuse Herdr for terminal/session management. Do not create another terminal daemon.
- Command definitions come from user-owned goal files, never from model output.
- Distinguish task execution, successful verification, and human acceptance.
- Preserve attempt history and evidence across failures; never replay success silently.
- Tests use temporary workspaces/state and controlled executables, not user credentials.
- Check cargo fmt --all --check, cargo clippy --all-targets --locked -- -D warnings,
  cargo test --locked, cargo build --locked, and hgm --help.
- Use nwiizo-coding-style and its structural diagnostics for Rust changes.
- Do not delegate, commit, push, install globally, or restart shared Herdr sessions
  unless explicitly requested.
