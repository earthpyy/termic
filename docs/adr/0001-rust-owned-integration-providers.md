# Integration providers live in Rust behind capability-gated IPC; PR association is derived, not persisted

Integrations (GitHub first; GitLab, Linear later) are modeled as a single provider registry in `src-tauri`, where each provider declares Capabilities (Remote Detection, Work Items, PR Status). The webview sees only purpose-built, provider-agnostic IPC commands and mirrored types; it never gets a generic `run_gh(args)` escape hatch. Rust owns process execution because all hardened spawn discipline (deadlines, kill-on-hang) already lives there, PR-status polling for many tasks wants Rust-side batching and caching (one `gh pr list` per project, not one spawn per task), and a raw CLI passthrough from the webview would widen the security surface the sandbox/CSP work deliberately keeps narrow. The accepted cost: adding a new provider means Rust work, not a drop-in TS module.

A task's PR association is always derived from its branch (upstream / provider branch-to-PR resolution), never stored on the `Task`. This keeps the model clean, and means any task whose branch grows a PR gets status for free, however the task was created. The trade-off: no durable "created from issue #N" record exists; the issue number lives only in the branch name and the seed prompt.

## Considered Options

- TS-owned provider interface with a dumb `run_gh(args)` IPC command: rejected (security surface, no Rust-side batching, N frontend timers).
- Persisting a work-item link (provider, kind, number, URL) on `Task`: rejected in favor of derivation from the branch.
