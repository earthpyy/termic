# GitHub Integration (Integration Providers v1)

Status: agreed design, not yet implemented.
Companion docs: [CONTEXT.md](../../CONTEXT.md) (glossary), [ADR 0001](../adr/0001-rust-owned-integration-providers.md) (architecture decisions).

## Summary

Bring GitHub into termic's task flow: start tasks from your issues and PRs, see each task's PR status in the sidebar, and manage the `gh` dependency from a new Integrations settings tab. Everything is built behind a provider abstraction so GitLab and Linear can slot in later without reshaping the UI.

## Architecture

- **One `IntegrationProvider` abstraction with declared Capabilities**: Remote Detection, Work Items, PR Status. UI surfaces gate on capabilities, never on provider names. GitHub is the first provider; a future Linear provider would declare Work Items only.
- **Rust owns it.** Provider trait + registry in `src-tauri`, purpose-built provider-agnostic IPC commands (list work items, PR statuses, availability probe). TS mirrors the types only. `gh` spawning reuses the existing hardened deadline/kill spawn discipline. No generic `run_gh(args)` escape hatch to the webview (see ADR 0001).
- **Availability chain** (all three required, checked per project):
  1. Detected default remote's host matches `github.com` (https, ssh, and `git@` forms). The stored `Project.remote` name string is not used for this.
  2. `gh` binary resolved: manual path from settings if set, else login-shell PATH (`shell_env::resolved_path`).
  3. `gh` is authenticated.

  Any link fails: integration surfaces silently don't render. The Integrations settings tab is the only place that shows the full diagnostic and remedies.

## Features

### 1. Work Items in the new-task popup

- New section above EXISTING WORKTREES in the project actions menu: **3 items total**, issues and PRs mixed, sorted by most recently updated.
- Query: issues `assignee:@me`, PRs `author:@me`.
- Fetched on menu mount (like the worktrees list) with a per-project cache: cached items render instantly, a background refresh updates them in place. No spinner row.
- **Picking an issue** respects the Main checkout / Worktree toggle. Task name = issue title. Branch = `<branchPrefix>/<number>-<slug>` via the existing derive/unique helpers. Agent = project `default_cli`.
- **Picking a PR** is the same, except the worktree checks out the **PR's existing head branch** (fork PRs via `gh`'s PR refs). No derived branch.
- **Prompt seeding**: after the agent launches, a seed prompt is typed into the PTY but **not submitted**, e.g. "Work on GitHub issue #123: (title) (url). Run `gh issue view 123` for full details before starting." The user edits, then sends.

### 2. Work Item field in the Advanced dialog

- One optional combobox near the top: searches ~20 of "my" items, and accepts a pasted URL (`github.com/owner/repo/issues/123` or `/pull/456`), `#123`, or a bare number.
- Selecting prefills name + branch but leaves them editable. Exception: selecting a PR sets the branch to the PR head branch and disables the branch and base-branch fields (branch identity is what ties the task to the PR).
- Cross-repo pastes are rejected inline ("this issue belongs to another repository").

### 3. PR status badge in the sidebar

- **Derived, never persisted**: PR association comes from the task's branch, so any task whose branch has a PR gets a badge, however the task was created. Nothing is stored on `Task`.
- Colored dot, right-aligned after the task name. Five states:

  | State | Color |
  |---|---|
  | Open, mergeable | green |
  | Open, conflicted | orange |
  | Draft | gray |
  | Merged | purple |
  | Closed (unmerged) | red |

- Fetching: **one `gh pr list` per project**, mapped to tasks by head branch (never one spawn per task).
- Cadence: refresh on window focus + every ~60s while focused (never poll unfocused, same discipline as git-status polling) + immediately after an in-app push.
- Interaction: hover shows a tooltip ("PR #456, Open, has conflicts"); click does nothing (row click keeps selecting the task); the task kebab menu gains "Open PR in browser" when a PR exists.

### 4. Integrations settings tab

- New "Integrations" rail item directly below "Agents & Terminals".
- One section per provider (GitHub only for now) showing:
  - the availability diagnostic: repo match, resolved `gh` path, auth status + account;
  - an install button that opens the official `gh` install page in the browser (KISS, no in-app download);
  - a "Re-check" button to re-detect after installing;
  - a manual `gh` path field.
- Auth is out of app scope: the tab shows status and tells the user to run `gh auth login` in a terminal.
- Persisted in backend `settings.json`, nested per provider: `integrations: { github: { path?: ... } }`.

## Out of scope for v1 (future roadmap)

- Self-hosted GitHub / GitHub Enterprise.
- GitLab and Linear providers (the abstraction is shaped for them; Linear = Work Items only).
- CI status in the sidebar badge (lifecycle + conflicts only for now).
- In-app `gh auth login` and in-app binary download.
- Cross-repo work items.
- "More..." beyond the 3 popup items.
