# termic

One window, many parallel agents, each in its own git-worktree task with an embedded terminal. This glossary pins the terms the codebase and discussions must use.

## Language

### Integrations

**Integration Provider**:
An external service (GitHub first; GitLab, Linear later) that termic can talk to. Providers live in a single registry and declare which Capabilities they support.
_Avoid_: plugin, connector, forge/tracker (as type names)

**Capability**:
A discrete feature a Provider can declare support for. Initial set: Remote Detection, Work Items, PR Status. UI surfaces gate themselves on "does the active provider have capability X", never on the provider's name.

**Work Item**:
A unit of work fetched from a Provider that can seed a new task: a GitHub issue or pull request, a GitLab issue or MR, a Linear issue. Carries a kind (issue vs PR/MR).
_Avoid_: ticket, card

**Remote Detection**:
Determining which Provider (if any) matches a project's git remote (e.g. remote URL host is github.com).

**PR Status**:
The lifecycle state of the pull/merge request associated with a task's branch. Always derived from the branch (upstream / provider branch-to-PR resolution), never persisted on the Task; a task has PR Status regardless of whether it was created from a Work Item.
